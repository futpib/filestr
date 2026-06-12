//! Share indexer: walks share roots, imports files into the blob store by
//! reference (no copy), and answers view-scoped list/search queries.

use std::collections::HashMap;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use iroh_blobs::BlobFormat;
use iroh_blobs::api::blobs::AddPathOptions;
use iroh_blobs::api::proto::ImportMode;
use iroh_blobs::store::fs::FsStore;
use libfilestr::config::Config;
use libfilestr::ctl::{FileEntry, MediaMeta};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexedFile {
    pub root: String,
    /// `<root name>/<relative path>`, the path peers see.
    pub path: String,
    pub size: u64,
    /// File mtime (seconds since the epoch); 0 if unavailable. Used with `size`
    /// to skip re-hashing unchanged files on a rescan.
    pub mtime: u64,
    pub hash: String,
    /// Media metadata (duration/tags), extracted at scan time. Empty for
    /// non-media or unreadable files.
    pub media: MediaMeta,
}

#[derive(Debug, Default, Clone)]
pub struct Index {
    pub files: Vec<IndexedFile>,
}

/// Bump when the on-disk `IndexedFile` layout changes, to invalidate old caches.
const INDEX_CACHE_VERSION: u32 = 2;

impl Index {
    /// Load the persisted index cache (best-effort): a content-keyed cache of
    /// path → hash + metadata so a restart can skip re-hashing unchanged files.
    /// Any read/parse error (missing, corrupt, version bump) yields an empty
    /// index, which just means a full rescan.
    pub fn load(path: &std::path::Path) -> Index {
        let Ok(bytes) = std::fs::read(path) else {
            return Index::default();
        };
        match postcard::from_bytes::<(u32, Vec<IndexedFile>)>(&bytes) {
            Ok((INDEX_CACHE_VERSION, files)) => Index { files },
            _ => Index::default(),
        }
    }

