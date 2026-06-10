//! The node's root secret is its **nostr identity** (a secp256k1 key), stored
//! as an `nsec` in `identity.key`. This is the portable, user-facing key —
//! importable into nostr clients. The iroh transport key is *derived* from it
//! one-way via domain-separated BLAKE3, so storing the single nsec covers the
//! whole node identity.
//!
//! The iroh key can still be overridden independently (see `iroh.key` in the
//! daemon), e.g. to keep a fixed endpoint id while the nsec drives the rest.

use std::path::Path;

use anyhow::{Context, Result, anyhow};
use bech32::Hrp;

/// BLAKE3 derivation context for the iroh transport key. Stable — changing it
/// rotates the derived key.
pub const CTX_IROH: &str = "filestr iroh transport key v1";

/// BLAKE3 derivation context for the at-rest MLS storage encryption key.
pub const CTX_MLS_DB: &str = "filestr mls storage key v1";

const NSEC_HRP: &str = "nsec";

/// secp256k1 group order (big-endian); a valid secret key is in `[1, N)`.
const SECP256K1_N: [u8; 32] = [
    0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFE,
    0xBA, 0xAE, 0xDC, 0xE6, 0xAF, 0x48, 0xA0, 0x3B, 0xBF, 0xD2, 0x5E, 0x8C, 0xD0, 0x36, 0x41, 0x41,
];

/// The node's root nostr secret. The iroh key derives from it.
#[derive(Clone)]
pub struct RootKey {
    secret: [u8; 32],
}

impl RootKey {
    /// Load the nsec (or raw hex) from `path`, generating and persisting one
    /// (as an nsec, 0600) on first use. Refuses an existing file whose
    /// permissions are too open (SSH `StrictModes` behaviour).
    pub fn load_or_create(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                ensure_secure_perms(path)?;
                Self::parse(text.trim())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let secret = generate_scalar();
                let root = Self { secret };
                write_secret(path, &root.nsec()?)?;
                Ok(root)
            }
            Err(e) => Err(e).context("reading identity.key"),
        }
    }

    /// Parse an `nsec1…` bech32 string or 64-char hex.
    pub fn parse(s: &str) -> Result<Self> {
        let secret = if s.starts_with("nsec1") {
            let (hrp, data) = bech32::decode(s).context("decoding nsec")?;
            if hrp.to_string() != NSEC_HRP {
                return Err(anyhow!("not an nsec (hrp was {hrp})"));
            }
            data.try_into().map_err(|_| anyhow!("nsec payload must be 32 bytes"))?
        } else {
            let bytes = data_encoding::HEXLOWER
                .decode(s.as_bytes())
                .context("identity key is neither nsec nor hex")?;
            bytes.try_into().map_err(|_| anyhow!("identity key must be 32 bytes"))?
        };
        if !is_valid_scalar(&secret) {
            return Err(anyhow!("identity key is not a valid secp256k1 secret"));
        }
        Ok(Self { secret })
    }

    /// Render as an `nsec1…` string.
    pub fn nsec(&self) -> Result<String> {
        let hrp = Hrp::parse(NSEC_HRP).expect("nsec is a valid hrp");
        bech32::encode::<bech32::Bech32>(hrp, &self.secret).context("encoding nsec")
    }

    /// The raw 32-byte secret (used by the chat plane to build nostr keys).
    pub fn secret_bytes(&self) -> [u8; 32] {
        self.secret
    }

    /// Derive the iroh ed25519 transport key seed.
    pub fn derive_iroh(&self) -> [u8; 32] {
        self.derive(CTX_IROH)
    }

    /// Derive a one-way 32-byte subkey for `context` (domain-separated).
    pub fn derive(&self, context: &str) -> [u8; 32] {
        blake3::derive_key(context, &self.secret)
    }
}

/// Whether `b` is a valid secp256k1 secret: nonzero and `< N`.
fn is_valid_scalar(b: &[u8; 32]) -> bool {
    if b.iter().all(|&x| x == 0) {
        return false;
    }
    // big-endian comparison b < N
    for i in 0..32 {
        if b[i] < SECP256K1_N[i] {
            return true;
        }
        if b[i] > SECP256K1_N[i] {
            return false;
        }
    }
    false // equal to N is invalid
}

fn generate_scalar() -> [u8; 32] {
    loop {
        let b: [u8; 32] = rand::random();
        if is_valid_scalar(&b) {
            return b;
        }
    }
}

/// Refuse a secret file that is group/other-accessible or not owned by us —
/// the way SSH rejects an over-permissive or wrong-owner private key
/// (`StrictModes`). No-op on non-unix.
pub fn ensure_secure_perms(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let meta = std::fs::metadata(path).with_context(|| format!("stat {}", path.display()))?;

        let mode = meta.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            return Err(anyhow!(
                "permissions {:#o} for {} are too open — group/others can access this secret.\n\
                 Fix it: chmod 600 {}",
                mode,
                path.display(),
                path.display(),
            ));
        }

        // owner must be us (or root, e.g. a system-managed deployment)
        let euid = unsafe { libc::geteuid() };
        let owner = meta.uid();
        if owner != euid && owner != 0 {
            return Err(anyhow!(
                "{} is owned by uid {} but you are uid {} — refusing to use another user's secret.\n\
                 Fix it: chown {} {}",
                path.display(),
                owner,
                euid,
                euid,
                path.display(),
            ));
        }
    }
    let _ = path;
    Ok(())
}

/// Create `dir` if missing and lock it to 0700 (owner-only), like `~/.ssh`.
pub fn ensure_private_dir(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .with_context(|| format!("locking {} to 0700", dir.display()))?;
    }
    Ok(())
}

/// Write `text` to `path` with 0600 perms.
pub fn write_secret(path: &Path, text: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, format!("{text}\n"))?;
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
    fn nsec_roundtrip() {
        let root = RootKey { secret: [3u8; 32] };
        let nsec = root.nsec().unwrap();
        assert!(nsec.starts_with("nsec1"));
        let back = RootKey::parse(&nsec).unwrap();
        assert_eq!(back.secret, root.secret);
    }

    #[test]
    fn hex_also_parses() {
        let hex = "01".repeat(32);
        let root = RootKey::parse(&hex).unwrap();
        assert_eq!(root.secret, [1u8; 32]);
    }

    #[test]
    fn iroh_derivation_is_deterministic_and_one_way() {
        let root = RootKey { secret: [9u8; 32] };
        assert_eq!(root.derive_iroh(), root.derive_iroh());
        assert_ne!(root.derive_iroh(), root.secret_bytes(), "iroh key != nsec");
    }

    #[test]
    fn rejects_invalid_scalars() {
        assert!(!is_valid_scalar(&[0u8; 32]));
        assert!(!is_valid_scalar(&SECP256K1_N));
        assert!(is_valid_scalar(&[1u8; 32]));
    }

    #[test]
    fn generated_keys_are_valid() {
        for _ in 0..100 {
            assert!(is_valid_scalar(&generate_scalar()));
        }
    }

    #[cfg(unix)]
    #[test]
    fn refuses_world_readable_secret() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("filestr-perm-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("identity.key");
        std::fs::write(&path, "x").unwrap();

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(ensure_secure_perms(&path).is_ok());

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(ensure_secure_perms(&path).is_err(), "0644 must be rejected");

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o660)).unwrap();
        assert!(ensure_secure_perms(&path).is_err(), "group-accessible must be rejected");

        std::fs::remove_dir_all(&dir).ok();
    }
}
