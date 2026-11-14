use super::*;

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
pub struct HostedThread {
    /// The workspace root, when one is bound (a workspace-free
    /// persona has none).
    pub workspace_root: Option<PathBuf>,
    /// Channel accounting shared with the assembled agent.
    pub human_channel: Arc<nuo_protocol::human_request::HumanChannelAccountant>,
    pub session: Arc<ThreadSession>,
    pub req_tx: mpsc::Sender<AgentRequest>,
    pub events: broadcast::Sender<AgentResponse>,
    pub cancel: CancellationToken,
    /// Released once the driver task has fully unwound (normal exit or
    /// panic-eviction). Purge waits on it before deleting durable state.
    pub driver_done: Arc<tokio::sync::Semaphore>,
    pub shared_confinement: nuo_protocol::SharedConfinement,
    /// The panel-facing tracker folding this session's event stream
    /// Owned here so the broadcast-tap task can fold events in
    /// and the registry can read rows out for snapshots.
    pub tracker: Arc<Mutex<MonitorTracker>>,
    /// Durable workspace trust store for the project at
    /// [`HostedThread::workspace_root`]. Reachable through
    /// [`AttachHandle`] so the WS attach path can push the trust decision to a
    /// client when the workspace contributions are unreviewed
    /// (`WorkspaceTrustState::needs_review`).
    pub security: Arc<nuo_persistence::workspace_security::WorkspaceSecurityStore>,

    /// Attach-time state-sync buffer: the startup events an attaching client
    /// cannot reconstruct (active provider/model, picker snapshot, key
    /// readiness, harness state). Filled by the broadcast-tap; snapshotted into
    /// each attaching client so it hydrates immediately.
    pub sync_buffer: Arc<Mutex<AttachSyncBuffer>>,
    /// Backend-owned slash-command vocabulary for every attached frontend.
    pub command_catalog: nuo_tool::CommandCatalog,
    /// Provider/model the session boots into, resolved during assembly. Used
    /// as the snapshot's fallback when the session has no durable `/models`
    /// pin yet (ADR-0133 hydration).
    pub initial_provider: String,
    pub initial_model: String,
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

/// A claimed session whose local harness is still being assembled.
///
/// `[INV-ATTACH-03]` (ADR-0133): this exists so `Wire::Welcome` can be sent
/// before any store load, harness construction, skill discovery, hook, or
/// credential work. The client-facing channels are created here; the build
/// task fills them in.
pub struct PendingSession {
    pub id: String,
    pub workspace_root: Option<PathBuf>,
    /// Canonical staffing role this session was claimed under. The monitor
    /// row carries it so every role-scoped listing can filter without opening
    /// the store.
    pub role: String,
    pub events: broadcast::Sender<AgentResponse>,
    pub req_tx: mpsc::Sender<AgentRequest>,
    /// Current readiness phase. A `watch` channel (not a bare cell) so a
    /// control-plane caller can await the transition without polling.
    pub phase: Arc<tokio::sync::watch::Sender<nuo_protocol::ThreadPhase>>,
    pub cancel: CancellationToken,
    /// Released once the build task has fully unwound. A purge waits on this
    /// before deleting a target so a late promotion cannot resurrect it.
    pub done: Arc<tokio::sync::Semaphore>,
    pub human_channel: Arc<nuo_protocol::human_request::HumanChannelAccountant>,
    pub sync_buffer: Arc<Mutex<AttachSyncBuffer>>,
    /// Set once, immediately before `Ready` is emitted, so a connection that
    /// joins in the promotion window still hydrates.
    pub latest_snapshot: Arc<Mutex<Option<nuo_protocol::ThreadSnapshot>>>,
    pub started_at: std::time::Instant,
}

/// What an attach resolved to: a building session or a fully hosted one.
///
/// Both variants expose the same minimal attach surface (identity, event bus,
/// request channel, human-channel accountant, phase), which is all
/// `handle_wire_stream` needs now that hydration happens over the snapshot.
#[derive(Clone)]
pub enum AttachHandle {
    Building(Arc<PendingSession>),
    Hosted(Arc<HostedThread>),
}

impl AttachHandle {
    pub async fn session_id(&self) -> String {
        match self {
            Self::Building(p) => p.id.clone(),
            Self::Hosted(h) => h.session.id().await,
        }
    }

