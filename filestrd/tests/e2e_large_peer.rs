//! Port of scripts/autotests/test-http-large-peer.sh — a peer can share more
//! files than the recent_sources LRU (4096) holds; the gateway must still resolve
//! and stream EVERY browsable file (guards the browse_sources full-listing map).

mod common;
use common::{wait_until, Node, NodeOpts};

#[tokio::test]
async fn every_file_of_a_large_peer_streams() {
    const N: usize = 5000; // > the 4096-entry LRU

    let a = Node::start("A", NodeOpts { share: true, ..Default::default() }).await;
    for i in 0..N {
        std::fs::write(
            a.share_dir().join(format!("f{i}.txt")),
            format!("filestr large-peer test file number {i}\n"),
        )
        .unwrap();
    }
    a.rescan().await;
    a.wait_share_files(N).await;

    let g = Node::start("G", NodeOpts { http_port: Some(39142), ..Default::default() }).await;
    g.peer_add(&a.invite_create(None).await).await;

    let http = g.http();
    http.wait_ready().await;

    // one browse must list all N peer files (populating browse_sources)
    wait_until("all peer files listed", || async {
        let files = http.files().await;
        files["files"].as_array().map(|a| a.len()).unwrap_or(0) >= N
    })
    .await;

    let files = http.files().await;
    let hashes: Vec<String> = files["files"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["source"].as_str() != Some("local"))
        .filter_map(|f| f["hash"].as_str().map(String::from))
        .collect();
    assert!(hashes.len() >= N, "gateway listed {} of {N} peer files", hashes.len());

    // every 50th hash (~100) must stream — with only the LRU, ~18% would 503
    let sample: Vec<&String> = hashes.iter().step_by(50).collect();
    let mut fails = 0;
    for h in &sample {
        let status = http.get_file(h, Some("0-9")).await.status();
        if !status.is_success() {
            fails += 1;
            if fails <= 5 {
                eprintln!("  FAIL {h} -> {status}");
            }
        }
    }
    assert_eq!(fails, 0, "{fails}/{} sampled files did not stream", sample.len());

    // the first-listed file (most likely evicted by a naive LRU) streams whole
    let first = &hashes[0];
    let body = http.get_file(first, None).await.bytes().await.unwrap();
    assert!(!body.is_empty(), "first peer file streamed empty");
}
