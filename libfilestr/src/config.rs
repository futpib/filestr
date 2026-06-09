//! Daemon configuration, loaded from `~/.config/filestr/config.toml`.
//!
//! Every field has a default so an absent or empty config file is valid.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

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
    pub reshare: ReshareConfig,
    pub search: SearchConfig,
    pub invite: InviteConfig,
    /// Shared directories. Each root has a unique name used by views.
    pub share: Vec<ShareRoot>,
    /// Named views: view name -> list of share root names. The view "full"
    /// (all roots) always exists implicitly.
    pub view: BTreeMap<String, Vec<String>>,
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
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self { max_ttl: 5, fanout: 8, timeout_secs: 15, result_cap: 500 }
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
