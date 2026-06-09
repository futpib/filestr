#!/usr/bin/env bash
# allow_reshare=false on the A->B grant: B itself can still search and fetch
# A's content, but must not re-serve it to C. (DESIGN.md §7.5)
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

start_node A --share
echo "do not reshare me" > "$TESTDIR/A/share/private.txt"
fctl A rescan > /dev/null

start_node B
fctl B peer add "$(fctl A invite create --no-reshare 2>/dev/null)" > /dev/null

start_node C
fctl C peer add "$(fctl B invite create 2>/dev/null)" > /dev/null

# B's own searches still reach A
fctl B --json search private > "$TESTDIR/b-hits.json"
[ -s "$TESTDIR/b-hits.json" ] || die "B should find A's file for itself"

# C must see nothing: B honors A's allow_reshare=false
fctl C --json search private > "$TESTDIR/c-hits.json"
if [ -s "$TESTDIR/c-hits.json" ]; then
    die "C found content that must not be reshared: $(cat "$TESTDIR/c-hits.json")"
fi

echo OK
