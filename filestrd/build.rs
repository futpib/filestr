//! Derive the Grayjay plugin's config version from the git commit count, so it
//! increases on every commit. Grayjay only re-fetches a plugin when its config
//! version goes up, and the version was previously a frozen literal — so plugin
//! changes never reached already-installed clients. Auto-bumping it removes that
//! footgun. Falls back to 1 when git isn't available (e.g. a crate tarball).

use std::process::Command;

fn main() {
    // re-run when a new commit lands or the plugin files change
    println!("cargo:rerun-if-changed=../.git/HEAD");
    println!("cargo:rerun-if-changed=../.git/refs/heads");
    println!("cargo:rerun-if-changed=../grayjay-plugin/FilestrScript.js");
    println!("cargo:rerun-if-changed=../grayjay-plugin/FilestrConfig.json");

    let version = Command::new("git")
        .args(["rev-list", "--count", "HEAD"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "1".to_owned());

    println!("cargo:rustc-env=FILESTR_PLUGIN_VERSION={version}");
}
