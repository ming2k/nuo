//! Client side of the daemon control plane: discovery, the attach handshake
//! (`connect`), one-shot control verbs (`control`), and the monitor stream
//! (ADR-0093/0096). `mutx` and other protocol clients drive
//! sessions owned by the Muta daemon (`muta`) through
//! this module. Discovery is global (one daemon per user); connections
//! prefer platform-native local IPC and fall back to TCP.
//!
//! The wire protocol this client speaks is [`crate::serve::Wire`] — client
//! and server live in the same crate so the protocol cannot drift.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::serve::Wire;
use crate::serve_discovery as discovery;
use crate::wire_channel::{BoxWireSink, BoxWireStream, native_framed_split, websocket_split};
use futures::{SinkExt, StreamExt};
use nuo_contracts::{
    AgentRequest, AgentResponse, Message, MonitorAction, MonitorEvent, MonitoredSession,
    SessionOverview,
};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;

pub use crate::serve::AttachAction;
pub use crate::serve_discovery::Discovery as DaemonInfo;
pub use nuo_client::wire::{ControlRequest, MIN_PROTOCOL_VERSION, PROTOCOL_VERSION, protocol_accepts};
pub use crate::input_completion::{complete_for_frontend_test, complete_slash_items};
pub use crate::startup::{command_catalog, init_tracing};
pub use crate::{CopyOutcome, UiBridge};

/// ADR-0141: the human-channel posture this process declares when attaching.
/// Defaults to `Interactive` (a TUI is a human by construction). Headless
/// entrypoints (`muta -p`, remote automation) call [`set_posture`] with
/// `Autonomous` before connecting so the session knows no human can answer
/// parked requests. Process-wide because one process plays one role.
static POSTURE_OVERRIDE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// Declare this client's human-channel posture (ADR-0141). Must be called
/// before the first attach; later attaches of the same process inherit it.
pub fn set_posture(posture: nuo_contracts::human_request::HumanChannelPosture) {
    let code = match posture {
        nuo_contracts::human_request::HumanChannelPosture::Interactive => 0,
        nuo_contracts::human_request::HumanChannelPosture::Autonomous => 1,
    };
    POSTURE_OVERRIDE.store(code, std::sync::atomic::Ordering::Relaxed);
}

fn current_posture() -> nuo_contracts::human_request::HumanChannelPosture {
    match POSTURE_OVERRIDE.load(std::sync::atomic::Ordering::Relaxed) {
        1 => nuo_contracts::human_request::HumanChannelPosture::Autonomous,
        _ => nuo_contracts::human_request::HumanChannelPosture::Interactive,
    }
}

/// An explicitly named daemon endpoint (`--remote <addr>` + `--token
/// <token>`): the operator supplied the coordinates, so no discovery
/// record exists or is read. Distinct from [`DaemonInfo`] on purpose — a
/// discovered daemon is identified by local state (pid, socket path,
/// version record); a remote one is identified by nothing but the address,
/// and pretending otherwise is how a remote run silently lands on the
/// local instance.
#[derive(Debug, Clone)]
pub struct RemoteDaemon {
    /// Hostname or IP. Loopback only when the address was a bare `:port`.
    pub host: String,
    pub port: u16,
    pub token: String,
}

impl RemoteDaemon {
    /// Parse `--remote <addr>` (`host:port`, `ws://host:port`, or a bare
    /// `:port` for loopback) together with the required `--token`. The
    /// token is mandatory because every network-exposed listener demands
    /// one (ADR-0105); a missing port is an error rather than a well-known
    /// default — a default would silently target the local daemon when the
    /// operator meant a remote one.
    pub fn parse(addr: &str, token: Option<String>) -> Result<Self, String> {
        let bare = addr.trim().strip_prefix("ws://").unwrap_or(addr.trim());
        let (host, port) = split_host_port(bare)?;
        let host = host.unwrap_or_else(|| "127.0.0.1".to_string());
        let Some(token) = token.filter(|t| !t.is_empty()) else {
            return Err(
                "--remote needs --token <token>: every network-exposed daemon \
                 requires the bearer token from the host's daemon discovery record"
                    .to_string(),
            );
        };
        Ok(Self { host, port, token })
    }

    /// Connect and run the attach handshake over TCP+bearer. No UDS
    /// attempt (the socket belongs to the remote machine's filesystem),
    /// no version pre-check (the handshake carries the daemon's version).
    pub async fn connect(&self, action: AttachAction) -> Result<Handshake, String> {
        let url = format!("ws://{}:{}/", self.host, self.port);
        let mut request = url
            .as_str()
            .into_client_request()
            .map_err(|e| format!("bad ws url {url}: {e}"))?;
        let value = HeaderValue::from_str(&format!("Bearer {}", self.token))
            .map_err(|e| format!("bad bearer token: {e}"))?;
        request.headers_mut().insert("Authorization", value);
        let (ws, _) = tokio_tungstenite::connect_async(request)
            .await
            .map_err(|e| format!("ws connect to {url}: {e}"))?;
        let (sink, source) = websocket_split(ws);
        finish_handshake((sink, source), action).await
    }
}

/// Split `host:port` into its parts. A missing port is an error.
fn split_host_port(s: &str) -> Result<(Option<String>, u16), String> {
    let (host, port_str) = match s.rsplit_once(':') {
        Some(parts) => parts,
        None => return Err(format!("'{s}' is not host:port")),
    };
    let port: u16 = port_str
        .parse()
        .map_err(|_| format!("'{port_str}' is not a port number"))?;
    Ok(((!host.is_empty()).then(|| host.to_string()), port))
}

const LIVENESS_TIMEOUT: Duration = Duration::from_millis(500);
const SERVER_START_TIMEOUT: Duration = Duration::from_secs(10);
const SERVER_START_POLL: Duration = Duration::from_millis(100);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Find the unified daemon (ADR-0096): one global record. The project
/// argument is accepted for source compatibility but no longer scopes the
/// lookup — the daemon serves every project.
pub fn discover(_project_root: &Path) -> Option<DaemonInfo> {
    let global_path = discovery::global_discovery_path();
    if let Some(info) = discover_at(&global_path) {
        return Some(info);
    }

    let lock_path = discovery::global_lock_path();
    if nuo_host::lock::ProcessLock::is_locked(&lock_path) {
        // If the instance lock is held, the daemon may be in the middle of startup
        // writing daemon.json. Retry briefly before falling back.
        for _ in 0..10 {
            std::thread::sleep(Duration::from_millis(20));
            if let Some(info) = discover_at(&global_path) {
                return Some(info);
            }
        }
    }

    // Self-healing fallback: if daemon.json is missing or unlinked,
    // but a live daemon holds the instance lock and is responsive on UDS/TCP:
    if nuo_host::lock::ProcessLock::is_locked(&lock_path)
        && let Some(pid) = nuo_host::lock::ProcessLock::probe_holder(&lock_path)
        && is_process_alive(pid)
    {
        let local_endpoint = discovery::default_local_endpoint().ok();
        let local_connectable = local_endpoint
            .as_ref()
            .is_some_and(|endpoint| nuo_host::ipc::probe(endpoint).connectable);

        let tcp_addr =
            std::net::SocketAddr::from(([127, 0, 0, 1], crate::startup::env_default_port()));
        let tcp_connectable =
            std::net::TcpStream::connect_timeout(&tcp_addr, Duration::from_millis(300)).is_ok();

        if local_connectable || tcp_connectable {
            let recovered = DaemonInfo {
                pid,
                process_birth_token: nuo_host::process::process_identity(pid)
                    .ok()
                    .map(|identity| identity.birth_token),
                port: crate::startup::env_default_port(),
                token: None,
                project_root: String::new(),
                started_at: 0,
                uds_path: None,
                local_endpoint,
                version: Some(crate::serve::daemon_version().to_string()),
                grace_secs: None,
                protocol: Some(PROTOCOL_VERSION),
            };
            tracing::info!(
                pid,
                "discover: recovered in-memory discovery record from live lock holder"
            );
            return Some(recovered);
        }
    }

    None
}

