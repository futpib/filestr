//! Shared daemon state.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use iroh::Endpoint;
use tokio_util::sync::CancellationToken;
use iroh_blobs::store::fs::FsStore;
use libfilestr::config::Config;
use libfilestr::ctl::Event;
use libfilestr::grants::Grants;

use crate::index::Index;

/// How long a minted handle stays resolvable without use (DESIGN.md §7.2).
const HANDLE_TTL: Duration = Duration::from_secs(3600);
/// LRU capacity for seen query ids / recent result sources.
const LRU_CAP: usize = 4096;

pub struct State {
    pub config_path: PathBuf,
    pub config: tokio::sync::RwLock<Config>,
    pub grants: tokio::sync::Mutex<GrantStore>,
    pub endpoint: Endpoint,
    pub store: FsStore,
    /// Directory holding cached thumbnails (cover art), keyed by content hash.
    pub thumbs_dir: PathBuf,
    /// Persisted index cache (path → hash + metadata) so a restart skips
    /// re-hashing unchanged files.
    pub index_path: PathBuf,
    pub index: tokio::sync::RwLock<Index>,
    /// Tracks the in-flight share scan: a cancel token and progress counters.
    pub scan: std::sync::Mutex<ScanCtl>,
    pub handles: tokio::sync::Mutex<Handles>,
    pub seen_queries: tokio::sync::Mutex<SeenQueries>,
    pub recent_sources: tokio::sync::Mutex<RecentSources>,
    pub transfers: tokio::sync::Mutex<crate::transfers::Transfers>,
    pub reputation: tokio::sync::Mutex<RepState>,
    /// The chat plane, or None when `[chat] enabled = false` (the node runs and
    /// peers files with no nostr).
    #[cfg(feature = "chat")]
    pub chat: Option<crate::chat::ChatState>,
    /// Hub tickets whose file-peering half was redeemed while chat was off; the
    /// MLS join completes when chat is enabled. Present regardless of `enabled`.
    #[cfg(feature = "chat")]
    pub pending_hubs: tokio::sync::Mutex<PendingHubs>,
    pub events: tokio::sync::broadcast::Sender<Event>,
    pub shutdown: tokio_util::sync::CancellationToken,
}

/// Cancellation + progress for the current/last share scan. Each scan installs
/// a fresh token and a fresh progress counter; an old (superseded) scan updates
/// its own counter, so reads always reflect the latest scan.
#[derive(Default)]
pub struct ScanCtl {
    pub cancel: Option<CancellationToken>,
    pub progress: Arc<ScanProgress>,
}

#[derive(Default)]
pub struct ScanProgress {
    pub active: AtomicBool,
    pub paused: AtomicBool,
    pub done: AtomicU64,
    pub total: AtomicU64,
    /// Notified when the scan is un-paused, to wake the paused launch loop.
    pub resume: tokio::sync::Notify,
}

impl State {
    pub fn emit(&self, event_type: &str, payload: serde_json::Value) {
        let _ = self.events.send(Event { event_type: event_type.to_string(), payload });
    }

    /// Begin a scan: cancel any in-flight scan, then install a fresh cancel
    /// token and progress counter. Returns them for the new scan to drive.
    pub fn begin_scan(&self) -> (CancellationToken, Arc<ScanProgress>) {
        let token = CancellationToken::new();
        let progress = Arc::new(ScanProgress::default());
        progress.active.store(true, Ordering::Relaxed);
        let mut ctl = self.scan.lock().unwrap();
        if let Some(old) = ctl.cancel.replace(token.clone()) {
            old.cancel();
        }
        ctl.progress = progress.clone();
        (token, progress)
    }

    /// Cancel the in-flight scan, if any.
    pub fn cancel_scan(&self) -> bool {
        let ctl = self.scan.lock().unwrap();
        match &ctl.cancel {
            Some(token) => {
                token.cancel();
                // wake a paused scan so it observes the cancel and unwinds
                ctl.progress.resume.notify_waiters();
                true
            }
            None => false,
        }
    }

    /// Pause the in-flight scan (if any and not already paused).
    pub fn pause_scan(&self) -> bool {
        let ctl = self.scan.lock().unwrap();
        let p = &ctl.progress;
        if p.active.load(Ordering::Relaxed) && !p.paused.load(Ordering::Relaxed) {
            p.paused.store(true, Ordering::Relaxed);
            true
        } else {
            false
        }
    }

    /// Resume a paused scan (if any).
    pub fn resume_scan(&self) -> bool {
        let ctl = self.scan.lock().unwrap();
        let p = &ctl.progress;
        if p.active.load(Ordering::Relaxed) && p.paused.load(Ordering::Relaxed) {
            p.paused.store(false, Ordering::Relaxed);
            p.resume.notify_waiters();
            true
        } else {
            false
        }
    }

    /// Snapshot scan progress (done, total, paused) if a scan is currently active.
    pub fn scan_progress(&self) -> Option<(u64, u64, bool)> {
        let p = self.scan.lock().unwrap().progress.clone();
        if p.active.load(Ordering::Relaxed) {
            Some((
                p.done.load(Ordering::Relaxed),
                p.total.load(Ordering::Relaxed),
                p.paused.load(Ordering::Relaxed),
            ))
        } else {
            None
        }
    }

