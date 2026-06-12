# Feature to consider — expose the filestr library as a virtual filesystem

**Status:** idea / not started. A design sketch, not a spec.

**One line:** present everything filestr can serve — local shares *and*
on-demand peer content — to other Android apps as a browsable, seekable file
tree, **without copying anything into storage**, by implementing a
`DocumentsProvider` backed by `StorageManager.openProxyFileDescriptor`.

## Why

filestr's bytes live in the app-private, content-addressed blob store; peer
content isn't even local until fetched. Today nothing outside the app can see
them, which is the root of two tracked gaps:

- [`app/README.md` → "Known limitation: sandbox-only storage"](app/README.md) —
  shares/downloads aren't user-visible.
- [`MEDIA-CONSUMER-GAPS.md` §5](MEDIA-CONSUMER-GAPS.md) — Grayjay's native
  Library (Artists/Albums/Videos/**Directories**) can't show filestr content.

The §5 backlog answer was "copy downloads into `Music/filestr` so MediaStore
scans them." That duplicates bytes, only covers already-downloaded files, and
needs scoped-storage plumbing. A virtual filesystem is a cleaner answer: the
*whole* library (including not-yet-fetched peer files) appears as files, streamed
on demand, with zero duplication and the content staying content-addressed.

## Is there FUSE on Android?

- **Classic FUSE (mount a userspace fs): not for apps.** The kernel has FUSE
  (Android itself uses it for `/sdcard` scoped storage via MediaProvider's
  FuseDaemon since Android 11), but `mount()` / `/dev/fuse` need root or system
  privilege — an `untrusted_app` can't.
- **App-level FUSE equivalent: `StorageManager.openProxyFileDescriptor(mode,
  ProxyFileDescriptorCallback, handler)`** (API 26+). The OS returns a
  FUSE-backed fd and routes `onGetSize` / `onRead(offset, size, …)` /
  `onWrite` / `onRelease` callbacks to our code. It is **seekable random
  access** (unlike a pipe `ParcelFileDescriptor`), so a media player can scrub.

## Design sketch

1. **Backing reads.** A `ProxyFileDescriptorCallback` per opened file:
   - `onGetSize()` → the file's known size (the daemon resolves this without
     fetching: local store / index / recent browse — same path `HEAD /file/{hash}`
     uses).
   - `onRead(offset, size, data)` → bytes `[offset, offset+size)` from the
     daemon's ranged stream. The gateway already serves seekable `Range`/`206`
     and reuses partial blobs, so this maps directly; peer content arrives via
     the existing windowed fetch.
   Run reads on a dedicated `HandlerThread`; the callback is blocking.

2. **DocumentsProvider.** A `ContentProvider` exposing a virtual tree:
   - `queryChildDocuments` → sources/folders (or album/artist tags, see
     `MEDIA-CONSUMER-GAPS.md` §4) as directories; playable files as documents.
   - `queryDocument` → name, MIME (the daemon already sniffs content-type),
     size, flags.
   - `openDocument(mode="r")` → the proxy fd from step 1.
   Read-only to start; `mode="w"` could later let other apps drop files into a
   share.

3. **Discovery surfaces.** Other apps reach it via the Storage Access Framework
   (tree/document `content://` URIs):
   - **Grayjay's Directories (+)** — *verified* it works over any
     DocumentsProvider, not just MediaStore: it stores `content://` tree URIs
     and enumerates with `DocumentFile.fromTreeUri(...).listFiles()`
     (`grayjay-android` `StateLibrary.kt:854`) and opens files via
     `DocumentFile.fromSingleUri` → `ContentResolver.openFileDescriptor`
     (`StateLibrary.kt:365`). So adding the filestr provider's tree makes filestr
     media show up natively, streamed on demand.
   - Any SAF-aware app (file managers, VLC, Kodi via SAF, …).

## Caveats / risks

- **MediaStore-only tabs won't index it.** Grayjay's Artists/Albums/Videos
  (and the system's media library) scan MediaStore, which does *not* index
  DocumentsProviders. filestr content appears under Grayjay's **Directories**
  browse (the `(+)` tab), not auto-merged into Artists/Albums. (MediaStore
  inclusion would still require the copy-to-public-storage path of §5.)
- **Blocking reads + latency.** `onRead` blocks; for peer content there's fetch
  latency, and the OS may read-ahead aggressively. Fine for local shares;
  needs buffering/read-ahead tuning for remote, and a clear failure mode when a
  peer is unreachable (surface an IO error, don't hang — mirror the bounded
  connect work).
- **Process/lifetime.** The provider runs in the app process and talks to the
  daemon over the control socket / gateway; reads must work whenever the FGS
  daemon is up. Behaviour when the daemon is stopped (notification "Stop")
  must be defined — fail cleanly.
- **Thumbnails/metadata** for the SAF browser come from `queryDocument` flags +
  `openDocumentThumbnail` (can reuse `/thumb/{hash}`).
- Real Android plumbing, but **no root, no storage permission, no copying.**

## Effort & relation to other items

Medium–high (a ContentProvider + proxy-fd worker + tree mapping), but
self-contained and **supersedes the copy-to-public-storage variant** in
`MEDIA-CONSUMER-GAPS.md` §5 for the "browse filestr as files" goal. The two
aren't mutually exclusive: a virtual FS gives instant, no-copy browsing
everywhere; copy-to-MediaStore is only needed if you specifically want entries
in the system's Artists/Albums index.

## Open questions

- Does Grayjay re-scan a Directories tree on demand or only when added? (affects
  whether new filestr content appears without re-adding.)
- Tree shape: sources→folders, or album/artist tags as the top level?
- Write support (drop-to-share) — worth it, or read-only?
- Read-ahead window + timeout policy for peer-backed reads.
