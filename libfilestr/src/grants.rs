//! Grant model and persistence (DESIGN.md §3).
//!
//! Two directions, both stored in one JSON file under the data dir with
//! atomic writes and 0600 permissions:
//! - [`GrantOut`]: invites/grants we issued — who may access *our* share.
//! - [`PeerIn`]: peers who granted *us* access (we redeemed their ticket).

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::unix_now;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrantState {
    Issued,
    Active,
    Revoked,
}

impl GrantState {
    pub fn as_str(&self) -> &'static str {
        match self {
            GrantState::Issued => "issued",
            GrantState::Active => "active",
            GrantState::Revoked => "revoked",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GrantOut {
    pub token_id: String,
    /// Present only while `Issued`; cleared on redeem/revoke.
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
    pub view: String,
    pub allow_reshare: bool,
    pub state: GrantState,
    /// Pinned at redemption.
    #[serde(default)]
    pub node_id: Option<String>,
    pub created_at: u64,
    #[serde(default)]
    pub expires_at: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerIn {
    pub node_id: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub relay: Vec<String>,
    #[serde(default)]
    pub ip: Vec<String>,
    /// Whether this peer allows us to re-serve their content (learned at
    /// redemption; advisory, §7.5).
    pub allow_reshare: bool,
    pub added_at: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Grants {
    pub grants: Vec<GrantOut>,
    pub peers: Vec<PeerIn>,
}

impl Grants {
    pub fn load_or_default(path: &Path) -> std::io::Result<Self> {
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e),
        }
    }

    /// Atomic save: write sibling temp file, fsync, rename over.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(self)?;
        {
            use std::io::Write;
            let mut file = std::fs::File::create(&tmp)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
            }
            file.write_all(&bytes)?;
            file.sync_all()?;
        }
        std::fs::rename(&tmp, path)
    }

    /// Mint a new invite. Returns the grant; the caller builds the ticket.
    pub fn issue(
        &mut self,
        view: String,
        label: Option<String>,
        allow_reshare: bool,
        expiry_secs: u64,
    ) -> &GrantOut {
        let token = new_token();
        let grant = GrantOut {
            token_id: token_id(&token),
            token: Some(token),
            label,
            view,
            allow_reshare,
            state: GrantState::Issued,
            node_id: None,
            created_at: unix_now(),
            expires_at: Some(unix_now() + expiry_secs),
        };
        self.grants.push(grant);
        self.grants.last().expect("just pushed")
    }

    /// Redeem `token`, pinning `node_id`. Burns the token; idempotent
    /// re-redemption by the *same* node succeeds (covers a lost response).
    pub fn redeem(&mut self, token: &str, node_id: &str) -> Option<&GrantOut> {
        // already pinned to this node? (response may have been lost)
        if let Some(i) = self
            .grants
            .iter()
            .position(|g| g.state == GrantState::Active && g.node_id.as_deref() == Some(node_id) && g.token_id == token_id(token))
        {
            return Some(&self.grants[i]);
        }
        let now = unix_now();
        let i = self.grants.iter().position(|g| {
            g.state == GrantState::Issued
                && g.token.as_deref() == Some(token)
                && g.expires_at.map(|t| t > now).unwrap_or(true)
        })?;
        let grant = &mut self.grants[i];
        grant.state = GrantState::Active;
        grant.token = None;
        grant.node_id = Some(node_id.to_string());
        grant.expires_at = None;
        Some(&self.grants[i])
    }

    /// Directly allow `node_id` (an active grant with no token) — the reverse
    /// half of a symmetric redemption: we let the grantor reach our share too.
    pub fn allow(&mut self, node_id: &str, view: String, allow_reshare: bool) {
        if self.active_for(node_id).is_some() {
            return;
        }
        self.grants.push(GrantOut {
            token_id: token_id(node_id),
            token: None,
            label: Some("symmetric".to_string()),
            view,
            allow_reshare,
            state: GrantState::Active,
            node_id: Some(node_id.to_string()),
            created_at: unix_now(),
            expires_at: None,
        });
    }

