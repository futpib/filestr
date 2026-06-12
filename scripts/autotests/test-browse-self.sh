#!/usr/bin/env bash
# `filestrctl browse` with no peer lists this node's own shared files (same
# FileEntry output as browsing a peer), so you can see what you're serving
# without a gateway. With a peer argument it still browses that peer.
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

start_node A --share
for n in alpha bravo charlie; do head -c 4096 /dev/urandom > "$TESTDIR/A/share/$n.bin"; done
fctl A rescan > /dev/null
wait_share_files A files 3

OUT="$(fctl A --json browse)"
echo "own shares: $OUT"
n=$(echo "$OUT" | jq 'length')
[ "$n" = "3" ] || die "browse (self) returned $n entries, expected 3"
for name in alpha bravo charlie; do
    echo "$OUT" | jq -e --arg p "files/$name.bin" 'any(.[]; .path == $p)' > /dev/null \
        || die "own browse missing $name.bin"
done
# every entry carries a content hash (so `get <hash>` would work)
echo "$OUT" | jq -e 'all(.[]; .hash | test("^[0-9a-f]+$"))' > /dev/null \
    || die "browse entry missing a hash"

# plain (non-json) output lists the files too
fctl A browse | grep -q "files/alpha.bin" || die "plain browse output missing files"

# browsing a non-existent peer still errors (peer arg path unchanged)
fctl A browse nonesuchpeer > /dev/null 2>&1 && die "browsing a bogus peer should fail"

echo OK
