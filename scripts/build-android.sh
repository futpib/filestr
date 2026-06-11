#!/usr/bin/env bash
# Cross-compile the iroh-only filestr daemon for Android and bundle the
# binaries into the Flutter app as jniLibs.
#
# The daemon is a plain executable, not a JNI library, but Android only
# extracts and grants exec permission to files under nativeLibraryDir whose
# names match lib*.so — so we ship it as `libfilestrd.so` and run it from
# there. "Files only": we build with --no-default-features, which drops the
# whole chat/nostr/MLS stack, leaving a pure file-peering daemon. We do enable
# the `grayjay` feature: the app's loopback HTTP gateway serves the Grayjay
# plugin and streams files to it.
#
# Requires: cargo-ndk, an Android NDK (set ANDROID_NDK_HOME), the Android Rust
# targets (rustup target add aarch64-linux-android x86_64-linux-android).
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo"

: "${ANDROID_NDK_HOME:?set ANDROID_NDK_HOME to your installed NDK (e.g. ~/Android/Sdk/ndk/<ver>)}"

# Android ABI -> Rust target triple. x86_64 is for the emulator.
declare -A targets=(
    [arm64-v8a]=aarch64-linux-android
    [x86_64]=x86_64-linux-android
)

jnilibs="$repo/app/android/app/src/main/jniLibs"

for abi in "${!targets[@]}"; do
    triple="${targets[$abi]}"
    echo ">> building filestrd for $abi ($triple)"
    cargo ndk -t "$abi" build -p filestrd --no-default-features --features grayjay --release
    mkdir -p "$jnilibs/$abi"
    cp "target/$triple/release/filestrd" "$jnilibs/$abi/libfilestrd.so"
    echo "   -> $jnilibs/$abi/libfilestrd.so"
done

echo "done. now: cd app && fvm flutter build apk"
