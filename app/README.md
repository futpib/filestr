# filestr — Android app (files only)

A Flutter/fvm Android front-end for [filestr](../README.md). It bundles the
iroh-only `filestrd` daemon as a native library, runs it inside a **foreground
service** (so it survives the app being backgrounded, like iroh-ssh-android),
and drives it over its unix-socket control protocol — the same JSON-lines
protocol `filestrctl` speaks.

**Scope:** files only. The bundled daemon is built with
`--no-default-features`, so there is no chat/nostr/MLS code in it at all. The
app surfaces identity/status, invitations, peers + browse, grant-graph search,
and downloads.

## How it works

- `scripts/build-android.sh` (repo root) cross-compiles `filestrd` for
  `arm64-v8a` and `x86_64` with `cargo ndk` and copies each binary to
  `android/app/src/main/jniLibs/<abi>/libfilestrd.so`.
- Android only marks files under `nativeLibraryDir` executable, so the daemon
  ships as `libfilestrd.so` (it's a plain executable, not a JNI library). The
  manifest sets `extractNativeLibs="true"` and gradle uses
  `jniLibs.useLegacyPackaging = true` so the binary is unpacked there.
- The daemon runs in a foreground service via
  [`flutter_foreground_task`](https://pub.dev/packages/flutter_foreground_task).
  `lib/daemon_runner.dart` is the service-isolate `TaskHandler`: it writes the
  config, spawns the daemon, supervises it (restarts on crash), and kills it on
  service stop. The UI isolate (`lib/daemon.dart`) only starts the service and
  connects a `ControlClient` to the socket the daemon brings up — the two
  isolates rendezvous on the unix socket.
- `NativePathsChannel.kt` exposes a `filestr/native` MethodChannel returning
  `nativeLibraryDir`, `filesDir`, and `cacheDir`. It is registered on the UI
  engine (`MainActivity.kt`) **and** on the service engine
  (`FgtEngineListener.kt`), since flutter_foreground_task does not run the
  plugin registrant on the service engine.
- `lib/control_client.dart` talks the JSON-lines control protocol over the
  unix socket.

### Android-only daemon tweak

The daemon is a standalone process with no JVM, so iroh's default DNS resolver
(`hickory`, which reads the system config via `ndk-context`) panics on Android.
`filestrd` therefore pins public UDP resolvers (`8.8.8.8`, `1.1.1.1`) under
`#[cfg(target_os = "android")]` — see `filestrd/src/main.rs`. Desktop behavior
is unchanged.

## Build

```sh
# 1. cross-compile + bundle the daemon (needs ANDROID_NDK_HOME, cargo-ndk,
#    rustup targets aarch64-linux-android + x86_64-linux-android)
../scripts/build-android.sh

# 2. build the app (uses the project-pinned Flutter via fvm)
fvm flutter build apk --debug
# -> build/app/outputs/flutter-apk/app-debug.apk
```

## Local HTTP gateway (Grayjay)

The generated config enables the daemon's loopback HTTP gateway
(`[http] listen = "127.0.0.1:11780"`), so other apps on the device can list and
stream whatever this node serves. The daemon also **serves the
[Grayjay](https://grayjay.app/) plugin itself** at `/grayjay/…`, and the Status
screen has an **"Add to Grayjay"** button that opens Grayjay's install flow for
it in one tap (fires a VIEW intent at Grayjay's `AddSourceActivity`, since
Grayjay's own "Install by URL" rejects `http://localhost`). See
[`../grayjay-plugin/`](../grayjay-plugin).

## Storage

Everything lives in the app sandbox:

- `files/share/` — the folder you share (rescanned on launch).
- `files/downloads/` — fetched files.
- `files/data`, `files/state`, cache dir — identity key, grants, blob store.
- `files/filestrd.log` — daemon stdout/stderr, for troubleshooting.

## Verified end-to-end

Run on an `x86_64` emulator (API 35) against a daemon on the host: the app
peered over iroh via an n0 relay, browsed the host's share, and downloaded
files whose SHA-256 matched the originals. The daemon kept running (same PID,
service still foreground) after the app was sent to the background.
