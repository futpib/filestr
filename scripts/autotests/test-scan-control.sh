#!/usr/bin/env bash
# Control over the background share scan: progress is visible in `status`, the
# scan can be cancelled (`rescan --cancel`), and the share recovers on a later
# rescan. (Timing-exact mid-scan assertions are avoided — hashing speed varies —
# but the cancel path and recovery are exercised end to end.)
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

start_node A
mkdir -p "$TESTDIR/lib"
for i in $(seq 1 5); do head -c 1048576 /dev/urandom > "$TESTDIR/lib/f$i.bin"; done

# share add returns immediately; indexing happens in the background
fctl A share add "$TESTDIR/lib" > /dev/null

# status JSON always parses, and when a scan is in flight it carries
# indexing.done/indexing.total
fctl A --json status > "$TESTDIR/st.json"
jq -e '.indexing == null or (.indexing.total >= .indexing.done)' "$TESTDIR/st.json" > /dev/null \
    || die "bad indexing field: $(jq -c .indexing "$TESTDIR/st.json")"

# cancelling is always safe — whether or not a scan is currently running
fctl A rescan --cancel > /dev/null || die "rescan --cancel errored"

# cancelling when nothing is running is a clean no-op
fctl A rescan --cancel > /dev/null || die "rescan --cancel (idle) errored"

# a normal rescan re-indexes everything; the share ends up fully indexed
fctl A rescan > /dev/null
wait_share_files A lib 5

echo OK
