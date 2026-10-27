//! AniList (<https://anilist.co>) anime metadata provider extension for
//! LisekMovie.
//!
//! AniList has a public GraphQL API at `https://graphql.anilist.co` that
//! needs no key (rate limit: 90 requests a minute, `429` beyond that).
//! Films are anime of format `MOVIE`; series are `TV`, `TV_SHORT`, `ONA`,
//! `OVA` and `SPECIAL`.
//!
//! The automatic match (scans) is strict: a title is only taken when one
//! of AniList's titles (romaji, English, native or a synonym) equals the
//! scanned one after normalization, so an anime source placed after TMDB
//! in a library's order does not hand a live-action film a random anime.
//! Fix match lists whatever AniList's search returns.

use std::time::Duration;

use async_trait::async_trait;
use lisek_plugin_api::{
    MediaKind, MetadataProvider, MovieQuery, ProviderCandidate, ProviderError, ProviderMatch,
    SeriesQuery, normalize_query_title,
};
use serde::Deserialize;

const DEFAULT_BASE_URL: &str = "https://graphql.anilist.co";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_TAGS: usize = 20;

const MOVIE_FORMATS: &[&str] = &["MOVIE"];
const SERIES_FORMATS: &[&str] = &["TV", "TV_SHORT", "ONA", "OVA", "SPECIAL"];

/// Fields read from every `Media`.
const MEDIA_FIELDS: &str = "id idMal format episodes duration averageScore isAdult genres synonyms
    title { romaji english native }
    startDate { year }
    description(asHtml: false)
    coverImage { extraLarge large medium }
    bannerImage
    studios(isMain: true) { nodes { name } }
    tags { name rank isMediaSpoiler }";

