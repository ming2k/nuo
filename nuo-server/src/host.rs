//! The session server runtime: one process that owns every
//! session across every project for the user and serves them over the
//! control plane (owner-only native local IPC by default, TCP + bearer token
//! with `--public`) so TUI/CLI/web clients can drive, observe, and manage them.
//!
//! Vocabulary: the *role* is the **server**; `nuo start --fg` runs
//! it in the foreground and `nuo start` detaches it.
//!
//! # Lifecycle (ADR-0034)
//!
//! Shutdown here is a **budgeted state transition**, not an await chain:
//!
//! 1. Any trigger (SIGINT/SIGTERM/SIGHUP, the `Shutdown` control verb, the
//!    idle-exit timer, a fatal startup error) funnels into one
//!    [`ShutdownGate`]; the first reason latches.
//! 2. The drain runs in phases under a total grace budget
//!    (`[server] shutdown_grace_secs`): pull the discovery advertisement,
//!    stop accepting, close live connections, tear every session down
//!    concurrently with per-hook deadlines.
//! 3. Every phase checks `gate.forced()` (a second signal skips the rest)
//!    and the remaining budget; the force path aborts stragglers, runs the
//!    RAII cleanup (discovery lease, local-listener guard), and exits anyway.
//!
//! The exit code is part of the contract: 0 for any completed graceful
//! shutdown (signals included — a supervisor's `stop` succeeding is the
//! normal outcome), 1 for fatal startup errors and forced exits.

use crate::UiBridge;
use crate::bootstrap;
use crate::registry::{HostParams, SessionRegistry};
use crate::serve::{ServeExpose, ServeOptions, StartupParts, start_server};
use crate::serve_discovery as discovery;
use crate::shutdown::{DrainProbe, ShutdownGate, ShutdownReason, SignalGuard, TaskBook};
use nuo_harness::{AgentIdentity, AgentRoleProfile};
use nuo_persistence::config::Config;
use nuo_host::lock::ProcessLock;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub struct HostOptions {
    pub port: u16,
    pub expose: ServeExpose,
    pub token: Option<String>,
    /// Require a bearer token on the loopback TCP listener;
    /// resolved by the CLI from `[server] local_auth` + `--no-local-auth`.
    pub local_auth: bool,
    /// Fall back to an OS-assigned port when the requested one is taken:
    /// on for the CLI default port, off for an explicit `--port`
    /// (a stated bind must fail loudly, not silently move).
    pub port_fallback: bool,
    /// Serve the control plane over the native per-user local IPC transport.
    pub local_endpoint: Option<nuo_host::ipc::LocalEndpoint>,
    /// [INV-SERVER-03] Zero TCP on Client-Bound Posture:
    /// Pure UDS/Local IPC mode — no TCP socket listener spawned.
    pub disable_tcp: bool,
}

pub struct HostIdentity {
    pub identity: AgentIdentity,
    pub preset: AgentRoleProfile,
    pub ui: Arc<dyn UiBridge>,
}

/// The server lifecycle configuration, resolved once at startup (ADR-0034).
/// Mirrors `[server]` in `config.toml` (see `ServerConfig`) with the
/// always-on escape hatch surfaced as `idle_exit: None`.
#[derive(Debug, Clone)]
pub struct LifecycleOptions {
    /// Total budget for the graceful drain before the force path.
    pub shutdown_grace: Duration,
    /// Auto-exit after this much continuous zero-sessions-zero-clients time.
    /// `None` = never (always-on deployments).
    pub idle_exit: Option<Duration>,
    /// Auto-exit when all interactive TUI clients close (ADR-0029).
    pub client_driven: bool,
    /// Test seam (never set in production): park the drain after it is
    /// announced so a test can land an escalation at a deterministic point.
    #[doc(hidden)]
    pub drain_probe: Option<Arc<DrainProbe>>,
}

impl LifecycleOptions {
    pub fn from_config() -> Self {
        let cfg = Config::load().server;
        Self {
            shutdown_grace: Duration::from_secs(cfg.shutdown_grace_secs.max(1)),
            idle_exit: match cfg.idle_exit_minutes {
                0 => None,
                minutes => Some(Duration::from_secs(minutes * 60)),
            },
            client_driven: false,
            drain_probe: None,
        }
    }
}

