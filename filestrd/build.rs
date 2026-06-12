//! Derive the Grayjay plugin's config version from the git commit count, so it
//! increases on every commit. Grayjay only re-fetches a plugin when its config
//! version goes up, and the version was previously a frozen literal — so plugin
//! changes never reached already-installed clients. Auto-bumping it removes that
//! footgun. Falls back to 1 when git isn't available (e.g. a crate tarball).
//!
//! Note: the embedded script `../grayjay-plugin/FilestrScript.js` is a generated
//! artifact, compiled from `../grayjay-plugin/src/FilestrScript.ts` (see that
//! crate's README / tsconfig.json). This build script embeds whatever `.js` is
//! committed; keeping it in sync with the `.ts` is enforced by the
//! test-grayjay-typecheck.sh autotest, not here.
//!
//! Two desync footguns this guards against, both of which shipped a stale
//! version once and made a fixed plugin look unfixed in Grayjay:
//!
//!   * the plugin script is embedded with `include_str!` (working-tree content),
//!     but the version is the *committed* count — so building with uncommitted
//!     plugin edits ships a new script under an old version, and Grayjay keeps
//!     the cached old script. We detect a dirty plugin tree and bump +1 so a
//!     dev build always out-versions its committed baseline.
//!   * `cargo` only re-runs this script when a watched path changes. Watching
//!     `.git/refs/heads` misses commits when refs are packed; the reflog
//!     (`.git/logs/HEAD`) is appended on every commit/checkout, so watching it
//!     keeps the baked version in step with HEAD regardless of ref storage.

use std::path::Path;
use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    Command::new("git")
        .args(args)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_owned())
}

fn main() {
    // Re-run when a new commit lands (reflog is appended on every commit, even
    // with packed refs) or when the plugin files change (so a dirty edit both
    // re-embeds the script and re-evaluates the version together).
    println!("cargo:rerun-if-changed=../.git/HEAD");
    println!("cargo:rerun-if-changed=../.git/logs/HEAD");
    println!("cargo:rerun-if-changed=../.git/refs/heads");
    println!("cargo:rerun-if-changed=../grayjay-plugin/FilestrScript.js");
    println!("cargo:rerun-if-changed=../grayjay-plugin/FilestrConfig.json");

    let count = git(&["rev-list", "--count", "HEAD"])
        .filter(|s| !s.is_empty())
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(1);

    // If the embedded plugin differs from HEAD, the committed count understates
    // what we're actually shipping — bump so this build out-versions the last
    // clean build at this commit, and warn loudly (the real fix is to commit).
    let plugin_dirty = git(&["status", "--porcelain", "--", "../grayjay-plugin"])
        .map(|s| !s.is_empty())
        .unwrap_or(false);

    let version = if plugin_dirty {
        println!(
            "cargo:warning=filestr: grayjay-plugin has uncommitted changes; \
             shipping plugin version {}+1={} — COMMIT the plugin so installed \
             clients get a clean, reproducible version.",
            count,
            count + 1
        );
        count + 1
    } else {
        count
    };

    // Reproducibility belt-and-braces: if a release binary is ever shipped from
    // a tree where the embedded script doesn't exist, that's a packaging error.
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../grayjay-plugin/FilestrScript.js");
    if !script.exists() {
        println!("cargo:warning=filestr: embedded plugin script not found at {script:?}");
    }

    println!("cargo:rustc-env=FILESTR_PLUGIN_VERSION={version}");
}
