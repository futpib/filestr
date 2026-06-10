#!/usr/bin/env bash
# Discover + request-to-join over nostr (ROADMAP item 2), reusing the
# filestrreq1 request ticket: owner announces a hub on a relay, a newcomer
# discovers it and sends an (encrypted) join request over the relay, and the
# owner auto-admits — newcomer ends up in the hub. No prior grant between them.
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

PORT=$((20000 + ($$ % 10000)))

# A: owner — hosts a websocket relay, uses it, and auto-admits requests
EXTRA_CONFIG="$(printf '[chat]\nrelay_listen = "127.0.0.1:%s"\nrelays = ["ws://127.0.0.1:%s"]\nauto_admit = true' "$PORT" "$PORT")"
start_node A --share
unset EXTRA_CONFIG

# B: newcomer — only knows the public relay (as if from the announcement)
EXTRA_CONFIG="$(printf '[chat]\nrelays = ["ws://127.0.0.1:%s"]' "$PORT")"
start_node B --share
unset EXTRA_CONFIG

HUB="$(fctl A --json hub create discoverable | jq -r .group_ref)"
[ -n "$HUB" ] || die "hub create failed"
fctl A hub announce "$HUB" > /dev/null || die "announce failed"
sleep 0.5

# B sees it on nostr
fctl B --json hub discover > "$TESTDIR/disc.json" || die "discover failed"
jq -e '.[] | select(.group_ref == "'"$HUB"'")' "$TESTDIR/disc.json" > /dev/null \
    || die "B did not discover the hub; got: $(cat "$TESTDIR/disc.json")"
OWNER="$(jq -r '.[] | select(.group_ref == "'"$HUB"'") | .owner' "$TESTDIR/disc.json")"
[ -n "$OWNER" ] || die "announcement missing owner pubkey"

# B requests to join over nostr (encrypted DM to the owner via the relay)
fctl B hub request --hub "$HUB" --to "$OWNER" > /dev/null || die "request send failed"

# A auto-admits; wait for B to land in the hub
for i in $(seq 1 100); do
    if fctl B --json hub ls 2>/dev/null | jq -e '.[] | select(.group_ref == "'"$HUB"'")' > /dev/null 2>&1; then
        break
    fi
    sleep 0.2
done
fctl B --json hub ls | jq -e '.[] | select(.group_ref == "'"$HUB"'")' > /dev/null \
    || die "B was not auto-admitted after requesting over nostr"

M="$(fctl A --json hub members "$HUB" | jq length)"
[ "$M" = 2 ] || die "owner sees $M members, expected 2"

# the freshly auto-admitted member can chat E2EE
fctl A hub send "$HUB" "auto-admitted hello" > /dev/null || die "A send failed"
sleep 0.3
fctl B --json hub log "$HUB" > "$TESTDIR/b.json"
jq -e '.[] | select(.content == "auto-admitted hello")' "$TESTDIR/b.json" > /dev/null \
    || die "auto-admitted member cannot read hub chat"

echo OK
