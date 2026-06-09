//! Background transfer manager. Each `get` becomes a tracked transfer that
//! runs in its own task, so many downloads proceed concurrently. Progress is
//! exposed two ways: a per-transfer watch channel (for a foreground `get`
//! that streams progress) and broadcast events (for `listen` / `transfers`).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Result, anyhow};
use libfilestr::ctl::TransferInfo;
use libfilestr::grants::PeerIn;
use libfilestr::unix_now;
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

use crate::search::{self, ByteRange};
use crate::state::State;

struct Entry {
    info: TransferInfo,
    tx: watch::Sender<TransferInfo>,
    cancel: CancellationToken,
}

#[derive(Default)]
pub struct Transfers {
    next_id: u64,
    map: HashMap<u64, Entry>,
}

impl Transfers {
    pub fn snapshot(&self) -> Vec<TransferInfo> {
        let mut transfers: Vec<TransferInfo> = self.map.values().map(|e| e.info.clone()).collect();
        transfers.sort_by_key(|t| t.id);
        transfers
    }

    pub fn cancel(&self, id: u64) -> bool {
        match self.map.get(&id) {
            Some(entry) => {
                entry.cancel.cancel();
                true
            }
            None => false,
        }
    }
}

/// Apply `mutate` to a transfer's info and publish it on its watch channel.
async fn patch(state: &State, id: u64, mutate: impl FnOnce(&mut TransferInfo)) {
    let mut transfers = state.transfers.lock().await;
    if let Some(entry) = transfers.map.get_mut(&id) {
        mutate(&mut entry.info);
        let _ = entry.tx.send(entry.info.clone());
    }
}

/// Create a transfer and spawn its task. Returns the id and a watch receiver
/// for the foreground path to stream progress (background callers drop it).
pub async fn start(
    state: Arc<State>,
    hash: String,
    out: PathBuf,
    range: Option<ByteRange>,
    peer_pref: Option<String>,
) -> Result<(u64, watch::Receiver<TransferInfo>)> {
    let _: iroh_blobs::Hash = hash.parse().map_err(|e| anyhow!("bad hash {hash}: {e}"))?;

    let total = match range {
        Some((start, end)) if end != u64::MAX => end - start + 1,
        _ => state
            .recent_sources
            .lock()
            .await
            .get(&hash)
            .iter()
            .map(|s| s.size)
            .max()
            .unwrap_or(0),
    };

    let mut transfers = state.transfers.lock().await;
    transfers.next_id += 1;
    let id = transfers.next_id;
    let info = TransferInfo {
        id,
        hash: hash.clone(),
        out: out.clone(),
        range: range.map(|(s, e)| [s, e]),
        total,
        transferred: 0,
        status: "queued".into(),
        error: None,
        started_at: unix_now(),
    };
    let (tx, rx) = watch::channel(info.clone());
    let cancel = CancellationToken::new();
    transfers.map.insert(id, Entry { info, tx, cancel: cancel.clone() });
    drop(transfers);

    state.emit("transfer_started", serde_json::json!({ "id": id, "hash": hash }));

    let task_state = state.clone();
    tokio::spawn(async move {
        run(task_state, id, hash, out, range, peer_pref, cancel).await;
    });
    Ok((id, rx))
}

async fn run(
    state: Arc<State>,
    id: u64,
    hash: String,
    out: PathBuf,
    range: Option<ByteRange>,
    peer_pref: Option<String>,
    cancel: CancellationToken,
) {
    let result = tokio::select! {
        _ = cancel.cancelled() => {
            patch(&state, id, |t| t.status = "cancelled".into()).await;
            state.emit("transfer_cancelled", serde_json::json!({ "id": id }));
            return;
        }
        result = transfer(&state, id, &hash, range, &peer_pref) => result,
    };

    match result {
        Ok(()) => match export(&state, id, &hash, &out, range).await {
            Ok(size) => {
                patch(&state, id, |t| {
                    t.status = "done".into();
                    if t.total == 0 {
                        t.total = size;
                    }
                    t.transferred = t.total;
                })
                .await;
                state.emit(
                    "transfer_done",
                    serde_json::json!({ "id": id, "hash": hash, "path": out, "size": size }),
                );
            }
            Err(e) => fail(&state, id, &hash, format!("export failed: {e:#}")).await,
        },
        Err(e) => fail(&state, id, &hash, format!("{e:#}")).await,
    }
}