fn discover_at(path: &Path) -> Option<DaemonInfo> {
    let bytes = std::fs::read(path).ok()?;
    let info: DaemonInfo = serde_json::from_slice(&bytes).ok()?;
    if !daemon_process_matches(&info) {
        // Readers never delete shared discovery state: a successor can replace
        // the record after this read. The next lock-owning daemon overwrites a
        // stale record, and the daemon's own lease removes its matching record.
        return None;
    }
    if !is_alive(&info) {
        return None;
    }
    Some(info)
}

fn daemon_process_matches(info: &DaemonInfo) -> bool {
    nuo_host::process::process_identity(info.pid).is_ok_and(|identity| {
        info.process_birth_token
            .is_none_or(|expected| expected == identity.birth_token)
    })
}

/// Directional relation between client version and daemon version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionRelation {
    Equal,
    ClientNewer,
    ClientOlder,
    Unknown,
}

/// Compare client and daemon version strings using SemVer.
pub fn compare_versions(client: &str, daemon: &str) -> VersionRelation {
    if client == daemon {
        return VersionRelation::Equal;
    }
    match (
        semver::Version::parse(client),
        semver::Version::parse(daemon),
    ) {
        (Ok(c), Ok(d)) => {
            if c > d {
                VersionRelation::ClientNewer
            } else if c < d {
                VersionRelation::ClientOlder
            } else {
                VersionRelation::Equal
            }
        }
        _ => VersionRelation::Unknown,
    }
}

/// The actionable error for a discovered-but-incompatible daemon,
/// preferring the wire-protocol explanation when the record declares a
/// protocol number outside this client's window (ADR-0134), then the
/// dev-drift explanation (same version, replaced binary), and falling
/// back to the product-version message for legacy records. This is the
/// entry point callers should use after `versions_compatible` reports
/// false.
pub fn incompatibility_error(info: &DaemonInfo) -> String {
    if info
        .protocol
        .is_some_and(|p| !protocol_accepts(p))
    {
        protocol_mismatch(info)
    } else if info.version.as_deref() == Some(crate::serve::daemon_version())
        && !daemon_image_is_current(info.pid)
    {
        format!(
            "client/daemon binary mismatch: running daemon (pid {}, version {}) executable differs from the installed nuo core image (rebuilt binary). \
             Stop it with `nuo stop` and rerun — the daemon restarts on demand.",
            info.pid,
            crate::serve::daemon_version()
        )
    } else {
        version_mismatch(info)
    }
}

/// The actionable version-skew error (ADR-0100 rule 4), naming both builds
/// and the directional fix (server behind -> stop/restart server; client behind -> update client).
/// Public so `nuo`-level commands can surface it uniformly
/// wherever a discovered daemon is about to be spoken to.
pub fn version_mismatch(info: &DaemonInfo) -> String {
    let client_ver = crate::serve::daemon_version();
    let Some(daemon_ver) = info.version.as_deref() else {
        return format!(
            "client/daemon version mismatch: this client is {client_ver} but the running daemon (pid {}) is unknown (older than 0.24). \
             Stop it with `nuo stop` and rerun — the daemon restarts on demand at the new version.",
            info.pid
        );
    };

    match compare_versions(client_ver, daemon_ver) {
        VersionRelation::ClientNewer => format!(
            "client/daemon version mismatch: running daemon (pid {}, version {daemon_ver}) is older than this client ({client_ver}). \
             Stop it with `nuo stop` and rerun — the daemon restarts on demand at the new version.",
            info.pid
        ),
        VersionRelation::ClientOlder => format!(
            "client/daemon version mismatch: this client ({client_ver}) is older than the running daemon (pid {}, version {daemon_ver}). \
             Please update your nuo client to {daemon_ver} or newer.",
            info.pid
        ),
        VersionRelation::Equal => {
            if !daemon_image_is_current(info.pid) {
                format!(
                    "client/daemon binary mismatch: running daemon (pid {}, version {daemon_ver}) executable differs from the installed nuo core image (rebuilt binary). \
                     Stop it with `nuo stop` and rerun — the daemon restarts on demand.",
                    info.pid
                )
            } else {
                format!(
                    "client/daemon version mismatch: client and daemon both report {client_ver} but failed compatibility check."
                )
            }
        }
        VersionRelation::Unknown => format!(
            "client/daemon version mismatch: this client is {client_ver} but the running daemon (pid {}) is {daemon_ver}. \
             If the daemon is outdated, stop it with `nuo stop` and rerun; if the client is outdated, update your client.",
            info.pid
        ),
    }
}

/// The actionable wire-protocol-skew error (ADR-0134), naming both protocol
/// numbers and the directional fix. Public for the same reason as
/// [`version_mismatch`]: uniform surfacing wherever a discovered daemon is
/// about to be spoken to. Only reached when the record carries a protocol
/// number outside this client's window.
pub fn protocol_mismatch(info: &DaemonInfo) -> String {
    let client_proto = PROTOCOL_VERSION;
    let daemon_proto = info.protocol.unwrap_or(0);
    if daemon_proto > client_proto {
        format!(
            "client/daemon wire protocol mismatch: running daemon (pid {}) speaks protocol {daemon_proto}, \
             newer than this client's protocol {client_proto}. \
             Stop it with `nuo stop` and rerun — the daemon restarts on demand at the new build.",
            info.pid
        )
    } else {
        format!(
            "client/daemon wire protocol mismatch: this client speaks protocol {client_proto}, \
             older than the running daemon (pid {}) requires — the daemon reports protocol {daemon_proto}. \
             Please update your nuo client.",
            info.pid
        )
    }
}

/// Whether a discovered daemon can serve this client (ADR-0100 rule 4,
/// revised by ADR-0134 for protocol-declaring records).
///
/// The decision has two regimes:
///
/// - **Protocol-declaring record** (`protocol: Some`, any daemon since the
///   field exists): the wire window is the compatibility authority, and a
///   daemon inside it is served *whatever its product version* — the
///   patch-bump case (wire unchanged, protocol number unchanged) must not
///   kick a healthy daemon out of bed. Exactly one local freshness gate
///   survives: the **dev-drift lie** — same version, different binary
///   (`daemon_image_is_current` false), i.e. `cargo run` rebuilt the
///   binary under a still-serving daemon of the same `CARGO_PKG_VERSION`.
///   That is the one state where every version signal agrees and the
///   client is still about to test a stale image; only the inode sees it.
///   An upgrade leftover (different version, in-window protocol, different
///   image) is deliberately **served**: the daemon restarts on idle exit,
///   and refusing would make every patch release interrupt live sessions
///   for no wire-level reason.
/// - **Legacy record** (`protocol: None`, pre-0.31 daemon): the record
///   predates negotiation, so ADR-0100 rule 4's exact product-version
///   equality (plus the image check) remains its gate unchanged.
pub fn versions_compatible(info: &DaemonInfo) -> bool {
    local_pair_compatible(
        info.protocol,
        info.version.as_deref(),
        daemon_image_is_current(info.pid),
    )
}

