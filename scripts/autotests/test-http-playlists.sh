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
# the folder grouping excludes the non-media file too: the share root "files"
# holds 3 media tracks (+ the excluded .bin)
jq -e '.folders[]|select(.name=="files")|.count==3' "$TESTDIR/pl.json" >/dev/null || die "folder 'files' count wrong"
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

echo OK
