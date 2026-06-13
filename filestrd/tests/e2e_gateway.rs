//! Ports of the data-plane HTTP gateway bash tests: test-http-gateway,
//! test-http-search, test-http-replay, test-http-stream-peer, test-streaming-ranges.

mod common;
use common::{dir_size, pseudo_bytes, write_share_file, Node, NodeOpts};

fn hash_of<'a>(files: &'a serde_json::Value, suffix: &str) -> Option<&'a str> {
    files["files"].as_array()?.iter().find_map(|f| {
        let name = f["name"].as_str()?;
        name.ends_with(suffix).then(|| f["hash"].as_str()).flatten()
    })
}

/// test-http-gateway.sh — list/HEAD/GET/Range/conditional/If-Range/content-type/plugin.
#[tokio::test]
async fn gateway_list_head_range_conditional_and_plugin() {
    let a = Node::start("A", NodeOpts { share: true, http_port: Some(39150), ..Default::default() })
        .await;
    let clip = pseudo_bytes(262144, 1);
    write_share_file(&a, "clip.bin", &clip);
    write_share_file(&a, "hello.txt", b"0123456789abcdef");
    a.rescan().await;

    let http = a.http();
    http.wait_ready().await;

    let files = http.files().await;
    let hash = hash_of(&files, "clip.bin").expect("clip.bin listed").to_string();
    let size = files["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"].as_str().unwrap().ends_with("clip.bin"))
        .unwrap()["size"]
        .as_u64()
        .unwrap();
    assert_eq!(size, 262144);

    // HEAD: size/etag/accept-ranges, no body
    let head = http.head(&format!("/file/{hash}")).await;
    let h = head.headers();
    assert_eq!(h.get("content-length").unwrap(), "262144");
    assert_eq!(h.get("etag").unwrap(), &format!("\"{hash}\""));
    assert_eq!(h.get("accept-ranges").unwrap(), "bytes");

    // full GET matches
    let body = http.get_file(&hash, None).await.bytes().await.unwrap();
    assert_eq!(body.as_ref(), clip.as_slice(), "full GET bytes");

    // range 0-99
    let r = http.get_file(&hash, Some("0-99")).await;
    assert_eq!(r.status(), 206);
    let rb = r.bytes().await.unwrap();
    assert_eq!(rb.len(), 100);
    assert_eq!(rb.as_ref(), &clip[..100]);

    // If-None-Match -> 304
    let etag = format!("\"{hash}\"");
    let cond = http.get_file_headers(&hash, &[("If-None-Match", &etag)]).await;
    assert_eq!(cond.status(), 304);

    // If-Range match -> 206; mismatch -> 200
    let irm = http.get_file_headers(&hash, &[("Range", "bytes=0-99"), ("If-Range", &etag)]).await;
    assert_eq!(irm.status(), 206, "If-Range match");
    let irx =
        http.get_file_headers(&hash, &[("Range", "bytes=0-99"), ("If-Range", "\"deadbeef\"")]).await;
    assert_eq!(irx.status(), 200, "If-Range mismatch");

    // content-type inferred from ?name=
    let ct = http.get(&format!("/file/{hash}?name=hello.txt")).await;
    assert!(
        ct.headers().get("content-type").unwrap().to_str().unwrap().starts_with("text/plain"),
        "content-type by name"
    );

    // grayjay plugin served, URLs rewritten
    let cfg = http.grayjay_config().await;
    assert!(cfg["scriptUrl"].as_str().unwrap().contains("127.0.0.1:39150"));
    assert!(http.get("/grayjay/FilestrScript.js").await.status().is_success());

    // daemon log is plain (no ANSI escapes)
    assert!(!a.log().contains('\u{1b}'), "daemon log has ANSI escapes");
}

