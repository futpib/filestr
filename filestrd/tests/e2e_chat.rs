//! Ports of the chat-plane bash tests: test-hub-chat, test-hub-request,
//! test-hub-address, test-persistence, test-no-nostr, test-relays. These use the
//! Marmot/MLS hub + nostr machinery (default `chat` feature stays on).

mod common;
use common::{wait_until, write_share_file, Node, NodeOpts};

fn has_msg(log: &[libfilestr::ctl::ChatMessage], content: &str) -> bool {
    log.iter().any(|m| m.content == content)
}

/// Recursively true if any file under `dir` contains `needle` (raw bytes).
fn dir_contains(dir: &std::path::Path, needle: &str) -> bool {
    let n = needle.as_bytes();
    std::fs::read_dir(dir).into_iter().flatten().flatten().any(|e| {
        let p = e.path();
        if p.is_dir() {
            dir_contains(&p, needle)
        } else {
            std::fs::read(&p).map(|b| b.windows(n.len()).any(|w| w == n)).unwrap_or(false)
        }
    })
}

/// test-hub-chat.sh — owner creates a hub, member joins via one ticket that does
/// BOTH chat and bidirectional file peering; E2EE messages flow; ticket is single-use.
#[tokio::test]
async fn hub_chat_join_message_and_share_to_join() {
    let a = Node::start("A", NodeOpts { share: true, ..Default::default() }).await;
    let b = Node::start("B", NodeOpts { share: true, ..Default::default() }).await;
    write_share_file(&a, "owner.txt", b"owner-only file\n");
    write_share_file(&b, "secret.txt", b"members-only file\n");
    a.rescan().await;
    b.rescan().await;

    let hub = a.hub_create("general").await;
    let ticket = a.hub_invite(&hub).await;
    assert!(ticket.starts_with("filestrhub1"), "bad hub ticket: {ticket}");

    assert!(!b.hub_join(&ticket).await, "join should not be queued (chat is on)");
    wait_until("owner sees 2 members", || async { a.hub_members(&hub).await.len() == 2 }).await;

    // owner -> member
    a.hub_send(&hub, "hello hub from owner").await;
    wait_until("member gets owner msg", || async {
        has_msg(&b.hub_log(&hub).await, "hello hub from owner")
    })
    .await;
    // member -> owner
    b.hub_send(&hub, "hi back from member").await;
    wait_until("owner gets member msg", || async {
        has_msg(&a.hub_log(&hub).await, "hi back from member")
    })
    .await;

    // the same ticket peered files both ways
    let a_id = a.node_id().await;
    let b_id = b.node_id().await;
    assert!(a.browse(&b_id).await.iter().any(|f| f.path.ends_with("secret.txt")), "owner sees member file");
    assert!(b.browse(&a_id).await.iter().any(|f| f.path.ends_with("owner.txt")), "member sees owner file");

    // single-use: a different node can't reuse it; membership unchanged
    let c = Node::start("C", NodeOpts::default()).await;
    assert!(!c.hub_join_expect_err(&ticket).await.is_empty(), "reuse should fail");
    assert_eq!(a.hub_members(&hub).await.len(), 2, "reuse leaked a member");

    // a fresh ticket lets C join
    let ticket2 = a.hub_invite(&hub).await;
    assert!(!c.hub_join(&ticket2).await);
}

/// test-hub-request.sh — member-initiated join via a filestrreq1… ticket the
/// owner admits; chat + bidirectional peering; single-use.
#[tokio::test]
async fn hub_request_admit_and_peering() {
    let a = Node::start("A", NodeOpts { share: true, ..Default::default() }).await;
    let b = Node::start("B", NodeOpts { share: true, ..Default::default() }).await;
    write_share_file(&a, "owner.txt", b"owner-only file\n");
    write_share_file(&b, "secret.txt", b"members-only file\n");
    a.rescan().await;
    b.rescan().await;

    let hub = a.hub_create("open-hub").await;
    let req = b.hub_request(None, Some("hi I'm B")).await;
    assert!(req.starts_with("filestrreq1"), "bad request ticket: {req}");

    a.hub_admit(&req).await;
    wait_until("owner sees 2 members", || async { a.hub_members(&hub).await.len() == 2 }).await;

    a.hub_send(&hub, "welcome aboard").await;
    wait_until("requester gets msg", || async { has_msg(&b.hub_log(&hub).await, "welcome aboard") }).await;
    b.hub_send(&hub, "thanks for having me").await;
    wait_until("owner gets reply", || async {
        has_msg(&a.hub_log(&hub).await, "thanks for having me")
    })
    .await;

    let a_id = a.node_id().await;
    let b_id = b.node_id().await;
    assert!(a.browse(&b_id).await.iter().any(|f| f.path.ends_with("secret.txt")));
    assert!(b.browse(&a_id).await.iter().any(|f| f.path.ends_with("owner.txt")));

    // request ticket is single-use
    assert!(!a.hub_admit_expect_err(&req).await.is_empty(), "re-admit should fail");
}

