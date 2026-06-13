//! Port of test-http-playlists.sh — server-side /playlists groupings, /playlist
//! resolution, and /related siblings. Needs ffmpeg for tagged-audio fixtures.

mod common;
use common::{ffmpeg, have_ffmpeg, Node, NodeOpts};

fn mk(node: &Node, file: &str, artist: &str, album: &str, title: &str) {
    let out = node.share_dir().join(file);
    ffmpeg(&[
        "-f", "lavfi", "-i", "sine=frequency=440:duration=1",
        "-metadata", &format!("artist={artist}"),
        "-metadata", &format!("album={album}"),
        "-metadata", &format!("title={title}"),
        "-write_xing", "1",
        out.to_str().unwrap(),
    ]);
}

fn img(node: &Node, rel: &str, color: &str) {
    let out = node.share_dir().join(rel);
    std::fs::create_dir_all(out.parent().unwrap()).unwrap();
    ffmpeg(&["-f", "lavfi", "-i", &format!("color=c={color}:s=16x16"), "-frames:v", "1", out.to_str().unwrap()]);
}

fn group_count(res: &serde_json::Value, kind: &str, name: &str) -> Option<u64> {
    res[kind].as_array()?.iter().find(|g| g["name"].as_str() == Some(name))?["count"].as_u64()
}

#[tokio::test]
async fn playlists_groupings_resolution_and_related() {
    if !have_ffmpeg() {
        eprintln!("SKIP (ffmpeg not installed)");
        return;
    }
    let a = Node::start("A", NodeOpts { share: true, http_port: Some(39154), ..Default::default() })
        .await;
    mk(&a, "gh1.mp3", "Tester", "Greatest Hits", "Hit One");
    mk(&a, "gh2.mp3", "Tester", "Greatest Hits", "Hit Two");
    mk(&a, "bs1.mp3", "Tester", "B Sides", "Rarity");
    std::fs::write(a.share_dir().join("notes.bin"), common::pseudo_bytes(4096, 1)).unwrap();
    img(&a, "artwork/cover.jpg", "red"); // image-only folder -> not a playlist
    img(&a, "folder.jpg", "blue"); // cover alongside music -> excluded from count
    a.rescan().await;

    let http = a.http();
    http.wait_ready().await;
    common::wait_until("tracks indexed", || async {
        http.files().await["files"]
            .as_array()
            .map(|a| a.iter().filter(|f| f["media"]["artist"] == "Tester").count())
            == Some(3)
    })
    .await;

    // --- whole-library groupings -----------------------------------------
    let pl = http.json("/playlists").await;
    assert_eq!(group_count(&pl, "albums", "Greatest Hits"), Some(2), "Greatest Hits count");
    assert_eq!(group_count(&pl, "albums", "B Sides"), Some(1), "B Sides count");
    assert_eq!(group_count(&pl, "artists", "Tester"), Some(3), "artist count (no non-media)");
    assert_eq!(group_count(&pl, "folders", "files"), Some(3), "folder count (no .bin/.jpg)");
    assert!(
        !pl["folders"].as_array().unwrap().iter().any(|g| g["name"] == "artwork"),
        "image-only folder served as a playlist"
    );
    assert!(pl.get("peers").is_some(), "missing peers array");
    // folder key is the path; album key is the name
    let files_folder = pl["folders"].as_array().unwrap().iter().find(|g| g["name"] == "files").unwrap();
    assert_eq!(files_folder["key"], "files");

    // --- ?source= scope --------------------------------------------------
    let local = http.json("/playlists?source=local").await;
    assert_eq!(group_count(&local, "artists", "Tester"), Some(3));
    let empty = http.json("/playlists?source=nosuchpeer").await;
    assert_eq!(empty["albums"].as_array().unwrap().len(), 0);
    assert_eq!(empty["artists"].as_array().unwrap().len(), 0);

    // --- resolve one grouping to its tracks (/playlist) ------------------
    let artist_tracks = http.json("/playlist?kind=artist&key=Tester&source=local").await;
    assert_eq!(artist_tracks["files"].as_array().unwrap().len(), 3);
    assert!(artist_tracks["files"]
        .as_array()
        .unwrap()
        .iter()
        .all(|f| f["media"]["artist"] == "Tester"));
    let album_tracks = http.json("/playlist?kind=album&key=Greatest%20Hits&source=local").await;
    assert_eq!(album_tracks["files"].as_array().unwrap().len(), 2);
    let folder_tracks = http.json("/playlist?kind=folder&key=files&source=local").await;
    assert_eq!(folder_tracks["files"].as_array().unwrap().len(), 3, "folder excludes non-media");
    // a resolved track streams
    let h = artist_tracks["files"][0]["hash"].as_str().unwrap();
    assert!(http.get_file(h, None).await.status().is_success());
    // empty source spans the library
    let any_src = http.json("/playlist?kind=artist&key=Tester&source=").await;
    assert_eq!(any_src["files"].as_array().unwrap().len(), 3);

    // --- the playlists a file belongs to (/related) ----------------------
    let files = http.files().await;
    let hash_of = |suffix: &str| -> String {
        files["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["name"].as_str().unwrap().ends_with(suffix))
            .unwrap()["hash"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let gh1 = hash_of("gh1.mp3");
    let rel = http.json(&format!("/related?hash={gh1}")).await;
    let sib = rel["files"].as_array().unwrap();
    assert_eq!(sib.len(), 2, "gh1 siblings");
    assert!(sib.iter().all(|f| f["media"]["artist"] == "Tester"));
    assert!(sib.iter().all(|f| f["hash"].as_str() != Some(&gh1)), "excludes itself");
    assert!(sib.iter().all(|f| {
        let ct = f["media"]["content_type"].as_str().unwrap_or("");
        ct.starts_with("audio") || ct.starts_with("video")
    }), "no non-media/image siblings");

    let bs1 = hash_of("bs1.mp3");
    assert_eq!(http.json(&format!("/related?hash={bs1}")).await["files"].as_array().unwrap().len(), 2);
    let notes = hash_of("notes.bin");
    assert_eq!(http.json(&format!("/related?hash={notes}")).await["files"].as_array().unwrap().len(), 0);
    assert_eq!(
        http.json(&format!("/related?hash={}", "0".repeat(64))).await["files"].as_array().unwrap().len(),
        0
    );
}
