//! filestrd — friend-to-friend file sharing daemon (see DESIGN.md).
//!
//! Foreground process, slopd-style: control socket for filestrctl, iroh
//! endpoint for peers. SIGHUP reloads config and rescans shares.

#[cfg(feature = "chat")]
mod chat;
mod ctl_server;
#[cfg(feature = "grayjay")]
mod http_bridge;
mod index;
mod metadata;
mod p2p;
mod priority;
mod search;
mod state;
mod transfers;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use iroh::endpoint::{default_relay_mode, presets};
use iroh::protocol::Router;
use iroh::{Endpoint, RelayMode, SecretKey};
use iroh_blobs::store::fs::FsStore;
use libfilestr::config::{Config, RelaySetting};
use libfilestr::keys::RootKey;
use libfilestr::paths;

use crate::p2p::FilestrProtocol;
use crate::state::{GrantStore, State};

#[derive(Debug, Parser)]
#[command(name = "filestrd", version, about = "filestr daemon")]
struct Args {
    /// Config file (default: ~/.config/filestr/config.toml)
    #[arg(long)]
    config: Option<PathBuf>,
    /// Control socket (default: $XDG_RUNTIME_DIR/filestrd/filestrd.sock)
    #[arg(long)]
    socket: Option<PathBuf>,
    /// Increase log verbosity (-v info, -vv debug, -vvv trace)
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,
}

fn init_tracing(verbose: u8) {
    let default = match verbose {
        0 => "warn",
        1 => "filestrd=info,libfilestr=info",
        2 => "filestrd=debug,libfilestr=debug,iroh=info",
        _ => "trace",
    };
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default));
    // stderr is captured to a log file (e.g. by the Android service), not a
    // TTY — emit plain text, not ANSI colour escapes, so the log stays readable.
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();
}

/// The iroh transport key: a user-supplied `iroh.key` override if present,
/// otherwise derived from the master seed. Overriding lets a node keep a
/// fixed endpoint identity (and the tickets that reference it) while the
/// master seed still drives the nostr identity.
fn load_iroh_key(data_dir: &std::path::Path, root: &RootKey) -> Result<SecretKey> {
    let override_path = data_dir.join("iroh.key");
    match std::fs::read_to_string(&override_path) {
        Ok(text) => {
            libfilestr::keys::ensure_secure_perms(&override_path)?;
            let bytes = data_encoding::HEXLOWER
                .decode(text.trim().as_bytes())
                .context("iroh.key is not valid hex")?;
            let bytes: [u8; 32] =
                bytes.try_into().map_err(|_| anyhow::anyhow!("iroh.key must be 32 bytes"))?;
            tracing::info!("using iroh.key override for endpoint identity");
            Ok(SecretKey::from_bytes(&bytes))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // derived from the nsec one-way (see libfilestr::keys)
            Ok(SecretKey::from_bytes(&root.derive_iroh()))
        }
        Err(e) => Err(e).context("reading iroh.key"),
    }
}

/// Re-read and validate the config file, swapping it in. No rescan — cheap.
pub(crate) async fn apply_config(state: &Arc<State>) -> Result<()> {
    let config = Config::load_or_default(&state.config_path)?;
    *state.config.write().await = config;
    Ok(())
}

/// Rescan shares against the current (in-memory) config, reusing unchanged files
/// from the current index, and swap the new index in. Returns the file count.
pub(crate) async fn rescan_now(state: &Arc<State>) -> Result<usize> {
    let config = state.config.read().await.clone();
    let prev = state.index.read().await.clone();
    let new_index = index::scan(&config, &state.store, &state.thumbs_dir, &prev).await?;
    let files = new_index.files.len();
    *state.index.write().await = new_index;
    state.emit("reloaded", serde_json::json!({ "files": files }));
    Ok(files)
}

/// SIGHUP: re-read config and rescan synchronously.
async fn reload(state: &Arc<State>) {
    if let Err(e) = apply_config(state).await {
        tracing::warn!("config reload failed, keeping old config: {e:#}");
        return;
    }
    match rescan_now(state).await {
        Ok(files) => tracing::info!(files, "config reloaded, share rescanned"),
        Err(e) => tracing::warn!("rescan failed: {e:#}"),
    }
}

