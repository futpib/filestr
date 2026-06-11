#!/usr/bin/env bash
# The index is persisted, so a restart reuses unchanged files instead of
# re-hashing the whole library. A changed file is still re-hashed.
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

start_node A --share
head -c 4194304 /dev/urandom > "$TESTDIR/A/share/a.bin"
head -c 1048576 /dev/urandom > "$TESTDIR/A/share/b.bin"
fctl A rescan > /dev/null

# first scan hashed both (start_node indexed on startup, then rescan reused;
# either way they're indexed now). Restart and confirm the scan reused them.
restart_node A
# the daemon logs "share scan complete files=N reused=R hashed=H" at -vv
LINE="$(rg -N 'share scan complete' "$TESTDIR/A/daemon.log" | tail -1)"
echo "restart scan: $LINE"
echo "$LINE" | grep -q 'reused=2' || die "restart did not reuse the cache: $LINE"
echo "$LINE" | grep -q 'hashed=0' || die "restart re-hashed despite the cache: $LINE"

# change one file (size differs) -> on rescan it is re-hashed, the other reused
head -c 2097152 /dev/urandom > "$TESTDIR/A/share/a.bin"
fctl A rescan > /dev/null
LINE2="$(rg -N 'share scan complete' "$TESTDIR/A/daemon.log" | tail -1)"
echo "rescan after change: $LINE2"
echo "$LINE2" | grep -q 'reused=1' || die "changed-file rescan should reuse the other file: $LINE2"
echo "$LINE2" | grep -q 'hashed=1' || die "changed file should be re-hashed: $LINE2"

echo OK
