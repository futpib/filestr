//! Invite tickets: self-contained `filestr1…` strings carrying everything
//! needed to dial the grantor and redeem the invite (DESIGN.md §3.1).
//!
//! Encoding: `"filestr1"` + lowercase base32 (no padding) of the JSON body.
//! The body carries an explicit version `v`; unknown versions fail with a
//! clear error rather than a garbled parse (§8.1).

use serde::{Deserialize, Serialize};

pub const PREFIX: &str = "filestr1";
pub const TICKET_VERSION: u8 = 0;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ticket {
    pub v: u8,
    /// Grantor endpoint id (lowercase hex).
    pub id: String,
    /// Relay URLs the grantor is reachable through.
    #[serde(default)]
    pub relay: Vec<String>,
    /// Direct socket addresses (empty when the invite is relay-only).
    #[serde(default)]
    pub ip: Vec<String>,
    /// Single-use invite token.
    pub token: String,
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Debug)]
pub enum TicketError {
    BadPrefix,
    BadEncoding,
    BadJson(String),
    /// Ticket from a newer filestr; upgrading is the fix.
    UnsupportedVersion(u8),
}

impl std::fmt::Display for TicketError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TicketError::BadPrefix => write!(f, "not a filestr ticket (expected {PREFIX}…)"),
            TicketError::BadEncoding => write!(f, "ticket base32 decode failed"),
            TicketError::BadJson(e) => write!(f, "ticket payload invalid: {e}"),
            TicketError::UnsupportedVersion(v) => {
                write!(f, "ticket version {v} not supported; upgrade filestr")
            }
        }
    }
}

impl std::error::Error for TicketError {}

impl Ticket {
    pub fn encode(&self) -> String {
        let json = serde_json::to_vec(self).expect("ticket always serializes");
        let body = data_encoding::BASE32_NOPAD.encode(&json).to_ascii_lowercase();
        format!("{PREFIX}{body}")
    }

    pub fn parse(s: &str) -> Result<Self, TicketError> {
        let s = s.trim();
        let body = s.strip_prefix(PREFIX).ok_or(TicketError::BadPrefix)?;
        let bytes = data_encoding::BASE32_NOPAD
            .decode(body.to_ascii_uppercase().as_bytes())
            .map_err(|_| TicketError::BadEncoding)?;
        let ticket: Ticket =
            serde_json::from_slice(&bytes).map_err(|e| TicketError::BadJson(e.to_string()))?;
        if ticket.v != TICKET_VERSION {
            return Err(TicketError::UnsupportedVersion(ticket.v));
        }
        Ok(ticket)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Ticket {
        Ticket {
            v: TICKET_VERSION,
            id: "ab".repeat(32),
            relay: vec!["https://relay.example/".into()],
            ip: vec!["192.168.1.2:4433".into()],
            token: "tokentokentoken".into(),
            label: Some("for-alice".into()),
        }
    }

    #[test]
    fn roundtrip() {
        let ticket = sample();
        let s = ticket.encode();
        assert!(s.starts_with(PREFIX));
        let back = Ticket::parse(&s).unwrap();
        assert_eq!(back.id, ticket.id);
        assert_eq!(back.token, ticket.token);
        assert_eq!(back.relay, ticket.relay);
    }

    #[test]
    fn case_insensitive_and_whitespace_tolerant() {
        let s = sample().encode();
        let shouty = format!("  {}{}\n", PREFIX, s[PREFIX.len()..].to_ascii_uppercase());
        assert!(Ticket::parse(&shouty).is_ok());
    }

    #[test]
    fn future_version_rejected_clearly() {
        let mut ticket = sample();
        ticket.v = 9;
        let err = Ticket::parse(&ticket.encode()).unwrap_err();
        assert!(matches!(err, TicketError::UnsupportedVersion(9)));
    }
}
