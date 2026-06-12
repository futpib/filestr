//! Control socket server: newline-delimited JSON over a unix socket,
//! mirroring slopd's `{"id", "body"}` protocol.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use libfilestr::config::{RelaySetting, VIEW_FULL};
use libfilestr::ctl::{
    DaemonStatus, FileEntry, IndexProgress, InviteInfo, PeerInfo, Request, RequestBody, Response,
    ResponseBody, SearchHit, ShareInfo, ViewInfo,
};
use libfilestr::grants::PeerIn;
use libfilestr::p2p::{P2pRequest, P2pResponse};
use libfilestr::ticket::{TICKET_VERSION, Ticket};
use libfilestr::{VERSION, unix_now};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::net::unix::OwnedWriteHalf;
use tokio::sync::mpsc;

use crate::search::{self as search_mod, HitSource, Requester};
use crate::state::{SourceRef, State};
use crate::transfers;

pub async fn run(state: Arc<State>, socket: PathBuf) -> Result<()> {
    if let Some(parent) = socket.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // single-daemon-per-socket assumption, like slopd: clear stale socket
    let _ = std::fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket)
        .with_context(|| format!("binding control socket {}", socket.display()))?;
    tracing::info!(socket = %socket.display(), "control socket ready");

    loop {
        tokio::select! {
            _ = state.shutdown.cancelled() => break,
            accepted = listener.accept() => {
                match accepted {
                    Ok((stream, _)) => {
                        let state = state.clone();
                        tokio::spawn(async move {
                            if let Err(e) = handle_connection(state, stream).await {
                                tracing::debug!("ctl connection error: {e:#}");
                            }
                        });
                    }
                    Err(e) => tracing::warn!("ctl accept error: {e}"),
                }
            }
        }
    }
    let _ = std::fs::remove_file(&socket);
    Ok(())
}

async fn write_response(
    writer: &mut OwnedWriteHalf,
    id: u64,
    body: ResponseBody,
) -> Result<()> {
    let mut line = serde_json::to_string(&Response { id, body })?;
    line.push('\n');
    writer.write_all(line.as_bytes()).await?;
    Ok(())
}

async fn handle_connection(state: Arc<State>, stream: tokio::net::UnixStream) -> Result<()> {
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let request: Request = match serde_json::from_str(&line) {
            Ok(request) => request,
            Err(e) => {
                // no id to echo; use 0 per protocol convention
                write_response(&mut write, 0, ResponseBody::Error {
                    message: format!("bad request: {e}"),
                })
                .await?;
                continue;
            }
        };
        let id = request.id;
        match request.body {
            RequestBody::Search { query, ttl } => {
                handle_search(&state, &mut write, id, query, ttl).await?;
            }
            RequestBody::Get { hash, out, peer, range, background } => {
                handle_get(&state, &mut write, id, hash, out, peer, range, background).await?;
            }
            RequestBody::Subscribe => {
                handle_subscribe(&state, &mut write, id).await?;
                break; // subscription consumed the connection
            }
            RequestBody::Shutdown => {
                write_response(&mut write, id, ResponseBody::ShuttingDown).await?;
                state.shutdown.cancel();
                break;
            }
            body => {
                let response = handle_simple(&state, body).await;
                write_response(&mut write, id, response).await?;
            }
        }
    }
    Ok(())
}

