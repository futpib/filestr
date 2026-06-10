//! Shared types and helpers for filestr: paths, config, control-socket
//! protocol, p2p wire protocol, tickets, and the grant model.
//!
//! Protocol evolution rules (see DESIGN.md §8.1): all wire messages are
//! tagged JSON with `#[serde(default)]` on new fields; unknown message types
//! get a structured error, never a parse abort; evolution is additive only.

pub mod config;
pub mod ctl;
pub mod grants;
pub mod keys;
pub mod p2p;
pub mod paths;
pub mod reputation;
pub mod ticket;

/// Crate version, reported in `status` and `hello`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Major protocol version. Also encoded in the ALPN ([`p2p::ALPN`]).
pub const PROTO_VERSION: u32 = 0;

/// Features this implementation supports, advertised in `hello`/`redeemed`.
pub const FEATURES: &[&str] = &["reshare"];

/// Seconds since the unix epoch; the only timestamp format used on disk.
pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