    /// The active grant for a connecting node, if any.
    pub fn active_for(&self, node_id: &str) -> Option<&GrantOut> {
        self.grants
            .iter()
            .find(|g| g.state == GrantState::Active && g.node_id.as_deref() == Some(node_id))
    }

    /// Revoke grants matching `needle` (node id, token id, or label prefix).
    /// Returns descriptions of what was revoked.
    pub fn revoke(&mut self, needle: &str) -> Vec<String> {
        let mut revoked = Vec::new();
        for grant in &mut self.grants {
            if grant.state == GrantState::Revoked {
                continue;
            }
            let matches = grant.token_id.starts_with(needle)
                || grant.node_id.as_deref().map(|n| n.starts_with(needle)).unwrap_or(false)
                || grant.label.as_deref() == Some(needle);
            if matches {
                grant.state = GrantState::Revoked;
                grant.token = None;
                revoked.push(grant.node_id.clone().unwrap_or_else(|| grant.token_id.clone()));
            }
        }
        revoked
    }

    /// Drop incoming peers matching `needle`. Returns dropped node ids.
    pub fn drop_peers(&mut self, needle: &str) -> Vec<String> {
        let mut dropped = Vec::new();
        self.peers.retain(|p| {
            let matches = p.node_id.starts_with(needle) || p.label.as_deref() == Some(needle);
            if matches {
                dropped.push(p.node_id.clone());
            }
            !matches
        });
        dropped
    }

    /// Record (or refresh) an incoming peer after redeeming their ticket.
    pub fn upsert_peer(&mut self, peer: PeerIn) {
        if let Some(existing) = self.peers.iter_mut().find(|p| p.node_id == peer.node_id) {
            *existing = peer;
        } else {
            self.peers.push(peer);
        }
    }
}

pub fn new_token() -> String {
    let bytes: [u8; 16] = rand::random();
    data_encoding::BASE32_NOPAD.encode(&bytes).to_ascii_lowercase()
}

/// Short stable reference to a token, safe to display and store after burn.
pub fn token_id(token: &str) -> String {
    token.chars().take(8).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redeem_burns_token() {
        let mut grants = Grants::default();
        let token = grants
            .issue("full".into(), None, true, 3600)
            .token
            .clone()
            .unwrap();

        let grant = grants.redeem(&token, "node-a").expect("first redeem works");
        assert_eq!(grant.state, GrantState::Active);
        assert_eq!(grant.node_id.as_deref(), Some("node-a"));

        // token is burned for everyone else
        assert!(grants.redeem(&token, "node-b").is_none());
        // …but the same node may re-redeem (lost response)
        assert!(grants.redeem(&token, "node-a").is_some());

        assert!(grants.active_for("node-a").is_some());
        assert!(grants.active_for("node-b").is_none());
    }

    #[test]
    fn expired_invite_cannot_be_redeemed() {
        let mut grants = Grants::default();
        let token = grants.issue("full".into(), None, true, 0).token.clone().unwrap();
        // expires_at == now, so strictly-greater check fails
        assert!(grants.redeem(&token, "node-a").is_none());
    }

    #[test]
    fn revoke_by_prefix() {
        let mut grants = Grants::default();
        let token = grants.issue("full".into(), Some("bob".into()), true, 3600).token.clone().unwrap();
        grants.redeem(&token, "nodeid1234");
        let revoked = grants.revoke("nodeid12");
        assert_eq!(revoked, vec!["nodeid1234".to_string()]);
        assert!(grants.active_for("nodeid1234").is_none());
    }

    #[test]
    fn persistence_roundtrip() {
        let dir = std::env::temp_dir().join(format!("filestr-test-{}", std::process::id()));
        let path = dir.join("grants.json");
        let mut grants = Grants::default();
        grants.issue("full".into(), None, true, 3600);
        grants.save(&path).unwrap();
        let back = Grants::load_or_default(&path).unwrap();
        assert_eq!(back.grants.len(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }
}