/// Non-streaming requests: one response each.
async fn handle_simple(state: &Arc<State>, body: RequestBody) -> ResponseBody {
    let result = match body {
        RequestBody::Status => handle_status(state).await,
        RequestBody::InviteCreate { view, label, allow_reshare, relay_only } => {
            handle_invite_create(state, view, label, allow_reshare, relay_only).await
        }
        RequestBody::InviteList => handle_invite_list(state).await,
        RequestBody::InviteRevoke { token_id } => handle_revoke(state, token_id).await,
        RequestBody::PeerAdd { ticket, label } => handle_peer_add(state, ticket, label).await,
        RequestBody::PeerList => handle_peer_list(state).await,
        RequestBody::PeerRevoke { peer } => handle_revoke(state, peer).await,
        RequestBody::ShareList => handle_share_list(state).await,
        RequestBody::ShareAdd { path, name } => handle_share_add(state, path, name).await,
        RequestBody::ShareRemove { name } => handle_share_remove(state, name).await,
        RequestBody::Rescan => handle_rescan(state).await,
        RequestBody::ScanCancel => handle_scan_cancel(state).await,
        RequestBody::ScanPause => handle_scan_pause(state).await,
        RequestBody::ScanResume => handle_scan_resume(state).await,
        RequestBody::Browse { peer } => handle_browse(state, peer).await,
        RequestBody::Transfers => handle_transfers(state).await,
        RequestBody::TransferCancel { id } => handle_transfer_cancel(state, id).await,
        RequestBody::Reputation => handle_reputation(state).await,
        RequestBody::HubCreate { name } => handle_hub_create(state, name).await,
        RequestBody::HubInvite { hub } => handle_hub_invite(state, hub).await,
        RequestBody::HubJoin { ticket } => handle_hub_join(state, ticket).await,
        RequestBody::HubRequest { address, hub, label } => {
            handle_hub_request(state, address, hub, label).await
        }
        RequestBody::HubAdmit { ticket, hub } => handle_hub_admit(state, ticket, hub).await,
        RequestBody::HubAddress { hub } => handle_hub_address(state, hub).await,
        RequestBody::HubPending => handle_hub_pending(state).await,
        RequestBody::HubList => handle_hub_list(state).await,
        RequestBody::HubMembers { hub } => handle_hub_members(state, hub).await,
        RequestBody::HubSend { hub, text } => handle_hub_send(state, hub, text).await,
        RequestBody::HubLog { hub } => handle_hub_log(state, hub).await,
        RequestBody::Search { .. }
        | RequestBody::Get { .. }
        | RequestBody::Subscribe
        | RequestBody::Shutdown => {
            unreachable!("handled by caller")
        }
    };
    match result {
        Ok(response) => response,
        Err(e) => ResponseBody::Error { message: format!("{e:#}") },
    }
}

async fn handle_status(state: &Arc<State>) -> Result<ResponseBody> {
    let addr = state.endpoint.addr();
    let grants = state.grants.lock().await;
    let (active, issued) = grants.grants.grants.iter().fold((0, 0), |(a, i), g| {
        match g.state {
            libfilestr::grants::GrantState::Active => (a + 1, i),
            libfilestr::grants::GrantState::Issued => (a, i + 1),
            libfilestr::grants::GrantState::Revoked => (a, i),
        }
    });
    Ok(ResponseBody::Status {
        status: DaemonStatus {
            endpoint_id: state.endpoint.id().to_string(),
            relays: addr.relay_urls().map(|u| u.to_string()).collect(),
            direct_addrs: addr.ip_addrs().map(|a| a.to_string()).collect(),
            files: state.index.read().await.files.len(),
            grants_active: active,
            grants_issued: issued,
            peers: grants.grants.peers.len(),
            version: VERSION.to_string(),
            indexing: state
                .scan_progress()
                .map(|(done, total, paused)| IndexProgress { done, total, paused }),
        },
    })
}

async fn handle_scan_cancel(state: &Arc<State>) -> Result<ResponseBody> {
    let cancelled = state.cancel_scan();
    state.emit("scan_cancelled", serde_json::json!({ "cancelled": cancelled }));
    Ok(ResponseBody::Rescanned { files: state.index.read().await.files.len() })
}

async fn handle_scan_pause(state: &Arc<State>) -> Result<ResponseBody> {
    let paused = state.pause_scan();
    state.emit("scan_paused", serde_json::json!({ "paused": paused }));
    Ok(ResponseBody::Rescanned { files: state.index.read().await.files.len() })
}

async fn handle_scan_resume(state: &Arc<State>) -> Result<ResponseBody> {
    let resumed = state.resume_scan();
    state.emit("scan_resumed", serde_json::json!({ "resumed": resumed }));
    Ok(ResponseBody::Rescanned { files: state.index.read().await.files.len() })
}

