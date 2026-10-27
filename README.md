# LisekMovie-plugin-anilist

AniList metadata provider extension for [LisekMovie](https://github.com/LisekMovieApp/LisekMovie-app), the self-hosted video-on-demand server.

| | |
|---|---|
| Provider key | `anilist` |
| Covers | Anime |
| Status | Basic — works, parts still missing |
| API key | None (public GraphQL API, 90 requests/min) |
| Fix match ids | `anilist:21`, `https://anilist.co/anime/21`, or `21` with AniList picked |

## What it does

AniList GraphQL API: search (films = format MOVIE, series = TV, TV_SHORT, ONA, OVA, SPECIAL; optional year), lookup by AniList id with English/romaji/native titles, synopsis (HTML stripped), average score, genres, ranked non-spoiler tags, main studio, MyAnimeList id, cover and banner. Automatic matching is strict: one of AniList's titles or synonyms must equal the scanned title.

## How it plugs in

The crate implements `lisek_plugin_api::MetadataProvider` and is compiled into `lisek-server` as a Cargo path dependency — there is no runtime plugin loading. Clone it **next to** `LisekMovie-app`:

```
LisekMovie/
  LisekMovie-app/
  LisekMovie-plugin-anilist/
  ...
```

The server lists the provider under **Admin → Plugins** (disabled by default). Turn it on, add the key if it needs one, and set its priority — globally, or per library (for example AniList or aniDB first in an anime library). `install-linux.sh` and `update-linux.sh` clone and update it with the other modules.

## Development

```bash
cargo clippy --all-targets -- -D warnings
cargo test
```

Tests run against a local `wiremock` server; nothing calls the real service.

## License

[MIT](LICENSE), like LisekMovie itself.
