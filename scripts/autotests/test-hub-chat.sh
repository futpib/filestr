#!/usr/bin/env bash
# Chat plane: real Marmot/MLS hub over the iroh nostr tunnel (no external
# relay). A single filestrhub1… ticket does BOTH: joins the chat AND peers the
# files in both directions. Owner creates a hub, member joins, E2EE messages
# flow both ways, and after joining each side can browse the other's share —
# all from the one ticket. (PLAN.md M5)
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

# both nodes share a file so we can prove file peering goes both directions
start_node A --share          # A = hub owner
start_node B --share          # B = member
echo "owner-only file"   > "$TESTDIR/A/share/owner.txt"
echo "members-only file" > "$TESTDIR/B/share/secret.txt"
fctl A rescan > /dev/null
fctl B rescan > /dev/null

# owner creates a hub and mints a join ticket
HUB="$(fctl A --json hub create "general" | jq -r .group_ref)"
[ -n "$HUB" ] || die "hub create failed"
TICKET="$(fctl A hub invite "$HUB" 2>/dev/null | tail -n1)"
case "$TICKET" in filestrhub1*) ;; *) die "bad hub ticket: $TICKET" ;; esac

# member joins over iroh
fctl B hub join "$TICKET" > /dev/null || die "hub join failed"

# both sides should see 2 members
sleep 0.5
MEMBERS_A="$(fctl A --json hub members "$HUB" | jq length)"
[ "$MEMBERS_A" = 2 ] || die "owner sees $MEMBERS_A members, expected 2"

# owner -> hub, member reads it (MLS-decrypted)
fctl A hub send "$HUB" "hello hub from owner" > /dev/null || die "owner send failed"
sleep 0.3
fctl B --json hub log "$HUB" > "$TESTDIR/b-log.json" || die "member log failed"
jq -e '.[] | select(.content == "hello hub from owner")' "$TESTDIR/b-log.json" > /dev/null \
    || die "member did not receive owner's message; got: $(cat "$TESTDIR/b-log.json")"

# member -> hub, owner reads it
fctl B hub send "$HUB" "hi back from member" > /dev/null || die "member send failed"
sleep 0.3
fctl A --json hub log "$HUB" > "$TESTDIR/a-log.json" || die "owner log failed"
jq -e '.[] | select(.content == "hi back from member")' "$TESTDIR/a-log.json" > /dev/null \
    || die "owner did not receive member's message; got: $(cat "$TESTDIR/a-log.json")"

# the SAME hub ticket also peered the files, both directions:

# (a) share-to-join — the owner can browse the member's files
B_ID="$(node_id B)"
fctl A --json browse "$B_ID" > "$TESTDIR/a-browses-b.json" 2>/dev/null \
    || die "owner cannot browse member after hub join (share-to-join broken)"
jq -e '.[] | select(.path | endswith("secret.txt"))' "$TESTDIR/a-browses-b.json" > /dev/null \
    || die "owner does not see member's shared file"

# (b) the member can browse the owner's files — from the same ticket, no extra
#     peer add
A_ID="$(node_id A)"
fctl B --json browse "$A_ID" > "$TESTDIR/b-browses-a.json" 2>/dev/null \
    || die "member cannot browse owner after hub join (file peering one-way only)"
jq -e '.[] | select(.path | endswith("owner.txt"))' "$TESTDIR/b-browses-a.json" > /dev/null \
    || die "member does not see owner's shared file"

# the hub ticket is single-use: a different node cannot reuse it (the embedded
# invite token was burned when B joined)
start_node C
if fctl C hub join "$TICKET" 2> "$TESTDIR/reuse.err"; then
    die "hub ticket reuse should have failed"
fi
grep -qi "refused\|denied\|already\|redeem\|expired\|invalid" "$TESTDIR/reuse.err" \
    || die "hub ticket reuse failed for the wrong reason: $(cat "$TESTDIR/reuse.err")"
# C must not have been added to the group
MEMBERS_AFTER="$(fctl A --json hub members "$HUB" | jq length)"
[ "$MEMBERS_AFTER" = 2 ] || die "reuse leaked a member: hub now has $MEMBERS_AFTER"

# a fresh ticket, however, lets C join (owner can add more members)
TICKET2="$(fctl A hub invite "$HUB" 2>/dev/null | tail -n1)"
fctl C hub join "$TICKET2" > /dev/null || die "join with a fresh ticket failed"

echo OK
