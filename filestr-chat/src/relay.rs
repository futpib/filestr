//! A minimal in-memory NIP-01 relay: store events, answer filtered queries,
//! and broadcast live events to subscribers. The hub owner runs one of these;
//! members reach it over the iroh `nostr` stream (see [`crate::transport`]).

use nostr::filter::MatchEventOptions;
use nostr::{Event, Filter};
use tokio::sync::broadcast;

pub struct Relay {
    stored: std::sync::Mutex<Vec<Event>>,
    tx: broadcast::Sender<Event>,
}

impl Default for Relay {
    fn default() -> Self {
        Self::new()
    }
}

impl Relay {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(1024);
        Self { stored: std::sync::Mutex::new(Vec::new()), tx }
    }

    /// Store (deduplicating by id) and broadcast an event. Returns false if it
    /// was already known.
    pub fn publish(&self, event: Event) -> bool {
        {
            let mut stored = self.stored.lock().unwrap();
            if stored.iter().any(|e| e.id == event.id) {
                return false;
            }
            stored.push(event.clone());
        }
        let _ = self.tx.send(event);
        true
    }

    /// Stored events matching any of `filters`, oldest first.
    pub fn query(&self, filters: &[Filter]) -> Vec<Event> {
        let stored = self.stored.lock().unwrap();
        let mut out: Vec<Event> = stored
            .iter()
            .filter(|e| filters.iter().any(|f| f.match_event(e, MatchEventOptions::new())))
            .cloned()
            .collect();
        out.sort_by_key(|e| e.created_at.as_secs());
        out
    }

    /// Subscribe to the live event stream.
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.tx.subscribe()
    }
}
