//! Federated search must propagate media metadata across hops. Regression for
//! the "-12:-55" duration bug: a two-hop file (C finds A's track via B) used to
//! arrive with `media: null`, so the player guessed the duration from the MP3
//! frames and mis-read it. The p2p `Hit` now embeds the full `FileEntry`, so a
//! hit carries its duration/tags no matter how many hops away the origin is.

mod common;
use common::{ffmpeg, have_ffmpeg, Node, NodeOpts};

/// A-B-C: A shares a tagged 2s MP3; C searches and the multi-hop hit (via B)
/// carries the artist tag and a ~2s duration, not an empty media record.
#[tokio::test]
async fn federated_search_carries_media_across_hops() {
    if !have_ffmpeg() {
        eprintln!("SKIP (ffmpeg not installed)");
        return;
    }
    let a = Node::start("A", NodeOpts { share: true, ..Default::default() }).await;
    ffmpeg(&[
        "-f", "lavfi", "-i", "sine=frequency=440:duration=2",
        "-metadata", "title=Cathedral", "-metadata", "artist=Curtis Yarvin",
        "-metadata", "album=Gray Mirror", "-write_xing", "1",
        a.share_dir().join("rant.mp3").to_str().unwrap(),
    ]);
    a.rescan().await;

    let b = Node::start("B", NodeOpts::default()).await;
    b.peer_add(&a.invite_create(None).await).await;
    let c = Node::start("C", NodeOpts::default()).await;
    c.peer_add(&b.invite_create(None).await).await;

    let b_id = b.node_id().await;

    // C's search reaches A two hops out, via B
    let hits = c.search("Yarvin").await;
    assert!(!hits.is_empty(), "no multi-hop hits (tag search across graph)");
    let hit = hits.iter().find(|h| h.name.ends_with("rant.mp3")).expect("rant.mp3 missing");
    assert_eq!(hit.via.as_deref(), Some(b_id.as_str()), "hit should arrive via B");

    // the metadata survived both hops — the crux of the regression
    assert_eq!(
        hit.media.artist.as_deref(),
        Some("Curtis Yarvin"),
        "artist tag dropped on the federated hit"
    );
    assert_eq!(hit.media.title.as_deref(), Some("Cathedral"), "title tag dropped");
    let dur = hit.media.duration_secs.expect("duration dropped on the federated hit");
    assert!((1.8..2.3).contains(&dur), "federated duration {dur} (want ~2.0)");

    // the file's mtime also survives both hops, so a consumer can show a real
    // date / stable sort instead of "just now"
    assert!(hit.mtime > 1_600_000_000, "mtime not propagated on the federated hit: {}", hit.mtime);
}
