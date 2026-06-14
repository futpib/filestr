//! Per-neighbour reputation: a decaying reciprocity ledger plus a policy that
//! decides whether to keep serving a peer.
//!
//! Reputation is **local and first-hand only** — we score the direct
//! neighbour we actually transact with, never a global identity (which we
//! couldn't build anyway: attribution-hiding means a relay's downstream never
//! learns the origin). Each edge of the grant graph is its own repeated game.
//!
//! Cheating-by-corruption is already impossible (BLAKE3 verifies every byte),
//! so the residual game is free-riding. We track verified bytes served vs
//! received per neighbour; once a peer's debt exceeds a configurable credit
//! limit we stop serving them until they reciprocate. Counters decay with a
//! half-life so old behaviour fades (no permanent grudges, no coasting).

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::unix_now;

/// What to do for a peer at a given moment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceAction {
    /// Serve normally.
    Serve,
    /// Refuse to serve content (free-riding past the credit limit). Cheap
    /// requests like search still go through.
    Deny,
}

/// What happens when a peer exceeds its allowance. (Throttle/SearchOnly are
/// future refinements.)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum OverLimit {
    /// Stop serving content until they reciprocate.
    #[default]
    Deny,
    /// Disable enforcement — always serve.
    Serve,
}

/// Resolved policy for one peer (base config merged with any override).
#[derive(Debug, Clone, Copy)]
pub struct Policy {
    pub enabled: bool,
    /// Bytes of debt (served − received) tolerated before acting.
    pub credit_limit: u64,
    /// Extra free allowance for a peer that has never given anything yet, so
    /// cooperation can bootstrap.
    pub newcomer_budget: u64,
    pub half_life_secs: u64,
    pub over_limit: OverLimit,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            enabled: true,
            credit_limit: 256 * 1024 * 1024,
            newcomer_budget: 64 * 1024 * 1024,
            half_life_secs: 7 * 24 * 3600,
            over_limit: OverLimit::Deny,
        }
    }
}

/// Decaying counters for one neighbour.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PeerStats {
    /// Verified bytes we have served to them.
    pub served: f64,
    /// Verified bytes they have served to us.
    pub received: f64,
    pub updated_at: u64,
}

impl PeerStats {
    fn decay(&mut self, now: u64, half_life_secs: u64) {
        if half_life_secs == 0 || self.updated_at == 0 || now <= self.updated_at {
            self.updated_at = now;
            return;
        }
        let dt = (now - self.updated_at) as f64;
        let factor = 0.5_f64.powf(dt / half_life_secs as f64);
        self.served *= factor;
        self.received *= factor;
        self.updated_at = now;
    }

    /// Outstanding debt they owe us (negative if they're a net creditor).
    pub fn debt(&self) -> f64 {
        self.served - self.received
    }
}

/// Decide whether to serve, given the peer's (decayed) stats and policy.
pub fn decide(stats: Option<&PeerStats>, policy: &Policy) -> ServiceAction {
    if !policy.enabled || policy.over_limit == OverLimit::Serve {
        return ServiceAction::Serve;
    }
    let debt = stats.map(|s| s.debt()).unwrap_or(0.0);
    let allowance = (policy.credit_limit + policy.newcomer_budget) as f64;
    if debt > allowance { ServiceAction::Deny } else { ServiceAction::Serve }
}

/// Persistent per-neighbour ledger.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RepStore {
    peers: HashMap<String, PeerStats>,
}

impl RepStore {
    pub fn load_or_default(path: &Path) -> std::io::Result<Self> {
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e),
        }
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&tmp, path)
    }

    /// Current (decayed) stats for a peer.
    pub fn stats(&self, node_id: &str, half_life_secs: u64) -> PeerStats {
        let mut s = self.peers.get(node_id).cloned().unwrap_or_default();
        s.decay(unix_now(), half_life_secs);
        s
    }

    pub fn record_served(&mut self, node_id: &str, bytes: u64, half_life_secs: u64) {
        let now = unix_now();
        let s = self.peers.entry(node_id.to_string()).or_default();
        s.decay(now, half_life_secs);
        s.served += bytes as f64;
    }

    pub fn record_received(&mut self, node_id: &str, bytes: u64, half_life_secs: u64) {
        let now = unix_now();
        let s = self.peers.entry(node_id.to_string()).or_default();
        s.decay(now, half_life_secs);
        s.received += bytes as f64;
    }

    /// (node_id, decayed stats) for every tracked peer.
    pub fn all(&self, half_life_secs: u64) -> Vec<(String, PeerStats)> {
        let now = unix_now();
        let mut out: Vec<(String, PeerStats)> = self
            .peers
            .iter()
            .map(|(k, v)| {
                let mut s = v.clone();
                s.decay(now, half_life_secs);
                (k.clone(), s)
            })
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(limit: u64, budget: u64) -> Policy {
        Policy { credit_limit: limit, newcomer_budget: budget, ..Policy::default() }
    }

    #[test]
    fn free_rider_is_denied_past_allowance() {
        let mut store = RepStore::default();
        let p = policy(1000, 0);
        let hl = p.half_life_secs;
        // serve under the limit -> still serve
        store.record_served("leech", 800, hl);
        assert_eq!(decide(Some(&store.stats("leech", hl)), &p), ServiceAction::Serve);
        // cross the limit -> deny
        store.record_served("leech", 400, hl);
        assert_eq!(decide(Some(&store.stats("leech", hl)), &p), ServiceAction::Deny);
    }

    #[test]
    fn reciprocation_restores_service() {
        let mut store = RepStore::default();
        let p = policy(1000, 0);
        let hl = p.half_life_secs;
        store.record_served("peer", 1500, hl);
        assert_eq!(decide(Some(&store.stats("peer", hl)), &p), ServiceAction::Deny);
        // they give back -> debt drops below the limit
        store.record_received("peer", 1000, hl);
        assert_eq!(decide(Some(&store.stats("peer", hl)), &p), ServiceAction::Serve);
    }

    #[test]
    fn newcomer_budget_bootstraps() {
        let p = policy(0, 500);
        // brand-new peer (no stats) is served thanks to the budget
        assert_eq!(decide(None, &p), ServiceAction::Serve);
    }

    #[test]
    fn disabled_always_serves() {
        let mut store = RepStore::default();
        let p = Policy { over_limit: OverLimit::Serve, ..policy(0, 0) };
        store.record_served("x", 1_000_000, p.half_life_secs);
        assert_eq!(decide(Some(&store.stats("x", p.half_life_secs)), &p), ServiceAction::Serve);
    }
}
