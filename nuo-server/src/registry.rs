use crate::UiBridge;
use crate::bootstrap::{self, BootstrapParams};
use crate::monitor::MonitorTracker;
use crate::serve::{AttachAction, AttachSyncBuffer, is_attach_sync_event};
use nuo_harness::{Agent, AgentIdentity, AgentRoleProfile};
use nuo_wire::{
    AgentRequest, AgentResponse, MonitorAction, MonitorEvent, MonitorSnapshot, MonitoredSession,
    PermissionDecision, SessionHosting, SessionOverview, SessionStatus,
};
use nuo_persistence::session::SessionStore;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{Mutex, broadcast, mpsc};
use tokio_util::sync::CancellationToken;

/// Wall-clock now in Unix-epoch milliseconds, for round-interrupt records
/// (C11). Same convention as the TUI's `now_epoch_ms`. Shared with the
/// driver's crash-residue inference.
pub(crate) fn unix_epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Clone)]
pub struct HostParams {
    pub identity: AgentIdentity,
    pub preset: AgentRoleProfile,
    pub ui: Arc<dyn UiBridge>,
}
pub struct HostedSession {
    /// The workspace root, when one is bound (ADR-0220: a workspace-free
    /// persona has none).
    pub workspace_root: Option<PathBuf>,
    /// ADR-0141: channel accounting shared with the assembled agent.
    pub human_channel: Arc<nuo_wire::human_request::HumanChannelAccountant>,
    pub session: Arc<SessionStore>,
    pub req_tx: mpsc::Sender<AgentRequest>,
    pub events: broadcast::Sender<AgentResponse>,
    pub cancel: CancellationToken,
    pub shared_confinement: nuo_wire::SharedConfinement,
    /// The panel-facing tracker folding this session's event stream
    /// (ADR-0093). Owned here so the broadcast-tap task can fold events in
    /// and the registry can read rows out for snapshots.
    pub tracker: Arc<Mutex<MonitorTracker>>,
    /// Durable workspace trust store for the project at
    /// [`HostedSession::workspace_root`]. Surfaced through [`BoundSession`]
    /// so the WS attach path can push the trust decision to a client when
    /// the workspace contributions are unreviewed (`WorkspaceTrustState::Quarantined`).
    pub security: Arc<nuo_persistence::workspace_security::WorkspaceSecurityStore>,

    /// Attach-time state-sync buffer: the startup events an attaching client
    /// cannot reconstruct (active provider/model, picker snapshot, key
    /// readiness, harness state). Filled by the broadcast-tap; snapshotted into
    /// each attaching client so it hydrates immediately.
    pub sync_buffer: Arc<Mutex<AttachSyncBuffer>>,
    /// Backend-owned slash-command vocabulary for every attached frontend.
    pub command_catalog: nuo_wire::CommandCatalog,
    /// When this hosted session was created (wall-clock, monotonic). Drives
    /// the idle reaper: a session that stays `is_empty_unpersisted` (no real
    /// content, never written to disk) past the idle TTL is reclaimed so
    /// abandoned empty sessions cannot accumulate in memory. `Instant` is
    /// process-local and never persisted, matching the in-memory-only nature
    /// of the sessions it guards.
    pub created_at: std::time::Instant,
    /// Last time the broadcast tap folded an event for this session
    /// (monotonic). Drives idle *suspension*: a persisted session with no
    /// clients attached and no activity for `IDLE_HOSTED_SESSION_TTL` is
    /// torn down in memory — its transcript is already durable, so the next
    /// attach lazy-resumes it. Before this, every real session a server ever
    /// hosted stayed resident forever (full transcript + agent + MCP
    /// runtime + two tasks each), so a multi-project server's memory grew
    /// monotonically with its session history.
    ///
    /// A `Mutex<Instant>` (not an atomic): this is written only by the
    /// once-a-minute reaper sweep and read nowhere else on the hot path.
    pub last_activity: Mutex<std::time::Instant>,
    /// The tap-tick watermark the reaper last observed. When it differs from
    /// [`Self::activity_tick`], events were folded since the last sweep and
    /// `last_activity` refreshes.
    pub last_seen_tick: std::sync::atomic::AtomicU64,
    /// Broadcast-tap side of the activity clock: bumped once per folded
    /// event under a cheap atomic so the reaper never touches the tracker
    /// mutex on the hot path.
    pub activity_tick: Arc<std::sync::atomic::AtomicU64>,
    /// Handle on the session's primary agent (the same `Arc` the bootstrap
    /// hands out as `agent_for_session_end`) so the registry can fire
    /// SessionEnd hooks when the session ends — killed over the
    /// control plane, reaped, or torn down on server shutdown. The driver
    /// task owns the agent otherwise. `None` only for hand-built test
    /// entries, which carry no agent and fire nothing.
    pub agent_for_session_end: Option<Arc<Agent>>,
}
#[derive(Clone)]
pub struct BoundSession {
    pub workspace_root: Option<std::path::PathBuf>,
    /// ADR-0141: the human-channel accountant for this hosted session.
    /// The WS attach layer ORs each client's declared posture in (attach /
    /// detach); the assembled agent's posture gate reads the effective
    /// value before parking any human request.
    pub human_channel: Arc<nuo_wire::human_request::HumanChannelAccountant>,
    pub session: Arc<SessionStore>,
    pub shared_confinement: nuo_wire::SharedConfinement,
    pub req_tx: mpsc::Sender<AgentRequest>,
    pub events: broadcast::Sender<AgentResponse>,
    /// Attach-time state-sync events buffered for this session (see
    /// [`HostedSession::sync_buffer`]). Non-destructively snapshotted by the WS
    /// layer into every attaching client right after it subscribes.
    pub sync_buffer: Arc<Mutex<AttachSyncBuffer>>,
    /// Server-level latest picker snapshot shared across sessions (see the
    /// registry's `latest_picker` cache). Spliced over the per-session
    /// buffer's picker at attach time so a new client hydrates the current
    /// *global* model ordering, not this session's last local one.
    pub server_picker: Arc<Mutex<Option<AgentResponse>>>,
    pub command_catalog: nuo_wire::CommandCatalog,
    /// Durable workspace trust store for this session's project. The
    /// WS attach path reads it to detect unreviewed workspace contributions
    /// (`WorkspaceTrustState::Quarantined`) and push the trust decision
    /// to the attaching client (see `serve.rs`).
    pub security: Arc<nuo_persistence::workspace_security::WorkspaceSecurityStore>,
}

impl BoundSession {
    /// The workspace root this session is bound to, when any.
    pub fn workspace_root(&self) -> Option<&std::path::Path> {
        self.workspace_root.as_deref()
    }

    /// Current project-asset trust snapshot. An unbound session has no project
    /// assets, so every domain is `Absent`.
    pub fn security_snapshot(&self) -> nuo_wire::WorkspaceSecuritySnapshot {
        match self.workspace_root() {
            Some(root) => self.security.snapshot(root),
            None => nuo_wire::WorkspaceSecuritySnapshot::new("workspace-free"),
        }
    }
}

/// A resolved session binding: an optional workspace plus the staffing role.
#[derive(Clone)]
pub struct SessionBinding {
    pub workspace: Option<nuo_wire::WorkspaceBinding>,
    pub role: Option<String>,
}

impl SessionBinding {
    pub fn workspace(root: PathBuf) -> Self {
        Self {
            workspace: Some(nuo_wire::WorkspaceBinding::new(root)),
            role: None,
        }
    }

    pub fn workspace_root(&self) -> Option<PathBuf> {
        self.workspace.as_ref().map(|binding| binding.root.clone())
    }
}
pub enum ResolveOutcome {
    Welcome(BoundSession),
    Pick { sessions: Vec<SessionOverview> },
    Error(String),
}

/// The host-level observability topic (ADR-0093): one broadcast channel
/// carrying [`MonitorEvent`] diffs for every hosted session. The registry is
/// the only publisher; any number of monitor clients subscribe. A fresh
/// subscriber is expected to first take a [`SessionRegistry::monitor_snapshot`]
/// so events published before it subscribed are not lost.
pub type MonitorBus = broadcast::Sender<MonitorEvent>;

/// Server provenance for monitor snapshots: who the host is and when it
/// started. A `/serve` prehost (no host record) reports `started_at: 0`.
#[derive(Clone, Default)]
pub struct MonitorMeta {
    pub project_root: Option<String>,
    pub started_at: u64,
}

