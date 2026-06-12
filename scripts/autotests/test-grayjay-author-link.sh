#!/usr/bin/env bash
# Every item the plugin emits must carry an author whose url is a resolvable
# channel url. Regression for "No source enabled to support this channel ()":
# the original plugin left the author link's url empty, so tapping the channel
# name under a video sent Grayjay an empty url that no source could resolve.
# Runs the plugin in Grayjay's own runtime against a live gateway and checks the
# author link on a home item, a search hit, and a content-details result.
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

SCAFFOLD="${GRAYJAY_SCRIPTS:-$ROOT/../grayjay-android/app/src/main/assets/scripts}"
command -v node > /dev/null 2>&1 || { echo "SKIP (node not installed)"; exit 0; }
[ -f "$SCAFFOLD/source.js" ] || { echo "SKIP (grayjay scaffolding not found)"; exit 0; }
(cd "$ROOT" && cargo build -q -p filestrd --features grayjay) || die "build (grayjay) failed"

PORT=39089
export EXTRA_CONFIG=$'[http]\nlisten = "127.0.0.1:'"$PORT"'"'
start_node A --share
unset EXTRA_CONFIG
head -c 131072 /dev/urandom > "$TESTDIR/A/share/clip.mp4"
fctl A rescan > /dev/null

BASE="http://127.0.0.1:$PORT"
wait_http "$BASE/files"

cat > "$TESTDIR/al.js" <<JS
const fs=require("fs"),vm=require("vm"),{execFileSync}=require("child_process");
function curl(m,url,h){const a=["-s","-X",m,url];for(const k of Object.keys(h||{}))a.push("-H",k+": "+h[k]);
  try{return {isOk:true,code:200,body:execFileSync("curl",a,{maxBuffer:1<<28}).toString("latin1")}}catch(e){return{isOk:false,code:0,body:""}}}
const SC="$SCAFFOLD";
const code=[fs.readFileSync(SC+"/polyfil.js","utf8"),fs.readFileSync(SC+"/source.js","utf8"),
  fs.readFileSync("$ROOT/grayjay-plugin/FilestrScript.js","utf8"),\`
  source.enable({id:"t"},{serverUrl:"$BASE"},null);
  const a=(x)=>x&&x.author?{url:x.author.url, ok:source.isChannelUrl(x.author.url||"")}:null;
  const home=source.getHome().results;
  out.home=a(home[0]);
  out.details=a(source.getContentDetails(home[0].url));
  const s=source.search("clip",null,null,[]).results;
  out.search=s.length?a(s[0]):{url:"<no hits>",ok:true};
\`].join("\n;\n");
const out={};
const ctx={console:{log(){}},bridge:{log(){},setTimeout,clearTimeout},
  http:{GET:(u,h)=>curl("GET",u,h),POST:(u,b,h)=>curl("POST",u,h)},out};
vm.createContext(ctx);
vm.runInContext(code,ctx,{timeout:60000});
console.log(JSON.stringify(out));
JS

OUT="$(node "$TESTDIR/al.js")"
echo "author links: $OUT"

for where in home details search; do
    URL="$(echo "$OUT" | jq -r ".$where.url")"
    [ -n "$URL" ] && [ "$URL" != "null" ] && [ "$URL" != "" ] \
        || die "$where author url is empty -> 'No source enabled to support this channel ()'"
    echo "$OUT" | jq -e ".$where.ok == true" > /dev/null \
        || die "$where author url not recognized by isChannelUrl: $URL"
done

echo OK
