# filestr — media-consumer gaps

What's missing when a **media player** (Grayjay, or any HTTP player: VLC, Kodi,
ExoPlayer) consumes filestr through the loopback HTTP gateway
([`filestrd/src/http_bridge.rs`](filestrd/src/http_bridge.rs)) and the Grayjay
source plugin
([`grayjay-plugin/FilestrScript.js`](grayjay-plugin/FilestrScript.js)).

This is a gap analysis, not a spec. Findings are grounded in the current code
with `file:line` references and ordered by consumer impact per unit of effort.

## What we serve today

- `GET /files` — flat JSON list of `{name, hash, size, source}`, aggregated
  from local shares + a live browse of every peer.
- `GET /file/{hash}` — the bytes, with single-range HTTP `Range`/`206`,
  `Accept-Ranges: bytes`, and `Content-Type` guessed from the filename.
- `GET /grayjay/…` — the embedded plugin (config/script/icon).
- Plugin: `getHome` / `search` / `getContentDetails` / `isContentDetailsUrl`,
  mapping each playable file to a `PlatformVideo`, with an audio/video search
  filter.

---

## 1. We download the whole file before playing it  *(biggest gap)*

`serve_file` calls `transfers::ensure_local(state, hash)`
([`http_bridge.rs:246`](filestrd/src/http_bridge.rs)) before streaming, and
that function "ensure[s] `hash` is present in the local store, **fetching the
whole blob** from a known source if needed"
([`transfers.rs:214`](filestrd/src/transfers.rs)). Only once the entire file is
local do we `export_ranges` and serve the requested range.

So a player's opening `Range: bytes=0-1` probe pulls the **entire file over
iroh** before returning 2 bytes. Consequences:

- No instant start / no progressive playback — first frame waits on a full
  download.
- Seeking near the end forces downloading everything before it.
- A `bytes=-65536` tail probe (MP4 `moov` atom at EOF) pulls the whole file.
- Large files feel broken on slow links even though Range *looks* supported.

**Fix:** translate the HTTP Range into an iroh-blobs **ranged** request and
stream peer → store → client as bytes arrive (bao supports verified ranged
fetch). Turns a download-then-serve gateway into a streaming one. Largest
effort, largest payoff.

## 2. The feed is filenames and blank tiles

Everything that makes a library browsable is absent:

- **Duration is always 0** — `PlatformVideo` / `PlatformVideoDetails` and the
  `VideoUrlSource` hardcode `duration: 0`. No track lengths, no total on the
  seek bar.
- **No thumbnails / album art / poster frames** — `Thumbnails([])` everywhere;
  the Home feed is a wall of identical blank tiles.
- **Titles are raw filenames** — `baseName(f.name)`; ID3 / container tags
  (artist, title, album) are ignored, so it's "Carefree.mp3", not
  "Carefree — Kevin MacLeod".
- **Dates are fake** — `datetime: nowSeconds()`, so everything is "just now",
  sort-by-date is meaningless, and items reshuffle on each fetch.

**Fix:** extract metadata at index time (audio: `symphonia`; video: MP4/Matroska
box parse) and surface it in `/files`; add a `/thumb/{hash}` endpoint and
album-art passthrough.

## 3. HTTP correctness players rely on

- **No HEAD** — the gateway is GET-only; `route` returns `405` for anything
  else ([`http_bridge.rs:77-78`](filestrd/src/http_bridge.rs)). Many players
  `HEAD` first to probe size / type / Range support; we reject it.
- **No `Last-Modified` / `ETag` / `If-Range`** — no validators, so no
  client/proxy caching and no safe resume; re-opening re-streams from scratch.
- **Content-Type is extension-only** (`content_type()`,
  [`http_bridge.rs:332`](filestrd/src/http_bridge.rs)) with no sniffing — a
  correctly-encoded file with a missing/wrong extension becomes
  `application/octet-stream` and won't play (and the plugin hides it from the
  list entirely via `isPlayable`).
- **No `Content-Disposition`** — no filename hint for a "download" action.

## 4. Browse structure & scale

- **Flat mixed feed, no channels/playlists** — the plugin implements only
  `getHome` / `search` / `getContentDetails`; there's no `getChannel` /
  `getPlaylist`. Peers are the natural "channels" and shared folders the
  natural "albums/playlists," but `baseName()` discards the directory path, so
  all structure is lost.
- **No pagination** — `/files` returns the whole library in one response and
  `FilestrVideoPager.nextPage()` always returns `[]`; a real library is one
  giant payload.
- **`getHome` re-browses every peer on every call** — `list_files` loops peers
  synchronously over iroh ([`http_bridge.rs:130-158`](filestrd/src/http_bridge.rs)),
  so it's slow and one dead/slow peer stalls the whole feed (the 408 we hit).
  Should be cached / incremental.
- **Search is shallow** — the plugin filters the already-aggregated `/files` by
  substring (`listVideos`); it does **not** use the daemon's grant-graph
  federated search ([`filestrd/src/search.rs`](filestrd/src/search.rs)). The
  only sort/filter offered is the audio/video toggle (`sorts: []`).

---

## Priority (impact per effort)

| # | Item | Effort | Notes |
|---|---|---|---|
| 1 | `HEAD` + `Last-Modified`/`ETag` | low | self-contained; unblocks pickier players + caching |
| 2 | Duration + tag-based titles in the index, surfaced in `/files` | med | biggest *visible* feed upgrade; audio easy, video needs a box parse |
| 3 | Thumbnail / album-art endpoint | med | turns the blank wall into a real library |
| 4 | Ranged peer fetch (true streaming) | high | the architectural fix; largest payoff for big media |
| 5 | Cache the peer-browse + paginate | med | robustness/scale for real libraries |

Lower priority / likely out of scope: HLS/DASH adaptive streaming &
transcoding, subtitles / multiple audio tracks / chapters, sort options beyond
name.
