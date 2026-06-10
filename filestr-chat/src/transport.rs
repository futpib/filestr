//! NIP-01 transport. The relay logic is shared between two carriers:
//! - the iroh `nostr` stream (newline-delimited, [`serve_relay`]), and
//! - a standard WebSocket ([`serve_relay_ws`] / [`ws_publish`] / [`ws_fetch`]),
//!   so a filestr node can expose its relay to, and use, ordinary nostr relays.
//!
//! Members drive their own read loops with the codec helpers, since incoming
//! events must be fed into MLS as they arrive.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use nostr::filter::MatchEventOptions;
use nostr::message::{ClientMessage, RelayMessage, SubscriptionId};
use nostr::{Event, Filter, JsonUtil};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;

use crate::relay::Relay;

type Subs = HashMap<String, Vec<Filter>>;

/// Handle one inbound NIP-01 client message, returning the response lines to
/// send back (OK / stored EVENTs + EOSE). Updates the subscription set.
fn handle_client_msg(relay: &Relay, subs: &mut Subs, line: &str) -> Vec<String> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        return Vec::new();
    };
    match ClientMessage::from_value(value) {
        Ok(ClientMessage::Event(ev)) => {
            let ev = ev.into_owned();
            let id = ev.id;
            relay.publish(ev);
            vec![
                RelayMessage::Ok { event_id: id, status: true, message: Cow::Borrowed("") }
                    .as_json(),
            ]
        }
        Ok(ClientMessage::Req { subscription_id, filters }) => {
            let sub = subscription_id.into_owned();
            let filters: Vec<Filter> = filters.into_iter().map(|f| f.into_owned()).collect();
            let mut out: Vec<String> = relay
                .query(&filters)
                .into_iter()
                .map(|ev| {
                    RelayMessage::Event {
                        subscription_id: Cow::Owned(sub.clone()),
                        event: Cow::Owned(ev),
                    }
                    .as_json()
                })
                .collect();
            out.push(RelayMessage::EndOfStoredEvents(Cow::Owned(sub.clone())).as_json());
            subs.insert(sub.to_string(), filters);
            out
        }
        Ok(ClientMessage::Close(sub)) => {
            subs.remove(sub.as_str());
            Vec::new()
        }
        _ => Vec::new(),
    }
}

/// Lines to push to a subscriber for a live event matching their filters.
fn live_to_lines(subs: &Subs, ev: &Event) -> Vec<String> {
    subs.iter()
        .filter(|(_, filters)| filters.iter().any(|f| f.match_event(ev, MatchEventOptions::new())))
        .map(|(sub, _)| {
            RelayMessage::Event {
                subscription_id: Cow::Owned(SubscriptionId::new(sub.clone())),
                event: Cow::Owned(ev.clone()),
            }
            .as_json()
        })
        .collect()
}

/// Serve the relay over a newline-delimited byte stream (the iroh `nostr`
/// stream).
pub async fn serve_relay<R, W>(relay: Arc<Relay>, reader: R, mut writer: W) -> Result<()>
where
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    let mut lines = BufReader::new(reader).lines();
    let mut live = relay.subscribe();
    let mut subs: Subs = HashMap::new();
    loop {
        tokio::select! {
            line = lines.next_line() => {
                let Some(line) = line? else { break };
                if line.trim().is_empty() { continue; }
                for resp in handle_client_msg(&relay, &mut subs, &line) {
                    writer.write_all(resp.as_bytes()).await?;
                    writer.write_all(b"\n").await?;
                }
            }
            ev = live.recv() => {
                match ev {
                    Ok(ev) => for resp in live_to_lines(&subs, &ev) {
                        writer.write_all(resp.as_bytes()).await?;
                        writer.write_all(b"\n").await?;
                    },
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                }
            }
        }
    }
    Ok(())
}

/// Accept a WebSocket handshake on an inbound TCP connection and serve the
/// relay over it (a standard NIP-01 relay endpoint).
pub async fn accept_ws(relay: Arc<Relay>, stream: TcpStream) -> Result<()> {
    let ws = tokio_tungstenite::accept_async(stream).await.context("ws handshake")?;
    serve_relay_ws(relay, ws).await
}