#[derive(Clone)]
pub struct SessionRegistry {
    params: Option<HostParams>,
    sessions: Arc<Mutex<HashMap<String, Arc<HostedSession>>>>,
    monitor: MonitorBus,
    meta: Arc<Mutex<MonitorMeta>>,
    /// Server-level task fabric (ADR-0190 D6): rehosted services and other
    /// owner-less tasks live here, published onto the monitor bus as
    /// `TaskUpdated`/`TaskRemoved` diffs and folded into monitor snapshots.
    server_tasks: Arc<crate::background_jobs::BackgroundJobManager>,
    /// The live server-task rows folded for snapshots (id → row).
    server_task_rows: Arc<std::sync::Mutex<HashMap<String, nuo_wire::MonitoredTask>>>,
    /// Latest durability-health state (ADR-0196 D4): folded from the
    /// persistence supervisor's transitions for monitor snapshots and
    /// published as `PersistenceHealth` diffs. `None` = never degraded.
    persistence_health: Arc<std::sync::Mutex<Option<nuo_wire::monitor::PersistenceHealth>>>,
    /// Server-level latest picker snapshot (cross-session model ordering).
    /// The picker's recency data lives in the shared SQLite `ConnectionUsage`,
    /// so a snapshot built by any one session's driver is global truth — but
    /// the driver only pushes it onto its *own* bus. This cache (shared by
    /// every `BoundSession.server_picker`) lets a freshly attached client of
    /// any session hydrate the current global ordering instead of the stale
    /// one captured at that session's last local switch.
    latest_picker: Arc<Mutex<Option<AgentResponse>>>,
    /// Authoritative runtime configuration shared across all hosted sessions (ADR-0209).
    shared_config: crate::SharedConfig,
    /// Authoritative model recency telemetry shared across all hosted sessions (ADR-0209).
    shared_provider_usage: crate::SharedConnectionUsage,
    /// The server's Archivist conversational service (ADR-0208): lazily
    /// consulted by the `AskArchivist` control verb; one agent, borrowed
    /// provider per round.
    archivist: Arc<crate::archivist_service::ArchivistService>,
    /// The server's agent communication fabric (ACP). The Hypervisor
    /// and every hosted session register their agent endpoints here.
    fabric: acp::Fabric,
    /// The session mailboxes' parked handles so teardown can drop them
    /// deterministically.
    session_mailboxes: Arc<Mutex<HashMap<String, acp::Mailbox>>>,
    /// The Hypervisor station, constructed lazily on the first
    /// hosted-session assemble.
    hypervisor: Arc<Mutex<Option<Arc<crate::hypervisor::Hypervisor>>>>,
}

/// How long a never-persisted (empty) hosted session may sit idle before the
/// reaper reclaims it. Five minutes is comfortably longer than any legitimate
/// create→attach→first-prompt gap, so an empty session a user is about to
/// type into is never swept from under them.
const IDLE_EMPTY_SESSION_TTL: std::time::Duration = std::time::Duration::from_secs(5 * 60);

/// How often the storage-maintenance pass (blob GC + usage-day retention)
/// runs inside the idle reaper's tick loop. Both phases scan the whole data
/// dir; daily is the right cadence for reclaiming garbage.
const STORAGE_MAINTENANCE_INTERVAL: std::time::Duration =
    std::time::Duration::from_secs(24 * 60 * 60);

/// How often the idle-empty reaper sweeps. One minute keeps abandoned empty
/// sessions bounded without meaningfully waking the server.
const IDLE_REAPER_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);

/// Per-session budget for `SessionEnd` hooks during teardown (ADR-0101).
/// A user hook runs an external process; a hung one must not pin the server
/// (this bound applies to single-session kills; server shutdown sizes the
/// same budget against its remaining grace).
const DEFAULT_SESSION_END_HOOK_BUDGET: std::time::Duration = std::time::Duration::from_secs(5);
/// How long an idle *persisted* hosted session with no attached clients may
/// stay resident before it is suspended (torn down in memory; the transcript
/// is durable, so the next attach lazy-resumes it). This is what bounds the
/// server's memory: without it, every real session a server ever hosted
/// stayed resident forever (full transcript + agent + MCP runtime + tasks).
const IDLE_HOSTED_SESSION_TTL: std::time::Duration = std::time::Duration::from_secs(30 * 60);

impl SessionRegistry {
    pub fn new(params: HostParams) -> Self {
        Self::with_meta(Some(params))
    }
    pub fn prehost_only() -> Self {
        Self::with_meta(None)
    }
    fn with_meta(params: Option<HostParams>) -> Self {
        let (monitor, _) = broadcast::channel::<MonitorEvent>(256);
        // Construction-order scaffold: the Archivist service joins the mesh
        // tracker at build time (its mailbox + delegation tool both bind to
        // the one tracker), so the tracker is built first and handed over.
        let fabric = acp::Fabric::new("nuo-local");
        let archivist = Arc::new(crate::archivist_service::ArchivistService::with_fabric(
            fabric.clone(),
        ));
        Self {
            params,
            sessions: Arc::new(Mutex::new(HashMap::new())),
            monitor,
            meta: Arc::new(Mutex::new(MonitorMeta::default())),
            server_tasks: Arc::new(crate::background_jobs::BackgroundJobManager::new()),
            server_task_rows: Arc::new(std::sync::Mutex::new(HashMap::new())),
            persistence_health: Arc::new(std::sync::Mutex::new(None)),
            latest_picker: Arc::new(Mutex::new(None)),
            shared_config: Arc::new(tokio::sync::RwLock::new(
                nuo_persistence::config::Config::load(),
            )),
            shared_provider_usage: Arc::new(tokio::sync::RwLock::new(
                nuo_persistence::connection_usage::ConnectionUsage::load(),
            )),
            fabric,
            archivist,
            session_mailboxes: Arc::new(Mutex::new(HashMap::new())),
            hypervisor: Arc::new(Mutex::new(None)),
        }
    }

    /// The server-level task fabric (ADR-0190 D6). Rehosted services and
    /// other owner-less tasks spawn here; every lifecycle event is folded
    /// into the snapshot cache and published as a monitor diff.
    pub fn server_tasks(&self) -> Arc<crate::background_jobs::BackgroundJobManager> {
        self.server_tasks.clone()
    }

    /// Spawn a task on the server fabric: monitor-visible, owner-less.
    pub async fn spawn_server_task(
        &self,
        command: String,
        label: Option<String>,
        kind: nuo_wire::JobKind,
    ) -> Result<nuo_wire::BackgroundJobInfo, String> {
        let mgr = self.server_tasks.clone();
        let roots: Vec<std::path::PathBuf> = Vec::new();
        let workspace = std::env::temp_dir();
        mgr.spawn_process_ex(
            command,
            crate::background_jobs::ProcessSpawnOptions {
                label,
                cwd: None,
                workspace_root: &workspace,
                additional_roots: &roots,
                detached: true,
                timeout: None,
                owner_session: None,
            },
            kind,
            Some(nuo_wire::Readiness::FirstOutput),
            None,
        )
        .await
    }

    /// Stop a server-level task (the human-side control the TUI panel will
    /// drive).
    pub fn stop_server_task(&self, task_id: &str) -> Result<(), String> {
        self.server_tasks
            .kill_job(&nuo_wire::JobId::from(task_id))
    }

    /// Subscribe the durability-health tap (ADR-0196 D4): the persistence
    /// supervisor's transitions are folded into the snapshot cache and
    /// published as `MonitorEvent::PersistenceHealth` diffs, so every
    /// frontend (and `nuo status`) sees degradation, not just the log.
    #[allow(clippy::unwrap_used)] // A poisoned snapshot mutex is an unrecoverable registry invariant.
    pub fn start_persistence_health_monitor(self: &Arc<Self>) {
        let registry = Arc::downgrade(self);
        let mut rx = nuo_persistence::db::get_persistence_handle().subscribe_health();
        tokio::spawn(async move {
            loop {
                if rx.changed().await.is_err() {
                    break; // supervisor gone: nothing left to report
                }
                let wire = rx.borrow().clone().to_wire();
                if let Some(r) = registry.upgrade() {
                    *r.persistence_health.lock().unwrap() = Some(wire.clone());
                    r.publish_host_event(MonitorEvent::PersistenceHealth(wire));
                }
            }
        });
    }

    /// Cross-session picker fan-out: refresh the server-level latest-picker
    /// cache and rebroadcast the snapshot to every *other* hosted session's
    /// bus, so a model switch made in one session is visible (and hydratable
    /// at attach time) everywhere. Called by the per-session broadcast tap
    /// whenever its driver emits a `ProviderPicker`.
    pub async fn broadcast_picker_to_other_sessions(&self, origin: &str, response: AgentResponse) {
        {
            let mut latest = self.latest_picker.lock().await;
            *latest = Some(response.clone());
        }
        let map = self.sessions.lock().await;
        for (id, session) in map.iter() {
            if id != origin {
                let _ = session.events.send(response.clone());
            }
        }
    }

    /// The server-level latest picker snapshot, for attach-time hydration
    /// (`None` before any session has pushed a picker this process lifetime —
    /// per-session attach-sync buffers still cover that case).
    pub async fn latest_picker(&self) -> Option<AgentResponse> {
        self.latest_picker.lock().await.clone()
    }

    /// Ask the Archivist (ADR-0208) one question, synchronously. The round
    /// borrows the live provider of any hosted session's agent (all sessions
    /// resolve the same shared channel after a switch); with no hosted
    /// session the round runs on the last-bound channel or refuses via the
    /// `NoProvider` sentinel.
    pub async fn ask_archivist(&self, text: String) -> crate::archivist_service::ArchivistAnswer {
        let provider: Option<Arc<dyn nuo_wire::Provider>> = {
            let map = self.sessions.lock().await;
            map.values()
                .find_map(|e| e.agent_for_session_end.as_ref())
                .map(|agent| agent.provider.clone())
        };
        let borrowed = provider.unwrap_or_else(|| Arc::new(nuo_harness::NoProvider));
        self.archivist.ask(borrowed, &text).await
    }

    /// The server's ACP communication fabric.
    pub fn fabric(&self) -> &acp::Fabric {
        &self.fabric
    }

    /// The Hypervisor station, once materialized (lazily constructed on the
    /// first hosted-session assemble). `None` before any session has hosted —
    /// a server with no fleet has nothing to orchestrate.
    pub async fn hypervisor(&self) -> Option<Arc<crate::hypervisor::Hypervisor>> {
        self.hypervisor.lock().await.clone()
    }

