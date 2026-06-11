//! Local HTTP gateway: a read-only, loopback-only bridge that lets other apps
//! on the same device list and stream the files this node can serve. It exists
//! so a Grayjay plugin (which can only speak HTTP) can browse and play files
//! the daemon exposes — its own shares plus everything reachable through its
//! grant graph, exactly what `browse`/`get` see.
//!
//! Two endpoints:
//!   GET /files            -> JSON: every servable file {name, hash, size, source}
//!   GET /file/{hash}      -> the bytes, with HTTP Range support (206)
//!
//! Both also answer HEAD (size/type/Range probe with no transfer), and
//! `/file/{hash}` carries a strong `ETag` (the content hash) with conditional
//! `If-None-Match` (304) and `If-Range` support.
//!
//! Bind only to 127.0.0.1 (enforced by config); there is no auth.

use std::convert::Infallible;
use std::sync::Arc;

use anyhow::{Context, Result};
use futures_lite::StreamExt;
use http_body_util::combinators::UnsyncBoxBody;
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::body::{Bytes, Frame};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode, header};
use hyper_util::rt::TokioIo;
use iroh_blobs::api::proto::ExportRangesItem;
use libfilestr::ctl::FileEntry;
use libfilestr::p2p::{ALPN, P2pRequest, P2pResponse};
use serde::Serialize;
use tokio::net::TcpListener;

use crate::search;
use crate::state::{SourceRef, State};
use crate::transfers;

type Body = UnsyncBoxBody<Bytes, std::io::Error>;

#[derive(Serialize)]
struct FileItem {
    name: String,
    hash: String,
    size: u64,
    /// "local" or a peer label / short node id — informational only.
    source: String,
}

/// Run the gateway until the process exits.
pub async fn serve(state: Arc<State>, addr: std::net::SocketAddr) -> Result<()> {
    let listener = TcpListener::bind(addr).await.with_context(|| format!("binding http {addr}"))?;
    tracing::info!(%addr, "http gateway listening");
    loop {
        let (stream, _peer) = match listener.accept().await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("http accept error: {e}");
                continue;
            }
        };
        let state = state.clone();
        tokio::spawn(async move {
            let io = TokioIo::new(stream);
            let service = service_fn(move |req| handle(state.clone(), req));
            if let Err(e) = http1::Builder::new().serve_connection(io, service).await {
                tracing::debug!("http connection error: {e}");
            }
        });
    }
}

async fn handle(state: Arc<State>, req: Request<hyper::body::Incoming>) -> Result<Response<Body>, Infallible> {
    let resp = route(state, req).await.unwrap_or_else(|e| {
        text(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}"))
    });
    Ok(resp)
}

async fn route(state: Arc<State>, req: Request<hyper::body::Incoming>) -> Result<Response<Body>> {
    // HEAD is served like GET but with the body suppressed, so players can probe
    // size / type / Range support (and revalidate via ETag) without a transfer.
    let is_head = req.method() == Method::HEAD;
    if req.method() != Method::GET && !is_head {
        return Ok(text(StatusCode::METHOD_NOT_ALLOWED, "GET/HEAD only".into()));
    }
    let path = req.uri().path().to_string();
    if path == "/" || path == "/files" {
        return list_files(&state, is_head).await;
    }
    if path.starts_with("/grayjay") {
        let host = req
            .headers()
            .get(header::HOST)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("127.0.0.1")
            .to_string();
        return serve_grayjay(&path, &host, is_head);
    }
    if let Some(hash) = path.strip_prefix("/file/") {
        let hdr = |name: header::HeaderName| {
            req.headers().get(name).and_then(|v| v.to_str().ok()).map(String::from)
        };
        let range = hdr(header::RANGE);
        let if_none_match = hdr(header::IF_NONE_MATCH);
        let if_range = hdr(header::IF_RANGE);
        let name = req
            .uri()
            .query()
            .and_then(|q| {
                url_query(q).into_iter().find(|(k, _)| k == "name").map(|(_, v)| v)
            })
            .unwrap_or_default();
        return serve_file(
            &state,
            hash,
            &name,
            range.as_deref(),
            if_none_match.as_deref(),
            if_range.as_deref(),
            is_head,
        )
        .await;
    }
    Ok(text(StatusCode::NOT_FOUND, "not found".into()))
}

