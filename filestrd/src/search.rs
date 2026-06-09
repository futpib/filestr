//! Recursive streaming search over the grant graph, plus the helpers for
//! dialing peers and pulling remote content through relays (DESIGN.md §6, §7).

use std::net::SocketAddr;
use std::ops::Deref;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use futures_lite::StreamExt;
use iroh::{EndpointAddr, EndpointId, RelayUrl, TransportAddr, endpoint::Connection};
use iroh_blobs::api::remote::GetProgressItem;
use iroh_blobs::get::StreamPair;
use iroh_blobs::protocol::{ChunkRanges, ChunkRangesExt, GetRequest};
use libfilestr::config::VIEW_FULL;
use libfilestr::grants::PeerIn;
use libfilestr::p2p::{self, P2pRequest, P2pResponse};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;

use crate::state::State;

/// A byte range, inclusive of both ends. An end of `u64::MAX` means
/// "to end of file".
pub type ByteRange = (u64, u64);

/// Parse `START-END` (inclusive) or `START-` (open-ended) into a [`ByteRange`].
pub fn parse_range(s: &str) -> Result<ByteRange> {
    let (start, end) = s
        .split_once('-')
        .ok_or_else(|| anyhow!("range must be START-END or START-"))?;
    let start: u64 = start.trim().parse().map_err(|_| anyhow!("bad range start"))?;
    let end = match end.trim() {
        "" => u64::MAX,
        e => e.parse().map_err(|_| anyhow!("bad range end"))?,
    };
    if end < start {
        return Err(anyhow!("range end is before start"));
    }
    Ok((start, end))
}

/// An internal search hit, before per-requester handle minting.
#[derive(Debug, Clone)]
pub struct Hit {
    pub name: String,
    pub size: u64,
    pub hash: String,
    pub source: HitSource,
}

#[derive(Debug, Clone)]
pub enum HitSource {
    Local,
    /// Came from `peer`, addressed there by `handle`.
    Upstream { peer: String, handle: String },
}

pub enum Requester {
    /// The local user via filestrctl: sees everything, results carry `via`.
    Local,
    /// A granted peer: local matches scoped to their view, forwarding only
    /// if we reshare, and only across peers that allow it.
    Peer { view_roots: Vec<String> },
}

pub fn peer_addr(peer: &PeerIn) -> Result<EndpointAddr> {
    let id: EndpointId = peer
        .node_id
        .parse()
        .map_err(|e| anyhow!("bad node id {}: {e}", peer.node_id))?;
    let mut addrs: Vec<TransportAddr> = Vec::new();
    for relay in &peer.relay {
        match relay.parse::<RelayUrl>() {
            Ok(url) => addrs.push(TransportAddr::Relay(url)),
            Err(e) => tracing::debug!("skipping relay url {relay}: {e}"),
        }
    }
    for ip in &peer.ip {
        match ip.parse::<SocketAddr>() {
            Ok(sa) => addrs.push(TransportAddr::Ip(sa)),
            Err(e) => tracing::debug!("skipping addr {ip}: {e}"),
        }
    }
    Ok(EndpointAddr::from_parts(id, addrs))
}

pub async fn connect(state: &State, peer: &PeerIn, alpn: &[u8]) -> Result<Connection> {
    let addr = peer_addr(peer)?;
    let conn = tokio::time::timeout(Duration::from_secs(10), state.endpoint.connect(addr, alpn))
        .await
        .map_err(|_| anyhow!("connect to {} timed out", peer.node_id))?
        .with_context(|| format!("connecting to {}", peer.node_id))?;
    Ok(conn)
}

/// Send one request on a fresh bidi stream; returns the stream for reading
/// the response line(s).
pub async fn request(
    conn: &Connection,
    req: &P2pRequest,
) -> Result<BufReader<iroh::endpoint::RecvStream>> {
    let (mut send, recv) = conn.open_bi().await.context("open_bi")?;
    send.write_all(p2p::encode(req).as_bytes()).await.context("send request")?;
    send.finish().ok();
    Ok(BufReader::new(recv))
}