/// test-hub-address.sh — join over nostr from a compact address (gift-wrapped
/// request over a relay), owner auto-admits; the request never hits disk in plaintext.
#[tokio::test]
async fn hub_address_join_over_nostr_relay() {
    let port = 39160;
    let a = Node::start(
        "A",
        NodeOpts {
            share: true,
            extra_config: format!(
                "[chat]\nrelay_listen = \"127.0.0.1:{port}\"\nrelays = [\"ws://127.0.0.1:{port}\"]\nauto_admit = true\n"
            ),
            ..Default::default()
        },
    )
    .await;
    let b = Node::start(
        "B",
        NodeOpts {
            share: true,
            extra_config: format!("[chat]\nrelays = [\"ws://127.0.0.1:{port}\"]\n"),
            ..Default::default()
        },
    )
    .await;

    let hub = a.hub_create("privatehub").await;
    let addr = a.hub_address(&hub).await;
    assert!(addr.starts_with("filestraddr1"), "bad hub address: {addr}");

    b.hub_request(Some(&addr), None).await; // gift-wrapped DM over the relay

    wait_until("B auto-admitted", || async {
        b.hub_ls().await.iter().any(|h| h.group_ref == hub)
    })
    .await;
    assert_eq!(a.hub_members(&hub).await.len(), 2);

    a.hub_send(&hub, "auto-admitted hello").await;
    wait_until("member reads chat", || async {
        has_msg(&b.hub_log(&hub).await, "auto-admitted hello")
    })
    .await;

    // the request crossed the relay gift-wrapped: no plaintext ticket on disk
    assert!(!dir_contains(&a.dir().join("data"), "filestrreq1"), "plaintext request ticket leaked");
}

/// test-persistence.sh — hub registry, membership and decrypted history survive a
/// cold restart; the MLS db is encrypted at rest.
#[tokio::test]
async fn hub_state_persists_across_restart() {
    let mut a = Node::start("A", NodeOpts { share: true, ..Default::default() }).await;
    let mut b = Node::start("B", NodeOpts { share: true, ..Default::default() }).await;

    let hub = a.hub_create("persists").await;
    b.hub_join(&a.hub_invite(&hub).await).await;
    wait_until("joined", || async { a.hub_members(&hub).await.len() == 2 }).await;

    a.hub_send(&hub, "before restart (owner)").await;
    b.hub_send(&hub, "before restart (member)").await;
    // sync both ways before restarting
    wait_until("owner has both", || async {
        let l = a.hub_log(&hub).await;
        has_msg(&l, "before restart (owner)") && has_msg(&l, "before restart (member)")
    })
    .await;
    wait_until("member has both", || async {
        let l = b.hub_log(&hub).await;
        has_msg(&l, "before restart (owner)") && has_msg(&l, "before restart (member)")
    })
    .await;

    a.restart().await;
    b.restart().await;

    // owner: hub, membership, full history survived
    assert!(a.hub_ls().await.iter().any(|h| h.group_ref == hub), "owner lost hub");
    assert_eq!(a.hub_members(&hub).await.len(), 2, "owner lost membership");
    let alog = a.hub_log(&hub).await;
    assert!(has_msg(&alog, "before restart (owner)") && has_msg(&alog, "before restart (member)"));
    // member: same, from local store
    assert!(b.hub_ls().await.iter().any(|h| h.group_ref == hub), "member lost hub");
    let blog = b.hub_log(&hub).await;
    assert!(has_msg(&blog, "before restart (owner)") && has_msg(&blog, "before restart (member)"));

    // encrypted at rest: no plaintext message in the MLS db
    let mls = a.dir().join("data/mls.sqlite");
    if mls.exists() {
        let bytes = std::fs::read(&mls).unwrap();
        assert!(
            !bytes.windows(14).any(|w| w == b"before restart"),
            "MLS db contains plaintext"
        );
    }
}