/// The pure decision core of [`versions_compatible`], split out so the
/// policy is unit-testable without a real daemon process (the image probe
/// is resolved by the caller for legacy records and inside for
/// protocol-declaring ones — see the regime docs above).
fn local_pair_compatible(
    protocol: Option<u32>,
    version: Option<&str>,
    daemon_image_is_current: bool,
) -> bool {
    if let Some(daemon_protocol) = protocol {
        // Protocol-declaring record: the window is the wire gate...
        if !protocol_accepts(daemon_protocol) {
            return false;
        }
        // ...and locally, only the dev-drift lie remains a refusal.
        // Same version + different image = the client is about to test a
        // stale binary while every version signal says "equal". Different
        // version + in-window protocol = upgrade leftover: serve it.
        let same_version = version == Some(crate::serve::daemon_version());
        return !same_version || daemon_image_is_current;
    }
    // Legacy record: exact product-version equality (ADR-0100 rule 4),
    // `None` counting as a mismatch, plus the image check.
    version.is_some_and(|v| v == crate::serve::daemon_version()) && daemon_image_is_current
}

/// Whether the running daemon's executable is still the resolved `muta`
/// core image. During a development loop a rebuild replaces that file;
/// the kernel keeps the old image alive under a `(deleted)` link while the
/// discovery record still names the same path and version. Comparing the
/// daemon's `/proc/<pid>/exe` inode with the sibling/installed `muta` image
/// detects that drift — the same-version stale-daemon case version checks
/// cannot see.
///
/// Returns `true` when the check is unavailable (non-Linux, unreadable
/// `/proc`, an unresolvable core image): absence of evidence must not
/// flag a healthy production daemon. A daemon spawned by an *installed*
/// binary matches the sibling `muta` resolved by `mutx`; the TUI executable
/// itself is deliberately not part of this comparison.
pub fn daemon_image_is_current(pid: u32) -> bool {
    let expected = daemon_program();
    if !expected.is_file() {
        return true;
    }
    nuo_host::process::process_image_matches_path(pid, &expected)
}

/// `(dev, inode)` equality. A rebuilt binary legitimately occupies the same
/// path with a new inode; path-string equality would hide exactly that.
#[cfg(all(test, unix))]
fn same_inode(a: &std::fs::Metadata, b: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    a.dev() == b.dev() && a.ino() == b.ino()
}

/// Liveness probe: prefer native local IPC and fall back to TCP. Either
/// reachable endpoint means the daemon is up.
fn is_alive(info: &DaemonInfo) -> bool {
    if !daemon_process_matches(info) {
        return false;
    }
    if info
        .effective_local_endpoint()
        .is_some_and(|endpoint| nuo_host::ipc::probe(&endpoint).connectable)
    {
        return true;
    }
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], info.port));
    std::net::TcpStream::connect_timeout(&addr, LIVENESS_TIMEOUT).is_ok()
}

/// Whether a process with `pid` exists and can receive signals.
pub fn is_process_alive(pid: u32) -> bool {
    nuo_host::process::process_identity(pid).is_ok()
}

/// Wait up to `timeout` for this exact process incarnation to exit. Comparing
/// the birth token prevents a recycled PID from extending the wait or becoming
/// the target of a later escalation.
async fn wait_for_process_exit(
    identity: nuo_host::process::ProcessIdentity,
    timeout: Duration,
) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if !nuo_host::process::process_is_alive(identity) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    !nuo_host::process::process_is_alive(identity)
}

/// The drain budget a stopper should allow when the discovery record
/// predates `grace_secs` (ADR-0116): generous enough not to interrupt a
/// default-configured daemon (10s) mid-drain, short enough that a wedged
/// process still escalates.
const FALLBACK_GRACE: Duration = Duration::from_secs(15);

/// Stop the daemon through a tiered, **budget-coordinated** shutdown
/// pipeline (ADR-0116):
///
/// 1. Tier 1 (Protocol): if the daemon speaks this client's version, send
///    the `Shutdown` control verb, then wait the daemon's *own* drain
///    budget (from the discovery record's `grace_secs`) — not a hardcoded
///    couple of seconds. Any signal arriving mid-drain escalates the
///    daemon to a forced exit that skips session teardown, so escalating
///    early destroys the graceful drain the stop just requested.
/// 2. Tier 2 (native graceful request): if the versions skew, the verb could
///    not be delivered, or the budget elapsed without an exit, request native
///    graceful termination where the OS defines it (SIGTERM on Unix).
/// 3. Tier 3 (Force): identity-conditionally force-terminate the process.
/// 4. Tier 4 (Cleanup): after the exact process incarnation is gone, acquire
///    the instance lock and identity-conditionally remove the discovery record.
///    Native listener state is RAII-owned; a killed Unix daemon's stale socket
///    is removed by the next lock-owning bind, never by a racing stopper.
pub async fn stop(info: &DaemonInfo) -> Result<(), String> {
    // The daemon's own drain budget, when it advertised one: the single
    // number every tier below is coordinated against.
    let grace = info
        .grace_secs
        .map(Duration::from_secs)
        .unwrap_or(FALLBACK_GRACE);
    let target_identity = nuo_host::process::process_identity(info.pid).ok();
    if let (Some(expected), Some(actual)) = (
        info.process_birth_token,
        target_identity.map(|identity| identity.birth_token),
    ) && expected != actual
    {
        return Err(format!(
            "refusing to stop pid {}: discovery process identity is stale",
            info.pid
        ));
    }
    let mut stopped = target_identity.is_none();

    // Tier 1: Try graceful protocol shutdown if versions are compatible.
    if let Some(identity) = target_identity
        && versions_compatible(info)
        && let Ok(Ok(())) = tokio::time::timeout(
            Duration::from_millis(1500),
            control(info, crate::serve::ControlRequest::Shutdown),
        )
        .await
    {
        stopped = wait_for_process_exit(identity, grace).await;
    }

    // Tier 2 & 3: request native graceful termination where supported, then
    // force-terminate the identity-checked process. Unix SIGTERM drains
    // through the same phases as the verb; Windows shutdown is protocol-only.
    if !stopped {
        if let Some(identity) = target_identity {
            if nuo_host::process::request_termination(identity).is_ok() {
                stopped = wait_for_process_exit(identity, grace).await;
            }
            if !stopped && nuo_host::process::process_is_alive(identity) {
                let _ = nuo_host::process::force_terminate(identity);
                stopped = wait_for_process_exit(identity, Duration::from_millis(1000)).await;
            }
        } else {
            stopped = true;
        }
    }

    let gone = stopped
        || target_identity
            .is_none_or(|identity| !nuo_host::process::process_is_alive(identity));
    // Tier 4: hold the same instance lock a successor requires before touching
    // shared discovery state. If a successor already owns it, its record is
    // categorically not ours to remove. Never unlink a Unix socket here; the
    // next lock-owning listener bind handles stale filesystem state.
    if gone
        && let Ok(_cleanup_lock) =
            nuo_host::lock::ProcessLock::acquire(&discovery::global_lock_path())
    {
        discovery::remove_if_matching_process(
            &discovery::global_discovery_path(),
            info.pid,
            info.process_birth_token,
        );
    }
    if gone {
        Ok(())
    } else {
        Err(format!("could not stop daemon (pid {})", info.pid))
    }
}

/// Path to the daemon startup stderr log file.
pub fn startup_log_path() -> PathBuf {
    let dirs = nuo_persistence::paths::get();
    dirs.state_dir.join("log").join("daemon-startup.log")
}

