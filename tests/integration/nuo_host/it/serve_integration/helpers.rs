//! Shared harness for the serve integration suites: registry hosting,
//! session stores, monitor snapshots, and control-plane round trips.

use std::sync::Arc;

use nuo::monitor::MonitorTracker;
use nuo::registry::{HostedThread, ThreadRegistry};
use nuo_persistence::thread::ThreadSession;
use nuo_protocol::{AgentRequest, AgentResponse, MonitorAction, MonitorEvent};
use tokio::sync::{Mutex, broadcast, mpsc};
pub(super) fn idle_base(id: String) -> nuo_protocol::MonitoredThread {
    nuo_protocol::MonitoredThread {
        id,
        title: None,
        overview: String::new(),
        created_at: 0,
        updated_at: 0,
        message_count: 0,
        status: nuo_protocol::ThreadActivity::Idle,
        hosting: nuo_protocol::ThreadHosting::Hosted,
        round: 0,
        turn: None,
        output_tokens: 0,
        elapsed_ms: 0,
        current_tool: None,
        activity: None,
        context_tokens: None,
        note: None,
        project_root: String::new(),
        role: "developer".into(),
        parent_id: None,
        origin: nuo_protocol::ThreadOrigin::default(),
        digest: None,
    }
}

pub(super) async fn prehosted(
    session: Arc<ThreadSession>,
) -> (
    Arc<ThreadRegistry>,
    mpsc::Receiver<AgentRequest>,
    broadcast::Sender<AgentResponse>,
) {
    prehosted_with_catalog(session, nuo_protocol::CommandCatalog::default()).await
}

