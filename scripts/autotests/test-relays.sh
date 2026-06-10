#!/usr/bin/env bash
# Configurable relays: (1) a custom iroh relay URL is accepted; (2) hub chat
# flows over an external nostr relay — here one node's own WebSocket relay
# listener, with the iroh-tunnelled embedded relay disabled so the external
# relay is the only path.
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

# (1) iroh: a node configured with a custom relay URL still starts
EXTRA_CONFIG='relay_urls = ["https://relay.example./"]'
start_node R    # start_node dies if it doesn't come up -> config accepted + bound
unset EXTRA_CONFIG

# (2) nostr external relay path
PORT=$((20000 + ($$ % 10000)))

# A: owner, hosts a standard NIP-01 relay on a WebSocket port, iroh-tunnelled
# embedded relay turned OFF so chat can only move via the websocket relay
EXTRA_CONFIG=$'[chat]\nembedded_relay = false\nrelay_listen = "127.0.0.1:'"$PORT"'"'
start_node A --share
unset EXTRA_CONFIG

# B: member, configured to use A's websocket relay as an external nostr relay
EXTRA_CONFIG=$'[chat]\nrelays = ["ws://127.0.0.1:'"$PORT"'"]'
start_node B --share
unset EXTRA_CONFIG

HUB="$(fctl A --json hub create relayed | jq -r .group_ref)"
[ -n "$HUB" ] || die "hub create failed"
TICKET="$(fctl A hub invite "$HUB" 2>/dev/null | tail -n1)"
fctl B hub join "$TICKET" > /dev/null || die "hub join failed"

# A -> hub (published to A's in-process store, exposed via the ws relay)
fctl A hub send "$HUB" "over the external relay" > /dev/null || die "A send failed"
sleep 0.6
# B can only have received it through A's websocket relay (iroh embedded is off)
fctl B --json hub log "$HUB" > "$TESTDIR/b.json" || die "B log failed"
jq -e '.[] | select(.content == "over the external relay")' "$TESTDIR/b.json" > /dev/null \
    || die "B did not get the message via the external relay; got: $(cat "$TESTDIR/b.json")"

# B -> hub (B publishes to the external relay; A reads it from its own store)
fctl B hub send "$HUB" "reply via relay" > /dev/null || die "B send failed"
sleep 0.6
fctl A --json hub log "$HUB" > "$TESTDIR/a.json" || die "A log failed"
jq -e '.[] | select(.content == "reply via relay")' "$TESTDIR/a.json" > /dev/null \
    || die "A did not get B's reply via the external relay; got: $(cat "$TESTDIR/a.json")"

echo OK
