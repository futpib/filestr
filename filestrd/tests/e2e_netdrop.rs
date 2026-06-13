//! Port of scripts/autotests/test-http-netdrop.sh — when the source peer is
//! unreachable, the gateway fails fast with a RETRYABLE 503 (never a truncated
//! 2xx, never a fatal 404), and the same request succeeds once the peer returns.
//! "Network out/in" = SIGSTOP/SIGCONT on the provider (address stays valid).

mod common;
use std::time::Instant;

use common::{write_share_file, Node, NodeOpts};

#[tokio::test]
async fn gateway_fails_fast_and_recovers_across_a_network_drop() {
    let a = Node::start("A", NodeOpts { share: true, ..Default::default() }).await;
    let movie: Vec<u8> = (0..9_437_184u32).map(|i| (i % 251) as u8).collect(); // 9 MiB
    write_share_file(&a, "movie.bin", &movie);
    a.rescan().await;

    // gateway node G, short connect/io timeouts so a drop fails fast
    let g = Node::start(
        "G",
        NodeOpts {
            http_port: Some(39141),
            extra_config:
                "[search]\nconnect_timeout_secs = 2\nbrowse_timeout_secs = 2\nio_timeout_secs = 3\n"
                    .to_string(),
            ..Default::default()
        },
    )
    .await;
    g.peer_add(&a.invite_create(None).await).await;

    let http = g.http();
    http.wait_ready().await;
    let files = http.files().await;
    let hash = files["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"].as_str().unwrap_or("").ends_with("movie.bin"))
        .and_then(|f| f["hash"].as_str())
        .expect("movie.bin listed")
        .to_string();

    // An unknown (valid-format) hash with no source -> retryable 503, not 404.
    let unknown = "0".repeat(64);
    assert_eq!(http.get_file(&unknown, None).await.status(), 503, "unknown hash should be 503");

    // baseline ranged GET works while A is up
    let r = http.get_file(&hash, Some("100000-100099")).await;
    assert_eq!(r.status(), 206, "baseline range");

    // --- network OUT ---
    a.pause();
    let t = Instant::now();
    let during = http.get_file(&hash, Some("200000-200099")).await;
    let elapsed = t.elapsed();
    assert_eq!(during.status(), 503, "during outage should be a retryable 503");
    assert!(elapsed.as_secs() <= 8, "should fail fast, took {elapsed:?}");

    // --- network BACK ---
    a.resume();

    // the SAME request now succeeds with correct bytes (no cached brokenness)
    let mut recovered = None;
    for _ in 0..4 {
        let r = http.get_file(&hash, Some("200000-200099")).await;
        if r.status() == 206 {
            let bytes = r.bytes().await.unwrap();
            if bytes.len() == 100 {
                recovered = Some(bytes);
                break;
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
    let bytes = recovered.expect("request should recover after the peer returns");
    assert_eq!(&bytes[..], &movie[200000..200100], "recovered bytes match");

    // and a full GET still reassembles correctly after the ordeal
    let full = http.get_file(&hash, None).await;
    assert!(full.status().is_success());
    assert_eq!(full.bytes().await.unwrap().as_ref(), movie.as_slice(), "full reassembly");
}
