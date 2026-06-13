//! Local HTTP gateway: a read-only, loopback-only bridge that lets other apps
//! on the same device list and stream the files this node can serve. It exists
//! so a Grayjay plugin (which can only speak HTTP) can browse and play files
//! the daemon exposes — its own shares plus everything reachable through its
//! grant graph, exactly what `browse`/`get` see.
//!
//! Endpoints:
//!   GET /files            -> JSON: every servable file {name, hash, size, source, media, thumb}
//!   GET /search?q=        -> JSON: federated grant-graph search, same file shape
//!   GET /playlists[?source=] -> JSON: server-side folder/album/artist groupings
//!                            ({name, key, count, cover}) for the Grayjay channel
//!                            Playlists tab, optionally scoped to one source
//!   GET /playlist?kind=&key=&source= -> JSON {files,peers}: the tracks of ONE
//!                            grouping (folder/album/artist), so an opened
//!                            playlist resolves without a full /files pull
//!   GET /memberships?hash= -> JSON {source, groups:[{kind,name,key,count,cover}]}:
//!                            the folder/album/artist playlists ONE file belongs
//!                            to (scoped to that file's source), for the Grayjay
//!                            "Recommended" tab on a content page
//!   GET /peers            -> JSON {peers:[{label,node_id}]}: granted channels
//!                            (no browse), for creator search
//!   GET /file/{hash}      -> the bytes, with HTTP Range support (206)
//!   GET /thumb/{hash}     -> cached cover-art thumbnail, if any
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
    /// Media metadata (duration/tags); omitted entirely when empty.
    #[serde(skip_serializing_if = "libfilestr::ctl::MediaMeta::is_empty")]
    media: libfilestr::ctl::MediaMeta,
    /// True when a cover-art thumbnail is cached for this hash (served at
    /// `/thumb/{hash}`). Omitted when false.
    #[serde(skip_serializing_if = "is_false")]
    thumb: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// Per-peer reachability for one aggregation pass (mirrors the `peers` array in
/// `/files`): a granted peer is `reachable` only if its live browse answered.
#[derive(Serialize, Clone)]
struct PeerStat {
    label: String,
    node_id: String,
    reachable: bool,
}

/// A playlist grouping (a folder, an album tag, or an artist tag) summarised for
/// the Grayjay channel Playlists tab: just enough to render a stub, so the plugin
/// never has to pull and group the whole file list. `key` is the opaque value the
/// plugin puts in the playlist URL (a folder path, or the album/artist name);
/// `name` is its display label.
#[derive(Serialize)]
struct Group {
    name: String,
    key: String,
    count: u64,
    /// A hash in the group that has a cached thumbnail (served at `/thumb/{cover}`),
    /// for the playlist cover. Omitted when none of the group's files have one.
    #[serde(skip_serializing_if = "Option::is_none")]
    cover: Option<String>,
}

