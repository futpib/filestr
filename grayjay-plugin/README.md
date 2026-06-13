# filestr — Grayjay source plugin

A [Grayjay](https://grayjay.app/) source plugin that lists and plays whatever
the **filestr app** on the same device can serve — its own shares plus
everything reachable through its grant graph — by talking to filestr's local
HTTP gateway over loopback.

```
Grayjay (plugin)  ──HTTP/localhost──►  filestr app's [http] gateway
                                          ├─ GET /files          (browse)
                                          ├─ GET /search?q=      (federated)
                                          ├─ GET /playlists      (folder/album/artist groups)
                                          └─ GET /file/{hash}    (stream, Range)
                                                 └─ iroh fetch from peers as needed
```

## How it fits together

- The filestr daemon exposes a read-only loopback gateway when configured with
  `[http] listen = "127.0.0.1:11780"`. The filestr Android app enables this
  automatically (see `app/lib/filestr_layout.dart`).
- This plugin (`FilestrConfig.json` + `FilestrScript.js`) implements
  `getHome` / `search` / `getContentDetails`, mapping each file to a Grayjay
  `PlatformVideo` whose stream URL is `…/file/{hash}`. The gateway streams the
  bytes (fetching from a peer over iroh first if needed) with HTTP Range
  support, so Grayjay's player can seek.
- Each peer (and this node) is a **channel**; its **Playlists tab**
  (`getChannelPlaylists`) groups that source's files by folder, album tag and
  artist tag, so a tagged collection browses like a music library.

## Files

- `FilestrConfig.json` — plugin config. `packages: ["Http"]`,
  `allowUrls` limited to the loopback gateway, and a `serverUrl` setting
  (default `http://127.0.0.1:11780`).
- `src/FilestrScript.ts` — **the plugin source** (TypeScript). Edit this.
- `FilestrScript.js` — the compiled, committed artifact `filestrd` embeds
  (`include_str!`). **Generated — don't hand-edit.**
- `test/harness.js` — a Node test harness (see below).

## Authoring (TypeScript)

The plugin is written in TypeScript and type-checked against
[`@types/grayjay-source`](https://gitlab.com/kaidelorenzo/grayjay-plugin-types)
(the de-facto community types — more complete than the app's bundled
`plugin.d.ts`; it declares e.g. `getChannelPlaylists`). Grayjay assigns plugin
methods to a global `source: Source`, so type-checking turns a **misnamed
method** — like the old `getPlaylistsUser` when Grayjay only ever calls
`getUserPlaylists` — into a compile error instead of a method Grayjay silently
never invokes. (That exact typo once made album/artist playlists simply not
appear.)

```sh
cd grayjay-plugin
npm install           # once: installs typescript + the pinned types
npm run build         # tsc → FilestrScript.js (single classic script, no bundler)
npm run typecheck     # tsc --noEmit, just the type check
```

`tsconfig.json` uses `module: none` + `outFile`, so the import-free source emits
one plain script (no module preamble) — exactly what Grayjay's V8 host wants.
After editing the `.ts`, **rebuild and commit `FilestrScript.js`** alongside it.
`scripts/autotests/test-grayjay-typecheck.sh` enforces both that the types check
and that the committed `.js` matches the `.ts` (no drift).

## Use it in Grayjay (plug and play)

The filestr daemon **serves this plugin itself** from its gateway, so there's no
separate web server or DevServer:

- `GET /grayjay/FilestrConfig.json` — config (its URLs are rewritten to the
  gateway's actual host:port at request time)
- `GET /grayjay/FilestrScript.js`, `GET /grayjay/filestr.png`

So you just point Grayjay at `http://127.0.0.1:11780/grayjay/FilestrConfig.json`.
The **Android app has an "Add to Grayjay" button** that does this in one tap
(it fires a VIEW intent at Grayjay's `AddSourceActivity` — see below).

The filestr source then appears under Sources; its Home feed is everything the
filestr node can serve, and opening an item plays it.

### Why a button (and not Grayjay's "Install by URL")

Grayjay's in-app **Install by URL only accepts `https://`** (or a
`grayjay://plugin/…` deep link) — a plain `http://127.0.0.1` URL is rejected
("not a plugin url"). But Grayjay's exported `AddSourceActivity` *does* accept an
`http` URL via a `VIEW` intent, so the app launches that directly:

```
Intent(ACTION_VIEW, "http://127.0.0.1:11780/grayjay/FilestrConfig.json")
  .setClassName("com.futo.platformplayer",
                "com.futo.platformplayer.activities.AddSourceActivity")
```

The plugin is unsigned (self-served), so Grayjay shows a "Missing Signature"
warning on install — tap "Install Anyway".

## Test

The harness loads Grayjay's **own** injected scaffolding
(`polyfil.js` + `source.js`) so the plugin runs against the real runtime
contracts, mocks the `http` package (synchronous, via `curl`), runs the plugin
against a live filestr gateway, and verifies that the stream URLs it returns
actually serve the correct bytes (full + Range/206).

```sh
# against a filestr daemon with [http] listen = 127.0.0.1:11780
node test/harness.js
# or against another gateway base url
node test/harness.js http://127.0.0.1:21780
```

## Verified in real Grayjay

Installed the official Grayjay APK (`releases.grayjay.app/app-x86_64-release.apk`)
on an emulator, enabled Developer Mode, started the DevServer, and injected this
plugin (`POST /plugin/loadDevPlugin`, with the script served over HTTP). In the
actual app: the filestr source's Home feed listed the files the filestr app
serves, and opening one **played the video** — Grayjay's player streamed it from
`http://127.0.0.1:11780/file/{hash}`, which the filestr app fetched from a peer
over iroh (HTTP Range → ExoPlayer seeking).

### Gotchas the JS harness can't catch (only real Grayjay does)

- **`allowUrls` matches the URL host only, no port.** Use `"127.0.0.1"`, not
  `"127.0.0.1:11780"` — otherwise `http.GET` throws "non-whitelisted url".
- **Avoid `Text` setting defaults that aren't valid JSON.** Grayjay's
  `parseSettings` runs `JSON.parse` on every string setting value, so a default
  like `http://127.0.0.1:11780` crashes `enable`. The gateway URL is a fixed
  constant in the script instead.
