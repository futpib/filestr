# filestr — media-consumer gaps

What's still missing when a **media player** (Grayjay, or any HTTP player: VLC,
Kodi, ExoPlayer) consumes filestr through the loopback HTTP gateway
([`filestrd/src/http_bridge.rs`](filestrd/src/http_bridge.rs)) and the Grayjay
source plugin ([`grayjay-plugin/FilestrScript.js`](grayjay-plugin/FilestrScript.js)).

Gap analysis, not a spec: **open items only**, grounded in the current code and
ordered by consumer impact per unit of effort. Shipped work is removed, not kept.

## What we serve today

- `GET /files` — JSON list of `{name, hash, size, source, media, thumb}` plus a
  `peers` reachability array, aggregated from local shares + a bounded,
  concurrent browse of every peer.
- `GET /file/{hash}` — ranged streaming (`Range`/`206`, `Accept-Ranges`, strong
  `ETag` + `If-Range`/`304`, HEAD), magic-byte–sniffed `Content-Type`, fetched
  from a peer window-by-window on demand (no whole-file staging, partial-blob
  reuse).
- `GET /thumb/{hash}` — embedded cover art (audio).
- `GET /search?q=` — federated grant-graph search, tag-aware.
- `GET /grayjay/…` — the embedded plugin.
- Plugin: home / search (+ audio·video filter) / content details / peers as
  channels / folders·albums·artists as playlists / offline-peer signalling.

## Open gaps

### Feed metadata
- **Some durations missing** — mkv/webm aren't reliably populated by `symphonia`,
  and a CBR MP3 with no Xing/Info header reports none.

### Thumbnails
- **No video poster frames** — needs a frame decoder / ffmpeg (out of the
  pure-Rust budget for now); audio cover art is done.
- **Peer cover art isn't fetched** — the gateway only caches art for its own
  local shares, so a peer's tiles stay blank.

### HTTP correctness
- **No `Content-Disposition`** — no filename hint for a player's "download"
  action.

### Browse structure & scale
- **No pagination** — `/files` returns the whole library in one response and
  `FilestrVideoPager.nextPage()` returns `[]`; a large library is one giant
  payload.
- **Peer browse isn't cached** — `list_files` re-browses every peer on each call.
  It's concurrent and per-peer bounded now (a dead peer no longer stalls the
  feed), but results aren't cached/incremental, so every call pays the
  round-trips.
- **No sort options** (`sorts: []`). (Peer/federated hits now *do* carry full
  media — title/artist/album/duration/content-type/mtime — because the p2p hit
  embeds the canonical `FileEntry`; only cover-art bytes are still local-only,
  tracked under Thumbnails → "Peer cover art isn't fetched".)

## Native device-library integration (Grayjay "Files" tab)

Grayjay's **Library → Files** tab (Artists / Albums / Videos / Directories) is
its built-in `LocalClient` (platform id `"LOCAL"`), backed by Android
**MediaStore** and SAF directories — `StateLibrary.getArtists()/getAlbums()/
getVideos()` query `MediaStore.Audio.*` / `MediaStore.Video.*` directly, and
`addFileDirectory()` opens the SAF directory picker
(`requestDirectoryAccess` → `takePersistableUriPermission` →
`DocumentFile.fromTreeUri`) and scans the tree.

**This tab is not plugin-extensible** — a source plugin can't inject
Artists/Albums/Videos, and there's no plugin content type for "artist"/"album".

**The one bridge is the Directories (+) / SAF picker:** Grayjay indexes any
folder the user grants it. So if filestr stored its shares/downloads in
**user-visible storage** instead of the app-private sandbox — the fix tracked in
[`app/README.md` → "Known limitation: sandbox-only storage"](app/README.md) — the
user could add that folder once and every filestr file would appear natively in
Artists/Albums/Videos (browsable by tag, offline, native player). A **no-copy
alternative** exposes the whole library (incl. on-demand peer content) as a
virtual filesystem via a `DocumentsProvider` + `StorageManager.openProxyFileDescriptor`
(Android's app-level FUSE equivalent), which the Directories picker accepts over
SAF — sketched in [`FEATURE-VIRTUAL-FILESYSTEM.md`](FEATURE-VIRTUAL-FILESYSTEM.md).

Effort: high (gated on the storage refactor / DocumentsProvider). Caveat:
MediaStore's own Artists/Albums tabs still won't index a DocumentsProvider —
filestr would appear under the Directories browse, not merged into those tabs.

## Priority (impact per effort)

| Item | Effort | Notes |
|---|---|---|
| Cache the peer-browse + paginate | med | scale for real libraries |
| `Content-Disposition` | low | filename hint for a download action |
| Cover art on peer browse/search hits | med | last missing piece of peer tiles (media tags + dates already carried) |
| Video poster frames | high | needs a frame decoder / ffmpeg |
| Native device-library integration (SAF dir / virtual FS) | high | see §above + `FEATURE-VIRTUAL-FILESYSTEM.md` |

Out of scope for now: HLS/DASH adaptive streaming & transcoding, subtitles /
multiple audio tracks / chapters, sort options beyond name.

## Tests

User-story e2e tests live in [`scripts/autotests/`](scripts/autotests) (real
daemons on localhost; run via `run-all.sh`) and cover the HTTP gateway, ranged
peer streaming, metadata/thumbnails, federated search, and the Grayjay plugin
(home / search / channels / playlists / album·artist / content-details / offline
peers).