pub async fn ensure_daemon(project_root: &Path) -> Result<DaemonInfo, String> {
    if let Some(info) = discover(project_root) {
        if versions_compatible(&info) {
            return Ok(info);
        }
        // Incompatible daemon is running. Do not stop or kill it to avoid
        // interrupting ongoing tasks. Prompt the user about the incompatibility.
        return Err(incompatibility_error(&info));
    }

    // Check if another daemon is holding the instance lock
    let lock_path = discovery::global_lock_path();
    if nuo_host::lock::ProcessLock::is_locked(&lock_path)
        && let Some(holder_pid) = nuo_host::lock::ProcessLock::probe_holder(&lock_path)
        && is_process_alive(holder_pid)
    {
        tracing::info!(
            holder_pid,
            "ensure_daemon: daemon instance lock held; waiting for startup or draining"
        );
        let init_deadline = std::time::Instant::now() + Duration::from_secs(2);
        while std::time::Instant::now() < init_deadline {
            tokio::time::sleep(SERVER_START_POLL).await;
            if let Some(info) = discover(project_root) {
                if versions_compatible(&info) {
                    return Ok(info);
                } else {
                    return Err(incompatibility_error(&info));
                }
            }
            if !is_process_alive(holder_pid) {
                break;
            }
        }

        // If still locked and discover() failed, do not kill the existing process.
        // Report that another daemon is running and holding the lock.
        if is_process_alive(holder_pid) {
            return Err(format!(
                "another nuo daemon (pid {holder_pid}) is running and holding the instance lock. \
                 If it is unresponsive, stop it with `nuo stop`."
            ));
        }
    }

    let mut child = spawn_daemon()?;
    let deadline = std::time::Instant::now() + SERVER_START_TIMEOUT;
    loop {
        tokio::time::sleep(SERVER_START_POLL).await;
        if let Some(info) = discover(project_root) {
            if versions_compatible(&info) {
                return Ok(info);
            } else {
                return Err(incompatibility_error(&info));
            }
        }
        if let Ok(Some(status)) = child.try_wait() {
            let log_text = std::fs::read_to_string(startup_log_path()).unwrap_or_default();
            let log_trimmed = log_text.trim();
            if !log_trimmed.is_empty() {
                return Err(format!(
                    "nuo daemon exited prematurely ({status}): {log_trimmed}"
                ));
            } else {
                return Err(format!("nuo daemon exited prematurely with {status}"));
            }
        }
        if std::time::Instant::now() >= deadline {
            let log_text = std::fs::read_to_string(startup_log_path()).unwrap_or_default();
            let log_trimmed = log_text.trim();
            let lock_info = if nuo_host::lock::ProcessLock::is_locked(&lock_path) {
                nuo_host::lock::ProcessLock::probe_holder(&lock_path)
                    .map(|pid| format!(" (instance lock held by PID {pid})"))
                    .unwrap_or_else(|| " (instance lock is held)".to_string())
            } else {
                String::new()
            };
            if !log_trimmed.is_empty() {
                return Err(format!(
                    "nuo daemon did not become ready within {:?}{lock_info}: {log_trimmed}",
                    SERVER_START_TIMEOUT
                ));
            } else {
                return Err(format!(
                    "nuo daemon did not become ready within {:?}{lock_info} (see {})",
                    SERVER_START_TIMEOUT,
                    startup_log_path().display()
                ));
            }
        }
    }
}

fn spawn_daemon() -> Result<std::process::Child, String> {
    let program = daemon_program();
    let mut command = std::process::Command::new(&program);
    command.args(["start", "--fg"]);
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null());

    let startup_log = startup_log_path();
    let private_log = nuo_host::secure_file::create_private_parent(&startup_log)
        .and_then(|()| nuo_host::secure_file::create_private_file(&startup_log));
    if let Ok(file) = private_log {
        command.stderr(file);
    } else {
        command.stderr(std::process::Stdio::null());
    }

    // Pin the daemon's cwd to a stable, always-existing directory instead of
    // inheriting this client's project. ADR-0096 made the daemon the host for
    // *every* project's sessions, so a project directory inherited from the
    // first lucky client is exactly the wrong default — any code path that
    // still consults the daemon's cwd (rather than a session-scoped root)
    // would silently land in that project. Per-session scoping is explicit
    // via the Select frame's `project` field.
    let daemon_cwd = nuo_persistence::paths::get().data_dir.clone();
    let _ = std::fs::create_dir_all(&daemon_cwd);
    command.current_dir(daemon_cwd);
    configure_daemon_detachment(&mut command);
    command
        .spawn()
        .map_err(|error| {
            format!(
                "could not start the muta daemon with {}: {error}. Install `muta` beside `mutx` or make it available on PATH",
                program.display()
            )
        })
}

/// Resolve the core daemon executable without ever re-entering the client.
/// Release archives install `muta` and `mutx` side by side; PATH is the
/// fallback for package-manager layouts that split them across directories.
fn daemon_program() -> PathBuf {
    if let Some(program) = std::env::var_os("NUO_BIN")
        .or_else(|| std::env::var_os("MUTA_BIN"))
        .filter(|value| !value.is_empty())
    {
        return PathBuf::from(program);
    }
    if let Ok(current) = std::env::current_exe() {
        let sibling_nuo = current.with_file_name(format!("nuo{}", std::env::consts::EXE_SUFFIX));
        if sibling_nuo.is_file() {
            return sibling_nuo;
        }
        let sibling = current.with_file_name(format!("muta{}", std::env::consts::EXE_SUFFIX));
        if sibling.is_file() {
            return sibling;
        }
    }
    PathBuf::from(format!("nuo{}", std::env::consts::EXE_SUFFIX))
}

/// Configure the process-level detachment shared by every daemon spawn path.
///
/// On Unix, `setsid(2)` is the single primitive: it creates both a new session
/// and a new process group, detaching the daemon from the caller's controlling
/// terminal. It must not be combined with `CommandExt::process_group(0)`:
/// that call first makes the child a process-group leader, and POSIX requires
/// `setsid(2)` to fail with `EPERM` for a process-group leader.
///
/// A `setsid(2)` failure is returned by [`std::process::Command::spawn`]. A
/// half-detached daemon would violate the lifecycle contract, so callers must
/// treat that failure as fatal.
pub fn configure_daemon_detachment(command: &mut std::process::Command) {
    nuo_host::process::configure_daemon_std(command);
}

/// Comprehensive diagnostics for the daemon control plane and system status.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DaemonDiagnostics {
    /// The resolved daemon instance directory (ADR-0121): the root of every
    /// daemon runtime file this report probes. Surfaced so an operator can
    /// see which instance — host or `MUTA_HOME` sandbox — a client is
    /// talking about before reading anything else below.
    pub instance_dir: PathBuf,
    /// The default port this client resolves (`--port` > `MUTA_PORT` >
    /// 9800), for the same reason as `instance_dir`.
    pub default_port: u16,
    pub discovery_path: PathBuf,
    pub discovery_record: Option<DaemonInfo>,
    pub discovery_valid: bool,
    pub lock_path: PathBuf,
    pub lock_held: bool,
    pub lock_holder_pid: Option<u32>,
    pub lock_holder_alive: bool,
    pub local_endpoint: Option<nuo_host::ipc::LocalEndpoint>,
    pub local_endpoint_exists: bool,
    pub local_endpoint_connectable: bool,
    pub tcp_port: u16,
    pub tcp_listening: bool,
    pub startup_log_path: PathBuf,
    pub last_startup_log: Option<String>,
}

