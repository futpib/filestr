#!/usr/bin/env bash
# Ranged streaming from a PEER, from a media player's standpoint: the player
# streams a file hosted on another node through the local gateway, by range,
# and the gateway fetches only what's asked — it does NOT download the whole
# file to answer a HEAD or a partial range. This is the streaming fix: an
# open-ended request starts without staging the entire blob.
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

(cd "$ROOT" && cargo build -q -p filestrd --features grayjay) || die "build (grayjay) failed"

# provider A shares a multi-window file (> 2 x the 4 MiB gateway window)
start_node A --share
SIZE=9437184  # 9 MiB
head -c "$SIZE" /dev/urandom > "$TESTDIR/A/share/movie.bin"
fctl A rescan > /dev/null

# gateway node G (no share of its own) exposes the loopback HTTP gateway
PORT=39081
export EXTRA_CONFIG=$'[http]\nlisten = "127.0.0.1:'"$PORT"'"'
start_node G
unset EXTRA_CONFIG
fctl G peer add "$(fctl A invite create 2>/dev/null)" > /dev/null

BASE="http://127.0.0.1:$PORT"
wait_http "$BASE/files"

# G lists A's file, sourced from the peer (not local)
curl -s "$BASE/files" > "$TESTDIR/g-files.json"
HASH="$(jq -r '.files[] | select(.name|endswith("movie.bin")) | .hash' "$TESTDIR/g-files.json")"
[ -n "$HASH" ] || die "peer movie.bin not listed by gateway"
jq -e '.files[] | select(.name|endswith("movie.bin")) | .source != "local"' "$TESTDIR/g-files.json" > /dev/null \
    || die "movie.bin should be sourced from a peer, not local"

# HEAD: size is known from the browse WITHOUT fetching any bytes
HCL="$(curl -s -I -o /dev/null -w '%header{content-length}' "$BASE/file/$HASH")"
[ "$HCL" = "$SIZE" ] || die "HEAD size from browse wrong: $HCL"
G_AFTER_HEAD="$(du -s "$TESTDIR/G/data" | awk '{print $1}')"
[ "$G_AFTER_HEAD" -lt 2048 ] || die "HEAD fetched data (G/data=${G_AFTER_HEAD}KiB)"

# RANGE in the middle: correct bytes, and the gateway must NOT have pulled the
# whole 9 MiB to answer it (the core of the streaming fix).
MSTAT="$(curl -s -H 'Range: bytes=5000000-5000099' "$BASE/file/$HASH" -o "$TESTDIR/mid.out" -w '%{http_code}')"
[ "$MSTAT" = 206 ] || die "middle range not 206: $MSTAT"
dd if="$TESTDIR/A/share/movie.bin" bs=100 skip=50000 count=1 status=none > "$TESTDIR/mid.orig"
cmp "$TESTDIR/mid.out" "$TESTDIR/mid.orig" || die "middle range bytes differ"
G_AFTER_MID="$(du -s "$TESTDIR/G/data" | awk '{print $1}')"
[ "$G_AFTER_MID" -lt 2048 ] \
    || die "ranged GET over-fetched (G/data=${G_AFTER_MID}KiB for a 100-byte range of a ${SIZE}-byte file)"

# OPEN-ENDED range to EOF reassembles correctly (fetched window by window)
TSTAT="$(curl -s -H 'Range: bytes=4000000-' "$BASE/file/$HASH" -o "$TESTDIR/tail.out" -w '%{http_code}')"
[ "$TSTAT" = 206 ] || die "open-ended range not 206: $TSTAT"
dd if="$TESTDIR/A/share/movie.bin" bs=4000000 skip=1 status=none > "$TESTDIR/tail.orig"
cmp "$TESTDIR/tail.out" "$TESTDIR/tail.orig" || die "open-ended tail differs"

# FULL GET reassembles to byte-identical content across all windows
curl -s "$BASE/file/$HASH" -o "$TESTDIR/full.out"
cmp "$TESTDIR/full.out" "$TESTDIR/A/share/movie.bin" || die "full reassembly differs"

echo OK