/// Wait briefly until the endpoint has a dialable address to put in tickets.
async fn dialable_addr(state: &Arc<State>) -> (Vec<String>, Vec<String>) {
    let relay_enabled =
        { state.config.read().await.relay == RelaySetting::Default };
    for _ in 0..20 {
        let addr = state.endpoint.addr();
        let relays: Vec<String> = addr.relay_urls().map(|u| u.to_string()).collect();
        let ips: Vec<String> = addr.ip_addrs().map(|a| a.to_string()).collect();
        if (relay_enabled && !relays.is_empty()) || (!relay_enabled && !ips.is_empty()) {
            return (relays, ips);
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let addr = state.endpoint.addr();
    (
        addr.relay_urls().map(|u| u.to_string()).collect(),
        addr.ip_addrs().map(|a| a.to_string()).collect(),
    )
}

/// Mint an invite, returning the ticket struct (callers encode it directly or
/// wrap it in a hub ticket).
pub(crate) async fn mint_invite(
    state: &Arc<State>,
    view: Option<String>,
    label: Option<String>,
    allow_reshare: Option<bool>,
    relay_only: Option<bool>,
) -> Result<(Ticket, String)> {
    let config = state.config.read().await.clone();
    let view = view.unwrap_or_else(|| VIEW_FULL.to_string());
    if config.view_roots(&view).is_none() {
        return Err(anyhow!("unknown view {view:?}"));
    }
    let (relays, ips) = dialable_addr(state).await;
    let relay_only = relay_only.unwrap_or(config.invite.relay_only);
    let ips = if relay_only { Vec::new() } else { ips };
    if relays.is_empty() && ips.is_empty() {
        return Err(anyhow!("endpoint has no dialable address yet; try again shortly"));
    }

    let mut grants = state.grants.lock().await;
    let grant = grants.grants.issue(
        view,
        label,
        allow_reshare.unwrap_or(config.reshare.allow),
        config.invite.expiry_secs,
    );
    let ticket = Ticket {
        v: TICKET_VERSION,
        id: state.endpoint.id().to_string(),
        relay: relays,
        ip: ips,
        token: grant.token.clone().expect("fresh grant has token"),
        label: grant.label.clone(),
    };
    let token_id = grant.token_id.clone();
    grants.save()?;
    Ok((ticket, token_id))
}

async fn handle_invite_create(
    state: &Arc<State>,
    view: Option<String>,
    label: Option<String>,
    allow_reshare: Option<bool>,
    relay_only: Option<bool>,
) -> Result<ResponseBody> {
    let (ticket, token_id) = mint_invite(state, view, label, allow_reshare, relay_only).await?;
    Ok(ResponseBody::InviteCreated { ticket: ticket.encode(), token_id })
}

fn invite_info(g: &libfilestr::grants::GrantOut) -> InviteInfo {
    InviteInfo {
        token_id: g.token_id.clone(),
        label: g.label.clone(),
        view: g.view.clone(),
        allow_reshare: g.allow_reshare,
        state: g.state.as_str().to_string(),
        node_id: g.node_id.clone(),
        created_at: g.created_at,
        expires_at: g.expires_at,
    }
}

async fn handle_invite_list(state: &Arc<State>) -> Result<ResponseBody> {
    let grants = state.grants.lock().await;
    Ok(ResponseBody::Invites {
        invites: grants.grants.grants.iter().map(invite_info).collect(),
    })
}

async fn handle_revoke(state: &Arc<State>, needle: String) -> Result<ResponseBody> {
    let mut grants = state.grants.lock().await;
    let mut revoked = grants.grants.revoke(&needle);
    revoked.extend(grants.grants.drop_peers(&needle));
    if revoked.is_empty() {
        return Err(anyhow!("nothing matched {needle:?}"));
    }
    grants.save()?;
    state.emit("revoked", serde_json::json!({ "revoked": revoked }));
    Ok(ResponseBody::PeerRevoked { revoked })
}

/// Redeem a parsed ticket: dial the grantor, present the token, and record
/// them as a peer. Returns the new peer info.
pub(crate) async fn redeem_ticket(
    state: &Arc<State>,
    ticket: Ticket,
    label: Option<String>,
) -> Result<PeerInfo> {
    let peer = PeerIn {
        node_id: ticket.id.clone(),
        label: label.or(ticket.label.clone()),
        relay: ticket.relay.clone(),
        ip: ticket.ip.clone(),
        allow_reshare: false, // learned from the redeem response
        added_at: unix_now(),
    };
    // symmetric: tell the grantor our address so they can reach our share too
    let (my_relay, my_ip) = dialable_addr(state).await;
    let conn = search_mod::connect(state, &peer, libfilestr::p2p::ALPN).await?;
    let mut reader = search_mod::request(
        &conn,
        &P2pRequest::Redeem { token: ticket.token.clone(), relay: my_relay, ip: my_ip },
    )
    .await?;
    let response = search_mod::read_response(&mut reader)
        .await?
        .ok_or_else(|| anyhow!("peer closed the stream without answering"))?;
    match response {
        P2pResponse::Redeemed { allow_reshare, view, .. } => {
            let peer = PeerIn { allow_reshare, ..peer };
            let info = PeerInfo {
                node_id: peer.node_id.clone(),
                label: peer.label.clone(),
                allow_reshare: peer.allow_reshare,
                added_at: peer.added_at,
                // we just connected to redeem the invite, so it's reachable
                reachable: Some(true),
            };
            let mut grants = state.grants.lock().await;
            grants.grants.upsert_peer(peer);
            // symmetric: allow the grantor to reach our full share in return
            grants.grants.allow(&ticket.id, VIEW_FULL.to_string(), true);
            grants.save()?;
            state.emit(
                "peer_added",
                serde_json::json!({ "node_id": info.node_id, "view": view }),
            );
            Ok(info)
        }
        P2pResponse::Error { code, message } => Err(anyhow!("peer refused: {code}: {message}")),
        other => Err(anyhow!("unexpected response: {other:?}")),
    }
}

async fn handle_peer_add(
    state: &Arc<State>,
    ticket: String,
    label: Option<String>,
) -> Result<ResponseBody> {
    let info = redeem_ticket(state, Ticket::parse(&ticket)?, label).await?;
    Ok(ResponseBody::PeerAdded { peer: info })
}

async fn handle_peer_list(state: &Arc<State>) -> Result<ResponseBody> {
    let (grant_infos, peers) = {
        let grants = state.grants.lock().await;
        (
            grants.grants.grants.iter().map(invite_info).collect::<Vec<_>>(),
            grants.grants.peers.clone(),
        )
    };
    // Probe each peer concurrently so the peers screen can show online/offline.
    // Bounded by search.connect_timeout_secs, run in parallel so the whole list
    // is never slower than a single probe.
    let mut probes = tokio::task::JoinSet::new();
    for peer in peers {
        let state = state.clone();
        probes.spawn(async move {
            let reachable =
                search_mod::connect(&state, &peer, libfilestr::p2p::ALPN).await.is_ok();
            PeerInfo {
                node_id: peer.node_id.clone(),
                label: peer.label.clone(),
                allow_reshare: peer.allow_reshare,
                added_at: peer.added_at,
                reachable: Some(reachable),
            }
        });
    }
    let mut peer_infos = Vec::new();
    while let Some(joined) = probes.join_next().await {
        if let Ok(info) = joined {
            peer_infos.push(info);
        }
    }
    // probe completion order is nondeterministic; present a stable order
    peer_infos.sort_by(|a, b| a.added_at.cmp(&b.added_at).then_with(|| a.node_id.cmp(&b.node_id)));
    Ok(ResponseBody::Peers { grants: grant_infos, peers: peer_infos })
}

async fn handle_share_list(state: &Arc<State>) -> Result<ResponseBody> {
    let config = state.config.read().await.clone();
    let index = state.index.read().await;
    let stats = index.root_stats();
    let shares = config
        .share
        .iter()
        .map(|s| {
            let (files, bytes) = stats.get(&s.name).copied().unwrap_or((0, 0));
            ShareInfo {
                name: s.name.clone(),
                path: libfilestr::paths::expand_path(&s.path),
                files,
                bytes,
            }
        })
        .collect();
    let mut views: Vec<ViewInfo> = vec![ViewInfo {
        name: VIEW_FULL.to_string(),
        roots: config.share.iter().map(|s| s.name.clone()).collect(),
    }];
    views.extend(
        config.view.iter().map(|(name, roots)| ViewInfo { name: name.clone(), roots: roots.clone() }),
    );
    Ok(ResponseBody::Shares { files: index.files.len(), shares, views })
}

async fn handle_share_add(
    state: &Arc<State>,
    path: PathBuf,
    name: Option<String>,
) -> Result<ResponseBody> {
    let dir = libfilestr::paths::expand_path(&path);
    if !dir.is_dir() {
        anyhow::bail!("not a directory: {}", dir.display());
    }
    let name = match name {
        Some(n) if !n.trim().is_empty() => n,
        _ => dir
            .file_name()
            .and_then(|n| n.to_str())
            .map(|s| s.to_string())
            .with_context(|| format!("cannot derive a name from {}; pass --name", dir.display()))?,
    };
    add_share_to_config(&state.config_path, &name, &dir)?;
    // Validate the edited config (cheap, no scan); roll the edit back if it's
    // rejected.
    if let Err(e) = crate::apply_config(state).await {
        let _ = remove_share_from_config(&state.config_path, &name);
        let _ = crate::apply_config(state).await;
        return Err(e.context("new share rejected; reverted config"));
    }
    // Index the new files in the BACKGROUND — hashing a big directory is slow,
    // so the command returns at once and the share's file count fills in as the
    // scan completes (parallel, at low CPU/IO priority — see priority.rs). The
    // scan is cancellable and its progress shows in `status`; `share rm` of this
    // share cancels it.
    crate::spawn_rescan(state, state.index.read().await.clone());
    state.emit("share_added", serde_json::json!({ "name": name, "path": dir }));
    handle_share_list(state).await
}

async fn handle_share_remove(state: &Arc<State>, name: String) -> Result<ResponseBody> {
    remove_share_from_config(&state.config_path, &name)?;
    crate::apply_config(state).await.context("apply config after removing share")?;
    // Removing only drops entries (everything else is reused), so this is cheap.
    crate::rescan_now(state).await.context("rescan after removing share")?;
    state.emit("share_removed", serde_json::json!({ "name": name }));
    handle_share_list(state).await
}

/// Append a `[[share]]` entry to the config file, preserving its formatting.
fn add_share_to_config(config_path: &std::path::Path, name: &str, dir: &std::path::Path) -> Result<()> {
    use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, value};
    let text = std::fs::read_to_string(config_path).unwrap_or_default();
    let mut doc: DocumentMut = text.parse().context("parsing config file")?;
    if doc.get("share").is_none() {
        doc["share"] = Item::ArrayOfTables(ArrayOfTables::new());
    }
    let arr = doc["share"]
        .as_array_of_tables_mut()
        .context("[[share]] in config is not an array of tables")?;
    if arr.iter().any(|t| t.get("name").and_then(|v| v.as_str()) == Some(name)) {
        anyhow::bail!("a share named {name:?} already exists");
    }
    let mut t = Table::new();
    t["name"] = value(name);
    t["path"] = value(dir.to_string_lossy().as_ref());
    arr.push(t);
    write_config_atomic(config_path, &doc.to_string())
}

/// Remove a `[[share]]` entry by name, plus any `[view]` references to it.
fn remove_share_from_config(config_path: &std::path::Path, name: &str) -> Result<()> {
    use toml_edit::DocumentMut;
    let text = std::fs::read_to_string(config_path)
        .with_context(|| format!("reading {}", config_path.display()))?;
    let mut doc: DocumentMut = text.parse().context("parsing config file")?;
    let arr = doc
        .get_mut("share")
        .and_then(|i| i.as_array_of_tables_mut())
        .context("no shares configured")?;
    let idx = arr
        .iter()
        .position(|t| t.get("name").and_then(|v| v.as_str()) == Some(name))
        .with_context(|| format!("no share named {name:?}"))?;
    arr.remove(idx);
    if let Some(views) = doc.get_mut("view").and_then(|i| i.as_table_mut()) {
        for (_, item) in views.iter_mut() {
            if let Some(a) = item.as_array_mut() {
                a.retain(|v| v.as_str() != Some(name));
            }
        }
    }
    write_config_atomic(config_path, &doc.to_string())
}

/// Write the config via a temp file + rename so a crash can't truncate it.
fn write_config_atomic(config_path: &std::path::Path, contents: &str) -> Result<()> {
    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let mut tmp = config_path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, contents).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, config_path)
        .with_context(|| format!("replacing {}", config_path.display()))?;
    Ok(())
}

