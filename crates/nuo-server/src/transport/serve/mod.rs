//! The server WebSocket / local IPC control plane.
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU16, Ordering};
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use crate::runtime::shutdown::ShutdownGate;

pub mod auth;
pub mod conn_table;
pub mod connection;
pub mod options;
pub mod sync_buffer;
pub mod tcp;

#[cfg(test)]
mod tests;

pub use auth::{
    check_bearer, check_credentials, constant_time_eq, generate_token, host_part, is_loopback_host,
    reject_forbidden, reject_unauthorized, validate_origin,
};
pub use conn_table::{ConnTable, InteractiveConnGuard};
use connection::{ServeCtx, spawn_local_connection, spawn_tcp_connection};
pub use nuo_protocol::{AttachAction, ControlRequest, Wire, WireFrame};
pub use options::{
    ServeExpose, ServeHandle, ServeOptions, Startup, StartupParts, protocol_mismatch_error,
    server_version,
};
pub use sync_buffer::AttachSyncBuffer;
pub(crate) use sync_buffer::{build_full_session_snapshot, is_attach_sync_event};
use tcp::TCP_MANAGER;
pub use tcp::{
    ACCEPT_BACKOFF_CAP, TcpManager, bind_tcp, close_dynamic_tcp, get_current_endpoints,
    open_dynamic_tcp,
};

