use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::identity::{DaemonUiBridge, agent_code};
use nuo_client as client;

/// The daemon-start flags that reach the runtime (one struct, one place).
#[derive(Debug, Clone, Default)]
pub struct DaemonStart {
    pub port: Option<u16>,
    pub public: bool,
    pub no_local_auth: bool,
    pub idle_exit_minutes: Option<u64>,
    pub shutdown_grace_secs: Option<u64>,
}

/// Start detached (the default): spawn the daemon in the background and return.
/// If a daemon is already running, report it instead of spawning a second one.
pub fn detach_daemon(flags: &DaemonStart) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(info) = client::discover(Path::new(".")) {
        return Err(format!(
            "a nuo daemon is already running (pid {}, port {}). Stop it with `nuo stop` before starting another.",
            info.pid, info.port
        )
        .into());
    }
    let program = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("nuo"));
    let mut command = std::process::Command::new(&program);
    command.args(["start", "--fg"]);
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
        "nuo: daemon started in the background (`nuo status` to observe, `nuo stop` to stop it)"
    );
    Ok(())
}

/// Stop the running daemon through the budget-aware shutdown pipeline.
pub async fn stop_daemon() -> Result<(), Box<dyn std::error::Error>> {
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
                    }
                } else {
                    eprintln!("nuo: no daemon is running.");
                    return Ok(());
                }
            } else {
                eprintln!("nuo: no daemon is running.");
                return Ok(());
            }
        }
    };
    client::stop(&info).await?;
    eprintln!("nuo: daemon stopped (pid {}).", info.pid);
    Ok(())
}

/// Run daemon in foreground (the supervisor shape).
pub async fn run_daemon_foreground(flags: DaemonStart) -> Result<(), Box<dyn std::error::Error>> {
    let mut lifecycle = nuo::host::LifecycleOptions::from_config();
    if let Some(minutes) = flags.idle_exit_minutes {
        lifecycle.idle_exit = match minutes {
            0 => None,
            m => Some(std::time::Duration::from_secs(m * 60)),
        };
    }
    if let Some(secs) = flags.shutdown_grace_secs {
        lifecycle.shutdown_grace = std::time::Duration::from_secs(secs.max(1));
    }

    let port = flags
        .port
        .unwrap_or(nuo::startup::env_default_port());
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
        },
        Arc::new(nuo::shutdown::ShutdownGate::new()),
        lifecycle,
    )
    .await;
    match &outcome {
        nuo::host::RunOutcome::Stopped { reason } => {
            eprintln!("nuo: daemon stopped ({reason}).");
        }
        nuo::host::RunOutcome::ForcedExit { reason } => {
            eprintln!(
                "nuo: daemon stopped ({reason}); grace budget expired, stragglers were \
                 aborted — see the log."
            );
        }
        nuo::host::RunOutcome::StartupFailed(what) => {
            eprintln!("nuo: {what}");
        }
    }
    std::process::exit(outcome.exit_code());
}