async fn handle_rescan(state: &Arc<State>) -> Result<ResponseBody> {
    // rescan_now reuses the in-memory index and persists the refreshed cache.
    let files = crate::rescan_now(state).await?;
    Ok(ResponseBody::Rescanned { files })
}

async fn find_peer(state: &Arc<State>, needle: &str) -> Result<PeerIn> {
    let grants = state.grants.lock().await;
    let matches: Vec<&PeerIn> = grants
        .grants
        .peers
        .iter()
        .filter(|p| p.node_id.starts_with(needle) || p.label.as_deref() == Some(needle))
        .collect();
    match matches.len() {
        0 => Err(anyhow!("no peer matches {needle:?}")),
        1 => Ok(matches[0].clone()),
        n => Err(anyhow!("{n} peers match {needle:?}; be more specific")),
    }
}

async fn handle_browse(state: &Arc<State>, peer: String) -> Result<ResponseBody> {
    let peer = find_peer(state, &peer).await?;
    let conn = search_mod::connect(state, &peer, libfilestr::p2p::ALPN).await?;
    let mut reader = search_mod::request(&conn, &P2pRequest::List).await?;
    let mut entries: Vec<FileEntry> = Vec::new();
    loop {
        match search_mod::read_response(&mut reader).await? {
            Some(P2pResponse::Entries { entries: chunk }) => entries.extend(chunk),
            Some(P2pResponse::ListDone { .. }) | None => break,
            Some(P2pResponse::Error { code, message }) => {
                return Err(anyhow!("peer error {code}: {message}"));
            }
            Some(other) => tracing::debug!("unexpected list response: {other:?}"),
        }
    }
    // remember sources so `get <hash>` works after a browse
    {
        let mut recent = state.recent_sources.lock().await;
        for entry in &entries {
            recent.insert(
                &entry.hash,
                SourceRef { peer: peer.node_id.clone(), handle: None, size: entry.size },
            );
        }
    }
    Ok(ResponseBody::Entries { entries })
}

