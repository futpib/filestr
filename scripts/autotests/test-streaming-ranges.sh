#!/usr/bin/env bash
# Streaming pass-through (relay does NOT cache), byte-range fetch, and several
# concurrent background downloads.
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

start_node A --share
# a file big enough that a relay caching it would be obvious on disk
head -c 1048576 /dev/urandom > "$TESTDIR/A/share/big.bin"
echo "0123456789abcdef" > "$TESTDIR/A/share/ranged.txt"
fctl A rescan > /dev/null

# A -> B -> C chain
start_node B
fctl B peer add "$(fctl A invite create 2>/dev/null)" > /dev/null
start_node C
fctl C peer add "$(fctl B invite create 2>/dev/null)" > /dev/null

A_ID="$(node_id A)"
fctl C --json search big > "$TESTDIR/hits.json"
BIG_HASH="$(jq -r 'select(.name | endswith("big.bin")) | .hash' "$TESTDIR/hits.json" | head -n1)"
[ -n "$BIG_HASH" ] || die "big.bin not found via relay"

# relayed streaming fetch through B
fctl C get "$BIG_HASH" -o "$TESTDIR/big.out" > /dev/null || die "relayed get failed"
cmp "$TESTDIR/big.out" "$TESTDIR/A/share/big.bin" || die "relayed big.bin differs"

# STREAMING ASSERTION: B must not have cached the blob. The blobs store keeps
# data files under data/blobs; B's store size must stay tiny (no ~1MiB blob).
B_BLOBS_BYTES="$(du -s "$TESTDIR/B/data" | awk '{print $1}')"
# du is in KiB blocks; a cached 1MiB blob would push this well over 512.
[ "$B_BLOBS_BYTES" -lt 512 ] || die "relay appears to have cached data (B/data = ${B_BLOBS_BYTES}KiB)"

# RANGE: fetch bytes 3..=8 of ranged.txt ("345678") directly from A via browse
fctl C peer add "$(fctl A invite create 2>/dev/null)" > /dev/null
fctl C --json browse "$A_ID" > "$TESTDIR/browse.json"
R_HASH="$(jq -r '.[] | select(.path | endswith("ranged.txt")) | .hash' "$TESTDIR/browse.json")"
fctl C get "$R_HASH" --range 3-8 -o "$TESTDIR/range.out" --peer "$A_ID" > /dev/null \
    || die "ranged get failed"
GOT="$(cat "$TESTDIR/range.out")"
[ "$GOT" = "345678" ] || die "range mismatch: expected 345678, got '$GOT'"

# BACKGROUND: kick off several downloads at once, then wait for completion
for f in big.bin ranged.txt; do
    H="$(jq -r '.[] | select(.path | endswith("'"$f"'")) | .hash' "$TESTDIR/browse.json")"
    fctl C get "$H" -o "$TESTDIR/bg-$f" --peer "$A_ID" --background > /dev/null \
        || die "background get $f failed to start"
done
# poll transfers until none are queued/active
for i in $(seq 1 100); do
    PENDING="$(fctl C --json transfers | jq '[.[] | select(.status=="queued" or .status=="active")] | length')"
    [ "$PENDING" = 0 ] && break
    sleep 0.1
done
DONE="$(fctl C --json transfers | jq '[.[] | select(.status=="done")] | length')"
[ "$DONE" -ge 2 ] || die "expected >=2 completed bg transfers, got $DONE; $(fctl C --json transfers)"
cmp "$TESTDIR/bg-big.bin" "$TESTDIR/A/share/big.bin" || die "bg big.bin differs"

echo OK
