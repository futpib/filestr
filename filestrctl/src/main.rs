//! filestrctl — CLI for filestrd, slopctl-style.

use std::path::PathBuf;

use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, Subcommand};
use libfilestr::ctl::{RequestBody, ResponseBody, SearchHit};
use libfilestrctl::Client;

#[derive(Debug, Parser)]
#[command(name = "filestrctl", version, about = "control the filestr daemon")]
struct Cli {
    /// Control socket (default: $XDG_RUNTIME_DIR/filestrd/filestrd.sock)
    #[arg(long, global = true)]
    socket: Option<PathBuf>,
    /// Print raw JSON responses
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Daemon and endpoint status
    Status,
    /// Manage invites (outgoing grants)
    Invite {
        #[command(subcommand)]
        command: InviteCommand,
    },
    /// Manage peers (incoming grants) and issued grants
    Peer {
        #[command(subcommand)]
        command: PeerCommand,
    },
    /// Show shared directories and views
    Share {
        #[command(subcommand)]
        command: ShareCommand,
    },
    /// Rescan share roots
    Rescan,
    /// Fetch a peer's file list
    Browse { peer: String },
    /// Search the grant graph; results stream in as peers answer
    Search {
        /// Search terms (AND, case-insensitive)
        #[arg(required = true)]
        query: Vec<String>,
        /// Hop cap (default: daemon's search.max_ttl)
        #[arg(long)]
        ttl: Option<u8>,
    },
    /// Download by hash, using recent search/browse results to pick a source
    Get {
        hash: String,
        /// Output file (default: ./<hash prefix>)
        #[arg(short, long)]
        out: Option<PathBuf>,
        /// Prefer this peer (node id prefix or label)
        #[arg(long)]
        peer: Option<String>,
        /// Inclusive byte range "START-END" or "START-" (whole file if absent)
        #[arg(long)]
        range: Option<String>,
        /// Start the download in the background and return its id immediately
        #[arg(short, long)]
        background: bool,
    },
    /// List transfers (queued/active/done/failed)
    Transfers,
    /// Cancel a transfer by id
    Cancel { id: u64 },
    /// Hubs: nostr/MLS group chat (requires a chat-enabled daemon)
    Hub {
        #[command(subcommand)]
        command: HubCommand,
    },
    /// Stream daemon events
    Listen,
    /// Stop the daemon
    Shutdown,
}

#[derive(Debug, Subcommand)]
enum HubCommand {
    /// Create a hub you own
    Create { name: String },
    /// Mint a join ticket for a hub you own
    Invite { hub: String },
    /// Join a hub from a filestrhub1… ticket
    Join { ticket: String },
    /// List hubs you own or have joined
    Ls,
    /// List a hub's members
    Members { hub: String },
    /// Send a chat message
    Send {
        hub: String,
        #[arg(required = true)]
        message: Vec<String>,
    },
    /// Show a hub's chat log
    Log { hub: String },
}

#[derive(Debug, Subcommand)]
enum InviteCommand {
    /// Mint a single-use invite ticket
    Create {
        /// Share view the invitee will see (default: full)
        #[arg(long)]
        view: Option<String>,
        #[arg(long)]
        label: Option<String>,
        /// Ask the invitee's client not to re-serve your content
        #[arg(long)]
        no_reshare: bool,
        /// Omit direct addresses from the ticket (relay only)
        #[arg(long)]
        relay_only: bool,
    },
    /// List invites and their states
    Ls,
    /// Revoke an invite / grant by token id
    Revoke { token_id: String },
}

#[derive(Debug, Subcommand)]
enum PeerCommand {
    /// Redeem a ticket: enroll with the grantor as a peer
    Add {
        ticket: String,
        #[arg(long)]
        label: Option<String>,
    },
    /// List grants we issued and peers we redeemed
    Ls,
    /// Revoke a grant or drop a peer (node id prefix, token id, or label)
    Revoke { peer: String },
}

#[derive(Debug, Subcommand)]
enum ShareCommand {
    /// List share roots and views
    Ls,
}

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 { format!("{bytes} B") } else { format!("{value:.1} {}", UNITS[unit]) }
}

fn print_json<T: serde::Serialize>(value: &T) {
    println!("{}", serde_json::to_string(value).expect("serializable"));
}