    /// Register this session's master at `agent://local/session/<id>` on the ACP fabric.
    ///
    /// Returns the session's [`acp::MailboxHandle`] so the caller can wire ACP
    /// collaboration tools into the live agent (ADR-0008 step 8 / ADR-0009 §3).
    async fn register_session_mailbox(&self, session_id: &str) -> acp::MailboxHandle {
        let addr = acp::AgentAddress::parse(&format!("agent://local/session/{session_id}"))
            .expect("valid session address");
        let manifest = acp::AgentManifest::new(
            addr.clone(),
            format!("Session {session_id}"),
            "Hosted session root agent",
        );
        let mailbox = self.fabric.join(manifest, 64).await;
        let handle = mailbox.handle();
        self.session_mailboxes
            .lock()
            .await
            .insert(session_id.to_string(), mailbox);

        let mut hypervisor = self.hypervisor.lock().await;
        if hypervisor.is_none() {
            *hypervisor = Some(Arc::new(crate::hypervisor::Hypervisor::new(
                Arc::new(nuo_harness::NoProvider),
                self.clone(),
                self.fabric.clone(),
            ).await));
        }
        handle
    }

    /// Drop one session's ACP mailbox (teardown path).
    async fn drop_session_mailbox(&self, session_id: &str) {
        if let Some(_mb) = self.session_mailboxes.lock().await.remove(session_id) {
            if let Ok(addr) = acp::AgentAddress::parse(&format!("agent://local/session/{session_id}")) {
                self.fabric.leave(&addr).await;
            }
        }
    }

