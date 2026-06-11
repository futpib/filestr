#!/usr/bin/env bash
# Media metadata, from a media player's standpoint: a shared audio file shows
# its real title/artist/album and duration (not just a filename), and an mp4
# reports its duration — surfaced through the gateway's /files. Fixtures are
# generated with ffmpeg; skips cleanly without it.
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

command -v ffmpeg > /dev/null 2>&1 || { echo "SKIP (ffmpeg not installed)"; exit 0; }
(cd "$ROOT" && cargo build -q -p filestrd --features grayjay) || die "build (grayjay) failed"

PORT=39083
export EXTRA_CONFIG=$'[http]\nlisten = "127.0.0.1:'"$PORT"'"'
start_node A --share
unset EXTRA_CONFIG

# a 2s tagged MP3 (Xing header so duration is readable) and a 3s mp4
ffmpeg -v error -f lavfi -i "sine=frequency=440:duration=2" \
    -metadata title="Test Title" -metadata artist="Test Artist" -metadata album="Test Album" \
    -write_xing 1 -y "$TESTDIR/A/share/song.mp3"
ffmpeg -v error -f lavfi -i "testsrc=duration=3:size=320x240:rate=10" \
    -pix_fmt yuv420p -y "$TESTDIR/A/share/clip.mp4"
fctl A rescan > /dev/null

BASE="http://127.0.0.1:$PORT"
wait_http "$BASE/files"
curl -s "$BASE/files" > "$TESTDIR/files.json"

# --- AUDIO: tags + duration -------------------------------------------------
sel() { jq -r '.files[] | select(.name|endswith("'"$1"'")) | '"$2" "$TESTDIR/files.json"; }
[ "$(sel song.mp3 .media.title)" = "Test Title" ] || die "mp3 title: $(sel song.mp3 .media.title)"
[ "$(sel song.mp3 .media.artist)" = "Test Artist" ] || die "mp3 artist: $(sel song.mp3 .media.artist)"
[ "$(sel song.mp3 .media.album)" = "Test Album" ] || die "mp3 album: $(sel song.mp3 .media.album)"
ADUR="$(sel song.mp3 .media.duration_secs)"
awk -v d="$ADUR" 'BEGIN{exit !(d>1.8 && d<2.3)}' || die "mp3 duration off: $ADUR (want ~2.0)"

# --- VIDEO: duration from the mp4 container ----------------------------------
VDUR="$(sel clip.mp4 .media.duration_secs)"
awk -v d="$VDUR" 'BEGIN{exit !(d>2.7 && d<3.3)}' || die "mp4 duration off: $VDUR (want ~3.0)"

echo OK
