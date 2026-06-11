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

## 1. ~~We download the whole file before playing it~~  *(done)*

**Done.** `serve_file` no longer fetches the whole blob first. It resolves the
size without fetching (`known_size`), then streams the requested `[start, end)`:

- **Fully-local blob** → streamed straight from the store per leaf
  (`stream_local`), zero extra buffering — the path for a node's own shares.
- **Not-yet-local blob** → fetched from a peer **one `WINDOW` (4 MiB) at a
  time** and emitted as each window lands (`stream_windowed` →
  `transfers::fetch_range` → bao-verified `ChunkRanges` get). An open-ended
  `bytes=0-` starts playing after the first window instead of staging the whole
  file; a seek opens a new range and we start a fresh window there. If the
  client disconnects, the body future drops and we stop fetching.

Verified end-to-end against a second node: a middle 100-byte range returned the
correct bytes in ~50 ms (positioned fetch, not a 12 MB download); full GET and
open-ended ranges reassembled to a byte-identical SHA-256.

Remaining nuance (follow-up, not blocking): each window does fetch-then-emit, so
within a window there's no overlap of transfer and delivery, and re-playing a
peer file re-fetches windows (the partial blob isn't reused across requests
unless the whole blob completes). True pipe-through (peer → client as bytes
verify, with store reuse) is a later refinement.

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

- ~~**No HEAD**~~ — **done.** Both endpoints answer HEAD (size/type/Range probe
  with no transfer); a HEAD on `/file/{hash}` resolves the size from the local
  store / index / a recent browse without fetching the blob.
- ~~**No `ETag` / `If-Range`**~~ — **done.** `/file/{hash}` carries a strong
  `ETag` (the content hash — content-addressed bytes are immutable), with
  `If-None-Match` → `304` and `If-Range` honoured. `Last-Modified` was skipped
  deliberately: it needs per-file mtime plumbing (part of #2) and adds nothing
  over an immutable strong validator.
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
| 1 | ~~`HEAD` + `ETag`/`If-Range`~~ | low | **done** — self-contained; unblocks pickier players + revalidation |
| 2 | Duration + tag-based titles in the index, surfaced in `/files` | med | biggest *visible* feed upgrade; audio easy, video needs a box parse |
| 3 | Thumbnail / album-art endpoint | med | turns the blank wall into a real library |
| 4 | ~~Ranged peer fetch (true streaming)~~ | high | **done** — windowed peer fetch; open-ended ranges start without staging the whole file |
| 5 | Cache the peer-browse + paginate | med | robustness/scale for real libraries |

Lower priority / likely out of scope: HLS/DASH adaptive streaming &
transcoding, subtitles / multiple audio tracks / chapters, sort options beyond
name.

## Tests

User-story e2e tests live in [`scripts/autotests/`](scripts/autotests) (real
daemons on localhost; run via `run-all.sh`):

- `test-http-gateway.sh` — a player lists / HEAD-probes / range-streams a local
  file, revalidates with the ETag (304, If-Range), gets the right content-type,
  and fetches the embedded plugin. Covers **#1** and local range streaming.
- `test-http-stream-peer.sh` — a player streams a 9 MiB **peer-hosted** file by
  range through the gateway; asserts the gateway does **not** download the whole
  file to answer a HEAD or a partial range, and that open-ended/full GETs
  reassemble byte-identically. Covers **#4**.
- `test-grayjay-plugin.sh` — runs the plugin's own JS harness against a live
  gateway: media-only listing (a `.txt` is hidden), audio/video filter, and
  range-streamed playback through the plugin's URLs.
