#!/usr/bin/env bash
# Join over nostr with no public notes: the owner shares a compact hub address
# (not published), the newcomer sends a NIP-17 gift-wrapped join request over a
# relay, and the owner auto-admits. No prior grant between them.
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

PORT=$((20000 + ($$ % 10000)))

# A: owner — hosts a websocket relay, uses it, and auto-admits requests
EXTRA_CONFIG="$(printf '[chat]\nrelay_listen = "127.0.0.1:%s"\nrelays = ["ws://127.0.0.1:%s"]\nauto_admit = true' "$PORT" "$PORT")"
start_node A --share
unset EXTRA_CONFIG

# B: newcomer — only knows the public relay (carried in the hub address)
EXTRA_CONFIG="$(printf '[chat]\nrelays = ["ws://127.0.0.1:%s"]' "$PORT")"
start_node B --share
unset EXTRA_CONFIG

HUB="$(fctl A --json hub create privatehub | jq -r .group_ref)"
[ -n "$HUB" ] || die "hub create failed"

# owner produces a shareable address (a small pointer, not a published note)
ADDR="$(fctl A hub address "$HUB" 2>/dev/null)"
case "$ADDR" in filestraddr1*) ;; *) die "bad hub address: $ADDR" ;; esac

# newcomer requests to join using the address (gift-wrapped DM over the relay)
fctl B hub request "$ADDR" > /dev/null || die "request failed"

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

# E2EE chat works for the new member
fctl A hub send "$HUB" "auto-admitted hello" > /dev/null || die "A send failed"
sleep 0.3
fctl B --json hub log "$HUB" > "$TESTDIR/b.json"
jq -e '.[] | select(.content == "auto-admitted hello")' "$TESTDIR/b.json" > /dev/null \
    || die "auto-admitted member cannot read hub chat"

# the request that crossed the relay was gift-wrapped (kind 1059), so no
# plaintext request ticket should ever touch disk
if grep -aqr "filestrreq1" "$TESTDIR/A/data" 2>/dev/null; then
    die "a plaintext request ticket leaked to disk — not gift-wrapped"
fi

echo OK
