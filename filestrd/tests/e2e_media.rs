//! Port of test-media-metadata.sh — tags + duration + content-sniffing +
//! tag-search + cover-art thumbnails through the gateway. Needs ffmpeg.

mod common;
use common::{ffmpeg, have_ffmpeg, Node, NodeOpts};

fn sel<'a>(files: &'a serde_json::Value, suffix: &str) -> &'a serde_json::Value {
    files["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"].as_str().unwrap().ends_with(suffix))
        .unwrap_or_else(|| panic!("{suffix} not listed"))
}

#[tokio::test]
async fn media_metadata_tags_duration_sniffing_and_thumbnails() {
    if !have_ffmpeg() {
        eprintln!("SKIP (ffmpeg not installed)");
        return;
    }
    let a = Node::start("A", NodeOpts { share: true, http_port: Some(39155), ..Default::default() })
        .await;
    let sd = a.share_dir();

    // a 2s tagged MP3 and a 3s mp4
    ffmpeg(&[
        "-f", "lavfi", "-i", "sine=frequency=440:duration=2",
        "-metadata", "title=Test Title", "-metadata", "artist=Test Artist",
        "-metadata", "album=Test Album", "-write_xing", "1",
        sd.join("song.mp3").to_str().unwrap(),
    ]);
    ffmpeg(&[
        "-f", "lavfi", "-i", "testsrc=duration=3:size=320x240:rate=10",
        "-pix_fmt", "yuv420p", sd.join("clip.mp4").to_str().unwrap(),
    ]);
    // a tagged MP3 with embedded cover art
    let cover = a.dir().join("cover.png");
    ffmpeg(&["-f", "lavfi", "-i", "color=c=red:s=64x64:d=1", "-frames:v", "1", cover.to_str().unwrap()]);
    ffmpeg(&[
        "-f", "lavfi", "-i", "sine=frequency=330:duration=2", "-i", cover.to_str().unwrap(),
        "-map", "0:a", "-map", "1:v", "-c:v", "copy", "-id3v2_version", "3",
        "-metadata:s:v", "title=Album cover", "-metadata:s:v", "comment=Cover (front)",
        "-write_xing", "1", sd.join("withcover.mp3").to_str().unwrap(),
    ]);
    a.rescan().await;

    let http = a.http();
    http.wait_ready().await;
    let files = http.files().await;

    // audio tags + duration
    let song = sel(&files, "song.mp3");
    assert_eq!(song["media"]["title"], "Test Title");
    assert_eq!(song["media"]["artist"], "Test Artist");
    assert_eq!(song["media"]["album"], "Test Album");
    let adur = song["media"]["duration_secs"].as_f64().unwrap();
    assert!((1.8..2.3).contains(&adur), "mp3 duration {adur} (want ~2.0)");

    // video duration from the mp4 container
    let vdur = sel(&files, "clip.mp4")["media"]["duration_secs"].as_f64().unwrap();
    assert!((2.7..3.3).contains(&vdur), "mp4 duration {vdur} (want ~3.0)");

    // content sniffing: an mp3 (distinct tone -> distinct hash, not deduped) with
    // a .dat extension is detected + served as audio
    let mys = a.dir().join("mys.mp3");
    ffmpeg(&["-f", "lavfi", "-i", "sine=frequency=550:duration=2", "-write_xing", "1", mys.to_str().unwrap()]);
    std::fs::copy(&mys, sd.join("mystery.dat")).unwrap();
    a.rescan().await;
    let files = http.files().await;
    let myst = sel(&files, "mystery.dat");
    assert_eq!(myst["media"]["content_type"], "audio/mpeg", "misnamed mp3 not sniffed");
    let dhash = myst["hash"].as_str().unwrap();
    let served_ct = http.get_file(dhash, None).await;
    assert_eq!(served_ct.headers().get("content-type").unwrap(), "audio/mpeg");

    // search matches the artist tag, not just the filename
    let search = http.search("Test Artist").await;
    assert!(
        search["files"].as_array().unwrap().iter().any(|f| f["name"].as_str().unwrap().ends_with("song.mp3")),
        "search by artist tag did not find song.mp3"
    );

    // thumbnails: plain song has none; the cover mp3 does, served as an image
    assert!(sel(&files, "song.mp3")["thumb"].as_bool() != Some(true), "song.mp3 should have no thumb");
    let withcover = sel(&files, "withcover.mp3");
    assert_eq!(withcover["thumb"], true, "withcover.mp3 missing thumb flag");
    let thash = withcover["hash"].as_str().unwrap().to_string();
    let thumb = http.get(&format!("/thumb/{thash}")).await;
    assert_eq!(thumb.status(), 200);
    assert!(thumb.headers().get("content-type").unwrap().to_str().unwrap().starts_with("image/"));
    assert!(!thumb.bytes().await.unwrap().is_empty(), "/thumb returned no bytes");
    let thumb_path = a.dir().join("data/thumbs").join(&thash);
    assert!(thumb_path.exists(), "thumbnail not cached on disk");

    // removing the source prunes the cached thumbnail on rescan
    std::fs::remove_file(sd.join("withcover.mp3")).unwrap();
    a.rescan().await;
    assert!(!thumb_path.exists(), "stale thumbnail not pruned after removal");
}
