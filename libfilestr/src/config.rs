//! Daemon configuration, loaded from `~/.config/filestr/config.toml`.
//!
//! Every field has a default so an absent or empty config file is valid.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::reputation::{OverLimit, Policy};

/// Name of the implicit view containing every share root.
pub const VIEW_FULL: &str = "full";

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Config {
    /// Override the control socket path.
    pub socket: Option<PathBuf>,
    /// Override the data directory (blob store, grants, secret key).
    pub data_dir: Option<PathBuf>,
    /// Relay usage: "default" (n0 public relays) or "disabled" (direct only).
    pub relay: RelaySetting,
    /// Custom iroh relay server URLs. If non-empty, these are used instead of
    /// the `relay` setting (self-hosted relays).
    pub relay_urls: Vec<String>,
    pub reshare: ReshareConfig,
    pub search: SearchConfig,
    pub invite: InviteConfig,
    pub reputation: ReputationConfig,
    pub chat: ChatConfig,
    pub http: HttpConfig,
    /// Shared directories. Each root has a unique name used by views.
    pub share: Vec<ShareRoot>,
    /// Named views: view name -> list of share root names. The view "full"
    /// (all roots) always exists implicitly.
    pub view: BTreeMap<String, Vec<String>>,
}

/// Reputation/anti-free-riding policy: a global default plus per-peer
/// overrides matched by node-id prefix or grant label.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ReputationConfig {
    pub enabled: bool,
    pub credit_limit_mib: u64,
    pub newcomer_budget_mib: u64,
    pub half_life_days: u64,
    pub over_limit: OverLimit,
    /// Per-peer overrides; the first whose `peer` matches wins.
    #[serde(rename = "override")]
    pub overrides: Vec<ReputationOverride>,
}

impl Default for ReputationConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            credit_limit_mib: 256,
            newcomer_budget_mib: 64,
            half_life_days: 7,
            over_limit: OverLimit::Deny,
            overrides: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ReputationOverride {
    /// Node-id prefix or grant label this override applies to.
    pub peer: String,
    pub enabled: Option<bool>,
    pub credit_limit_mib: Option<u64>,
    pub newcomer_budget_mib: Option<u64>,
    pub half_life_days: Option<u64>,
    pub over_limit: Option<OverLimit>,
}

impl ReputationConfig {
    fn base_policy(&self) -> Policy {
        Policy {
            enabled: self.enabled,
            credit_limit: self.credit_limit_mib * 1024 * 1024,
            newcomer_budget: self.newcomer_budget_mib * 1024 * 1024,
            half_life_secs: self.half_life_days * 24 * 3600,
            over_limit: self.over_limit,
        }
    }
}

