#!/usr/bin/env bash
# The local HTTP gateway from a media player's standpoint: a player on the same
# device can LIST files, PROBE one with HEAD (no transfer), RANGE-stream it,
# revalidate with the ETag (304 / If-Range), and fetch the embedded Grayjay
# plugin. Single node, files served from its own share.
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

# the gateway is behind the off-by-default `grayjay` feature
(cd "$ROOT" && cargo build -q -p filestrd --features grayjay) || die "build (grayjay) failed"

PORT=39080
export EXTRA_CONFIG=$'[http]\nlisten = "127.0.0.1:'"$PORT"'"'
start_node A --share
unset EXTRA_CONFIG

head -c 262144 /dev/urandom > "$TESTDIR/A/share/clip.bin"
printf '0123456789abcdef' > "$TESTDIR/A/share/hello.txt"
fctl A rescan > /dev/null

BASE="http://127.0.0.1:$PORT"
wait_http "$BASE/files"

# --- LIST: the file appears with its hash and size --------------------------
curl -s "$BASE/files" > "$TESTDIR/files.json"
HASH="$(jq -r '.files[] | select(.name|endswith("clip.bin")) | .hash' "$TESTDIR/files.json")"
SIZE="$(jq -r '.files[] | select(.name|endswith("clip.bin")) | .size' "$TESTDIR/files.json")"
[ -n "$HASH" ] || die "clip.bin not listed by /files"
[ "$SIZE" = 262144 ] || die "wrong size from /files: $SIZE"

# --- HEAD: probe size/type/range support with no body, no transfer ----------
HCL="$(curl -s -I -o /dev/null -w '%header{content-length}' "$BASE/file/$HASH")"
[ "$HCL" = 262144 ] || die "HEAD content-length: $HCL"
HET="$(curl -s -I -o /dev/null -w '%header{etag}' "$BASE/file/$HASH")"
[ "$HET" = "\"$HASH\"" ] || die "HEAD etag: $HET"
HAR="$(curl -s -I -o /dev/null -w '%header{accept-ranges}' "$BASE/file/$HASH")"
[ "$HAR" = bytes ] || die "HEAD accept-ranges: $HAR"
HBODY="$(curl -s -I -o /dev/null -w '%{size_download}' "$BASE/file/$HASH")"
[ "$HBODY" = 0 ] || die "HEAD returned a body ($HBODY bytes)"

# --- GET: full content matches the original ---------------------------------
curl -s "$BASE/file/$HASH?name=clip.bin" -o "$TESTDIR/clip.out"
cmp "$TESTDIR/clip.out" "$TESTDIR/A/share/clip.bin" || die "full GET bytes differ"

# --- RANGE: 206 with the correct slice --------------------------------------
RSTAT="$(curl -s -o "$TESTDIR/r.out" -w '%{http_code}' -H 'Range: bytes=0-99' "$BASE/file/$HASH")"
[ "$RSTAT" = 206 ] || die "range status not 206: $RSTAT"
[ "$(wc -c < "$TESTDIR/r.out")" = 100 ] || die "range body not 100 bytes"
head -c 100 "$TESTDIR/A/share/clip.bin" > "$TESTDIR/r.orig"
cmp "$TESTDIR/r.out" "$TESTDIR/r.orig" || die "range bytes differ"

# --- CONDITIONAL: If-None-Match with the ETag -> 304 ------------------------
CSTAT="$(curl -s -o /dev/null -w '%{http_code}' -H "If-None-Match: \"$HASH\"" "$BASE/file/$HASH")"
[ "$CSTAT" = 304 ] || die "If-None-Match not 304: $CSTAT"

# --- If-Range: match honours the range (206); mismatch serves full (200) ----
IRM="$(curl -s -o /dev/null -w '%{http_code}' -H 'Range: bytes=0-99' -H "If-Range: \"$HASH\"" "$BASE/file/$HASH")"
[ "$IRM" = 206 ] || die "If-Range match not 206: $IRM"
IRX="$(curl -s -o /dev/null -w '%{http_code}' -H 'Range: bytes=0-99' -H 'If-Range: "deadbeef"' "$BASE/file/$HASH")"
[ "$IRX" = 200 ] || die "If-Range mismatch not 200: $IRX"

# --- CONTENT-TYPE inferred from the ?name= hint -----------------------------
CT="$(curl -s -o /dev/null -w '%header{content-type}' "$BASE/file/$HASH?name=hello.txt")"
case "$CT" in text/plain*) ;; *) die "content-type by name wrong: $CT";; esac

# --- GRAYJAY plugin served, URLs rewritten to this host:port -----------------
curl -s "$BASE/grayjay/FilestrConfig.json" > "$TESTDIR/cfg.json"
jq -e '.scriptUrl | contains("127.0.0.1:'"$PORT"'")' "$TESTDIR/cfg.json" > /dev/null \
    || die "grayjay config scriptUrl not rewritten to gateway host"
curl -sf "$BASE/grayjay/FilestrScript.js" > /dev/null || die "grayjay script not served"

# --- LOGS: daemon output is plain text, no ANSI colour escapes (so it stays
# --- readable when the app surfaces it on a failed-start screen) ------------
if grep -q $'\x1b\[' "$TESTDIR/A/daemon.log"; then
    die "daemon log contains ANSI escape codes"
fi

echo OK