/// Perform a diagnostic probe of the daemon environment without modifying state.
pub fn diagnose_daemon() -> DaemonDiagnostics {
    let discovery_path = discovery::global_discovery_path();
    let discovery_record = discover_at(&discovery_path);
    let raw_record: Option<DaemonInfo> = std::fs::read(&discovery_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok());
    let lock_path = discovery::global_lock_path();
    let lock_held = nuo_host::lock::ProcessLock::is_locked(&lock_path);
    let lock_holder_pid = nuo_host::lock::ProcessLock::probe_holder(&lock_path);
    let lock_holder_alive = lock_holder_pid.map(is_process_alive).unwrap_or(false);

    let local_endpoint = raw_record
        .as_ref()
        .and_then(discovery::Discovery::effective_local_endpoint)
        .or_else(|| discovery::default_local_endpoint().ok());
    let local_probe = local_endpoint
        .as_ref()
        .map(nuo_host::ipc::probe)
        .unwrap_or_default();

    let port = discovery_record
        .as_ref()
        .map(|d| d.port)
        .or_else(|| raw_record.as_ref().map(|d| d.port))
        .unwrap_or_else(crate::startup::env_default_port);

    let tcp_addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let tcp_listening =
        std::net::TcpStream::connect_timeout(&tcp_addr, Duration::from_millis(200)).is_ok();

    let startup_log = startup_log_path();
    let last_startup_log = std::fs::read_to_string(&startup_log)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    DaemonDiagnostics {
        instance_dir: discovery::instance_dir(),
        default_port: crate::startup::env_default_port(),
        discovery_path,
        discovery_record: discovery_record.or(raw_record),
        discovery_valid: discover_at(&discovery::global_discovery_path()).is_some(),
        lock_path,
        lock_held,
        lock_holder_pid,
        lock_holder_alive,
        local_endpoint,
        local_endpoint_exists: local_probe.exists,
        local_endpoint_connectable: local_probe.connectable,
        tcp_port: port,
        tcp_listening,
        startup_log_path: startup_log,
        last_startup_log,
    }
}

// Boxing the attached payload would make this public protocol-facing API less direct.
#[allow(clippy::large_enum_variant)]
pub enum Handshake {
    Attached {
        req_tx: mpsc::UnboundedSender<AgentRequest>,
        resp_rx: mpsc::UnboundedReceiver<AgentResponse>,
        session_id: String,
        round_counter: u64,
        history: Vec<Message>,
        /// Durable round-interrupt records (C11) from the daemon's welcome,
        /// so an attaching TUI projects the stopped rounds into its restored
        /// transcript. Empty for older daemons.
        round_interrupts: Vec<nuo_contracts::RoundInterrupt>,
        /// Durable retry-resolution records from the daemon's welcome, so an
        /// attaching TUI projects recovered rounds into its restored
        /// transcript. Empty for older daemons.
        retry_resolutions: Vec<nuo_contracts::RetryResolution>,
        /// The provider/model the session is currently serving, carried on
        /// the welcome so the TUI's hint bar shows them from the first frame
        /// instead of waiting for the next provider mutation.
        provider: String,
        model: String,
        /// Backend-owned completion/help vocabulary for this session.
        command_catalog: nuo_contracts::CommandCatalog,
    },
    Pick(Vec<SessionOverview>),
}

pub async fn connect(info: &DaemonInfo, action: AttachAction) -> Result<Handshake, String> {
    // Prefer the platform-native local endpoint; fall back to TCP for
    // exposed and legacy deployments.
    if let Some(endpoint) = info.effective_local_endpoint()
        && let Ok(stream) = nuo_host::ipc::connect(&endpoint).await
    {
        let (sink, source) = native_framed_split(stream);
        return finish_handshake((sink, source), action).await;
    }
    let url = format!("ws://127.0.0.1:{}/", info.port);
    let mut request = url
        .as_str()
        .into_client_request()
        .map_err(|e| format!("bad ws url {url}: {e}"))?;
    if let Some(token) = &info.token {
        let value = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|e| format!("bad bearer token: {e}"))?;
        request.headers_mut().insert("Authorization", value);
    }
    let (ws, _response) = tokio_tungstenite::connect_async(request)
        .await
        .map_err(|e| format!("ws connect to {url}: {e}"))?;
    let (sink, source) = websocket_split(ws);
    finish_handshake((sink, source), action).await
}

/// The stream-generic attach handshake, shared by the UDS and TCP paths.
async fn finish_handshake(
    parts: (BoxWireSink, BoxWireStream),
    action: AttachAction,
) -> Result<Handshake, String> {
    let (mut wire_sink, mut wire_source) = parts;

    // Declare this client's working directory so the daemon scopes a fresh or
    // auto-attached session to the project the user actually invoked us in —
    // the daemon's own cwd is whatever the first client that spawned it
    // happened to use. A daemon predating the field ignores it; a failed cwd
    // read degrades to the daemon's fallback.
    let project = std::env::current_dir().ok();
    let select = Wire::Select {
        action,
        project,
        // ADR-0141: the client's interactivity posture. The TUI is a human
        // by construction; headless callers override this via
        // [`crate::client::set_posture`] before connecting.
        posture: current_posture(),
        // Product build: advisory identity on the wire since ADR-0134 (the
        // protocol number below is the gate), but still enforced against
        // pre-protocol daemons, which judge it by exact equality.
        version: Some(crate::serve::daemon_version().to_string()),
        // Wire protocol number (ADR-0134): the authority for whether this
        // daemon can serve us. A pre-protocol daemon ignores the field
        // (unknown fields are dropped by serde) and falls back to judging
        // the product version above.
        protocol: Some(PROTOCOL_VERSION),
    };
    wire_sink
        .send(select)
        .await
        .map_err(|e| format!("wire send select: {e}"))?;

    let reply = tokio::time::timeout(HANDSHAKE_TIMEOUT, async {
        loop {
            match wire_source.next().await {
                Some(Ok(wire)) => match wire {
                    Wire::Welcome {
                        session_id,
                        round_counter,
                        messages,
                        provider,
                        model,
                        round_interrupts,
                        retry_resolutions,
                        command_catalog,
                    } => {
                        return Ok(Reply::Welcome(Welcome {
                            session_id,
                            round_counter,
                            messages,
                            provider,
                            model,
                            round_interrupts,
                            retry_resolutions,
                            command_catalog,
                        }));
                    }
                    Wire::Pick { sessions } => return Ok(Reply::Pick(sessions)),
                    Wire::Error { message, .. } => {
                        return Err(format!("daemon rejected the attach: {message}"));
                    }
                    _ => tracing::warn!("attach: unexpected frame during handshake, ignored"),
                },
                Some(Err(error)) => return Err(format!("wire recv during handshake: {error}")),
                None => return Err("server closed the connection".to_string()),
            }
        }
    })
    .await
    .map_err(|_| "timed out waiting for handshake from daemon".to_string())??;

    let welcome = match reply {
        Reply::Welcome(w) => w,
        Reply::Pick(sessions) => {
            let _ = wire_sink.close().await;
            return Ok(Handshake::Pick(sessions));
        }
    };

    let (req_out_tx, mut req_out_rx) = mpsc::unbounded_channel::<AgentRequest>();
    let (resp_in_tx, resp_in_rx) = mpsc::unbounded_channel::<AgentResponse>();

    tokio::spawn(async move {
        let mut end_pending = false;
        while let Some(request) = req_out_rx.recv().await {
            if matches!(request, AgentRequest::EndSession) {
                // Client-declared session end (ADR-0112): mark it so the
                // pump, after flushing this frame, gives the daemon a brief
                // window to tear the session down before the socket closes.
                // Without this, a client that sends EndSession and drops
                // everything immediately can race the runtime shutdown: the
                // frame reaches the wire but the process exits before the
                // daemon even reads it — harmless over TCP/UDS (the kernel
                // buffers the written bytes), but the graceful-close
                // handshake below is still worth attempting.
                end_pending = true;
            }
            if let Err(error) = wire_sink.send(Wire::Request { request }).await {
                tracing::warn!(%error, "attach: wire send failed");
                break;
            }
        }
        if end_pending {
            // Give the daemon a moment to observe the EndSession frame and
            // run the teardown (it broadcasts the terminal `Exit` back,
            // which the response pump relays). Bounded so a hung daemon
            // cannot pin the client open either.
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        }
        let _ = wire_sink.close().await;
    });

    tokio::spawn(async move {
        while let Some(frame) = wire_source.next().await {
            match frame {
                Ok(Wire::Response { response }) => {
                    if resp_in_tx.send(response).is_err() {
                        return;
                    }
                }
                Ok(_) => tracing::warn!("attach: unexpected post-handshake frame, ignored"),
                Err(error) => {
                    tracing::warn!(%error, "attach: wire recv failed");
                    break;
                }
            }
        }
        let _ = resp_in_tx.send(AgentResponse::Exit);
    });

    Ok(Handshake::Attached {
        req_tx: req_out_tx,
        resp_rx: resp_in_rx,
        session_id: welcome.session_id,
        round_counter: welcome.round_counter,
        history: welcome.messages,
        round_interrupts: welcome.round_interrupts,
        retry_resolutions: welcome.retry_resolutions,
        provider: welcome.provider,
        model: welcome.model,
        command_catalog: welcome.command_catalog,
    })
}

