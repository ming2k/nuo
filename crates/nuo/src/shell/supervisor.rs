//! In-process connection supervisor for single-lifetime TUI sessions
//! (ADR-0224, ADR-0260 `[INV-SRVPHASE-02]`, `[INV-SRVPHASE-04]`).
//!
//! Owns the full transport lifecycle (`Discover / Spawn -> Hello -> ServerHello ->
//! Monitor/Response/LinkEvent translators -> Reconnect backoff`) in one
//! background task while keeping `App` alive across link drops and server
//! restarts without tearing down tabs, scroll state, or unsent drafts.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::shell::app::state::ClientLinkPhase;
use crate::shell::client;
use crate::shell::event_loop::{self, AppMutation, UiRuntime};
use crate::shell::runner::{RELINK_STABILITY_WINDOW, relink_delay_ms, relink_max_attempts};

pub(crate) struct SupervisorConfig {
    pub remote: Option<String>,
    pub token: Option<String>,
    pub project_override: Option<PathBuf>,
    pub explicit_port: Option<u16>,
    pub runtime: UiRuntime,
    pub tui_config: Arc<view::tui_config::TuiConfig>,
    pub should_quit: Arc<AtomicBool>,
    pub cancel: CancellationToken,
    pub reconnect_notify: Arc<Notify>,
    pub resolved_target: Arc<Mutex<Option<client::ClientTarget>>>,
}

async fn emit(runtime: &UiRuntime, mutation: AppMutation) {
    runtime.mutations.send(mutation).await;
    runtime.dirty.store(true, Ordering::SeqCst);
    runtime.dirty_notify.notify_one();
}

async fn resolve_target(
    remote: &Option<String>,
    token: &Option<String>,
    project_override: &Option<PathBuf>,
    explicit_port: Option<u16>,
    cancel: &CancellationToken,
) -> Result<client::ClientTarget, String> {
    if let Some(addr) = remote {
        let remote_server = client::RemoteServer::parse(addr, token.clone())?;
        return Ok(client::ClientTarget::Remote(remote_server));
    }
    let raw_root = project_override
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let project_root = host::paths::find_project_root(&raw_root);
    let info = tokio::select! {
        res = client::ensure_server_explicit(&project_root, explicit_port) => res?,
        _ = cancel.cancelled() => return Err("cancelled while starting Nuo server".to_string()),
    };
    if !client::versions_compatible(&info) {
        return Err(client::incompatibility_error(&info));
    }
    Ok(client::ClientTarget::Local(info))
}

