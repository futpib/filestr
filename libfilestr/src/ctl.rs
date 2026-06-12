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
    /// Add a directory to the shared roots (persisted to the config file),
    /// then reload + rescan. `name` defaults to the directory's basename.
    ShareAdd {
        path: PathBuf,
        #[serde(default)]
        name: Option<String>,
    },
    /// Remove a share root by name (and any view references), then reload +
    /// rescan.
    ShareRemove {
        name: String,
    },
    Rescan,
    /// Cancel an in-flight share scan (hashing).
    ScanCancel,
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
    /// Produce a join-request ticket (`filestrreq1…`). With `address` (a hub's
    /// `filestraddr1…`), also gift-wrap and send it to the owner over nostr.
    HubRequest {
        #[serde(default)]
        address: Option<String>,
        #[serde(default)]
        hub: Option<String>,
        #[serde(default)]
        label: Option<String>,
    },
    /// Admit a join-request ticket into a hub we own.
    HubAdmit {
        ticket: String,
        #[serde(default)]
        hub: Option<String>,
    },
    /// Get a hub's shareable address (`filestraddr1…`) for a hub we own.
    HubAddress {
        hub: String,
    },
    /// List join requests received over nostr awaiting manual admit.
    HubPending,
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
    /// Per-peer reputation ledger and the resulting service decision.
    Reputation,
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
    HubJoined { hub: HubInfo, queued: bool },
    HubRequestTicket { ticket: String, sent: bool },
    HubAdmitted { hub: HubInfo },
    HubAddress { address: String },
    HubPending { requests: Vec<PendingRequest> },
    Hubs { hubs: Vec<HubInfo> },
    HubMembers { members: Vec<String> },
    HubSent,
    HubMessages { messages: Vec<ChatMessage> },
    Reputation { peers: Vec<PeerReputation> },
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
    /// Present while a share scan (hashing) is in progress.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub indexing: Option<IndexProgress>,
}

/// Progress of an in-flight share scan: `done` of `total` files hashed/reused.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexProgress {
    pub done: u64,
    pub total: u64,
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
    /// Whether the peer answered a connection probe just now. `None` when the
    /// lister didn't probe, so a missing field never reads as "offline".
    #[serde(default)]
    pub reachable: Option<bool>,
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

/// Optional media metadata for a file, extracted at index time. All fields are
/// optional and skipped when absent, so this is forward/backward compatible on
/// the p2p browse wire (an older peer simply omits them).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MediaMeta {
    // No per-field skip_serializing_if: it would break postcard (a
    // non-self-describing format that reads fields positionally) when this is
    // persisted in the index cache. The whole `media` field is still omitted
    // when empty by the container's `skip_serializing_if = MediaMeta::is_empty`.
    #[serde(default)]
    pub duration_secs: Option<f64>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub artist: Option<String>,
    #[serde(default)]
    pub album: Option<String>,
    /// Content type sniffed from the file's magic bytes at index time (so a
    /// misnamed or extensionless media file is still recognised). e.g.
    /// "audio/mpeg", "video/mp4".
    #[serde(default)]
    pub content_type: Option<String>,
}

impl MediaMeta {
    pub fn is_empty(&self) -> bool {
        *self == MediaMeta::default()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    pub path: String,
    pub size: u64,
    pub hash: String,
    #[serde(default, skip_serializing_if = "MediaMeta::is_empty")]
    pub media: MediaMeta,
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
pub struct PendingRequest {
    /// Requester nostr pubkey (hex).
    pub from: String,
    /// The `filestrreq1…` ticket to pass to `hub admit`.
    pub ticket: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    /// Author nostr pubkey (hex).
    pub author: String,
    pub content: String,
    pub created_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerReputation {
    pub node_id: String,
    /// Bytes we've served them (decayed).
    pub served: u64,
    /// Bytes they've served us (decayed).
    pub received: u64,
    /// served − received; positive means they owe us.
    pub debt: i64,
    /// Current service decision: "serve" or "deny".
    pub action: String,
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