async fn fail(state: &State, id: u64, hash: &str, message: String) {
    patch(state, id, |t| {
        t.status = "failed".into();
        t.error = Some(message.clone());
    })
    .await;
    state.emit(
        "transfer_failed",
        serde_json::json!({ "id": id, "hash": hash, "error": message }),
    );
}

/// Stream the content into our store from the first source that works.
async fn transfer(
    state: &Arc<State>,
    id: u64,
    hash: &str,
    range: Option<ByteRange>,
    peer_pref: &Option<String>,
) -> Result<()> {
    patch(state, id, |t| t.status = "active".into()).await;

    // a full file already complete locally needs no transfer
    if range.is_none() {
        let parsed: iroh_blobs::Hash = hash.parse()?;
        if matches!(
            state.store.blobs().status(parsed).await?,
            iroh_blobs::api::proto::BlobStatus::Complete { .. }
        ) {
            return Ok(());
        }
    }

    let candidates = candidates(state, hash, peer_pref).await?;
    if candidates.is_empty() {
        return Err(anyhow!(
            "no known source for {hash}; search or browse first, or pass --peer"
        ));
    }

    let mut last_error = anyhow!("no source tried");
    for (peer, handle) in candidates {
        let (ptx, mut prx) = mpsc::channel::<u64>(32);
        let pump_state = state.clone();
        let pump = tokio::spawn(async move {
            while let Some(n) = prx.recv().await {
                patch(&pump_state, id, |t| t.transferred = n).await;
            }
        });
        let result = search::fetch_source(state, &peer, handle, hash, range, &ptx).await;
        drop(ptx);
        let _ = pump.await;
        match result {
            Ok(()) => return Ok(()),
            Err(e) => {
                tracing::debug!("source {} failed: {e:#}", peer.node_id);
                last_error = e;
            }
        }
    }
    Err(last_error.context("all sources failed"))
}

/// Candidate sources for `hash`: recent search/browse results (optionally
/// filtered by `peer_pref`), falling back to `peer_pref` as a direct peer.
async fn candidates(
    state: &Arc<State>,
    hash: &str,
    peer_pref: &Option<String>,
) -> Result<Vec<(PeerIn, Option<String>)>> {
    let mut candidates = Vec::new();
    let recent = state.recent_sources.lock().await.get(hash);
    let grants = state.grants.lock().await;
    for source in recent {
        let Some(peer) = grants.grants.peers.iter().find(|p| p.node_id == source.peer) else {
            continue;
        };
        let preferred = peer_pref
            .as_deref()
            .map(|needle| peer.node_id.starts_with(needle) || peer.label.as_deref() == Some(needle))
            .unwrap_or(true);
        if preferred {
            candidates.push((peer.clone(), source.handle.clone()));
        }
    }
    if candidates.is_empty()
        && let Some(needle) = &peer_pref
    {
        let matches: Vec<&PeerIn> = grants
            .grants
            .peers
            .iter()
            .filter(|p| p.node_id.starts_with(needle) || p.label.as_deref() == Some(needle))
            .collect();
        if matches.len() == 1 {
            candidates.push((matches[0].clone(), None));
        } else if matches.len() > 1 {
            return Err(anyhow!("{} peers match {needle:?}; be more specific", matches.len()));
        }
    }
    Ok(candidates)
}

/// Write the fetched content (whole blob or clipped byte range) to `out`.
async fn export(
    state: &State,
    _id: u64,
    hash: &str,
    out: &PathBuf,
    range: Option<ByteRange>,
) -> Result<u64> {
    let parsed: iroh_blobs::Hash = hash.parse()?;
    if let Some(parent) = out.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    match range {
        Some((start, end)) => {
            let end_excl = if end == u64::MAX { u64::MAX } else { end + 1 };
            let bytes = state
                .store
                .blobs()
                .export_ranges(parsed, start..end_excl)
                .concatenate()
                .await?;
            let len = bytes.len() as u64;
            tokio::fs::write(out, bytes).await?;
            Ok(len)
        }
        None => {
            state.store.blobs().export(parsed, out).await?;
            match state.store.blobs().status(parsed).await? {
                iroh_blobs::api::proto::BlobStatus::Complete { size } => Ok(size),
                other => Err(anyhow!("blob incomplete after fetch: {other:?}")),
            }
        }
    }
}
