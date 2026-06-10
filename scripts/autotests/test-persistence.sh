#!/usr/bin/env bash
# MLS hub state persists across daemon restarts (item 1 of ROADMAP.md):
# the hub registry, group membership, and decrypted chat history all survive
# stopping and relaunching the daemon.
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

start_node A --share     # owner
start_node B --share     # member

HUB="$(fctl A --json hub create persists | jq -r .group_ref)"
[ -n "$HUB" ] || die "hub create failed"
fctl B hub join "$(fctl A hub invite "$HUB" 2>/dev/null | tail -n1)" > /dev/null || die "join failed"

fctl A hub send "$HUB" "before restart (owner)" > /dev/null || die "A send failed"
sleep 0.3
fctl B hub send "$HUB" "before restart (member)" > /dev/null || die "B send failed"
sleep 0.3
fctl A hub log "$HUB" > /dev/null   # owner syncs member's message into its store
fctl B hub log "$HUB" > /dev/null   # member syncs owner's message into its store

# restart both daemons (cold) — same data/state dirs
restart_node A
restart_node B

# owner: hub registry, membership and full history survived
fctl A --json hub ls | jq -e '.[] | select(.group_ref == "'"$HUB"'")' > /dev/null \
    || die "owner lost the hub after restart"
M="$(fctl A --json hub members "$HUB" | jq length)"
[ "$M" = 2 ] || die "owner lost membership after restart (got $M)"
fctl A --json hub log "$HUB" > "$TESTDIR/a.json"
for msg in "before restart (owner)" "before restart (member)"; do
    jq -e '.[] | select(.content == "'"$msg"'")' "$TESTDIR/a.json" > /dev/null \
        || die "owner lost history after restart: missing '$msg'"
done

# member: same — reading stored history needs no network
fctl B --json hub ls | jq -e '.[] | select(.group_ref == "'"$HUB"'")' > /dev/null \
    || die "member lost the hub after restart"
fctl B --json hub log "$HUB" > "$TESTDIR/b.json"
for msg in "before restart (owner)" "before restart (member)"; do
    jq -e '.[] | select(.content == "'"$msg"'")' "$TESTDIR/b.json" > /dev/null \
        || die "member lost history after restart: missing '$msg'"
done

# the on-disk MLS store is encrypted at rest (no plaintext message in the db)
if grep -aq "before restart" "$TESTDIR/A/data/mls.sqlite" 2>/dev/null; then
    die "MLS db contains plaintext — not encrypted at rest"
fi

echo OK