    pub fn events(&self) -> broadcast::Sender<AgentResponse> {
        match self {
            Self::Building(p) => p.events.clone(),
            Self::Hosted(h) => h.events.clone(),
        }
    }

    pub fn req_tx(&self) -> mpsc::Sender<AgentRequest> {
        match self {
            Self::Building(p) => p.req_tx.clone(),
            Self::Hosted(h) => h.req_tx.clone(),
        }
    }

    pub fn human_channel(&self) -> Arc<nuo_protocol::human_request::HumanChannelAccountant> {
        match self {
            Self::Building(p) => p.human_channel.clone(),
            Self::Hosted(h) => h.human_channel.clone(),
        }
    }

    pub fn workspace_root(&self) -> Option<&std::path::Path> {
        match self {
            Self::Building(p) => p.workspace_root.as_deref(),
            Self::Hosted(h) => h.workspace_root.as_deref(),
        }
    }

    pub fn phase(&self) -> nuo_protocol::ThreadPhase {
        match self {
            Self::Building(p) => p.phase.borrow().clone(),
            Self::Hosted(_) => nuo_protocol::ThreadPhase::Ready,
        }
    }

    /// The authoritative hydration frame, when it exists yet.
    ///
    /// A hosted session builds it on demand (local reads only, after Welcome);
    /// a building session returns the snapshot cached at promotion, if any.
    pub async fn snapshot(&self) -> Option<nuo_protocol::ThreadSnapshot> {
        match self {
            Self::Building(p) => p.latest_snapshot.lock().await.clone(),
            Self::Hosted(h) => Some(crate::transport::serve::build_full_session_snapshot(h).await),
        }
    }
}

/// A resolved session binding: an optional workspace plus the staffing role.
#[derive(Clone)]
pub struct SessionBinding {
    pub workspace: Option<nuo_protocol::WorkspaceBinding>,
    pub role: Option<String>,
}

impl SessionBinding {
    pub fn workspace(root: PathBuf) -> Self {
        Self {
            workspace: Some(nuo_protocol::WorkspaceBinding::new(root)),
            role: None,
        }
    }

    pub fn workspace_root(&self) -> Option<PathBuf> {
        self.workspace.as_ref().map(|binding| binding.root.clone())
    }
}
pub enum ResolveOutcome {
    Welcome(AttachHandle),
    Pick {
        sessions: Vec<ThreadOverview>,
    },
    /// A refusal the client is expected to branch on (ADR-0123
    /// `[INV-ATTACH-01]`).
    ///
    /// The taxonomy travels with the message so the decision site never has to
    /// sniff prose: `code` is `Some(...)` only when the server can prove which
    /// recovery is legitimate. `None` means "no specific taxonomy" and a client
    /// MUST treat it as unrecoverable rather than invent a recovery it cannot
    /// justify.
    Refused {
        message: String,
        code: Option<&'static str>,
    },
}

impl ResolveOutcome {
    /// A refusal with a machine-readable taxonomy.
    pub fn refused(code: &'static str, message: impl Into<String>) -> Self {
        Self::Refused {
            message: message.into(),
            code: Some(code),
        }
    }