/// Read the next response line, skipping unknown response types for forward
/// compatibility (DESIGN.md §8.1). `None` on stream end.
pub async fn read_response(
    reader: &mut BufReader<iroh::endpoint::RecvStream>,
) -> Result<Option<P2pResponse>> {
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            return Ok(None);
        }
        if line.len() > p2p::MAX_LINE {
            return Err(anyhow!("response line too long"));
        }
        match serde_json::from_str::<P2pResponse>(&line) {
            Ok(resp) => return Ok(Some(resp)),
            Err(_) => {
                tracing::debug!("skipping unknown response line: {}", line.trim());
                continue;
            }
        }
    }
}

/// Run a search: local matches first, then breadth-first fan-out to peers.
/// Hits stream into `tx` as they arrive; the function returns when all
/// branches finished or the deadline passed.
pub async fn run_search(
    state: Arc<State>,
    query_id: String,
    query: String,
    ttl: u8,
    requester: Requester,
    tx: mpsc::Sender<Hit>,
) {
    let config = state.config.read().await.clone();

    let (roots, forwarding_allowed, respect_allow_reshare) = match &requester {
        Requester::Local => (
            config.view_roots(VIEW_FULL).unwrap_or_default(),
            true,
            // my own searches may ask everyone; nothing is being re-served
            false,
        ),
        Requester::Peer { view_roots } => {
            (view_roots.clone(), config.reshare.serve, true)
        }
    };

    // local matches
    {
        let index = state.index.read().await;
        for file in index.search(&roots, &query) {
            let hit = Hit {
                name: file.path.clone(),
                size: file.size,
                hash: file.hash.clone(),
                source: HitSource::Local,
            };
            if tx.send(hit).await.is_err() {
                return;
            }
        }
    }

    if ttl == 0 || !forwarding_allowed {
        return;
    }

    // fan out to peers that granted us access
    let peers: Vec<PeerIn> = {
        let grants = state.grants.lock().await;
        grants
            .grants
            .peers
            .iter()
            .filter(|p| !respect_allow_reshare || p.allow_reshare)
            .take(config.search.fanout)
            .cloned()
            .collect()
    };
    if peers.is_empty() {
        return;
    }

    let deadline = Duration::from_secs(config.search.timeout_secs);
    let mut tasks = tokio::task::JoinSet::new();
    for peer in peers {
        let state = state.clone();
        let tx = tx.clone();
        let query_id = query_id.clone();
        let query = query.clone();
        tasks.spawn(async move {
            let result = tokio::time::timeout(
                deadline,
                forward_to_peer(&state, &peer, &query_id, &query, ttl - 1, tx),
            )
            .await;
            match result {
                Ok(Ok(())) => {}
                Ok(Err(e)) => tracing::debug!("search via {}: {e:#}", peer.node_id),
                Err(_) => tracing::debug!("search via {} timed out", peer.node_id),
            }
        });
    }
    while tasks.join_next().await.is_some() {}
}

async fn forward_to_peer(
    state: &State,
    peer: &PeerIn,
    query_id: &str,
    query: &str,
    ttl: u8,
    tx: mpsc::Sender<Hit>,
) -> Result<()> {
    let conn = connect(state, peer, p2p::ALPN).await?;
    let mut reader = request(
        &conn,
        &P2pRequest::Search {
            query_id: query_id.to_string(),
            ttl,
            query: query.to_string(),
        },
    )
    .await?;
    while let Some(resp) = read_response(&mut reader).await? {
        match resp {
            P2pResponse::Hit { name, size, hash, handle } => {
                let hit = Hit {
                    name,
                    size,
                    hash,
                    source: HitSource::Upstream { peer: peer.node_id.clone(), handle },
                };
                if tx.send(hit).await.is_err() {
                    break;
                }
            }
            P2pResponse::SearchDone => break,
            P2pResponse::Error { code, message } => {
                return Err(anyhow!("peer error {code}: {message}"));
            }
            other => tracing::debug!("unexpected search response: {other:?}"),
        }
    }
    Ok(())
}

/// Build the iroh-blobs request for `hash`, optionally restricted to a byte
/// range (inclusive). Ranges round up to 16 KiB chunk boundaries on the wire;
/// callers clip exact bytes on export.
fn get_request(hash: iroh_blobs::Hash, range: Option<ByteRange>) -> GetRequest {
    match range {
        Some((start, end)) => GetRequest::builder()
            .root(ChunkRanges::bytes(start..=end))
            .build(hash),
        None => GetRequest::blob(hash),
    }
}