impl Default for LifecycleOptions {
    fn default() -> Self {
        Self {
            shutdown_grace: Duration::from_secs(10),
            idle_exit: Some(Duration::from_secs(5 * 60)),
            client_driven: false,
            drain_probe: None,
        }
    }
}

/// What ended the server: surfaced to the binary for its exit line/code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunOutcome {
    /// The graceful drain completed within its budget.
    Stopped { reason: ShutdownReason },
    /// The grace budget expired (or a second trigger escalated); stragglers
    /// were aborted. Exit code is still 0 for a signal-initiated stop — the
    /// server *did* stop, the hooks that did not finish are named in the log.
    ForcedExit { reason: ShutdownReason },
    /// Startup could not complete (bind failure, single-instance lock
    /// contended past its wait). Exit code 1.
    StartupFailed(String),
}

impl RunOutcome {
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Stopped { .. } | Self::ForcedExit { .. } => 0,
            Self::StartupFailed(_) => 1,
        }
    }

    pub fn reason(&self) -> String {
        match self {
            Self::Stopped { reason } | Self::ForcedExit { reason } => reason.to_string(),
            Self::StartupFailed(what) => format!("startup failed: {what}"),
        }
    }
}

/// Run the server until a shutdown trigger, then drain within the configured
/// grace budget. Installs the OS signal listeners itself. See the module
/// docs for the phase-by-phase breakdown.
pub async fn run(
    identity: HostIdentity,
    opts: HostOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    let outcome = run_with_outcome(identity, opts).await;
    if let RunOutcome::StartupFailed(what) = &outcome {
        return Err(what.clone().into());
    }
    Ok(())
}

/// The lifecycle state machine (ADR-0034). Exposed for the binaries, which
/// map [`RunOutcome`] onto exit codes and final log lines.
pub async fn run_with_outcome(identity: HostIdentity, opts: HostOptions) -> RunOutcome {
    run_with_gate(
        identity,
        opts,
        Arc::new(ShutdownGate::new()),
        LifecycleOptions::from_config(),
    )
    .await
}

/// The testable core: `gate` is the shutdown trigger source (the production
/// caller installs OS signals into it; tests request reasons directly) and
/// `lifecycle` carries the budgets. See [`run_with_outcome`].
pub async fn run_with_gate(
    identity: HostIdentity,
    opts: HostOptions,
    gate: Arc<ShutdownGate>,
    lifecycle: LifecycleOptions,
) -> RunOutcome {
    run_inner(identity, opts, gate, lifecycle, None).await
}

/// [`run_with_gate`] with an externally supplied registry: integration tests
/// host hand-built sessions (no assembly) and observe the drain through the
/// same registry the run loop drives. Production always builds its own
/// ([`SessionRegistry::new`]) via [`run_with_gate`].
pub async fn run_with_registry(
    identity: HostIdentity,
    opts: HostOptions,
    gate: Arc<ShutdownGate>,
    lifecycle: LifecycleOptions,
    registry: Arc<SessionRegistry>,
) -> RunOutcome {
    run_inner(identity, opts, gate, lifecycle, Some(registry)).await
}

