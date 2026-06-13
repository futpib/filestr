//! Ports of test-browse-self, test-share-add, test-scan-control, test-persistence-rehash.

mod common;
use common::{pseudo_bytes, wait_until, write_share_file, Node, NodeOpts};

/// test-browse-self.sh — `browse` with no peer lists this node's own files.
#[tokio::test]
async fn browse_self_lists_own_shares() {
    let a = Node::start("A", NodeOpts { share: true, ..Default::default() }).await;
    for (i, n) in ["alpha", "bravo", "charlie"].iter().enumerate() {
        write_share_file(&a, &format!("{n}.bin"), &pseudo_bytes(4096, i as u64 + 1));
    }
    a.rescan().await;
    a.wait_share_files(3).await;

    let own = a.browse_self().await;
    assert_eq!(own.len(), 3, "browse-self count");
    for n in ["alpha", "bravo", "charlie"] {
        assert!(own.iter().any(|f| f.path == format!("files/{n}.bin")), "missing {n}");
    }
    assert!(
        own.iter().all(|f| f.hash.chars().all(|c| c.is_ascii_hexdigit())),
        "every entry carries a hex hash"
    );

    // browsing a bogus peer still errors
    let err = a.call_expect_err(libfilestr::ctl::RequestBody::Browse { peer: "nonesuch".into() }).await;
    assert!(!err.is_empty());
}

/// test-share-add.sh — add/list/remove share roots from the control protocol.
#[tokio::test]
async fn share_add_list_remove() {
    let a = Node::start("A", NodeOpts::default()).await;
    let media = a.dir().join("media");
    std::fs::create_dir_all(&media).unwrap();
    std::fs::write(media.join("song.bin"), pseudo_bytes(1024, 1)).unwrap();

    assert_eq!(a.shares().await.len(), 0, "starts with no shares");

    a.share_add(&media, None).await;
    assert!(a.shares().await.iter().any(|s| s.name == "media"), "media registered");
    wait_until("media indexed", || async {
        a.shares().await.iter().find(|s| s.name == "media").map(|s| s.files) == Some(1)
    })
    .await;
    assert!(
        std::fs::read_to_string(a.config_path()).unwrap().contains("name = \"media\""),
        "share persisted to config"
    );

    // a second dir with an explicit name
    let docs = a.dir().join("docs");
    std::fs::create_dir_all(&docs).unwrap();
    std::fs::write(docs.join("readme.txt"), b"hello\n").unwrap();
    a.share_add(&docs, Some("documents")).await;
    assert_eq!(a.shares().await.len(), 2);
    wait_until("documents indexed", || async {
        a.shares().await.iter().find(|s| s.name == "documents").map(|s| s.files) == Some(1)
    })
    .await;

    // duplicate name fails
    assert!(!a.share_add_expect_err(&media, None).await.is_empty(), "duplicate name should fail");

    // incremental rescan picks up a grown file
    let before = a.shares().await.iter().find(|s| s.name == "documents").unwrap().bytes;
    std::fs::write(docs.join("readme.txt"), pseudo_bytes(4096, 9)).unwrap();
    a.rescan().await;
    let after = a.shares().await.iter().find(|s| s.name == "documents").unwrap().bytes;
    assert!(after > before, "rescan missed a changed file ({before} -> {after})");

    // remove one; gone from listing and config; the other untouched
    a.share_remove("media").await;
    assert!(a.shares().await.iter().all(|s| s.name != "media"));
    assert!(!std::fs::read_to_string(a.config_path()).unwrap().contains("name = \"media\""));
    assert!(a.shares().await.iter().any(|s| s.name == "documents"));

    // removing a non-existent share fails
    assert!(
        !a.call_expect_err(libfilestr::ctl::RequestBody::ShareRemove { name: "nope".into() })
            .await
            .is_empty()
    );
}

/// test-scan-control.sh — the background scan is pausable/resumable/cancellable
/// and serves files incrementally as they hash.
#[tokio::test]
async fn scan_pause_resume_cancel() {
    const N: usize = 1500;
    let a = Node::start("A", NodeOpts::default()).await;
    let lib = a.dir().join("lib");
    std::fs::create_dir_all(&lib).unwrap();
    let payload = pseudo_bytes(4096, 1);
    for i in 0..N {
        std::fs::write(lib.join(format!("f{i}.bin")), &payload).unwrap();
    }

    a.share_add(&lib, None).await; // returns immediately; hashing is background
    a.scan_pause().await;

    // status stays coherent (done <= total) whenever a scan is reported
    for _ in 0..40 {
        if let Some(p) = a.status().await.indexing {
            assert!(p.done <= p.total, "indexing.done {} > total {}", p.done, p.total);
        } else {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    // resume and finish: every file ends up served
    a.scan_resume().await;
    a.wait_share_files(N).await;
    assert_eq!(a.status().await.files, N);

    // cancel/pause/resume are clean no-ops when idle
    a.scan_cancel().await;
    a.scan_pause().await;
    a.scan_resume().await;
}

/// test-persistence-rehash.sh — the index cache lets a restart reuse unchanged
/// files; a changed file is re-hashed.
#[tokio::test]
async fn restart_reuses_index_cache() {
    let mut a = Node::start("A", NodeOpts { share: true, ..Default::default() }).await;
    write_share_file(&a, "a.bin", &pseudo_bytes(4 * 1024 * 1024, 1));
    write_share_file(&a, "b.bin", &pseudo_bytes(1024 * 1024, 2));
    a.rescan().await;

    a.restart().await;
    let line = last_scan_line(&a.log());
    assert!(line.contains("reused=2"), "restart did not reuse cache: {line}");
    assert!(line.contains("hashed=0"), "restart re-hashed despite cache: {line}");

    // change one file -> it is re-hashed, the other reused
    write_share_file(&a, "a.bin", &pseudo_bytes(2 * 1024 * 1024, 3));
    a.rescan().await;
    let line2 = last_scan_line(&a.log());
    assert!(line2.contains("reused=1"), "changed-file rescan should reuse the other: {line2}");
    assert!(line2.contains("hashed=1"), "changed file should be re-hashed: {line2}");
}

fn last_scan_line(log: &str) -> String {
    log.lines()
        .filter(|l| l.contains("share scan complete"))
        .next_back()
        .unwrap_or("")
        .to_string()
}
