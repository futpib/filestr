#!/usr/bin/env bash
# getContentDetails must hand Grayjay a *playable* source with an authoritative
# duration. Regression for the "00:00 / -12:-55" player bug: an audio file was
# returned as a bare VideoUrlSource, so the player ignored our duration and
# guessed it from the MP3 frames (mis-reading VBR files into a negative time).
# The fix: audio -> UnMuxVideoSourceDescriptor + AudioUrlSource carrying the
# index's exact duration; video -> VideoSourceDescriptor + VideoUrlSource.
# Runs the plugin in Grayjay's own runtime against a live gateway.
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

SCAFFOLD="${GRAYJAY_SCRIPTS:-$ROOT/../grayjay-android/app/src/main/assets/scripts}"
command -v node > /dev/null 2>&1 || { echo "SKIP (node not installed)"; exit 0; }
command -v ffmpeg > /dev/null 2>&1 || { echo "SKIP (ffmpeg not installed)"; exit 0; }
[ -f "$SCAFFOLD/source.js" ] || { echo "SKIP (grayjay scaffolding not found)"; exit 0; }
(cd "$ROOT" && cargo build -q -p filestrd --features grayjay) || die "build (grayjay) failed"

PORT=39088
export EXTRA_CONFIG=$'[http]\nlisten = "127.0.0.1:'"$PORT"'"'
start_node A --share
unset EXTRA_CONFIG

# a 2s MP3 (Xing header so the index reads a duration) and a 3s mp4
ffmpeg -v error -f lavfi -i "sine=frequency=440:duration=2" \
    -write_xing 1 -y "$TESTDIR/A/share/song.mp3"
ffmpeg -v error -f lavfi -i "testsrc=duration=3:size=320x240:rate=10" \
    -pix_fmt yuv420p -y "$TESTDIR/A/share/clip.mp4"
fctl A rescan > /dev/null

BASE="http://127.0.0.1:$PORT"
wait_http "$BASE/files"

cat > "$TESTDIR/cd.js" <<JS
const fs=require("fs"),vm=require("vm"),{execFileSync}=require("child_process");
function curl(m,url,h){const a=["-s","-X",m,url];for(const k of Object.keys(h||{}))a.push("-H",k+": "+h[k]);
  try{return {isOk:true,code:200,body:execFileSync("curl",a,{maxBuffer:1<<28}).toString("latin1")}}catch(e){return{isOk:false,code:0,body:""}}}
const SC="$SCAFFOLD";
const code=[fs.readFileSync(SC+"/polyfil.js","utf8"),fs.readFileSync(SC+"/source.js","utf8"),
  fs.readFileSync("$ROOT/grayjay-plugin/FilestrScript.js","utf8"),\`
  source.enable({id:"t"},{serverUrl:"$BASE"},null);
  const home=source.getHome().results;
  const find=(ext)=>home.find(v=>String(v.url).indexOf(ext)!==-1);
  const dump=(d)=>{
    const desc=d.video||{};
    const a=(desc.audioSources||[])[0]||null;
    const v=(desc.videoSources||[])[0]||null;
    return {duration:d.duration, descType:desc.plugin_type, isUnMuxed:desc.isUnMuxed,
            nVideo:(desc.videoSources||[]).length, nAudio:(desc.audioSources||[]).length,
            audio:a&&{type:a.plugin_type,container:a.container,duration:a.duration},
            video:v&&{type:v.plugin_type,container:v.container,duration:v.duration}};
  };
  out.audio=dump(source.getContentDetails(find("song.mp3").url));
  out.video=dump(source.getContentDetails(find("clip.mp4").url));
\`].join("\n;\n");
const out={};
const ctx={console:{log(){}},bridge:{log(){},setTimeout,clearTimeout},
  http:{GET:(u,h)=>curl("GET",u,h),POST:(u,b,h)=>curl("POST",u,h)},out};
vm.createContext(ctx);
vm.runInContext(code,ctx,{timeout:60000});
console.log(JSON.stringify(out));
JS

OUT="$(node "$TESTDIR/cd.js")"
echo "content details: $OUT"

# --- AUDIO: an AudioUrlSource with a real, positive duration ----------------
echo "$OUT" | jq -e '.audio.descType == "UnMuxVideoSourceDescriptor"' > /dev/null \
    || die "audio not unmuxed (was served as video -> the -12:-55 bug)"
echo "$OUT" | jq -e '.audio.isUnMuxed == true'    > /dev/null || die "audio descriptor not flagged unmuxed"
echo "$OUT" | jq -e '.audio.nVideo == 0'          > /dev/null || die "audio descriptor has a video source"
echo "$OUT" | jq -e '.audio.audio.type == "AudioUrlSource"' > /dev/null || die "audio source not an AudioUrlSource"
echo "$OUT" | jq -e '.audio.audio.container | startswith("audio/")' > /dev/null || die "audio container not audio/*"
# the regression: duration must be present and positive on BOTH the details and
# the source, so the player never has to guess it from the MP3 frames
echo "$OUT" | jq -e '.audio.duration > 1 and .audio.duration < 4' > /dev/null \
    || die "audio details duration off: $(echo "$OUT"|jq .audio.duration) (want ~2)"
echo "$OUT" | jq -e '.audio.audio.duration > 1 and .audio.audio.duration < 4' > /dev/null \
    || die "audio source duration off/missing: $(echo "$OUT"|jq .audio.audio.duration) (want ~2)"
echo "$OUT" | jq -e '.audio.audio.duration == .audio.duration' > /dev/null \
    || die "audio source/details duration mismatch"

# --- VIDEO: still a normal muxed VideoUrlSource ------------------------------
echo "$OUT" | jq -e '.video.descType == "MuxVideoSourceDescriptor"' > /dev/null || die "video not a mux descriptor"
echo "$OUT" | jq -e '.video.video.type == "VideoUrlSource"' > /dev/null || die "video source not a VideoUrlSource"
echo "$OUT" | jq -e '.video.video.duration > 2 and .video.video.duration < 4' > /dev/null \
    || die "video source duration off: $(echo "$OUT"|jq .video.video.duration) (want ~3)"

echo OK
