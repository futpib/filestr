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

**Partial-blob reuse — done.** `transfers::fetch_range` now checks the store's
bitfield (`observe`) and skips fetching a range already present in the partial
blob, so replays and re-seeks within previously-fetched ranges serve from the
local store with no peer round-trip (verified by `test-http-replay.sh`: kill the
peer, the fetched range still serves, an un-fetched range doesn't).

**Byte-level pipe-through — done.** A not-yet-local range is now streamed as it
arrives: one background fetch of the whole range, while the body emits the
available contiguous prefix from the store bitfield (`observe`) as each chunk is
bao-verified in (the prefix end is found by binary search over `is_subset`, so
there's no manual chunk-unit math). First byte reaches the player after one
chunk, not a whole window — measured first-byte 0.35 ms vs 134 ms total on a
32 MB peer file, bytes identical. A client disconnect drops the body, which
aborts the fetch.

## 2. The feed is filenames and blank tiles  *(partly done)*

- ~~**Duration is always 0**~~ — **done.** Extracted at index time and shown on
  the tile and player (whole-second `duration`). Audio via `symphonia` (frame
  count × timebase); mp4-family video via the `mp4` container header. (MP3s with
  no Xing/Info header still report no duration — a small remaining gap.)
- ~~**Titles are raw filenames**~~ — **done.** ID3 / Vorbis / mp4 tags are read,
  and the feed shows `Artist — Title` (falling back to the filename). Search now
  matches the tag fields too, so what's shown is findable.
- ~~**No thumbnails / album art**~~ — **done for audio** (see below). Video
  poster frames still pending (need a decoder).
- **Dates are fake** — `datetime: nowSeconds()`, so everything is "just now",
  sort-by-date is meaningless, and items reshuffle on each fetch.

How it works: `filestrd/src/metadata.rs` probes each file during the index scan
(off the async runtime, best-effort — failures yield empty metadata); the
fields ride on `MediaMeta`, which is attached to `IndexedFile`, the `FileEntry`
browse wire (so peer files carry metadata too), and the gateway's `/files`.

Remaining: thumbnails / album art (#3), Matroska/WebM duration, and real
dates.

### Thumbnails / album art — done for audio

At index time the daemon extracts embedded cover art (`symphonia` visuals —
front cover, else the largest image) and caches it under `cache_dir/thumbs/{hash}`.
The gateway serves it at `GET /thumb/{hash}` (sniffed content-type, strong
`ETag`, immutable `Cache-Control`, HEAD-aware), and `/files` carries a
`thumb: true` flag for hashes with cached art. The plugin maps that to a Grayjay
`Thumbnail`, so audio with embedded art shows cover tiles.

Pending: **video poster frames** (need a frame decoder / ffmpeg — out of the
pure-Rust budget for now), cover art for **peer-hosted** files (the gateway only
caches art for its own local shares; a peer's art isn't fetched), and stale-thumb
cleanup on rescan.

## 3. HTTP correctness players rely on

- ~~**No HEAD**~~ — **done.** Both endpoints answer HEAD (size/type/Range probe
  with no transfer); a HEAD on `/file/{hash}` resolves the size from the local
  store / index / a recent browse without fetching the blob.
- ~~**No `ETag` / `If-Range`**~~ — **done.** `/file/{hash}` carries a strong
  `ETag` (the content hash — content-addressed bytes are immutable), with
  `If-None-Match` → `304` and `If-Range` honoured. `Last-Modified` was skipped
  deliberately: it needs per-file mtime plumbing (part of #2) and adds nothing
  over an immutable strong validator.
- ~~**Content-Type is extension-only**~~ — **done.** The content type is sniffed
  from magic bytes at index time (`metadata::sniff_content_type`), stored in
  `MediaMeta.content_type`, and used by the gateway (serving) and the plugin
  (listing/playability). A media file with a missing/wrong extension is now
  detected, listed, and served correctly. (Remaining duration gap: mkv/webm
  duration isn't reliably populated by symphonia, and CBR MP3 without a Xing
  header still reports none.)
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
- ~~**Search is shallow**~~ — **done.** The gateway exposes `GET /search?q=`
  which runs the daemon's grant-graph federated search
  ([`filestrd/src/search.rs`](filestrd/src/search.rs)) — reaching the whole
  reachable graph via TTL forwarding, not just direct peers — and `index.search`
  now matches the tag metadata (title/artist/album), not only the path. The
  gateway records upstream sources so results are playable, and enriches local
  hits with their media/thumbnail. The plugin's search now hits `/search`.
  (Remaining: peer hits don't carry media over the search wire yet, and there
  are still no sort options — `sorts: []`.)

---

## Priority (impact per effort)

| # | Item | Effort | Notes |
|---|---|---|---|
| 1 | ~~`HEAD` + `ETag`/`If-Range`~~ | low | **done** — self-contained; unblocks pickier players + revalidation |
| 2 | ~~Duration + tag-based titles in the index, surfaced in `/files`~~ | med | **done** (audio + mp4 video duration; thumbnails split to #3) |
| 3 | ~~Thumbnail / album-art endpoint~~ | med | **done** for audio (cover art → `/thumb/{hash}`); video frames pending |
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
- `test-media-metadata.sh` — a tagged MP3 and an MP4 (ffmpeg fixtures) surface
  their title/artist/album and duration through `/files`; an MP3 with embedded
  cover art gets a `thumb` flag and a real image at `/thumb/{hash}`; `/search`
  by artist tag finds the file. Covers **#2** and the audio half of thumbnails.
- `test-http-search.sh` — a 3-node chain (A ← B ← G): the gateway's `/search`
  finds a file two hops away that the one-hop `/files` browse can't, and streams
  it (relayed). Covers federated search.
