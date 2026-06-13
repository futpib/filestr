#!/usr/bin/env bash
# The gateway groups the library server-side at GET /playlists, so the Grayjay
# plugin's channel Playlists tab gets a few hundred grouping stubs instead of
# pulling and grouping the whole (potentially huge) /files listing. Checks the
# folder/album/artist groupings, the ?source= scope, the peers reachability
# array, and that non-media files are excluded (so a group's count matches what
# the plugin later resolves).
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

command -v ffmpeg > /dev/null 2>&1 || { echo "SKIP (ffmpeg not installed)"; exit 0; }
(cd "$ROOT" && cargo build -q -p filestrd --features grayjay) || die "build (grayjay) failed"

PORT=39096
export EXTRA_CONFIG=$'[http]\nlisten = "127.0.0.1:'"$PORT"'"'
start_node A --share
unset EXTRA_CONFIG

# artist "Tester": 3 tracks across two albums, plus a NON-media file that must be
# excluded from the groupings.
mk() { # $1 file  $2 artist  $3 album  $4 title
    ffmpeg -v error -f lavfi -i "sine=frequency=440:duration=1" \
        -metadata artist="$2" -metadata album="$3" -metadata title="$4" \
        -write_xing 1 -y "$TESTDIR/A/share/$1"
}
mk gh1.mp3 "Tester" "Greatest Hits" "Hit One"
mk gh2.mp3 "Tester" "Greatest Hits" "Hit Two"
mk bs1.mp3 "Tester" "B Sides" "Rarity"
head -c 4096 /dev/urandom > "$TESTDIR/A/share/notes.bin"   # non-media (octet-stream)
# an artwork-only subfolder: nothing but images -> must NOT become a playlist
mkdir -p "$TESTDIR/A/share/artwork"
ffmpeg -v error -f lavfi -i "color=c=red:s=16x16" -frames:v 1 -y "$TESTDIR/A/share/artwork/cover.jpg"
# a cover image alongside the music in the root folder -> excluded from its count
ffmpeg -v error -f lavfi -i "color=c=blue:s=16x16" -frames:v 1 -y "$TESTDIR/A/share/folder.jpg"
fctl A rescan > /dev/null

BASE="http://127.0.0.1:$PORT"
wait_http "$BASE/files"
for i in $(seq 1 50); do
    [ "$(curl -s "$BASE/files" | jq '[.files[]|select(.media.artist=="Tester")]|length')" = "3" ] && break
    sleep 0.2
done

# --- whole-library groupings --------------------------------------------------
curl -s "$BASE/playlists" > "$TESTDIR/pl.json"
echo "playlists: $(cat "$TESTDIR/pl.json")"

# two albums, split by tag, with the right counts
jq -e '[.albums[]|select(.name=="Greatest Hits")]|length==1' "$TESTDIR/pl.json" >/dev/null || die "missing 'Greatest Hits' album"
jq -e '.albums[]|select(.name=="Greatest Hits")|.count==2' "$TESTDIR/pl.json" >/dev/null || die "'Greatest Hits' count wrong"
jq -e '.albums[]|select(.name=="B Sides")|.count==1' "$TESTDIR/pl.json" >/dev/null || die "'B Sides' count wrong"
# one artist with all 3 tracks (NOT 4 — the .bin is excluded)
jq -e '.artists[]|select(.name=="Tester")|.count==3' "$TESTDIR/pl.json" >/dev/null || die "artist 'Tester' count wrong (non-media leaked?)"
# the root folder "files" holds 3 audio tracks; the .bin (non-media) AND the
# folder.jpg (image) are both excluded from the count
jq -e '.folders[]|select(.name=="files")|.count==3' "$TESTDIR/pl.json" >/dev/null || die "folder 'files' count wrong (non-media/image leaked?)"
# an image-only folder ("artwork") is NOT served as a playlist
jq -e '[.folders[]|select(.name=="artwork")]|length==0' "$TESTDIR/pl.json" >/dev/null || die "image-only folder served as a playlist"
# folder url key is the full path; album/artist key is the name
jq -e '.folders[]|select(.name=="files")|.key=="files"' "$TESTDIR/pl.json" >/dev/null || die "folder key should be the path"
jq -e '.albums[]|select(.name=="Greatest Hits")|.key=="Greatest Hits"' "$TESTDIR/pl.json" >/dev/null || die "album key should be the name"
# peers array present (empty here — no granted peers), same contract as /files
jq -e 'has("peers")' "$TESTDIR/pl.json" >/dev/null || die "missing peers array"

