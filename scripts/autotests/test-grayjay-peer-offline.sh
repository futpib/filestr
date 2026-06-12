#!/usr/bin/env bash
# An unreachable peer must be reported as OFFLINE, not silently dropped. Before
# this, a peer that didn't answer the browse just vanished from /files, so the
# app and Grayjay couldn't tell "offline" from "has nothing to share" — you'd
# get a generic error or an empty list with no hint that nobody was there to
# serve. Now /files carries a `peers` array with per-peer `reachable`, the plugin
# keeps an offline peer visible as a subscription, marks its channel offline, and
# raises a clear error when you open it.
#
#   A (shares files)  <--  G (gateway + plugin)   ... then A is killed
#
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

SCAFFOLD="${GRAYJAY_SCRIPTS:-$ROOT/../grayjay-android/app/src/main/assets/scripts}"
command -v node > /dev/null 2>&1 || { echo "SKIP (node not installed)"; exit 0; }
[ -f "$SCAFFOLD/source.js" ] || { echo "SKIP (grayjay scaffolding not found)"; exit 0; }
(cd "$ROOT" && cargo build -q -p filestrd --features grayjay) || die "build (grayjay) failed"

start_node A --share
head -c 131072 /dev/urandom > "$TESTDIR/A/share/clip.mp4"
fctl A rescan > /dev/null

PORT=39090
export EXTRA_CONFIG=$'[http]\nlisten = "127.0.0.1:'"$PORT"$'"\n[search]\nbrowse_timeout_secs = 2'
start_node G
unset EXTRA_CONFIG
fctl G peer add "$(fctl A invite create 2>/dev/null)" > /dev/null

BASE="http://127.0.0.1:$PORT"
wait_http "$BASE/files"
# wait until A's file shows up (peer reachable)
for i in $(seq 1 50); do
    [ "$(curl -s "$BASE/files" | jq '[.files[]|select(.source!="local")]|length')" -ge 1 ] && break
    sleep 0.2
done
curl -s "$BASE/files" | jq -e '[.peers[]|select(.reachable==true)]|length >= 1' > /dev/null \
    || die "peer not reported reachable while it is up"

# take A offline
kill "$PID_A" 2>/dev/null
sleep 1

# A must now be reported offline (reachable=false), still listed, not dropped
for i in $(seq 1 30); do
    off="$(curl -s "$BASE/files" | jq '[.peers[]|select(.reachable==false)]|length')"
    [ "$off" -ge 1 ] && break
    sleep 0.3
done
echo "peers when offline: $(curl -s "$BASE/files" | jq -c .peers)"
curl -s "$BASE/files" | jq -e '[.peers[]|select(.reachable==false)]|length >= 1' > /dev/null \
    || die "offline peer not reported reachable=false (silently dropped — the bug)"

cat > "$TESTDIR/off.js" <<JS
const fs=require("fs"),vm=require("vm"),{execFileSync}=require("child_process");
function curl(m,url,h){const a=["-s","-X",m,url];for(const k of Object.keys(h||{}))a.push("-H",k+": "+h[k]);
  try{return {isOk:true,code:200,body:execFileSync("curl",a,{maxBuffer:1<<28}).toString("latin1")}}catch(e){return{isOk:false,code:0,body:""}}}
const SC="$SCAFFOLD";
const code=[fs.readFileSync(SC+"/polyfil.js","utf8"),fs.readFileSync(SC+"/source.js","utf8"),
  fs.readFileSync("$ROOT/grayjay-plugin/FilestrScript.js","utf8"),\`
  source.enable({id:"t"},{serverUrl:"$BASE"},null);
  out.subs=source.getUserSubscriptions();
  out.desc=out.subs.length?source.getChannel(out.subs[0]).description:null;
  try { source.getChannelContents(out.subs[0],null,null,{}); out.threw=false; }
  catch(e){ out.threw=true; out.msg=String(e); }
\`].join("\n;\n");
const out={};
const ctx={console:{log(){}},bridge:{log(){},setTimeout,clearTimeout},
  http:{GET:(u,h)=>curl("GET",u,h),POST:(u,b,h)=>curl("POST",u,h)},out};
vm.createContext(ctx);
vm.runInContext(code,ctx,{timeout:60000});
console.log(JSON.stringify(out));
JS

OUT="$(node "$TESTDIR/off.js")"
echo "plugin offline: $OUT"
echo "$OUT" | jq -e '.subs | length >= 1' > /dev/null \
    || die "offline peer dropped from subscriptions (should stay visible)"
echo "$OUT" | jq -e '.desc | test("Offline")' > /dev/null \
    || die "offline peer channel not marked Offline: $(echo "$OUT"|jq .desc)"
echo "$OUT" | jq -e '.threw == true' > /dev/null \
    || die "opening an offline channel did not raise an error (showed empty list)"
echo "$OUT" | jq -e '.msg | test("offline")' > /dev/null \
    || die "offline channel error message not descriptive: $(echo "$OUT"|jq .msg)"

echo OK