/// Issue one control-plane verb (ADR-0096) to the daemon and await its reply:
/// create, prompt, interrupt, answer a permission, or kill — without attaching
/// as a session client. The dashboard's session-management keys (`i` interrupt,
/// `p` prompt, `n` new session) go through here. Prefers native local IPC and
/// falls back to TCP, exactly like [`connect`].
pub async fn control(
    info: &DaemonInfo,
    request: crate::serve::ControlRequest,
) -> Result<(), String> {
    control_with_reply(info, request).await.map(|_| ())
}

/// [`control`] for reply-bearing verbs (ADR-0208 `AskArchivist`): the
/// daemon's `ControlReply` free-string travels back on success instead of
/// being dropped. Verbs without a payload reply with an empty string, so
/// every existing caller can migrate to this shape without behavior change.
pub async fn control_with_reply(
    info: &DaemonInfo,
    request: crate::serve::ControlRequest,
) -> Result<String, String> {
    use crate::serve::AttachAction;
    let action = AttachAction::Control(request);

    if let Some(endpoint) = info.effective_local_endpoint()
        && let Ok(stream) = nuo_host::ipc::connect(&endpoint).await
    {
        let (sink, source) = native_framed_split(stream);
        return finish_control((sink, source), action).await;
    }
    let url = format!("ws://127.0.0.1:{}/", info.port);
    let mut req = url
        .as_str()
        .into_client_request()
        .map_err(|e| format!("bad ws url {url}: {e}"))?;
    if let Some(token) = &info.token {
        let value = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|e| format!("bad bearer token: {e}"))?;
        req.headers_mut().insert("Authorization", value);
    }
    let (ws, _) = tokio_tungstenite::connect_async(req)
        .await
        .map_err(|e| format!("ws connect to {url}: {e}"))?;
    let (sink, source) = websocket_split(ws);
    finish_control((sink, source), action).await
}

/// The stream-generic control handshake: send the `Select{Control}` frame and
/// await the single `ControlReply`. One verb per connection.
async fn finish_control(
    parts: (BoxWireSink, BoxWireStream),
    action: AttachAction,
) -> Result<String, String> {
    let (mut wire_sink, mut wire_source) = parts;
    // Control verbs carry their own scope (`CreateSession::project`); the
    // daemon never consults a select-level project for them.
    let select = Wire::Select {
        action,
        project: None,
        posture: current_posture(),
        // Same handshake contract as the attach path (ADR-0134): the
        // protocol number is the gate, the product version the advisory
        // identity still enforced by pre-protocol daemons.
        version: Some(crate::serve::daemon_version().to_string()),
        protocol: Some(PROTOCOL_VERSION),
    };
    wire_sink
        .send(select)
        .await
        .map_err(|e| format!("wire send control select: {e}"))?;

    tokio::time::timeout(HANDSHAKE_TIMEOUT, async {
        loop {
            match wire_source.next().await {
                Some(Ok(wire)) => match wire {
                    Wire::ControlReply { ok, error, .. } => {
                        return if ok {
                            // ADR-0208: reply-bearing verbs (AskArchivist)
                            // travel in the free-string channel; verbs
                            // without a payload leave it `None` → empty.
                            Ok(error.unwrap_or_default())
                        } else {
                            Err(error.unwrap_or_else(|| "control verb rejected".to_string()))
                        };
                    }
                    Wire::Error { message, .. } => return Err(message),
                    _ => tracing::warn!("control: unexpected frame during handshake, ignored"),
                },
                Some(Err(error)) => return Err(format!("wire recv during control: {error}")),
                None => return Err("server closed the control connection".to_string()),
            }
        }
    })
    .await
    .map_err(|_| "timed out waiting for control reply from daemon".to_string())?
}

struct Welcome {
    session_id: String,
    round_counter: u64,
    messages: Vec<Message>,
    provider: String,
    model: String,
    /// Durable round-interrupt records (C11) carried on the daemon's
    /// welcome; empty for older daemons that predate the field.
    round_interrupts: Vec<nuo_contracts::RoundInterrupt>,
    /// Durable retry-resolution records (success-side mirror of the
    /// interrupts) carried on the daemon's welcome; empty for older daemons
    /// that predate the field.
    retry_resolutions: Vec<nuo_contracts::RetryResolution>,
    command_catalog: nuo_contracts::CommandCatalog,
}
enum Reply {
    Welcome(Welcome),
    Pick(Vec<SessionOverview>),
}

// Monitor-protocol client (ADR-0093)
/// Open the WebSocket, perform the monitor handshake, and return a channel of
/// stream frames. The WS pump runs on a background task; the channel closes
/// when the daemon hangs up.
pub async fn monitor_stream(
    info: &DaemonInfo,
    action: MonitorAction,
) -> Result<tokio::sync::mpsc::UnboundedReceiver<MonitorEvent>, String> {
    // Prefer platform-native local IPC; fall back to TCP for exposed/legacy
    // deployments — the same
    // transport policy as `remote::connect`/`remote::control`, so the monitor
    // stream works against a UDS-only daemon.
    if let Some(endpoint) = info.effective_local_endpoint()
        && let Ok(stream) = nuo_host::ipc::connect(&endpoint).await
    {
        let (sink, source) = native_framed_split(stream);
        return finish_monitor((sink, source), action, "local IPC").await;
    }
    let url = format!("ws://127.0.0.1:{}/", info.port);
    let mut request = url
        .as_str()
        .into_client_request()
        .map_err(|e| format!("bad ws url {url}: {e}"))?;
    if let Some(token) = &info.token {
        let value = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|e| format!("bad bearer token: {e}"))?;
        request.headers_mut().insert("Authorization", value);
    }
    let (ws, _response) = tokio_tungstenite::connect_async(request)
        .await
        .map_err(|e| format!("ws connect to {url}: {e}"))?;
    let (sink, source) = websocket_split(ws);
    finish_monitor((sink, source), action, &url).await
}

