//! XDG-style path resolution, mirroring slopd's conventions.

use std::path::PathBuf;

fn uid() -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let Ok(meta) = std::fs::metadata("/proc/self") {
            return meta.uid();
        }
    }
    0
}

/// Runtime directory for the control socket: `$XDG_RUNTIME_DIR`, falling back
/// to `/run/user/<uid>`, falling back to `$TMPDIR/filestr-<uid>`.
pub fn runtime_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR")
        && !dir.is_empty()
    {
        return PathBuf::from(dir);
    }
    let run = PathBuf::from(format!("/run/user/{}", uid()));
    if run.is_dir() {
        return run;
    }
    std::env::temp_dir().join(format!("filestr-{}", uid()))
}

/// Default control socket path: `$XDG_RUNTIME_DIR/filestrd/filestrd.sock`.
pub fn socket_path() -> PathBuf {
    runtime_dir().join("filestrd/filestrd.sock")
}

/// Default config file path: `~/.config/filestr/config.toml`.
pub fn config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("filestr/config.toml")
}

/// Default data directory: `~/.local/share/filestr` (blob store, grants,
/// secret key).
pub fn data_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("filestr")
}