#[derive(Debug, Clone, Default, Deserialize)]
struct RawTitle {
    romaji: Option<String>,
    english: Option<String>,
    native: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct RawDate {
    year: Option<i32>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawCover {
    extra_large: Option<String>,
    large: Option<String>,
    medium: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct RawName {
    name: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct RawStudios {
    #[serde(default)]
    nodes: Vec<RawName>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawTag {
    name: String,
    #[serde(default)]
    rank: u32,
    #[serde(default)]
    is_media_spoiler: bool,
}

/// One AniList `Media` (anime).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnilistMedia {
    pub id: u64,
    id_mal: Option<u64>,
    format: Option<String>,
    duration: Option<u32>,
    average_score: Option<u32>,
    #[serde(default)]
    is_adult: bool,
    #[serde(default)]
    genres: Vec<String>,
    #[serde(default)]
    synonyms: Vec<String>,
    #[serde(default)]
    title: RawTitle,
    #[serde(default)]
    start_date: RawDate,
    description: Option<String>,
    #[serde(default)]
    cover_image: RawCover,
    banner_image: Option<String>,
    #[serde(default)]
    studios: RawStudios,
    #[serde(default)]
    tags: Vec<RawTag>,
}

/// AniList descriptions carry a few HTML tags (`<br>`, `<i>`) even with
/// `asHtml: false`, plus `(Source: …)` notes; both are dropped.
pub fn plain_description(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_tag = false;
    for ch in text.chars() {
        match ch {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    let out = out
        .replace("&quot;", "\"")
        .replace("&amp;", "&")
        .replace("&#039;", "'");
    let trimmed = match out.rfind("(Source:") {
        Some(start) if out[start..].trim_end().ends_with(')') => &out[..start],
        _ => out.as_str(),
    };
    trimmed
        .lines()
        .map(str::trim)
        .collect::<Vec<_>>()
        .join("\n")
        .split("\n\n\n")
        .collect::<Vec<_>>()
        .join("\n\n")
        .trim()
        .to_string()
}

impl AnilistMedia {
    /// English when AniList has it, else romaji, else native.
    pub fn display_title(&self) -> String {
        [&self.title.english, &self.title.romaji, &self.title.native]
            .into_iter()
            .flatten()
            .map(|t| t.trim())
            .find(|t| !t.is_empty())
            .unwrap_or_default()
            .to_string()
    }

    fn all_titles(&self) -> impl Iterator<Item = &str> {
        [&self.title.english, &self.title.romaji, &self.title.native]
            .into_iter()
            .flatten()
            .map(String::as_str)
            .chain(self.synonyms.iter().map(String::as_str))
    }

    fn has_title(&self, wanted: &str) -> bool {
        let wanted = normalize_query_title(wanted);
        !wanted.is_empty()
            && self
                .all_titles()
                .any(|t| normalize_query_title(t) == wanted)
    }

    fn is(&self, kind: MediaKind) -> bool {
        let formats = match kind {
            MediaKind::Movie => MOVIE_FORMATS,
            MediaKind::Series => SERIES_FORMATS,
        };
        self.format.as_deref().is_some_and(|f| formats.contains(&f))
    }

    fn poster(&self) -> Option<String> {
        self.cover_image
            .extra_large
            .clone()
            .or_else(|| self.cover_image.large.clone())
    }

    fn to_match(&self, with_title: bool) -> ProviderMatch {
        let original = self
            .title
            .native
            .clone()
            .or_else(|| self.title.romaji.clone());
        let mut ranked: Vec<&RawTag> = self.tags.iter().filter(|t| !t.is_media_spoiler).collect();
        ranked.sort_by_key(|t| std::cmp::Reverse(t.rank));
        let tags: Vec<String> = ranked
            .into_iter()
            .take(MAX_TAGS)
            .map(|t| t.name.clone())
            .collect();
        let mut fields = serde_json::json!({
            "anilist_id": self.id,
            "overview": self.description.as_deref().map(plain_description).unwrap_or_default(),
            "genres": self.genres,
            "studios": self.studios.nodes.iter().map(|s| s.name.clone()).collect::<Vec<_>>(),
            "tags": tags,
        });
        if let Some(mal) = self.id_mal {
            fields["mal_id"] = serde_json::json!(mal);
        }
        if let Some(original) = original.filter(|o| *o != self.display_title()) {
            fields["original_title"] = serde_json::json!(original);
        }
        if let Some(score) = self.average_score.filter(|s| *s > 0) {
            fields["rating"] = serde_json::json!(f64::from(score) / 10.0);
        }
        if let Some(duration) = self.duration.filter(|d| *d > 0) {
            fields["runtime_minutes"] = serde_json::json!(duration);
        }
        if self.is_adult {
            fields["age_rating"] = serde_json::json!(18);
        }
        if with_title {
            fields["title"] = serde_json::json!(self.display_title());
            fields["year"] = serde_json::json!(self.start_date.year);
        }
        ProviderMatch {
            poster_url: self.poster(),
            backdrop_url: self.banner_image.clone(),
            banner_url: self.banner_image.clone(),
            fields,
            ..ProviderMatch::default()
        }
    }
}

#[derive(Debug, Deserialize)]
struct GraphQlError {
    message: String,
    status: Option<u16>,
}

#[derive(Debug, Deserialize)]
struct GraphQlResponse<T> {
    data: Option<T>,
    #[serde(default)]
    errors: Vec<GraphQlError>,
}

#[derive(Debug, Deserialize)]
struct PageData {
    #[serde(rename = "Page")]
    page: Option<PageMedia>,
}

#[derive(Debug, Deserialize)]
struct PageMedia {
    #[serde(default)]
    media: Vec<AnilistMedia>,
}

#[derive(Debug, Deserialize)]
struct MediaData {
    #[serde(rename = "Media")]
    media: Option<AnilistMedia>,
}

/// HTTP client of the AniList GraphQL API. `base_url` is injectable for
/// tests.
pub struct AnilistClient {
    http: reqwest::Client,
    base_url: String,
}

impl AnilistClient {
    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(REQUEST_TIMEOUT)
                .build()
                .unwrap_or_default(),
            base_url: base_url.into(),
        }
    }

    async fn query<T: serde::de::DeserializeOwned>(
        &self,
        query: &str,
        variables: serde_json::Value,
    ) -> Result<Option<T>, ProviderError> {
        let response = self
            .http
            .post(&self.base_url)
            .header("Accept", "application/json")
            .json(&serde_json::json!({ "query": query, "variables": variables }))
            .send()
            .await
            .map_err(|err| ProviderError::Request(err.to_string()))?;
        let status = response.status();
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(ProviderError::Request(
                "AniList rate limit reached (90 requests a minute)".into(),
            ));
        }
        let body: GraphQlResponse<T> = response
            .json()
            .await
            .map_err(|err| ProviderError::UnexpectedResponse(err.to_string()))?;
        if let Some(error) = body.errors.first() {
            // An unknown id is a 404 error, not a failure.
            if error.status == Some(404) || status == reqwest::StatusCode::NOT_FOUND {
                return Ok(None);
            }
            return Err(ProviderError::UnexpectedResponse(error.message.clone()));
        }
        Ok(body.data)
    }

    /// Up to 10 anime of `kind` for a title, AniList's best first.
    pub async fn search(
        &self,
        kind: MediaKind,
        title: &str,
        year: Option<i32>,
        include_adult: bool,
    ) -> Result<Vec<AnilistMedia>, ProviderError> {
        let query = format!(
            "query ($search: String, $formats: [MediaFormat], $after: FuzzyDateInt, $before: FuzzyDateInt, $adult: Boolean) {{
              Page(perPage: 10) {{
                media(search: $search, type: ANIME, format_in: $formats, startDate_greater: $after,
                      startDate_lesser: $before, isAdult: $adult, sort: SEARCH_MATCH) {{ {MEDIA_FIELDS} }}
              }}
            }}"
        );
        let formats = match kind {
            MediaKind::Movie => MOVIE_FORMATS,
            MediaKind::Series => SERIES_FORMATS,
        };
        let mut variables = serde_json::json!({ "search": title, "formats": formats });
        if let Some(year) = year {
            variables["after"] = serde_json::json!(year * 10_000);
            variables["before"] = serde_json::json!((year + 1) * 10_000);
        }
        if !include_adult {
            variables["adult"] = serde_json::json!(false);
        }
        let data: Option<PageData> = self.query(&query, variables).await?;
        Ok(data
            .and_then(|d| d.page)
            .map(|p| p.media)
            .unwrap_or_default())
    }

    /// One anime by its AniList id.
    pub async fn media(&self, id: u64) -> Result<Option<AnilistMedia>, ProviderError> {
        let query =
            format!("query ($id: Int) {{ Media(id: $id, type: ANIME) {{ {MEDIA_FIELDS} }} }}");
        let data: Option<MediaData> = self.query(&query, serde_json::json!({ "id": id })).await?;
        Ok(data.and_then(|d| d.media))
    }
}

/// The [`MetadataProvider`] for `"anilist"`. Needs no API key.
pub struct AnilistProvider {
    base_url: String,
}

impl AnilistProvider {
    pub fn new() -> Self {
        Self::with_base_url(DEFAULT_BASE_URL)
    }

    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
        }
    }

    fn client(&self) -> AnilistClient {
        AnilistClient::with_base_url(self.base_url.clone())
    }

    async fn find(
        &self,
        kind: MediaKind,
        title: &str,
        year: Option<i32>,
    ) -> Result<Option<ProviderMatch>, ProviderError> {
        let found = self.client().search(kind, title, year, false).await?;
        Ok(found
            .iter()
            .find(|m| m.is(kind) && m.has_title(title))
            .map(|m| m.to_match(false)))
    }
}