pub(crate) fn spawn_connection_supervisor(config: SupervisorConfig) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let SupervisorConfig {
            remote,
            token,
            project_override,
            explicit_port,
            runtime,
            tui_config,
            should_quit,
            cancel,
            reconnect_notify,
            resolved_target,
        } = config;

        let initial_label = if let Some(ref addr) = remote {
            format!("Connecting to {addr}…")
        } else {
            "Starting Nuo server…".to_string()
        };
        emit(
            &runtime,
            AppMutation::LinkPhase(ClientLinkPhase::Connecting {
                label: initial_label,
            }),
        )
        .await;

        let mut target_server = match resolve_target(
            &remote,
            &token,
            &project_override,
            explicit_port,
            &cancel,
        )
        .await
        {
            Ok(target) => {
                *resolved_target.lock().unwrap_or_else(|e| e.into_inner()) = Some(target.clone());
                target
            }
            Err(message) => {
                if !cancel.is_cancelled() {
                    emit(
                        &runtime,
                        AppMutation::LinkPhase(ClientLinkPhase::Failed { message }),
                    )
                    .await;
                }
                return;
            }
        };

        let mut ever_connected = false;
        let mut consecutive_relinks: u32 = 0;

        loop {
            if should_quit.load(Ordering::SeqCst) || cancel.is_cancelled() {
                return;
            }

            let endpoint = target_server.endpoint_display();
            if !ever_connected {
                emit(
                    &runtime,
                    AppMutation::LinkPhase(ClientLinkPhase::Connecting {
                        label: format!("Connecting to {endpoint}…"),
                    }),
                )
                .await;
            }

            let monitor = protocol::MonitorAction {
                watch: true,
                include_idle: true,
            };
            let connect_res = tokio::select! {
                res = target_server.connect_server(Some(monitor)) => res,
                _ = cancel.cancelled() => return,
            };

            match connect_res {
                Ok(link) => {
                    ever_connected = true;
                    let connected_at = Instant::now();
                    let server_id = link.hello.server_id.clone();
                    let server_phase = link.hello.phase.clone();
                    let persistence_health = link.hello.persistence_health.clone();

                    event_loop::translators::spawn_monitor_translator(&runtime, link.monitor_rx);
                    event_loop::translators::spawn_link_event_translator(
                        &runtime,
                        link.link_event_rx,
                    );
                    event_loop::translators::spawn_response_translator(
                        event_loop::translators::ResponseTranslatorConfig {
                            rx: link.resp_rx,
                            runtime: runtime.clone(),
                            link_lost: link.link_lost,
                            tui_config: tui_config.clone(),
                            initial_provider: String::new(),
                            initial_model: String::new(),
                            initial_round_count: 0,
                            live_session: String::new(),
                        },
                    );

                    emit(
                        &runtime,
                        AppMutation::LinkEstablished {
                            server_id,
                            server_phase,
                            persistence_health,
                            req_tx: link.req_tx,
                        },
                    )
                    .await;

                    let closed_reason = tokio::select! {
                        res = link.closed_rx => res.unwrap_or(Some("server connection closed".to_string())),
                        _ = reconnect_notify.notified() => Some("outbox channel closed".to_string()),
                        _ = cancel.cancelled() => return,
                    };

                    if should_quit.load(Ordering::SeqCst) || cancel.is_cancelled() {
                        return;
                    }
                    // Explicit server-instructed `AgentResponse::Exit` (e.g., `/exit`).
                    if closed_reason.is_none() {
                        return;
                    }
                    if connected_at.elapsed() >= RELINK_STABILITY_WINDOW {
                        consecutive_relinks = 0;
                    }
                }
                Err(error) => {
                    if !ever_connected && !error.is_transport_failure() {
                        emit(
                            &runtime,
                            AppMutation::LinkPhase(ClientLinkPhase::Failed {
                                message: error.message,
                            }),
                        )
                        .await;
                        return;
                    }
                    tracing::warn!(
                        endpoint = %endpoint,
                        %error,
                        "connection supervisor: dial failed; entering backoff"
                    );
                }
            }

            let (is_remote, max_attempts) = relink_max_attempts(&target_server);
            consecutive_relinks += 1;
            let Some(delay_ms) = relink_delay_ms(is_remote, consecutive_relinks, |span| {
                fastrand::u64(0..=span)
            }) else {
                emit(
                    &runtime,
                    AppMutation::LinkPhase(ClientLinkPhase::Failed {
                        message: format!(
                            "server connection to {endpoint} lost; gave up after {max_attempts} attempts"
                        ),
                    }),
                )
                .await;
                return;
            };

            emit(
                &runtime,
                AppMutation::LinkPhase(ClientLinkPhase::Reconnecting {
                    endpoint: endpoint.clone(),
                    attempt: consecutive_relinks,
                    max_attempts,
                    delay_ms: Some(delay_ms),
                }),
            )
            .await;

            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(delay_ms)) => {}
                _ = cancel.cancelled() => return,
            }

            if !is_remote {
                match resolve_target(&None, &None, &project_override, explicit_port, &cancel).await
                {
                    Ok(fresh) => {
                        *resolved_target.lock().unwrap_or_else(|e| e.into_inner()) =
                            Some(fresh.clone());
                        target_server = fresh;
                    }
                    Err(error) => {
                        tracing::warn!(%error, "connection supervisor: local re-discovery failed");
                    }
                }
            }
        }
    })
}
