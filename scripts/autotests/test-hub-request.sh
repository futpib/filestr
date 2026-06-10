#!/usr/bin/env bash
# Member-initiated join: a requester produces a filestrreq1… ticket out of
# band; the owner admits it. Verifies E2EE chat both ways and bidirectional
# file peering, all from the one request ticket — and that it's single-use.
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

start_node A --share     # hub owner
start_node B --share     # requester
echo "owner-only file"   > "$TESTDIR/A/share/owner.txt"
echo "members-only file" > "$TESTDIR/B/share/secret.txt"
fctl A rescan > /dev/null
fctl B rescan > /dev/null

HUB="$(fctl A --json hub create open-hub | jq -r .group_ref)"
[ -n "$HUB" ] || die "hub create failed"

# B produces a request ticket out of band (doesn't need to know A's hub id —
# A owns exactly one hub, so admit picks it)
REQ="$(fctl B hub request --label "hi I'm B" 2>/dev/null)"
case "$REQ" in filestrreq1*) ;; *) die "bad request ticket: $REQ" ;; esac

# A admits the request
fctl A hub admit "$REQ" > /dev/null || die "admit failed"

sleep 0.5
M="$(fctl A --json hub members "$HUB" | jq length)"
[ "$M" = 2 ] || die "owner sees $M members, expected 2"

# E2EE chat both directions
fctl A hub send "$HUB" "welcome aboard" > /dev/null || die "A send failed"
sleep 0.3
fctl B --json hub log "$HUB" > "$TESTDIR/b.json"
jq -e '.[] | select(.content == "welcome aboard")' "$TESTDIR/b.json" > /dev/null \
    || die "requester did not receive owner's message"
fctl B hub send "$HUB" "thanks for having me" > /dev/null || die "B send failed"
sleep 0.3
fctl A --json hub log "$HUB" > "$TESTDIR/a.json"
jq -e '.[] | select(.content == "thanks for having me")' "$TESTDIR/a.json" > /dev/null \
    || die "owner did not receive requester's reply"

# bidirectional file peering established by the same request
A_ID="$(node_id A)"; B_ID="$(node_id B)"
fctl A --json browse "$B_ID" > "$TESTDIR/a-b.json" 2>/dev/null \
    || die "owner cannot browse requester (share-to-join broken)"
jq -e '.[] | select(.path | endswith("secret.txt"))' "$TESTDIR/a-b.json" > /dev/null \
    || die "owner does not see requester's file"
fctl B --json browse "$A_ID" > "$TESTDIR/b-a.json" 2>/dev/null \
    || die "requester cannot browse owner"
jq -e '.[] | select(.path | endswith("owner.txt"))' "$TESTDIR/b-a.json" > /dev/null \
    || die "requester does not see owner's file"

# the request ticket is single-use: admitting it again fails
if fctl A hub admit "$REQ" 2> "$TESTDIR/reuse.err"; then
    die "re-admitting the same request ticket should fail"
fi

echo OK