/// Serve the relay over a WebSocket — a standard NIP-01 relay endpoint.
pub async fn serve_relay_ws(relay: Arc<Relay>, ws: WebSocketStream<TcpStream>) -> Result<()> {
    let (mut tx, mut rx) = ws.split();
    let mut live = relay.subscribe();
    let mut subs: Subs = HashMap::new();
    loop {
        tokio::select! {
            msg = rx.next() => {
                let Some(msg) = msg else { break };
                let msg = msg.context("ws read")?;
                match msg {
                    Message::Text(text) => {
                        for resp in handle_client_msg(&relay, &mut subs, text.as_str()) {
                            tx.send(Message::text(resp)).await?;
                        }
                    }
                    Message::Ping(p) => tx.send(Message::Pong(p)).await?,
                    Message::Close(_) => break,
                    _ => {}
                }
            }
            ev = live.recv() => {
                match ev {
                    Ok(ev) => for resp in live_to_lines(&subs, &ev) {
                        tx.send(Message::text(resp)).await?;
                    },
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                }
            }
        }
    }
    Ok(())
}

// --- WebSocket client (talk to external nostr relays) ---

/// Publish an event to an external nostr relay over WebSocket.
pub async fn ws_publish(url: &str, event: Event) -> Result<()> {
    let (mut ws, _) = tokio_tungstenite::connect_async(url)
        .await
        .with_context(|| format!("connecting to {url}"))?;
    ws.send(Message::text(encode_event(event))).await?;
    // give the relay a moment to ack before closing so it isn't dropped
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), ws.next()).await;
    let _ = ws.close(None).await;
    Ok(())
}

/// Fetch stored events matching `filters` from an external nostr relay.
pub async fn ws_fetch(url: &str, filters: Vec<Filter>) -> Result<Vec<Event>> {
    let (mut ws, _) = tokio_tungstenite::connect_async(url)
        .await
        .with_context(|| format!("connecting to {url}"))?;
    ws.send(Message::text(encode_req("hub", filters))).await?;
    let mut events = Vec::new();
    let deadline = std::time::Duration::from_secs(15);
    loop {
        let msg = match tokio::time::timeout(deadline, ws.next()).await {
            Ok(Some(Ok(Message::Text(t)))) => t,
            Ok(Some(Ok(_))) => continue,
            _ => break,
        };
        match parse_relay(msg.as_str()) {
            RelayItem::Event(ev) => events.push(*ev),
            RelayItem::EndOfStored => break,
            RelayItem::Other => {}
        }
    }
    let _ = ws.close(None).await;
    Ok(events)
}

/// Long-lived subscription to an external nostr relay: REQ `filters`, then
/// forward every live event to `tx` until the stream closes or `tx` is
/// dropped. One connection lifetime — callers reconnect if they want.
pub async fn ws_subscribe(
    url: &str,
    filters: Vec<Filter>,
    tx: tokio::sync::mpsc::Sender<Event>,
) -> Result<()> {
    let (mut ws, _) = tokio_tungstenite::connect_async(url)
        .await
        .with_context(|| format!("connecting to {url}"))?;
    ws.send(Message::text(encode_req("sub", filters))).await?;
    while let Some(msg) = ws.next().await {
        let Ok(Message::Text(t)) = msg else { continue };
        if let RelayItem::Event(ev) = parse_relay(t.as_str())
            && tx.send(*ev).await.is_err()
        {
            break;
        }
    }
    Ok(())
}

// --- codec helpers (member-side read loops) ---

/// A NIP-01 REQ line subscribing `sub` to `filters`.
pub fn encode_req(sub: &str, filters: Vec<Filter>) -> String {
    let filters: Vec<Cow<'_, Filter>> = filters.into_iter().map(Cow::Owned).collect();
    ClientMessage::Req { subscription_id: Cow::Owned(SubscriptionId::new(sub)), filters }
        .as_json()
}

/// A NIP-01 EVENT line publishing `event`.
pub fn encode_event(event: Event) -> String {
    ClientMessage::Event(Cow::Owned(event)).as_json()
}

/// A relay→client message, reduced to what the member read loop cares about.
pub enum RelayItem {
    Event(Box<Event>),
    EndOfStored,
    Other,
}

pub fn parse_relay(line: &str) -> RelayItem {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        return RelayItem::Other;
    };
    match RelayMessage::from_value(value) {
        Ok(RelayMessage::Event { event, .. }) => RelayItem::Event(Box::new(event.into_owned())),
        Ok(RelayMessage::EndOfStoredEvents(_)) => RelayItem::EndOfStored,
        _ => RelayItem::Other,
    }
}
