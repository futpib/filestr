#!/usr/bin/env bash
# A channel page's Playlists tab is driven by source.getChannelPlaylists(url):
# Grayjay shows it whenever the plugin defines that method (login-free), which is
# where filestr surfaces a source's albums/artists/folders for browsing. This
# checks that getChannelPlaylists on the local channel returns playlist STUBS
# (name + count, no contents) scoped to that source, and that getPlaylist then
# resolves a stub's url to exactly that group's tracks. Runs the plugin in
# Grayjay's runtime against a live gateway.
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

SCAFFOLD="${GRAYJAY_SCRIPTS:-$ROOT/../grayjay-android/app/src/main/assets/scripts}"
command -v node > /dev/null 2>&1 || { echo "SKIP (node not installed)"; exit 0; }
command -v ffmpeg > /dev/null 2>&1 || { echo "SKIP (ffmpeg not installed)"; exit 0; }
[ -f "$SCAFFOLD/source.js" ] || { echo "SKIP (grayjay scaffolding not found)"; exit 0; }
(cd "$ROOT" && cargo build -q -p filestrd --features grayjay) || die "build (grayjay) failed"

PORT=39094
export EXTRA_CONFIG=$'[http]\nlisten = "127.0.0.1:'"$PORT"'"'
start_node A --share
unset EXTRA_CONFIG

# artist "Tester": 3 tracks across two albums; grouping is tag-driven.
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
for i in $(seq 1 50); do
    [ "$(curl -s "$BASE/files" | jq '[.files[]|select(.media.artist=="Tester")]|length')" = "3" ] && break
    sleep 0.2
done

cat > "$TESTDIR/cp.js" <<JS
const fs=require("fs"),vm=require("vm"),{execFileSync}=require("child_process");
function curl(m,url,h){const a=["-s","-X",m,url];for(const k of Object.keys(h||{}))a.push("-H",k+": "+h[k]);
  try{return {isOk:true,code:200,body:execFileSync("curl",a,{maxBuffer:1<<28}).toString("utf8")}}catch(e){return{isOk:false,code:0,body:""}}}
const SC="$SCAFFOLD";
const code=[fs.readFileSync(SC+"/polyfil.js","utf8"),fs.readFileSync(SC+"/source.js","utf8"),
  fs.readFileSync("$ROOT/grayjay-plugin/FilestrScript.js","utf8"),\`
  source.enable({id:"t"},{serverUrl:"$BASE"},null);
  const pager=source.getChannelPlaylists("$BASE/channel/local");
  out.stubs=pager.results.map(p=>({name:p.name,count:p.videoCount,url:p.url,
    hasContents:!!(p.contents)}));
  // resolve the artist stub to its tracks
  const art=pager.results.find(p=>p.name==="Tester");
  const det=art?source.getPlaylist(art.url):null;
  out.artist=det?{name:det.name,count:det.videoCount,
    first:(det.contents&&det.contents.results[0])?det.contents.results[0].url:null}:null;
\`].join("\n;\n");
const out={};
const ctx={console:{log(){}},bridge:{log(){},setTimeout,clearTimeout},
  http:{GET:(u,h)=>curl("GET",u,h),POST:(u,b,h)=>curl("POST",u,h)},out};
vm.createContext(ctx); vm.runInContext(code,ctx,{timeout:60000});
console.log(JSON.stringify(out));
JS

OUT="$(node "$TESTDIR/cp.js")"
echo "channel playlists: $OUT"

# stubs are lightweight: name + count, NO resolved contents (Grayjay fetches
# those lazily via getPlaylist when one is opened)
echo "$OUT" | jq -e '.stubs | length >= 3' > /dev/null || die "expected folder+album+artist stubs"
echo "$OUT" | jq -e 'all(.stubs[]; .hasContents == false)' > /dev/null || die "stub carried contents (should be lazy)"
echo "$OUT" | jq -e 'all(.stubs[]; .url|test("/playlist/"))' > /dev/null || die "stub missing playlist url"

# one artist playlist "Tester" with 3 tracks; two albums split by tag
echo "$OUT" | jq -e '[.stubs[]|select(.name=="Tester")]|length == 1' > /dev/null || die "no single artist stub"
echo "$OUT" | jq -e '.stubs[]|select(.name=="Tester")|.count == 3' > /dev/null || die "artist stub wrong count"
echo "$OUT" | jq -e '.stubs[]|select(.name=="Greatest Hits")|.count == 2' > /dev/null || die "album 'Greatest Hits' wrong count"
echo "$OUT" | jq -e '.stubs[]|select(.name=="B Sides")|.count == 1' > /dev/null || die "album 'B Sides' wrong count"

# the artist stub's url resolves to exactly its 3 tracks, all playable
echo "$OUT" | jq -e '.artist.count == 3' > /dev/null || die "resolved artist playlist wrong count"
echo "$OUT" | jq -e '.artist.first|test("/file/")' > /dev/null || die "resolved track not playable"

echo OK
