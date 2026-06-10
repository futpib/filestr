//! Optional chat plane for filestr: **real Marmot/MLS** group messaging
//! (via `mdk-core` + OpenMLS — the White Noise stack) carried over nostr,
//! with the owner hosting an embedded NIP-01 relay reachable over the iroh
//! `nostr` stream.
//!
//! A hub is a Marmot/MLS group. Messages are MLS application messages wrapped
//! as nostr kind:445 events; membership uses MLS key packages (kind:30443) and
//! welcomes (kind:444). This gives genuine forward secrecy and post-compromise
//! security and is wire-compatible with the Marmot protocol — not a homegrown
//! scheme.
//!
//! Transport is decoupled from MLS: [`relay`] + [`transport`] speak NIP-01
//! over any `AsyncRead + AsyncWrite`, so filestrd can run them on the iroh
//! `nostr` stream. Nothing here depends on iroh.
//!
//! v1 limitation: MLS state lives in `mdk-memory-storage` (the official
//! testing backend), so groups are lost on daemon restart. Swapping in
//! `mdk-sqlite-storage` for persistence is a storage-provider change only.

pub mod identity;
pub mod mls;
pub mod relay;
pub mod ticket;
pub mod transport;

pub use identity::Identity;
pub use mls::{DecryptedMessage, Mls};
pub use relay::Relay;
pub use ticket::HubTicket;
