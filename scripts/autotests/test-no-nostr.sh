#!/usr/bin/env bash
# A node can run and peer files with the chat plane disabled at runtime
# ([chat] enabled = false), then enable it and join hubs later.
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

start_node A --share
echo "shared by A" > "$TESTDIR/A/share/a.txt"
fctl A rescan > /dev/null

# B runs with nostr off entirely
EXTRA_CONFIG=$'[chat]\nenabled = false'
start_node B --share
unset EXTRA_CONFIG

# file peering works without nostr: B redeems A's invite and fetches
TICKET="$(fctl A invite create 2>/dev/null)"
fctl B peer add "$TICKET" > /dev/null || die "peer add failed with chat off"
A_ID="$(node_id A)"
fctl B --json browse "$A_ID" > "$TESTDIR/list.json" || die "browse failed with chat off"
HASH="$(jq -r '.[] | select(.path | endswith("a.txt")) | .hash' "$TESTDIR/list.json")"
fctl B get "$HASH" -o "$TESTDIR/a.out" > /dev/null || die "get failed with chat off"
cmp "$TESTDIR/a.out" "$TESTDIR/A/share/a.txt" || die "fetched content differs"

# hub commands are refused while chat is off
if fctl B hub ls 2> "$TESTDIR/hub.err"; then
    die "hub commands should fail while chat is disabled"
fi
grep -qi "disabled" "$TESTDIR/hub.err" || die "wrong error for disabled chat: $(cat "$TESTDIR/hub.err")"

# join later: turn chat on and restart — hub commands now work
sed -i 's/enabled = false/enabled = true/' "$TESTDIR/B/config.toml"
restart_node B
fctl B hub ls > /dev/null || die "hub commands should work after enabling chat"
# B can now create/own a hub (proves the chat plane really came up)
fctl B hub create later > /dev/null || die "hub create failed after enabling chat"

echo OK