/// Aggregate everything this node can serve: its own shares plus a live browse
/// of every peer. Browsing also records the source so a later `/file/{hash}`
/// can fetch it.
async fn list_files(state: &Arc<State>, is_head: bool) -> Result<Response<Body>> {
    use std::collections::BTreeMap;
    let mut by_hash: BTreeMap<String, FileItem> = BTreeMap::new();

    // local shares (the implicit "full" view = all roots)
    {
        let index = state.index.read().await;
        let roots: Vec<String> =
            index.files.iter().map(|f| f.root.clone()).collect::<std::collections::BTreeSet<_>>().into_iter().collect();
        for e in index.entries(&roots) {
            by_hash.entry(e.hash.clone()).or_insert(FileItem {
                name: e.path,
                hash: e.hash,
                size: e.size,
                source: "local".into(),
            });
        }
    }

    // each peer's browse
    let peers = { state.grants.lock().await.grants.peers.clone() };
    for peer in peers {
        let label = peer.label.clone().unwrap_or_else(|| short(&peer.node_id));
        let entries = match browse_peer(state, &peer.node_id).await {
            Ok(e) => e,
            Err(e) => {
                tracing::debug!("browse {} failed: {e:#}", peer.node_id);
                continue;
            }
        };
        // record sources so /file/{hash} can locate them
        {
            let mut recent = state.recent_sources.lock().await;
            for entry in &entries {
                recent.insert(
                    &entry.hash,
                    SourceRef { peer: peer.node_id.clone(), handle: None, size: entry.size },
                );
            }
        }
        for e in entries {
            by_hash.entry(e.hash.clone()).or_insert(FileItem {
                name: e.path,
                hash: e.hash,
                size: e.size,
                source: label.clone(),
            });
        }
    }

    let files: Vec<FileItem> = by_hash.into_values().collect();
    let json = serde_json::to_vec(&serde_json::json!({ "files": files }))?;
    let len = json.len() as u64;
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::CONTENT_LENGTH, len)
        .header("access-control-allow-origin", "*")
        .body(if is_head { empty() } else { full(json) })?)
}

// The Grayjay source plugin, embedded so the daemon is self-contained: a
// device can add the source straight from this gateway
// (http://127.0.0.1:11780/grayjay/FilestrConfig.json) with no extra server.
const GRAYJAY_CONFIG: &str = include_str!("../../grayjay-plugin/FilestrConfig.json");
const GRAYJAY_SCRIPT: &str = include_str!("../../grayjay-plugin/FilestrScript.js");
const GRAYJAY_ICON: &[u8] = include_bytes!("../../grayjay-plugin/filestr.png");

/// Serve the bundled Grayjay plugin. The config's URLs are rewritten to absolute
/// URLs against the request's Host, so it works on whatever address/port the
/// gateway is reached at.
fn serve_grayjay(path: &str, host: &str, is_head: bool) -> Result<Response<Body>> {
    let base = format!("http://{host}/grayjay");
    let (content_type, bytes): (&str, Vec<u8>) = match path {
        "/grayjay/FilestrConfig.json" | "/grayjay" | "/grayjay/" => {
            let mut cfg: serde_json::Value = serde_json::from_str(GRAYJAY_CONFIG)?;
            cfg["sourceUrl"] = format!("{base}/FilestrConfig.json").into();
            cfg["scriptUrl"] = format!("{base}/FilestrScript.js").into();
            cfg["iconUrl"] = format!("{base}/filestr.png").into();
            ("application/json", serde_json::to_vec(&cfg)?)
        }
        "/grayjay/FilestrScript.js" => ("application/javascript", GRAYJAY_SCRIPT.as_bytes().to_vec()),
        "/grayjay/filestr.png" => ("image/png", GRAYJAY_ICON.to_vec()),
        _ => return Ok(text(StatusCode::NOT_FOUND, "not found".into())),
    };
    let len = bytes.len() as u64;
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CONTENT_LENGTH, len)
        .header("access-control-allow-origin", "*")
        .body(if is_head { empty() } else { full(bytes) })?)
}

