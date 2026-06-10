//! The node's nostr identity: a secp256k1 keypair used to author hub messages
//! and as the stable id of a hub member. Derived from the shared master seed
//! (`libfilestr::keys`) so one backup covers both the iroh and nostr keys.

use anyhow::{Result, anyhow};
use libfilestr::keys::{CTX_NOSTR, Master};
use nostr::{Keys, SecretKey};

#[derive(Clone)]
pub struct Identity {
    pub keys: Keys,
}

impl Identity {
    /// Derive the nostr identity from the master seed. Derived bytes must be a
    /// valid secp256k1 scalar; on the negligible chance they aren't, salt with
    /// an incrementing counter until they are.
    pub fn derive(master: &Master) -> Result<Self> {
        for counter in 0..16u32 {
            let material = master.derive_counter(CTX_NOSTR, counter);
            if let Ok(secret) = SecretKey::from_slice(&material) {
                return Ok(Self { keys: Keys::new(secret) });
            }
        }
        Err(anyhow!("could not derive a valid nostr key from the master seed"))
    }

    /// Public key as lowercase hex (the member id shown to users).
    pub fn pubkey_hex(&self) -> String {
        self.keys.public_key().to_hex()
    }
}
