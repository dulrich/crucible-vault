//! The Unix-socket JSON-RPC server (plan Public Interfaces "Transport and handshake").
//!
//! Newline-delimited JSON-RPC 2.0, one task per connection. `hello`/`subscribe` run inline (they
//! change connection state); every other request runs in the connection's own task set and its
//! reply goes back through the single writer, so a slow call never blocks the connection's other
//! replies or notifications (ids are echoed; out-of-order replies are fine). Any call before a successful `hello`
//! gets `HANDSHAKE_REQUIRED`; a `hello` with another `protocol_version` gets `VERSION_MISMATCH`
//! (the connection stays usable for a retry, never a degraded session). Notifications go only to
//! connections subscribed to their topic; a lagging subscriber skips (logged), never blocks.

use std::collections::HashSet;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use anyhow::{Context, Result};
use cv_protocol::{
    JSONRPC_VERSION, METHOD_NAMES, PROTOCOL_VERSION, Request, RpcError, RpcId, RpcNotification,
    RpcOutcome, RpcRequest, RpcResponse, SubscribeResult, Topic, error_codes,
};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::JoinSet;

use crate::daemon::{Shared, clear_stale_socket, rpc_err};

/// Binds `socket` (stale file removed; a live daemon is an error) with mode 0600.
pub fn bind(socket: &Path) -> Result<UnixListener> {
    clear_stale_socket(socket)?;
    let l = UnixListener::bind(socket).with_context(|| format!("binding {}", socket.display()))?;
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("chmod {}", socket.display()))?;
    Ok(l)
}

/// Accepts until `stop` flips, then waits for every connection task to end.
pub async fn serve(listener: UnixListener, shared: Arc<Shared>, mut stop: watch::Receiver<bool>) {
    let mut conns = JoinSet::new();
    loop {
        tokio::select! {
            _ = stop.changed() => break,
            r = listener.accept() => match r {
                Ok((stream, _)) => {
                    conns.spawn(connection(stream, Arc::clone(&shared), stop.clone()));
                }
                Err(e) => tracing::warn!(error = %e, "accept"),
            },
            Some(_) = conns.join_next(), if !conns.is_empty() => {}
        }
    }
    drop(listener);
    while conns.join_next().await.is_some() {}
}

/// Decrements the `open_requested` subscriber count when a subscribed connection ends.
struct OpenSubGuard<'a>(&'a Shared, bool);

impl Drop for OpenSubGuard<'_> {
    fn drop(&mut self) {
        if self.1 {
            self.0.open_subscribers().fetch_sub(1, Ordering::SeqCst);
        }
    }
}

fn response(id: RpcId, outcome: Result<Value, RpcError>) -> Value {
    let outcome = match outcome {
        Ok(v) => RpcOutcome::Result(v),
        Err(e) => RpcOutcome::Error(e),
    };
    serde_json::to_value(RpcResponse {
        jsonrpc: JSONRPC_VERSION.to_owned(),
        id,
        outcome,
    })
    .unwrap_or_else(|e| json!({"jsonrpc": JSONRPC_VERSION, "id": null, "error": {"code": error_codes::INTERNAL_ERROR, "message": e.to_string()}}))
}

fn null_id_error(code: i64, message: &str) -> Value {
    json!({"jsonrpc": JSONRPC_VERSION, "id": null, "error": {"code": code, "message": message}})
}

/// What one request line asks of the connection.
enum Line {
    /// Answer now (parse errors, `hello`, `subscribe`, handshake refusals).
    Reply(Value),
    /// Run through [`Shared::handle`] off the read loop.
    Dispatch(RpcId, Request),
}

