//! Chat plane integration (feature `chat`): hubs are Marmot/MLS groups, the
//! owner hosts an embedded NIP-01 relay over the iroh `nostr` stream, and the
//! join flow reuses the file-sharing invite/redeem machinery in both
//! directions (owner→member so the joiner can reach the relay; member→owner
//! for share-to-join).

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use filestr_chat::mls::Mls;
use filestr_chat::ticket::HubTicket;
use filestr_chat::{Identity, Relay};
use libfilestr::ctl::{ChatMessage, HubInfo};
use libfilestr::grants::PeerIn;
use libfilestr::p2p::{self, P2pRequest, P2pResponse};
use libfilestr::ticket::Ticket;
use nostr::{Event, Filter, JsonUtil, Kind, RelayUrl, UnsignedEvent};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, BufReader};

use crate::ctl_server;
use crate::search;
use crate::state::State;

/// Synthetic relay url stamped into MLS group/key-package metadata. The real
/// transport is the iroh `nostr` stream, not this URL.
const SYNTH_RELAY: &str = "ws://filestr.invalid";
/// nostr kind of the MLS group-message wrapper (Marmot MIP-03).
const KIND_GROUP_MESSAGE: u16 = 445;

pub struct ChatState {
    pub mls: std::sync::Mutex<Mls>,
    pub relay: Arc<Relay>,
    pub hubs: tokio::sync::Mutex<HashMap<String, HubRecord>>,
    /// Where the hub registry (names, roles, how to reach owners) is persisted.
    hubs_path: std::path::PathBuf,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct HubRecord {
    pub name: String,
    pub owner: bool,
    /// How to reach the owner's relay (None when we are the owner).
    pub owner_peer: Option<PeerIn>,
}

impl ChatState {
    /// Open the chat state: the persistent encrypted MLS store plus the hub
    /// registry, both under the state dir.
    pub fn open(
        identity: Identity,
        mls_db: std::path::PathBuf,
        mls_key: [u8; 32],
        hubs_path: std::path::PathBuf,
    ) -> Result<Self> {
        let hubs: HashMap<String, HubRecord> = match std::fs::read(&hubs_path) {
            Ok(bytes) => serde_json::from_slice(&bytes).context("parsing hubs.json")?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
            Err(e) => return Err(e).context("reading hubs.json"),
        };
        let mls = Mls::open(identity.keys, &mls_db, mls_key)?;
        Ok(Self {
            mls: std::sync::Mutex::new(mls),
            relay: Arc::new(Relay::new()),
            hubs: tokio::sync::Mutex::new(hubs),
            hubs_path,
        })
    }

    /// Persist the hub registry (call after create/join).
    pub async fn save_hubs(&self) {
        let snapshot = self.hubs.lock().await.clone();
        let result = (|| -> Result<()> {
            if let Some(parent) = self.hubs_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let tmp = self.hubs_path.with_extension("json.tmp");
            std::fs::write(&tmp, serde_json::to_vec_pretty(&snapshot)?)?;
            std::fs::rename(&tmp, &self.hubs_path)?;
            Ok(())
        })();
        if let Err(e) = result {
            tracing::warn!("saving hubs.json: {e:#}");
        }
    }
}

/// Relay URLs to stamp into hub metadata: the configured external relays, or
/// the synthetic placeholder if none (the real transport is the iroh tunnel).
async fn hub_relays(state: &Arc<State>) -> Vec<RelayUrl> {
    let configured = state.config.read().await.chat.relays.clone();
    let parsed: Vec<RelayUrl> = configured.iter().filter_map(|u| RelayUrl::parse(u).ok()).collect();
    if parsed.is_empty() {
        vec![RelayUrl::parse(SYNTH_RELAY).expect("valid synthetic relay url")]
    } else {
        parsed
    }
}

/// External nostr relay URLs configured for this node.
async fn external_relays(state: &Arc<State>) -> Vec<String> {
    state.config.read().await.chat.relays.clone()
}

/// Spawn the optional WebSocket relay listener so the embedded relay is also
/// reachable as a standard NIP-01 relay. Called once at startup.
pub async fn spawn_relay_listener(state: Arc<State>) {
    let addr = { state.config.read().await.chat.relay_listen.clone() };
    let Some(addr) = addr else { return };
    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!("nostr relay listen on {addr} failed: {e}");
            return;
        }
    };
    tracing::info!("nostr relay (websocket) listening on {addr}");
    let relay = state.chat.relay.clone();
    let shutdown = state.shutdown.clone();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,
                accepted = listener.accept() => {
                    let Ok((stream, _)) = accepted else { continue };
                    let relay = relay.clone();
                    tokio::spawn(async move {
                        if let Err(e) = filestr_chat::transport::accept_ws(relay, stream).await {
                            tracing::debug!("ws relay connection ended: {e}");
                        }
                    });
                }
            }
        }
    });
}

