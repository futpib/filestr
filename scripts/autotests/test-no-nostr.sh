#!/usr/bin/env bash
# Run and peer files with the chat plane disabled, then join hubs later.
# Includes: a hub ticket redeemed while chat is off peers files now and queues
# the MLS join, which completes automatically once chat is enabled.
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

start_node A --share          # A: chat on (default), owns a hub
echo "shared by A" > "$TESTDIR/A/share/a.txt"
fctl A rescan > /dev/null
HUB="$(fctl A --json hub create lobby | jq -r .group_ref)"
HUBTICKET="$(fctl A hub invite "$HUB" 2>/dev/null | tail -n1)"

EXTRA_CONFIG=$'[chat]\nenabled = false'
start_node B --share          # B: chat off
unset EXTRA_CONFIG

# 1. plain file peering works with chat off
TICKET="$(fctl A invite create 2>/dev/null)"
fctl B peer add "$TICKET" > /dev/null || die "peer add failed with chat off"
A_ID="$(node_id A)"
fctl B --json browse "$A_ID" > "$TESTDIR/list.json" || die "browse failed with chat off"
HASH="$(jq -r '.[] | select(.path | endswith("a.txt")) | .hash' "$TESTDIR/list.json")"
fctl B get "$HASH" -o "$TESTDIR/a.out" > /dev/null || die "get failed with chat off"
cmp "$TESTDIR/a.out" "$TESTDIR/A/share/a.txt" || die "fetched content differs"

# 2. hub commands are refused while chat is off
if fctl B hub ls 2> "$TESTDIR/hub.err"; then
    die "hub commands should fail while chat is disabled"
fi
grep -qi "disabled" "$TESTDIR/hub.err" || die "wrong error for disabled chat"

# 3. a hub ticket redeemed with chat off peers files now and QUEUES the join
fctl B hub join "$HUBTICKET" > "$TESTDIR/join.out" 2>&1 || die "queued hub join failed"
grep -qi "queued" "$TESTDIR/join.out" || die "hub join should report queued; got: $(cat "$TESTDIR/join.out")"

# 4. enable chat + restart → the queued join completes on its own
sed -i 's/enabled = false/enabled = true/' "$TESTDIR/B/config.toml"
restart_node B
for i in $(seq 1 100); do
    if fctl B --json hub ls 2>/dev/null | jq -e '.[] | select(.group_ref == "'"$HUB"'")' > /dev/null 2>&1; then
        break
    fi
    sleep 0.2
done
fctl B --json hub ls | jq -e '.[] | select(.group_ref == "'"$HUB"'")' > /dev/null \
    || die "queued hub join did not complete after enabling chat"

# 5. and chat now works for the formerly-queued member
M="$(fctl A --json hub members "$HUB" | jq length)"
[ "$M" = 2 ] || die "owner sees $M members, expected 2"
fctl A hub send "$HUB" "welcome, queued joiner" > /dev/null || die "A send failed"
sleep 0.3
fctl B --json hub log "$HUB" > "$TESTDIR/b.json"
jq -e '.[] | select(.content == "welcome, queued joiner")' "$TESTDIR/b.json" > /dev/null \
    || die "formerly-queued member cannot read hub chat"

echo OK
