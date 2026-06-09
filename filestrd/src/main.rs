//! filestrd — friend-to-friend file sharing daemon (see DESIGN.md).
//!
//! Foreground process, slopd-style: control socket for filestrctl, iroh
//! endpoint for peers. SIGHUP reloads config and rescans shares.

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

fn load_secret_key(data_dir: &std::path::Path) -> Result<SecretKey> {
    let path = data_dir.join("secret.key");
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let bytes = data_encoding::HEXLOWER
                .decode(text.trim().as_bytes())
                .context("secret.key is not valid hex")?;
            let bytes: [u8; 32] =
                bytes.try_into().map_err(|_| anyhow::anyhow!("secret.key has wrong length"))?;
            Ok(SecretKey::from_bytes(&bytes))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let key = SecretKey::generate();
            let encoded = data_encoding::HEXLOWER.encode(&key.to_bytes());
            std::fs::write(&path, format!("{encoded}\n"))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
            }
            tracing::info!("generated new endpoint secret key");
            Ok(key)
        }
        Err(e) => Err(e).context("reading secret.key"),
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
    let data_dir = config
        .data_dir
        .clone()
        .map(|p| paths::expand_path(&p))
        .unwrap_or_else(paths::data_dir);
    std::fs::create_dir_all(&data_dir)
        .with_context(|| format!("creating {}", data_dir.display()))?;

    let secret_key = load_secret_key(&data_dir)?;
    let store = FsStore::load(data_dir.join("blobs")).await.context("opening blob store")?;

    let relay_mode = match config.relay {
        RelaySetting::Default => default_relay_mode(),
        RelaySetting::Disabled => RelayMode::Disabled,
    };
    // presets::Minimal on purpose: no address lookup at all — this node is
    // never published anywhere; it is dialable only via tickets (DESIGN.md §2)
    let endpoint = Endpoint::builder(presets::Minimal)
        .secret_key(secret_key)
        .relay_mode(relay_mode)
        .bind()
        .await
        .context("binding iroh endpoint")?;
    tracing::info!(endpoint_id = %endpoint.id(), "endpoint bound");

    let initial_index = index::scan(&config, &store).await?;

    let grants_path = data_dir.join("grants.json");
    let grants = libfilestr::grants::Grants::load_or_default(&grants_path)
        .context("loading grants.json")?;

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
        events,
        shutdown: tokio_util::sync::CancellationToken::new(),
    });

    // single ALPN: transfers ride inside filestr/0 (see DESIGN.md §8), so
    // there is no separately-addressable blobs endpoint to gate.
    let router = Router::builder(endpoint.clone())
        .accept(libfilestr::p2p::ALPN, FilestrProtocol { state: state.clone() })
        .spawn();

    let ctl_task = tokio::spawn(ctl_server::run(state.clone(), socket.clone()));

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