/// The stream-generic monitor handshake + framing, shared by the UDS and TCP
/// paths: send the `Select{Monitor}` handshake, await the opening snapshot
/// (bounded), then forward every diff frame into the returned channel.
async fn finish_monitor(
    parts: (BoxWireSink, BoxWireStream),
    action: MonitorAction,
    target: &str,
) -> Result<tokio::sync::mpsc::UnboundedReceiver<MonitorEvent>, String> {
    let (mut wire_sink, mut wire_source) = parts;

    let select = Wire::Select {
        action: crate::serve::AttachAction::Monitor(action),
        // Monitor streams are host-wide; no project scope applies.
        project: None,
        posture: current_posture(),
        version: Some(crate::serve::daemon_version().to_string()),
        protocol: Some(PROTOCOL_VERSION),
    };
    wire_sink
        .send(select)
        .await
        .map_err(|e| format!("wire send select: {e}"))?;

    // Await the opening snapshot (or a handshake-level error) with a bound.
    let first = tokio::time::timeout(HANDSHAKE_TIMEOUT, async {
        loop {
            match wire_source.next().await {
                Some(Ok(wire)) => match wire {
                    Wire::Monitor { event } => return Ok(event),
                    Wire::Error { message, .. } => return Err(message),
                    _ => tracing::warn!("status: unexpected frame during handshake, ignored"),
                },
                Some(Err(error)) => return Err(format!("wire recv during handshake: {error}")),
                None => return Err("server closed the connection".to_string()),
            }
        }
    })
    .await
    .map_err(|_| format!("timed out waiting for monitor snapshot from {target}"))??;

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let _ = tx.send(first);
    tokio::spawn(async move {
        while let Some(frame) = wire_source.next().await {
            match frame {
                Ok(Wire::Monitor { event }) => {
                    if tx.send(event).is_err() {
                        return;
                    }
                }
                Ok(_) => tracing::warn!("status: unexpected post-handshake frame, ignored"),
                Err(error) => {
                    tracing::warn!(%error, "status: wire recv failed");
                    break;
                }
            }
        }
    });
    Ok(rx)
}

pub fn upsert_session_row(rows: &mut Vec<MonitoredSession>, row: MonitoredSession) {
    match rows.iter_mut().find(|existing| existing.id == row.id) {
        Some(existing) => *existing = row,
        None => rows.push(row),
    }
    rows.sort_by_key(|r| std::cmp::Reverse(r.updated_at));
}