/// One playlist a given file belongs to, for the Grayjay "Recommended" tab — a
/// `Group` plus the `kind` (folder/album/artist) so the plugin can build the
/// playlist URL without re-deriving it.
#[derive(Serialize)]
struct MemberGroup {
    kind: &'static str,
    name: String,
    key: String,
    count: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    cover: Option<String>,
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
    if path == "/search" {
        let query = req
            .uri()
            .query()
            .and_then(|q| url_query(q).into_iter().find(|(k, _)| k == "q").map(|(_, v)| v))
            .unwrap_or_default();
        return search_files(&state, &query, is_head).await;
    }
    if path == "/playlists" {
        // optional `?source=<label>` scopes the groupings to one channel
        // ("local" or a peer label); absent = the whole reachable library.
        let source = req
            .uri()
            .query()
            .and_then(|q| url_query(q).into_iter().find(|(k, _)| k == "source").map(|(_, v)| v));
        return list_playlists(&state, source.as_deref(), is_head).await;
    }
    if path == "/playlist" {
        // resolve ONE grouping to its tracks: ?kind=folder|album|artist&key=&source=
        let q: Vec<(String, String)> = req.uri().query().map(url_query).unwrap_or_default();
        let get = |k: &str| q.iter().find(|(kk, _)| kk == k).map(|(_, v)| v.clone()).unwrap_or_default();
        return list_playlist(&state, &get("kind"), &get("key"), &get("source"), is_head).await;
    }
    if path == "/memberships" {
        // ?hash=<file hash>: the folder/album/artist playlists this file is in
        let hash = req
            .uri()
            .query()
            .and_then(|q| url_query(q).into_iter().find(|(k, _)| k == "hash").map(|(_, v)| v))
            .unwrap_or_default();
        return list_memberships(&state, &hash, is_head).await;
    }
    if path == "/peers" {
        return list_peers(&state, is_head).await;
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
    if let Some(hash) = path.strip_prefix("/thumb/") {
        return serve_thumb(&state, hash, is_head).await;
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
/// of every peer, as `FileItem`s plus per-peer reachability for this pass.
/// Browsing also records each file's source so a later `/file/{hash}` can fetch
/// it. Shared by `/files` and `/playlists` so both see the same fresh view.
async fn collect_files(state: &Arc<State>) -> Result<(Vec<FileItem>, Vec<PeerStat>)> {
    use std::collections::BTreeMap;
    let mut by_hash: BTreeMap<String, FileItem> = BTreeMap::new();

    // local shares (the implicit "full" view = all roots)
    {
        let index = state.index.read().await;
        let roots: Vec<String> =
            index.files.iter().map(|f| f.root.clone()).collect::<std::collections::BTreeSet<_>>().into_iter().collect();
        for e in index.entries(&roots) {
            let thumb = has_thumb(state, &e.hash);
            by_hash.entry(e.hash.clone()).or_insert(FileItem {
                name: e.path,
                hash: e.hash,
                size: e.size,
                source: "local".into(),
                media: e.media,
                thumb,
            });
        }
    }

    // each peer's browse — run concurrently with a per-peer deadline so one slow
    // or unreachable peer can't stall the whole listing (a hung browse used to
    // hang /files past an HTTP client's timeout). Results stay fresh: a peer that
    // misses the deadline is simply omitted from this response, never cached.
    let peers = { state.grants.lock().await.grants.peers.clone() };
    // Reachability for this browse: every granted peer starts unreachable and
    // flips to reachable only if its live browse answers in time. We report it
    // alongside the files so the app and the Grayjay plugin can say "this peer
    // is offline" instead of silently dropping it — an omitted peer is otherwise
    // indistinguishable from a peer that simply has nothing to share.
    let mut reach: BTreeMap<String, (String, bool)> = BTreeMap::new();
    for peer in &peers {
        let label = peer.label.clone().unwrap_or_else(|| short(&peer.node_id));
        reach.insert(peer.node_id.clone(), (label, false));
    }
    let browse_timeout =
        std::time::Duration::from_secs(state.config.read().await.search.browse_timeout_secs.max(1));
    let mut browses = tokio::task::JoinSet::new();
    for peer in peers {
        let label = peer.label.clone().unwrap_or_else(|| short(&peer.node_id));
        let node_id = peer.node_id.clone();
        let state = state.clone();
        browses.spawn(async move {
            let res = tokio::time::timeout(browse_timeout, browse_peer(&state, &node_id)).await;
            (label, node_id, res)
        });
    }
    while let Some(joined) = browses.join_next().await {
        let (label, node_id, res) = match joined {
            Ok(v) => v,
            Err(e) => {
                tracing::debug!("browse task panicked: {e:#}");
                continue;
            }
        };
        let entries = match res {
            Ok(Ok(e)) => e,
            Ok(Err(e)) => {
                tracing::debug!("browse {node_id} failed: {e:#}");
                continue;
            }
            Err(_) => {
                tracing::debug!("browse {node_id} timed out after {browse_timeout:?}");
                continue;
            }
        };
        // the browse answered: this peer is reachable for this listing
        if let Some(s) = reach.get_mut(&node_id) {
            s.1 = true;
        }
        // record sources so /file/{hash} can locate them
        {
            let mut recent = state.recent_sources.lock().await;
            for entry in &entries {
                recent.insert(
                    &entry.hash,
                    SourceRef { peer: node_id.clone(), handle: None, size: entry.size },
                );
            }
        }
        for e in entries {
            let thumb = has_thumb(state, &e.hash);
            by_hash.entry(e.hash.clone()).or_insert(FileItem {
                name: e.path,
                hash: e.hash,
                size: e.size,
                source: label.clone(),
                media: e.media,
                thumb,
            });
        }
    }

    let files: Vec<FileItem> = by_hash.into_values().collect();
    let peer_status: Vec<PeerStat> = reach
        .into_iter()
        .map(|(node_id, (label, reachable))| PeerStat { label, node_id, reachable })
        .collect();
    Ok((files, peer_status))
}

/// `/files`: every servable file plus per-peer reachability.
async fn list_files(state: &Arc<State>, is_head: bool) -> Result<Response<Body>> {
    let (files, peers) = collect_files(state).await?;
    json_response(&serde_json::json!({ "files": files, "peers": peers }), is_head)
}

/// `/playlists[?source=<label>]`: the channel Playlists tab, computed server-side
/// so the plugin gets a few hundred grouping stubs instead of pulling and
/// grouping the entire (potentially 14k-file) listing itself. Returns one entry
/// per folder, album tag and artist tag — scoped to `source` when given — plus
/// the same `peers` reachability array as `/files` (so the plugin can still tell
/// an offline peer apart from one with no files).
async fn list_playlists(state: &Arc<State>, source: Option<&str>, is_head: bool) -> Result<Response<Body>> {
    let (files, peers) = collect_files(state).await?;
    // an empty `?source=` means "whole library", same as omitting it
    let source = source.filter(|s| !s.is_empty());
    // Only group media files — mirror the plugin's `isPlayable` (anything that
    // maps to the generic octet-stream container is hidden) so a folder/album's
    // count here matches what getPlaylist later resolves.
    let items: Vec<&FileItem> = files
        .iter()
        .filter(|f| item_playable(f))
        .filter(|f| source.map_or(true, |s| f.source == s))
        .collect();
    // folders: audio/video only, so an artwork/cover-art folder isn't a playlist
    let av: Vec<&FileItem> = items.iter().copied().filter(|f| is_audio_or_video(f)).collect();
    let folders = group(&av, |f| Some(folder_of(&f.name)), folder_name);
    let albums = group(&items, |f| f.media.album.clone().filter(|s| !s.is_empty()), str::to_string);
    let artists = group(&items, |f| f.media.artist.clone().filter(|s| !s.is_empty()), str::to_string);
    json_response(
        &serde_json::json!({ "folders": folders, "albums": albums, "artists": artists, "peers": peers }),
        is_head,
    )
}

/// `/playlist?kind=&key=&source=`: the files in ONE grouping (a folder, album tag
/// or artist tag), so the plugin can resolve an opened playlist without pulling
/// and filtering the whole listing. `source` empty/absent = across the whole
/// reachable library. Same `{files, peers}` shape as `/files`.
async fn list_playlist(
    state: &Arc<State>,
    kind: &str,
    key: &str,
    source: &str,
    is_head: bool,
) -> Result<Response<Body>> {
    let (files, peers) = collect_files(state).await?;
    let want_source = (!source.is_empty()).then_some(source);
    let files: Vec<FileItem> = files
        .into_iter()
        .filter(|f| item_playable(f))
        .filter(|f| want_source.map_or(true, |s| f.source == s))
        .filter(|f| match kind {
            // folders are audio/video only (matches the /playlists folder grouping),
            // so a folder playlist never lists cover art / artwork images
            "folder" => is_audio_or_video(f) && folder_of(&f.name) == key,
            "album" => f.media.album.as_deref() == Some(key),
            "artist" => f.media.artist.as_deref() == Some(key),
            _ => false,
        })
        .collect();
    json_response(&serde_json::json!({ "files": files, "peers": peers }), is_head)
}

/// `/memberships?hash=<hash>`: the playlists ONE file belongs to — its folder
/// (if it's audio/video), its album tag and its artist tag — each scoped to that
/// file's own source, so opening one from the Recommended tab resolves to the
/// same-source collection (and counts/covers match the channel Playlists tab).
/// Drives the Grayjay content page's Recommended tab: "more from this album /
/// artist / folder". Empty `groups` when the hash isn't currently servable.
async fn list_memberships(state: &Arc<State>, hash: &str, is_head: bool) -> Result<Response<Body>> {
    let (files, _peers) = collect_files(state).await?;
    // the file in question (must be playable to have any playlist membership)
    let target = files.iter().find(|f| f.hash == hash && item_playable(f));
    let Some(target) = target else {
        return json_response(&serde_json::json!({ "source": "", "groups": [] }), is_head);
    };
    let source = target.source.clone();
    // its siblings: same-source playable files (the scope its playlists resolve in)
    let siblings: Vec<&FileItem> =
        files.iter().filter(|f| item_playable(f) && f.source == source).collect();

    // Summarise one grouping (count + a thumbnailed cover) over the siblings that
    // share `key` under `key_of` — mirrors how `group()` builds a stub.
    let summarise = |key_of: &dyn Fn(&FileItem) -> Option<String>, key: &str| -> (u64, Option<String>) {
        let mut count = 0u64;
        let mut cover = None;
        for f in &siblings {
            if key_of(f).as_deref() == Some(key) {
                count += 1;
                if cover.is_none() && f.thumb {
                    cover = Some(f.hash.clone());
                }
            }
        }
        (count, cover)
    };

    let mut groups: Vec<MemberGroup> = Vec::new();
    // folder — only for audio/video (an image's "folder" isn't a playlist), and
    // the count is audio/video only too, matching the /playlists folder grouping
    // (cover art doesn't pad a music folder's track count).
    if is_audio_or_video(target) {
        let key = folder_of(&target.name);
        let key_of = |f: &FileItem| is_audio_or_video(f).then(|| folder_of(&f.name));
        let (count, cover) = summarise(&key_of, &key);
        groups.push(MemberGroup { kind: "folder", name: folder_name(&key), key, count, cover });
    }
    // album tag
    if let Some(album) = target.media.album.clone().filter(|s| !s.is_empty()) {
        let key_of = |f: &FileItem| f.media.album.clone().filter(|s| !s.is_empty());
        let (count, cover) = summarise(&key_of, &album);
        groups.push(MemberGroup { kind: "album", name: album.clone(), key: album, count, cover });
    }
    // artist tag
    if let Some(artist) = target.media.artist.clone().filter(|s| !s.is_empty()) {
        let key_of = |f: &FileItem| f.media.artist.clone().filter(|s| !s.is_empty());
        let (count, cover) = summarise(&key_of, &artist);
        groups.push(MemberGroup { kind: "artist", name: artist.clone(), key: artist, count, cover });
    }

    json_response(&serde_json::json!({ "source": source, "groups": groups }), is_head)
}

/// `/peers`: the granted peers (label + node id) straight from the grant graph —
/// no file browse, so it's instant. Used for creator (channel) search, which only
/// needs the channel list, not their files or live reachability.
async fn list_peers(state: &Arc<State>, is_head: bool) -> Result<Response<Body>> {
    let peers = { state.grants.lock().await.grants.peers.clone() };
    let arr: Vec<serde_json::Value> = peers
        .iter()
        .map(|p| {
            let label = p.label.clone().unwrap_or_else(|| short(&p.node_id));
            serde_json::json!({ "label": label, "node_id": p.node_id })
        })
        .collect();
    json_response(&serde_json::json!({ "peers": arr }), is_head)
}

/// Group files by a key (album/artist tag, or folder path), counting each group
/// and picking a thumbnailed hash as its cover. `name_of` turns the grouping key
/// into a display label (identity for tags; last path segment for folders).
fn group(
    items: &[&FileItem],
    key_of: impl Fn(&FileItem) -> Option<String>,
    name_of: impl Fn(&str) -> String,
) -> Vec<Group> {
    use std::collections::BTreeMap;
    let mut map: BTreeMap<String, (u64, Option<String>)> = BTreeMap::new();
    for f in items {
        let Some(key) = key_of(f) else { continue };
        let e = map.entry(key).or_insert((0, None));
        e.0 += 1;
        if e.1.is_none() && f.thumb {
            e.1 = Some(f.hash.clone());
        }
    }
    map.into_iter()
        .map(|(key, (count, cover))| Group { name: name_of(&key), key, count, cover })
        .collect()
}

/// The content type of a file: sniffed at index time, else inferred from the
/// extension.
fn item_content_type(f: &FileItem) -> String {
    f.media.content_type.clone().unwrap_or_else(|| content_type(&f.name).to_string())
}

/// Whether a file is media the plugin would play — i.e. its content type isn't the
/// generic octet-stream. Matches the plugin's `isPlayable`.
fn item_playable(f: &FileItem) -> bool {
    item_content_type(f) != "application/octet-stream"
}

/// Whether a file is audio or video. Folder playlists require this so a folder of
/// nothing but cover art / artwork (images) isn't served as a playlist, and so
/// images don't pad a music folder's track count. (Album/artist groupings are
/// tag-based, so untagged images never form one.)
fn is_audio_or_video(f: &FileItem) -> bool {
    matches!(item_content_type(f).split('/').next().unwrap_or(""), "audio" | "video")
}

/// The folder a file lives in: its path minus the last segment ("" at the root).
fn folder_of(name: &str) -> String {
    name.rsplit_once('/').map(|(dir, _)| dir.to_string()).unwrap_or_default()
}

/// Display name for a folder grouping: its last path segment, or "files" at root.
fn folder_name(folder: &str) -> String {
    match folder.rsplit('/').next() {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => "files".to_string(),
    }
}

/// Serialize `value` as a JSON response (CORS-open), body suppressed for HEAD.
fn json_response(value: &serde_json::Value, is_head: bool) -> Result<Response<Body>> {
    let json = serde_json::to_vec(value)?;
    let len = json.len() as u64;
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::CONTENT_LENGTH, len)
        .header("access-control-allow-origin", "*")
        .body(if is_head { empty() } else { full(json) })?)
}

/// Run the daemon's federated grant-graph search and return the matches in the
/// same JSON shape as `/files`. Unlike `/files` (local shares + a one-hop browse
/// of direct peers), this reaches the whole reachable graph via TTL forwarding.
/// Upstream sources are recorded so `/file/{hash}` can fetch a result; local
/// hits are enriched with their media metadata + thumbnail.
async fn search_files(state: &Arc<State>, query: &str, is_head: bool) -> Result<Response<Body>> {
    use std::collections::BTreeMap;
    let query = query.trim();
    let mut by_hash: BTreeMap<String, FileItem> = BTreeMap::new();

    if !query.is_empty() {
        let config = state.config.read().await.clone();
        let query_id = search::new_query_id();
        state.seen_queries.lock().await.check_and_insert(&query_id);

        // peer node-id -> friendly label, to match the `source` shown by /files
        let labels: std::collections::HashMap<String, String> = {
            let grants = state.grants.lock().await;
            grants
                .grants
                .peers
                .iter()
                .filter_map(|p| p.label.clone().map(|l| (p.node_id.clone(), l)))
                .collect()
        };

        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let task = tokio::spawn(search::run_search(
            state.clone(),
            query_id,
            query.to_string(),
            config.search.max_ttl,
            search::Requester::Local,
            tx,
        ));

        let until = tokio::time::Instant::now()
            + std::time::Duration::from_secs(config.search.timeout_secs);
        loop {
            if by_hash.len() >= config.search.result_cap {
                break;
            }
            let hit = match tokio::time::timeout_at(until, rx.recv()).await {
                Ok(Some(hit)) => hit,
                Ok(None) => break, // search completed
                Err(_) => break,   // overall deadline
            };
            let search::Hit { name, size, hash, source } = hit;
            let src = match source {
                search::HitSource::Local => "local".to_string(),
                search::HitSource::Upstream { peer, handle } => {
                    state.recent_sources.lock().await.insert(
                        &hash,
                        SourceRef { peer: peer.clone(), handle: Some(handle), size },
                    );
                    labels.get(&peer).cloned().unwrap_or_else(|| short(&peer))
                }
            };
            by_hash.entry(hash.clone()).or_insert_with(|| FileItem {
                name,
                hash,
                size,
                source: src,
                media: Default::default(),
                thumb: false,
            });
        }
        task.abort();

        // enrich local results with media + thumbnail (peer hits carry neither
        // over the search wire yet)
        let index = state.index.read().await;
        for item in by_hash.values_mut() {
            if let Some(f) = index.files.iter().find(|f| f.hash == item.hash) {
                item.media = f.media.clone();
            }
            item.thumb = has_thumb(state, &item.hash);
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
            // Auto-derived from the git commit count (build.rs) so it rises on
            // every change — Grayjay only updates a plugin when its version goes
            // up. Without this, plugin changes never reach installed clients.
            cfg["version"] = env!("FILESTR_PLUGIN_VERSION").parse::<u64>().unwrap_or(1).into();
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

    // Resolve the size without fetching — for HEAD and GET alike. The range
    // maths and Content-Length need it up front; the bytes are fetched on
    // demand below. (`known_size` answers from the local store, the share
    // index, or a recent browse.)
    let size = match known_size(state, parsed, hash).await {
        Some(s) => s,
        None => return Ok(text(StatusCode::NOT_FOUND, "unknown hash".into())),
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

    // Build the body. HEAD -> headers only. Otherwise stream [start, end_excl):
    // if the whole blob is already local, stream it straight from the store;
    // otherwise fetch it from a peer window-by-window, so an open-ended range
    // (`bytes=0-`) starts playing without staging the entire file first.
    let complete = matches!(
        state.store.blobs().status(parsed).await?,
        iroh_blobs::api::proto::BlobStatus::Complete { .. }
    );
    let body = if is_head {
        empty()
    } else if complete {
        stream_local(state, parsed, start, end_excl)
    } else {
        stream_windowed(state.clone(), parsed, hash.to_string(), start, end_excl)
    };

    // Prefer the content type sniffed at index time (correct even for a file
    // with a missing/wrong extension); fall back to the name's extension.
    let ctype = state
        .index
        .read()
        .await
        .files
        .iter()
        .find(|f| f.hash == hash)
        .and_then(|f| f.media.content_type.clone())
        .unwrap_or_else(|| content_type(name).to_string());
    let mut builder = Response::builder()
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_TYPE, ctype)
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

/// Max bytes emitted per frame when piping a peer blob through — bounds the
/// per-frame memory of `concatenate` while still yielding as data arrives.
const MAX_EMIT: u64 = 1024 * 1024;

/// Stream `[start, end_excl)` of a fully-local blob straight from the store,
/// per leaf, clipping to the exact byte range. Zero extra buffering.
fn stream_local(state: &Arc<State>, parsed: iroh_blobs::Hash, start: u64, end_excl: u64) -> Body {
    let body_stream = state
        .store
        .blobs()
        .export_ranges(parsed, start..end_excl)
        .stream()
        .filter_map(move |item| match item {
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
            ExportRangesItem::Error(e) => Some(Err(std::io::Error::other(format!("export: {e}")))),
        });
    BodyExt::boxed_unsync(StreamBody::new(body_stream))
}

/// Stream `[start, end_excl)` of a not-yet-local blob, piping it through from a
/// peer as it arrives: a single background fetch of the whole range, while we
/// emit the available contiguous prefix from the store's bitfield as each chunk
/// is bao-verified in — so the first bytes reach the player after one chunk, not
/// a whole window. If the client disconnects (e.g. a seek), the body future
/// drops, which cancels the fetch.
fn stream_windowed(
    state: Arc<State>,
    parsed: iroh_blobs::Hash,
    hash: String,
    start: u64,
    end_excl: u64,
) -> Body {
    let body_stream = async_stream::try_stream! {
        // one fetch of the whole range; ticks on the channel wake us to drain
        let (tx, mut rx) = tokio::sync::mpsc::channel::<u64>(16);
        let fetch_state = state.clone();
        let fetch_hash = hash.clone();
        let _fetcher = AbortOnDrop(tokio::spawn(async move {
            let _ = transfers::fetch_range(&fetch_state, &fetch_hash, start, end_excl - 1, &tx).await;
        }));

        let mut cursor = start;
        loop {
            // how far is the contiguous, locally-present prefix from `cursor`?
            let bitfield = state.store.blobs().observe(parsed).await
                .map_err(|e| std::io::Error::other(format!("observe: {e}")))?;
            let avail = available_end(&bitfield.ranges, cursor, end_excl).min(cursor + MAX_EMIT);
            if avail > cursor {
                let bytes = state.store.blobs().export_ranges(parsed, cursor..avail)
                    .concatenate().await
                    .map_err(|e| std::io::Error::other(format!("export: {e}")))?;
                yield Frame::data(Bytes::from(bytes));
                cursor = avail;
                if cursor >= end_excl { break; }
                continue;
            }
            // nothing new yet — wait for the fetch to make progress
            match rx.recv().await {
                Some(_) => continue,
                None => {
                    // fetch finished; one final drain of anything it left present
                    let bitfield = state.store.blobs().observe(parsed).await
                        .map_err(|e| std::io::Error::other(format!("observe: {e}")))?;
                    let avail = available_end(&bitfield.ranges, cursor, end_excl);
                    if avail > cursor {
                        let bytes = state.store.blobs().export_ranges(parsed, cursor..avail)
                            .concatenate().await
                            .map_err(|e| std::io::Error::other(format!("export: {e}")))?;
                        yield Frame::data(Bytes::from(bytes));
                        cursor = avail;
                    }
                    if cursor < end_excl {
                        Err(std::io::Error::other("fetch ended before the range was complete"))?;
                    }
                    break;
                }
            }
        }
    };
    BodyExt::boxed_unsync(StreamBody::new(body_stream))
}

/// Largest byte offset `E` in `(cursor, end_excl]` such that every chunk covering
/// `[cursor, E)` is present in `ranges` (the store bitfield). Found by binary
/// search over `is_subset`, so there's no manual chunk-unit arithmetic. Returns
/// `cursor` when even the chunk at `cursor` isn't present yet.
fn available_end(
    ranges: &iroh_blobs::protocol::ChunkRanges,
    cursor: u64,
    end_excl: u64,
) -> u64 {
    use iroh_blobs::protocol::ChunkRangesExt;
    if cursor >= end_excl {
        return cursor;
    }
    let present = |e: u64| iroh_blobs::protocol::ChunkRanges::bytes(cursor..=e - 1).is_subset(ranges);
    if !present(cursor + 1) {
        return cursor;
    }
    let (mut lo, mut hi) = (cursor + 1, end_excl);
    while lo < hi {
        let mid = lo + (hi - lo).div_ceil(2);
        if present(mid) {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    lo
}

/// Aborts the wrapped task when dropped — so a disconnected client cancels the
/// in-flight fetch instead of leaving it running.
struct AbortOnDrop(tokio::task::JoinHandle<()>);
impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Whether a cover-art thumbnail is cached locally for `hash`.
fn has_thumb(state: &Arc<State>, hash: &str) -> bool {
    !hash.is_empty() && state.thumbs_dir.join(hash).exists()
}

/// Serve a cached cover-art thumbnail. Like the file endpoint, it carries a
/// strong ETag (the file's content hash) and answers conditional/HEAD requests,
/// since the artwork for a given hash never changes.
async fn serve_thumb(state: &Arc<State>, hash: &str, is_head: bool) -> Result<Response<Body>> {
    // guard against path traversal: the hash is the only path component
    if hash.is_empty() || hash.contains('/') || hash.contains('.') {
        return Ok(text(StatusCode::BAD_REQUEST, "bad hash".into()));
    }
    let bytes = match tokio::fs::read(state.thumbs_dir.join(hash)).await {
        Ok(b) => b,
        Err(_) => return Ok(text(StatusCode::NOT_FOUND, "no thumbnail".into())),
    };
    let len = bytes.len() as u64;
    let ctype = sniff_image(&bytes);
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, ctype)
        .header(header::CONTENT_LENGTH, len)
        .header(header::ETAG, format!("\"{hash}\""))
        .header(header::CACHE_CONTROL, "public, max-age=31536000, immutable")
        .header("access-control-allow-origin", "*")
        .body(if is_head { empty() } else { full(bytes) })?)
}

/// Content type of an image from its magic bytes (cover art is ~always JPEG or
/// PNG); defaults to JPEG.
fn sniff_image(b: &[u8]) -> &'static str {
    if b.starts_with(&[0x89, b'P', b'N', b'G']) {
        "image/png"
    } else if b.starts_with(&[b'G', b'I', b'F']) {
        "image/gif"
    } else if b.len() >= 12 && &b[0..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        "image/webp"
    } else {
        "image/jpeg"
    }
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