fn main() -> Result<()> {
    let args = Args::parse();
    init_tracing(args.verbose);
    // A dedicated low-priority runtime hosts the blob store, so share hashing and
    // blob IO yield to foreground work. The main runtime — control socket,
    // endpoint, search routing — stays at normal priority.
    let blob_runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("filestr-blobs")
        .on_thread_start(priority::lower_current_thread)
        .build()?;
    let main_runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    let result = main_runtime.block_on(run(args, blob_runtime.handle().clone()));
    // keep the blob runtime alive until the main loop returns
    drop(blob_runtime);
    result
}

async fn run(args: Args, blob_rt: tokio::runtime::Handle) -> Result<()> {
    let config_path = args.config.unwrap_or_else(paths::config_path);
    let config = Config::load_or_default(&config_path)
        .with_context(|| format!("loading {}", config_path.display()))?;
    let socket = args
        .socket
        .or_else(|| config.socket.clone())
        .map(|p| paths::expand_path(&p))
        .unwrap_or_else(paths::socket_path);
    // XDG split: identity → data, grants → state, blobs → cache. An explicit
    // `data_dir` config override collapses all three into one root (handy for
    // single-dir or isolated/test deployments).
    let (data_dir, state_dir, cache_dir) = match config.data_dir.clone() {
        Some(p) => {
            let d = paths::expand_path(&p);
            (d.clone(), d.clone(), d)
        }
        None => (paths::data_dir(), paths::state_dir(), paths::cache_dir()),
    };
    for dir in [&data_dir, &state_dir, &cache_dir] {
        libfilestr::keys::ensure_private_dir(dir)?;
    }

    let root = RootKey::load_or_create(&data_dir.join("identity.key"))
        .context("loading identity key")?;
    let secret_key = load_iroh_key(&data_dir, &root)?;
    // Load the store on the low-priority blob runtime so its actor — and the
    // hashing/IO it drives — runs niced, off the main runtime.
    let store = {
        let blobs_path = cache_dir.join("blobs");
        blob_rt
            .spawn(async move { FsStore::load(blobs_path).await })
            .await
            .context("joining blob store load")?
            .context("opening blob store")?
    };
    // Regenerable cache of cover-art thumbnails, keyed by content hash.
    let thumbs_dir = cache_dir.join("thumbs");
    std::fs::create_dir_all(&thumbs_dir).context("creating thumbs dir")?;

    let relay_mode = if !config.relay_urls.is_empty() {
        // self-hosted / custom iroh relays take precedence over the preset
        let mut urls = Vec::new();
        for u in &config.relay_urls {
            urls.push(
                u.parse::<iroh::RelayUrl>().with_context(|| format!("bad relay url {u:?}"))?,
            );
        }
        tracing::info!(count = urls.len(), "using custom iroh relays");
        RelayMode::custom(urls)
    } else {
        match config.relay {
            RelaySetting::Default => default_relay_mode(),
            RelaySetting::Disabled => RelayMode::Disabled,
        }
    };
    // presets::Minimal on purpose: no address lookup at all — this node is
    // never published anywhere; it is dialable only via tickets (DESIGN.md §2)
    #[cfg_attr(not(target_os = "android"), allow(unused_mut))]
    let mut builder = Endpoint::builder(presets::Minimal)
        .secret_key(secret_key)
        .relay_mode(relay_mode);
    // On Android the default DNS resolver reads the system config via
    // `ndk-context`, which panics in a standalone process with no JVM attached.
    // Pin public UDP resolvers so relay hostnames still resolve.
    #[cfg(target_os = "android")]
    {
        use iroh::dns::{DnsProtocol, DnsResolver};
        builder = builder.dns_resolver(
            DnsResolver::builder()
                .with_nameservers([
                    ("8.8.8.8:53".parse().unwrap(), DnsProtocol::Udp),
                    ("1.1.1.1:53".parse().unwrap(), DnsProtocol::Udp),
                ])
                .build(),
        );
    }
    let endpoint = builder.bind().await.context("binding iroh endpoint")?;
    tracing::info!(endpoint_id = %endpoint.id(), "endpoint bound");

    let initial_index = index::scan(&config, &store, &thumbs_dir, &index::Index::default()).await?;

    let grants_path = state_dir.join("grants.json");
    // adopt grants from the pre-split location (data dir) if present
    let legacy_grants = data_dir.join("grants.json");
    let grants = if !grants_path.exists() && legacy_grants.exists() {
        let g = libfilestr::grants::Grants::load_or_default(&legacy_grants)
            .context("loading legacy grants.json")?;
        g.save(&grants_path).context("migrating grants.json to state dir")?;
        tracing::info!("migrated grants.json from data dir to state dir");
        g
    } else {
        libfilestr::grants::Grants::load_or_default(&grants_path)
            .context("loading grants.json")?
    };

    let rep_path = state_dir.join("reputation.json");
    let rep_store = libfilestr::reputation::RepStore::load_or_default(&rep_path)
        .context("loading reputation.json")?;

    // chat plane is opt-out at runtime: default on, but `[chat] enabled =
    // false` runs a pure file-peering node with no nostr (join hubs later by
    // enabling it and restarting).
    #[cfg(feature = "chat")]
    let chat_state = if config.chat.enabled {
        let identity =
            filestr_chat::Identity::from_root(&root).context("building nostr identity")?;
        let mls_key = root.derive(libfilestr::keys::CTX_MLS_DB);
        Some(
            crate::chat::ChatState::open(
                identity,
                state_dir.join("mls.sqlite"),
                mls_key,
                state_dir.join("hubs.json"),
            )
            .context("opening chat state")?,
        )
    } else {
        tracing::info!("chat plane disabled (file peering only)");
        None
    };

    let (events, _) = tokio::sync::broadcast::channel(256);
    let state = Arc::new(State {
        config_path,
        config: tokio::sync::RwLock::new(config),
        grants: tokio::sync::Mutex::new(GrantStore { grants, path: grants_path }),
        endpoint: endpoint.clone(),
        store: store.clone(),
        thumbs_dir,
        index: tokio::sync::RwLock::new(initial_index),
        handles: tokio::sync::Mutex::new(Default::default()),
        seen_queries: tokio::sync::Mutex::new(Default::default()),
        recent_sources: tokio::sync::Mutex::new(Default::default()),
        transfers: tokio::sync::Mutex::new(Default::default()),
        reputation: tokio::sync::Mutex::new(crate::state::RepState {
            store: rep_store,
            path: rep_path,
        }),
        #[cfg(feature = "chat")]
        chat: chat_state,
        #[cfg(feature = "chat")]
        pending_hubs: tokio::sync::Mutex::new(crate::state::PendingHubs::load_or_default(
            state_dir.join("pending_hubs.json"),
        )),
        events,
        shutdown: tokio_util::sync::CancellationToken::new(),
    });

    // single ALPN: transfers ride inside filestr/0 (see DESIGN.md §8), so
    // there is no separately-addressable blobs endpoint to gate.
    let router = Router::builder(endpoint.clone())
        .accept(libfilestr::p2p::ALPN, FilestrProtocol { state: state.clone() })
        .spawn();

    let ctl_task = tokio::spawn(ctl_server::run(state.clone(), socket.clone()));

    // Optional loopback HTTP gateway (e.g. for a Grayjay plugin). Compiled in
    // only with the `grayjay` feature (off by default).
    #[cfg(feature = "grayjay")]
    {
        let listen = state.config.read().await.http.listen.clone();
        if let Some(addr) = listen {
            match addr.parse::<std::net::SocketAddr>() {
                Ok(sa) => {
                    let s = state.clone();
                    tokio::spawn(async move {
                        if let Err(e) = http_bridge::serve(s, sa).await {
                            tracing::error!("http gateway stopped: {e:#}");
                        }
                    });
                }
                Err(e) => tracing::error!("bad [http] listen {addr:?}: {e}"),
            }
        }
    }
    #[cfg(not(feature = "grayjay"))]
    if state.config.read().await.http.listen.is_some() {
        tracing::warn!("[http] listen is set but filestrd was built without the `grayjay` feature; gateway disabled");
    }

    #[cfg(feature = "chat")]
    if state.chat.is_some() {
        crate::chat::spawn_relay_listener(state.clone()).await;
        crate::chat::spawn_dm_listener(state.clone()).await;
        // finish any hub joins queued while chat was disabled
        let s = state.clone();
        tokio::spawn(async move { crate::chat::process_pending_hubs(&s).await });
    }

    let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut sighup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())?;
    loop {
        tokio::select! {
            _ = sigint.recv() => { state.shutdown.cancel(); break }
            _ = sigterm.recv() => { state.shutdown.cancel(); break }
            _ = sighup.recv() => reload(&state).await,
            _ = state.shutdown.cancelled() => break, // ctl shutdown request
        }
    }
    tracing::info!("shutting down");

    router.shutdown().await.ok();
    endpoint.close().await;
    ctl_task.await.ok().transpose().ok();
    Ok(())
}