    /// Fold one fabric event into the snapshot cache and publish the diff.
    /// Runs on a dedicated subscriber task per registry instance.
    #[allow(clippy::unwrap_used)] // A poisoned task-row mutex is an unrecoverable registry invariant.
    pub fn start_server_task_monitor(self: &Arc<Self>) {
        let mgr = self.server_tasks.clone();
        let rows = self.server_task_rows.clone();
        let registry = Arc::downgrade(self);
        tokio::spawn(async move {
            let mut rx = mgr.subscribe();
            loop {
                match rx.recv().await {
                    Ok(crate::background_jobs::BackgroundJobEvent::Started(info)) => {
                        Self::upsert_task_row(&rows, &info, None);
                        if let Some(r) = registry.upgrade()
                            && let Some(task) = rows.lock().unwrap().get(&info.id.0)
                        {
                            r.publish_host_event(MonitorEvent::TaskUpdated(task.clone()));
                        }
                    }
                    Ok(crate::background_jobs::BackgroundJobEvent::Progress { job_id, line }) => {
                        let changed = {
                            let mut guard = rows.lock().unwrap();
                            if let Some(row) = guard.get_mut(&job_id.0) {
                                row.latest_output = Some(line);
                                true
                            } else {
                                false
                            }
                        };
                        if changed
                            && let Some(r) = registry.upgrade()
                            && let Some(task) = rows.lock().unwrap().get(&job_id.0)
                        {
                            r.publish_host_event(MonitorEvent::TaskUpdated(task.clone()));
                        }
                    }
                    Ok(crate::background_jobs::BackgroundJobEvent::Ready { job_id }) => {
                        let changed = {
                            let mut guard = rows.lock().unwrap();
                            if let Some(row) = guard.get_mut(&job_id.0) {
                                row.state = nuo_wire::JobState::Ready {
                                    started_at_ms: row.created_at_ms,
                                    ready_at_ms: crate::registry::unix_epoch_ms(),
                                };
                                true
                            } else {
                                false
                            }
                        };
                        if changed
                            && let Some(r) = registry.upgrade()
                            && let Some(task) = rows.lock().unwrap().get(&job_id.0)
                        {
                            r.publish_host_event(MonitorEvent::TaskUpdated(task.clone()));
                        }
                    }
                    Ok(crate::background_jobs::BackgroundJobEvent::Completed(outcome)) => {
                        if let Some(info) = mgr.get_job(&outcome.job_id) {
                            Self::upsert_task_row(&rows, &info, Some(&outcome));
                            if let Some(r) = registry.upgrade()
                                && let Some(task) = rows.lock().unwrap().get(&outcome.job_id.0)
                            {
                                r.publish_host_event(MonitorEvent::TaskUpdated(task.clone()));
                            }
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                }
            }
        });
    }

    #[allow(clippy::unwrap_used)] // A poisoned task-row mutex is an unrecoverable registry invariant.
    fn upsert_task_row(
        rows: &Arc<std::sync::Mutex<HashMap<String, nuo_wire::MonitoredTask>>>,
        info: &nuo_wire::BackgroundJobInfo,
        outcome: Option<&nuo_wire::BackgroundJobOutcome>,
    ) {
        let mut guard = rows.lock().unwrap();
        let row = guard.entry(info.id.0.clone()).or_insert_with(|| {
            let (label, spec) = match &info.spec {
                nuo_wire::JobSpec::Process { command, label, .. } => (
                    label.clone().unwrap_or_else(|| {
                        command
                            .split_whitespace()
                            .next()
                            .unwrap_or("task")
                            .to_string()
                    }),
                    command.clone(),
                ),
                nuo_wire::JobSpec::Timer { label, prompt, .. } => (
                    label.clone().unwrap_or_else(|| "timer".to_string()),
                    prompt.clone(),
                ),
            };
            let log_path = None;
            nuo_wire::MonitoredTask {
                id: info.id.0.clone(),
                label,
                spec,
                state: info.state.clone(),
                owner_session: None,
                created_at_ms: info.created_at_ms,
                completed_at_ms: info.completed_at_ms,
                latest_output: info.latest_output.clone(),
                log_path,
            }
        });
        row.state = match outcome {
            Some(o) => o.state.clone(),
            None => info.state.clone(),
        };
        row.completed_at_ms = info.completed_at_ms.or(row.completed_at_ms);
        if let Some(o) = outcome {
            row.latest_output = Some(o.summary.clone());
        }
    }
    /// Look up an active hosted session by its ID.
    pub async fn get(&self, session_id: &str) -> Option<Arc<HostedSession>> {
        self.sessions.lock().await.get(session_id).cloned()
    }

    /// Record the host's provenance (project root + start time) so monitor
    /// snapshots can identify the host. Called once by `host::run` before
    /// the listener starts accepting monitor clients.
    pub async fn set_monitor_meta(&self, project_root: String, started_at: u64) {
        *self.meta.lock().await = MonitorMeta {
            project_root: Some(project_root),
            started_at,
        };
    }
    /// Resolve an attach. `caller_project` scopes creation and lazy resume
    /// (ADR-0096): `New` / `Attach(None)` create in the caller's project;
    /// `Attach(Some(id))` first looks up every hosted session globally, then
    /// lazy-resumes from the caller's project on disk.
    pub async fn resolve(
        &self,
        action: AttachAction,
        caller_project: &std::path::Path,
    ) -> ResolveOutcome {
        self.resolve_with_declaration(action, caller_project, true)
            .await
    }

    /// [`Self::resolve`] with the caller's declaration status made explicit.
    /// A *declared* project (modern client's `Select.project`) forbids
    /// silently auto-binding a session from another project — that is the
    /// "launched in project A, working in project B" trap. An undeclared
    /// project (legacy client; the server guessing its own cwd) keeps the
    /// historical lone-session auto-bind because the server cannot tell a
    /// cross-project attach from a same-project one.
    pub async fn resolve_with_declaration(
        &self,
        action: AttachAction,
        caller_project: &std::path::Path,
        declared: bool,
    ) -> ResolveOutcome {
        match action {
            AttachAction::New(options) => {
                self.create_session_outcome(
                    caller_project.to_path_buf(),
                    options.unwrap_or_default(),
                )
                .await
            }
            AttachAction::Attach(None) => self.resolve_auto(caller_project, declared).await,
            AttachAction::Attach(Some(id)) => self.resolve_id(&id, caller_project).await,
            // The picker carrier (ADR-0116): a throwaway session whose only
            // job is to host the client's picker modal. The bootstrap skips
            // restore and hooks for it; `/sessions <id>` switches to the
            // real session through the ordinary re-attach path.
            AttachAction::Picker(init_options) => {
                let opts = init_options.unwrap_or_default();
                let role_id = opts.role.clone();
                let ws = if role_id.as_deref() == Some("philosophist") {
                    None
                } else if let Some(r_id) = role_id.as_deref() {
                    let roles_cfg = nuo_persistence::roles::RolesConfig::load_for_workspace(Some(
                        caller_project,
                    ));
                    if let Some(role_entry) = roles_cfg.get(r_id) {
                        match role_entry.resolved_workspace() {
                            nuo_persistence::roles::RoleWorkspace::None => None,
                            nuo_persistence::roles::RoleWorkspace::Inherit => Some(
                                nuo_wire::WorkspaceBinding::new(caller_project.to_path_buf()),
                            ),
                            nuo_persistence::roles::RoleWorkspace::Fixed(root) => {
                                Some(nuo_wire::WorkspaceBinding::new(root))
                            }
                        }
                    } else {
                        Some(nuo_wire::WorkspaceBinding::new(
                            caller_project.to_path_buf(),
                        ))
                    }
                } else {
                    Some(nuo_wire::WorkspaceBinding::new(
                        caller_project.to_path_buf(),
                    ))
                };
                self.assemble_hosted(
                    crate::startup::SessionStart::Picker,
                    SessionBinding {
                        workspace: ws,
                        role: role_id,
                    },
                    opts,
                )
                .await
                .map(ResolveOutcome::Welcome)
                .unwrap_or_else(|e| match e {
                    AssembleErr::NoHost => {
                        ResolveOutcome::Error("this host cannot create sessions".into())
                    }
                    AssembleErr::AssembleFailed(e) => {
                        ResolveOutcome::Error(format!("could not open the session picker: {e}"))
                    }
                })
            }
            // Monitor handshakes are intercepted by the WS layer
            // (`serve::handle_connection`) and never reach `resolve`.
            AttachAction::Monitor(_) => {
                ResolveOutcome::Error("monitor connections are served directly".into())
            }
            AttachAction::Control(_) => {
                ResolveOutcome::Error("control connections are served directly".into())
            }
        }
    }

    /// Control plane (ADR-0096): create a session for `project` and return
    /// its id. The session is server-held; no client is attached yet.
    pub async fn create_session(&self, project: PathBuf) -> Result<String, String> {
        self.create_session_with_options(project, nuo_client::wire::SessionInitOptions::default())
            .await
    }

    /// Control plane: create a session for `project` with explicit initial posture options.
    pub async fn create_session_with_options(
        &self,
        project: PathBuf,
        options: nuo_client::wire::SessionInitOptions,
    ) -> Result<String, String> {
        let bound = self
            .assemble_hosted(
                crate::startup::SessionStart::Fresh,
                SessionBinding::workspace(project),
                options,
            )
            .await
            .map_err(|e| match e {
                AssembleErr::NoHost => "this host cannot create sessions".to_string(),
                AssembleErr::AssembleFailed(e) => format!("could not start a new session: {e}"),
            })?;
        Ok(bound.session.id().await)
    }

    async fn create_session_outcome(
        &self,
        project: PathBuf,
        options: nuo_client::wire::SessionInitOptions,
    ) -> ResolveOutcome {
        match self
            .assemble_hosted(
                crate::startup::SessionStart::Fresh,
                SessionBinding::workspace(project),
                options,
            )
            .await
        {
            Ok(b) => ResolveOutcome::Welcome(b),
            Err(AssembleErr::NoHost) => {
                ResolveOutcome::Error("this host cannot create sessions".into())
            }
            Err(AssembleErr::AssembleFailed(e)) => {
                ResolveOutcome::Error(format!("could not start a new session: {e}"))
            }
        }
    }
    /// Control plane (ADR-0096): send an interrupt to a hosted session.
    pub async fn interrupt(&self, session_id: &str) -> Result<(), String> {
        let map = self.sessions.lock().await;
        let Some(e) = map.get(session_id) else {
            return Err(format!(
                "session '{session_id}' is not hosted on this server"
            ));
        };
        let _ = e.req_tx.send(AgentRequest::Interrupt).await;
        Ok(())
    }

    /// Control plane (ADR-0096): answer a pending permission request.
    pub async fn resolve_permission(
        &self,
        session_id: &str,
        request_id: String,
        decision: PermissionDecision,
    ) -> Result<(), String> {
        let map = self.sessions.lock().await;
        let Some(e) = map.get(session_id) else {
            return Err(format!(
                "session '{session_id}' is not hosted on this server"
            ));
        };
        let _ = e
            .req_tx
            .send(AgentRequest::PermissionReply {
                request_id,
                decision,
                parent_call_id: None,
            })
            .await;
        Ok(())
    }

    /// Control plane (ADR-0096): send a prompt to a hosted session as a new
    /// round (how a panel or web UI "starts a task" on an existing session).
    pub async fn send_prompt(&self, session_id: &str, text: String) -> Result<(), String> {
        let map = self.sessions.lock().await;
        let Some(e) = map.get(session_id) else {
            return Err(format!(
                "session '{session_id}' is not hosted on this server"
            ));
        };
        let _ = e
            .req_tx
            .send(AgentRequest::Prompt {
                text,
                images: Vec::new(),
                sent_at_ms: None,
            })
            .await;
        Ok(())
    }

    /// Control plane (ADR-0096): tear down a hosted session — cancel its
    /// driver, fire its SessionEnd hooks (a killed session is a
    /// session that ended), drop it from the registry, and tell monitors it
    /// is gone.
    ///
    /// `hook_budget` bounds each `fire_session_end` call. A user-configured
    /// SessionEnd hook runs an external process; a hung one must never pin
    /// the server's shutdown (or this verb) open. On timeout the remaining
    /// hook work is abandoned — the hook's side effects are best-effort by
    /// design — and the teardown continues.
    pub async fn kill_session(&self, session_id: &str) -> Result<(), String> {
        self.kill_session_with_hook_budget(session_id, DEFAULT_SESSION_END_HOOK_BUDGET)
            .await
    }

    /// Tear down a session whose driver task panicked (the "evict" leg of
    /// task supervision). Reuses the kill path's cleanup — entry removal,
    /// terminal `Exit` broadcast, bounded SessionEnd hooks,
    /// `SessionRemoved` — but is tolerant of racing callers (another client
    /// may have already killed the session). Uses a tighter hook budget:
    /// this runs from inside the crashed task, so a hanging SessionEnd hook
    /// must not pin the teardown.
    async fn evict_crashed_session(
        &self,
        session_id: &str,
        cancel: &CancellationToken,
        events: broadcast::Sender<AgentResponse>,
        agent_for_session_end: Option<Arc<Agent>>,
    ) {
        // Only act if this exact entry is still live: kill_session or a
        // concurrent crash-eviction may have removed it already. Comparing
        // the cancel token distinguishes "already gone" from "a newer
        // session under the same id".
        {
            let map = self.sessions.lock().await;
            if map.get(session_id).is_none() {
                tracing::debug!(
                    session = %session_id,
                    "crash eviction: entry already removed"
                );
                return;
            }
        }
        // Same cleanup sequence as kill_session_with_hook_budget, with the
        // tight crash budget (2s) — an external-process SessionEnd hook must
        // not pin a teardown that already represents a failure path.
        self.sessions.lock().await.remove(session_id);
        self.drop_session_mailbox(session_id).await;
        cancel.cancel();
        let _ = events.send(AgentResponse::Exit);
        if let Some(agent) = agent_for_session_end
            && tokio::time::timeout(std::time::Duration::from_secs(2), agent.fire_session_end())
                .await
                .is_err()
        {
            tracing::warn!(
                session = %session_id,
                "crash eviction: SessionEnd hook exceeded 2s; abandoning it"
            );
        }
        self.publish(MonitorEvent::SessionRemoved {
            session_id: session_id.to_string(),
        })
        .await;
    }

    /// Suspend a hosted session: tear it down **in memory** without ending
    /// it. Unlike [`Self::kill_session`], no terminal `Exit` is broadcast
    /// (there are no clients to receive it — suspension requires zero
    /// receivers) and SessionEnd hooks do **not** fire: the session is not
    /// over, merely parked. Its transcript is durable, so the next attach
    /// resolves through the standard lazy-resume path and rebuilds
    /// everything. Scheduled jobs live in the session store, so they resume
    /// with it.
    ///
    /// Returns `Ok(())` when the session was suspended, `Err` when it was
    /// already gone (a racing kill/suspend) or is not suspendable.
    pub async fn suspend_session(&self, session_id: &str) -> Result<(), String> {
        let removed = self.sessions.lock().await.remove(session_id);
        self.drop_session_mailbox(session_id).await;
        let Some(e) = removed else {
            return Err(format!(
                "session '{session_id}' is not hosted on this server"
            ));
        };
        // Cancel the driver (drops the run future). The req_tx senders in
        // clients' BoundSession clones die with the entry removal, so no
        // further requests queue into a drained channel.
        e.cancel.cancel();
        // Deliberately NOT sending AgentResponse::Exit and NOT firing
        // SessionEnd: suspension is invisible by design (no receivers exist
        // as a precondition) and the session continues on resume.
        self.publish(MonitorEvent::SessionRemoved {
            session_id: session_id.to_string(),
        })
        .await;
        tracing::info!(session = %session_id, "suspended idle hosted session");
        Ok(())
    }

    /// Control plane: park a hosted session on demand (the
    /// `suspend_session` verb). The same in-memory teardown as the idle
    /// reaper's [`Self::suspend_session`], but guarded for an explicit
    /// human/panel request: a session with an attached client or an active
    /// round (running / needs-approval / needs-input) is refused with a
    /// message naming what to do instead — detach or interrupt first. A
    /// never-persisted empty session is refused too: suspending it would
    /// silently discard it (there is no transcript to lazy-resume from),
    /// so killing is the honest verb for that case.
    pub async fn suspend_session_control(&self, session_id: &str) -> Result<(), String> {
        // Snapshot the probes under the lock; suspend outside it (never
        // hold the map lock across an await). `is_empty_unpersisted` needs
        // an await on the store, so the entry is cloned out and probed
        // lock-free.
        let entry = {
            let map = self.sessions.lock().await;
            let Some(e) = map.get(session_id) else {
                return Err(format!(
                    "session '{session_id}' is not hosted on this server"
                ));
            };
            Arc::clone(e)
        };
        if entry.events.receiver_count() > 0 {
            return Err(format!(
                "session '{session_id}' has {} attached client(s); detach before suspending",
                entry.events.receiver_count()
            ));
        }
        let status = entry.tracker.lock().await.row().status;
        if status.is_active() {
            return Err(format!(
                "session '{session_id}' is {} — interrupt it before suspending",
                status.as_str()
            ));
        }
        if entry.session.is_empty_unpersisted().await {
            return Err(format!(
                "session '{session_id}' has no content to keep — kill it instead"
            ));
        }
        self.suspend_session(session_id).await
    }

    /// Sweep for idle hosted sessions to suspend. A session is suspendable
    /// when (a) no client is attached (`receiver_count == 0`), (b) its
    /// monitor status is not active (not running / awaiting approval /
    /// awaiting input), and (c) it has had no tap activity for the TTL.
    /// Empty unpersisted sessions are left to the tighter empty-reaper
    /// above; this path exists for *real* sessions whose memory the server
    /// would otherwise hold forever.
    pub async fn suspend_idle_sessions(&self) -> Vec<String> {
        self.suspend_idle_sessions_with(IDLE_HOSTED_SESSION_TTL)
            .await
    }

    /// [`Self::suspend_idle_sessions`] with an explicit TTL (tests).
    pub async fn suspend_idle_sessions_with(&self, ttl: std::time::Duration) -> Vec<String> {
        // Snapshot under the lock; probe + suspend outside it (same pattern
        // as the empty reaper: never hold the map lock across awaits).
        let candidates: Vec<(String, Arc<HostedSession>)> = self
            .sessions
            .lock()
            .await
            .iter()
            .map(|(id, e)| (id.clone(), Arc::clone(e)))
            .collect();
        let mut suspended = Vec::new();
        for (id, entry) in candidates {
            if entry.events.receiver_count() != 0 {
                continue; // someone is attached — theirs to keep alive
            }
            // Active-looking status (running round, pending approval/input)
            // disqualifies even with no receivers: the work must be allowed
            // to finish or be interrupted explicitly.
            let status = entry.tracker.lock().await.row().status;
            if status.is_active() {
                continue;
            }
            // An armed `/schedule` job is work that *will* run unattended
            // Activity clock: starts at host time and is refreshed below
            // whenever the tap tick advanced since the last sweep, so "idle"
            // means "no folded events for the whole TTL".
            let tick_now = entry
                .activity_tick
                .load(std::sync::atomic::Ordering::Relaxed);
            let tick_seen = entry
                .last_seen_tick
                .load(std::sync::atomic::Ordering::Relaxed);
            if tick_now != tick_seen {
                // Events were folded since the last sweep: refresh the idle
                // clock and record the watermark. Not a suspension candidate
                // this round.
                *entry.last_activity.lock().await = std::time::Instant::now();
                entry
                    .last_seen_tick
                    .store(tick_now, std::sync::atomic::Ordering::Relaxed);
                continue;
            }
            if entry.last_activity.lock().await.elapsed() < ttl {
                continue;
            }
            // Never-persisted empties belong to the tighter reaper above.
            if entry.session.is_empty_unpersisted().await {
                continue;
            }
            if self.suspend_session(&id).await.is_ok() {
                suspended.push(id);
            }
        }
        suspended
    }

    /// [`Self::kill_session`] with an explicit hook budget, so server
    /// shutdown can size it against its own remaining grace (ADR-0101).
    pub async fn kill_session_with_hook_budget(
        &self,
        session_id: &str,
        hook_budget: std::time::Duration,
    ) -> Result<(), String> {
        let removed = self.sessions.lock().await.remove(session_id);
        self.drop_session_mailbox(session_id).await;
        // ADR-0234: a closed session's retained results are dropped with it —
        // no cross-session delivery, and a closed session is not revived to
        // receive one. Server-level jobs (`owner_session: None`) are untouched.
        let dropped = self.server_tasks.discard_pending_for_session(session_id);
        if dropped > 0 {
            tracing::debug!(
                session = %session_id,
                dropped,
                "registry: discarded retained background job outcomes on session close"
            );
        }
        let Some(e) = removed else {
            return Err(format!(
                "session '{session_id}' is not hosted on this server"
            ));
        };
        // Record the terminated-with-process stop (C11) *before* dropping the
        // driver future: the round task it kills here never runs its own tail,
        // so this durable record is the only trace the round ever stopped.
        // Best-effort — a persistence failure must not block teardown. The
        // monitor tracker is the registry-side view of "a round is live";
        // NeedsApproval/NeedsInput also count (parked work dies with the
        // driver too).
        let status = e.tracker.lock().await.row().status;
        if status.is_active() {
            let record = nuo_wire::RoundInterrupt {
                reason: nuo_wire::RoundInterruptReason::Terminated,
                at_ms: unix_epoch_ms(),
                round: Some(e.session.round_counter().await),
                detail: None,
            };
            if let Err(error) = e.session.record_round_interrupt(record).await {
                tracing::warn!(
                    session = %session_id,
                    %error,
                    "registry: could not record round interrupt on kill"
                );
            }
        }
        e.cancel.cancel();
        let _ = e.events.send(AgentResponse::Exit);
        // SessionEnd observers fire best-effort after the driver is cancelled;
        // the hook context (session id + cwd) does not depend on it. Bounded:
        // an external-process hook that hangs cannot pin the server open.
        if let Some(agent) = &e.agent_for_session_end
            && tokio::time::timeout(hook_budget, agent.fire_session_end())
                .await
                .is_err()
        {
            tracing::warn!(
                session = %session_id,
                timeout_secs = hook_budget.as_secs_f32(),
                "registry: SessionEnd hook exceeded its budget; abandoning it"
            );
        }
        self.publish(MonitorEvent::SessionRemoved {
            session_id: session_id.to_string(),
        })
        .await;
        Ok(())
    }

    /// Broadcast an agent response to every hosted session's event bus.
    pub async fn broadcast_all_sessions(&self, response: AgentResponse) {
        let map = self.sessions.lock().await;
        for session in map.values() {
            let _ = session.events.send(response.clone());
        }
    }

    /// Graceful server shutdown (ADR-0096): tear down every hosted session
    /// via [`Self::kill_session_with_hook_budget`], so each one's SessionEnd
    /// hooks fire before the process exits. `host::run` calls
    /// this after the listeners stop accepting and the connections drain
    /// (ADR-0101).
    ///
    /// Sessions are torn down **concurrently** and each hook is bounded by
    /// `hook_budget` individually: N sessions cost max(hook) — not the sum —
    /// so one slow hook cannot starve the others of their own budget.
    /// Best-effort per session: one failure does not skip the rest.
    /// ADR-0034 Level 1 (soft reload): re-read the configuration matrix into
    /// the server-wide snapshot and ask every hosted session to re-sync its
    /// MCP servers and skills — without dropping a single connection.
    pub async fn reload_runtime(&self) -> Result<(), String> {
        let config = nuo_persistence::config::Config::load();
        *self.shared_config.write().await = config;
        let sessions: Vec<Arc<HostedSession>> = {
            let guard = self.sessions.lock().await;
            guard.values().cloned().collect()
        };
        for session in sessions {
            let _ = session
                .req_tx
                .send(nuo_wire::AgentRequest::ReloadRuntime)
                .await;
        }
        Ok(())
    }

    pub async fn shutdown_all_sessions(&self) {
        self.shutdown_all_sessions_with_hook_budget(DEFAULT_SESSION_END_HOOK_BUDGET)
            .await;
    }

    pub async fn shutdown_all_sessions_with_hook_budget(&self, hook_budget: std::time::Duration) {
        let ids: Vec<String> = self.sessions.lock().await.keys().cloned().collect();
        let mut tears: Vec<std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>> =
            Vec::with_capacity(ids.len());
        for id in ids {
            let this = self.clone();
            tears.push(Box::pin(async move {
                if let Err(error) = this.kill_session_with_hook_budget(&id, hook_budget).await {
                    tracing::warn!(session = %id, %error, "registry: session teardown failed on shutdown");
                }
            }));
        }
        futures::future::join_all(tears).await;
    }

    /// One pass of the idle-empty reaper: remove every hosted session that is
    /// still `is_empty_unpersisted` (no user-facing content, never written to
    /// disk) **and** has been hosted longer than `IDLE_EMPTY_SESSION_TTL`
    /// **and** currently has no attached client. Returns the reclaimed ids.
    ///
    /// Only never-persisted sessions are eligible — a session that has real
    /// content (however brief) is user history and is never reaped. The
    /// `receiver_count() == 0` probe treats any live event subscriber as an
    /// attached client, so a session someone is actively watching is left
    /// alone even while still empty. This is the in-memory counterpart of the
    /// persistence layer's lazy-materialisation guard (ADR-0018): that guard
    /// keeps empty sessions off disk, this one keeps them from piling up in
    /// the server's memory when clients create-then-abandon them.
    pub async fn reap_idle_empty_sessions(&self) -> Vec<String> {
        self.reap_idle_empty_sessions_with(IDLE_EMPTY_SESSION_TTL)
            .await
    }

    /// TTL-parameterised core of [`Self::reap_idle_empty_sessions`], split out
    /// so tests can sweep immediately instead of waiting out the real TTL.
    #[doc(hidden)]
    pub async fn reap_idle_empty_sessions_with(&self, ttl: std::time::Duration) -> Vec<String> {
        // Snapshot candidates under the lock, then probe + kill outside it so
        // we never hold the map lock across an `await` on a session's state.
        let candidates: Vec<(String, Arc<HostedSession>)> = self
            .sessions
            .lock()
            .await
            .iter()
            .map(|(id, e)| (id.clone(), Arc::clone(e)))
            .collect();
        let mut reaped = Vec::new();
        for (id, entry) in candidates {
            let idle_long_enough = entry.created_at.elapsed() >= ttl;
            let no_clients = entry.events.receiver_count() == 0;
            if !idle_long_enough || !no_clients {
                continue;
            }
            if !entry.session.is_empty_unpersisted().await {
                continue;
            }
            // Guard against the id-key stability hazard: an in-session
            // `/session open` or `/new` may have repointed the store to a
            // *different*, now persisted session while the map key still holds
            // the original id.
            // Only kill when the store is still on the entry's original id —
            // otherwise the entry has been repurposed and must be left alone.
            if entry.session.id().await != id {
                continue;
            }
            if self.kill_session(&id).await.is_ok() {
                tracing::info!(session_id = %id, "reaped idle never-persisted session");
                reaped.push(id);
            }
        }
        reaped
    }

    /// Spawn the background idle-empty reaper, sweeping every
    /// `IDLE_REAPER_INTERVAL` until `cancel` fires. The server calls this
    /// once at startup; the task stops cleanly on shutdown.
    ///
    /// The same tick runs the low-frequency storage-maintenance pass
    /// ([`Self::spawn_storage_maintenance`]) at most once per day: blob
    /// garbage collection and usage-day retention. Both are whole-data-dir
    /// scans, far too expensive to run per session event.
    pub fn spawn_idle_reaper(self: &Arc<Self>, cancel: CancellationToken) {
        let registry = Arc::clone(self);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(IDLE_REAPER_INTERVAL);
            // `interval` fires immediately on creation; skip the first tick so
            // a just-started server does not reap sessions still being set up.
            tick.tick().await;
            let mut last_maintenance = std::time::Instant::now();
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => {
                        tracing::info!("idle-session reaper: shutdown");
                        break;
                    }
                    _ = tick.tick() => {
                        registry.reap_idle_empty_sessions().await;
                        registry.suspend_idle_sessions().await;
                        if last_maintenance.elapsed() >= STORAGE_MAINTENANCE_INTERVAL {
                            last_maintenance = std::time::Instant::now();
                            registry.spawn_storage_maintenance();
                        }
                    }
                }
            }
        });
    }

    /// One storage-maintenance pass: sweep unreferenced blobs and prune
    /// over-age usage day files. Both phases are synchronous directory scans
    /// proportional to the data dir's size, so the work runs on the blocking
    /// pool. Blob liveness is decided by the durable `blob_refs` ledger
    /// (ADR-0187), not by a filesystem scan.
    pub fn spawn_storage_maintenance(self: &Arc<Self>) {
        let dirs = nuo_persistence::paths::get();
        let blobs = nuo_persistence::blobs::BlobStore::new(dirs.blobs_dir());
        let usage = nuo_persistence::usage_stats::UsageStatsStore::new();
        tokio::task::spawn_blocking(move || {
            let handle = nuo_persistence::db::get_persistence_handle();
            // The blob sweep is a filesystem pass, so it runs here against a
            // reader snapshot instead of holding up the writer (ADR-0231);
            // the entry DELETE is a real mutation and goes through the actor.
            let (count, bytes) = match handle.reader().and_then(|r| r.live_blob_hashes()) {
                Ok(live) => blobs.retain_only(&live),
                Err(error) => {
                    tracing::warn!(%error, "blob garbage collection failed; skipping pass");
                    (0, 0)
                }
            };
            let entries = handle
                .collect_entry_garbage_blocking()
                .unwrap_or_else(|error| {
                    tracing::warn!(%error, "entry garbage collection failed; skipping pass");
                    0
                });
            let days = usage.prune_old_days();
            if count > 0 || entries > 0 || days > 0 {
                tracing::info!(
                    blobs = count,
                    bytes,
                    entries,
                    pruned_days = days,
                    "storage maintenance: reclaimed unreferenced data"
                );
            }
        });
    }

    pub async fn host(&self, entry: HostedSession) -> BoundSession {
        let id = entry.session.id().await;
        let b = BoundSession {
            workspace_root: entry.workspace_root.clone(),
            human_channel: entry.human_channel.clone(),
            session: entry.session.clone(),
            shared_confinement: entry.shared_confinement.clone(),
            req_tx: entry.req_tx.clone(),
            events: entry.events.clone(),
            sync_buffer: entry.sync_buffer.clone(),
            server_picker: self.latest_picker.clone(),
            command_catalog: entry.command_catalog.clone(),
            security: entry.security.clone(),
        };
        let tracker = entry.tracker.clone();
        self.publish(MonitorEvent::SessionAdded(tracker.lock().await.row()))
            .await;
        self.sessions.lock().await.insert(id, Arc::new(entry));
        b
    }
    async fn resolve_auto(
        &self,
        caller_project: &std::path::Path,
        declared: bool,
    ) -> ResolveOutcome {
        // Prefer the caller's own project's sessions; only when that project
        // has none do we consider the global set, and only a single global
        // session auto-binds (otherwise the client must choose).
        let map = self.sessions.lock().await;
        let mine: Vec<&Arc<HostedSession>> = map
            .values()
            .filter(|e| e.workspace_root.as_deref() == Some(caller_project))
            .collect();
        match mine.len() {
            1 => return ResolveOutcome::Welcome(self.bound_from(mine[0])),
            n if n > 1 => {
                return ResolveOutcome::Pick {
                    sessions: self.overview_subset(&mine).await,
                };
            }
            _ => {}
        }
        match map.len() {
            0 => {
                drop(map);
                self.create_session_outcome(
                    caller_project.to_path_buf(),
                    nuo_client::wire::SessionInitOptions::default(),
                )
                .await
            }
            1 => {
                // The one hosted session belongs to a *different* project
                // than the calling client declared. Auto-binding it would
                // silently attach this client to another project's session —
                // the "launched in project A, working in project B" trap —
                // so a declared-project client gets the picker instead (the
                // cross-project session is one explicit choice; a fresh
                // session in the caller's project is created by `New`).
                // A legacy client that declared no project keeps the
                // historical auto-bind: the server cannot distinguish a
                // cross-project attach from a same-project one.
                if declared {
                    return ResolveOutcome::Pick {
                        sessions: self.overview(&map).await,
                    };
                }
                if let Some(e) = map.values().next() {
                    ResolveOutcome::Welcome(self.bound_from(e))
                } else {
                    unreachable!("len==1")
                }
            }
            _ => ResolveOutcome::Pick {
                sessions: self.overview(&map).await,
            },
        }
    }
    async fn resolve_id(&self, id: &str, _caller_project: &std::path::Path) -> ResolveOutcome {
        {
            let map = self.sessions.lock().await;
            if let Some(e) = map.get(id) {
                return ResolveOutcome::Welcome(self.bound_from(e));
            }
        }
        if self.params.is_none() {
            return ResolveOutcome::Error(format!("session '{id}' is not hosted on this server"));
        }
        // Lazy resume resolves the session's own durable binding (scope +
        // workspace) so an unbound session is reachable from any caller
        // (ADR-0219/0220), not only the caller's workspace.
        let Some(binding) = lookup_session_workspace(id) else {
            return ResolveOutcome::Error(format!("unknown session id '{id}'"));
        };
        match self
            .assemble_hosted(
                crate::startup::SessionStart::Resume(id.to_string()),
                binding,
                nuo_client::wire::SessionInitOptions::default(),
            )
            .await
        {
            Ok(b) => ResolveOutcome::Welcome(b),
            Err(AssembleErr::NoHost) => {
                ResolveOutcome::Error(format!("session '{id}' is not hosted on this server"))
            }
            Err(AssembleErr::AssembleFailed(e)) => {
                ResolveOutcome::Error(format!("could not resume session {id}: {e}"))
            }
        }
    }

    async fn assemble_hosted(
        &self,
        startup: crate::startup::SessionStart,
        binding: SessionBinding,
        init_options: nuo_client::wire::SessionInitOptions,
    ) -> Result<BoundSession, AssembleErr> {
        let HostParams {
            identity,
            preset,
            ui,
        } = self.params.as_ref().ok_or(AssembleErr::NoHost)?.clone();
        // ADR-0220/0226: a persona overrides the server's fixed identity and
        // preset. An explicit selection (`init_options.persona`) also applies
        // the persona's workspace policy; a resume uses the stored workspace
        // binding unchanged. `workspace = None` is the unbound set.
        let mut identity = identity;
        let mut preset = preset;
        let mut workspace = binding.workspace;
        let role_selected = init_options.role.clone();
        let role_id = role_selected.clone().or(binding.role);

        // `--resume` (ADR-0250): pick the most recent matching session instead
        // of a fresh one, partitioned truthfully by Workspace or Role.
        let mut startup = startup;
        if init_options.resume && matches!(startup, crate::startup::SessionStart::Fresh) {
            let partition = nuo_wire::SessionPartition::from_binding(
                workspace.as_ref(),
                role_id.as_deref(),
            );
            if let Some(id) = lookup_latest_session_in_partition(&partition) {
                startup = crate::startup::SessionStart::Resume(id);
            }
        }

        // Hermetic session manifest restoration (ADR-0245):
        // If resuming an existing session, restore directly from its immutable birth manifest.
        // This guarantees 100% byte-identical prompt cache and zero crash if external roles.toml was modified/deleted.
        let resumed_session_id = match &startup {
            crate::startup::SessionStart::Resume(id) => Some(id.as_str()),
            _ => None,
        };
        let manifest = resumed_session_id.and_then(lookup_session_manifest);

        if let Some(manifest) = manifest {
            let role_identity = manifest.identity;
            let mut role = nuo_wire::AgentRoleProfile::with_identity(
                manifest.role_id.clone(),
                role_identity.clone(),
            );
            role.tools = nuo_wire::ToolSelection::from_allowlist(&manifest.tools);
            role.admit_mcp = manifest.admit_mcp.clone();
            identity = role_identity;
            preset = role;
        } else if let Some(role_id) = role_id.as_deref() {
            let ws_root = workspace.as_ref().map(|b| b.root.as_path());
            let roles_cfg = nuo_persistence::roles::RolesConfig::load_for_workspace(ws_root);
            if let Some(role_entry) = roles_cfg.get(role_id) {
                if role_selected.is_some() {
                    workspace = match role_entry.resolved_workspace() {
                        nuo_persistence::roles::RoleWorkspace::None => None,
                        nuo_persistence::roles::RoleWorkspace::Inherit => workspace,
                        nuo_persistence::roles::RoleWorkspace::Fixed(root) => {
                            Some(nuo_wire::WorkspaceBinding::new(root))
                        }
                    };
                }
                let role_identity = role_entry.identity();
                let mut role = nuo_wire::AgentRoleProfile::with_identity(
                    role_id.to_string(),
                    role_identity.clone(),
                );
                role.tools = nuo_wire::ToolSelection::from_allowlist(&role_entry.tools);
                role.admit_mcp = role_entry.admit_mcp.clone();
                identity = role_identity;
                preset = role;
            } else if let Some(builtin) = nuo_wire::MainAgentRole::parse(role_id) {
                if role_selected.is_some() && !builtin.requires_workspace() {
                    workspace = None;
                }
                let role = nuo_wire::AgentRoleProfile::from_role(builtin, &identity);
                identity = role.identity.clone();
                preset = role;
            } else {
                return Err(AssembleErr::AssembleFailed(Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("unknown role '{role_id}'"),
                ))));
            }
        }
        let workspace_root = workspace.as_ref().map(|binding| binding.root.clone());
        let effective_confined = if let Some(builtin) = role_id
            .as_deref()
            .and_then(nuo_wire::MainAgentRole::parse)
        {
            if !builtin.requires_workspace() {
                builtin.default_confined() && init_options.confined
            } else {
                init_options.confined
            }
        } else {
            init_options.confined
        };
        // The session's lifetime token (ADR-0125): shared by the driver
        // select below and the background `/schedule` scheduler inside the
        // assemble, so one cancel stops the harness *and* its tick loop.
        // Created before the assemble because the scheduler is spawned
        // during it and receives the token as a spawn parameter.
        let cancel = CancellationToken::new();
        // ADR-0141: per-session channel accounting — attach/detach on the
        // WS layer keeps this fresh; the agent reads it live.
        let human_channel = Arc::new(nuo_wire::human_request::HumanChannelAccountant::new());
        let boot = bootstrap::assemble(BootstrapParams {
            identity,
            preset,
            ui,
            startup,
            project_root: workspace_root.clone(),
            role: role_id.clone(),
            unattended: init_options.unattended,
            confined: effective_confined,
            human_channel: Some(Arc::clone(&human_channel)),
            teardown_token: Some(cancel.clone()),
            shared_config: Some(self.shared_config.clone()),
            shared_provider_usage: Some(self.shared_provider_usage.clone()),
        })
        .await
        .map_err(AssembleErr::AssembleFailed)?;
        let session = boot.session.clone();
        // ADR-0167 mesh registration: the session's master joins the server
        // mesh at `session/<id>`, parented to the Hypervisor. Failure is not
        // session-fatal (mesh delivery is fail-open observability), so a
        // duplicate registration simply replaces the mailbox.
        let session_id = session.id().await;
        let mailbox_handle = self.register_session_mailbox(&session_id).await;
        // ADR-0008 step 8 / ADR-0009 §3: wire ACP collaboration tools into the
        // live agent through its dynamic-tool sink (the same seam MCP uses).
        // Feature-gated (ADR-0005 §5) so lean single-agent builds omit the
        // whole multi-agent surface. Published after the mailbox exists so the
        // tools can address peers and channels on the server fabric.
        #[cfg(feature = "collaboration")]
        {
            let addr = acp::AgentAddress::parse(&format!("agent://local/session/{session_id}"))
                .expect("valid session address");
            let ctx = Arc::new(acp::AcpToolContext::for_fabric(
                addr,
                self.fabric.clone(),
                mailbox_handle,
                std::time::Duration::from_secs(120),
            ));
            let acp_tools = acp::create_acp_tools(ctx);
            boot.agent.dynamic_tool_sink().replace("acp", acp_tools);
        }
        #[cfg(not(feature = "collaboration"))]
        {
            let _ = mailbox_handle;
        }
        let req_tx = boot.req_tx.clone();
        let command_catalog = boot.command_catalog.clone();
        let (events_tx, _) = broadcast::channel::<AgentResponse>(1024);
        let tap = events_tx.clone();
        let mut rr = boot.resp_rx;

        // Monitor wiring (ADR-0093): seed the tracker from the session header,
        // then fold every broadcast response into the row and publish diffs.
        // A lazily-resumed session whose round was cut off mid-flight will
        // re-drive from the first `TurnStarted` the driver emits, so the
        // tracker needs no transcript-tail heuristic — it starts Idle and
        // catches up from the live stream.
        let base = overview_of(&session, true).await;
        let tracker = Arc::new(Mutex::new(MonitorTracker::bootstrap(
            base_row(base, workspace_root.as_deref()),
            SessionStatus::Idle,
        )));
        let tracker_for_tap = tracker.clone();
        let monitor_bus = self.monitor.clone();
        let sync_buffer = Arc::new(Mutex::new(AttachSyncBuffer::new()));
        let sync_buffer_for_tap = sync_buffer.clone();
        // Cross-session picker fan-out (see `broadcast_picker_to_other_sessions`):
        // whenever this session's driver rebuilds the model-picker snapshot
        // (switch, favorite toggle, connection add/delete, startup), every
        // other hosted session's bus hears it too, and the server-level
        // latest snapshot is refreshed for future attachers.
        let cross_for_tap = self.clone();
        let origin_for_tap = session.id().await;
        // Idle-suspension clock: bumped once per folded event (cheap atomic,
        // no mutex) so the reaper can distinguish "alive but quiet because
        // idle" from "hosted but forgotten".
        let activity_tick = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let tick_for_tap = activity_tick.clone();
        tokio::spawn(async move {
            use futures::FutureExt;
            while let Some(r) = rr.recv().await {
                // Isolate per event (the "isolate" supervision policy): a
                // poison response costs one dropped frame, not the session's
                // entire observability path. Before this, a panic anywhere in
                // the fold killed the tap task; the driver kept running and
                // burning tokens while every subscriber's stream froze.
                let fold = std::panic::AssertUnwindSafe(async {
                    {
                        tick_for_tap.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let mut guard = tracker_for_tap.lock().await;
                        guard.observe(&r);
                        let row = guard.row();
                        drop(guard);
                        let _ = monitor_bus.send(MonitorEvent::SessionUpdated(row));
                    }
                    // Buffer the attach-sync events before broadcasting so a
                    // client that attaches later still hydrates. Latest-value
                    // semantics retain startup keys/picker state across turns.
                    if is_attach_sync_event(&r) {
                        let mut buf = sync_buffer_for_tap.lock().await;
                        buf.observe(&r);
                    }
                    if matches!(r, AgentResponse::ProviderPicker(_)) {
                        cross_for_tap
                            .broadcast_picker_to_other_sessions(&origin_for_tap, r.clone())
                            .await;
                    }
                    let _ = tap.send(r);
                });
                if let Err(payload) = fold.catch_unwind().await {
                    tracing::error!(
                        panic = %crate::task_fault_tolerance::panic_detail(payload),
                        "monitor tap panicked folding an event; dropped one event"
                    );
                }
            }
        });
        let cd = cancel.clone();
        let driver = boot.driver;
        // Supervised driver spawn (the "evict" policy). The select arm keeps
        // kill_session's cancel semantics: cancellation drops the driver
        // future. The catch_unwind wrapper adds the panic leg — before it, a
        // driver panic left a zombie entry: nobody drained `req_tx` (an
        // unbounded channel, so clients kept queueing into memory), the
        // control plane's `let _ = req_tx.send(...)` silently succeeded, and
        // the only recovery was restarting the server.
        //
        // The registry clone is cheap (every field is an Arc), and moving the
        // spawn after the map insert is required so eviction can find the
        // entry. `id` is cloned below for the same reason.
        let crash_registry = self.clone();
        let crash_id = session.id().await;
        let crash_events = events_tx.clone();
        let cancel_for_crash = cancel.clone();
        let agent_for_crash = Some(boot.agent_for_session_end.clone());
        let (driver_done_tx, mut driver_done_rx) = mpsc::channel::<()>(1);
        tokio::spawn(async move {
            use futures::FutureExt;
            let run = async {
                tokio::select! {
                    _ = cd.cancelled() => (),
                    _ = driver.run() => (),
                }
            };
            let outcome = std::panic::AssertUnwindSafe(run).catch_unwind().await;
            let _ = driver_done_tx.send(()).await;
            if let Err(payload) = outcome {
                let detail = crate::task_fault_tolerance::panic_detail(payload);
                tracing::error!(
                    session = %crash_id,
                    panic = %detail,
                    "session driver panicked; evicting the hosted session"
                );

                // Visible failure instead of silence: attached clients learn
                // why before the Exit marker, then the entry is torn down
                // through the standard path (SessionRemoved,
                // SessionEnd hooks bounded at 2s). The session stays on disk,
                // so the next attach lazy-resumes it cleanly.
                let _ = crash_events.send(AgentResponse::Error(format!(
                    "internal error: session driver panicked: {detail}"
                )));
                crash_registry
                    .evict_crashed_session(
                        &crash_id,
                        &cancel_for_crash,
                        crash_events.clone(),
                        agent_for_crash,
                    )
                    .await;
            }
        });
        let id = session.id().await;
        let bound = BoundSession {
            workspace_root: workspace_root.clone(),
            human_channel: human_channel.clone(),
            session: session.clone(),
            shared_confinement: boot.shared_confinement.clone(),
            req_tx: req_tx.clone(),
            events: events_tx.clone(),
            sync_buffer: sync_buffer.clone(),
            server_picker: self.latest_picker.clone(),
            command_catalog: command_catalog.clone(),
            security: boot.security.clone(),
        };
        let hosted = Arc::new(HostedSession {
            workspace_root,
            human_channel,
            security: boot.security.clone(),
            session,
            shared_confinement: boot.shared_confinement,
            req_tx,
            events: events_tx,
            cancel,
            tracker,
            sync_buffer,
            command_catalog,
            created_at: std::time::Instant::now(),
            last_activity: Mutex::new(std::time::Instant::now()),
            last_seen_tick: std::sync::atomic::AtomicU64::new(0),
            activity_tick: activity_tick.clone(),
            agent_for_session_end: Some(boot.agent_for_session_end),
        });
        self.publish(MonitorEvent::SessionAdded(
            hosted.tracker.lock().await.row(),
        ))
        .await;
        self.sessions.lock().await.insert(id, hosted);
        // Keep the panic-supervision wrapper alive until the driver settles;
        // dropping this receiver early would let the send above fail
        // (harmlessly) but the wrapper task would already be parked on it.
        let _ = &mut driver_done_rx;
        Ok(bound)
    }
    fn bound_from(&self, e: &Arc<HostedSession>) -> BoundSession {
        BoundSession {
            workspace_root: e.workspace_root.clone(),
            human_channel: e.human_channel.clone(),
            session: e.session.clone(),
            shared_confinement: e.shared_confinement.clone(),
            req_tx: e.req_tx.clone(),
            events: e.events.clone(),
            sync_buffer: e.sync_buffer.clone(),
            server_picker: self.latest_picker.clone(),
            command_catalog: e.command_catalog.clone(),
            security: e.security.clone(),
        }
    }

    /// Number of currently hosted sessions (the idle-exit probe,
    /// ADR-0100 rule 3).
    pub async fn session_count(&self) -> usize {
        self.sessions.lock().await.len()
    }

    /// Subscribe to the host-level monitor topic (ADR-0093 §4).
    pub fn subscribe_monitor(&self) -> broadcast::Receiver<MonitorEvent> {
        self.monitor.subscribe()
    }

    /// The current monitor snapshot: every hosted session's row, newest
    /// activity first, honouring the client's `include_idle` filter.
    #[allow(clippy::unwrap_used)] // A poisoned snapshot mutex is an unrecoverable registry invariant.
    pub async fn monitor_snapshot(&self, action: MonitorAction) -> MonitorSnapshot {
        let mut sessions = Vec::new();
        {
            let map = self.sessions.lock().await;
            for hosted in map.values() {
                let row = hosted.tracker.lock().await.row();
                if action.include_idle || row.status != SessionStatus::Idle {
                    sessions.push(row);
                }
            }
        }
        sessions.sort_by_key(|row| std::cmp::Reverse(row.updated_at));
        let tasks = self
            .server_task_rows
            .lock()
            .unwrap()
            .values()
            .cloned()
            .collect();
        let meta = self.meta.lock().await.clone();
        MonitorSnapshot {
            project_root: meta.project_root.unwrap_or_default(),
            server_started_at: meta.started_at,
            sessions,
            tasks,
            persistence_health: self.persistence_health.lock().unwrap().clone(),
        }
    }

    /// Publish a host-level diff. Best-effort: with no subscribers the
    /// broadcast send fails, which is fine — a fresh subscriber always takes
    /// a snapshot first (ADR-0093 §4).
    async fn publish(&self, event: MonitorEvent) {
        let _ = self.monitor.send(event);
    }

    /// Publish a host-level (server-scope) event on the monitor bus
    /// (ADR-0101): the run loop announces `ServerDraining` when the graceful
    /// drain begins so every watch client learns the server is going away
    /// before its connection closes. Public and synchronous because the bus
    /// is a broadcast sender — sending never blocks on subscribers.
    pub fn publish_host_event(&self, event: MonitorEvent) {
        let _ = self.monitor.send(event);
    }

    /// Test-only hook: integration tests construct `HostedSession` by hand
    /// (no driver), so they emulate the registry's broadcast-tap by folding
    /// responses into the tracker themselves and publishing the diff through
    /// this narrow seam. Not part of the production surface. Best-effort:
    /// with no subscribers the send is dropped, exactly like production.
    #[doc(hidden)]
    pub fn publish_for_test(&self, event: MonitorEvent) {
        let _ = self.monitor.send(event);
    }

    async fn overview(&self, map: &HashMap<String, Arc<HostedSession>>) -> Vec<SessionOverview> {
        let hosted: Vec<&Arc<HostedSession>> = map.values().collect();
        self.overview_subset(&hosted).await
    }
    async fn overview_subset(&self, entries: &[&Arc<HostedSession>]) -> Vec<SessionOverview> {
        let mut out = Vec::new();
        for e in entries {
            out.push(overview_of(&e.session, true).await);
        }
        out.sort_by_key(|i| std::cmp::Reverse(i.updated_at));
        out
    }
}
enum AssembleErr {
    NoHost,
    AssembleFailed(Box<dyn std::error::Error>),
}