#[allow(clippy::too_many_arguments)]
async fn handle_get(
    state: &Arc<State>,
    write: &mut OwnedWriteHalf,
    id: u64,
    hash: String,
    out: PathBuf,
    peer_pref: Option<String>,
    range: Option<String>,
    background: bool,
) -> Result<()> {
    let response = match start_transfer(state, hash, out, peer_pref, range).await {
        Ok((tid, rx)) => {
            if background {
                ResponseBody::TransferStarted { id: tid }
            } else {
                // foreground: stream progress on this connection until the
                // transfer reaches a terminal state
                return stream_transfer(write, id, rx).await;
            }
        }
        Err(e) => ResponseBody::Error { message: format!("{e:#}") },
    };
    write_response(write, id, response).await
}

/// Validate args and hand off to the transfer manager.
async fn start_transfer(
    state: &Arc<State>,
    hash: String,
    out: PathBuf,
    peer_pref: Option<String>,
    range: Option<String>,
) -> Result<(u64, tokio::sync::watch::Receiver<libfilestr::ctl::TransferInfo>)> {
    if !out.is_absolute() {
        return Err(anyhow!("output path must be absolute"));
    }
    let range = match range {
        Some(s) => Some(search_mod::parse_range(&s)?),
        None => None,
    };
    transfers::start(state.clone(), hash, out, range, peer_pref).await
}