async fn run_inner(
    identity: HostIdentity,
    opts: HostOptions,
    gate: Arc<ShutdownGate>,
    lifecycle: LifecycleOptions,
    registry: Option<Arc<SessionRegistry>>,
) -> RunOutcome {
    let HostIdentity {
        identity,
        preset,
        ui,
    } = identity;
    let _signals = SignalGuard::install(gate.clone());
    // Server-wide panic visibility (task supervision): a detached server has
    // no controlling terminal, so a panicking task's default-hook output
    // went nowhere. Log every panic (with origin) through tracing first;
    // supervised call sites then turn it into a state transition instead of
    // a silent zombie. Installed before any task is spawned.
    crate::task_fault_tolerance::install_panic_hook();
    let gate: Arc<ShutdownGate> =
        Arc::new((*gate).clone().with_version(crate::serve::server_version()));
    bootstrap::ensure_app_roots();

    // Single instance & takeover (ADR-0029, ADR-0034, ADR-0101):
    // MUST complete takeover and acquire the instance lock BEFORE initializing
    // SessionRegistry or any persistence handles, preventing lock races and handle poisoning.
    let lock_path = discovery::global_lock_path();
    if let Some(parent) = lock_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let local_uds_path = match &opts.local_endpoint {
        Some(nuo_host::ipc::LocalEndpoint::UnixSocket(path)) => Some(path.as_path()),
        _ => None,
    };
    takeover_conflicting_server(&lock_path, local_uds_path, opts.port).await;

    let _instance_lock = match ProcessLock::acquire(&lock_path) {
        Ok(lock) => Some(lock),
        Err(busy) => {
            let budget = Duration::from_millis(1500);
            tracing::warn!(%busy, "nuo server: another server holds the instance lock; attempting takeover");
            match wait_for_lock(&lock_path, budget).await {
                Ok(lock) => Some(lock),
                Err(_) => {
                    if let Some(pid) = ProcessLock::probe_holder(&lock_path) {
                        if pid != std::process::id() {
                            if let Ok(identity) = nuo_host::process::process_identity(pid) {
                                let _ = nuo_host::process::force_terminate(identity);
                            }
                        }
                    }
                    match wait_for_lock(&lock_path, Duration::from_millis(500)).await {
                        Ok(lock) => Some(lock),
                        Err(_) => {
                            return RunOutcome::StartupFailed(format!(
                                "could not acquire instance lock on {}",
                                lock_path.display()
                            ));
                        }
                    }
                }
            }
        }
    };

    let registry = registry.unwrap_or_else(|| {
        Arc::new(SessionRegistry::new(HostParams {
            identity,
            preset,
            ui,
        }))
    });
    let started_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    registry.set_monitor_meta(String::new(), started_at).await;
    // The server-task monitor tap folds server-fabric events
    // into monitor snapshots/diffs so rehosted services are operator-visible.
    registry.start_server_task_monitor();
    // The durability-health tap publishes persistence-writer
    // transitions so degradation is user-visible, not log-only.
    registry.start_persistence_health_monitor();

    let mut handle = start_server(
        ServeOptions {
            port: opts.port,
            expose: opts.expose,
            token: opts.token,
            local_auth: opts.local_auth,
            port_fallback: opts.port_fallback,
            local_endpoint: opts.local_endpoint.clone(),
            disable_tcp: opts.disable_tcp,
        },
        Arc::clone(&registry),
    );
    // The server's gate is the serve gate (the Shutdown control verb funnels
    // into the same trigger as signals).
    let gate: Arc<ShutdownGate> = handle_gate(handle.gate.clone(), &gate);
    registry.spawn_idle_reaper(handle.cancel.clone());
    // Destructure the startup receivers into locals: awaiting through the
    // struct would partially move `handle`, which the drain phases still
    // need (conns / tasks / cancel).
    let StartupParts { port_rx, local_rx } = handle.startup.take();
    let port = match port_rx.await {
        Ok(Ok(port)) => port,
        Ok(Err(error)) => {
            // Bind failed with the real io::Error — cancel the sibling
            // listener, then surface a readable fatal.
            handle.cancel.cancel();
            return RunOutcome::StartupFailed(error.to_string());
        }
        Err(_) => {
            handle.cancel.cancel();
            return RunOutcome::StartupFailed(
                "the TCP listener task exited before binding".to_string(),
            );
        }
    };
    let bound_local = match local_rx.await {
        Ok(Ok(endpoint)) => endpoint,
        Ok(Err(error)) => {
            handle.cancel.cancel();
            return RunOutcome::StartupFailed(format!("native local IPC bind failed: {error}"));
        }
        Err(_) => {
            handle.cancel.cancel();
            return RunOutcome::StartupFailed(
                "the native local IPC listener task exited before binding".to_string(),
            );
        }
    };

    let process_identity = match nuo_host::process::process_identity(std::process::id()) {
        Ok(identity) => identity,
        Err(error) => {
            handle.cancel.cancel();
            return RunOutcome::StartupFailed(format!(
                "could not establish server process identity: {error}"
            ));
        }
    };

    // Discovery record (ADR-0096/0100): written only after both configured
    // transports are confirmed bound, carrying the server's version for skew
    // detection.
    // The lease removes it on *every* exit path (Drop), including panics.
    //
    // ADR-0021: capture the server's own executable image identity once, here
    // at boot, before the image can be replaced under us. It is the server's
    // half of the content-based dev-drift check a client performs; a `None`
    // (unreadable image) simply leaves the client on the legacy inode probe.
    let image_identity = nuo_host::process::current_exe_digest_len();
    let record = discovery::Discovery {
        pid: std::process::id(),
        process_birth_token: Some(process_identity.birth_token),
        port,
        token: handle.token.clone(),
        project_root: String::new(), // server is project-agnostic now
        started_at,
        uds_path: match &bound_local {
            Some(nuo_host::ipc::LocalEndpoint::UnixSocket(path)) => Some(path.clone()),
            _ => None,
        },
        local_endpoint: bound_local.clone(),
        version: Some(crate::serve::server_version().to_string()),
        protocol: Some(nuo_client::wire::PROTOCOL_VERSION),
        // ADR-0021: the server's own image identity, so a client can tell a
        // rebuilt binary apart from a live one by content, not just by path.
        image_digest: image_identity.as_ref().map(|(_, digest)| digest.clone()),
        image_len: image_identity.as_ref().map(|(len, _)| *len),
        // Publish the drain budget so `nuo stop` waits *this*
        // server's grace before escalating: an early SIGTERM
        // would force-exit the server and skip the very session teardown
        // the stop requested.
        grace_secs: Some(lifecycle.shutdown_grace.as_secs()),
    };
    let discovery_path = match discovery::write_global(&record) {
        Ok(path) => path,
        Err(error) => {
            handle.cancel.cancel();
            return RunOutcome::StartupFailed(format!(
                "could not publish server discovery record: {error}"
            ));
        }
    };
    let mut discovery_lease = discovery::DiscoveryLease::new(
        Some(discovery_path),
        record.pid,
        record.process_birth_token,
    );

    // Foreground banner: where the server listens and how to reach it, on
    // stderr so piping stays clean.
    let bind = if opts.expose == crate::serve::ServeExpose::Public {
        "0.0.0.0"
    } else {
        "127.0.0.1"
    };
    if let Some(endpoint) = &bound_local {
        eprintln!("nuo: local control plane on {endpoint}");
    }
    if port > 0 {
        eprintln!("nuo: serving sessions on ws://{bind}:{port}");
        eprintln!("nuo: health probe on http://{bind}:{port}/healthz");
    }
    eprintln!(
        "nuo: observe with `nuo status --watch`, drive with `nuo attach [id]`, stop with `nuo stop`"
    );
    if handle.token.is_some() {
        // Never print the token itself: it is a credential and stderr lands
        // in scrollback, logs, and terminal sharing. The discovery record
        // carries it, written owner-only (0600) — point the operator there.
        let scope = if opts.expose == crate::serve::ServeExpose::Public {
            "exposed listener"
        } else {
            "listener (local_auth)"
        };
        match discovery::global_discovery_path().exists() {
            true => eprintln!(
                "nuo: {scope} requires a bearer token; read it from the discovery file {}",
                discovery::global_discovery_path().display()
            ),
            false => eprintln!(
                "nuo: {scope} requires a bearer token, but the discovery file could not be written — check the logs"
            ),
        }
    }
    tracing::info!(%bind, port, "nuo server: listening");

    // Boot rehost: every service task in the durable task ledger
    // that carries a restart policy is re-spawned on the **registry's
    // server-task fabric** — so its lifecycle events are monitor-visible,
    // not lost to an unobserved manager. Failures are logged
    // and non-fatal.
    {
        use nuo_wire::JobSpec;
        let registry_for_rehost = Arc::clone(&registry);
        crate::task_ledger::rehost_all(
            &nuo_persistence::db::get_persistence_handle(),
            move |row| {
                let JobSpec::Process {
                    command,
                    restart: Some(_policy),
                    ..
                } = &row.spec
                else {
                    return;
                };
                let registry = Arc::clone(&registry_for_rehost);
                let command = command.clone();
                tokio::spawn(async move {
                    match registry
                        .spawn_server_task(
                            command,
                            Some(format!("rehost:{}", row.job_id)),
                            nuo_wire::JobKind::Service,
                        )
                        .await
                    {
                        Ok(info) => {
                            tracing::info!(job = %info.id.0, "rehosted service started");
                        }
                        Err(error) => {
                            tracing::warn!(%error, "rehosted service failed to start");
                        }
                    }
                });
            },
        );
    }

    // Serving
    // Wait for a trigger, or the idle-exit timer.
    serve_until_trigger(
        &gate,
        &registry,
        &handle,
        lifecycle.idle_exit,
        lifecycle.client_driven,
    )
    .await;

    // Draining (ADR-0034): budgeted phases, each checking `forced`
    let reason = gate
        .reason()
        .unwrap_or(ShutdownReason::Fatal("unknown".into()));
    tracing::info!(%reason, remaining_budget =? lifecycle.shutdown_grace, "nuo server: draining");
    let deadline = tokio::time::Instant::now() + lifecycle.shutdown_grace;

    // Test seam: park here so a test can land an escalation (or observe the
    // budget) before the graceful phases run. Never installed in production.
    if let Some(probe) = &lifecycle.drain_probe {
        probe.wait_released().await;
    }

    // Phase 1 — pull the advertisement *first*: a client reading the record
    // right now must not discover a server that is going away.
    discovery_lease.release();

    // Phase 2 — stop accepting, close live connections, confirm the loops.
    handle.cancel.cancel();
    registry.publish_host_event(nuo_wire::MonitorEvent::ServerDraining);
    registry
        .broadcast_all_sessions(nuo_wire::AgentResponse::Exit)
        .await;
    if !gate.forced() {
        handle.conns.drain().await;
    }
    let tasks: Arc<TaskBook> = handle.tasks.clone();
    if !gate.forced() {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let hung = tasks.join_all_with_budget(remaining).await;
        for (name, why) in &hung {
            tracing::warn!(task = %name, %why, "nuo server: task did not stop within the grace budget");
        }
    }

    // Phase 3 — tear every session down concurrently, each hook bounded by
    // the *remaining* budget (a slow listener drain must not eat the
    // sessions' share).
    let hook_budget = deadline.saturating_duration_since(tokio::time::Instant::now());
    let hook_budget = hook_budget
        .min(Duration::from_secs(5))
        .max(Duration::from_millis(50));
    if !gate.forced() {
        registry
            .shutdown_all_sessions_with_hook_budget(hook_budget)
            .await;
    }

    // Force path (budget exhausted or second trigger): abort stragglers; the
    // RAII leases below still run. `reason` already latched the *original*
    // trigger; the outcome distinguishes only how the drain ended.
    let forced = gate.forced() || tokio::time::Instant::now() > deadline;
    if forced {
        tasks.abort_all();
    }
    drop(_instance_lock);

    if forced {
        tracing::warn!(
            %reason,
            "nuo server: forced exit — some teardown work was abandoned (see the task warnings above)"
        );
        RunOutcome::ForcedExit { reason }
    } else {
        RunOutcome::Stopped { reason }
    }
}