impl std::fmt::Debug for AssembleErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoHost => f.write_str("no host parameters"),
            Self::AssembleFailed(e) => write!(f, "assemble failed: {e}"),
        }
    }
}

/// Project the picker's cheap header row into the monitor base row; every
/// status/accounting field starts at its zero value and is folded from the
/// live event stream by the [`MonitorTracker`]. `project_root` rides along
/// from the registry's two-level index so clients can name the workspace
/// without a second lookup.
fn base_row(
    overview: SessionOverview,
    workspace_root: Option<&std::path::Path>,
) -> MonitoredSession {
    MonitoredSession {
        id: overview.id,
        overview: overview.overview,
        created_at: overview.created_at,
        updated_at: overview.updated_at,
        message_count: overview.message_count,
        hosting: SessionHosting::Hosted,
        status: SessionStatus::Idle,
        round: 0,
        turn: None,
        output_tokens: 0,
        elapsed_ms: 0,
        current_tool: None,
        activity: None,
        context_tokens: None,
        note: None,
        project_root: workspace_root
            .map(|root| root.display().to_string())
            .unwrap_or_else(|| "workspace-free".to_string()),
        // Lineage rides from the overview (ADR-0103 fork surfacing).
        parent_id: overview.parent_id,
        fork_kind: overview.fork_kind,
        digest: overview.digest,
    }
}

