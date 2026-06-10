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

/// Durable data directory: `$XDG_DATA_HOME/filestr` (the identity key — the
/// one thing worth backing up).
pub fn data_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("filestr")
}

/// State directory: `$XDG_STATE_HOME/filestr` (grants — mutable, persists, but
/// not "data"). Falls back to the data dir where XDG state is unavailable.
pub fn state_dir() -> PathBuf {
    dirs::state_dir().map(|d| d.join("filestr")).unwrap_or_else(data_dir)
}

/// Cache directory: `$XDG_CACHE_HOME/filestr` (the blob store — reference
/// imports of files that still live in the share roots, so it is regenerable
/// by a rescan). Falls back to the data dir.
pub fn cache_dir() -> PathBuf {
    dirs::cache_dir().map(|d| d.join("filestr")).unwrap_or_else(data_dir)
}

/// Expand `~` (or `~/...`) to the home directory and `$VAR` / `${VAR}` to
/// environment values in a config path. Unknown variables are left as-is.
/// Mirrors slopd's `expand_path` so config files behave the same: the shell
/// does not expand paths read from a file, so we do it here.
pub fn expand_path(path: &std::path::Path) -> PathBuf {
    let s = path.to_string_lossy();
    let expanded = shellexpand::full_with_context_no_errors(
        s.as_ref(),
        || dirs::home_dir().and_then(|p| p.into_os_string().into_string().ok()),
        |var| std::env::var(var).ok(),
    );
    PathBuf::from(expanded.as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn tilde_alone() {
        if let Some(home) = dirs::home_dir() {
            assert_eq!(expand_path(Path::new("~")), home);
        }
    }

    #[test]
    fn tilde_slash() {
        if let Some(home) = dirs::home_dir() {
            assert_eq!(expand_path(Path::new("~/music")), home.join("music"));
        }
    }

    #[test]
    fn dollar_var() {
        // SAFETY: single-threaded test process.
        unsafe { std::env::set_var("FILESTR_TEST_SHARE", "/srv/share") };
        assert_eq!(expand_path(Path::new("$FILESTR_TEST_SHARE/a")), PathBuf::from("/srv/share/a"));
        assert_eq!(
            expand_path(Path::new("${FILESTR_TEST_SHARE}/b")),
            PathBuf::from("/srv/share/b")
        );
    }

    #[test]
    fn absolute_unchanged() {
        assert_eq!(expand_path(Path::new("/absolute/path")), PathBuf::from("/absolute/path"));
    }

    #[test]
    fn unknown_var_left_as_is() {
        assert_eq!(
            expand_path(Path::new("/base/$FILESTR_NONEXISTENT_XYZ/end")),
            PathBuf::from("/base/$FILESTR_NONEXISTENT_XYZ/end")
        );
    }
}
