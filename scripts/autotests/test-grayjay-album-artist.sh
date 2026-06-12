#!/usr/bin/env bash
# The plugin groups the library by embedded tags, not just folders:
# getPlaylistsUser exposes one playlist per `album` tag and one per `artist` tag
# (across the whole reachable library), and getPlaylist resolves each to its
# tracks. So a tagged collection reads like a music library regardless of how
# it's foldered. Runs the plugin in Grayjay's runtime against a live gateway.
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

SCAFFOLD="${GRAYJAY_SCRIPTS:-$ROOT/../grayjay-android/app/src/main/assets/scripts}"
command -v node > /dev/null 2>&1 || { echo "SKIP (node not installed)"; exit 0; }
command -v ffmpeg > /dev/null 2>&1 || { echo "SKIP (ffmpeg not installed)"; exit 0; }
[ -f "$SCAFFOLD/source.js" ] || { echo "SKIP (grayjay scaffolding not found)"; exit 0; }
(cd "$ROOT" && cargo build -q -p filestrd --features grayjay) || die "build (grayjay) failed"

PORT=39092
export EXTRA_CONFIG=$'[http]\nlisten = "127.0.0.1:'"$PORT"'"'
start_node A --share
unset EXTRA_CONFIG

# artist "Tester": 3 tracks; album "Greatest Hits": 2; album "B Sides": 1.
# Files are flat (no folders) so grouping is purely tag-driven.
mk() { # $1 file  $2 artist  $3 album  $4 title
    ffmpeg -v error -f lavfi -i "sine=frequency=440:duration=1" \
        -metadata artist="$2" -metadata album="$3" -metadata title="$4" \
        -write_xing 1 -y "$TESTDIR/A/share/$1"
}
mk gh1.mp3 "Tester" "Greatest Hits" "Hit One"
mk gh2.mp3 "Tester" "Greatest Hits" "Hit Two"
mk bs1.mp3 "Tester" "B Sides" "Rarity"
fctl A rescan > /dev/null

BASE="http://127.0.0.1:$PORT"
wait_http "$BASE/files"
# wait until tags are indexed (background scan)
for i in $(seq 1 50); do
    [ "$(curl -s "$BASE/files" | jq '[.files[]|select(.media.artist=="Tester")]|length')" = "3" ] && break
    sleep 0.2
done

cat > "$TESTDIR/aa.js" <<JS
const fs=require("fs"),vm=require("vm"),{execFileSync}=require("child_process");
function curl(m,url,h){const a=["-s","-X",m,url];for(const k of Object.keys(h||{}))a.push("-H",k+": "+h[k]);
  try{return {isOk:true,code:200,body:execFileSync("curl",a,{maxBuffer:1<<28}).toString("utf8")}}catch(e){return{isOk:false,code:0,body:""}}}
const SC="$SCAFFOLD";
const code=[fs.readFileSync(SC+"/polyfil.js","utf8"),fs.readFileSync(SC+"/source.js","utf8"),
  fs.readFileSync("$ROOT/grayjay-plugin/FilestrScript.js","utf8"),\`
  source.enable({id:"t"},{serverUrl:"$BASE"},null);
  out.pls=source.getPlaylistsUser().map(u=>{const p=source.getPlaylist(u);
    return {name:p.name,count:p.videoCount,isPl:source.isPlaylistUrl(u),
            first:(p.contents&&p.contents.results[0])?p.contents.results[0].url:null};});
\`].join("\n;\n");
const out={};
const ctx={console:{log(){}},bridge:{log(){},setTimeout,clearTimeout},
  http:{GET:(u,h)=>curl("GET",u,h),POST:(u,b,h)=>curl("POST",u,h)},out};
vm.createContext(ctx); vm.runInContext(code,ctx,{timeout:60000});
console.log(JSON.stringify(out));
JS

OUT="$(node "$TESTDIR/aa.js")"
echo "plugin playlists: $OUT"

# one artist playlist with all 3 tracks
echo "$OUT" | jq -e '[.pls[]|select(.name=="Tester")]|length == 1' > /dev/null || die "no single 'Tester' artist playlist"
echo "$OUT" | jq -e '.pls[]|select(.name=="Tester")|.count == 3'      > /dev/null || die "artist playlist wrong count"
echo "$OUT" | jq -e '.pls[]|select(.name=="Tester")|.isPl == true'    > /dev/null || die "isPlaylistUrl false for artist"
echo "$OUT" | jq -e '.pls[]|select(.name=="Tester")|.first|test("/file/")' > /dev/null || die "artist track not playable"

# two album playlists, split by album tag
echo "$OUT" | jq -e '.pls[]|select(.name=="Greatest Hits")|.count == 2' > /dev/null || die "album 'Greatest Hits' wrong count"
echo "$OUT" | jq -e '.pls[]|select(.name=="B Sides")|.count == 1'       > /dev/null || die "album 'B Sides' wrong count"

echo OK