    /// Borrow the chat plane, erroring if it's disabled.
    #[cfg(feature = "chat")]
    pub fn chat(&self) -> anyhow::Result<&crate::chat::ChatState> {
        self.chat
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("chat plane is disabled; set [chat] enabled = true and restart"))
    }

    /// Resolve the effective reputation policy for a peer (config + the peer's
    /// grant label for override matching).
    async fn rep_policy(&self, node_id: &str) -> libfilestr::reputation::Policy {
        let label = {
            let grants = self.grants.lock().await;
            grants.grants.active_for(node_id).and_then(|g| g.label.clone())
        };
        self.config.read().await.reputation_policy(node_id, label.as_deref())
    }

    /// Decide whether to serve content to a peer right now.
    pub async fn rep_action(&self, node_id: &str) -> libfilestr::reputation::ServiceAction {
        let policy = self.rep_policy(node_id).await;
        let stats = self.reputation.lock().await.store.stats(node_id, policy.half_life_secs);
        libfilestr::reputation::decide(Some(&stats), &policy)
    }

    pub async fn rep_record_served(&self, node_id: &str, bytes: u64) {
        let hl = self.rep_policy(node_id).await.half_life_secs;
        let mut rep = self.reputation.lock().await;
        rep.store.record_served(node_id, bytes, hl);
        if let Err(e) = rep.save() {
            tracing::warn!("saving reputation: {e}");
        }
    }

    pub async fn rep_record_received(&self, node_id: &str, bytes: u64) {
        let hl = self.rep_policy(node_id).await.half_life_secs;
        let mut rep = self.reputation.lock().await;
        rep.store.record_received(node_id, bytes, hl);
        if let Err(e) = rep.save() {
            tracing::warn!("saving reputation: {e}");
        }
    }
}

/// Hub tickets queued for an MLS join once chat is enabled (their file-peering
/// half was already redeemed). Persisted so a restart doesn't lose them.
#[cfg(feature = "chat")]
#[derive(Default)]
pub struct PendingHubs {
    pub tickets: Vec<String>,
    pub path: PathBuf,
}

#[cfg(feature = "chat")]
impl PendingHubs {
    pub fn load_or_default(path: PathBuf) -> Self {
        let tickets = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Self { tickets, path }
    }

    pub fn save(&self) {
        if let Ok(bytes) = serde_json::to_vec_pretty(&self.tickets) {
            let _ = std::fs::write(&self.path, bytes);
        }
    }
}

/// Reputation ledger plus its on-disk location.
pub struct RepState {
    pub store: libfilestr::reputation::RepStore,
    pub path: PathBuf,
}

impl RepState {
    pub fn save(&self) -> std::io::Result<()> {
        self.store.save(&self.path)
    }
}

/// Grants plus their on-disk location, so every mutation site can persist.
pub struct GrantStore {
    pub grants: Grants,
    pub path: PathBuf,
}

impl GrantStore {
    pub fn save(&self) -> std::io::Result<()> {
        self.grants.save(&self.path)
    }
}

#[derive(Debug, Clone)]
pub enum HandleTarget {
    /// Content in our own store; the fetcher names the hash in its get
    /// request, so no hash is stored here.
    Local,
    Remote { peer: String, upstream: String },
}

#[derive(Default)]
pub struct Handles {
    map: HashMap<String, (HandleTarget, Instant)>,
}

impl Handles {
    pub fn mint(&mut self, target: HandleTarget) -> String {
        if self.map.len() > LRU_CAP {
            let now = Instant::now();
            self.map.retain(|_, (_, expires)| *expires > now);
        }
        let handle = libfilestr::grants::new_token();
        self.map.insert(handle.clone(), (target, Instant::now() + HANDLE_TTL));
        handle
    }

    /// Resolve and refresh the expiry.
    pub fn resolve(&mut self, handle: &str) -> Option<HandleTarget> {
        let (target, expires) = self.map.get_mut(handle)?;
        if *expires <= Instant::now() {
            return None;
        }
        *expires = Instant::now() + HANDLE_TTL;
        Some(target.clone())
    }
}

/// Query-id LRU for search loop prevention (DESIGN.md §6).
#[derive(Default)]
pub struct SeenQueries {
    set: HashSet<String>,
    order: VecDeque<String>,
}

impl SeenQueries {
    /// Returns true if the query id is fresh (and records it).
    pub fn check_and_insert(&mut self, query_id: &str) -> bool {
        if self.set.contains(query_id) {
            return false;
        }
        self.set.insert(query_id.to_string());
        self.order.push_back(query_id.to_string());
        while self.order.len() > LRU_CAP {
            if let Some(old) = self.order.pop_front() {
                self.set.remove(&old);
            }
        }
        true
    }
}

/// Where recent local searches saw each hash, so `get <hash>` can pick a
/// source without re-searching.
#[derive(Default)]
pub struct RecentSources {
    map: HashMap<String, Vec<SourceRef>>,
    order: VecDeque<String>,
}

#[derive(Debug, Clone)]
pub struct SourceRef {
    pub peer: String,
    /// The upstream peer's handle for this content (None when the hash is in
    /// the peer's own browsable list).
    pub handle: Option<String>,
    /// Total size as reported by the source, for progress totals.
    pub size: u64,
}

impl RecentSources {
    pub fn insert(&mut self, hash: &str, source: SourceRef) {
        let entry = self.map.entry(hash.to_string()).or_insert_with(|| {
            self.order.push_back(hash.to_string());
            Vec::new()
        });
        if !entry.iter().any(|s| s.peer == source.peer && s.handle == source.handle) {
            entry.push(source);
        }
        while self.order.len() > LRU_CAP {
            if let Some(old) = self.order.pop_front() {
                self.map.remove(&old);
            }
        }
    }

    pub fn get(&self, hash: &str) -> Vec<SourceRef> {
        self.map.get(hash).cloned().unwrap_or_default()
    }
}
