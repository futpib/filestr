#!/usr/bin/env bash
# A peer can share more files than the recent_sources LRU (4096) holds. The
# gateway must still resolve the size/source of EVERY browsable file — otherwise
# a single browse self-evicts most of its own entries from the LRU and those
# files can't be streamed at all (they 503 forever, regardless of network). This
# guards the browse_sources map: the full per-peer listing, never self-evicting.
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

(cd "$ROOT" && cargo build -q -p filestrd --features grayjay) || die "build (grayjay) failed"

# provider A shares > 4096 files (the LRU cap), each tiny and unique.
start_node A --share
N=5000
echo "creating $N files..."
for i in $(seq 1 "$N"); do
    printf 'filestr large-peer test file number %d\n' "$i" > "$TESTDIR/A/share/f$i.txt"
done
fctl A rescan > /dev/null
wait_share_files A files "$N"

# gateway node G granted to A
PORT=39140
EXTRA_CONFIG="$(printf '[http]\nlisten = "127.0.0.1:%s"' "$PORT")"
export EXTRA_CONFIG
start_node G
unset EXTRA_CONFIG
fctl G peer add "$(fctl A invite create 2>/dev/null)" > /dev/null

BASE="http://127.0.0.1:$PORT"
wait_http "$BASE/files"

# one browse populates the gateway's source map with all N peer files
for i in $(seq 1 80); do
    [ "$(curl -s "$BASE/files" | jq '[.files[]|select(.source!="local")]|length')" -ge "$N" ] && break
    sleep 0.25
done
curl -s "$BASE/files" > "$TESTDIR/g.json"
PEERCOUNT="$(jq '[.files[]|select(.source!="local")]|length' "$TESTDIR/g.json")"
[ "$PEERCOUNT" -ge "$N" ] || die "gateway only listed $PEERCOUNT/$N peer files"

# Sample 100 files spread across the whole listing and require EVERY one to
# stream. With only the 4096-entry LRU, ~18% (the evicted ones) would 503; with
# browse_sources, all resolve.
mapfile -t SAMPLE < <(jq -r '[.files[]|select(.source!="local")]|.[].hash' "$TESTDIR/g.json" \
    | awk 'NR%50==1')   # every 50th -> ~100 hashes across the listing
echo "sampling ${#SAMPLE[@]} files across the listing"
fails=0
for h in "${SAMPLE[@]}"; do
    code="$(curl -s -o /dev/null -w '%{http_code}' "$BASE/file/$h")"
    case "$code" in
        200|206) ;;
        *) fails=$((fails + 1)); [ "$fails" -le 5 ] && echo "  FAIL $h -> $code" ;;
    esac
done
[ "$fails" -eq 0 ] || die "$fails/${#SAMPLE[@]} sampled peer files did not stream (source evicted from the cache)"

# And the very first file in the listing (most likely to have been evicted by a
# naive LRU) streams with correct bytes.
FH="$(jq -r '[.files[]|select(.source!="local")]|.[0].hash' "$TESTDIR/g.json")"
FN="$(jq -r '[.files[]|select(.source!="local")]|.[0].name' "$TESTDIR/g.json")"
curl -s "$BASE/file/$FH" -o "$TESTDIR/first.out"
cmp "$TESTDIR/first.out" "$TESTDIR/A/share/$(basename "$FN")" || die "first file bytes differ"

echo OK
