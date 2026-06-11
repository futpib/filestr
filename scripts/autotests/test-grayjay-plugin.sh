#!/usr/bin/env bash
# The Grayjay source plugin, end to end against a live gateway: it must list
# only the files Grayjay can play (audio/video — not the .txt), let the user
# filter by type, and hand back stream URLs that serve the correct bytes (full
# and Range). Runs the very harness the plugin ships with, which loads
# Grayjay's own polyfil/source scaffolding so the plugin runs against the real
# runtime contracts.
#
# Skips cleanly where node or the Grayjay scaffolding isn't available.
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

SCAFFOLD="${GRAYJAY_SCRIPTS:-$ROOT/../grayjay-android/app/src/main/assets/scripts}"
command -v node > /dev/null 2>&1 || { echo "SKIP (node not installed)"; exit 0; }
[ -f "$SCAFFOLD/source.js" ] || { echo "SKIP (grayjay scaffolding not at $SCAFFOLD)"; exit 0; }

(cd "$ROOT" && cargo build -q -p filestrd --features grayjay) || die "build (grayjay) failed"

PORT=39082
export EXTRA_CONFIG=$'[http]\nlisten = "127.0.0.1:'"$PORT"'"'
start_node A --share
unset EXTRA_CONFIG

# a mix: two playable media + one non-media the plugin must hide
head -c 131072 /dev/urandom > "$TESTDIR/A/share/song.mp3"
head -c 131072 /dev/urandom > "$TESTDIR/A/share/movie.mp4"
echo "not playable by a media player" > "$TESTDIR/A/share/readme.txt"
fctl A rescan > /dev/null

BASE="http://127.0.0.1:$PORT"
wait_http "$BASE/files"

node "$ROOT/grayjay-plugin/test/harness.js" "$BASE" "$SCAFFOLD" || die "grayjay plugin harness failed"

echo OK
