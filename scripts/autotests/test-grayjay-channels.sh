#!/usr/bin/env bash
# The Grayjay plugin maps peers to channels: getUserSubscriptions lists your
# peers as channels, and getChannel/getChannelContents browse a peer's library.
# Runs the plugin in Grayjay's runtime against a 2-node gateway.
#
#   A (shares files)  <--  G (gateway + plugin)
#
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

SCAFFOLD="${GRAYJAY_SCRIPTS:-$ROOT/../grayjay-android/app/src/main/assets/scripts}"
command -v node > /dev/null 2>&1 || { echo "SKIP (node not installed)"; exit 0; }
[ -f "$SCAFFOLD/source.js" ] || { echo "SKIP (grayjay scaffolding not found)"; exit 0; }
(cd "$ROOT" && cargo build -q -p filestrd --features grayjay) || die "build (grayjay) failed"

start_node A --share
head -c 131072 /dev/urandom > "$TESTDIR/A/share/song.mp3"
head -c 131072 /dev/urandom > "$TESTDIR/A/share/clip.mp4"
fctl A rescan > /dev/null

PORT=39086
export EXTRA_CONFIG=$'[http]\nlisten = "127.0.0.1:'"$PORT"'"'
start_node G
unset EXTRA_CONFIG
fctl G peer add "$(fctl A invite create 2>/dev/null)" > /dev/null

BASE="http://127.0.0.1:$PORT"
wait_http "$BASE/files"
for i in $(seq 1 50); do
    [ "$(curl -s "$BASE/files" | jq '[.files[]|select(.source!="local")]|length')" -ge 1 ] && break
    sleep 0.2
done

cat > "$TESTDIR/ch.js" <<JS
const fs=require("fs"),vm=require("vm"),{execFileSync}=require("child_process");
function curl(m,url,h){const a=["-s","-X",m,url];for(const k of Object.keys(h||{}))a.push("-H",k+": "+h[k]);
  try{return {isOk:true,code:200,body:execFileSync("curl",a,{maxBuffer:1<<28}).toString("latin1")}}catch(e){return{isOk:false,code:0,body:""}}}
const SC="$SCAFFOLD";
const code=[fs.readFileSync(SC+"/polyfil.js","utf8"),fs.readFileSync(SC+"/source.js","utf8"),
  fs.readFileSync("$ROOT/grayjay-plugin/FilestrScript.js","utf8"),\`
  source.enable({id:"t"},{serverUrl:"$BASE"},null);
  out.subs=source.getUserSubscriptions();
  out.ch = out.subs.length ? source.getChannel(out.subs[0]) : null;
  const c = out.subs.length ? source.getChannelContents(out.subs[0],null,null,{}) : {results:[]};
  out.contentCount=c.results.length;
  out.firstUrl=c.results[0]?c.results[0].url:null;
  out.isChannelUrl=out.subs.length ? source.isChannelUrl(out.subs[0]) : false;
\`].join("\n;\n");
const out={};
const ctx={console:{log(){}},bridge:{log(){},setTimeout,clearTimeout},
  http:{GET:(u,h)=>curl("GET",u,h),POST:(u,b,h)=>curl("POST",u,h)},out};
vm.createContext(ctx);
vm.runInContext(code,ctx,{timeout:60000});
console.log(JSON.stringify(out));
JS

OUT="$(node "$TESTDIR/ch.js")"
echo "plugin channels: $OUT"
echo "$OUT" | jq -e '.subs | length >= 1' > /dev/null || die "getUserSubscriptions returned no peer channels"
echo "$OUT" | jq -e '.isChannelUrl == true' > /dev/null || die "isChannelUrl false for a channel url"
echo "$OUT" | jq -e '.ch.name | length > 0' > /dev/null || die "getChannel returned no name"
echo "$OUT" | jq -e '.contentCount == 2' > /dev/null || die "getChannelContents wrong count"
echo "$OUT" | jq -e '.firstUrl | test("/file/")' > /dev/null || die "channel content not playable"

echo OK