impl Default for AnilistProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl MetadataProvider for AnilistProvider {
    fn provider_key(&self) -> &'static str {
        "anilist"
    }

    async fn find_movie(
        &self,
        query: &MovieQuery<'_>,
        _lang: &str,
    ) -> Result<Option<ProviderMatch>, ProviderError> {
        self.find(MediaKind::Movie, query.title, query.year).await
    }

    async fn find_series(
        &self,
        query: &SeriesQuery<'_>,
        _lang: &str,
    ) -> Result<Option<ProviderMatch>, ProviderError> {
        self.find(MediaKind::Series, query.title, None).await
    }

    async fn search(
        &self,
        kind: MediaKind,
        title: &str,
        year: Option<i32>,
        _lang: &str,
    ) -> Result<Vec<ProviderCandidate>, ProviderError> {
        Ok(self
            .client()
            .search(kind, title, year, true)
            .await?
            .into_iter()
            .map(|m| ProviderCandidate {
                provider_id: m.id.to_string(),
                title: m.display_title(),
                year: m.start_date.year,
                overview: m
                    .description
                    .as_deref()
                    .map(plain_description)
                    .unwrap_or_default(),
                poster_url: m.cover_image.medium.clone().or_else(|| m.poster()),
            })
            .collect())
    }

    /// Any format: an administrator may pin an OVA on a film.
    async fn lookup(
        &self,
        _kind: MediaKind,
        provider_id: &str,
        _lang: &str,
    ) -> Result<Option<ProviderMatch>, ProviderError> {
        let Ok(id) = provider_id.trim().parse::<u64>() else {
            return Ok(None);
        };
        Ok(self.client().media(id).await?.map(|m| m.to_match(true)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lisek_plugin_api::DirectId;
    use wiremock::matchers::{body_partial_json, method};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn one_piece() -> serde_json::Value {
        serde_json::json!({
            "id": 21, "idMal": 21, "format": "TV", "episodes": null, "duration": 24, "averageScore": 88,
            "isAdult": false, "genres": ["Action", "Adventure"], "synonyms": ["OP"],
            "title": { "romaji": "ONE PIECE", "english": "ONE PIECE", "native": "ワンピース" },
            "startDate": { "year": 1999 },
            "description": "Gold Roger was known as the <i>Pirate King</i>.<br>\n<br>\nLuffy sets sail. (Source: Crunchyroll)",
            "coverImage": { "extraLarge": "https://img/xl.jpg", "large": "https://img/l.jpg", "medium": "https://img/m.jpg" },
            "bannerImage": "https://img/banner.jpg",
            "studios": { "nodes": [{ "name": "Toei Animation" }] },
            "tags": [{ "name": "Pirates", "rank": 95, "isMediaSpoiler": false },
                     { "name": "Twist", "rank": 99, "isMediaSpoiler": true },
                     { "name": "Ensemble Cast", "rank": 97, "isMediaSpoiler": false }]
        })
    }

    #[test]
    fn descriptions_lose_tags_and_sources() {
        assert_eq!(
            plain_description("A <i>pirate</i>.<br>\n<br>\nLuffy. (Source: Crunchyroll)"),
            "A pirate.\n\nLuffy."
        );
        assert_eq!(plain_description("Tom &amp; Jerry"), "Tom & Jerry");
    }

    #[tokio::test]
    async fn search_and_strict_automatic_match() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(body_partial_json(serde_json::json!({ "variables": { "search": "One Piece", "formats": SERIES_FORMATS } })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": { "Page": { "media": [one_piece()] } }
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(body_partial_json(
                serde_json::json!({ "variables": { "search": "Peaky Blinders" } }),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": { "Page": { "media": [one_piece()] } }
            })))
            .mount(&server)
            .await;
        let provider = AnilistProvider::with_base_url(server.uri());

        let found = provider
            .search(MediaKind::Series, "One Piece", None, "pl")
            .await
            .unwrap();
        assert_eq!(
            (
                found[0].provider_id.as_str(),
                found[0].title.as_str(),
                found[0].year
            ),
            ("21", "ONE PIECE", Some(1999))
        );
        assert_eq!(found[0].poster_url.as_deref(), Some("https://img/m.jpg"));

        let matched = provider
            .find_series(&SeriesQuery { title: "One Piece" }, "pl")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(matched.fields["anilist_id"], 21);
        assert_eq!(matched.fields["mal_id"], 21);
        assert_eq!(matched.fields["rating"], 8.8);
        assert_eq!(matched.fields["original_title"], "ワンピース");
        assert_eq!(
            matched.fields["studios"],
            serde_json::json!(["Toei Animation"])
        );
        // Spoiler tags stay hidden, the rest by rank.
        assert_eq!(
            matched.fields["tags"],
            serde_json::json!(["Ensemble Cast", "Pirates"])
        );
        assert_eq!(matched.poster_url.as_deref(), Some("https://img/xl.jpg"));
        assert_eq!(
            matched.backdrop_url.as_deref(),
            Some("https://img/banner.jpg")
        );
        // AniList's best answer for a live-action series is not taken.
        assert!(
            provider
                .find_series(
                    &SeriesQuery {
                        title: "Peaky Blinders"
                    },
                    "pl"
                )
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn an_anilist_id_is_looked_up() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(body_partial_json(
                serde_json::json!({ "variables": { "id": 21 } }),
            ))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "data": { "Media": one_piece() } })),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(body_partial_json(
                serde_json::json!({ "variables": { "id": 999999 } }),
            ))
            .respond_with(ResponseTemplate::new(404).set_body_json(serde_json::json!({
                "errors": [{ "message": "Not Found.", "status": 404 }], "data": { "Media": null }
            })))
            .mount(&server)
            .await;
        let provider = AnilistProvider::with_base_url(server.uri());

        let candidate = provider
            .lookup_by_id(MediaKind::Series, &DirectId::Numeric("21".into()), "pl")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            (candidate.title.as_str(), candidate.year),
            ("ONE PIECE", Some(1999))
        );
        assert!(candidate.overview.starts_with("Gold Roger"));
        assert!(
            provider
                .lookup(MediaKind::Series, "999999", "pl")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            provider
                .lookup_by_id(MediaKind::Series, &DirectId::Imdb("tt0388629".into()), "pl")
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn rate_limits_are_request_errors() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(429))
            .mount(&server)
            .await;
        let provider = AnilistProvider::with_base_url(server.uri());
        assert!(matches!(
            provider.search(MediaKind::Movie, "Akira", None, "pl").await,
            Err(ProviderError::Request(_))
        ));
    }

    #[test]
    fn provider_key_is_anilist() {
        assert_eq!(AnilistProvider::new().provider_key(), "anilist");
    }
}
