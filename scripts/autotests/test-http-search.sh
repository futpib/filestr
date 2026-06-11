#!/usr/bin/env bash
# Federated search through the gateway, from a media player's standpoint: the
# player finds a file hosted two hops away (a friend-of-a-friend) — which a
# one-hop /files browse can't see — and can play it. This is what /search adds
# over /files.
#
#   A (shares the file)  <--  B  <--  G (gateway + player)
#
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

(cd "$ROOT" && cargo build -q -p filestrd --features grayjay) || die "build (grayjay) failed"

# A shares a uniquely-named file
start_node A --share
head -c 262144 /dev/urandom > "$TESTDIR/A/share/zforatest-movie.bin"
fctl A rescan > /dev/null

# B sits in the middle (no shares of its own), peered with A
start_node B
fctl B peer add "$(fctl A invite create 2>/dev/null)" > /dev/null

# G is the gateway/player, peered with B only — so A is two hops away
PORT=39084
export EXTRA_CONFIG=$'[http]\nlisten = "127.0.0.1:'"$PORT"'"'
start_node G
unset EXTRA_CONFIG
fctl G peer add "$(fctl B invite create 2>/dev/null)" > /dev/null

BASE="http://127.0.0.1:$PORT"
wait_http "$BASE/files"

# /files only browses DIRECT peers (one hop), so A's file must NOT appear there
curl -s "$BASE/files" > "$TESTDIR/files.json"
if jq -e '.files[] | select(.name|contains("zforatest"))' "$TESTDIR/files.json" > /dev/null; then
    die "/files unexpectedly reached a two-hop file (browse should be one hop)"
fi

# /search forwards across the grant graph, so it DOES find the two-hop file
curl -s "$BASE/search?q=zforatest" > "$TESTDIR/search.json"
HASH="$(jq -r '.files[] | select(.name|contains("zforatest")) | .hash' "$TESTDIR/search.json")"
[ -n "$HASH" ] || die "federated /search did not find the two-hop file"
SRC="$(jq -r '.files[] | select(.name|contains("zforatest")) | .source' "$TESTDIR/search.json")"
[ "$SRC" != "local" ] || die "two-hop hit wrongly marked local"

# and the result is playable: streaming it through the gateway (relayed via B)
# yields the original bytes
curl -s "$BASE/file/$HASH?name=zforatest-movie.bin" -o "$TESTDIR/got.bin"
cmp "$TESTDIR/got.bin" "$TESTDIR/A/share/zforatest-movie.bin" || die "streamed federated result differs"

# empty query returns nothing (can't federate "match everything")
[ "$(curl -s "$BASE/search?q=" | jq '.files | length')" = 0 ] || die "empty query should return no results"

echo OK