async fn connection(stream: UnixStream, shared: Arc<Shared>, mut stop: watch::Receiver<bool>) {
    let (rd, mut wr) = stream.into_split();
    let mut lines = BufReader::new(rd).lines();
    let mut events = shared.subscribe();
    let mut topics: HashSet<Topic> = HashSet::new();
    let mut greeted = false;
    let mut guard = OpenSubGuard(&shared, false);
    let (reply_tx, mut replies) = mpsc::unbounded_channel::<Value>();
    let mut calls = JoinSet::new();
    loop {
        let out: Value = tokio::select! {
            _ = stop.changed() => break,
            Some(v) = replies.recv() => v,
            Some(_) = calls.join_next(), if !calls.is_empty() => continue,
            ev = events.recv(), if !topics.is_empty() => match ev {
                Ok(n) if topics.contains(&n.topic()) => match serde_json::to_value(RpcNotification::new(n)) {
                    Ok(v) => v,
                    Err(e) => { tracing::warn!(error = %e, "encoding notification"); continue; }
                },
                Ok(_) => continue,
                Err(broadcast::error::RecvError::Lagged(k)) => {
                    tracing::warn!(skipped = k, "subscriber lagged; notifications dropped");
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
            line = lines.next_line() => match line {
                Ok(Some(l)) if l.trim().is_empty() => continue,
                Ok(Some(l)) => {
                    let was_empty = topics.is_empty();
                    let step = handle_line(&l, &shared, &mut greeted, &mut topics, &mut guard);
                    if was_empty && !topics.is_empty() {
                        // Start the feed at subscription time: no backlog from before it.
                        events = shared.subscribe();
                    }
                    match step {
                        Line::Reply(v) => v,
                        Line::Dispatch(id, req) => {
                            let (shared, tx) = (Arc::clone(&shared), reply_tx.clone());
                            calls.spawn(async move {
                                let _ = tx.send(response(id, shared.handle(req).await));
                            });
                            continue;
                        }
                    }
                }
                Ok(None) => break,
                Err(e) => { tracing::debug!(error = %e, "connection read"); break; }
            },
        };
        let mut buf = out.to_string();
        buf.push('\n');
        if let Err(e) = wr.write_all(buf.as_bytes()).await {
            tracing::debug!(error = %e, "connection write");
            break;
        }
    }
}

fn handle_line(
    line: &str,
    shared: &Shared,
    greeted: &mut bool,
    topics: &mut HashSet<Topic>,
    guard: &mut OpenSubGuard<'_>,
) -> Line {
    let v: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => return Line::Reply(null_id_error(error_codes::PARSE_ERROR, &e.to_string())),
    };
    let req: RpcRequest = match serde_json::from_value(v.clone()) {
        Ok(r) => r,
        Err(e) => {
            let Some(id) = v
                .get("id")
                .and_then(|i| serde_json::from_value::<RpcId>(i.clone()).ok())
            else {
                return Line::Reply(null_id_error(error_codes::INVALID_REQUEST, &e.to_string()));
            };
            let method = v.get("method").and_then(Value::as_str).unwrap_or("");
            let err = if METHOD_NAMES.contains(&method) {
                rpc_err(error_codes::INVALID_PARAMS, e.to_string())
            } else {
                rpc_err(
                    error_codes::METHOD_NOT_FOUND,
                    format!("unknown method {method:?}"),
                )
            };
            return Line::Reply(response(id, Err(err)));
        }
    };
    let id = req.id;
    let outcome = match req.call {
        Request::Hello(h) => {
            if h.protocol_version == PROTOCOL_VERSION {
                *greeted = true;
                tracing::debug!(client = %h.client, "hello");
                serde_json::to_value(shared.hello())
                    .map_err(|e| rpc_err(error_codes::INTERNAL_ERROR, e.to_string()))
            } else {
                Err(rpc_err(
                    error_codes::VERSION_MISMATCH,
                    format!(
                        "protocol_version {} is not the daemon's {PROTOCOL_VERSION}",
                        h.protocol_version
                    ),
                ))
            }
        }
        _ if !*greeted => Err(rpc_err(error_codes::HANDSHAKE_REQUIRED, "call hello first")),
        Request::Subscribe(s) => {
            topics.extend(s.topics);
            if topics.contains(&Topic::OpenRequested) && !guard.1 {
                guard.1 = true;
                shared.open_subscribers().fetch_add(1, Ordering::SeqCst);
            }
            serde_json::to_value(SubscribeResult {
                topics: topics.iter().copied().collect(),
            })
            .map_err(|e| rpc_err(error_codes::INTERNAL_ERROR, e.to_string()))
        }
        other => return Line::Dispatch(id, other),
    };
    Line::Reply(response(id, outcome))
}