impl Config {
    /// Resolve the effective reputation policy for a peer (base config with the
    /// first matching per-peer override applied).
    pub fn reputation_policy(&self, node_id: &str, label: Option<&str>) -> Policy {
        let mut policy = self.reputation.base_policy();
        if let Some(o) = self.reputation.overrides.iter().find(|o| {
            !o.peer.is_empty() && (node_id.starts_with(&o.peer) || label == Some(o.peer.as_str()))
        }) {
            if let Some(v) = o.enabled {
                policy.enabled = v;
            }
            if let Some(v) = o.credit_limit_mib {
                policy.credit_limit = v * 1024 * 1024;
            }
            if let Some(v) = o.newcomer_budget_mib {
                policy.newcomer_budget = v * 1024 * 1024;
            }
            if let Some(v) = o.half_life_days {
                policy.half_life_secs = v * 24 * 3600;
            }
            if let Some(v) = o.over_limit {
                policy.over_limit = v;
            }
        }
        policy
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum RelaySetting {
    #[default]
    Default,
    Disabled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ReshareConfig {
    /// Serve content reachable through my peers to my grantees, and forward
    /// their searches (DESIGN.md §7).
    pub serve: bool,
    /// Default `allow_reshare` for newly created invites: may the grantee
    /// re-serve my content to their grantees (advisory, §7.5).
    pub allow: bool,
}

impl Default for ReshareConfig {
    fn default() -> Self {
        Self { serve: true, allow: true }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SearchConfig {
    /// Default and maximum accepted TTL (hop cap).
    pub max_ttl: u8,
    /// Maximum peers a query is forwarded to.
    pub fanout: usize,
    /// Overall deadline for a search, seconds.
    pub timeout_secs: u64,
    /// Maximum results returned/forwarded per query.
    pub result_cap: usize,
    /// Per-peer deadline for a `/files` browse, seconds. A slow or unreachable
    /// peer can't stall the whole listing past this — its results are simply
    /// omitted from that response (kept fresh, never cached).
    pub browse_timeout_secs: u64,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self { max_ttl: 5, fanout: 8, timeout_secs: 15, result_cap: 500, browse_timeout_secs: 4 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct InviteConfig {
    /// Put only the relay address in tickets (hide direct IPs from invitees).
    pub relay_only: bool,
    /// Unredeemed invites expire after this many seconds.
    pub expiry_secs: u64,
}

impl Default for InviteConfig {
    fn default() -> Self {
        Self { relay_only: false, expiry_secs: 7 * 24 * 3600 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShareRoot {
    pub name: String,
    pub path: PathBuf,
}

/// Chat-plane (nostr) relay configuration. Used only with the `chat` feature.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ChatConfig {
    /// Activate the chat plane (nostr identity, MLS store, hub commands,
    /// relays). When false the node runs and peers files with no nostr at all;
    /// flip it on and restart to join hubs later. Default on.
    pub enabled: bool,
    /// Serve the embedded relay to grantees over the iroh `nostr` stream.
    pub embedded_relay: bool,
    /// Optionally also expose the embedded relay as a standard NIP-01 relay on
    /// this TCP WebSocket address (e.g. "127.0.0.1:7777").
    pub relay_listen: Option<String>,
    /// External nostr relay URLs (ws:// or wss://) to also publish hub events
    /// to and read them from, and to advertise in hub metadata.
    pub relays: Vec<String>,
    /// Auto-admit join requests that arrive over nostr (open-hub UX). When
    /// false, requests are queued for manual `hub admit`.
    pub auto_admit: bool,
}

impl Default for ChatConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            embedded_relay: true,
            relay_listen: None,
            relays: Vec::new(),
            auto_admit: false,
        }
    }
}

/// Local HTTP gateway: a read-only, loopback-only bridge that lets other apps
/// on the same device (e.g. a Grayjay plugin) list and stream the files this
/// node can serve. Off by default; bind only to localhost.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct HttpConfig {
    /// TCP address to listen on, e.g. "127.0.0.1:11780". Disabled when unset.
    pub listen: Option<String>,
}

#[derive(Debug)]
pub enum ConfigError {
    Io(std::io::Error),
    Parse(toml::de::Error),
    /// A view references a share root name that does not exist.
    UnknownRoot { view: String, root: String },
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::Io(e) => write!(f, "config io error: {e}"),
            ConfigError::Parse(e) => write!(f, "config parse error: {e}"),
            ConfigError::UnknownRoot { view, root } => {
                write!(f, "view {view:?} references unknown share root {root:?}")
            }
        }
    }
}

impl std::error::Error for ConfigError {}

impl Config {
    /// Load from `path`; a missing file yields the default config.
    pub fn load_or_default(path: &Path) -> Result<Self, ConfigError> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(ConfigError::Io(e)),
        };
        let config: Config = toml::from_str(&text).map_err(ConfigError::Parse)?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        for (view, roots) in &self.view {
            for root in roots {
                if !self.share.iter().any(|s| &s.name == root) {
                    return Err(ConfigError::UnknownRoot {
                        view: view.clone(),
                        root: root.clone(),
                    });
                }
            }
        }
        Ok(())
    }

    /// Share root names visible through `view`. `None` if the view is unknown.
    pub fn view_roots(&self, view: &str) -> Option<Vec<String>> {
        if view == VIEW_FULL {
            return Some(self.share.iter().map(|s| s.name.clone()).collect());
        }
        self.view.get(view).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_config_is_valid() {
        let config: Config = toml::from_str("").unwrap();
        assert_eq!(config.search.max_ttl, 5);
        assert!(config.reshare.serve);
        assert!(config.reshare.allow);
        assert_eq!(config.relay, RelaySetting::Default);
    }

    #[test]
    fn unknown_fields_are_ignored() {
        // forward compatibility: configs written by newer versions still load
        let config: Config = toml::from_str("some_future_setting = true").unwrap();
        assert!(config.share.is_empty());
    }

    #[test]
    fn view_validation() {
        let text = r#"
            [[share]]
            name = "music"
            path = "/tmp/music"

            [view]
            friends = ["music"]
            broken = ["nope"]
        "#;
        let config: Config = toml::from_str(text).unwrap();
        assert!(config.validate().is_err());
        assert_eq!(config.view_roots("full").unwrap(), vec!["music".to_string()]);
        assert_eq!(config.view_roots("friends").unwrap(), vec!["music".to_string()]);
        assert!(config.view_roots("missing").is_none());
    }
}