async fn stream_transfer(
    write: &mut OwnedWriteHalf,
    id: u64,
    mut rx: tokio::sync::watch::Receiver<libfilestr::ctl::TransferInfo>,
) -> Result<()> {
    loop {
        let info = rx.borrow_and_update().clone();
        match info.status.as_str() {
            "done" => {
                write_response(
                    write,
                    id,
                    ResponseBody::GetDone { path: info.out, hash: info.hash, size: info.total },
                )
                .await?;
                return Ok(());
            }
            "failed" | "cancelled" => {
                let message = info.error.unwrap_or_else(|| info.status.clone());
                write_response(write, id, ResponseBody::Error { message }).await?;
                return Ok(());
            }
            _ => {
                write_response(
                    write,
                    id,
                    ResponseBody::GetProgress {
                        transferred: info.transferred,
                        total: info.total,
                    },
                )
                .await?;
            }
        }
        if rx.changed().await.is_err() {
            // sender dropped without a terminal status
            return write_response(
                write,
                id,
                ResponseBody::Error { message: "transfer ended unexpectedly".into() },
            )
            .await;
        }
    }
}

async fn handle_transfers(state: &Arc<State>) -> Result<ResponseBody> {
    let transfers = state.transfers.lock().await.snapshot();
    Ok(ResponseBody::Transfers { transfers })
}

