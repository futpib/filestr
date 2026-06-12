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

# a tagged MP3 WITH embedded cover art (so the gateway caches a thumbnail)
ffmpeg -v error -f lavfi -i "color=c=red:s=64x64:d=1" -frames:v 1 -y "$TESTDIR/cover.png"
ffmpeg -v error -f lavfi -i "sine=frequency=330:duration=2" -i "$TESTDIR/cover.png" \
    -map 0:a -map 1:v -c:v copy -id3v2_version 3 \
    -metadata:s:v title="Album cover" -metadata:s:v comment="Cover (front)" \
    -write_xing 1 -y "$TESTDIR/A/share/withcover.mp3"
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

# --- CONTENT SNIFFING: a media file with the wrong extension is still detected,
# listed, and served with the right content type --------------------------------
ffmpeg -v error -f lavfi -i "sine=frequency=550:duration=2" -write_xing 1 -y "$TESTDIR/mys.mp3"
cp "$TESTDIR/mys.mp3" "$TESTDIR/A/share/mystery.dat"   # mp3 bytes, .dat extension
fctl A rescan > /dev/null
curl -s "$BASE/files" > "$TESTDIR/files.json"
[ "$(sel mystery.dat .media.content_type)" = "audio/mpeg" ] \
    || die "misnamed mp3 not sniffed as audio/mpeg: $(sel mystery.dat .media.content_type)"
DHASH="$(sel mystery.dat .hash)"
DCT="$(curl -s -o /dev/null -w '%header{content-type}' "$BASE/file/$DHASH")"
[ "$DCT" = "audio/mpeg" ] || die "misnamed file served as $DCT, want audio/mpeg"

# --- SEARCH matches tags, not just the filename ------------------------------
# the file is song.mp3 (no "artist" in the name), but its artist tag is "Test
# Artist" — a federated /search for it must still find the file
curl -s "$BASE/search?q=Test%20Artist" > "$TESTDIR/search.json"
jq -e '.files[] | select(.name|endswith("song.mp3"))' "$TESTDIR/search.json" > /dev/null \
    || die "search by artist tag did not find song.mp3"

# --- THUMBNAIL: embedded cover art is cached and served ----------------------
# the plain song.mp3 has no art -> no thumb flag
[ "$(sel song.mp3 .thumb)" = "null" ] || die "song.mp3 unexpectedly has a thumb"
# the cover mp3 -> thumb flag set, and /thumb/{hash} serves a real image
[ "$(sel withcover.mp3 .thumb)" = "true" ] || die "withcover.mp3 missing thumb flag"
THASH="$(sel withcover.mp3 .hash)"
TSTAT="$(curl -s "$BASE/thumb/$THASH" -o "$TESTDIR/thumb.out" -w '%{http_code}')"
[ "$TSTAT" = 200 ] || die "/thumb status $TSTAT"
TCT="$(curl -s -o /dev/null -w '%header{content-type}' "$BASE/thumb/$THASH")"
case "$TCT" in image/*) ;; *) die "/thumb content-type not an image: $TCT";; esac
[ "$(wc -c < "$TESTDIR/thumb.out")" -gt 0 ] || die "/thumb returned no bytes"
# the thumbnail is cached on disk; removing the source prunes it on rescan
[ -e "$TESTDIR/A/data/thumbs/$THASH" ] || die "thumbnail not cached on disk"
rm "$TESTDIR/A/share/withcover.mp3"
fctl A rescan > /dev/null
[ -e "$TESTDIR/A/data/thumbs/$THASH" ] && die "stale thumbnail not pruned after removal"

echo OK
