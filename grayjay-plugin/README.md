# filestr — Grayjay source plugin

A [Grayjay](https://grayjay.app/) source plugin that lists and plays whatever
the **filestr app** on the same device can serve — its own shares plus
everything reachable through its grant graph — by talking to filestr's local
HTTP gateway over loopback.

```
Grayjay (plugin)  ──HTTP/localhost──►  filestr app's [http] gateway
                                          ├─ GET /files          (browse)
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

## Files

- `FilestrConfig.json` — plugin config. `packages: ["Http"]`,
  `allowUrls` limited to the loopback gateway, and a `serverUrl` setting
  (default `http://127.0.0.1:11780`).
- `FilestrScript.js` — the plugin script.
- `test/harness.js` — a Node test harness (see below).

## Use it in Grayjay

1. Run the filestr app (it starts the gateway on `127.0.0.1:11780`).
2. In Grayjay: enable Developer Mode, start the DevServer, and load this
   plugin's `FilestrConfig.json` (serve this folder over HTTP, e.g.
   `npx serve`, and point the DevServer at the config URL), then Inject Plugin.
   See `grayjay-android/plugin-development.md`.
3. The filestr source appears under Sources; its Home feed is everything the
   filestr node can serve.

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