# --- ?source= scope -----------------------------------------------------------
# local source: same groupings
jq_local() { curl -s "$BASE/playlists?source=local" | jq "$1"; }
[ "$(jq_local '.artists[]|select(.name=="Tester")|.count')" = "3" ] || die "source=local artist count wrong"
# a source nobody serves: empty groupings (but still well-formed)
EMPTY="$(curl -s "$BASE/playlists?source=nosuchpeer")"
[ "$(echo "$EMPTY" | jq '.albums|length')" = "0" ] || die "unknown source should have no albums"
[ "$(echo "$EMPTY" | jq '.artists|length')" = "0" ] || die "unknown source should have no artists"

# --- resolve ONE grouping to its tracks (GET /playlist) -----------------------
# artist "Tester" -> its 3 tracks, all playable; non-media never appears
curl -s "$BASE/playlist?kind=artist&key=Tester&source=local" > "$TESTDIR/one.json"
echo "one playlist: $(cat "$TESTDIR/one.json")"
[ "$(jq '.files|length' "$TESTDIR/one.json")" = "3" ] || die "/playlist artist=Tester wrong track count"
jq -e 'all(.files[]; .media.artist=="Tester")' "$TESTDIR/one.json" >/dev/null || die "/playlist returned a non-matching track"
# album scope
[ "$(curl -s "$BASE/playlist?kind=album&key=Greatest%20Hits&source=local" | jq '.files|length')" = "2" ] || die "/playlist album wrong count"
# folder scope (the share root "files") excludes the non-media .bin -> 3, not 4
[ "$(curl -s "$BASE/playlist?kind=folder&key=files&source=local" | jq '.files|length')" = "3" ] || die "/playlist folder wrong count (non-media leaked?)"
# the resolved tracks are streamable
H="$(jq -r '.files[0].hash' "$TESTDIR/one.json")"
curl -s -o "$TESTDIR/trk.mp3" "$BASE/file/$H"
[ -s "$TESTDIR/trk.mp3" ] || die "resolved track did not stream"
# empty source = whole library (still finds the artist here)
[ "$(curl -s "$BASE/playlist?kind=artist&key=Tester&source=" | jq '.files|length')" = "3" ] || die "/playlist empty source should span library"

# --- sibling tracks for the Recommended tab (GET /related) --------------------
# gh1.mp3 is in album "Greatest Hits" (with gh2), folder "files" (with gh2, bs1)
# and artist "Tester" (gh2, bs1). Related = the union minus itself = {gh2, bs1},
# all by Tester, never gh1 and never the non-media/image files.
GH1="$(curl -s "$BASE/files" | jq -r '.files[]|select(.name|endswith("gh1.mp3"))|.hash')"
[ -n "$GH1" ] || die "could not find gh1.mp3 hash"
curl -s "$BASE/related?hash=$GH1" > "$TESTDIR/rel.json"
echo "related: $(cat "$TESTDIR/rel.json")"
[ "$(jq '.files|length' "$TESTDIR/rel.json")" = "2" ] || die "/related wrong sibling count"
jq -e 'all(.files[]; .media.artist=="Tester")' "$TESTDIR/rel.json" >/dev/null || die "/related returned a non-sibling"
jq -e "all(.files[]; .hash!=\"$GH1\")" "$TESTDIR/rel.json" >/dev/null || die "/related must exclude the file itself"
jq -e 'all(.files[]; (.media.content_type|tostring|startswith("audio") or startswith("video")))' "$TESTDIR/rel.json" >/dev/null || die "/related leaked a non-media/image file"
# bs1.mp3 (album "B Sides", alone) still has folder+artist siblings (gh1, gh2)
BS1="$(curl -s "$BASE/files" | jq -r '.files[]|select(.name|endswith("bs1.mp3"))|.hash')"
[ "$(curl -s "$BASE/related?hash=$BS1" | jq '.files|length')" = "2" ] || die "/related for bs1 wrong count"
# a non-media file has no related siblings (notes.bin is octet-stream)
BIN="$(curl -s "$BASE/files" | jq -r '.files[]|select(.name|endswith("notes.bin"))|.hash // empty')"
if [ -n "$BIN" ]; then
    [ "$(curl -s "$BASE/related?hash=$BIN" | jq '.files|length')" = "0" ] || die "non-media file should have no related siblings"
fi
# an unknown hash is well-formed and empty
[ "$(curl -s "$BASE/related?hash=deadbeef" | jq '.files|length')" = "0" ] || die "unknown hash should have no related siblings"

echo OK
