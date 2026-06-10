#!/usr/bin/env bash
# Chat plane: real Marmot/MLS hub over the iroh nostr tunnel (no external
# relay). Owner creates a hub, member joins, E2EE messages flow both ways,
# joining auto-grants the owner file access (share-to-join). (PLAN.md M5)
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

# member shares a file so we can prove share-to-join gives the owner access
start_node A --share          # A = hub owner
start_node B --share          # B = member
echo "members-only file" > "$TESTDIR/B/share/secret.txt"
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

# share-to-join: the owner can now browse the member's files
B_ID="$(node_id B)"
fctl A --json browse "$B_ID" > "$TESTDIR/browse.json" 2>/dev/null \
    || die "owner cannot browse member after hub join (share-to-join broken)"
jq -e '.[] | select(.path | endswith("secret.txt"))' "$TESTDIR/browse.json" > /dev/null \
    || die "owner does not see member's shared file"

echo OK
