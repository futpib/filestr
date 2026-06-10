//! filestrd — friend-to-friend file sharing daemon (see DESIGN.md).
//!
//! Foreground process, slopd-style: control socket for filestrctl, iroh
//! endpoint for peers. SIGHUP reloads config and rescans shares.

#[cfg(feature = "chat")]
mod chat;
mod ctl_server;
mod index;
mod p2p;
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
    tracing_subscriber::fmt().with_env_filter(filter).with_writer(std::io::stderr).init();
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

async fn reload(state: &Arc<State>) {
    match Config::load_or_default(&state.config_path) {
        Ok(config) => {
            match index::scan(&config, &state.store).await {
                Ok(new_index) => {
                    let files = new_index.files.len();
                    *state.index.write().await = new_index;
                    *state.config.write().await = config;
                    state.emit("reloaded", serde_json::json!({ "files": files }));
                    tracing::info!(files, "config reloaded, share rescanned");
                }
                Err(e) => tracing::warn!("rescan failed, keeping old index: {e:#}"),
            }
        }
        Err(e) => tracing::warn!("config reload failed, keeping old config: {e}"),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    init_tracing(args.verbose);

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
    let store = FsStore::load(cache_dir.join("blobs")).await.context("opening blob store")?;

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

    let initial_index = index::scan(&config, &store).await?;

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
