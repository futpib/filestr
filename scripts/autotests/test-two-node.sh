#!/usr/bin/env bash
# Two nodes: invite/redeem, browse, search, verified fetch, single-use token,
# revoke. (PLAN.md gates M1-M3)
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

start_node A --share
echo "hello filestr" > "$TESTDIR/A/share/hello.txt"
mkdir -p "$TESTDIR/A/share/sub"
head -c 4096 /dev/urandom > "$TESTDIR/A/share/sub/data.bin"
fctl A rescan | grep -q "rescanned: 2 files" || die "rescan did not find 2 files"

TICKET="$(fctl A invite create --label e2e 2>/dev/null)"
case "$TICKET" in filestr1*) ;; *) die "bad ticket: $TICKET" ;; esac

start_node B --share
echo "from B" > "$TESTDIR/B/share/from-b.txt"
fctl B rescan > /dev/null
fctl B peer add "$TICKET" > /dev/null || die "peer add failed"
A_ID="$(node_id A)"; B_ID="$(node_id B)"

# tickets are symmetric: redeeming A's invite also lets A browse B
fctl A --json browse "$B_ID" > "$TESTDIR/a-browses-b.json" 2>/dev/null \
    || die "symmetric: A cannot browse B after B redeemed A's invite"
jq -e '.[] | select(.path | endswith("from-b.txt"))' "$TESTDIR/a-browses-b.json" > /dev/null \
    || die "symmetric: A does not see B's file"

# browse
fctl B --json browse "$A_ID" > "$TESTDIR/browse.json" || die "browse failed"
COUNT="$(jq length "$TESTDIR/browse.json")"
[ "$COUNT" = 2 ] || die "expected 2 entries, got $COUNT"
HASH="$(jq -r '.[] | select(.path | endswith("hello.txt")) | .hash' "$TESTDIR/browse.json")"
[ -n "$HASH" ] || die "hello.txt not in listing"

# search (hits stream from A, attributed via=A locally)
fctl B --json search hello > "$TESTDIR/hits.json" || die "search failed"
grep -q "$HASH" "$TESTDIR/hits.json" || die "search did not find hello.txt"
VIA="$(jq -r 'select(.hash == "'"$HASH"'") | .via' "$TESTDIR/hits.json" | head -n 1)"
[ "$VIA" = "$A_ID" ] || die "expected via=$A_ID, got $VIA"

# verified fetch
fctl B get "$HASH" -o "$TESTDIR/out.txt" > /dev/null || die "get failed"
cmp "$TESTDIR/out.txt" "$TESTDIR/A/share/hello.txt" || die "fetched content differs"

# the big one too
HASH2="$(jq -r '.[] | select(.path | endswith("data.bin")) | .hash' "$TESTDIR/browse.json")"
fctl B get "$HASH2" -o "$TESTDIR/out.bin" > /dev/null || die "get data.bin failed"
cmp "$TESTDIR/out.bin" "$TESTDIR/A/share/sub/data.bin" || die "data.bin differs"

# single-use: a different node cannot redeem the same token
start_node C
if fctl C peer add "$TICKET" 2> "$TESTDIR/reuse.err"; then
    die "token reuse should have failed"
fi
grep -qi "denied\|refused" "$TESTDIR/reuse.err" || die "reuse failed for wrong reason: $(cat "$TESTDIR/reuse.err")"

# re-redemption by the same node is fine (lost-response recovery)
fctl B peer add "$TICKET" > /dev/null || die "same-node re-redeem should succeed"

# revoke: B loses access
fctl A peer revoke "$(node_id B)" > /dev/null || die "revoke failed"
if fctl B browse "$A_ID" > /dev/null 2> "$TESTDIR/revoked.err"; then
    die "browse should fail after revoke"
fi

echo OK
