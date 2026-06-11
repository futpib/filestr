#!/usr/bin/env bash
# Managing share roots from the CLI: `filestrctl share add <dir>` adds a
# directory (indexed immediately, persisted to the config file) and
# `share rm <name>` removes it — no hand-editing config + SIGHUP.
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

# a node with no shares of its own
start_node A
mkdir -p "$TESTDIR/media"
head -c 1024 /dev/urandom > "$TESTDIR/media/song.bin"
CONFIG="$TESTDIR/A/config.toml"

# starts empty
[ "$(fctl A --json share ls | jq '.shares | length')" = 0 ] || die "expected no shares initially"

# add: default name = basename, file indexed right away
ADD="$(fctl A --json share add "$TESTDIR/media")"
echo "$ADD" | jq -e '.shares[] | select(.name=="media") | .files == 1' > /dev/null \
    || die "share add did not add+index the dir: $ADD"

# persisted to the config file
grep -q 'name = "media"' "$CONFIG" || die "share not written to config"

# still there on a fresh listing
fctl A --json share ls | jq -e '.shares[] | select(.name=="media")' > /dev/null \
    || die "share ls missing media after add"

# a second dir with an explicit name
mkdir -p "$TESTDIR/docs"
echo hello > "$TESTDIR/docs/readme.txt"
fctl A --json share add "$TESTDIR/docs" --name documents > /dev/null
[ "$(fctl A --json share ls | jq '.shares | length')" = 2 ] || die "second share not added"

# adding a duplicate name fails
if fctl A share add "$TESTDIR/media" > /dev/null 2>&1; then
    die "duplicate share name should have failed"
fi

# remove one; it disappears from the listing and the config file
fctl A share rm media > /dev/null
fctl A --json share ls | jq -e 'all(.shares[]; .name != "media")' > /dev/null \
    || die "media still listed after rm"
if grep -q 'name = "media"' "$CONFIG"; then
    die "media still in config after rm"
fi
# the other share is untouched
fctl A --json share ls | jq -e '.shares[] | select(.name=="documents")' > /dev/null \
    || die "documents share lost during rm"

# removing a non-existent share fails
if fctl A share rm nope > /dev/null 2>&1; then
    die "removing a non-existent share should have failed"
fi

echo OK