    /// A refusal with no specific taxonomy (unrecoverable for the client).
    pub fn refused_untaxed(message: impl Into<String>) -> Self {
        Self::Refused {
            message: message.into(),
            code: None,
        }
    }
}

/// The host-level observability topic: one broadcast channel
/// carrying [`MonitorEvent`] diffs for every hosted session. The registry is
/// the only publisher; any number of monitor clients subscribe. A fresh
/// subscriber is expected to first take a [`ThreadRegistry::monitor_snapshot`]
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
pub struct ThreadRegistry {
    pub(super) params: Option<HostParams>,
    pub(super) sessions: Arc<Mutex<HashMap<String, Arc<HostedThread>>>>,
    /// Sessions whose identity is claimed but whose harness is still being
    /// assembled (ADR-0133). Kept separate from `sessions` so every existing
    /// lifecycle/monitor path keeps operating on fully hosted entries only;
    /// `session_count`/`has_active_work` include this map so a building
    /// session is never mistaken for an idle server.
    pub(super) building: Arc<Mutex<HashMap<String, Arc<PendingSession>>>>,
    pub(super) monitor: MonitorBus,
    pub(super) meta: Arc<Mutex<MonitorMeta>>,
    /// Server-level task fabric (D6): rehosted services and other
    /// owner-less tasks live here, published onto the monitor bus as
    /// `TaskUpdated`/`TaskRemoved` diffs and folded into monitor snapshots.
    pub(super) server_tasks: Arc<crate::runtime::background_jobs::BackgroundJobManager>,
    /// The live server-task rows folded for snapshots (id → row).
    pub(super) server_task_rows:
        Arc<std::sync::Mutex<HashMap<String, nuo_protocol::MonitoredTask>>>,
    /// Latest durability-health state (D4): folded from the
    /// persistence supervisor's transitions for monitor snapshots and
    /// published as `PersistenceHealth` diffs. `None` = never degraded.
    pub(super) persistence_health:
        Arc<std::sync::Mutex<Option<nuo_protocol::monitor::PersistenceHealth>>>,
    /// Server-level latest picker snapshot (cross-session model ordering).
    /// The picker's recency data lives in the shared SQLite `ConnectionUsage`,
    /// so a snapshot built by any one session's driver is global truth — but
    /// the driver only pushes it onto its *own* bus. This cache (shared by
    /// every `BoundSession.server_picker`) lets a freshly attached client of
    /// any session hydrate the current global ordering instead of the stale
    /// one captured at that session's last local switch.
    pub(super) latest_picker: Arc<Mutex<Option<AgentResponse>>>,
    /// Authoritative runtime configuration shared across all hosted sessions.
    pub(super) shared_config: crate::SharedConfig,
    /// Authoritative model recency telemetry shared across all hosted sessions.
    pub(super) shared_provider_usage: crate::SharedConnectionUsage,
    /// The server's Archivist conversational service: lazily
    /// consulted by the `AskArchivist` control verb; one agent, borrowed
    /// provider per round.
    pub(super) archivist: Arc<crate::runtime::archivist_service::ArchivistService>,
    /// The server's agent communication fabric (ACP). The Hypervisor
    /// and every hosted session register their agent endpoints here.
    pub(super) fabric: acp::Fabric,
    /// The session mailboxes' parked handles so teardown can drop them
    /// deterministically.
    pub(super) session_mailboxes: Arc<Mutex<HashMap<String, acp::Mailbox>>>,
    /// The Hypervisor station, constructed lazily on the first
    /// hosted-session assemble.
    pub(super) hypervisor: Arc<Mutex<Option<Arc<crate::runtime::hypervisor::Hypervisor>>>>,
    /// Serializes durable purge against new session claims. A purge holds the
    /// write side for its whole run, so no attach can lazy-resume a thread
    /// between the `is_hosted` probe and the row delete.
    pub(super) purge_gate: Arc<tokio::sync::RwLock<()>>,
}

/// How long a never-persisted (empty) hosted session may sit idle before the
/// reaper reclaims it. Five minutes is comfortably longer than any legitimate
/// create→attach→first-prompt gap, so an empty session a user is about to
/// type into is never swept from under them.
pub(super) const IDLE_EMPTY_SESSION_TTL: std::time::Duration =
    std::time::Duration::from_secs(5 * 60);

/// How often the storage-maintenance pass (blob GC + usage-day retention)
/// runs inside the idle reaper's tick loop. Both phases scan the whole data
/// dir; daily is the right cadence for reclaiming garbage.
pub(super) const STORAGE_MAINTENANCE_INTERVAL: std::time::Duration =
    std::time::Duration::from_secs(24 * 60 * 60);

/// How often the idle-empty reaper sweeps. One minute keeps abandoned empty
/// sessions bounded without meaningfully waking the server.
pub(super) const IDLE_REAPER_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);

