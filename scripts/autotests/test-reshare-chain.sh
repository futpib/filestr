#!/usr/bin/env bash
# Three-node line A-B-C: C finds A's file through B with no A attribution,
# relayed fetch verifies; a full cycle still terminates. (PLAN.md gate M4)
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

start_node A --share
head -c 8192 /dev/urandom > "$TESTDIR/A/share/secret-song.mp3"
fctl A rescan > /dev/null

start_node B
fctl B peer add "$(fctl A invite create 2>/dev/null)" > /dev/null || die "A->B grant failed"

start_node C
fctl C peer add "$(fctl B invite create 2>/dev/null)" > /dev/null || die "B->C grant failed"

A_ID="$(node_id A)"
B_ID="$(node_id B)"

# C searches: the hit must come via B, carry a handle, and contain no trace of A
fctl C --json search song > "$TESTDIR/hits.json" || die "search failed"
[ -s "$TESTDIR/hits.json" ] || die "no hits"
if grep -q "$A_ID" "$TESTDIR/hits.json"; then
    die "search results leak origin node id"
fi
VIA="$(jq -r .via "$TESTDIR/hits.json" | head -n 1)"
[ "$VIA" = "$B_ID" ] || die "expected via=$B_ID, got $VIA"
HANDLE="$(jq -r .handle "$TESTDIR/hits.json" | head -n 1)"
[ -n "$HANDLE" ] && [ "$HANDLE" != "null" ] || die "hit missing handle"
HASH="$(jq -r .hash "$TESTDIR/hits.json" | head -n 1)"

# relayed fetch: C pulls through B; bytes must verify against A's original
fctl C get "$HASH" -o "$TESTDIR/got.mp3" > /dev/null || die "relayed get failed"
cmp "$TESTDIR/got.mp3" "$TESTDIR/A/share/secret-song.mp3" || die "relayed content differs"

# close the loop (C grants A) and search from A: must terminate and find the
# file locally despite the A->C->B->A cycle
fctl A peer add "$(fctl C invite create 2>/dev/null)" > /dev/null || die "C->A grant failed"
timeout 30 "$BIN/filestrctl" --socket "$TESTDIR/A/ctl.sock" --json search song \
    > "$TESTDIR/cycle-hits.json" || die "cyclic search did not terminate cleanly"
jq -r 'select(.via == null) | .hash' "$TESTDIR/cycle-hits.json" | grep -q "$HASH" \
    || die "A did not find its own file"

echo OK
