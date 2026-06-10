//! Key material. By default a single 32-byte **master seed** (persisted to
//! `master.key`) deterministically derives every other key the node needs —
//! the iroh transport key and the nostr identity — via domain-separated
//! BLAKE3. Back up one file, get your whole identity.
//!
//! The iroh key can be overridden independently (see `iroh.key` handling in
//! the daemon), e.g. to keep a pre-existing endpoint identity while letting
//! the master seed drive everything else.

use std::path::Path;

use anyhow::{Context, Result, anyhow};

/// BLAKE3 derivation contexts. Stable strings — changing one rotates the
/// derived key, so never edit these.
pub const CTX_IROH: &str = "filestr iroh transport key v1";
pub const CTX_NOSTR: &str = "filestr nostr identity key v1";

/// The node's root secret. Everything else is derived from it.
#[derive(Clone)]
pub struct Master {
    seed: [u8; 32],
}

impl Master {
    /// Load the seed from `path`, generating and persisting one (0600) on
    /// first use.
    pub fn load_or_create(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let bytes = data_encoding::HEXLOWER
                    .decode(text.trim().as_bytes())
                    .context("master.key is not valid hex")?;
                let seed: [u8; 32] =
                    bytes.try_into().map_err(|_| anyhow!("master.key must be 32 bytes"))?;
                Ok(Self { seed })
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let seed: [u8; 32] = rand::random();
                write_secret(path, &data_encoding::HEXLOWER.encode(&seed))?;
                Ok(Self { seed })
            }
            Err(e) => Err(e).context("reading master.key"),
        }
    }

    /// Derive 32 bytes for `context`. Distinct contexts yield independent keys.
    pub fn derive(&self, context: &str) -> [u8; 32] {
        blake3::derive_key(context, &self.seed)
    }

    /// Derive 32 bytes for `context` salted with `counter` — used to retry
    /// when a derived value must satisfy extra constraints (e.g. a valid
    /// secp256k1 scalar).
    pub fn derive_counter(&self, context: &str, counter: u32) -> [u8; 32] {
        let mut material = [0u8; 36];
        material[..32].copy_from_slice(&self.seed);
        material[32..].copy_from_slice(&counter.to_le_bytes());
        blake3::derive_key(context, &material)
    }
}

/// Write `hex` to `path` with 0600 perms (atomic-ish: direct create).
pub fn write_secret(path: &Path, hex: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, format!("{hex}\n"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derivation_is_deterministic_and_separated() {
        let m = Master { seed: [7u8; 32] };
        assert_eq!(m.derive(CTX_IROH), m.derive(CTX_IROH), "deterministic");
        assert_ne!(m.derive(CTX_IROH), m.derive(CTX_NOSTR), "domain-separated");
        assert_ne!(
            m.derive_counter(CTX_NOSTR, 0),
            m.derive_counter(CTX_NOSTR, 1),
            "counter varies output"
        );
    }

    #[test]
    fn different_seeds_differ() {
        let a = Master { seed: [1u8; 32] };
        let b = Master { seed: [2u8; 32] };
        assert_ne!(a.derive(CTX_IROH), b.derive(CTX_IROH));
    }
}
