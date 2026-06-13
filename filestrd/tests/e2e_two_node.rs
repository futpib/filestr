//! Port of scripts/autotests/test-two-node.sh — invite/redeem, symmetric browse,
//! search attribution, verified fetch, single-use token, re-redeem, revoke.
//! (PLAN.md gates M1–M3)

mod common;
use common::{write_share_file, Node, NodeOpts};
use libfilestr::ctl::RequestBody;

#[tokio::test]
async fn two_node_grant_browse_search_fetch_revoke() {
    let a = Node::start("A", NodeOpts { share: true, ..Default::default() }).await;
    write_share_file(&a, "hello.txt", b"hello filestr\n");
    write_share_file(&a, "sub/data.bin", &vec![7u8; 4096]);
    assert_eq!(a.rescan().await, 2, "rescan should find 2 files");

    let ticket = a.invite_create(Some("e2e")).await;
    assert!(ticket.starts_with("filestr1"), "bad ticket: {ticket}");

    let b = Node::start("B", NodeOpts { share: true, ..Default::default() }).await;
    write_share_file(&b, "from-b.txt", b"from B\n");
    b.rescan().await;
    b.peer_add(&ticket).await;

    let a_id = a.node_id().await;
    let b_id = b.node_id().await;

    // tickets are symmetric: redeeming A's invite also lets A browse B
    let a_sees_b = a.browse(&b_id).await;
    assert!(
        a_sees_b.iter().any(|f| f.path.ends_with("from-b.txt")),
        "symmetric: A should see B's file, got {a_sees_b:?}"
    );

    // browse: B sees A's two files
    let listing = b.browse(&a_id).await;
    assert_eq!(listing.len(), 2, "expected 2 entries, got {}", listing.len());
    let hello = listing.iter().find(|f| f.path.ends_with("hello.txt")).expect("hello.txt");
    let data = listing.iter().find(|f| f.path.ends_with("data.bin")).expect("data.bin");

    // search streams from A, attributed via=A locally
    let hits = b.search("hello").await;
    let hit = hits.iter().find(|h| h.hash == hello.hash).expect("search found hello.txt");
    assert_eq!(hit.via.as_deref(), Some(a_id.as_str()), "expected via=A");

    // verified fetch of both files
    assert_eq!(b.get(&hello.hash).await, b"hello filestr\n", "hello.txt content");
    assert_eq!(b.get(&data.hash).await, vec![7u8; 4096], "data.bin content");

    // single-use: a different node can't redeem the same token
    let c = Node::start("C", NodeOpts::default()).await;
    let err = c.call_expect_err(RequestBody::PeerAdd { ticket: ticket.clone(), label: None }).await;
    let lower = err.to_lowercase();
    assert!(
        lower.contains("denied") || lower.contains("refused"),
        "token reuse should be denied, got: {err}"
    );

    // same-node re-redeem is fine (lost-response recovery)
    b.peer_add(&ticket).await;

    // revoke: B loses access to A
    a.peer_revoke(&b_id).await;
    let err = b.call_expect_err(RequestBody::Browse { peer: a_id.clone() }).await;
    assert!(!err.is_empty(), "browse after revoke should error");
}
