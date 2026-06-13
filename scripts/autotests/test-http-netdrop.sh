#!/usr/bin/env bash
# When the source peer is unreachable (the "5G dropped" case), the gateway must
# FAIL FAST with a RETRYABLE error, not hang and not lie. Specifically:
#   - a peer file whose source can't be reached -> 503 (retryable), promptly,
#     never a 206/200 with a truncated body (a player reads that as a broken
#     stream that play/pause won't recover) and never a 404 (players treat 404
#     as fatal and cache the item as gone -> "it caches the brokenness forever");
#   - once the peer is back, the very same request succeeds with correct bytes.
# "Network out/in" is simulated with SIGSTOP/SIGCONT on the provider: its address
# stays valid, it just stops answering — exactly like the radio dropping.
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

(cd "$ROOT" && cargo build -q -p filestrd --features grayjay) || die "build (grayjay) failed"

start_node A --share
SIZE=9437184  # 9 MiB, multi-window
head -c "$SIZE" /dev/urandom > "$TESTDIR/A/share/movie.bin"
fctl A rescan > /dev/null

# gateway G granted to A; short connect/io timeouts so a drop fails fast
PORT=39139
EXTRA_CONFIG="$(printf '[http]\nlisten = "127.0.0.1:%s"\n[search]\nconnect_timeout_secs = 2\nbrowse_timeout_secs = 2\nio_timeout_secs = 3' "$PORT")"
export EXTRA_CONFIG
start_node G
unset EXTRA_CONFIG
fctl G peer add "$(fctl A invite create 2>/dev/null)" > /dev/null

BASE="http://127.0.0.1:$PORT"
wait_http "$BASE/files"
HASH="$(curl -s "$BASE/files" | jq -r '.files[]|select(.name|endswith("movie.bin"))|.hash')"
[ -n "$HASH" ] || die "movie.bin not listed by gateway"

# An unknown (valid-format) hash with no known source -> 503 (retryable), NOT 404.
UNK="$(printf '0%.0s' $(seq 1 64))"
S_UNK="$(curl -s -o /dev/null -w '%{http_code}' "$BASE/file/$UNK")"
[ "$S_UNK" = 503 ] || die "unknown hash should be 503 (retryable), got $S_UNK"

# Baseline: a ranged GET works while A is up.
S0="$(curl -s -m 15 -H 'Range: bytes=100000-100099' "$BASE/file/$HASH" -o "$TESTDIR/b0.out" -w '%{http_code}')"
[ "$S0" = 206 ] || die "baseline range not 206: $S0"

# --- network OUT ---
kill -STOP "$PID_A"
sleep 0.3
# A fresh range (not yet fetched) must fail FAST with a retryable 503, not hang
# and not return a 2xx-with-empty-body.
T1=$(date +%s)
S1="$(curl -s -m 20 -H 'Range: bytes=200000-200099' "$BASE/file/$HASH" -o "$TESTDIR/b1.out" -w '%{http_code}' || echo curlfail)"
T2=$(date +%s)
ELAPSED=$((T2 - T1))
[ "$S1" = 503 ] || { kill -CONT "$PID_A"; die "during outage expected 503, got $S1 (body $(wc -c < "$TESTDIR/b1.out") bytes)"; }
[ "$ELAPSED" -le 8 ] || { kill -CONT "$PID_A"; die "during outage took ${ELAPSED}s — should fail fast"; }

# --- network BACK ---
kill -CONT "$PID_A"
sleep 1

# The SAME request now succeeds with the correct bytes (no "cached brokenness").
ok=0
for attempt in 1 2 3; do
    S2="$(curl -s -m 20 -H 'Range: bytes=200000-200099' "$BASE/file/$HASH" -o "$TESTDIR/b2.out" -w '%{http_code}' || echo curlfail)"
    [ "$S2" = 206 ] && [ "$(wc -c < "$TESTDIR/b2.out")" = 100 ] && { ok=1; break; }
    sleep 2
done
[ "$ok" = 1 ] || die "request did not recover after the peer came back (last http=$S2)"
dd if="$TESTDIR/A/share/movie.bin" bs=1 skip=200000 count=100 status=none > "$TESTDIR/b2.orig"
cmp "$TESTDIR/b2.out" "$TESTDIR/b2.orig" || die "recovered bytes differ"

# And a full GET still reassembles correctly after the whole ordeal.
curl -s -m 30 "$BASE/file/$HASH" -o "$TESTDIR/full.out"
cmp "$TESTDIR/full.out" "$TESTDIR/A/share/movie.bin" || die "full reassembly differs after recovery"

echo OK