async fn browse_peer(state: &Arc<State>, node_id: &str) -> Result<Vec<FileEntry>> {
    let peer = {
        let grants = state.grants.lock().await;
        grants
            .grants
            .peers
            .iter()
            .find(|p| p.node_id == node_id)
            .cloned()
            .context("peer gone")?
    };
    let conn = search::connect(state, &peer, ALPN).await?;
    let mut reader = search::request(&conn, &P2pRequest::List).await?;
    let mut entries = Vec::new();
    loop {
        match search::read_response(&mut reader).await? {
            Some(P2pResponse::Entries { entries: chunk }) => entries.extend(chunk),
            Some(P2pResponse::ListDone { .. }) | None => break,
            Some(P2pResponse::Error { code, message }) => {
                anyhow::bail!("peer error {code}: {message}")
            }
            Some(_) => {}
        }
    }
    Ok(entries)
}

/// Fetch the blob into the local store if needed, then stream the requested
/// byte range from the store (chunked, never buffering the whole file).
async fn serve_file(
    state: &Arc<State>,
    hash: &str,
    name: &str,
    range: Option<&str>,
    if_none_match: Option<&str>,
    if_range: Option<&str>,
    is_head: bool,
) -> Result<Response<Body>> {
    let parsed: iroh_blobs::Hash = match hash.parse() {
        Ok(h) => h,
        Err(_) => return Ok(text(StatusCode::BAD_REQUEST, "bad hash".into())),
    };

    // The content hash is a perfect strong validator: content-addressed bytes
    // are immutable, so a client holding this ETag never needs a re-transfer.
    let etag = format!("\"{hash}\"");
    if if_none_match.is_some_and(|inm| etag_matches(inm, hash)) {
        return Ok(Response::builder()
            .status(StatusCode::NOT_MODIFIED)
            .header(header::ETAG, &etag)
            .header(header::ACCEPT_RANGES, "bytes")
            .header("access-control-allow-origin", "*")
            .body(empty())?);
    }

    // Resolve the size. A HEAD must not trigger a transfer — answer from what we
    // already know (local store / index / a recent browse). A GET fetches the
    // blob into the local store first, as before.
    let size = if is_head {
        match known_size(state, parsed, hash).await {
            Some(s) => s,
            None => return Ok(text(StatusCode::NOT_FOUND, "unknown hash".into())),
        }
    } else {
        if let Err(e) = transfers::ensure_local(state, hash).await {
            return Ok(text(StatusCode::NOT_FOUND, format!("{e:#}")));
        }
        match state.store.blobs().status(parsed).await? {
            iroh_blobs::api::proto::BlobStatus::Complete { size } => size,
            other => return Ok(text(StatusCode::NOT_FOUND, format!("incomplete: {other:?}"))),
        }
    };

    // If-Range: only honour the Range when the validator still matches; an
    // immutable hash always does, so a stale-validator client gets a clean 200.
    let range = match if_range {
        Some(ir) if !etag_matches(ir, hash) => None,
        _ => range,
    };

    let (start, end_incl, partial) = match range.and_then(|r| parse_range(r, size)) {
        Some((s, e)) => (s, e, true),
        None if range.is_some() => {
            // unsatisfiable
            return Ok(Response::builder()
                .status(StatusCode::RANGE_NOT_SATISFIABLE)
                .header(header::CONTENT_RANGE, format!("bytes */{size}"))
                .header(header::ETAG, &etag)
                .body(empty())?);
        }
        None => (0, size.saturating_sub(1), false),
    };
    let end_excl = end_incl + 1;
    let len = end_excl - start;

    // HEAD: headers only. GET: chunked stream from the store, clipped to range.
    let body = if is_head {
        empty()
    } else {
        let progress = state.store.blobs().export_ranges(parsed, start..end_excl);
        let body_stream = progress.stream().filter_map(move |item| match item {
            ExportRangesItem::Data(leaf) => {
                let chunk_start = leaf.offset;
                let chunk_end = leaf.offset + leaf.data.len() as u64;
                let from = start.max(chunk_start);
                let to = end_excl.min(chunk_end);
                if from >= to {
                    return None;
                }
                let lo = (from - chunk_start) as usize;
                let hi = (to - chunk_start) as usize;
                let bytes = leaf.data.slice(lo..hi);
                Some(Ok(Frame::data(Bytes::copy_from_slice(&bytes))))
            }
            ExportRangesItem::Size(_) => None,
            ExportRangesItem::Error(e) => {
                Some(Err(std::io::Error::other(format!("export: {e}"))))
            }
        });
        BodyExt::boxed_unsync(StreamBody::new(body_stream))
    };

    let mut builder = Response::builder()
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_TYPE, content_type(name))
        .header(header::CONTENT_LENGTH, len)
        .header(header::ETAG, &etag)
        .header("access-control-allow-origin", "*");
    if partial {
        builder = builder
            .status(StatusCode::PARTIAL_CONTENT)
            .header(header::CONTENT_RANGE, format!("bytes {start}-{end_incl}/{size}"));
    } else {
        builder = builder.status(StatusCode::OK);
    }
    Ok(builder.body(body)?)
}