/// test-http-search.sh — federated /search reaches a two-hop file /files can't.
#[tokio::test]
async fn gateway_federated_search_two_hops() {
    let a = Node::start("A", NodeOpts { share: true, ..Default::default() }).await;
    write_share_file(&a, "zforatest-movie.bin", &pseudo_bytes(262144, 2));
    a.rescan().await;

    let b = Node::start("B", NodeOpts::default()).await;
    b.peer_add(&a.invite_create(None).await).await;

    let g = Node::start("G", NodeOpts { http_port: Some(39151), ..Default::default() }).await;
    g.peer_add(&b.invite_create(None).await).await;

    let http = g.http();
    http.wait_ready().await;

    // /files is one hop — must not reach A's file
    let files = http.files().await;
    assert!(
        !files["files"].as_array().unwrap().iter().any(|f| f["name"]
            .as_str()
            .unwrap_or("")
            .contains("zforatest")),
        "/files should not reach a two-hop file"
    );

    // /search forwards across the graph
    let search = http.search("zforatest").await;
    let hit = search["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"].as_str().unwrap_or("").contains("zforatest"))
        .expect("federated search found the two-hop file");
    assert_ne!(hit["source"].as_str(), Some("local"));
    let hash = hit["hash"].as_str().unwrap().to_string();

    // streamable through the gateway (relayed via B)
    let got = http.get_file(&hash, None).await.bytes().await.unwrap();
    assert_eq!(got.as_ref(), pseudo_bytes(262144, 2).as_slice());

    // empty query returns nothing
    assert_eq!(http.search("").await["files"].as_array().unwrap().len(), 0);
}

/// test-http-replay.sh — a fetched range is reused after the peer dies; an
/// un-fetched range fails (now a fast 503).
#[tokio::test]
async fn gateway_partial_blob_reuse_after_peer_dies() {
    let a = Node::start("A", NodeOpts { share: true, ..Default::default() }).await;
    let movie = pseudo_bytes(8 * 1024 * 1024, 3);
    write_share_file(&a, "movie.bin", &movie);
    a.rescan().await;

    let g = Node::start(
        "G",
        NodeOpts {
            http_port: Some(39152),
            extra_config: "[search]\nconnect_timeout_secs = 2\nio_timeout_secs = 3\n".to_string(),
            ..Default::default()
        },
    )
    .await;
    g.peer_add(&a.invite_create(None).await).await;
    let http = g.http();
    http.wait_ready().await;
    let hash = hash_of(&http.files().await, "movie.bin").unwrap().to_string();

    // fetch a small range -> lands in the local partial blob
    let r1 = http.get_file(&hash, Some("1000000-1000099")).await;
    assert_eq!(r1.status(), 206);
    assert_eq!(r1.bytes().await.unwrap().as_ref(), &movie[1000000..1000100]);

    // kill the provider; only locally-present ranges can be served now
    a.kill();
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;

    // the already-fetched range still serves (reused, no peer needed)
    let r1b = http.get_file(&hash, Some("1000000-1000099")).await;
    assert_eq!(r1b.status(), 206, "fetched range reused after peer died");
    assert_eq!(r1b.bytes().await.unwrap().as_ref(), &movie[1000000..1000100]);

    // a different, un-fetched range cannot be served (peer gone) -> retryable 503
    let r2 = http.get_file(&hash, Some("6000000-6000099")).await;
    assert!(!r2.status().is_success(), "un-fetched range should fail with the peer dead");
    assert!(r2.bytes().await.unwrap().len() < 100, "must not serve the un-fetched bytes");
}

