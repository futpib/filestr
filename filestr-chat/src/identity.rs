//! The node's nostr identity: a secp256k1 keypair persisted to disk, used to
//! author hub messages and as the stable id of a hub member.

use std::path::Path;

use anyhow::{Context, Result};
use nostr::Keys;

#[derive(Clone)]
pub struct Identity {
    pub keys: Keys,
}

impl Identity {
    /// Load the identity from `path`, generating and persisting one (0600) on
    /// first use.
    pub fn load_or_create(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let keys = Keys::parse(text.trim()).context("parsing nostr identity key")?;
                Ok(Self { keys })
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let keys = Keys::generate();
                let hex = keys.secret_key().to_secret_hex();
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(path, format!("{hex}\n"))?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
                }
                Ok(Self { keys })
            }
            Err(e) => Err(e).context("reading nostr identity key"),
        }
    }

    /// Public key as lowercase hex (the member id shown to users).
    pub fn pubkey_hex(&self) -> String {
        self.keys.public_key().to_hex()
    }
}