fn print_hit(hit: &SearchHit, json: bool) {
    if json {
        print_json(hit);
        return;
    }
    let via = hit.via.as_deref().map(|v| &v[..v.len().min(12)]).unwrap_or("local");
    println!("{:>10}  {:<12}  {}  {}", human_size(hit.size), via, hit.hash, hit.name);
}

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let cli = Cli::parse();
    let socket = cli.socket.clone().unwrap_or_else(libfilestr::paths::socket_path);
    let mut client = Client::connect(&socket).await.with_context(|| {
        format!("connecting to {} (is filestrd running?)", socket.display())
    })?;

    match cli.command {
        Command::Status => {
            let response = client.roundtrip(RequestBody::Status).await?;
            let ResponseBody::Status { status } = response else {
                bail!("unexpected response");
            };
            if cli.json {
                print_json(&status);
            } else {
                println!("endpoint id:   {}", status.endpoint_id);
                println!("relays:        {}", status.relays.join(", "));
                println!("direct addrs:  {}", status.direct_addrs.join(", "));
                println!("shared files:  {}", status.files);
                println!(
                    "grants:        {} active, {} issued",
                    status.grants_active, status.grants_issued
                );
                println!("peers:         {}", status.peers);
                println!("version:       {}", status.version);
            }
        }
        Command::Invite { command } => match command {
            InviteCommand::Create { view, label, no_reshare, relay_only } => {
                let response = client
                    .roundtrip(RequestBody::InviteCreate {
                        view,
                        label,
                        allow_reshare: if no_reshare { Some(false) } else { None },
                        relay_only: if relay_only { Some(true) } else { None },
                    })
                    .await?;
                let ResponseBody::InviteCreated { ticket, token_id } = response else {
                    bail!("unexpected response");
                };
                if cli.json {
                    print_json(&serde_json::json!({ "ticket": ticket, "token_id": token_id }));
                } else {
                    println!("{ticket}");
                    eprintln!("token id: {token_id} (single use; revoke with: filestrctl invite revoke {token_id})");
                }
            }
            InviteCommand::Ls => {
                let response = client.roundtrip(RequestBody::InviteList).await?;
                let ResponseBody::Invites { invites } = response else {
                    bail!("unexpected response");
                };
                if cli.json {
                    print_json(&invites);
                } else {
                    for invite in invites {
                        println!(
                            "{:<10} {:<8} view={:<10} reshare={:<5} {} {}",
                            invite.token_id,
                            invite.state,
                            invite.view,
                            invite.allow_reshare,
                            invite.node_id.as_deref().unwrap_or("-"),
                            invite.label.as_deref().unwrap_or(""),
                        );
                    }
                }
            }
            InviteCommand::Revoke { token_id } => {
                let response = client.roundtrip(RequestBody::InviteRevoke { token_id }).await?;
                print_revoked(response, cli.json)?;
            }
        },
        Command::Peer { command } => match command {
            PeerCommand::Add { ticket, label } => {
                let response = client.roundtrip(RequestBody::PeerAdd { ticket, label }).await?;
                let ResponseBody::PeerAdded { peer } = response else {
                    bail!("unexpected response");
                };
                if cli.json {
                    print_json(&peer);
                } else {
                    println!(
                        "peer added: {} (reshare allowed: {})",
                        peer.node_id, peer.allow_reshare
                    );
                }
            }
            PeerCommand::Ls => {
                let response = client.roundtrip(RequestBody::PeerList).await?;
                let ResponseBody::Peers { grants, peers } = response else {
                    bail!("unexpected response");
                };
                if cli.json {
                    print_json(&serde_json::json!({ "grants": grants, "peers": peers }));
                } else {
                    println!("grants issued (they can access our share):");
                    for grant in grants {
                        println!(
                            "  {:<10} {:<8} view={:<10} {} {}",
                            grant.token_id,
                            grant.state,
                            grant.view,
                            grant.node_id.as_deref().unwrap_or("-"),
                            grant.label.as_deref().unwrap_or(""),
                        );
                    }
                    println!("peers (we can access their share):");
                    for peer in peers {
                        println!(
                            "  {} reshare={} {}",
                            peer.node_id,
                            peer.allow_reshare,
                            peer.label.as_deref().unwrap_or(""),
                        );
                    }
                }
            }
            PeerCommand::Revoke { peer } => {
                let response = client.roundtrip(RequestBody::PeerRevoke { peer }).await?;
                print_revoked(response, cli.json)?;
            }
        },
        Command::Share { command } => match command {
            ShareCommand::Ls => {
                let response = client.roundtrip(RequestBody::ShareList).await?;
                let ResponseBody::Shares { files, shares, views } = response else {
                    bail!("unexpected response");
                };
                if cli.json {
                    print_json(&serde_json::json!({
                        "files": files, "shares": shares, "views": views
                    }));
                } else {
                    for share in shares {
                        println!(
                            "{:<12} {:<6} files  {:>10}  {}",
                            share.name,
                            share.files,
                            human_size(share.bytes),
                            share.path.display()
                        );
                    }
                    for view in views {
                        println!("view {:<10} = [{}]", view.name, view.roots.join(", "));
                    }
                }
            }
        },
        Command::Rescan => {
            let response = client.roundtrip(RequestBody::Rescan).await?;
            let ResponseBody::Rescanned { files } = response else {
                bail!("unexpected response");
            };
            if cli.json {
                print_json(&serde_json::json!({ "files": files }));
            } else {
                println!("rescanned: {files} files");
            }
        }
        Command::Browse { peer } => {
            let response = client.roundtrip(RequestBody::Browse { peer }).await?;
            let ResponseBody::Entries { entries } = response else {
                bail!("unexpected response");
            };
            if cli.json {
                print_json(&entries);
            } else {
                for entry in entries {
                    println!("{:>10}  {}  {}", human_size(entry.size), entry.hash, entry.path);
                }
            }
        }
        Command::Search { query, ttl } => {
            let id = client
                .send(RequestBody::Search { query: query.join(" "), ttl })
                .await?;
            loop {
                match client.recv(id).await? {
                    ResponseBody::SearchHit { hit } => print_hit(&hit, cli.json),
                    ResponseBody::SearchDone { hits } => {
                        if !cli.json {
                            eprintln!("{hits} result(s)");
                        }
                        break;
                    }
                    other => bail!("unexpected response: {other:?}"),
                }
            }
        }
        Command::Get { hash, out, peer, range, background } => {
            let out = out.unwrap_or_else(|| {
                PathBuf::from(hash.get(..16).unwrap_or(hash.as_str()).to_string())
            });
            let out = if out.is_absolute() {
                out
            } else {
                std::env::current_dir()
                    .map_err(|e| anyhow!("cannot resolve cwd: {e}"))?
                    .join(out)
            };
            let id = client
                .send(RequestBody::Get { hash, out, peer, range, background })
                .await?;
            loop {
                match client.recv(id).await? {
                    ResponseBody::TransferStarted { id: tid } => {
                        if cli.json {
                            print_json(&serde_json::json!({ "transfer": tid }));
                        } else {
                            println!("transfer {tid} started (watch: filestrctl transfers)");
                        }
                        break;
                    }
                    ResponseBody::GetProgress { transferred, total } => {
                        if !cli.json {
                            eprintln!("{} / {}", human_size(transferred), human_size(total));
                        }
                    }
                    ResponseBody::GetDone { path, hash, size } => {
                        if cli.json {
                            print_json(&serde_json::json!({
                                "path": path, "hash": hash, "size": size
                            }));
                        } else {
                            println!("{} ({})", path.display(), human_size(size));
                        }
                        break;
                    }
                    other => bail!("unexpected response: {other:?}"),
                }
            }
        }
        Command::Transfers => {
            let response = client.roundtrip(RequestBody::Transfers).await?;
            let ResponseBody::Transfers { transfers } = response else {
                bail!("unexpected response");
            };
            if cli.json {
                print_json(&transfers);
            } else {
                for t in transfers {
                    let progress = if t.total > 0 {
                        format!("{}/{}", human_size(t.transferred), human_size(t.total))
                    } else {
                        human_size(t.transferred)
                    };
                    let range = t
                        .range
                        .map(|[s, e]| format!(" [{s}-{}]", if e == u64::MAX { "".into() } else { e.to_string() }))
                        .unwrap_or_default();
                    println!(
                        "{:<4} {:<9} {:<16} {} {}{}",
                        t.id,
                        t.status,
                        progress,
                        t.hash,
                        t.out.display(),
                        range,
                    );
                    if let Some(err) = t.error {
                        println!("       error: {err}");
                    }
                }
            }
        }
        Command::Cancel { id } => {
            let response = client.roundtrip(RequestBody::TransferCancel { id }).await?;
            let ResponseBody::TransferCancelled { id } = response else {
                bail!("unexpected response");
            };
            if !cli.json {
                println!("cancelled transfer {id}");
            }
        }
        Command::Hub { command } => run_hub(&mut client, cli.json, command).await?,
        Command::Listen => {
            let id = client.send(RequestBody::Subscribe).await?;
            loop {
                match client.recv(id).await? {
                    ResponseBody::Subscribed => continue,
                    ResponseBody::Event { event } => print_json(&event),
                    other => bail!("unexpected response: {other:?}"),
                }
            }
        }
        Command::Shutdown => {
            let response = client.roundtrip(RequestBody::Shutdown).await?;
            let ResponseBody::ShuttingDown = response else {
                bail!("unexpected response");
            };
            if !cli.json {
                println!("daemon shutting down");
            }
        }
    }
    Ok(())
}

