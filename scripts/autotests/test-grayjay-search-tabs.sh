#!/usr/bin/env bash
# The Grayjay search screen has Media / Creators / Playlists tabs. filestr backs
# all three: search() (Media, covered elsewhere), searchChannels() (Creators =
# your peers + this node, matched by label) and searchPlaylists() (Playlists =
# album/artist groupings across the reachable library). Also checks the
# lightweight /peers endpoint that creator search uses. Runs the plugin in
# Grayjay's runtime against a live gateway.
#
#   A (gateway + tagged shares)  <--  peers with  -->  B
#
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

SCAFFOLD="${GRAYJAY_SCRIPTS:-$ROOT/../grayjay-android/app/src/main/assets/scripts}"
command -v node > /dev/null 2>&1 || { echo "SKIP (node not installed)"; exit 0; }
command -v ffmpeg > /dev/null 2>&1 || { echo "SKIP (ffmpeg not installed)"; exit 0; }
[ -f "$SCAFFOLD/source.js" ] || { echo "SKIP (grayjay scaffolding not found)"; exit 0; }
(cd "$ROOT" && cargo build -q -p filestrd --features grayjay) || die "build (grayjay) failed"

PORT=39098
export EXTRA_CONFIG=$'[http]\nlisten = "127.0.0.1:'"$PORT"'"'
start_node A --share
unset EXTRA_CONFIG

mk() { # $1 file  $2 artist  $3 album  $4 title
    ffmpeg -v error -f lavfi -i "sine=frequency=440:duration=1" \
        -metadata artist="$2" -metadata album="$3" -metadata title="$4" \
        -write_xing 1 -y "$TESTDIR/A/share/$1"
}
mk gh1.mp3 "Tester" "Greatest Hits" "Hit One"
mk gh2.mp3 "Tester" "Greatest Hits" "Hit Two"
mk bs1.mp3 "Tester" "B Sides" "Rarity"
fctl A rescan > /dev/null

# B is a peer of A (no shares needed) so /peers and creator search have a peer
start_node B
fctl A peer add "$(fctl B invite create 2>/dev/null)" > /dev/null

BASE="http://127.0.0.1:$PORT"
wait_http "$BASE/files"
for i in $(seq 1 50); do
    [ "$(curl -s "$BASE/files" | jq '[.files[]|select(.media.artist=="Tester")]|length')" = "3" ] && break
    sleep 0.2
done

# --- /peers endpoint: granted peer present, no browse needed -------------------
curl -s "$BASE/peers" > "$TESTDIR/peers.json"
echo "peers: $(cat "$TESTDIR/peers.json")"
[ "$(jq '.peers|length' "$TESTDIR/peers.json")" -ge 1 ] || die "/peers returned no granted peer"
PEER_LABEL="$(jq -r '.peers[0].label' "$TESTDIR/peers.json")"
PEER_SUB="${PEER_LABEL:0:6}"   # a substring of the peer's label to search for
[ -n "$PEER_SUB" ] || die "peer label empty"

cat > "$TESTDIR/st.js" <<JS
const fs=require("fs"),vm=require("vm"),{execFileSync}=require("child_process");
function curl(m,url,h){const a=["-s","-X",m,url];for(const k of Object.keys(h||{}))a.push("-H",k+": "+h[k]);
  try{return {isOk:true,code:200,body:execFileSync("curl",a,{maxBuffer:1<<28}).toString("utf8")}}catch(e){return{isOk:false,code:0,body:""}}}
const SC="$SCAFFOLD";
const code=[fs.readFileSync(SC+"/polyfil.js","utf8"),fs.readFileSync(SC+"/source.js","utf8"),
  fs.readFileSync("$ROOT/grayjay-plugin/FilestrScript.js","utf8"),\`
  source.enable({id:"t"},{serverUrl:"$BASE"},null);
  // Playlists tab
  out.plArtist=source.searchPlaylists("tester").results.map(p=>({name:p.name,count:p.videoCount,url:p.url}));
  out.plAlbum=source.searchPlaylists("greatest").results.map(p=>p.name);
  out.plNone=source.searchPlaylists("zzqqxx-nomatch").results.length;
  // Creators tab
  out.chThis=source.searchChannels("this").results.map(c=>c.name);
  out.chAll=source.searchChannels("").results.length;
  out.chNone=source.searchChannels("zzqq-nomatch").results.length;
  out.chPeer=source.searchChannels("$PEER_SUB").results.map(c=>c.name);
\`].join("\n;\n");
const out={};
const ctx={console:{log(){}},bridge:{log(){},setTimeout,clearTimeout},
  http:{GET:(u,h)=>curl("GET",u,h),POST:(u,b,h)=>curl("POST",u,h)},out};
vm.createContext(ctx); vm.runInContext(code,ctx,{timeout:60000});
console.log(JSON.stringify(out));
JS

OUT="$(node "$TESTDIR/st.js")"
echo "search tabs: $OUT"

# --- Playlists tab ------------------------------------------------------------
echo "$OUT" | jq -e '[.plArtist[]|select(.name=="Tester")]|length==1' >/dev/null || die "searchPlaylists(tester) missing artist"
echo "$OUT" | jq -e '.plArtist[]|select(.name=="Tester")|.count==3' >/dev/null || die "searchPlaylists artist count wrong"
echo "$OUT" | jq -e '.plArtist[]|select(.name=="Tester")|.url|test("/playlist/")' >/dev/null || die "searchPlaylists result has no playlist url"
echo "$OUT" | jq -e '[.plAlbum[]|select(.=="Greatest Hits")]|length==1' >/dev/null || die "searchPlaylists(greatest) missing album"
echo "$OUT" | jq -e '.plNone==0' >/dev/null || die "searchPlaylists nonsense query should be empty"

# --- Creators tab -------------------------------------------------------------
echo "$OUT" | jq -e '[.chThis[]|select(.=="This node")]|length==1' >/dev/null || die "searchChannels(this) should match This node"
echo "$OUT" | jq -e '.chAll>=2' >/dev/null || die "searchChannels() should return local + the peer"
echo "$OUT" | jq -e '.chNone==0' >/dev/null || die "searchChannels nonsense query should be empty"
echo "$OUT" | jq -e '.chPeer|length>=1' >/dev/null || die "searchChannels by peer-label substring missed the peer"

echo OK