/// Per-session budget for `SessionEnd` hooks during teardown.
/// A user hook runs an external process; a hung one must not pin the server
/// (this bound applies to single-session kills; server shutdown sizes the
/// same budget against its remaining grace).
pub(super) const DEFAULT_SESSION_END_HOOK_BUDGET: std::time::Duration =
    std::time::Duration::from_secs(5);
/// A purge waits for a cancelled driver to finish unwinding before deleting
/// its durable rows. A driver that cannot settle in this budget is skipped,
/// never deleted underneath.
pub(super) const DRIVER_SHUTDOWN_BUDGET: std::time::Duration = std::time::Duration::from_secs(15);

/// Total budget for assembling a claimed session. Hooks, local skill scans and
/// store loads all have their own bounds; this is the backstop that turns a
/// wedged build into a visible `SessionPhase::Failed` instead of a session that
/// stays `Preparing` forever (ADR-0133).
pub(super) const BUILD_BUDGET: std::time::Duration = std::time::Duration::from_secs(120);

/// Defensive pre-`Ready` request buffer. The client queues until the snapshot
/// arrives; this bounds what a misbehaving client can enqueue on the server.
pub(super) const SESSION_REQUEST_BUFFER: usize = 256;
/// How long an idle *persisted* hosted session with no attached clients may
/// stay resident before it is suspended (torn down in memory; the transcript
/// is durable, so the next attach lazy-resumes it). This is what bounds the
/// server's memory: without it, every real session a server ever hosted
/// stayed resident forever (full transcript + agent + MCP runtime + tasks).
pub(super) const IDLE_HOSTED_SESSION_TTL: std::time::Duration =
    std::time::Duration::from_secs(30 * 60);

impl ThreadRegistry {
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
        let archivist = Arc::new(
            crate::runtime::archivist_service::ArchivistService::with_fabric(fabric.clone()),
        );
        Self {
            params,
            sessions: Arc::new(Mutex::new(HashMap::new())),
            building: Arc::new(Mutex::new(HashMap::new())),
            monitor,
            meta: Arc::new(Mutex::new(MonitorMeta::default())),
            server_tasks: Arc::new(crate::runtime::background_jobs::BackgroundJobManager::new()),
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
            purge_gate: Arc::new(tokio::sync::RwLock::new(())),
        }
    }
}

pub(crate) enum AssembleErr {
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

pub(super) fn lookup_latest_session_in_scope(scope: &nuo_protocol::ThreadScope) -> Option<String> {
    nuo_persistence::db::get_persistence_handle()
        .reader()
        .ok()?
        .latest_session_in_scope(scope)
        .ok()
        .flatten()
}

/// Resolve a session's durable binding (scope + workspace) by exact id,
/// independent of the caller's workspace (0220).
///
/// Runs on the blocking pool: `[INV-ATTACH-03]` allows only bounded local work
/// before Welcome, but that work must not park an async worker.
pub(super) async fn lookup_session_workspace(id: &str) -> Option<SessionBinding> {
    let id = id.to_string();
    tokio::task::spawn_blocking(move || {
        let (workspace, role) = nuo_persistence::db::get_persistence_handle()
            .reader()
            .ok()?
            .lookup_session_workspace(&id)
            .ok()
            .flatten()?;
        Some(SessionBinding { workspace, role })
    })
    .await
    .ok()
    .flatten()
}

pub(super) async fn lookup_session_manifest(id: &str) -> Option<nuo_protocol::ThreadRoleManifest> {
    let id = id.to_string();
    tokio::task::spawn_blocking(move || {
        nuo_persistence::db::get_persistence_handle()
            .reader()
            .ok()?
            .lookup_session_manifest(&id)
            .ok()
            .flatten()
    })
    .await
    .ok()
    .flatten()
}