#[allow(clippy::collapsible_if)]
async fn overview_of(session: &SessionStore, active: bool) -> SessionOverview {
    let id = session.id().await;
    if let Ok(items) = session.list().await {
        if let Some(item) = items.into_iter().find(|i| i.id == id) {
            return SessionOverview {
                id: item.id,
                overview: item.overview,
                created_at: item.created_at,
                updated_at: item.updated_at,
                message_count: item.message_count,
                active,
                parent_id: item.parent_id,
                fork_kind: item.fork_kind,
                digest: item.digest,
            };
        }
    }
    let mc = session.full_transcript().await.len();
    let digest = session.digest().await.0;
    SessionOverview {
        id,
        overview: String::new(),
        created_at: 0,
        updated_at: 0,
        message_count: mc,
        active,
        // Not on disk yet (never persisted or empty): no lineage to report.
        parent_id: None,
        fork_kind: nuo_wire::SessionForkKind::Trunk,
        digest,
    }
}
fn lookup_latest_session_in_partition(
    partition: &nuo_wire::SessionPartition,
) -> Option<String> {
    nuo_persistence::db::get_persistence_handle()
        .reader()
        .ok()?
        .latest_session_in_partition(partition)
        .ok()
        .flatten()
}

/// Resolve a session's durable binding (scope + workspace) by exact id,
/// independent of the caller's workspace (ADR-0219/0220).
fn lookup_session_workspace(id: &str) -> Option<SessionBinding> {
    let (workspace, role) = nuo_persistence::db::get_persistence_handle()
        .reader()
        .ok()?
        .lookup_session_workspace(id)
        .ok()
        .flatten()?;
    Some(SessionBinding { workspace, role })
}

