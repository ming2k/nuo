//! TCP listener management, dynamic port opening/closing, and connection classification.
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU16, Ordering};
use tokio::net::TcpListener;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use super::auth::generate_token;
use super::conn_table::ConnTable;
use super::connection::{ServeCtx, spawn_tcp_connection};
use super::options::ServeExpose;
use crate::runtime::shutdown::ShutdownGate;

/// Backoff ceiling for persistent accept errors (EMFILE and friends): each
/// consecutive failure doubles the wait, capped here, so a resource-exhausted
/// listener degrades to a slow poll instead of a hot spin. Any successful
/// accept resets it.
pub const ACCEPT_BACKOFF_CAP: std::time::Duration = std::time::Duration::from_secs(1);

/// What the first bytes of an accepted TCP connection ask for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TcpTransport {
    /// `Upgrade: websocket` — the control plane.
    WebSocket,
    /// Plain HTTP — the generic health endpoint.
    Http,
}

/// Cap on header bytes examined when splitting HTTP from WebSocket on the
/// single port. A handshake that does not terminate its headers within this
/// budget is handed to the WebSocket path to reject.
pub const PEEK_CAP: usize = 16 * 1024;

/// Split an accepted TCP connection into the WebSocket control plane or the
/// plain-HTTP health endpoint. Peek-only: the socket buffer is not consumed,
/// so downstream handshake parsers receive the bytes untouched.
pub async fn classify(stream: &tokio::net::TcpStream) -> std::io::Result<TcpTransport> {
    let mut buf = [0u8; 1024];
    let n = stream.peek(&mut buf).await?;
    Ok(classify_head(&buf[..n]))
}

pub fn classify_head(head: &[u8]) -> TcpTransport {
    let text = String::from_utf8_lossy(head);
    let mut lines = text.lines();
    let request_line = lines.next().unwrap_or("");
    let looks_http = request_line.ends_with("HTTP/1.1") || request_line.ends_with("HTTP/1.0");
    let ws_upgrade = lines.any(|line| {
        let line = line.trim_start();
        line.len() >= "upgrade:".len()
            && line[.."upgrade:".len()].eq_ignore_ascii_case("upgrade:")
            && line["upgrade:".len()..]
                .trim()
                .eq_ignore_ascii_case("websocket")
    });
    if looks_http && !ws_upgrade {
        TcpTransport::Http
    } else {
        TcpTransport::WebSocket
    }
}

/// Bind the TCP listener, falling back to an OS-assigned port when the
/// requested one is taken and `port_fallback` is on (the discovery record
/// then carries the real one). Returns the listener and the port it bound.
pub async fn bind_tcp(
    addr: SocketAddr,
    port_fallback: bool,
) -> std::io::Result<(TcpListener, u16)> {
    match TcpListener::bind(addr).await {
        Ok(l) => {
            let port = l.local_addr()?.port();
            Ok((l, port))
        }
        Err(e) if port_fallback && addr.port() != 0 => {
            let fallback_addr = SocketAddr::new(addr.ip(), 0);
            tracing::warn!(
                %addr,
                error = %e,
                "requested port taken; binding ephemeral port on loopback"
            );
            let l = TcpListener::bind(fallback_addr).await?;
            let port = l.local_addr()?.port();
            Ok((l, port))
        }
        Err(e) => Err(e),
    }
}

pub static TCP_MANAGER: std::sync::RwLock<Option<Arc<TcpManager>>> = std::sync::RwLock::new(None);

pub struct TcpManager {
    pub(crate) port: Arc<AtomicU16>,
    pub(crate) token: Arc<RwLock<Option<String>>>,
    pub(crate) cancel: Arc<RwLock<Option<CancellationToken>>>,
    pub(crate) expose: ServeExpose,
    pub(crate) local_endpoint: Option<nuo_host::ipc::LocalEndpoint>,
    pub(crate) registry: Arc<crate::runtime::registry::ThreadRegistry>,
    pub(crate) conns: Arc<ConnTable>,
    pub(crate) gate: Arc<ShutdownGate>,
    pub(crate) tasks: Arc<crate::runtime::shutdown::TaskBook>,
}