/// Bridge the serve-side gate (which the `Shutdown` control verb funnels
/// into) onto the run-loop gate. Serve's gate starts unarmed; arming it from
/// the run-loop's gate (which the signals feed) keeps one source of truth:
/// every request made on *either* gate lands in the run-loop's latch.
fn handle_gate(serve_gate: Arc<ShutdownGate>, run_gate: &Arc<ShutdownGate>) -> Arc<ShutdownGate> {
    let forwarding = Arc::clone(run_gate);
    tokio::spawn(async move {
        serve_gate.triggered().await;
        // Whatever triggered the serve gate (the control verb) forwards into
        // the run-loop's gate, preserving the reason if it latched one.
        let reason = serve_gate.reason().unwrap_or(ShutdownReason::ControlVerb);
        forwarding.request(reason, false);
    });
    // The run-loop keeps its own gate (signals + idle timer + forwarded
    // control verb); serve's gate exists only to receive the verb.
    Arc::clone(run_gate)
}

#[cfg(unix)]
fn is_uds_live(path: &std::path::Path) -> bool {
    if !path.exists() {
        return false;
    }
    std::os::unix::net::UnixStream::connect(path).is_ok()
}

#[cfg(not(unix))]
fn is_uds_live(_path: &std::path::Path) -> bool {
    false
}