fn lookup_session_manifest(id: &str) -> Option<nuo_wire::SessionRoleManifest> {
    nuo_persistence::db::get_persistence_handle()
        .reader()
        .ok()?
        .lookup_session_manifest(id)
        .ok()
        .flatten()
}

#[cfg(test)]
mod acp_fabric_tests {
    use super::*;

    #[tokio::test]
    async fn session_mailbox_registers_and_teardown_unregisters() {
        let registry = SessionRegistry::prehost_only();

        assert!(registry.hypervisor().await.is_none());

        registry.register_session_mailbox("s-test").await;
        assert!(registry.hypervisor().await.is_some());

        let target_addr = acp::AgentAddress::parse("agent://local/session/s-test").unwrap();
        assert!(registry.fabric().manifest_of(&target_addr).await.is_some());

        registry.drop_session_mailbox("s-test").await;
        assert!(registry.fabric().manifest_of(&target_addr).await.is_none());
    }

    #[tokio::test]
    async fn instruction_routing_is_fail_closed_after_teardown() {
        let registry = SessionRegistry::prehost_only();
        registry.register_session_mailbox("s-live").await;

        let from = acp::AgentAddress::parse("agent://local/hypervisor").unwrap();
        let to = acp::AgentAddress::parse("agent://local/session/s-live").unwrap();
        let envelope = acp::AgentEnvelope::new(
            from.clone(),
            to.clone(),
            acp::MessageIntent::steer("align the retry loop", acp::SteerAction::Note),
        );

        assert!(registry.fabric().dispatch(envelope).await.is_ok());

        registry.drop_session_mailbox("s-live").await;
        let envelope = acp::AgentEnvelope::new(
            from,
            to,
            acp::MessageIntent::steer("nobody home", acp::SteerAction::Note),
        );
        assert!(registry.fabric().dispatch(envelope).await.is_err());
    }
}