fn message_filter() -> Filter {
    Filter::new().kind(Kind::Custom(KIND_GROUP_MESSAGE))
}

// --- hub control RPC carried over the opaque p2p `Hub` request ---

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum HubRpc {
    /// member → owner: a joiner asks the owner to add them with their MLS key
    /// package plus a reciprocal file invite (share-to-join). Used by the
    /// owner-initiated flow (the joiner redeemed a hub ticket first).
    Join { group_ref: String, key_package: String, reciprocal: String },
    /// owner → member: the owner admits a join request and pushes the MLS
    /// welcome plus an owner→member invite so the member can reach the relay.
    Welcome { group_ref: String, hub_name: String, welcome: String, owner_invite: String },
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum HubRpcReply {
    /// reply to `Join`: the MLS welcome for the joiner.
    Welcome { welcome: String },
    /// reply to `Welcome`: the member joined successfully.
    Ok,
    Error { message: String },
}

fn hub_info(record: &HubRecord, group_ref: &str, members: usize) -> HubInfo {
    HubInfo { group_ref: group_ref.to_string(), name: record.name.clone(), owner: record.owner, members }
}

async fn find_hub(state: &Arc<State>, needle: &str) -> Result<String> {
    let hubs = state.chat.hubs.lock().await;
    let matches: Vec<&String> = hubs
        .iter()
        .filter(|(gref, rec)| gref.starts_with(needle) || rec.name == needle)
        .map(|(gref, _)| gref)
        .collect();
    match matches.len() {
        0 => Err(anyhow!("no hub matches {needle:?}")),
        1 => Ok(matches[0].clone()),
        n => Err(anyhow!("{n} hubs match {needle:?}; be more specific")),
    }
}

// === ctl handlers ===

pub async fn create(state: &Arc<State>, name: String) -> Result<HubInfo> {
    let relays = hub_relays(state).await;
    let group_ref = {
        let mls = state.chat.mls.lock().unwrap();
        mls.create_group(&name, &relays)?
    };
    let record = HubRecord { name, owner: true, owner_peer: None };
    let info = hub_info(&record, &group_ref, 1);
    state.chat.hubs.lock().await.insert(group_ref.clone(), record);
    state.chat.save_hubs().await;
    state.emit("hub_created", serde_json::json!({ "group_ref": group_ref }));
    Ok(info)
}

pub async fn invite(state: &Arc<State>, hub: String) -> Result<String> {
    let group_ref = find_hub(state, &hub).await?;
    let name = {
        let hubs = state.chat.hubs.lock().await;
        let rec = hubs.get(&group_ref).ok_or_else(|| anyhow!("hub gone"))?;
        if !rec.owner {
            return Err(anyhow!("only the hub owner can invite"));
        }
        rec.name.clone()
    };
    // owner→member grant so the joiner can reach our relay / hub RPC
    let (invite, _token_id) =
        ctl_server::mint_invite(state, None, Some(format!("hub:{name}")), None, None).await?;
    let ticket = HubTicket { v: 0, invite, name, group_ref };
    Ok(ticket.encode())
}

pub async fn join(state: &Arc<State>, ticket_str: String) -> Result<HubInfo> {
    let ticket = HubTicket::parse(&ticket_str)?;
    let owner_peer = PeerIn {
        node_id: ticket.invite.id.clone(),
        label: Some(format!("hub:{}", ticket.name)),
        relay: ticket.invite.relay.clone(),
        ip: ticket.invite.ip.clone(),
        allow_reshare: false,
        added_at: libfilestr::unix_now(),
    };

    // 1. redeem the owner's invite: now the owner allows us to connect, and we
    //    record the owner as a file peer (we can browse them too).
    ctl_server::redeem_ticket(state, ticket.invite.clone(), Some(format!("hub:{}", ticket.name)))
        .await
        .context("redeeming hub invite")?;

    // 2. our MLS key package + a reciprocal file invite (share-to-join).
    let relays = hub_relays(state).await;
    let key_package = {
        let mls = state.chat.mls.lock().unwrap();
        mls.key_package_event(&relays)?.as_json()
    };
    let (reciprocal, _) =
        ctl_server::mint_invite(state, None, Some(format!("hub:{}", ticket.name)), None, None)
            .await?;

    // 3. ask the owner to admit us.
    let rpc = HubRpc::Join {
        group_ref: ticket.group_ref.clone(),
        key_package,
        reciprocal: serde_json::to_string(&reciprocal)?,
    };
    let reply = hub_rpc(state, &owner_peer, &rpc).await?;
    let welcome_json = match reply {
        HubRpcReply::Welcome { welcome } => welcome,
        HubRpcReply::Error { message } => return Err(anyhow!("owner refused join: {message}")),
        HubRpcReply::Ok => return Err(anyhow!("owner returned no welcome")),
    };

    // 4. join the MLS group from the welcome.
    let welcome: UnsignedEvent =
        UnsignedEvent::from_json(welcome_json.as_bytes()).context("parse welcome")?;
    let group_ref = {
        let mls = state.chat.mls.lock().unwrap();
        mls.join_from_welcome(&welcome)?
    };

    let record = HubRecord { name: ticket.name, owner: false, owner_peer: Some(owner_peer) };
    let members = members_count(state, &group_ref);
    let info = hub_info(&record, &group_ref, members);
    state.chat.hubs.lock().await.insert(group_ref.clone(), record);
    state.chat.save_hubs().await;
    state.emit("hub_joined", serde_json::json!({ "group_ref": group_ref }));
    Ok(info)
}

/// Member side: produce a self-contained join-request ticket (`filestrreq1…`)
/// the owner can `admit` — works pasted out-of-band or sent over nostr.
pub async fn request(
    state: &Arc<State>,
    hub: Option<String>,
    label: Option<String>,
) -> Result<String> {
    let relays = hub_relays(state).await;
    let key_package = {
        let mls = state.chat.mls.lock().unwrap();
        mls.key_package_event(&relays)?.as_json()
    };
    // a reciprocal invite the owner redeems: dial-back + share-to-join
    let (reciprocal, _) = ctl_server::mint_invite(
        state,
        None,
        label.clone().or_else(|| Some("hub-request".to_string())),
        None,
        None,
    )
    .await?;
    let ticket = filestr_chat::ticket::RequestTicket { v: 0, reciprocal, key_package, hub, label };
    Ok(ticket.encode())
}

/// Owner side: admit a join-request ticket — add the requester to the hub and
/// push them the welcome (which also hands them an owner→member invite).
pub async fn admit(
    state: &Arc<State>,
    ticket_str: String,
    hub_override: Option<String>,
) -> Result<HubInfo> {
    let req = filestr_chat::ticket::RequestTicket::parse(&ticket_str)?;
    let group_ref = resolve_owned_hub(state, req.hub.clone().or(hub_override)).await?;

    // add to MLS (+ redeem the reciprocal for share-to-join)
    let reciprocal_json = serde_json::to_string(&req.reciprocal)?;
    let welcome = add_member(state, &group_ref, &req.key_package, Some(&reciprocal_json)).await?;

    // reach the requester (now allows us) to push the welcome + an owner invite
    let member_peer = PeerIn {
        node_id: req.reciprocal.id.clone(),
        label: Some("hub-member".to_string()),
        relay: req.reciprocal.relay.clone(),
        ip: req.reciprocal.ip.clone(),
        allow_reshare: false,
        added_at: libfilestr::unix_now(),
    };
    let hub_name = {
        let hubs = state.chat.hubs.lock().await;
        hubs.get(&group_ref).map(|r| r.name.clone()).unwrap_or_default()
    };
    let (owner_invite, _) =
        ctl_server::mint_invite(state, None, Some(format!("hub:{hub_name}")), None, None).await?;
    let rpc = HubRpc::Welcome {
        group_ref: group_ref.clone(),
        hub_name,
        welcome,
        owner_invite: serde_json::to_string(&owner_invite)?,
    };
    match hub_rpc(state, &member_peer, &rpc).await? {
        HubRpcReply::Ok => {}
        HubRpcReply::Error { message } => return Err(anyhow!("member failed to join: {message}")),
        other => return Err(anyhow!("unexpected admit reply: {}", serde_json::to_string(&other)?)),
    }

    let members = members_count(state, &group_ref);
    let hubs = state.chat.hubs.lock().await;
    hubs.get(&group_ref)
        .map(|r| hub_info(r, &group_ref, members))
        .ok_or_else(|| anyhow!("hub gone"))
}

/// Resolve which owned hub to admit into: the hint (group-ref prefix or name),
/// or the sole owned hub if there's exactly one.
async fn resolve_owned_hub(state: &Arc<State>, hint: Option<String>) -> Result<String> {
    let hubs = state.chat.hubs.lock().await;
    let owned: Vec<(&String, &HubRecord)> = hubs.iter().filter(|(_, r)| r.owner).collect();
    match hint {
        Some(h) => owned
            .iter()
            .find(|(g, r)| g.starts_with(&h) || r.name == h)
            .map(|(g, _)| (*g).clone())
            .ok_or_else(|| anyhow!("you don't own a hub matching {h:?}")),
        None => match owned.len() {
            1 => Ok(owned[0].0.clone()),
            0 => Err(anyhow!("you own no hubs to admit into")),
            n => Err(anyhow!("you own {n} hubs; pass --hub to choose")),
        },
    }
}

pub async fn list(state: &Arc<State>) -> Result<Vec<HubInfo>> {
    let hubs = state.chat.hubs.lock().await;
    let mut out: Vec<HubInfo> = hubs
        .iter()
        .map(|(gref, rec)| hub_info(rec, gref, members_count(state, gref)))
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

pub async fn members(state: &Arc<State>, hub: String) -> Result<Vec<String>> {
    let group_ref = find_hub(state, &hub).await?;
    let mls = state.chat.mls.lock().unwrap();
    mls.members(&group_ref)
}

fn members_count(state: &Arc<State>, group_ref: &str) -> usize {
    state.chat.mls.lock().unwrap().members(group_ref).map(|m| m.len()).unwrap_or(0)
}

pub async fn send(state: &Arc<State>, hub: String, text: String) -> Result<()> {
    let group_ref = find_hub(state, &hub).await?;
    let (event, owner_peer) = {
        let owner_peer = {
            let hubs = state.chat.hubs.lock().await;
            hubs.get(&group_ref).and_then(|r| r.owner_peer.clone())
        };
        let event = {
            let mls = state.chat.mls.lock().unwrap();
            mls.create_message(&group_ref, &text)?
        };
        (event, owner_peer)
    };
    match &owner_peer {
        None => {
            // we host the relay
            state.chat.relay.publish(event.clone());
        }
        Some(owner) => publish_to_owner(state, owner, event.clone()).await?,
    }
    // also publish to any configured external nostr relays
    for url in external_relays(state).await {
        if let Err(e) = filestr_chat::transport::ws_publish(&url, event.clone()).await {
            tracing::debug!("publish to {url} failed: {e:#}");
        }
    }
    state.emit("hub_sent", serde_json::json!({ "group_ref": group_ref }));
    Ok(())
}

pub async fn log(state: &Arc<State>, hub: String) -> Result<Vec<ChatMessage>> {
    let group_ref = find_hub(state, &hub).await?;
    let owner_peer = {
        let hubs = state.chat.hubs.lock().await;
        hubs.get(&group_ref).and_then(|r| r.owner_peer.clone())
    };

    // gather group-message events from whichever relay we can reach; a
    // failure here (e.g. the owner is offline) must not stop us returning the
    // history already stored locally
    let mut events = match &owner_peer {
        None => state.chat.relay.query(&[message_filter()]),
        Some(owner) => fetch_from_owner(state, owner).await.unwrap_or_else(|e| {
            tracing::debug!("fetch from owner failed: {e:#}");
            Vec::new()
        }),
    };
    // plus any configured external nostr relays
    for url in external_relays(state).await {
        match filestr_chat::transport::ws_fetch(&url, vec![message_filter()]).await {
            Ok(mut more) => events.append(&mut more),
            Err(e) => tracing::debug!("fetch from {url} failed: {e:#}"),
        }
    }
    // advance MLS state with anything new (own/duplicate events are ignored)
    {
        let mls = state.chat.mls.lock().unwrap();
        for ev in &events {
            let _ = mls.process(ev);
        }
    }
    let messages = {
        let mls = state.chat.mls.lock().unwrap();
        mls.get_messages(&group_ref)?
    };
    Ok(messages
        .into_iter()
        .map(|m| ChatMessage { author: m.author, content: m.content, created_at: m.created_at })
        .collect())
}

// === owner-side p2p Hub RPC handler ===

pub async fn handle_hub_rpc(state: &Arc<State>, payload: &str) -> String {
    let reply = match serde_json::from_str::<HubRpc>(payload) {
        Ok(HubRpc::Join { group_ref, key_package, reciprocal }) => {
            match add_member(state, &group_ref, &key_package, Some(&reciprocal)).await {
                Ok(welcome) => HubRpcReply::Welcome { welcome },
                Err(e) => HubRpcReply::Error { message: format!("{e:#}") },
            }
        }
        Ok(HubRpc::Welcome { group_ref, hub_name, welcome, owner_invite }) => {
            match handle_welcome(state, group_ref, hub_name, welcome, owner_invite).await {
                Ok(()) => HubRpcReply::Ok,
                Err(e) => HubRpcReply::Error { message: format!("{e:#}") },
            }
        }
        Err(e) => HubRpcReply::Error { message: format!("bad hub rpc: {e}") },
    };
    serde_json::to_string(&reply).unwrap_or_else(|_| "{\"type\":\"error\"}".into())
}

/// Owner side: add a member to one of our hubs from their key package,
/// optionally redeeming a reciprocal share-to-join invite. Returns the MLS
/// welcome (JSON).
async fn add_member(
    state: &Arc<State>,
    group_ref: &str,
    key_package: &str,
    reciprocal: Option<&str>,
) -> Result<String> {
    {
        let hubs = state.chat.hubs.lock().await;
        let rec = hubs.get(group_ref).ok_or_else(|| anyhow!("unknown hub"))?;
        if !rec.owner {
            return Err(anyhow!("not the owner of this hub"));
        }
    }
    if let Some(reciprocal) = reciprocal {
        // share-to-join: redeem the joiner's reciprocal invite so we can browse them
        let reciprocal: Ticket =
            serde_json::from_str(reciprocal).context("parse reciprocal ticket")?;
        if let Err(e) =
            ctl_server::redeem_ticket(state, reciprocal, Some("hub-member".to_string())).await
        {
            tracing::warn!("share-to-join redeem failed: {e:#}");
        }
    }
    let key_package: Event =
        Event::from_json(key_package.as_bytes()).context("parse key package")?;
    let (welcome, evolution) = {
        let mls = state.chat.mls.lock().unwrap();
        mls.add_member(group_ref, &key_package)?
    };
    state.chat.relay.publish(evolution);
    state.emit("hub_member_added", serde_json::json!({ "group_ref": group_ref }));
    Ok(welcome.as_json())
}

/// Member side: handle an owner pushing us a welcome after admitting our
/// request — join the MLS group and redeem the owner→member invite so we can
/// reach the relay.
async fn handle_welcome(
    state: &Arc<State>,
    _group_ref: String,
    hub_name: String,
    welcome: String,
    owner_invite: String,
) -> Result<()> {
    let welcome: UnsignedEvent =
        UnsignedEvent::from_json(welcome.as_bytes()).context("parse welcome")?;
    let joined = {
        let mls = state.chat.mls.lock().unwrap();
        mls.join_from_welcome(&welcome)?
    };
    let owner_ticket: Ticket =
        serde_json::from_str(&owner_invite).context("parse owner invite")?;
    let owner_peer = PeerIn {
        node_id: owner_ticket.id.clone(),
        label: Some(format!("hub:{hub_name}")),
        relay: owner_ticket.relay.clone(),
        ip: owner_ticket.ip.clone(),
        allow_reshare: false,
        added_at: libfilestr::unix_now(),
    };
    ctl_server::redeem_ticket(state, owner_ticket, Some(format!("hub:{hub_name}")))
        .await
        .context("redeeming owner invite")?;
    let record = HubRecord { name: hub_name, owner: false, owner_peer: Some(owner_peer) };
    state.chat.hubs.lock().await.insert(joined.clone(), record);
    state.chat.save_hubs().await;
    state.emit("hub_joined", serde_json::json!({ "group_ref": joined }));
    Ok(())
}

// === nostr-over-iroh: serve our relay on the `nostr` stream ===

pub async fn serve_nostr(
    state: &Arc<State>,
    recv: iroh::endpoint::RecvStream,
    send: iroh::endpoint::SendStream,
) -> Result<()> {
    if !state.config.read().await.chat.embedded_relay {
        return Ok(()); // embedded relay disabled; peers must use external relays
    }
    filestr_chat::transport::serve_relay(state.chat.relay.clone(), recv, send).await
}

// === member→owner relay client over the `nostr` stream ===

async fn open_nostr(
    state: &Arc<State>,
    owner: &PeerIn,
) -> Result<(iroh::endpoint::SendStream, iroh::endpoint::RecvStream)> {
    let conn = search::connect(state, owner, p2p::ALPN).await?;
    let (mut send, recv) = conn.open_bi().await.context("open_bi")?;
    send.write_all(p2p::encode(&P2pRequest::Nostr).as_bytes()).await?;
    Ok((send, recv))
}

async fn publish_to_owner(state: &Arc<State>, owner: &PeerIn, event: Event) -> Result<()> {
    let (mut send, recv) = open_nostr(state, owner).await?;
    send.write_all(filestr_chat::transport::encode_event(event).as_bytes()).await?;
    send.write_all(b"\n").await?;
    // wait for the relay's OK before closing so the event isn't dropped
    let mut lines = BufReader::new(recv).lines();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(10), lines.next_line()).await;
    send.finish().ok();
    Ok(())
}

async fn fetch_from_owner(state: &Arc<State>, owner: &PeerIn) -> Result<Vec<Event>> {
    use filestr_chat::transport::{RelayItem, encode_req, parse_relay};
    let (mut send, recv) = open_nostr(state, owner).await?;
    send.write_all(encode_req("hub", vec![message_filter()]).as_bytes()).await?;
    send.write_all(b"\n").await?;

    let mut lines = BufReader::new(recv).lines();
    let mut events = Vec::new();
    let deadline = std::time::Duration::from_secs(15);
    loop {
        let line = match tokio::time::timeout(deadline, lines.next_line()).await {
            Ok(Ok(Some(line))) => line,
            _ => break,
        };
        match parse_relay(&line) {
            RelayItem::Event(ev) => events.push(*ev),
            RelayItem::EndOfStored => break,
            RelayItem::Other => {}
        }
    }
    send.finish().ok();
    Ok(events)
}

// === hub RPC client (member→owner over the opaque `Hub` request) ===

async fn hub_rpc(state: &Arc<State>, owner: &PeerIn, rpc: &HubRpc) -> Result<HubRpcReply> {
    let conn = search::connect(state, owner, p2p::ALPN).await?;
    let payload = serde_json::to_string(rpc)?;
    let mut reader = search::request(&conn, &P2pRequest::Hub { payload }).await?;
    match search::read_response(&mut reader).await? {
        Some(P2pResponse::HubReply { payload }) => {
            Ok(serde_json::from_str(&payload).context("parse hub reply")?)
        }
        Some(P2pResponse::Error { code, message }) => Err(anyhow!("owner error {code}: {message}")),
        other => Err(anyhow!("unexpected hub response: {other:?}")),
    }
}
