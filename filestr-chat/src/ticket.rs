//! Hub join ticket: a filestr invite (so the joiner can connect to the hub
//! owner / its relay) plus the hub's name. Encoded as `filestrhub1` +
//! base32(JSON), same shape as the file-sharing ticket.

use anyhow::{Result, anyhow};
use libfilestr::ticket::Ticket;
use serde::{Deserialize, Serialize};

pub const PREFIX: &str = "filestrhub1";
pub const HUB_TICKET_VERSION: u8 = 0;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HubTicket {
    pub v: u8,
    /// filestr invite for the owner→joiner grant (lets the joiner reach the
    /// owner's relay over the `nostr` stream).
    pub invite: Ticket,
    /// Human-readable hub name (the joiner confirms it in the welcome).
    pub name: String,
    /// MLS group id (hex) the joiner echoes back so the owner knows which hub
    /// to admit them to.
    pub group_ref: String,
}

impl HubTicket {
    pub fn new(invite: Ticket, name: String, group_ref: String) -> Self {
        Self { v: HUB_TICKET_VERSION, invite, name, group_ref }
    }

    pub fn encode(&self) -> String {
        let json = serde_json::to_vec(self).expect("hub ticket serializes");
        format!("{PREFIX}{}", data_encoding::BASE32_NOPAD.encode(&json).to_ascii_lowercase())
    }

    pub fn parse(s: &str) -> Result<Self> {
        let body = s
            .trim()
            .strip_prefix(PREFIX)
            .ok_or_else(|| anyhow!("not a filestr hub ticket (expected {PREFIX}…)"))?;
        let bytes = data_encoding::BASE32_NOPAD
            .decode(body.to_ascii_uppercase().as_bytes())
            .map_err(|_| anyhow!("hub ticket base32 decode failed"))?;
        let ticket: HubTicket =
            serde_json::from_slice(&bytes).map_err(|e| anyhow!("hub ticket invalid: {e}"))?;
        if ticket.v != HUB_TICKET_VERSION {
            return Err(anyhow!("hub ticket version {} not supported; upgrade filestr", ticket.v));
        }
        Ok(ticket)
    }
}

pub const REQ_PREFIX: &str = "filestrreq1";
pub const REQ_TICKET_VERSION: u8 = 0;

/// A member-initiated **join request**: everything an owner needs to admit the
/// requester unprompted. Self-contained so it works pasted out-of-band or
/// carried over a nostr DM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestTicket {
    pub v: u8,
    /// filestr invite the owner redeems — dials the requester back (to push
    /// the welcome) and grants the owner file access (share-to-join).
    pub reciprocal: Ticket,
    /// The requester's MLS key-package event (JSON), signed by their nostr key.
    pub key_package: String,
    /// Target hub (MLS group ref) the requester wants; None lets the owner
    /// pick (e.g. their only hub, or one chosen at admit time).
    #[serde(default)]
    pub hub: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
}

impl RequestTicket {
    pub fn encode(&self) -> String {
        let json = serde_json::to_vec(self).expect("request ticket serializes");
        format!("{REQ_PREFIX}{}", data_encoding::BASE32_NOPAD.encode(&json).to_ascii_lowercase())
    }

    pub fn parse(s: &str) -> Result<Self> {
        let body = s
            .trim()
            .strip_prefix(REQ_PREFIX)
            .ok_or_else(|| anyhow!("not a filestr request ticket (expected {REQ_PREFIX}…)"))?;
        let bytes = data_encoding::BASE32_NOPAD
            .decode(body.to_ascii_uppercase().as_bytes())
            .map_err(|_| anyhow!("request ticket base32 decode failed"))?;
        let ticket: RequestTicket =
            serde_json::from_slice(&bytes).map_err(|e| anyhow!("request ticket invalid: {e}"))?;
        if ticket.v != REQ_TICKET_VERSION {
            return Err(anyhow!(
                "request ticket version {} not supported; upgrade filestr",
                ticket.v
            ));
        }
        Ok(ticket)
    }
}