/// Stream `hash` (optionally a byte range) from one source into our store,
/// verified end-to-end. `handle` is the source peer's search/browse handle
/// (None = the peer owns the file). Cumulative payload-byte progress is sent
/// to `progress`. The transfer streams: a relaying source splices bytes
/// through without buffering, so nothing is staged to disk mid-path.
pub async fn fetch_source(
    state: &State,
    peer: &PeerIn,
    handle: Option<String>,
    hash: &str,
    range: Option<ByteRange>,
    progress: &mpsc::Sender<u64>,
) -> Result<()> {
    let parsed: iroh_blobs::Hash = hash.parse().map_err(|e| anyhow!("bad hash {hash}: {e}"))?;
    let conn = connect(state, peer, p2p::ALPN).await?;
    let (mut send, recv) = conn.open_bi().await.context("open_bi")?;
    // header naming the source handle; the rest of the stream is iroh-blobs
    send.write_all(p2p::encode(&P2pRequest::Get { handle }).as_bytes())
        .await
        .context("send get header")?;

    let pair = StreamPair::new(conn.stable_id() as u64, recv, send);
    let request = get_request(parsed, range);
    let mut stream = state.store.remote().execute_get(pair, request).stream();
    while let Some(item) = stream.next().await {
        match item {
            GetProgressItem::Progress(n) => {
                let _ = progress.send(n).await;
            }
            GetProgressItem::Done(_) => return Ok(()),
            GetProgressItem::Error(e) => {
                return Err(anyhow!("transfer from {} failed: {e}", peer.node_id));
            }
        }
    }
    Err(anyhow!("transfer stream closed without completing"))
}

/// Serve a `Get` request from our own store on the given stream halves. The
/// peer drives the iroh-blobs client protocol; we answer from the store.
pub async fn serve_local(
    state: &State,
    conn_id: u64,
    recv: iroh::endpoint::RecvStream,
    send: iroh::endpoint::SendStream,
) -> Result<()> {
    let pair = iroh_blobs::provider::StreamPair::new(
        conn_id,
        recv,
        send,
        iroh_blobs::provider::events::EventSender::DEFAULT,
    );
    iroh_blobs::provider::handle_stream(pair, state.store.deref().clone())
        .await
        .map_err(|e| anyhow!("serving blob: {e}"))
}

/// Relay a `Get` to an upstream source, splicing raw bytes both ways
/// (DESIGN.md §7.3). No buffering: the client's request and the upstream's
/// bao-verified response stream straight through, so the relay never stages
/// the content and verification remains end-to-end.
pub async fn relay_get(
    state: &State,
    peer_node_id: &str,
    upstream_handle: String,
    mut client_recv: iroh::endpoint::RecvStream,
    mut client_send: iroh::endpoint::SendStream,
) -> Result<()> {
    let peer = {
        let grants = state.grants.lock().await;
        grants
            .grants
            .peers
            .iter()
            .find(|p| p.node_id == peer_node_id)
            .cloned()
            .ok_or_else(|| anyhow!("upstream peer {peer_node_id} no longer known"))?
    };
    let conn = connect(state, &peer, p2p::ALPN).await?;
    let (mut up_send, mut up_recv) = conn.open_bi().await.context("open_bi to upstream")?;
    up_send
        .write_all(p2p::encode(&P2pRequest::Get { handle: Some(upstream_handle) }).as_bytes())
        .await
        .context("forward get header")?;

    state.emit("relay_started", serde_json::json!({ "upstream": peer_node_id }));

    // client -> upstream (the get request), then signal end-of-request
    let up = tokio::spawn(async move {
        let _ = tokio::io::copy(&mut client_recv, &mut up_send).await;
        let _ = up_send.finish();
    });
    // upstream -> client (the bao-verified payload)
    let down = tokio::io::copy(&mut up_recv, &mut client_send).await;
    let _ = client_send.finish();
    let _ = up.await;
    down.context("relaying payload")?;
    Ok(())
}

/// Make a fresh query id for locally-originated searches.
pub fn new_query_id() -> String {
    libfilestr::grants::new_token()
}
