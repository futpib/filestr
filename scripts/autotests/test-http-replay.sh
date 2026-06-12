#!/usr/bin/env bash
# Partial-blob reuse: a range fetched from a peer is kept in the local store, so
# replaying/seeking within it doesn't re-download. Proven by killing the peer
# after the first fetch: the already-fetched range still serves; an un-fetched
# range does not.
#
#   A (shares the file)  <--  G (gateway + player)
#
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

(cd "$ROOT" && cargo build -q -p filestrd --features grayjay) || die "build (grayjay) failed"

start_node A --share
head -c 8388608 /dev/urandom > "$TESTDIR/A/share/movie.bin"   # 8 MiB
fctl A rescan > /dev/null

PORT=39085
export EXTRA_CONFIG=$'[http]\nlisten = "127.0.0.1:'"$PORT"'"'
start_node G
unset EXTRA_CONFIG
fctl G peer add "$(fctl A invite create 2>/dev/null)" > /dev/null

BASE="http://127.0.0.1:$PORT"
wait_http "$BASE/files"
HASH="$(curl -s "$BASE/files" | jq -r '.files[]|select(.name|endswith("movie.bin"))|.hash')"
[ -n "$HASH" ] || die "movie.bin not listed"

# fetch a small range near the start -> lands in the local partial blob
R1="bytes=1000000-1000099"
curl -s -H "Range: $R1" "$BASE/file/$HASH" -o "$TESTDIR/r1.out" -w '%{http_code}' > "$TESTDIR/r1.code"
[ "$(cat "$TESTDIR/r1.code")" = 206 ] || die "first range fetch not 206"
dd if="$TESTDIR/A/share/movie.bin" bs=100 skip=10000 count=1 status=none > "$TESTDIR/r1.orig"
cmp "$TESTDIR/r1.out" "$TESTDIR/r1.orig" || die "first range bytes differ"

# kill the provider — now only locally-present ranges can be served
kill "$PID_A" 2>/dev/null || true
sleep 1

# the already-fetched range still serves correctly (reused, no peer needed)
curl -s -H "Range: $R1" "$BASE/file/$HASH" -o "$TESTDIR/r1b.out" -w '%{http_code}' > /dev/null
cmp "$TESTDIR/r1b.out" "$TESTDIR/r1.orig" || die "fetched range not reused after peer died"

# a DIFFERENT, un-fetched range cannot be served (peer gone) — confirms the
# reuse above wasn't just because the whole blob was local. The body stream
# fails mid-transfer, so curl may exit non-zero; that's expected.
: > "$TESTDIR/r2.out"
curl -s -H "Range: bytes=6000000-6000099" "$BASE/file/$HASH" -o "$TESTDIR/r2.out" 2>/dev/null || true
GOT=$(wc -c < "$TESTDIR/r2.out")
[ "$GOT" -lt 100 ] || die "un-fetched range served $GOT bytes with the peer dead (unexpected)"

echo OK