pub(super) async fn prehosted_with_catalog(
    session: Arc<ThreadSession>,
    command_catalog: nuo_protocol::CommandCatalog,
) -> (
    Arc<ThreadRegistry>,
    mpsc::Receiver<AgentRequest>,
    broadcast::Sender<AgentResponse>,
) {
    let (req_tx, req_rx) = mpsc::channel::<AgentRequest>(512);
    let (bc_tx, _) = broadcast::channel::<AgentResponse>(1024);
    let registry = Arc::new(ThreadRegistry::prehost_only());
    // The synthetic project has no contributed assets, so the attach path has
    // no quarantine notice to add to these tests' frames. The state file
    // needs its own directory: the
    // atomic-write path chmods the *parent* private (0700), which fails
    // with EPERM on the shared root-owned /tmp itself.
    let state_dir = std::env::temp_dir().join(format!("nuo-serve-it-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&state_dir).unwrap();
    let security = Arc::new(
        nuo_persistence::workspace_security::WorkspaceSecurityStore::load_from(
            state_dir.join("security.json"),
        ),
    );
    let base = idle_base(session.id().await);

    let tracker = Arc::new(Mutex::new(MonitorTracker::bootstrap(
        base,
        nuo_protocol::ThreadActivity::Idle,
    )));
    // Mimic the registry's broadcast-tap: fold every emitted response into the
    // tracker, publish a monitor diff, and buffer the attach-sync events, so
    // attach tests exercise the same path a real hosted session would take.
    let tap_tracker = tracker.clone();
    let mut tap_rx = bc_tx.subscribe();
    let registry_for_tap = registry.clone();
    let sync_buffer = Arc::new(Mutex::new(nuo::serve::AttachSyncBuffer::new()));
    let sync_buffer_for_tap = sync_buffer.clone();
    tokio::spawn(async move {
        while let Ok(response) = tap_rx.recv().await {
            let row = {
                let mut guard = tap_tracker.lock().await;
                guard.observe(&response);
                guard.row()
            };
            registry_for_tap.publish_for_test(MonitorEvent::ThreadUpdated(row));
            if matches!(
                response,
                AgentResponse::ProviderSwitched { .. }
                    | AgentResponse::ProviderPicker(_)
                    | AgentResponse::ProviderKeys(_)
            ) {
                sync_buffer_for_tap.lock().await.observe(&response);
            }
        }
    });
    registry
        .host(HostedThread {
            workspace_root: Some(std::path::PathBuf::from("/tmp/nuo-test-project")),
            human_channel: std::sync::Arc::new(
                nuo_protocol::human_request::HumanChannelAccountant::new(),
            ),
            security,
            session,
            shared_confinement: nuo_protocol::SharedConfinement::default(),
            req_tx,
            events: bc_tx.clone(),
            cancel: tokio_util::sync::CancellationToken::new(),
            driver_done: std::sync::Arc::new(tokio::sync::Semaphore::new(0)),
            tracker,
            sync_buffer,
            command_catalog,
            initial_provider: "test-provider".into(),
            initial_model: "test-model".into(),
            created_at: std::time::Instant::now(),
            last_activity: tokio::sync::Mutex::new(std::time::Instant::now()),
            last_seen_tick: std::sync::atomic::AtomicU64::new(0),
            activity_tick: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            agent_for_session_end: None,
        })
        .await;
    (registry, req_rx, bc_tx)
}

/// Host a throwaway session rooted at `project`, returning its id. Unlike
/// [`prehosted`] this takes the project explicitly and skips the
/// broadcast-tap: project-scoping tests need several hosted sessions with
/// distinct roots in one registry, and only the registry's project index
/// matters for them.
pub(super) async fn host_with_project(
    registry: &ThreadRegistry,
    project: std::path::PathBuf,
) -> String {
    // `for_path` keeps every artifact under the given project dir instead of
    // minting files in the real XDG project bucket; the registry only needs
    // `project_root` for routing/indexing.
    let session = Arc::new(ThreadSession::for_path(
        project.join("sessions").join("session.json"),
    ));
    let id = session.id().await;
    let (req_tx, _req_rx) = mpsc::channel::<AgentRequest>(512);
    let (bc_tx, _) = broadcast::channel::<AgentResponse>(1024);
    let tracker = Arc::new(Mutex::new(MonitorTracker::bootstrap(
        idle_base(id.clone()),
        nuo_protocol::ThreadActivity::Idle,
    )));
    registry
        .host(HostedThread {
            workspace_root: Some(project),
            human_channel: std::sync::Arc::new(
                nuo_protocol::human_request::HumanChannelAccountant::new(),
            ),
            security: std::sync::Arc::new(
                nuo_persistence::workspace_security::WorkspaceSecurityStore::load(),
            ),
            session,
            shared_confinement: nuo_protocol::SharedConfinement::default(),
            req_tx,
            events: bc_tx,
            cancel: tokio_util::sync::CancellationToken::new(),
            driver_done: std::sync::Arc::new(tokio::sync::Semaphore::new(0)),
            tracker,
            sync_buffer: Arc::new(Mutex::new(nuo::serve::AttachSyncBuffer::new())),
            command_catalog: nuo_protocol::CommandCatalog::default(),
            initial_provider: "test-provider".into(),
            initial_model: "test-model".into(),
            created_at: std::time::Instant::now(),
            last_activity: tokio::sync::Mutex::new(std::time::Instant::now()),
            last_seen_tick: std::sync::atomic::AtomicU64::new(0),
            activity_tick: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            agent_for_session_end: None,
        })
        .await;
    id
}
/// only potential event receiver is one the test adds explicitly. Unlike
/// [`prehosted`] this leaves `events.receiver_count() == 0`, which is what the
/// reaper's "no attached client" probe keys on.
pub(super) async fn host_bare(
    session: Arc<ThreadSession>,
    created_at: std::time::Instant,
) -> (
    Arc<ThreadRegistry>,
    broadcast::Sender<AgentResponse>,
    String,
) {
    let (req_tx, _req_rx) = mpsc::channel::<AgentRequest>(512);
    let (bc_tx, _) = broadcast::channel::<AgentResponse>(1024);
    let registry = Arc::new(ThreadRegistry::prehost_only());
    let base = idle_base(session.id().await);
    let tracker = Arc::new(Mutex::new(MonitorTracker::bootstrap(
        base,
        nuo_protocol::ThreadActivity::Idle,
    )));
    let id = session.id().await;
    registry
        .host(HostedThread {
            workspace_root: Some(std::env::temp_dir().join("nuo-reaper-project")),
            human_channel: std::sync::Arc::new(
                nuo_protocol::human_request::HumanChannelAccountant::new(),
            ),
            security: std::sync::Arc::new(
                nuo_persistence::workspace_security::WorkspaceSecurityStore::load(),
            ),
            session,
            shared_confinement: nuo_protocol::SharedConfinement::default(),
            req_tx,
            events: bc_tx.clone(),
            cancel: tokio_util::sync::CancellationToken::new(),
            driver_done: std::sync::Arc::new(tokio::sync::Semaphore::new(0)),
            tracker,
            sync_buffer: Arc::new(Mutex::new(nuo::serve::AttachSyncBuffer::new())),
            command_catalog: nuo_protocol::CommandCatalog::default(),
            initial_provider: "test-provider".into(),
            initial_model: "test-model".into(),
            created_at,
            last_activity: tokio::sync::Mutex::new(created_at),
            last_seen_tick: std::sync::atomic::AtomicU64::new(0),
            activity_tick: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            agent_for_session_end: None,
        })
        .await;
    (registry, bc_tx, id)
}

/// A fresh project dir + ThreadSession that has never persisted (empty).
pub(super) fn fresh_empty_store(tag: &str) -> (std::path::PathBuf, Arc<ThreadSession>) {
    let dir = std::env::temp_dir().join(format!("nuo-reaper-{tag}-{}", uuid::Uuid::new_v4()));
    // `for_path` keeps every artifact under the throwaway dir; nothing lands
    // in the real XDG project bucket.
    let store = Arc::new(ThreadSession::for_path(dir.join("session.json")));
    (dir, store)
}

/// Snapshot the set of currently hosted session ids.
pub(super) async fn hosted_ids(registry: &ThreadRegistry) -> std::collections::HashSet<String> {
    registry
        .monitor_snapshot(MonitorAction {
            watch: false,
            include_idle: true,
        })
        .await
        .threads
        .into_iter()
        .map(|r| r.id)
        .collect()
}
