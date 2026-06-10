//! Node-to-node wire protocol (DESIGN.md §8).
//!
//! One bidi QUIC stream per request on ALPN `filestr/0`: the dialer writes a
//! single JSON request line, then either reads JSON response lines (control
//! requests) or, for [`P2pRequest::Get`], hands the rest of the stream to the
//! iroh-blobs transfer protocol.
//!
//! `Get` is the streaming-transfer entrypoint. After the one-line header, the
//! remainder of the bidi stream *is* a bao-verified iroh-blobs get exchange.
//! A relay handling `Get` with a remote handle does not buffer: it dials the
//! upstream, forwards the header, and splices raw bytes both ways, so transfers
//! stream through and verification stays end-to-end (DESIGN.md §7.3).
//!
//! Unknown request types must be answered with `{"type":"error",
//! "code":"unsupported"}` (§8.1), which is why requests are decoded in two
//! steps via [`decode_request`].

use serde::{Deserialize, Serialize};

use crate::ctl::FileEntry;

/// Control ALPN; the trailing digit is the protocol major version.
pub const ALPN: &[u8] = b"filestr/0";

/// Maximum length of a single JSON line on the wire.
pub const MAX_LINE: usize = 1024 * 1024;

/// Error codes used in [`P2pResponse::Error`].
pub mod code {
    pub const UNSUPPORTED: &str = "unsupported";
    pub const DENIED: &str = "denied";
    pub const NOT_FOUND: &str = "not_found";
    pub const BAD_REQUEST: &str = "bad_request";
    pub const INTERNAL: &str = "internal";
    /// Refused for free-riding past the credit limit (DESIGN.md §9 reputation).
    pub const RATE_LIMITED: &str = "rate_limited";
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum P2pRequest {
    /// Version/feature negotiation; allowed from anyone.
    Hello,
    /// Present an invite token; the only other request allowed from
    /// not-yet-granted nodes. Carries the redeemer's own dialable address so
    /// the grantor can reach back — tickets are symmetric (both sides end up
    /// granting each other).
    Redeem {
        token: String,
        #[serde(default)]
        relay: Vec<String>,
        #[serde(default)]
        ip: Vec<String>,
    },
    /// Full file list for the caller's view.
    List,
    /// Streaming search. Results carry no origin attribution (§7.1).
    Search { query_id: String, ttl: u8, query: String },
    /// Streaming transfer. The header names an optional search/browse
    /// `handle`; everything after the header line on this stream is the
    /// iroh-blobs get protocol (the client picks the hash and byte ranges
    /// there). A `None` handle means "serve from your own store"; a handle
    /// resolving to a remote source makes us splice through to it (§7.3).
    Get {
        #[serde(default)]
        handle: Option<String>,
        /// Content hash being fetched. Lets the server pre-check availability
        /// and account served bytes (and is forwarded by relays).
        #[serde(default)]
        hash: Option<String>,
    },
    /// nostr-over-iroh tunnel (DESIGN.md §8.2): the rest of the stream is a
    /// NIP-01 relay session. Answered with `unsupported` when the chat feature
    /// is disabled.
    Nostr,
    /// Hub control RPC (opaque to this layer; the chat feature defines the
    /// payload). One JSON request line in, one `HubReply` out.
    Hub { payload: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum P2pResponse {
    Hello {
        v: u32,
        features: Vec<String>,
        version: String,
    },
    Redeemed {
        view: String,
        /// Whether the grantor allows us to re-serve their content (§7.5).
        allow_reshare: bool,
        v: u32,
        features: Vec<String>,
    },
    /// One chunk of a file list; the stream ends with `list_done`.
    Entries { entries: Vec<FileEntry> },
    ListDone { total: u64 },
    /// One search result: `{name, size, hash, handle}` and nothing else —
    /// no origin node id, no path through the graph (§7.1).
    Hit {
        name: String,
        size: u64,
        hash: String,
        handle: String,
    },
    SearchDone,
    /// Sent on a `Get` stream just before the bao transfer to signal the
    /// server accepted; denial uses `Error` (e.g. code `rate_limited`).
    GetOk,
    /// Reply to a `Hub` request (opaque; chat feature defines the payload).
    HubReply { payload: String },
    Error { code: String, message: String },
}

/// Outcome of decoding an incoming request line.
#[derive(Debug)]
pub enum DecodedRequest {
    Known(P2pRequest),
    /// Parsed fine but the `type` is from the future: respond `unsupported`.
    Unknown { request_type: String },
    Malformed { message: String },
}

/// Two-step decode so unknown `type`s degrade to a structured error instead
/// of a parse failure (§8.1).
pub fn decode_request(line: &str) -> DecodedRequest {
    let value: serde_json::Value = match serde_json::from_str(line) {
        Ok(value) => value,
        Err(e) => return DecodedRequest::Malformed { message: e.to_string() },
    };
    let request_type = match value.get("type").and_then(|t| t.as_str()) {
        Some(t) => t.to_string(),
        None => {
            return DecodedRequest::Malformed { message: "missing \"type\" field".into() };
        }
    };
    match serde_json::from_value::<P2pRequest>(value) {
        Ok(request) => DecodedRequest::Known(request),
        // the type tag itself didn't match any variant -> future message
        Err(_) if !known_request_type(&request_type) => {
            DecodedRequest::Unknown { request_type }
        }
        Err(e) => DecodedRequest::Malformed { message: e.to_string() },
    }
}

fn known_request_type(t: &str) -> bool {
    matches!(t, "hello" | "redeem" | "list" | "search" | "get" | "nostr" | "hub")
}

pub fn encode<T: Serialize>(msg: &T) -> String {
    let mut line = serde_json::to_string(msg).expect("wire types always serialize");
    line.push('\n');
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_type_is_not_an_error() {
        match decode_request(r#"{"type":"telepathy","payload":1}"#) {
            DecodedRequest::Unknown { request_type } => assert_eq!(request_type, "telepathy"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn known_type_with_future_fields_decodes() {
        match decode_request(r#"{"type":"list","compression":"zstd"}"#) {
            DecodedRequest::Known(P2pRequest::List) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn malformed_is_reported() {
        assert!(matches!(decode_request("{"), DecodedRequest::Malformed { .. }));
        assert!(matches!(decode_request("{}"), DecodedRequest::Malformed { .. }));
    }

    #[test]
    fn hit_has_no_attribution_fields() {
        let hit = P2pResponse::Hit {
            name: "song.flac".into(),
            size: 1,
            hash: "aa".into(),
            handle: "hh".into(),
        };
        let value = serde_json::to_value(&hit).unwrap();
        let keys: Vec<&str> = value.as_object().unwrap().keys().map(|k| k.as_str()).collect();
        assert_eq!(keys, ["handle", "hash", "name", "size", "type"]);
    }
}
