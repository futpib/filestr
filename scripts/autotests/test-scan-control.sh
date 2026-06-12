#!/usr/bin/env bash
# Control over the background share scan:
#   - `share add` indexes in the background and serves files AS THEY HASH
#     (incremental) — the served count tracks indexing.done, instead of jumping
#     from 0 to everything only when the whole scan finishes;
#   - the scan is pausable/resumable (`rescan --pause` / `--resume`) and
#     cancellable (`rescan --cancel`), with done/total + paused in `status`.
# Mid-scan sampling is best-effort (hashing speed varies); the pause/resume/cancel
# commands and full recovery are asserted unconditionally.
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

start_node A

# Many small files (cloned, so setup is fast) so the scan stays in flight long
# enough to sample and pause. Each is still hashed + probed independently.
N=1500
mkdir -p "$TESTDIR/lib"
head -c 4096 /dev/urandom > "$TESTDIR/lib/f1.bin"
for i in $(seq 2 $N); do cp "$TESTDIR/lib/f1.bin" "$TESTDIR/lib/f$i.bin"; done

# share add returns immediately; indexing happens in the background
fctl A share add "$TESTDIR/lib" > /dev/null
# pause right away so there's still work queued when the pause lands
fctl A rescan --pause > /dev/null || die "rescan --pause errored"

# --- catch the paused, mid-scan state -----------------------------------------
caught=no
for i in $(seq 1 100); do
    S="$(fctl A --json status 2>/dev/null)"
    paused=$(echo "$S" | jq -r '.indexing.paused // false')
    done=$(echo "$S" | jq -r '.indexing.done // empty')
    total=$(echo "$S" | jq -r '.indexing.total // empty')
    files=$(echo "$S" | jq -r '.files')
    if [ "$paused" = "true" ] && [ -n "$done" ] && [ "$done" -lt "$total" ]; then
        caught=yes
        break
    fi
    # status JSON must always be coherent (done <= total)
    [ -z "$done" ] || [ "$done" -le "$total" ] || die "indexing.done > total: $(echo "$S"|jq -c .indexing)"
    [ "$files" = "$N" ] && break   # whole scan finished before we caught the pause
    sleep 0.05
done

if [ "$caught" = yes ]; then
    echo "paused mid-scan (first sample): done=$done/$total served=$files"
    # Let the in-flight jobs that were already launched before the pause landed
    # finish and drain (pause stops *new* launches, not in-flight ones), then the
    # indexed count must hold.
    sleep 1.5
    S1="$(fctl A --json status 2>/dev/null)"
    done1=$(echo "$S1" | jq -r '.indexing.done // empty')
    files1=$(echo "$S1" | jq -r '.files')
    total1=$(echo "$S1" | jq -r '.indexing.total // empty')
    echo "after settle: done=$done1/$total1 served=$files1"
    # incremental serving: every file hashed so far is already served
    [ "$files1" = "$done1" ] || die "served=$files1 != indexed=$done1 (not serving incrementally)"
    [ "$done1" -ge 1 ] || die "paused with nothing served yet"
    # genuinely paused mid-scan, not finished
    [ "$done1" -lt "$total1" ] || die "scan completed instead of pausing"
    # pause holds: the indexed count doesn't keep climbing
    sleep 1.5
    done2=$(fctl A --json status 2>/dev/null | jq -r '.indexing.done // empty')
    [ "$done2" = "$done1" ] || die "scan kept hashing while paused ($done1 -> $done2)"
    echo "pause holds (done stable at $done2 < $total1; served==indexed)"
else
    echo "NOTE: scan finished too fast to sample the paused state this run"
fi

# --- resume and finish --------------------------------------------------------
fctl A rescan --resume > /dev/null || die "rescan --resume errored"
wait_share_files A lib "$N"
served="$(fctl A --json status | jq -r '.files')"
[ "$served" = "$N" ] || die "after resume, served=$served, expected $N"
echo "resumed and fully indexed ($served files)"

# --- cancel / pause / resume are clean no-ops when idle -----------------------
fctl A rescan --cancel > /dev/null || die "rescan --cancel (idle) errored"
fctl A rescan --pause  > /dev/null || die "rescan --pause (idle) errored"
fctl A rescan --resume > /dev/null || die "rescan --resume (idle) errored"

echo OK
