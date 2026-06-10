//! The node's nostr identity: a secp256k1 keypair used to author hub messages
//! and as the stable id of a hub member. Derived from the shared master seed
//! (`libfilestr::keys`) so one backup covers both the iroh and nostr keys.

use anyhow::{Context, Result};
use libfilestr::keys::RootKey;
use nostr::{Keys, SecretKey};

#[derive(Clone)]
pub struct Identity {
    pub keys: Keys,
}

impl Identity {
    /// Build the nostr identity from the root key — the stored nsec *is* this
    /// secret key.
    pub fn from_root(root: &RootKey) -> Result<Self> {
        let secret = SecretKey::from_slice(&root.secret_bytes())
            .context("root key is not a valid nostr secret")?;
        Ok(Self { keys: Keys::new(secret) })
    }

    /// Public key as lowercase hex (the member id shown to users).
    pub fn pubkey_hex(&self) -> String {
        self.keys.public_key().to_hex()
    }
}