async fn handle_transfer_cancel(state: &Arc<State>, id: u64) -> Result<ResponseBody> {
    if state.transfers.lock().await.cancel(id) {
        Ok(ResponseBody::TransferCancelled { id })
    } else {
        Err(anyhow!("no transfer with id {id}"))
    }
}

async fn handle_reputation(state: &Arc<State>) -> Result<ResponseBody> {
    // longest half-life across config so decay is shown consistently
    let half_life = {
        let config = state.config.read().await;
        config.reputation.half_life_days * 24 * 3600
    };
    let entries = state.reputation.lock().await.store.all(half_life);
    let mut peers = Vec::with_capacity(entries.len());
    for (node_id, stats) in entries {
        let action = state.rep_action(&node_id).await;
        peers.push(libfilestr::ctl::PeerReputation {
            node_id,
            served: stats.served as u64,
            received: stats.received as u64,
            debt: stats.debt() as i64,
            action: match action {
                libfilestr::reputation::ServiceAction::Serve => "serve",
                libfilestr::reputation::ServiceAction::Deny => "deny",
            }
            .to_string(),
        });
    }
    Ok(ResponseBody::Reputation { peers })
}

// --- hub handlers (feature-gated; without `chat` they report unsupported) ---

#[cfg(feature = "chat")]
async fn handle_hub_create(state: &Arc<State>, name: String) -> Result<ResponseBody> {
    Ok(ResponseBody::HubCreated { hub: crate::chat::create(state, name).await? })
}
#[cfg(feature = "chat")]
async fn handle_hub_invite(state: &Arc<State>, hub: String) -> Result<ResponseBody> {
    Ok(ResponseBody::HubInvite { ticket: crate::chat::invite(state, hub).await? })
}
#[cfg(feature = "chat")]
async fn handle_hub_join(state: &Arc<State>, ticket: String) -> Result<ResponseBody> {
    let (hub, queued) = crate::chat::join(state, ticket).await?;
    Ok(ResponseBody::HubJoined { hub, queued })
}
#[cfg(feature = "chat")]
async fn handle_hub_request(
    state: &Arc<State>,
    address: Option<String>,
    hub: Option<String>,
    label: Option<String>,
) -> Result<ResponseBody> {
    let sent = address.is_some();
    let ticket = crate::chat::request(state, address, hub, label).await?;
    Ok(ResponseBody::HubRequestTicket { ticket, sent })
}
#[cfg(feature = "chat")]
async fn handle_hub_admit(
    state: &Arc<State>,
    ticket: String,
    hub: Option<String>,
) -> Result<ResponseBody> {
    Ok(ResponseBody::HubAdmitted { hub: crate::chat::admit(state, ticket, hub).await? })
}
#[cfg(feature = "chat")]
async fn handle_hub_address(state: &Arc<State>, hub: String) -> Result<ResponseBody> {
    Ok(ResponseBody::HubAddress { address: crate::chat::address(state, hub).await? })
}
#[cfg(feature = "chat")]
async fn handle_hub_pending(state: &Arc<State>) -> Result<ResponseBody> {
    let requests = crate::chat::pending(state)
        .await
        .into_iter()
        .map(|(from, ticket)| libfilestr::ctl::PendingRequest { from, ticket })
        .collect();
    Ok(ResponseBody::HubPending { requests })
}
#[cfg(feature = "chat")]
async fn handle_hub_list(state: &Arc<State>) -> Result<ResponseBody> {
    Ok(ResponseBody::Hubs { hubs: crate::chat::list(state).await? })
}
#[cfg(feature = "chat")]
async fn handle_hub_members(state: &Arc<State>, hub: String) -> Result<ResponseBody> {
    Ok(ResponseBody::HubMembers { members: crate::chat::members(state, hub).await? })
}
#[cfg(feature = "chat")]
async fn handle_hub_send(state: &Arc<State>, hub: String, text: String) -> Result<ResponseBody> {
    crate::chat::send(state, hub, text).await?;
    Ok(ResponseBody::HubSent)
}
#[cfg(feature = "chat")]
async fn handle_hub_log(state: &Arc<State>, hub: String) -> Result<ResponseBody> {
    Ok(ResponseBody::HubMessages { messages: crate::chat::log(state, hub).await? })
}