async fn run_hub(client: &mut Client, json: bool, command: HubCommand) -> Result<()> {
    match command {
        HubCommand::Create { name } => {
            let response = client.roundtrip(RequestBody::HubCreate { name }).await?;
            let ResponseBody::HubCreated { hub } = response else { bail!("unexpected response") };
            if json {
                print_json(&hub);
            } else {
                println!("created hub {} ({})", hub.name, hub.group_ref);
            }
        }
        HubCommand::Invite { hub } => {
            let response = client.roundtrip(RequestBody::HubInvite { hub }).await?;
            let ResponseBody::HubInvite { ticket } = response else { bail!("unexpected response") };
            if json {
                print_json(&serde_json::json!({ "ticket": ticket }));
            } else {
                println!("{ticket}");
            }
        }
        HubCommand::Join { ticket } => {
            let response = client.roundtrip(RequestBody::HubJoin { ticket }).await?;
            let ResponseBody::HubJoined { hub } = response else { bail!("unexpected response") };
            if json {
                print_json(&hub);
            } else {
                println!("joined hub {} ({})", hub.name, hub.group_ref);
            }
        }
        HubCommand::Ls => {
            let response = client.roundtrip(RequestBody::HubList).await?;
            let ResponseBody::Hubs { hubs } = response else { bail!("unexpected response") };
            if json {
                print_json(&hubs);
            } else {
                for h in hubs {
                    let role = if h.owner { "owner" } else { "member" };
                    println!("{:<6} {:<3} members  {:<20} {}", role, h.members, h.name, h.group_ref);
                }
            }
        }
        HubCommand::Members { hub } => {
            let response = client.roundtrip(RequestBody::HubMembers { hub }).await?;
            let ResponseBody::HubMembers { members } = response else {
                bail!("unexpected response")
            };
            if json {
                print_json(&members);
            } else {
                for m in members {
                    println!("{m}");
                }
            }
        }
        HubCommand::Send { hub, message } => {
            let response =
                client.roundtrip(RequestBody::HubSend { hub, text: message.join(" ") }).await?;
            let ResponseBody::HubSent = response else { bail!("unexpected response") };
            if !json {
                println!("sent");
            }
        }
        HubCommand::Log { hub } => {
            let response = client.roundtrip(RequestBody::HubLog { hub }).await?;
            let ResponseBody::HubMessages { messages } = response else {
                bail!("unexpected response")
            };
            if json {
                print_json(&messages);
            } else {
                for m in messages {
                    let who = &m.author[..m.author.len().min(12)];
                    println!("{who}  {}", m.content);
                }
            }
        }
    }
    Ok(())
}

fn print_revoked(response: ResponseBody, json: bool) -> Result<()> {
    let ResponseBody::PeerRevoked { revoked } = response else {
        bail!("unexpected response");
    };
    if json {
        print_json(&revoked);
    } else {
        for item in revoked {
            println!("revoked: {item}");
        }
    }
    Ok(())
}
