#!/usr/bin/env bash
# Reputation / anti-free-riding: a peer that only takes is denied past the
# credit limit; a per-peer override (reloaded via SIGHUP) lifts the limit.
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

# A serves; tiny credit limit, no newcomer budget, no decay during the test
EXTRA_CONFIG=$'[reputation]\ncredit_limit_mib = 1\nnewcomer_budget_mib = 0\nhalf_life_days = 3650'
start_node A --share
unset EXTRA_CONFIG

# three ~0.7 MiB files: under the 1 MiB limit the first two go, the third tips
# the debt over and must be denied
for n in 1 2 3; do head -c 700000 /dev/urandom > "$TESTDIR/A/share/f$n.bin"; done
fctl A rescan > /dev/null

start_node B   # B = pure leecher (shares nothing)
fctl B peer add "$(fctl A invite create 2>/dev/null)" > /dev/null
A_ID="$(node_id A)"
fctl B --json browse "$A_ID" > "$TESTDIR/list.json"
h() { jq -r '.[] | select(.path | endswith("'"$1"'")) | .hash' "$TESTDIR/list.json"; }

fctl B get "$(h f1.bin)" -o "$TESTDIR/o1" > /dev/null || die "f1 should be served (under limit)"
fctl B get "$(h f2.bin)" -o "$TESTDIR/o2" > /dev/null || die "f2 should be served (under limit)"

# third fetch must be refused for free-riding
if fctl B get "$(h f3.bin)" -o "$TESTDIR/o3" 2> "$TESTDIR/deny.err"; then
    die "f3 should have been denied (over credit limit)"
fi
grep -qi "refused\|rate_limited\|credit" "$TESTDIR/deny.err" \
    || die "denial for wrong reason: $(cat "$TESTDIR/deny.err")"

# A's ledger should show B as denied, with debt and zero received
fctl A --json rep > "$TESTDIR/rep.json"
B_ID="$(node_id B)"
jq -e '.[] | select(.node_id | startswith("'"${B_ID:0:8}"'")) | select(.action == "deny")' \
    "$TESTDIR/rep.json" > /dev/null || die "A should mark B denied; got: $(cat "$TESTDIR/rep.json")"
RECV="$(jq -r '.[] | select(.node_id | startswith("'"${B_ID:0:8}"'")) | .received' "$TESTDIR/rep.json")"
[ "$RECV" = 0 ] || die "B served nothing, expected received=0, got $RECV"

# per-peer override: raise B's credit limit, reload A, and the fetch succeeds
cat >> "$TESTDIR/A/config.toml" <<EOF

[[reputation.override]]
peer = "$B_ID"
credit_limit_mib = 1000
EOF
kill -HUP "$PID_A"
sleep 0.7
fctl B get "$(h f3.bin)" -o "$TESTDIR/o3" > /dev/null \
    || die "f3 should be served after per-peer override"
cmp "$TESTDIR/o3" "$TESTDIR/A/share/f3.bin" || die "f3 content differs"

echo OK