#[cfg(not(feature = "chat"))]
async fn handle_hub_create(_: &Arc<State>, _: String) -> Result<ResponseBody> {
    Err(anyhow!("chat plane not enabled in this build"))
}
#[cfg(not(feature = "chat"))]
async fn handle_hub_invite(_: &Arc<State>, _: String) -> Result<ResponseBody> {
    Err(anyhow!("chat plane not enabled in this build"))
}
#[cfg(not(feature = "chat"))]
async fn handle_hub_join(_: &Arc<State>, _: String) -> Result<ResponseBody> {
    Err(anyhow!("chat plane not enabled in this build"))
}
#[cfg(not(feature = "chat"))]
async fn handle_hub_request(
    _: &Arc<State>,
    _: Option<String>,
    _: Option<String>,
    _: Option<String>,
) -> Result<ResponseBody> {
    Err(anyhow!("chat plane not enabled in this build"))
}
#[cfg(not(feature = "chat"))]
async fn handle_hub_admit(_: &Arc<State>, _: String, _: Option<String>) -> Result<ResponseBody> {
    Err(anyhow!("chat plane not enabled in this build"))
}
#[cfg(not(feature = "chat"))]
async fn handle_hub_address(_: &Arc<State>, _: String) -> Result<ResponseBody> {
    Err(anyhow!("chat plane not enabled in this build"))
}
#[cfg(not(feature = "chat"))]
async fn handle_hub_pending(_: &Arc<State>) -> Result<ResponseBody> {
    Err(anyhow!("chat plane not enabled in this build"))
}
#[cfg(not(feature = "chat"))]
async fn handle_hub_list(_: &Arc<State>) -> Result<ResponseBody> {
    Err(anyhow!("chat plane not enabled in this build"))
}
#[cfg(not(feature = "chat"))]
async fn handle_hub_members(_: &Arc<State>, _: String) -> Result<ResponseBody> {
    Err(anyhow!("chat plane not enabled in this build"))
}
#[cfg(not(feature = "chat"))]
async fn handle_hub_send(_: &Arc<State>, _: String, _: String) -> Result<ResponseBody> {
    Err(anyhow!("chat plane not enabled in this build"))
}
#[cfg(not(feature = "chat"))]
async fn handle_hub_log(_: &Arc<State>, _: String) -> Result<ResponseBody> {
    Err(anyhow!("chat plane not enabled in this build"))
}

async fn handle_search(
    state: &Arc<State>,
    write: &mut OwnedWriteHalf,
    id: u64,
    query: String,
    ttl: Option<u8>,
) -> Result<()> {
    let config = state.config.read().await.clone();
    let ttl = ttl.unwrap_or(config.search.max_ttl).min(config.search.max_ttl);
    let query_id = search_mod::new_query_id();
    // record our own query id so a cycle coming back to us dies (§6)
    state.seen_queries.lock().await.check_and_insert(&query_id);

    let (tx, mut rx) = mpsc::channel::<search_mod::Hit>(64);
    let task = tokio::spawn(search_mod::run_search(
        state.clone(),
        query_id,
        query,
        ttl,
        Requester::Local,
        tx,
    ));

    let mut count = 0usize;
    while let Some(hit) = rx.recv().await {
        if count >= config.search.result_cap {
            break;
        }
        let (handle, via) = match hit.source {
            HitSource::Local => (String::new(), None),
            HitSource::Upstream { peer, handle } => {
                state.recent_sources.lock().await.insert(
                    &hit.hash,
                    SourceRef { peer: peer.clone(), handle: Some(handle.clone()), size: hit.size },
                );
                (handle, Some(peer))
            }
        };
        let body = ResponseBody::SearchHit {
            hit: SearchHit { name: hit.name, size: hit.size, hash: hit.hash, handle, via },
        };
        write_response(write, id, body).await?;
        count += 1;
    }
    task.abort();
    write_response(write, id, ResponseBody::SearchDone { hits: count }).await?;
    Ok(())
}

async fn handle_subscribe(
    state: &Arc<State>,
    write: &mut OwnedWriteHalf,
    id: u64,
) -> Result<()> {
    let mut rx = state.events.subscribe();
    write_response(write, id, ResponseBody::Subscribed).await?;
    loop {
        tokio::select! {
            _ = state.shutdown.cancelled() => break,
            event = rx.recv() => {
                match event {
                    Ok(event) => {
                        if write_response(write, id, ResponseBody::Event { event }).await.is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                }
            }
        }
    }
    Ok(())
}
