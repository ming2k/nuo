use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::identity::{DaemonUiBridge, agent_code};
use nuo_client as client;

/// The server-start flags that reach the runtime (one struct, one place).
#[derive(Debug, Clone, Default)]
pub struct ServerStart {
    pub port: Option<u16>,
    pub public: bool,
    pub no_local_auth: bool,
    pub idle_exit_minutes: Option<u64>,
    pub shutdown_grace_secs: Option<u64>,
    pub client_driven: bool,
}

/// Backward-compatible alias for [`ServerStart`].
#[allow(dead_code)]
pub type DaemonStart = ServerStart;

/// Start detached: spawn the server in the background and return.
/// If a conflicting server is already running, directly replace it (ADR-0029).
pub fn detach_server(flags: &ServerStart) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(info) = client::discover(Path::new(".")) {
        if info.pid != std::process::id() {
            eprintln!("nuo: replacing existing server (pid {})...", info.pid);
            let _ = nuo_host::process::takeover_pid_sync(
                info.pid,
                nuo_host::process::TakeoverOptions::default(),
            );
            client::discovery::remove(&client::discovery::global_discovery_path());
        }
    }
    let program = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("nuo"));
    let mut command = std::process::Command::new(&program);
    command.args(["start", "--fg"]);
    if flags.client_driven {
        command.arg("--client-driven");
    }
    if let Some(port) = flags.port {
        command.arg("--port").arg(port.to_string());
    }
    if flags.public {
        command.arg("--public");
    }
    if flags.no_local_auth {
        command.arg("--no-local-auth");
    }
    if let Some(minutes) = flags.idle_exit_minutes {
        command.arg("--idle-exit").arg(minutes.to_string());
    }
    if let Some(secs) = flags.shutdown_grace_secs {
        command.arg("--grace").arg(secs.to_string());
    }
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    client::configure_daemon_detachment(&mut command);
    command
        .spawn()
        .map_err(|e| format!("could not spawn {}: {e}", program.display()))?;
    eprintln!(
        "nuo: server started in the background (`nuo status` to observe, `nuo stop` to stop it)"
    );
    Ok(())
}

/// Backward-compatible alias for [`detach_server`].
#[allow(unused_imports)]
pub use detach_server as detach_daemon;

/// Stop the running server through the budget-aware shutdown pipeline.
pub async fn stop_server() -> Result<(), Box<dyn std::error::Error>> {
    let info = match client::discover(Path::new(".")) {
        Some(info) => info,
        None => {
            let lock_path = nuo::serve_discovery::global_lock_path();
            if let Some(pid) = nuo_host::lock::ProcessLock::probe_holder(&lock_path) {
                if client::is_process_alive(pid) {
                    client::DaemonInfo {
                        pid,
                        process_birth_token: nuo_host::process::process_identity(pid)
                            .ok()
                            .map(|identity| identity.birth_token),
                        port: nuo::startup::env_default_port(),
                        token: None,
                        project_root: String::new(),
                        started_at: 0,
                        #[cfg(unix)]
                        uds_path: Some(nuo::serve_discovery::default_uds_path()),
                        #[cfg(not(unix))]
                        uds_path: None,
                        local_endpoint: nuo::serve_discovery::default_local_endpoint()
                            .ok(),
                        version: None,
                        grace_secs: None,
                        protocol: None,
                        ..Default::default()
                    }
                } else {
                    eprintln!("nuo: no server is running.");
                    return Ok(());
                }
            } else {
                eprintln!("nuo: no server is running.");
                return Ok(());
            }
        }
    };
    client::stop(&info).await?;
    eprintln!("nuo: server stopped (pid {}).", info.pid);
    Ok(())
}

/// Backward-compatible alias for [`stop_server`].
#[allow(unused_imports)]
pub use stop_server as stop_daemon;

/// Restart the server: stop the running instance (if any) and detach a replacement.
pub async fn restart_server(
    flags: &ServerStart,
    force: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(info) = client::discover(Path::new(".")) {
        eprintln!("nuo: stopping existing server (pid {})...", info.pid);
        if force {
            let _ = nuo_host::process::takeover_pid(
                info.pid,
                nuo_host::process::TakeoverOptions::default(),
            )
            .await;
            client::discovery::remove(&client::discovery::global_discovery_path());
        } else {
            let _ = client::stop(&info).await;
        }
    }
    detach_server(flags)
}

/// Run server in foreground (the supervisor shape).
pub async fn run_server_foreground(flags: ServerStart) -> Result<(), Box<dyn std::error::Error>> {
    let mut lifecycle = nuo::host::LifecycleOptions::from_config();
    lifecycle.client_driven = flags.client_driven;
    if let Some(minutes) = flags.idle_exit_minutes {
        lifecycle.idle_exit = match minutes {
            0 => None,
            m => Some(std::time::Duration::from_secs(m * 60)),
        };
    }
    if let Some(secs) = flags.shutdown_grace_secs {
        lifecycle.shutdown_grace = std::time::Duration::from_secs(secs.max(1));
    }

    // [INV-SERVER-03] Zero TCP on Client-Bound Posture:
    // When running in client_driven mode without an explicit port, do not bind TCP.
    let disable_tcp = flags.client_driven && flags.port.is_none();
    let port = flags.port.unwrap_or(nuo::startup::env_default_port());
    let preset = agent_code();
    let outcome = nuo::host::run_with_gate(
        nuo::host::HostIdentity {
            identity: preset.identity.clone(),
            preset,
            ui: Arc::new(DaemonUiBridge),
        },
        nuo::host::HostOptions {
            port,
            expose: if flags.public {
                nuo::serve::ServeExpose::Public
            } else {
                nuo::serve::ServeExpose::Local
            },
            token: None,
            local_auth: !flags.no_local_auth
                && nuo_persistence::config::Config::load().daemon.local_auth,
            port_fallback: flags.port.is_none(),
            local_endpoint: Some(
                nuo::serve_discovery::default_local_endpoint()
                    .map_err(std::io::Error::other)?,
            ),
            disable_tcp,
        },
        Arc::new(nuo::shutdown::ShutdownGate::new()),
        lifecycle,
    )
    .await;
    match &outcome {
        nuo::host::RunOutcome::Stopped { reason } => {
            eprintln!("nuo: server stopped ({reason}).");
        }
        nuo::host::RunOutcome::ForcedExit { reason } => {
            eprintln!(
                "nuo: server stopped ({reason}); grace budget expired, stragglers were \
                 aborted — see the log."
            );
        }
        nuo::host::RunOutcome::StartupFailed(what) => {
            eprintln!("nuo: {what}");
        }
    }
    std::process::exit(outcome.exit_code());
}

/// Backward-compatible alias for [`run_server_foreground`].
#[allow(unused_imports)]
pub use run_server_foreground as run_daemon_foreground;