/// test-no-nostr.sh — file peering works with chat off; hub commands are refused;
/// a hub ticket queues the join, which completes once chat is enabled + restarted.
#[tokio::test]
async fn chat_disabled_peers_files_and_queues_hub_join() {
    let a = Node::start("A", NodeOpts { share: true, ..Default::default() }).await;
    write_share_file(&a, "a.txt", b"shared by A\n");
    a.rescan().await;
    let hub = a.hub_create("lobby").await;
    let hub_ticket = a.hub_invite(&hub).await;

    let mut b = Node::start(
        "B",
        NodeOpts { share: true, extra_config: "[chat]\nenabled = false\n".to_string(), ..Default::default() },
    )
    .await;

    // 1. plain file peering works with chat off
    b.peer_add(&a.invite_create(None).await).await;
    let a_id = a.node_id().await;
    let listing = b.browse(&a_id).await;
    let hash = listing.iter().find(|f| f.path.ends_with("a.txt")).unwrap().hash.clone();
    assert_eq!(b.get(&hash).await, b"shared by A\n");

    // 2. hub commands refused while chat is off
    assert!(b.hub_ls_expect_err().await.to_lowercase().contains("disabled"));

    // 3. a hub ticket redeemed with chat off peers files now and QUEUES the join
    assert!(b.hub_join(&hub_ticket).await, "hub join should report queued with chat off");

    // 4. enable chat + restart -> the queued join completes on its own
    let cfg = std::fs::read_to_string(b.config_path()).unwrap().replace("enabled = false", "enabled = true");
    b.rewrite_config(&cfg);
    b.restart().await;
    wait_until("queued join completes", || async {
        b.hub_ls().await.iter().any(|h| h.group_ref == hub)
    })
    .await;

    // 5. chat now works for the formerly-queued member
    assert_eq!(a.hub_members(&hub).await.len(), 2);
    a.hub_send(&hub, "welcome, queued joiner").await;
    wait_until("queued member reads chat", || async {
        has_msg(&b.hub_log(&hub).await, "welcome, queued joiner")
    })
    .await;
}

/// test-relays.sh — a custom iroh relay URL is accepted, and hub chat flows over
/// an external nostr relay (one node's own websocket listener).
#[tokio::test]
async fn custom_iroh_relay_and_external_nostr_relay() {
    // (1) a node configured with a custom iroh relay URL still starts
    let _r = Node::start(
        "R",
        NodeOpts { extra_config: "relay_urls = [\"https://relay.example./\"]\n".to_string(), ..Default::default() },
    )
    .await;

    // (2) hub chat over an external nostr relay; embedded iroh relay off on the host
    let port = 39170;
    let a = Node::start(
        "A",
        NodeOpts {
            share: true,
            extra_config: format!("[chat]\nembedded_relay = false\nrelay_listen = \"127.0.0.1:{port}\"\n"),
            ..Default::default()
        },
    )
    .await;
    let b = Node::start(
        "B",
        NodeOpts {
            share: true,
            extra_config: format!("[chat]\nrelays = [\"ws://127.0.0.1:{port}\"]\n"),
            ..Default::default()
        },
    )
    .await;

    let hub = a.hub_create("relayed").await;
    b.hub_join(&a.hub_invite(&hub).await).await;

    a.hub_send(&hub, "over the external relay").await;
    wait_until("B via external relay", || async {
        has_msg(&b.hub_log(&hub).await, "over the external relay")
    })
    .await;
    b.hub_send(&hub, "reply via relay").await;
    wait_until("A via external relay", || async {
        has_msg(&a.hub_log(&hub).await, "reply via relay")
    })
    .await;
}