/// Size of `hash` from what we already know, without fetching: the local store
/// if complete, else the share index, else a size reported by a recent browse.
async fn known_size(state: &Arc<State>, parsed: iroh_blobs::Hash, hash: &str) -> Option<u64> {
    if let Ok(iroh_blobs::api::proto::BlobStatus::Complete { size }) =
        state.store.blobs().status(parsed).await
    {
        return Some(size);
    }
    if let Some(f) = state.index.read().await.files.iter().find(|f| f.hash == hash) {
        return Some(f.size);
    }
    state
        .recent_sources
        .lock()
        .await
        .get(hash)
        .into_iter()
        .map(|s| s.size)
        .find(|&s| s > 0)
}

/// Whether an `If-None-Match` / `If-Range` header value matches our strong tag.
/// Accepts `*`, a comma-separated list, and weak (`W/`) prefixes.
fn etag_matches(header_value: &str, hash: &str) -> bool {
    header_value.split(',').any(|t| {
        let t = t.trim();
        t == "*" || t.trim_start_matches("W/").trim_matches('"') == hash
    })
}

/// Parse an HTTP Range header (single range) against a known size. Returns an
/// inclusive (start, end) clamped to the file, or None if unsatisfiable.
fn parse_range(raw: &str, size: u64) -> Option<(u64, u64)> {
    let spec = raw.strip_prefix("bytes=")?.split(',').next()?.trim();
    let (a, b) = spec.split_once('-')?;
    if size == 0 {
        return None;
    }
    let (start, end) = if a.is_empty() {
        // suffix: last N bytes
        let n: u64 = b.parse().ok()?;
        if n == 0 {
            return None;
        }
        (size.saturating_sub(n), size - 1)
    } else {
        let start: u64 = a.parse().ok()?;
        let end: u64 = if b.is_empty() { size - 1 } else { b.parse().ok()? };
        (start, end.min(size - 1))
    };
    if start > end || start >= size {
        return None;
    }
    Some((start, end))
}

fn content_type(name: &str) -> &'static str {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "mp4" | "m4v" => "video/mp4",
        "webm" => "video/webm",
        "mkv" => "video/x-matroska",
        "mov" => "video/quicktime",
        "avi" => "video/x-msvideo",
        "ts" => "video/mp2t",
        "m3u8" => "application/vnd.apple.mpegurl",
        "mp3" => "audio/mpeg",
        "m4a" | "aac" => "audio/mp4",
        "flac" => "audio/flac",
        "ogg" | "opus" => "audio/ogg",
        "wav" => "audio/wav",
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "pdf" => "application/pdf",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

fn short(node_id: &str) -> String {
    node_id.chars().take(12).collect::<String>() + "…"
}

/// Minimal `application/x-www-form-urlencoded` query parse (keys/values we
/// emit are simple; this only needs to recover `name`).
fn url_query(q: &str) -> Vec<(String, String)> {
    q.split('&')
        .filter_map(|kv| {
            let (k, v) = kv.split_once('=')?;
            Some((percent_decode(k), percent_decode(v)))
        })
        .collect()
}

fn percent_decode(s: &str) -> String {
    let bytes = s.replace('+', " ");
    let bytes = bytes.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(&format!("{}{}", bytes[i + 1] as char, bytes[i + 2] as char), 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn full(body: impl Into<Bytes>) -> Body {
    BodyExt::boxed_unsync(Full::new(body.into()).map_err(|e| match e {}))
}

fn empty() -> Body {
    BodyExt::boxed_unsync(Full::new(Bytes::new()).map_err(|e| match e {}))
}

fn text(status: StatusCode, msg: String) -> Response<Body> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(full(msg))
        .unwrap()
}
