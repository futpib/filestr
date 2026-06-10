//! Inbound p2p protocol handler for the `filestr/0` ALPN. Control requests
//! (hello/redeem/list/search) get JSON responses; `get` hands the rest of the
//! stream to the iroh-blobs transfer protocol, serving locally or splicing to
//! an upstream relay (DESIGN.md §7, §8).

use std::sync::Arc;

use iroh::endpoint::{Connection, RecvStream, SendStream};
use iroh::protocol::{AcceptError, ProtocolHandler};
use libfilestr::p2p::{self, DecodedRequest, P2pRequest, P2pResponse, code};
use libfilestr::{FEATURES, PROTO_VERSION, VERSION};
use tokio::sync::mpsc;

use crate::search::{self, HitSource, Requester};
use crate::state::{HandleTarget, State};

/// Read one `\n`-terminated line directly from the stream without buffering
/// past the newline, so the remainder is pristine for the iroh-blobs protocol
/// that may follow on the same stream. Returns `None` at clean EOF.
async fn read_line_raw(recv: &mut RecvStream, max: usize) -> anyhow::Result<Option<String>> {
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        // inherent RecvStream::read returns Ok(None) at end of stream
        match recv.read(&mut byte).await? {
            None | Some(0) => {
                if buf.is_empty() {
                    return Ok(None);
                }
                break;
            }
            Some(_) => {}
        }
        if byte[0] == b'\n' {
            break;
        }
        buf.push(byte[0]);
        if buf.len() > max {
            anyhow::bail!("request line too long");
        }
    }
    Ok(Some(String::from_utf8(buf)?))
}

fn features() -> Vec<String> {
    FEATURES.iter().map(|f| f.to_string()).collect()
}

/// Handler for the `filestr/0` control ALPN.
#[derive(Clone)]
pub struct FilestrProtocol {
    pub state: Arc<State>,
}

impl std::fmt::Debug for FilestrProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FilestrProtocol")
    }
}

impl ProtocolHandler for FilestrProtocol {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        let node_id = connection.remote_id().to_string();
        tracing::debug!(node = %node_id, "p2p connection");
        // one request per bidi stream; a connection may carry many
        loop {
            let (send, recv) = match connection.accept_bi().await {
                Ok(pair) => pair,
                Err(_) => break, // connection closed
            };
            let state = self.state.clone();
            let node_id = node_id.clone();
            let conn_id = connection.stable_id() as u64;
            tokio::spawn(async move {
                if let Err(e) = handle_stream(state, node_id.clone(), conn_id, send, recv).await {
                    tracing::debug!(node = %node_id, "p2p stream error: {e:#}");
                }
            });
        }
        Ok(())
    }
}

async fn write_response(send: &mut SendStream, resp: &P2pResponse) -> anyhow::Result<()> {
    send.write_all(p2p::encode(resp).as_bytes()).await?;
    Ok(())
}

