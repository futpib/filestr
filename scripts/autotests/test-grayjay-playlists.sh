#!/usr/bin/env bash
# The Grayjay plugin maps shared folders to playlists/albums: getPlaylistsUser
# lists this node's folders, getPlaylist resolves one to its files. Runs the
# plugin in Grayjay's runtime against a live gateway.
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

SCAFFOLD="${GRAYJAY_SCRIPTS:-$ROOT/../grayjay-android/app/src/main/assets/scripts}"
command -v node > /dev/null 2>&1 || { echo "SKIP (node not installed)"; exit 0; }
[ -f "$SCAFFOLD/source.js" ] || { echo "SKIP (grayjay scaffolding not found)"; exit 0; }
(cd "$ROOT" && cargo build -q -p filestrd --features grayjay) || die "build (grayjay) failed"

PORT=39087
export EXTRA_CONFIG=$'[http]\nlisten = "127.0.0.1:'"$PORT"'"'
start_node A --share
unset EXTRA_CONFIG
mkdir -p "$TESTDIR/A/share/album"
for i in 1 2 3; do head -c 65536 /dev/urandom > "$TESTDIR/A/share/album/track$i.mp3"; done
fctl A rescan > /dev/null

BASE="http://127.0.0.1:$PORT"
wait_http "$BASE/files"

cat > "$TESTDIR/pl.js" <<JS
const fs=require("fs"),vm=require("vm"),{execFileSync}=require("child_process");
function curl(m,url,h){const a=["-s","-X",m,url];for(const k of Object.keys(h||{}))a.push("-H",k+": "+h[k]);
  try{return {isOk:true,code:200,body:execFileSync("curl",a,{maxBuffer:1<<28}).toString("utf8")}}catch(e){return{isOk:false,code:0,body:""}}}
const SC="$SCAFFOLD";
const code=[fs.readFileSync(SC+"/polyfil.js","utf8"),fs.readFileSync(SC+"/source.js","utf8"),
  fs.readFileSync("$ROOT/grayjay-plugin/FilestrScript.js","utf8"),\`
  source.enable({id:"t"},{serverUrl:"$BASE"},null);
  out.urls=source.getPlaylistsUser();
  out.pls=out.urls.map(u=>{const p=source.getPlaylist(u);
    return {name:p.name,count:p.videoCount,isPl:source.isPlaylistUrl(u),
            firstUrl:(p.contents&&p.contents.results[0])?p.contents.results[0].url:null};});
\`].join("\n;\n");
const out={};
const ctx={console:{log(){}},bridge:{log(){},setTimeout,clearTimeout},
  http:{GET:(u,h)=>curl("GET",u,h),POST:(u,b,h)=>curl("POST",u,h)},out};
vm.createContext(ctx); vm.runInContext(code,ctx,{timeout:60000});
console.log(JSON.stringify(out));
JS

OUT="$(node "$TESTDIR/pl.js")"
echo "plugin playlists: $OUT"
echo "$OUT" | jq -e '[.pls[]|select(.name=="album")]|length == 1' > /dev/null || die "no 'album' playlist"
echo "$OUT" | jq -e '.pls[]|select(.name=="album")|.count == 3' > /dev/null || die "album playlist wrong count"
echo "$OUT" | jq -e '.pls[]|select(.name=="album")|.isPl == true' > /dev/null || die "isPlaylistUrl false"
echo "$OUT" | jq -e '.pls[]|select(.name=="album")|.firstUrl|test("/file/")' > /dev/null || die "playlist item not playable"

echo OK