impl TcpManager {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        port: Arc<AtomicU16>,
        token: Arc<RwLock<Option<String>>>,
        cancel: Arc<RwLock<Option<CancellationToken>>>,
        expose: ServeExpose,
        local_endpoint: Option<nuo_host::ipc::LocalEndpoint>,
        registry: Arc<crate::runtime::registry::ThreadRegistry>,
        conns: Arc<ConnTable>,
        gate: Arc<ShutdownGate>,
        tasks: Arc<crate::runtime::shutdown::TaskBook>,
    ) -> Self {
        Self {
            port,
            token,
            cancel,
            expose,
            local_endpoint,
            registry,
            conns,
            gate,
            tasks,
        }
    }

    pub async fn open_tcp(
        &self,
        requested_port: Option<u16>,
    ) -> Result<nuo_protocol::ServerEndpointsReport, String> {
        let current_port = self.port.load(Ordering::SeqCst);
        let target_port = requested_port.unwrap_or_else(|| {
            if current_port > 0 {
                current_port
            } else {
                crate::composition::startup::env_default_port()
            }
        });

        if current_port == target_port && current_port > 0 {
            return Ok(self.get_endpoints().await);
        }

        self.close_tcp_internal().await;

        let bind_addr: SocketAddr = match self.expose {
            ServeExpose::Local => ([127, 0, 0, 1], target_port).into(),
            ServeExpose::Public => ([0, 0, 0, 0], target_port).into(),
        };

        let (listener, actual_port) = bind_tcp(bind_addr, true)
            .await
            .map_err(|e| format!("could not bind TCP listener to {bind_addr}: {e}"))?;

        let mut token_lock = self.token.write().await;
        if token_lock.is_none() {
            *token_lock = Some(generate_token());
        }
        let tf = token_lock.clone();
        drop(token_lock);

        let cancel = CancellationToken::new();
        *self.cancel.write().await = Some(cancel.clone());
        self.port.store(actual_port, Ordering::SeqCst);

        if let Some(mut record) = crate::transport::serve_discovery::read() {
            record.port = actual_port;
            record.token = tf.clone();
            let _ = crate::transport::serve_discovery::write_global(&record);
        }

        let cc = cancel;
        let registry = self.registry.clone();
        let conns = self.conns.clone();
        let gate = self.gate.clone();
        let expose = self.expose;
        let tcp_mgr = Arc::new(self.clone_manager());
        let handle = tokio::spawn(async move {
            let mut backoff = std::time::Duration::from_millis(5);
            loop {
                tokio::select! {
                    _ = cc.cancelled() => {
                        tracing::info!("nuo server: dynamic TCP listener cancelled");
                        break;
                    }
                    ac = listener.accept() => {
                        let (stream, peer) = match ac {
                            Ok(c) => c,
                            Err(e) => {
                                tracing::warn!(error=%e, backoff_ms=backoff.as_millis() as u64, "nuo server: accept failed");
                                tokio::time::sleep(backoff).await;
                                backoff = (backoff * 2).min(ACCEPT_BACKOFF_CAP);
                                continue;
                            }
                        };
                        backoff = std::time::Duration::from_millis(5);
                        spawn_tcp_connection(
                            stream,
                            ServeCtx {
                                registry: registry.clone(),
                                conns: conns.clone(),
                                gate: gate.clone(),
                                listeners: cc.clone(),
                                tcp_manager: Some(tcp_mgr.clone()),
                            },
                            tf.clone(),
                            expose,
                            peer.to_string(),
                        );
                    }
                }
            }
        });
        self.tasks.track("dynamic-tcp-accept", handle);

        Ok(self.get_endpoints().await)
    }

    async fn close_tcp_internal(&self) {
        let mut cancel_lock = self.cancel.write().await;
        if let Some(cancel) = cancel_lock.take() {
            cancel.cancel();
        }
        self.port.store(0, Ordering::SeqCst);
    }

    pub async fn close_tcp(&self) -> Result<nuo_protocol::ServerEndpointsReport, String> {
        self.close_tcp_internal().await;
        if let Some(mut record) = crate::transport::serve_discovery::read() {
            record.port = 0;
            record.token = None;
            let _ = crate::transport::serve_discovery::write_global(&record);
        }
        Ok(self.get_endpoints().await)
    }

    pub async fn get_endpoints(&self) -> nuo_protocol::ServerEndpointsReport {
        let port = self.port.load(Ordering::SeqCst);
        let active = port > 0;
        let token = self.token.read().await.clone();
        let local_endpoint = self.local_endpoint.as_ref().map(|e| e.to_string());
        nuo_protocol::ServerEndpointsReport {
            tcp_port: port,
            tcp_active: active,
            token,
            local_endpoint,
        }
    }

    pub(crate) fn clone_manager(&self) -> Self {
        Self {
            port: self.port.clone(),
            token: self.token.clone(),
            cancel: self.cancel.clone(),
            expose: self.expose,
            local_endpoint: self.local_endpoint.clone(),
            registry: self.registry.clone(),
            conns: self.conns.clone(),
            gate: self.gate.clone(),
            tasks: self.tasks.clone(),
        }
    }
}

pub async fn get_current_endpoints() -> nuo_protocol::ServerEndpointsReport {
    let mgr_opt = TCP_MANAGER.read().ok().and_then(|guard| guard.clone());
    if let Some(mgr) = mgr_opt {
        mgr.get_endpoints().await
    } else if let Some(rec) = crate::transport::serve_discovery::read() {
        let local_endpoint = rec.effective_local_endpoint().map(|e| e.to_string());
        nuo_protocol::ServerEndpointsReport {
            tcp_port: rec.port,
            tcp_active: rec.port > 0,
            token: rec.token,
            local_endpoint,
        }
    } else {
        nuo_protocol::ServerEndpointsReport {
            tcp_port: 0,
            tcp_active: false,
            token: None,
            local_endpoint: None,
        }
    }
}

pub async fn open_dynamic_tcp(
    port: Option<u16>,
) -> Result<nuo_protocol::ServerEndpointsReport, String> {
    let mgr_opt = TCP_MANAGER.read().ok().and_then(|guard| guard.clone());
    if let Some(mgr) = mgr_opt {
        mgr.open_tcp(port).await
    } else {
        Err("TCP manager not initialized".to_string())
    }
}

pub async fn close_dynamic_tcp() -> Result<nuo_protocol::ServerEndpointsReport, String> {
    let mgr_opt = TCP_MANAGER.read().ok().and_then(|guard| guard.clone());
    if let Some(mgr) = mgr_opt {
        mgr.close_tcp().await
    } else {
        Err("TCP manager not initialized".to_string())
    }
}
