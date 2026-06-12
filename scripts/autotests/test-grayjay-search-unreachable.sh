#!/usr/bin/env bash
# Search must not hang on an unreachable peer. The federated /search fans out to
# granted peers; a dead peer used to stall the WHOLE search on a hard-coded 10s
# connect timeout, so in Grayjay the filestr search spun ~forever while every
# other plugin had already returned. The connect timeout is now short and
# configurable, so a dead peer is given up on quickly and the (local + reachable)
# results come back promptly.
#
#   G (gateway + plugin, shares a match)  --grant-->  A (killed: unreachable)
#
# e2e: drive source.search() through Grayjay's runtime and assert it returns the
# local hit well within the old 10s hang.
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

SCAFFOLD="${GRAYJAY_SCRIPTS:-$ROOT/../grayjay-android/app/src/main/assets/scripts}"
command -v node > /dev/null 2>&1 || { echo "SKIP (node not installed)"; exit 0; }
[ -f "$SCAFFOLD/source.js" ] || { echo "SKIP (grayjay scaffolding not found)"; exit 0; }
(cd "$ROOT" && cargo build -q -p filestrd --features grayjay) || die "build (grayjay) failed"

# A is the soon-to-be-dead peer.
start_node A --share
head -c 131072 /dev/urandom > "$TESTDIR/A/share/unrelated.mp4"
fctl A rescan > /dev/null

# G shares a file whose name matches our query, and has a short connect timeout
# so the regression (a dead peer hanging the search) is measurable in seconds.
PORT=39091
export EXTRA_CONFIG=$'[http]\nlisten = "127.0.0.1:'"$PORT"$'"\n[search]\nconnect_timeout_secs = 1\ntimeout_secs = 8'
start_node G --share
unset EXTRA_CONFIG
head -c 131072 /dev/urandom > "$TESTDIR/G/share/tekwars-clip.mp4"
fctl G rescan > /dev/null
fctl G peer add "$(fctl A invite create 2>/dev/null)" > /dev/null

BASE="http://127.0.0.1:$PORT"
wait_http "$BASE/files"
wait_share_files G files 1

# kill A so the grant points at an unreachable peer
kill "$PID_A" 2>/dev/null
sleep 1

# --- the regression: search must return promptly, not on the 10s connect hang --
START=$(date +%s.%N)
node - "$SCAFFOLD" "$ROOT" "$BASE" > "$TESTDIR/search_out.json" <<'JS'
const fs=require("fs"),vm=require("vm"),{execFileSync}=require("child_process");
const [SC,ROOT,BASE]=process.argv.slice(2);
function curl(m,url,h){const a=["-s","-X",m,url];for(const k of Object.keys(h||{}))a.push("-H",k+": "+h[k]);
  try{return {isOk:true,code:200,body:execFileSync("curl",a,{maxBuffer:1<<28}).toString("latin1")}}catch(e){return{isOk:false,code:0,body:""}}}
const code=[fs.readFileSync(SC+"/polyfil.js","utf8"),fs.readFileSync(SC+"/source.js","utf8"),
  fs.readFileSync(ROOT+"/grayjay-plugin/FilestrScript.js","utf8"),`
  source.enable({id:"t"},{serverUrl:"${BASE}"},null);
  const r=source.search("tekwars",null,null,{},null);
  out.count=r.results.length;
  out.names=r.results.map(v=>v.name);
`].join("\n;\n");
const out={};
const ctx={console:{log(){}},bridge:{log(){},setTimeout,clearTimeout},
  http:{GET:(u,h)=>curl("GET",u,h),POST:(u,b,h)=>curl("POST",u,h)},out};
vm.createContext(ctx);
vm.runInContext(code,ctx,{timeout:60000});
console.log(JSON.stringify(out));
JS
END=$(date +%s.%N)
ELAPSED=$(python3 -c "print(f'{$END-$START:.1f}')")
OUT="$(cat "$TESTDIR/search_out.json")"
echo "search took ${ELAPSED}s -> $OUT"

# the old hard-coded 10s connect hang would blow this; the fix returns in ~1-2s
python3 -c "import sys; sys.exit(0 if $ELAPSED < 6 else 1)" \
    || die "search took ${ELAPSED}s — a dead peer is still stalling it (the hang)"
echo "$OUT" | jq -e '.count >= 1' > /dev/null \
    || die "search returned no results (local hit lost): $OUT"
echo "$OUT" | jq -e '[.names[]|select(test("tekwars"))]|length >= 1' > /dev/null \
    || die "search did not return the matching local file: $OUT"

echo OK
