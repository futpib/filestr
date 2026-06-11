//! Share indexer: walks share roots, imports files into the blob store by
//! reference (no copy), and answers view-scoped list/search queries.

use std::collections::HashMap;

use anyhow::{Context, Result};
use iroh_blobs::BlobFormat;
use iroh_blobs::api::blobs::AddPathOptions;
use iroh_blobs::api::proto::ImportMode;
use iroh_blobs::store::fs::FsStore;
use libfilestr::config::Config;
use libfilestr::ctl::{FileEntry, MediaMeta};

#[derive(Debug, Clone)]
pub struct IndexedFile {
    pub root: String,
    /// `<root name>/<relative path>`, the path peers see.
    pub path: String,
    pub size: u64,
    pub hash: String,
    /// Media metadata (duration/tags), extracted at scan time. Empty for
    /// non-media or unreadable files.
    pub media: MediaMeta,
}

#[derive(Debug, Default)]
pub struct Index {
    pub files: Vec<IndexedFile>,
}

impl Index {
    pub fn entries(&self, roots: &[String]) -> Vec<FileEntry> {
        self.files
            .iter()
            .filter(|f| roots.contains(&f.root))
            .map(|f| FileEntry {
                path: f.path.clone(),
                size: f.size,
                hash: f.hash.clone(),
                media: f.media.clone(),
            })
            .collect()
    }

    /// Case-insensitive AND-of-terms substring search over visible paths.
    pub fn search(&self, roots: &[String], query: &str) -> Vec<&IndexedFile> {
        let terms: Vec<String> = query.split_whitespace().map(|t| t.to_lowercase()).collect();
        if terms.is_empty() {
            return Vec::new();
        }
        self.files
            .iter()
            .filter(|f| roots.contains(&f.root))
            .filter(|f| {
                let haystack = f.path.to_lowercase();
                terms.iter().all(|t| haystack.contains(t))
            })
            .collect()
    }

    /// (file count, total bytes) per share root.
    pub fn root_stats(&self) -> HashMap<String, (usize, u64)> {
        let mut stats: HashMap<String, (usize, u64)> = HashMap::new();
        for f in &self.files {
            let entry = stats.entry(f.root.clone()).or_default();
            entry.0 += 1;
            entry.1 += f.size;
        }
        stats
    }
}

/// Walk all share roots and (re)import into the blob store. Reference import
/// means unchanged files are re-hashed but not copied; the share stays on
/// disk where it is.
pub async fn scan(config: &Config, store: &FsStore) -> Result<Index> {
    let mut files = Vec::new();
    for root in &config.share {
        let base = libfilestr::paths::expand_path(&root.path);
        if !base.is_dir() {
            tracing::warn!(root = %root.name, path = %base.display(), "share root missing, skipping");
            continue;
        }
        for entry in walkdir::WalkDir::new(&base).follow_links(false) {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    tracing::warn!("walk error under {}: {e}", base.display());
                    continue;
                }
            };
            if !entry.file_type().is_file() {
                continue;
            }
            let abs = entry.path().to_path_buf();
            let rel = match abs.strip_prefix(&base) {
                Ok(rel) => rel.to_string_lossy().replace('\\', "/"),
                Err(_) => continue,
            };
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            let tag = store
                .blobs()
                .add_path_with_opts(AddPathOptions {
                    path: abs.clone(),
                    mode: ImportMode::TryReference,
                    format: BlobFormat::Raw,
                })
                .await
                .with_context(|| format!("importing {}", abs.display()))?;
            // Probe media metadata off the async runtime (it does blocking file
            // IO). Best-effort: failures yield empty metadata.
            let media = {
                let p = abs.clone();
                tokio::task::spawn_blocking(move || crate::metadata::probe(&p))
                    .await
                    .unwrap_or_default()
            };
            files.push(IndexedFile {
                root: root.name.clone(),
                path: format!("{}/{}", root.name, rel),
                size,
                hash: tag.hash.to_hex().to_string(),
                media,
            });
        }
    }
    tracing::info!(files = files.len(), "share scan complete");
    Ok(Index { files })
}