pub fn start_server(
    opts: ServeOptions,
    registry: Arc<crate::runtime::registry::ThreadRegistry>,
) -> ServeHandle {
    let (actual_port_tx, actual_port_rx) =
        tokio::sync::oneshot::channel::<Result<u16, std::io::Error>>();
    let cancel = CancellationToken::new();
    let conns = Arc::new(ConnTable::new());
    let tasks = Arc::new(crate::runtime::shutdown::TaskBook::new());
    // The serve gate carries the server's version from the start so the
    // handshake refusal names it even when the serve layer runs standalone
    // (host::run attaches its own run-loop gate on top of this one).
    let gate = Arc::new(ShutdownGate::new().with_version(server_version()));
    let token = opts.token.clone().or_else(|| match opts.expose {
        ServeExpose::Public => Some(generate_token()),
        // Loopback auth: generated per start, published owner-only
        // in the discovery record; Rust clients read and present it, browser
        // clients use the `bearer.` subprotocol.
        ServeExpose::Local if opts.local_auth => Some(generate_token()),
        ServeExpose::Local => None,
    });
    let bind_addr: SocketAddr = match opts.expose {
        ServeExpose::Local => ([127, 0, 0, 1], opts.port).into(),
        ServeExpose::Public => ([0, 0, 0, 0], opts.port).into(),
    };
    let port_fallback = opts.port_fallback;
    let expose = opts.expose;
    let tcp_mgr = Arc::new(TcpManager {
        port: Arc::new(AtomicU16::new(0)),
        token: Arc::new(RwLock::new(token.clone())),
        cancel: Arc::new(RwLock::new(None)),
        expose,
        local_endpoint: opts.local_endpoint.clone(),
        registry: registry.clone(),
        conns: conns.clone(),
        gate: gate.clone(),
        tasks: tasks.clone(),
    });
    if let Ok(mut lock) = TCP_MANAGER.write() {
        *lock = Some(tcp_mgr.clone());
    }

    if opts.disable_tcp {
        // [INV-SERVER-03] Zero TCP on Client-Bound Posture:
        // Pure UDS/Local IPC mode — no TCP socket listener spawned.
        let _ = actual_port_tx.send(Ok(0));
    } else {
        let cc = cancel.clone();
        let tf = token.clone();
        let registry = registry.clone();
        let conns = conns.clone();
        let tasks = tasks.clone();
        let gate = gate.clone();
        let tcp_mgr_for_init = tcp_mgr.clone();
        let handle = tokio::spawn(async move {
            let listener = match bind_tcp(bind_addr, port_fallback).await {
                Ok((l, actual)) => {
                    let _ = actual_port_tx.send(Ok(actual));
                    tcp_mgr_for_init.port.store(actual, Ordering::SeqCst);
                    *tcp_mgr_for_init.cancel.write().await = Some(cc.clone());
                    tracing::info!(%bind_addr,actual_port=actual,auth=tf.is_some(),"nuo server: listener started");
                    l
                }
                Err(e) => {
                    // Surface the real io::Error: the run loop turns it into
                    // a fatal shutdown (readable at the top level), not a
                    // bare RecvError.
                    let _ = actual_port_tx.send(Err(std::io::Error::new(
                        e.kind(),
                        format!("could not bind {bind_addr}: {e}"),
                    )));
                    return;
                }
            };
            // Exponential backoff across *consecutive* accept failures so a
            // resource-exhausted listener (EMFILE) degrades to a slow poll
            // instead of a hot spin; any success resets it.
            let mut backoff = std::time::Duration::from_millis(5);
            loop {
                tokio::select! {_=cc.cancelled()=>{tracing::info!("nuo server: cancelled");break;}
                ac=listener.accept()=>{let(stream,peer)=match ac{Ok(c)=>c,Err(e)=>{tracing::warn!(error=%e,backoff_ms=backoff.as_millis() as u64,"nuo server: accept failed");tokio::time::sleep(backoff).await;backoff=(backoff*2).min(ACCEPT_BACKOFF_CAP);continue;}};
                backoff=std::time::Duration::from_millis(5);
                spawn_tcp_connection(
                    stream,
                    ServeCtx {
                        registry: registry.clone(),
                        conns: conns.clone(),
                        gate: gate.clone(),
                        listeners: cc.clone(),
                        tcp_manager: Some(tcp_mgr_for_init.clone()),
                    },
                    tf.clone(),
                    expose,
                    peer.to_string(),
                );}}
            }
        });
        tasks.track("tcp-accept", handle);
    }

    let local_rx = {
        let (local_tx, local_rx) = tokio::sync::oneshot::channel::<
            Result<Option<nuo_host::ipc::LocalEndpoint>, std::io::Error>,
        >();
        if let Some(endpoint) = opts.local_endpoint.clone() {
            let cc = cancel.clone();
            let registry = registry.clone();
            let conns = conns.clone();
            let tasks = tasks.clone();
            let gate = gate.clone();
            let handle = tokio::spawn(async move {
                let mut listener = match nuo_host::ipc::LocalListener::bind(&endpoint) {
                    Ok(l) => {
                        let _ = local_tx.send(Ok(Some(endpoint.clone())));
                        l
                    }
                    Err(e) => {
                        tracing::error!(endpoint=?endpoint,error=%e,"nuo server: local IPC bind failed");
                        let _ = local_tx.send(Err(e));
                        return;
                    }
                };
                tracing::info!(endpoint=?endpoint,"nuo server: local IPC listener started");
                let mut backoff = std::time::Duration::from_millis(5);
                loop {
                    tokio::select! {
                        _ = cc.cancelled() => {
                            tracing::info!("nuo server: local IPC cancelled");
                            break;
                        }
                        ac = listener.accept() => {
                            let stream = match ac {
                                Ok(c) => c,
                                Err(e) => {
                                    tracing::warn!(
                                        error = %e,
                                        backoff_ms = backoff.as_millis() as u64,
                                        "nuo server: local IPC accept failed"
                                    );
                                    tokio::time::sleep(backoff).await;
                                    backoff = (backoff * 2).min(ACCEPT_BACKOFF_CAP);
                                    continue;
                                }
                            };
                            backoff = std::time::Duration::from_millis(5);
                            spawn_local_connection(
                                stream,
                                ServeCtx {
                                    registry: registry.clone(),
                                    conns: conns.clone(),
                                    gate: gate.clone(),
                                    listeners: cc.clone(),
                                    tcp_manager: Some(tcp_mgr.clone()),
                                },
                                format!("local:{endpoint:?}"),
                            );
                        }
                    }
                }
            });
            tasks.track("local-ipc-accept", handle);
        } else {
            let _ = local_tx.send(Ok(None));
        }
        local_rx
    };

    ServeHandle {
        startup: Startup {
            port: Some(actual_port_rx),
            local_ready: Some(local_rx),
        },
        cancel,
        conns,
        tasks,
        token,
        gate,
        version: server_version(),
    }
}