/// test-http-stream-peer.sh — ranged streaming from a peer without over-fetching.
#[tokio::test]
async fn gateway_streams_peer_file_by_range_without_overfetch() {
    let a = Node::start("A", NodeOpts { share: true, ..Default::default() }).await;
    let movie = pseudo_bytes(9_437_184, 4); // 9 MiB
    write_share_file(&a, "movie.bin", &movie);
    a.rescan().await;

    let g = Node::start("G", NodeOpts { http_port: Some(39153), ..Default::default() }).await;
    g.peer_add(&a.invite_create(None).await).await;
    let http = g.http();
    http.wait_ready().await;

    let files = http.files().await;
    let entry = files["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"].as_str().unwrap().ends_with("movie.bin"))
        .unwrap();
    assert_ne!(entry["source"].as_str(), Some("local"), "movie should be peer-sourced");
    let hash = entry["hash"].as_str().unwrap().to_string();

    // HEAD: size known from the browse, no bytes fetched
    let head = http.head(&format!("/file/{hash}")).await;
    assert_eq!(head.headers().get("content-length").unwrap(), "9437184");
    assert!(dir_size(&g.dir().join("data")) < 2 * 1024 * 1024, "HEAD fetched data");

    // middle range: correct slice, no whole-file pull
    let mid = http.get_file(&hash, Some("5000000-5000099")).await;
    assert_eq!(mid.status(), 206);
    assert_eq!(mid.bytes().await.unwrap().as_ref(), &movie[5000000..5000100]);
    assert!(dir_size(&g.dir().join("data")) < 2 * 1024 * 1024, "ranged GET over-fetched");

    // open-ended range to EOF reassembles
    let tail = http.get_file(&hash, Some("4000000-")).await;
    assert_eq!(tail.status(), 206);
    assert_eq!(tail.bytes().await.unwrap().as_ref(), &movie[4000000..]);

    // full GET reassembles byte-identical
    let full = http.get_file(&hash, None).await.bytes().await.unwrap();
    assert_eq!(full.as_ref(), movie.as_slice());
}

/// test-streaming-ranges.sh — relay does NOT cache; ranged + background fetches.
#[tokio::test]
async fn relay_does_not_cache_and_ranged_background_fetch() {
    let a = Node::start("A", NodeOpts { share: true, ..Default::default() }).await;
    let big = pseudo_bytes(1024 * 1024, 5);
    write_share_file(&a, "big.bin", &big);
    write_share_file(&a, "ranged.txt", b"0123456789abcdef\n");
    a.rescan().await;

    let b = Node::start("B", NodeOpts::default()).await;
    b.peer_add(&a.invite_create(None).await).await;
    let c = Node::start("C", NodeOpts::default()).await;
    c.peer_add(&b.invite_create(None).await).await;

    let a_id = a.node_id().await;

    // C finds big.bin via the relay and fetches it
    let hits = c.search("big").await;
    let big_hash = hits.iter().find(|h| h.name.ends_with("big.bin")).expect("big.bin via relay").hash.clone();
    assert_eq!(c.get(&big_hash).await, big, "relayed big.bin");

    // B (the relay) must not have cached the 1 MiB blob
    assert!(dir_size(&b.dir().join("data")) < 512 * 1024, "relay cached forwarded data");

    // ranged fetch 3..=8 of ranged.txt directly from A == "345678"
    c.peer_add(&a.invite_create(None).await).await;
    let listing = c.browse(&a_id).await;
    let ranged_hash = listing.iter().find(|f| f.path.ends_with("ranged.txt")).unwrap().hash.clone();
    let slice = c.get_opts(&ranged_hash, Some("3-8"), Some(&a_id)).await.unwrap();
    assert_eq!(&slice, b"345678", "ranged slice");

    // background downloads complete
    let big_out = c.dir().join("bg-big.bin");
    c.get_background(&big_hash, Some(&a_id), &big_out).await;
    c.get_background(&ranged_hash, Some(&a_id), &c.dir().join("bg-ranged.txt")).await;
    common::wait_until("background transfers drain", || async {
        c.transfers().await.iter().all(|t| t.status != "queued" && t.status != "active")
    })
    .await;
    let done = c.transfers().await.iter().filter(|t| t.status == "done").count();
    assert!(done >= 2, "expected >=2 completed bg transfers, got {done}");
    assert_eq!(std::fs::read(&big_out).unwrap(), big, "bg big.bin");
}