fn is_tcp_port_live(port: u16) -> bool {
    if port == 0 {
        return false;
    }
    std::net::TcpStream::connect(("127.0.0.1", port)).is_ok()
}

async fn takeover_conflicting_server(
    lock_path: &std::path::Path,
    uds_path: Option<&std::path::Path>,
    port: u16,
) {
    let current_pid = std::process::id();
    let mut candidate_pids = std::collections::HashSet::new();

    if let Some(pid) = ProcessLock::probe_holder(lock_path) {
        if pid != current_pid {
            candidate_pids.insert(pid);
        }
    }
    let db_lock_path = nuo_host::paths::get().db_file().with_extension("db.owner.lock");
    if let Some(pid) = ProcessLock::probe_holder(&db_lock_path) {
        if pid != current_pid {
            candidate_pids.insert(pid);
        }
    }
    if let Some(record) = discovery::read() {
        if record.pid != current_pid {
            candidate_pids.insert(record.pid);
        }
    }

    let is_uds_in_use = uds_path.map(is_uds_live).unwrap_or(false);
    let is_port_in_use = is_tcp_port_live(port);
    let is_locked = ProcessLock::is_locked(lock_path);
    let is_db_locked = ProcessLock::is_locked(&db_lock_path);

    if !candidate_pids.is_empty() || is_uds_in_use || is_port_in_use || is_locked || is_db_locked {
        for pid in candidate_pids {
            tracing::warn!(pid, "interface or lock conflict detected; terminating existing instance for takeover");
            let _ = nuo_host::process::takeover_pid(pid, nuo_host::process::TakeoverOptions::default()).await;
        }

        if let Some(path) = uds_path {
            if path.exists() {
                let _ = std::fs::remove_file(path);
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Block until the lock at `path` is acquirable, polling with a total bound.
async fn wait_for_lock(path: &std::path::Path, budget: Duration) -> Result<ProcessLock, ()> {
    let deadline = tokio::time::Instant::now() + budget;
    loop {
        if let Ok(lock) = ProcessLock::acquire(path) {
            return Ok(lock);
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// The serving steady-state: wait for the first trigger. When `idle_exit`
/// is armed, also watch for "zero sessions + zero connections held for the
/// whole grace period" and request the IdleTimeout trigger.
/// When `client_driven` is armed (ADR-0029), auto-terminate when all
/// interactive TUI clients close.
async fn serve_until_trigger(
    gate: &Arc<ShutdownGate>,
    registry: &Arc<SessionRegistry>,
    handle: &crate::serve::ServeHandle,
    idle_exit: Option<Duration>,
    client_driven: bool,
) {
    let triggered = gate.triggered();
    tokio::pin!(triggered);
    let idle: std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> = match idle_exit {
        Some(grace) => idle_exit_future(registry, handle, grace),
        None => Box::pin(std::future::pending::<()>()),
    };
    tokio::pin!(idle);
    let client_driven_fut: std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> =
        if client_driven {
            Box::pin(client_driven_exit_future(handle.conns.clone()))
        } else {
            Box::pin(std::future::pending::<()>())
        };
    tokio::pin!(client_driven_fut);
    tokio::select! {
        _ = &mut triggered => {}
        _ = &mut idle => {
            gate.request(ShutdownReason::IdleTimeout, false);
        }
        _ = &mut client_driven_fut => {
            gate.request(ShutdownReason::AllClientsClosed, false);
        }
    }
}

async fn client_driven_exit_future(conns: Arc<crate::serve::ConnTable>) {
    let startup_deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while !conns.has_had_interactive() && conns.interactive_count() == 0 {
        if tokio::time::Instant::now() >= startup_deadline {
            tracing::info!("no interactive client connected within startup window; terminating server");
            return;
        }
        tokio::select! {
            _ = conns.notified() => {}
            _ = tokio::time::sleep(Duration::from_millis(200)) => {}
        }
    }

    loop {
        if conns.interactive_count() == 0 {
            tokio::select! {
                _ = conns.notified() => {
                    continue;
                }
                _ = tokio::time::sleep(Duration::from_millis(1500)) => {
                    if conns.interactive_count() == 0 {
                        tracing::info!("all interactive clients closed and debounce expired; terminating server");
                        return;
                    }
                }
            }
        } else {
            conns.notified().await;
        }
    }
}

/// Resolves after `grace` of continuous zero-sessions-zero-connections.
/// Resets its timer on any activity, so spawn/exit flapping between
/// back-to-back invocations never trips it.
fn idle_exit_future(
    registry: &Arc<SessionRegistry>,
    handle: &crate::serve::ServeHandle,
    grace: Duration,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
    let registry = registry.clone();
    let conns = handle.conns.clone();
    Box::pin(async move {
        let mut idle_since: Option<tokio::time::Instant> = None;
        loop {
            tokio::time::sleep(Duration::from_secs(5)).await;
            let empty = registry.session_count().await == 0 && conns.is_empty();
            if empty {
                let since = *idle_since.get_or_insert_with(tokio::time::Instant::now);
                if since.elapsed() >= grace {
                    return;
                }
            } else {
                idle_since = None;
            }
        }
    })
}
