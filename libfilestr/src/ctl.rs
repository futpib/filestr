//! Control-socket protocol between filestrctl and filestrd.
//!
//! Newline-delimited JSON over a unix socket, slopd-style: requests are
//! `{"id": N, "body": {...}}`, responses `{"id": N, "body": {...}}` with the
//! same id. Streaming operations (search, get, subscribe) emit multiple
//! responses with the same id, ending with a terminal variant
//! (`search_done`, `get_done`) or running until the client disconnects
//! (`event`).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub id: u64,
    pub body: RequestBody,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub id: u64,
    pub body: ResponseBody,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RequestBody {
    Status,
    InviteCreate {
        #[serde(default)]
        view: Option<String>,
        #[serde(default)]
        label: Option<String>,
        #[serde(default)]
        allow_reshare: Option<bool>,
        #[serde(default)]
        relay_only: Option<bool>,
    },
    InviteList,
    InviteRevoke {
        token_id: String,
    },
    /// Redeem a ticket: dial the grantor and enroll as their peer.
    PeerAdd {
        ticket: String,
        #[serde(default)]
        label: Option<String>,
    },
    PeerList,
    /// Revoke an outgoing grant (by node id or token id prefix) or drop an
    /// incoming peer.
    PeerRevoke {
        peer: String,
    },
    ShareList,
    Rescan,
    /// Fetch the remote file list of a peer that granted us access.
    Browse {
        peer: String,
    },
    Search {
        query: String,
        #[serde(default)]
        ttl: Option<u8>,
    },
    Get {
        hash: String,
        out: PathBuf,
        /// Prefer this source peer (node id prefix); otherwise the daemon
        /// uses recent search results / browses to pick one.
        #[serde(default)]
        peer: Option<String>,
        /// Inclusive byte range "START-END" / "START-" (whole file if absent).
        #[serde(default)]
        range: Option<String>,
        /// Start the transfer and return immediately instead of streaming
        /// progress on this connection.
        #[serde(default)]
        background: bool,
    },
    /// List background/active/finished transfers.
    Transfers,
    /// Cancel a transfer by id.
    TransferCancel {
        id: u64,
    },
    /// Create a hub (MLS group) we own.
    HubCreate {
        name: String,
    },
    /// Mint a hub join ticket (also grants the invitee file-relay access).
    HubInvite {
        hub: String,
    },
    /// Join a hub from a `filestrhub1…` ticket.
    HubJoin {
        ticket: String,
    },
    /// List hubs we own or have joined.
    HubList,
    /// List a hub's members (nostr pubkeys).
    HubMembers {
        hub: String,
    },
    /// Send a chat message to a hub.
    HubSend {
        hub: String,
        text: String,
    },
    /// Sync and return a hub's chat log.
    HubLog {
        hub: String,
    },
    Subscribe,
    Shutdown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseBody {
    Status { status: DaemonStatus },
    InviteCreated { ticket: String, token_id: String },
    Invites { invites: Vec<InviteInfo> },
    InviteRevoked { token_id: String },
    PeerAdded { peer: PeerInfo },
    Peers { grants: Vec<GrantInfo>, peers: Vec<PeerInfo> },
    PeerRevoked { revoked: Vec<String> },
    Shares { files: usize, shares: Vec<ShareInfo>, views: Vec<ViewInfo> },
    Rescanned { files: usize },
    Entries { entries: Vec<FileEntry> },
    SearchHit { hit: SearchHit },
    SearchDone { hits: usize },
    GetProgress { transferred: u64, total: u64 },
    GetDone { path: PathBuf, hash: String, size: u64 },
    TransferStarted { id: u64 },
    Transfers { transfers: Vec<TransferInfo> },
    TransferCancelled { id: u64 },
    HubCreated { hub: HubInfo },
    HubInvite { ticket: String },
    HubJoined { hub: HubInfo },
    Hubs { hubs: Vec<HubInfo> },
    HubMembers { members: Vec<String> },
    HubSent,
    HubMessages { messages: Vec<ChatMessage> },
    Subscribed,
    Event { event: Event },
    ShuttingDown,
    Error { message: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonStatus {
    pub endpoint_id: String,
    #[serde(default)]
    pub relays: Vec<String>,
    #[serde(default)]
    pub direct_addrs: Vec<String>,
    pub files: usize,
    pub grants_active: usize,
    pub grants_issued: usize,
    pub peers: usize,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InviteInfo {
    pub token_id: String,
    #[serde(default)]
    pub label: Option<String>,
    pub view: String,
    pub allow_reshare: bool,
    pub state: String,
    #[serde(default)]
    pub node_id: Option<String>,
    pub created_at: u64,
    #[serde(default)]
    pub expires_at: Option<u64>,
}

/// An outgoing grant (someone we allow to access our share).
pub type GrantInfo = InviteInfo;

/// An incoming peer (someone who granted us access to their share).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerInfo {
    pub node_id: String,
    #[serde(default)]
    pub label: Option<String>,
    /// Whether we may re-serve this peer's content (their choice, advisory).
    pub allow_reshare: bool,
    pub added_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShareInfo {
    pub name: String,
    pub path: PathBuf,
    pub files: usize,
    pub bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ViewInfo {
    pub name: String,
    pub roots: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    pub path: String,
    pub size: u64,
    pub hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHit {
    pub name: String,
    pub size: u64,
    pub hash: String,
    pub handle: String,
    /// Which of our peers delivered the hit (local knowledge only; never on
    /// the p2p wire).
    #[serde(default)]
    pub via: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub event_type: String,
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HubInfo {
    /// MLS group id (hex) — the stable hub handle used in commands.
    pub group_ref: String,
    pub name: String,
    /// True if this node owns the hub (hosts its relay).
    pub owner: bool,
    pub members: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    /// Author nostr pubkey (hex).
    pub author: String,
    pub content: String,
    pub created_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferInfo {
    pub id: u64,
    pub hash: String,
    pub out: PathBuf,
    /// Inclusive byte range, if this is a ranged transfer.
    #[serde(default)]
    pub range: Option<[u64; 2]>,
    /// Best-effort total bytes (0 if unknown until transfer starts).
    pub total: u64,
    pub transferred: u64,
    /// One of: queued, active, done, failed, cancelled.
    pub status: String,
    #[serde(default)]
    pub error: Option<String>,
    pub started_at: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let req = Request { id: 7, body: RequestBody::Search { query: "flac".into(), ttl: None } };
        let line = serde_json::to_string(&req).unwrap();
        let back: Request = serde_json::from_str(&line).unwrap();
        assert_eq!(back.id, 7);
        match back.body {
            RequestBody::Search { query, ttl } => {
                assert_eq!(query, "flac");
                assert_eq!(ttl, None);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn unknown_fields_tolerated() {
        // a newer client may send fields we don't know yet
        let line = r#"{"id":1,"body":{"type":"status","future_field":42}}"#;
        let req: Request = serde_json::from_str(line).unwrap();
        assert!(matches!(req.body, RequestBody::Status));
    }
}
