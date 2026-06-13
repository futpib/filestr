//! Ports of test-reshare-chain and test-no-reshare.

mod common;
use common::{pseudo_bytes, write_share_file, Node, NodeOpts};

/// test-reshare-chain.sh — A-B-C: C finds A's file via B with no A attribution,
/// relayed fetch verifies, and a closed cycle still terminates.
#[tokio::test]
async fn reshare_chain_attribution_free_and_cycle_terminates() {
    let a = Node::start("A", NodeOpts { share: true, ..Default::default() }).await;
    let song = pseudo_bytes(8192, 1);
    write_share_file(&a, "secret-song.mp3", &song);
    a.rescan().await;

    let b = Node::start("B", NodeOpts::default()).await;
    b.peer_add(&a.invite_create(None).await).await;
    let c = Node::start("C", NodeOpts::default()).await;
    c.peer_add(&b.invite_create(None).await).await;

    let a_id = a.node_id().await;
    let b_id = b.node_id().await;

    // C searches: the hit comes via B, carries a handle, leaks no trace of A
    let hits = c.search("song").await;
    assert!(!hits.is_empty(), "no hits");
    let hit = &hits[0];
    assert_eq!(hit.via.as_deref(), Some(b_id.as_str()), "hit should be via B");
    assert!(!hit.handle.is_empty(), "hit missing handle");
    assert!(
        hits.iter().all(|h| h.via.as_deref() != Some(a_id.as_str()) && !h.handle.contains(&a_id)),
        "search results leak origin node id"
    );
    let hash = hit.hash.clone();

    // relayed fetch: C pulls through B; bytes verify against A's original
    assert_eq!(c.get(&hash).await, song, "relayed content");

    // close the loop (C grants A) and search from A: terminates and finds local
    a.peer_add(&c.invite_create(None).await).await;
    let cyclic = a.search("song").await;
    assert!(
        cyclic.iter().any(|h| h.via.is_none() && h.hash == hash),
        "A did not find its own file in a cyclic search"
    );
}

/// test-no-reshare.sh — allow_reshare=false: B can fetch A's file but must not
/// re-serve it to C.
#[tokio::test]
async fn no_reshare_blocks_the_third_hop() {
    let a = Node::start("A", NodeOpts { share: true, ..Default::default() }).await;
    write_share_file(&a, "private.txt", b"do not reshare me\n");
    a.rescan().await;

    let b = Node::start("B", NodeOpts::default()).await;
    b.peer_add(&a.invite_create_opts(None, Some(false), None).await).await;
    let c = Node::start("C", NodeOpts::default()).await;
    c.peer_add(&b.invite_create(None).await).await;

    // B's own searches still reach A
    assert!(!b.search("private").await.is_empty(), "B should find A's file for itself");

    // C must see nothing: B honours allow_reshare=false
    assert!(c.search("private").await.is_empty(), "C found content that must not be reshared");
}