async fn handle_stream(
    state: Arc<State>,
    node_id: String,
    conn_id: u64,
    mut send: SendStream,
    mut recv: RecvStream,
) -> anyhow::Result<()> {
    let line = match read_line_raw(&mut recv, p2p::MAX_LINE).await? {
        Some(line) => line,
        None => return Ok(()), // peer opened a stream and closed it
    };

    let request = match p2p::decode_request(&line) {
        DecodedRequest::Known(request) => request,
        DecodedRequest::Unknown { request_type } => {
            write_response(
                &mut send,
                &P2pResponse::Error {
                    code: code::UNSUPPORTED.into(),
                    message: format!("unknown request type {request_type:?}"),
                },
            )
            .await?;
            send.finish().ok();
            return Ok(());
        }
        DecodedRequest::Malformed { message } => {
            write_response(
                &mut send,
                &P2pResponse::Error { code: code::BAD_REQUEST.into(), message },
            )
            .await?;
            send.finish().ok();
            return Ok(());
        }
    };

    match request {
        // allowed from anyone
        P2pRequest::Hello => {
            write_response(
                &mut send,
                &P2pResponse::Hello {
                    v: PROTO_VERSION,
                    features: features(),
                    version: VERSION.to_string(),
                },
            )
            .await?;
        }
        P2pRequest::Redeem { token } => {
            let redeemed = {
                let mut grants = state.grants.lock().await;
                let result = grants
                    .grants
                    .redeem(&token, &node_id)
                    .map(|g| (g.token_id.clone(), g.view.clone(), g.allow_reshare));
                if result.is_some() {
                    grants.save()?;
                }
                result
            };
            match redeemed {
                Some((token_id, view, allow_reshare)) => {
                    state.emit(
                        "grant_redeemed",
                        serde_json::json!({ "token_id": token_id, "node_id": node_id }),
                    );
                    write_response(
                        &mut send,
                        &P2pResponse::Redeemed {
                            view,
                            allow_reshare,
                            v: PROTO_VERSION,
                            features: features(),
                        },
                    )
                    .await?;
                }
                None => {
                    write_response(
                        &mut send,
                        &P2pResponse::Error {
                            code: code::DENIED.into(),
                            message: "invalid, expired, or already-redeemed token".into(),
                        },
                    )
                    .await?;
                }
            }
        }
        // everything else requires an active grant
        other => {
            let grant = {
                let grants = state.grants.lock().await;
                grants.grants.active_for(&node_id).cloned()
            };
            let Some(grant) = grant else {
                write_response(
                    &mut send,
                    &P2pResponse::Error {
                        code: code::DENIED.into(),
                        message: "no active grant for your node".into(),
                    },
                )
                .await?;
                send.finish().ok();
                return Ok(());
            };
            match other {
                P2pRequest::List => {
                    let roots = {
                        let config = state.config.read().await;
                        config.view_roots(&grant.view).unwrap_or_default()
                    };
                    let entries = state.index.read().await.entries(&roots);
                    let total = entries.len() as u64;
                    for chunk in entries.chunks(500) {
                        write_response(&mut send, &P2pResponse::Entries { entries: chunk.to_vec() })
                            .await?;
                    }
                    write_response(&mut send, &P2pResponse::ListDone { total }).await?;
                }
                P2pRequest::Search { query_id, ttl, query } => {
                    handle_search(&state, &grant.view, &mut send, query_id, ttl, query).await?;
                }
                P2pRequest::Get { handle } => {
                    // serve from our store, or splice through to the upstream
                    // the handle points at — no buffering either way (§7.3)
                    let target = match &handle {
                        Some(h) => state.handles.lock().await.resolve(h),
                        None => None,
                    };
                    return match target {
                        Some(HandleTarget::Remote { peer, upstream }) => {
                            search::relay_get(&state, &peer, upstream, recv, send).await
                        }
                        _ => search::serve_local(&state, conn_id, recv, send).await,
                    };
                }
                P2pRequest::Nostr => {
                    #[cfg(feature = "chat")]
                    {
                        return crate::chat::serve_nostr(&state, recv, send).await;
                    }
                    #[cfg(not(feature = "chat"))]
                    {
                        write_response(
                            &mut send,
                            &P2pResponse::Error {
                                code: code::UNSUPPORTED.into(),
                                message: "nostr-over-iroh tunnel not enabled on this node".into(),
                            },
                        )
                        .await?;
                    }
                }
                P2pRequest::Hub { payload } => {
                    #[cfg(feature = "chat")]
                    {
                        let reply = crate::chat::handle_hub_rpc(&state, &payload).await;
                        write_response(&mut send, &P2pResponse::HubReply { payload: reply }).await?;
                    }
                    #[cfg(not(feature = "chat"))]
                    {
                        let _ = payload;
                        write_response(
                            &mut send,
                            &P2pResponse::Error {
                                code: code::UNSUPPORTED.into(),
                                message: "hub/chat not enabled on this node".into(),
                            },
                        )
                        .await?;
                    }
                }
                P2pRequest::Hello | P2pRequest::Redeem { .. } => unreachable!(),
            }
        }
    }
    send.finish().ok();
    Ok(())
}

async fn handle_search(
    state: &Arc<State>,
    view: &str,
    send: &mut SendStream,
    query_id: String,
    ttl: u8,
    query: String,
) -> anyhow::Result<()> {
    // loop prevention: drop repeated query ids (DESIGN.md §6)
    let fresh = state.seen_queries.lock().await.check_and_insert(&query_id);
    if !fresh {
        write_response(send, &P2pResponse::SearchDone).await?;
        return Ok(());
    }

    let (view_roots, max_ttl, result_cap) = {
        let config = state.config.read().await;
        (
            config.view_roots(view).unwrap_or_default(),
            config.search.max_ttl,
            config.search.result_cap,
        )
    };
    let ttl = ttl.min(max_ttl);

    let (tx, mut rx) = mpsc::channel::<search::Hit>(64);
    let search_task = tokio::spawn(search::run_search(
        state.clone(),
        query_id,
        query,
        ttl,
        Requester::Peer { view_roots },
        tx,
    ));

    let mut count = 0usize;
    while let Some(hit) = rx.recv().await {
        if count >= result_cap {
            break;
        }
        // re-attribution: mint our own handle, emit no origin (§7.1)
        let target = match hit.source {
            HitSource::Local => HandleTarget::Local,
            HitSource::Upstream { peer, handle } => {
                HandleTarget::Remote { peer, upstream: handle }
            }
        };
        let handle = state.handles.lock().await.mint(target);
        write_response(
            send,
            &P2pResponse::Hit { name: hit.name, size: hit.size, hash: hit.hash, handle },
        )
        .await?;
        count += 1;
    }
    drop(rx);
    search_task.abort();
    write_response(send, &P2pResponse::SearchDone).await?;
    Ok(())
}