    /// Delete cached thumbnails no longer referenced by any indexed file (their
    /// source was removed or changed to a new hash). Best-effort.
    pub fn prune_thumbs(&self, thumbs_dir: &std::path::Path) {
        let keep: std::collections::HashSet<&str> =
            self.files.iter().map(|f| f.hash.as_str()).collect();
        let Ok(dir) = std::fs::read_dir(thumbs_dir) else {
            return;
        };
        let mut removed = 0usize;
        for entry in dir.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if !keep.contains(name) {
                if std::fs::remove_file(entry.path()).is_ok() {
                    removed += 1;
                }
            }
        }
        if removed > 0 {
            tracing::debug!(removed, "pruned orphaned thumbnails");
        }
    }

    /// Persist the index cache via a temp file + rename. Best-effort: a failure
    /// is logged and ignored (the cache is regenerable by rescanning).
    pub fn save(&self, path: &std::path::Path) {
        let bytes = match postcard::to_allocvec(&(INDEX_CACHE_VERSION, &self.files)) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!("serializing index cache: {e}");
                return;
            }
        };
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".tmp");
        let tmp = std::path::PathBuf::from(tmp);
        if let Err(e) = std::fs::write(&tmp, &bytes).and_then(|_| std::fs::rename(&tmp, path)) {
            tracing::warn!("writing index cache {}: {e}", path.display());
        }
    }

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

    /// Case-insensitive AND-of-terms substring search over the visible path and
    /// the media tags (title/artist/album), so a query matches what a consumer
    /// actually sees in the feed, not just the filename.
    pub fn search(&self, roots: &[String], query: &str) -> Vec<&IndexedFile> {
        let terms: Vec<String> = query.split_whitespace().map(|t| t.to_lowercase()).collect();
        if terms.is_empty() {
            return Vec::new();
        }
        self.files
            .iter()
            .filter(|f| roots.contains(&f.root))
            .filter(|f| {
                let mut haystack = f.path.to_lowercase();
                for tag in [&f.media.title, &f.media.artist, &f.media.album] {
                    if let Some(t) = tag {
                        haystack.push(' ');
                        haystack.push_str(&t.to_lowercase());
                    }
                }
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

/// Walk all share roots and (re)import into the blob store, reusing the hash and
/// metadata of any file whose path, size and mtime are unchanged since `prev`.
/// Reference import means changed/new files are hashed but not copied; the share
/// stays on disk where it is. Hashing + metadata probing is the expensive part,
/// so skipping unchanged files keeps a rescan (and `share add`) cheap on a large
/// library — only genuinely new/changed files are touched.
pub async fn scan(
    config: &Config,
    store: &FsStore,
    thumbs_dir: &std::path::Path,
    prev: &Index,
    cancel: &tokio_util::sync::CancellationToken,
    progress: &std::sync::Arc<crate::state::ScanProgress>,
    live: &tokio::sync::RwLock<Index>,
) -> Result<usize> {
    use std::sync::atomic::Ordering;
    // visible path -> previously indexed file, for the unchanged-file fast path
    let prev_by_path: HashMap<&str, &IndexedFile> =
        prev.files.iter().map(|f| (f.path.as_str(), f)).collect();

    let mut reused_files = Vec::new();
    let mut pending: Vec<PendingFile> = Vec::new();
    let mut reused = 0usize;
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
            let path = format!("{}/{}", root.name, rel);
            let meta = entry.metadata().ok();
            let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
            let mtime = meta
                .as_ref()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);

            // Fast path: an unchanged file (same path/size/mtime) keeps its hash
            // and metadata — no re-hash, no re-probe. The blob and thumbnail are
            // already in the store/cache from the previous scan.
            if mtime != 0 {
                if let Some(p) = prev_by_path.get(path.as_str()) {
                    if p.size == size && p.mtime == mtime {
                        reused_files.push(IndexedFile {
                            root: root.name.clone(),
                            path,
                            size,
                            mtime,
                            hash: p.hash.clone(),
                            media: p.media.clone(),
                        });
                        reused += 1;
                        continue;
                    }
                }
            }
            pending.push(PendingFile { root: root.name.clone(), abs, path, size, mtime });
        }
    }

    // Hash + probe the new/changed files concurrently — up to one job per core,
    // so a `share add` of a large directory saturates the CPU (the daemon runs
    // niced, so this still yields to foreground work). A single bad file is
    // logged and skipped rather than failing the whole scan.
    let to_hash = pending.len();
    progress.total.store((reused + to_hash) as u64, Ordering::Relaxed);
    progress.done.store(reused as u64, Ordering::Relaxed);
    // Publish the reused (unchanged) files at once so they serve immediately and
    // any deleted ones drop, while new/changed files hash in the background and
    // append below — `share add` serves files as each one is indexed.
    *live.write().await = Index { files: std::mem::take(&mut reused_files) };
    let concurrency = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(concurrency));
    let mut set = tokio::task::JoinSet::new();
    let mut iter = pending.into_iter();
    let mut done_launching = false;
    // Single loop that launches hashing work AND drains finished hashes
    // concurrently, so each file is published to the live index the moment it's
    // hashed (incremental serving) — not after every file has been spawned.
    // Pause stops launching but keeps draining; cancel aborts in-flight work.
    loop {
        if done_launching && set.is_empty() {
            break;
        }
        let paused = progress.paused.load(Ordering::Relaxed);
        let can_launch = !done_launching && !paused;
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                set.shutdown().await;
                break;
            }
            // A hash finished: publish it and count it as done right away.
            Some(res) = set.join_next(), if !set.is_empty() => {
                if let Ok(Some(f)) = res {
                    live.write().await.files.push(f);
                    progress.done.fetch_add(1, Ordering::Relaxed);
                }
            }
            // A slot is free and we're allowed to launch: hash the next file.
            permit = sem.clone().acquire_owned(), if can_launch => {
                let permit = permit.expect("semaphore");
                match iter.next() {
                    None => done_launching = true,
                    Some(p) => {
                        let store = store.clone();
                        let thumbs = thumbs_dir.to_path_buf();
                        set.spawn(async move {
                            let _permit = permit;
                            let tag = match store
                                .blobs()
                                .add_path_with_opts(AddPathOptions {
                                    path: p.abs.clone(),
                                    mode: ImportMode::TryReference,
                                    format: BlobFormat::Raw,
                                })
                                .await
                            {
                                Ok(t) => t,
                                Err(e) => {
                                    tracing::warn!("importing {}: {e:#}", p.abs.display());
                                    return None;
                                }
                            };
                            let probed = {
                                let path = p.abs.clone();
                                tokio::task::spawn_blocking(move || crate::metadata::probe(&path))
                                    .await
                                    .unwrap_or_default()
                            };
                            let hash = tag.hash.to_hex().to_string();
                            // Cache any embedded cover art as a thumbnail keyed by
                            // content hash, for the gateway's /thumb/{hash}.
                            if let Some(cover) = probed.cover {
                                let _ = tokio::fs::write(thumbs.join(&hash), &cover).await;
                            }
                            Some(IndexedFile {
                                root: p.root,
                                path: p.path,
                                size: p.size,
                                mtime: p.mtime,
                                hash,
                                media: probed.meta,
                            })
                        });
                    }
                }
            }
            // Paused with more to launch: wake when resumed (or cancelled) and
            // re-evaluate, so we don't spin while idle.
            _ = progress.resume.notified(), if paused && !done_launching => {}
        }
    }
    progress.active.store(false, Ordering::Relaxed);

    if cancel.is_cancelled() {
        tracing::info!(reused, "share scan cancelled (files indexed so far stay served)");
        anyhow::bail!("scan cancelled");
    }
    let n = live.read().await.files.len();
    tracing::info!(files = n, reused, hashed = to_hash, "share scan complete");
    Ok(n)
}

/// A file that needs hashing + metadata probing (new or changed since `prev`).
struct PendingFile {
    root: String,
    abs: std::path::PathBuf,
    path: String,
    size: u64,
    mtime: u64,
}
