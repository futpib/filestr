//! Port of test-reputation.sh — anti-free-riding credit limit + per-peer override.

mod common;
use common::{pseudo_bytes, write_share_file, Node, NodeOpts};

#[tokio::test]
async fn reputation_denies_free_rider_until_override() {
    // A serves; tiny credit limit, no newcomer budget, no decay during the test.
    let a = Node::start(
        "A",
        NodeOpts {
            share: true,
            extra_config:
                "[reputation]\ncredit_limit_mib = 1\nnewcomer_budget_mib = 0\nhalf_life_days = 3650\n"
                    .to_string(),
            ..Default::default()
        },
    )
    .await;
    // three ~0.7 MiB files: two fit under 1 MiB, the third tips the debt over
    for n in 1..=3 {
        write_share_file(&a, &format!("f{n}.bin"), &pseudo_bytes(700_000, n));
    }
    a.rescan().await;

    let b = Node::start("B", NodeOpts::default()).await; // pure leecher
    b.peer_add(&a.invite_create(None).await).await;
    let a_id = a.node_id().await;
    let b_id = b.node_id().await;

    let listing = b.browse(&a_id).await;
    let h = |name: &str| listing.iter().find(|f| f.path.ends_with(name)).unwrap().hash.clone();

    b.get(&h("f1.bin")).await; // under limit
    b.get(&h("f2.bin")).await; // under limit

    // third fetch refused for free-riding
    let err = b.get_opts(&h("f3.bin"), None, None).await.expect_err("f3 should be denied");
    let low = err.to_lowercase();
    assert!(
        low.contains("refused") || low.contains("credit") || low.contains("rate_limited"),
        "denial for wrong reason: {err}"
    );

    // A's ledger marks B denied, with zero received
    let rep = a.reputation().await;
    let brep = rep
        .iter()
        .find(|p| p.node_id.starts_with(&b_id[..8]))
        .expect("B should appear in A's ledger");
    assert_eq!(brep.action, "deny");
    assert_eq!(brep.received, 0, "B served nothing");

    // per-peer override raises B's limit; SIGHUP reloads; the fetch now succeeds
    a.append_config(&format!(
        "[[reputation.override]]\npeer = \"{b_id}\"\ncredit_limit_mib = 1000\n"
    ));
    a.sighup();
    common::wait_until("override applied", || async {
        b.get_opts(&h("f3.bin"), None, None).await.is_ok()
    })
    .await;
    let got = b.get_opts(&h("f3.bin"), None, None).await.unwrap();
    assert_eq!(got, pseudo_bytes(700_000, 3), "f3 content after override");
}