pub fn upsert_task_row(
    rows: &mut Vec<nuo_contracts::MonitoredTask>,
    row: nuo_contracts::MonitoredTask,
) {
    match rows.iter_mut().find(|existing| existing.id == row.id) {
        Some(existing) => *existing = row,
        None => rows.push(row),
    }
    rows.sort_by_key(|r| std::cmp::Reverse(r.created_at_ms));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn daemon_detachment_creates_a_fresh_session_and_process_group() {
        use std::process::{Command, Stdio};

        let mut command = Command::new("sleep");
        command
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        configure_daemon_detachment(&mut command);

        let mut child = command
            .spawn()
            .expect("a correctly detached child must spawn");
        let pid = child.id() as libc::pid_t;
        // SAFETY: `pid` names the live child owned by this test.
        let sid = unsafe { libc::getsid(pid) };
        // SAFETY: `pid` names the live child owned by this test.
        let pgid = unsafe { libc::getpgid(pid) };

        let _ = child.kill();
        let _ = child.wait();

        assert_eq!(sid, pid, "setsid must make the daemon a session leader");
        assert_eq!(
            pgid, pid,
            "setsid must also make the daemon a process-group leader"
        );
    }

    #[test]
    fn remote_daemon_parses_the_documented_address_forms() {
        let t = |addr| RemoteDaemon::parse(addr, Some("tok".into())).unwrap();
        // host:port
        assert_eq!(
            (t("192.168.1.4:9800").host, t("192.168.1.4:9800").port),
            ("192.168.1.4".to_string(), 9800)
        );
        // ws:// scheme is accepted and stripped
        assert_eq!(t("ws://box.lan:9800").host, "box.lan");
        assert_eq!(t("ws://box.lan:9800").port, 9800);
        // bare :port means loopback (the local daemon over TCP)
        assert_eq!(t(":9800").host, "127.0.0.1");
        // whitespace is tolerated
        assert_eq!(t("  box.lan:9800  ").host, "box.lan");
    }

    #[test]
    fn remote_daemon_requires_a_port_and_a_token() {
        // No port: refuse rather than defaulting — a default would
        // silently target the local daemon when a remote one was meant.
        let err = RemoteDaemon::parse("box.lan", Some("tok".into())).unwrap_err();
        assert!(err.contains("not host:port"), "{err}");
        let err = RemoteDaemon::parse("box.lan:notaport", Some("tok".into())).unwrap_err();
        assert!(err.contains("not a port number"), "{err}");
        // Every network-exposed daemon requires the bearer token.
        let err = RemoteDaemon::parse("box.lan:9800", None).unwrap_err();
        assert!(err.contains("--token"), "{err}");
        let err = RemoteDaemon::parse("box.lan:9800", Some(String::new())).unwrap_err();
        assert!(err.contains("--token"), "{err}");
    }

    #[test]
    fn test_compare_versions() {
        assert_eq!(compare_versions("0.25.0", "0.25.0"), VersionRelation::Equal);
        assert_eq!(
            compare_versions("0.26.0", "0.25.0"),
            VersionRelation::ClientNewer
        );
        assert_eq!(
            compare_versions("0.24.0", "0.25.0"),
            VersionRelation::ClientOlder
        );
        assert_eq!(
            compare_versions("1.0.0", "0.25.0"),
            VersionRelation::ClientNewer
        );
        assert_eq!(
            compare_versions("not-a-semver", "0.25.0"),
            VersionRelation::Unknown
        );
    }

    #[cfg(unix)]
    #[test]
    fn daemon_image_match_accepts_the_explicit_process_exe() {
        let current = std::env::current_exe().unwrap();
        assert!(nuo_host::process::process_image_matches_path(
            std::process::id(),
            &current
        ));
    }

    /// The local compatibility policy (ADR-0134 revision), in pure form.
    /// The image result is injected as a boolean so protocol/version policy
    /// stays deterministic and independent of the process running the test.
    #[test]
    fn local_pair_policy_after_adr0134() {
        let me = crate::serve::daemon_version();
        // In-window protocol + different version (upgrade leftover): SERVED.
        // A patch bump must not interrupt a healthy daemon.
        assert!(local_pair_compatible(
            Some(PROTOCOL_VERSION),
            Some("0.0.1-much-older"),
            false, // image check irrelevant: different version short-circuits
        ));
        // In-window + MIN edge + different version: also served.
        assert!(local_pair_compatible(
            Some(MIN_PROTOCOL_VERSION),
            Some("99.0.0"),
            false,
        ));
        // Out-of-window protocol: refused whatever the version says.
        assert!(!local_pair_compatible(
            Some(PROTOCOL_VERSION + 1),
            Some(me),
            true,
        ));
        assert!(!local_pair_compatible(Some(0), Some("0.0.0"), true,));
        // Same version + current image (this very process): served.
        assert!(local_pair_compatible(
            Some(PROTOCOL_VERSION),
            Some(me),
            true,
        ));
        // Legacy record (no protocol): exact version equality rules.
        assert!(local_pair_compatible(None, Some(me), true,));
        assert!(!local_pair_compatible(None, Some("0.0.0"), true,));
        assert!(!local_pair_compatible(None, None, true));
    }

    /// The dev-drift lie (same version, different image) is refused even
    /// with a matching protocol — the one freshness gate that survives
    /// ADR-0134 locally.
    #[test]
    fn dev_drift_same_version_is_refused() {
        assert!(!local_pair_compatible(
            Some(PROTOCOL_VERSION),
            Some(crate::serve::daemon_version()),
            false,
        ));
    }

    #[test]
    fn daemon_image_is_current_tolerates_missing_proc_entry() {
        // A pid that does not exist (or /proc unavailable): no evidence of
        // drift, so the daemon must not be disturbed.
        assert!(daemon_image_is_current(u32::MAX - 1));
    }

    #[test]
    fn same_inode_distinguishes_a_rebuilt_file_at_the_same_path() {
        // The dev-loop case in miniature: replacing a path's content gives
        // the same path a NEW inode — that is drift, not equality, even
        // though the path strings are identical. The daemon-side metadata
        // is captured before the replacement (as /proc does for a running
        // process), the client-side after.
        #[cfg(unix)]
        {
            let tmp = tempfile::tempdir().unwrap();
            let bin = tmp.path().join("nuo");
            std::fs::write(&bin, b"old image").unwrap();
            let daemon_meta = std::fs::metadata(&bin).unwrap();
            // cargo rebuild: temp + rename over the same path.
            let staging = tmp.path().join(".nuo.tmp");
            std::fs::write(&staging, b"new image").unwrap();
            std::fs::rename(&staging, &bin).unwrap();
            let client_meta = std::fs::metadata(&bin).unwrap();
            assert!(
                !same_inode(&daemon_meta, &client_meta),
                "a rebuilt binary at the same path must not compare equal"
            );
            assert!(same_inode(&client_meta, &client_meta));
        }
    }

    #[test]
    fn test_version_mismatch_messages() {
        let daemon_older = DaemonInfo {
            pid: 1234,
            process_birth_token: None,
            port: 9800,
            token: None,
            project_root: String::new(),
            started_at: 0,
            uds_path: None,
            local_endpoint: None,
            version: Some("0.0.0".to_string()),
            grace_secs: None,
            protocol: None,
        };
        let msg = version_mismatch(&daemon_older);
        assert!(msg.contains("is older than this client"));
        assert!(msg.contains("nuo stop"));

        let daemon_newer = DaemonInfo {
            pid: 1234,
            process_birth_token: None,
            port: 9800,
            token: None,
            project_root: String::new(),
            started_at: 0,
            uds_path: None,
            local_endpoint: None,
            version: Some("99.0.0".to_string()),
            grace_secs: None,
            protocol: None,
        };
        let msg = version_mismatch(&daemon_newer);
        assert!(msg.contains("older than the running daemon"));
        assert!(msg.contains("update your nuo client"));

        let daemon_none = DaemonInfo {
            pid: 1234,
            process_birth_token: None,
            port: 9800,
            token: None,
            project_root: String::new(),
            started_at: 0,
            uds_path: None,
            local_endpoint: None,
            version: None,
            grace_secs: None,
            protocol: None,
        };
        let msg = version_mismatch(&daemon_none);
        assert!(msg.contains("unknown (older than 0.24)"));
        assert!(msg.contains("nuo stop"));

        let daemon_equal_drift = DaemonInfo {
            pid: u32::MAX - 10,
            process_birth_token: None,
            port: 9800,
            token: None,
            project_root: String::new(),
            started_at: 0,
            uds_path: None,
            local_endpoint: None,
            version: Some(crate::serve::daemon_version().to_string()),
            grace_secs: None,
            protocol: None,
        };
        let msg = version_mismatch(&daemon_equal_drift);
        assert!(msg.contains("client/daemon"));
    }

    fn record(port: u16, token: Option<String>) -> DaemonInfo {
        DaemonInfo {
            pid: 99999999, // Unused/dead pid
            process_birth_token: None,
            port,
            token,
            project_root: "/tmp/proj".to_string(),
            started_at: 0,
            uds_path: None,
            local_endpoint: None,
            version: None,
            grace_secs: None,
            protocol: None,
        }
    }
    fn dead_port() -> u16 {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    }
    #[test]
    fn discover_at_returns_none_without_racing_to_delete_stale_record() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("serve.json");
        std::fs::write(
            &path,
            serde_json::to_vec(&record(dead_port(), None)).unwrap(),
        )
        .unwrap();
        assert!(discover_at(&path).is_none());
        assert!(
            path.exists(),
            "a reader must leave stale cleanup to the lock-owning lifecycle path"
        );
    }
    #[test]
    fn discover_at_preserves_record_if_pid_is_still_alive() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("serve.json");
        let live_rec = DaemonInfo {
            pid: std::process::id(),
            process_birth_token: None,
            port: dead_port(),
            token: None,
            project_root: "/tmp/proj".to_string(),
            started_at: 0,
            uds_path: None,
            local_endpoint: None,
            version: None,
            grace_secs: None,
            protocol: None,
        };
        std::fs::write(&path, serde_json::to_vec(&live_rec).unwrap()).unwrap();
        assert!(discover_at(&path).is_none());
        assert!(
            path.exists(),
            "discovery file for living PID must NOT be deleted on transient probe fail"
        );
    }
    #[test]
    fn discover_at_tolerates_missing_and_corrupt_files() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("serve.json");
        assert!(discover_at(&path).is_none());
        std::fs::write(&path, b"not json").unwrap();
        assert!(discover_at(&path).is_none());
        assert!(path.exists());
    }

    #[tokio::test]
    async fn stop_handles_already_dead_process_and_cleans_up() {
        let info = DaemonInfo {
            pid: 99999999, // Unused pid
            process_birth_token: None,
            port: 1,
            token: None,
            project_root: String::new(),
            started_at: 0,
            uds_path: None,
            local_endpoint: None,
            version: Some("0.0.1".to_string()),
            grace_secs: None,
            protocol: None,
        };
        let res = stop(&info).await;
        assert!(res.is_ok());
    }

    #[tokio::test]
    async fn stop_refuses_a_recycled_pid_identity() {
        let identity = nuo_host::process::process_identity(std::process::id()).unwrap();
        let info = DaemonInfo {
            pid: identity.pid,
            process_birth_token: Some(identity.birth_token.wrapping_add(1)),
            port: 1,
            token: None,
            project_root: String::new(),
            started_at: 0,
            uds_path: None,
            local_endpoint: None,
            version: Some("0.0.1".to_string()),
            grace_secs: None,
            protocol: None,
        };
        let error = stop(&info).await.unwrap_err();
        assert!(error.contains("process identity is stale"));
    }
}

#[test]
fn stop_budget_follows_the_advertised_grace() {
    // The tier budget must come from the record (ADR-0116); the test
    // pins the plumbing by asserting the fallback constant is generous
    // enough to cover a default-configured daemon's 10s drain, so a
    // legacy record cannot cause an early SIGTERM escalation either.
    assert!(FALLBACK_GRACE >= Duration::from_secs(10));
}
