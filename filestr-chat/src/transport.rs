//! NIP-01 over a generic byte stream. The owner side ([`serve_relay`]) speaks
//! the relay half of NIP-01; the member side uses the small codec helpers to
//! drive its own read loop (it needs to feed events into MLS as they arrive).

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use nostr::filter::MatchEventOptions;
use nostr::message::{ClientMessage, RelayMessage, SubscriptionId};
use nostr::{Event, Filter, JsonUtil};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

use crate::relay::Relay;

async fn write_line<W: AsyncWrite + Unpin>(w: &mut W, msg: &RelayMessage<'_>) -> Result<()> {
    let mut line = msg.as_json();
    line.push('\n');
    w.write_all(line.as_bytes()).await?;
    Ok(())
}

/// Serve the relay side of NIP-01 to one connected client: store/broadcast
/// EVENTs, answer REQ with stored matches + EOSE, then forward live matching
/// events until the stream closes.
pub async fn serve_relay<R, W>(relay: Arc<Relay>, reader: R, mut writer: W) -> Result<()>
where
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    let mut lines = BufReader::new(reader).lines();
    let mut live = relay.subscribe();
    let mut subs: HashMap<String, Vec<Filter>> = HashMap::new();

    loop {
        tokio::select! {
            line = lines.next_line() => {
                let Some(line) = line? else { break };
                if line.trim().is_empty() { continue; }
                let value: serde_json::Value = match serde_json::from_str(&line) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                match ClientMessage::from_value(value) {
                    Ok(ClientMessage::Event(ev)) => {
                        let ev = ev.into_owned();
                        let id = ev.id;
                        relay.publish(ev);
                        write_line(&mut writer, &RelayMessage::Ok {
                            event_id: id,
                            status: true,
                            message: Cow::Borrowed(""),
                        }).await?;
                    }
                    Ok(ClientMessage::Req { subscription_id, filters }) => {
                        let sub = subscription_id.into_owned();
                        let filters: Vec<Filter> =
                            filters.into_iter().map(|f| f.into_owned()).collect();
                        for ev in relay.query(&filters) {
                            write_line(&mut writer, &RelayMessage::Event {
                                subscription_id: Cow::Owned(sub.clone()),
                                event: Cow::Owned(ev),
                            }).await?;
                        }
                        write_line(&mut writer, &RelayMessage::EndOfStoredEvents(
                            Cow::Owned(sub.clone()),
                        )).await?;
                        subs.insert(sub.to_string(), filters);
                    }
                    Ok(ClientMessage::Close(sub)) => {
                        subs.remove(sub.as_str());
                    }
                    _ => {}
                }
            }
            ev = live.recv() => {
                match ev {
                    Ok(ev) => {
                        for (sub, filters) in &subs {
                            if filters.iter().any(|f| f.match_event(&ev, MatchEventOptions::new())) {
                                write_line(&mut writer, &RelayMessage::Event {
                                    subscription_id: Cow::Owned(SubscriptionId::new(sub.clone())),
                                    event: Cow::Owned(ev.clone()),
                                }).await?;
                            }
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

// --- member-side codec helpers ---

/// A NIP-01 REQ line subscribing `sub` to `filters`.
pub fn encode_req(sub: &str, filters: Vec<Filter>) -> String {
    let filters: Vec<Cow<'_, Filter>> = filters.into_iter().map(Cow::Owned).collect();
    ClientMessage::Req { subscription_id: Cow::Owned(SubscriptionId::new(sub)), filters }
        .as_value()
        .to_string()
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
