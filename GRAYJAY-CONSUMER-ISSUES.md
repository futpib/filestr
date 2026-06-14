# Grayjay consumer issues — screenshot triage

Issues found from a real Grayjay-on-Android session against filestr (a content
page for an Infected Mushroom track served by peer `874a…`), **reproduced in the
emulator** against the same live peer (the app's loopback gateway exposed its
14,342-file browse). Ordered by user impact. Status reflects what's fixed vs.
open as of this doc.

The reproduction setup: emulator app (latest APK) peered with the real `874a`
node (Online), its gateway forwarded to the host (`adb forward`) and inspected
directly. Symptoms were confirmed in that live data, not just synthesized.

## 1. `-12:-55` duration on the played track — FIXED (client side)

**Symptom.** The seekbar reads `00:00 / -12:-55`; the player can't scrub.

**Root cause.** `getContentDetails(url)` resolved the duration by downloading the
*entire* library (`GET /files` browses every peer) to find one row. For a large
/ slow peer (here 874a, 14,342 files) that response arrives late or incomplete,
the row isn't found, `duration` falls to `0`, and Grayjay frame-guesses the
length → the `-12:-55` seekbar (see the `sourceDescriptor` comment: an audio
source with duration 0 makes the player guess). It was **intermittent** —
the same file (`12 - On the Road Again.flac`, 23.3 MiB) showed `-12:-55` once and
a correct duration on a later browse. Not a missing-duration-at-source problem
for that file; a lookup-flakiness problem.

**Fix.** Embed the full `FileEntry` (which the plugin already holds when building
the card) in the content-page URL (`&m=`), and have `getContentDetails` read it
back — zero network, no cache, deterministic. The player still streams from the
plain `/file` URL. Commit: *"Resolve content-details metadata from the URL, not a
full re-browse."* Regression test in `content-details.test.ts` deletes the file
so every lookup misses and asserts the duration survives from the URL.

**Not covered by this fix:** files whose *source* genuinely reports no duration —
see issue 6.

## 2. Duplicate recommendations — OPEN

**Symptom.** "Infected Mushroom — She Zoremet" appears twice, identical (both
5:15, both `874a`).

**Root cause.** Confirmed in live data: 874a really has two rips —
`01. Infected Mushroom - She Zoremet.flac` and `01 - She Zoremet.flac` (different
files → different hashes, near-identical duration). `list_related`
([`http_bridge.rs`](filestrd/src/http_bridge.rs) `list_related`) **dedups by hash
only**, so same-song-different-bytes copies both pass. Reproduced deterministically
with two synthetic same-title files → `/related` returned both.

**Proposed fix.** Dedup recommendations by a content key like
`(artist, title, round(duration))` (or `(displayName, size)`), not just hash.
Low effort. Keep hash-dedup as the first pass; collapse near-identical siblings.

## 3. Non-media files surfaced as playable "tracks" — OPEN

**Symptom (found while reproducing).** Junk entries in the feed: an image
`00-ugress-unicorn-2008-back_scan-prs.jpg` showed up as a video; logs/playlists
(`*.log`, `*.txt`, `*.m3u`, `auCDtect.txt`) are served as `audio/mpeg`.

**Root cause.** Two layers:
- `isPlayable` ([`FilestrScript.ts`](grayjay-plugin/src/FilestrScript.ts)) only
  excludes `application/octet-stream`, so `image/*` (and anything else) passes.
- Content sniffing mis-tags some text/log files as `audio/mpeg` (a byte run that
  matches the MP3 frame-sync heuristic).

**Proposed fix.**
- `isPlayable` should *require* an `audio/` or `video/` content-type prefix
  (allowlist), not just "not octet-stream". Low effort, removes images outright.
- Tighten the MP3 sniff (require a valid frame header, or don't sniff non-media
  extensions as audio). Medium.

## 4. No cover art on peer tiles — OPEN (known gap)

**Symptom.** Every tile and the player show the generic `fs` placeholder.

**Root cause.** The gateway only extracts/caches cover art for *local* shares;
peer cover-art bytes aren't fetched. Tracked in
[`MEDIA-CONSUMER-GAPS.md`](MEDIA-CONSUMER-GAPS.md) (Thumbnails → "Peer cover art
isn't fetched"). Medium effort.

## 5. Channel shown as a raw node-id — OPEN (minor / config)

**Symptom.** The creator/channel reads `874a188ebafe…` instead of a name.

**Root cause.** That peer has no label set (the emulator's other peers show
`android` / `grayjay`). The plugin falls back to the short node id when a peer is
unlabeled. Fix is UX: prompt for / allow editing a peer label, and/or surface the
peer's self-advertised name if we add one to the protocol.

## 6. Some files have no duration *at the source* — OPEN (indexer side)

**Symptom.** ~359 of 11,014 audio files on 874a report no duration right now;
several also have `content_type: null` (e.g. `02. Long Arm - The Ashes.mp3`,
various `.flac`). These will still show `-12:-55` after issue 1's fix, because
there is genuinely no duration to embed.

**Root cause.** The serving node's index has no media for those files —
consistent with an **incomplete / in-progress index** on 874a (some files in the
same album have full media, some have none), or a per-file extraction miss.
`symphonia` normally estimates a duration even for headerless CBR/VBR MP3 and
ADTS AAC (verified — synthetic no-header files still got durations), so a true
null is unusual and points at not-yet-indexed files.

**Proposed fix.** Indexer side: ensure media extraction completes for every
served file (don't serve a file as playable until its media is probed), and
re-probe `content_type: null` entries. Separately, the plugin could avoid handing
Grayjay a bogus seekbar when duration is unknown (e.g. present as unknown-length
rather than 0). Medium.

## Not bugs (verified)

- **Dates** ("10 years ago" on the phone): the mtime fix working — that's the
  real file mtime, stable and correct. (The emulator's Grayjay showed
  "0 seconds ago" only because it had the *old cached plugin*; Grayjay re-fetches
  a source plugin only on a version bump — see
  [`reference_grayjay_plugin_version`].)

## Reproduction notes

- Emulator app gateway (inside the device) listens on `127.0.0.1:11780`; forward
  with `adb forward tcp:<local> tcp:11780` and hit `/files`, `/related?hash=`,
  `/search?q=` to inspect exactly what a consumer sees.
- The played file's identity was confirmed by size: `12 - On the Road Again.flac`
  is 23.3 MiB, matching the screenshot's "23.3 MiB".
- Grayjay's content feed renders `/related` as a content pager, so the Recommended
  tab is issue 2's surface.
