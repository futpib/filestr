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
}

pub struct HubRecord {
    pub name: String,
    pub owner: bool,
    /// How to reach the owner's relay (None when we are the owner).
    pub owner_peer: Option<PeerIn>,
}

impl ChatState {
    pub fn new(identity: Identity) -> Self {
        Self {
            mls: std::sync::Mutex::new(Mls::new(identity.keys)),
            relay: Arc::new(Relay::new()),
            hubs: tokio::sync::Mutex::new(HashMap::new()),
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
    /// A joiner asks the owner to add them: their MLS key package plus a
    /// reciprocal file invite (share-to-join).
    Join { group_ref: String, key_package: String, reciprocal: String },
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum HubRpcReply {
    Welcome { welcome: String },
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
    state.emit("hub_joined", serde_json::json!({ "group_ref": group_ref }));
    Ok(info)
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

    // gather group-message events from whichever relay we can reach
    let mut events = match &owner_peer {
        None => state.chat.relay.query(&[message_filter()]),
        Some(owner) => fetch_from_owner(state, owner).await?,
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
        Ok(rpc) => match handle_join(state, rpc).await {
            Ok(welcome) => HubRpcReply::Welcome { welcome },
            Err(e) => HubRpcReply::Error { message: format!("{e:#}") },
        },
        Err(e) => HubRpcReply::Error { message: format!("bad hub rpc: {e}") },
    };
    serde_json::to_string(&reply).unwrap_or_else(|_| "{\"type\":\"error\"}".into())
}

async fn handle_join(state: &Arc<State>, rpc: HubRpc) -> Result<String> {
    let HubRpc::Join { group_ref, key_package, reciprocal } = rpc;

    // we must own this hub
    {
        let hubs = state.chat.hubs.lock().await;
        let rec = hubs.get(&group_ref).ok_or_else(|| anyhow!("unknown hub"))?;
        if !rec.owner {
            return Err(anyhow!("not the owner of this hub"));
        }
    }

    // share-to-join: redeem the joiner's reciprocal invite so we can browse them
    let reciprocal: Ticket =
        serde_json::from_str(&reciprocal).context("parse reciprocal ticket")?;
    if let Err(e) = ctl_server::redeem_ticket(state, reciprocal, Some("hub-member".to_string())).await {
        tracing::warn!("share-to-join redeem failed: {e:#}");
    }

    // add the member to the MLS group
    let key_package: Event = Event::from_json(key_package.as_bytes()).context("parse key package")?;
    let (welcome, evolution) = {
        let mls = state.chat.mls.lock().unwrap();
        mls.add_member(&group_ref, &key_package)?
    };
    // publish the commit so other members advance when they next sync
    state.chat.relay.publish(evolution);
    state.emit("hub_member_added", serde_json::json!({ "group_ref": group_ref }));
    Ok(welcome.as_json())
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
