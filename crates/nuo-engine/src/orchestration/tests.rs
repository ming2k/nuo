//! Orchestration tests: projection budgets, session titles, and capture pruning.

use super::proxy::{MAX_CAPTURE_FILES, prune_capture_dir};
use super::*;

#[test]
fn compaction_target_accounts_for_projected_request_overhead() {
    let settings = ContextProjectionSettings {
        request_overhead_tokens: 0,
        budget: nuo_protocol::CompactionPolicy::default().resolve(200_000),
        preserve_rounds: 6,
        summarize: true,
        prune: true,
        prune_protect_tokens: 24_000,
    };
    let resolved = settings.for_request(RequestTokenEstimate {
        history_tokens: 100_000,
        overhead_tokens: 12_000,
        total_tokens: 112_000,
        temporary_context_tokens: 0,
    });

    assert_eq!(settings.budget.target_tokens, 50_000);
    assert_eq!(resolved.budget.target_tokens, 38_000);
    assert_eq!(resolved.budget.compaction_threshold_tokens, 170_000);
}

use crate::AgentIdentity;
use async_trait::async_trait;
use nuo_protocol::{ModelRequest, ProviderStreamEvent};
use nuo_tool::{Message, Role};

use std::sync::atomic::{AtomicUsize, Ordering};

struct TitleProvider {
    consults: AtomicUsize,
}

#[async_trait]
impl nuo_protocol::Provider for TitleProvider {
    async fn chat(
        &self,
        _request: &ModelRequest,
    ) -> Result<nuo_protocol::ProviderCompletion, nuo_protocol::ProviderError> {
        self.consults.fetch_add(1, Ordering::SeqCst);
        Ok(nuo_protocol::ProviderCompletion::message(Message::new(
            Role::Assistant,
            "Fixing the build",
        )))
    }
    async fn stream_chat(
        &self,
        _request: &ModelRequest,
        _telemetry: nuo_protocol::TransportTelemetry,
    ) -> Result<
        futures::stream::BoxStream<'static, Result<String, nuo_protocol::ProviderError>>,
        nuo_protocol::ProviderError,
    > {
        Ok(Box::pin(futures::stream::empty()))
    }
    async fn stream_chat_events(
        &self,
        _request: &ModelRequest,
        _telemetry: nuo_protocol::TransportTelemetry,
    ) -> Result<
        futures::stream::BoxStream<
            'static,
            Result<ProviderStreamEvent, nuo_protocol::ProviderError>,
        >,
        nuo_protocol::ProviderError,
    > {
        Ok(Box::pin(futures::stream::empty()))
    }
}

#[allow(dead_code)]
struct AutoCleanArcSession(Arc<ThreadSession>, tempfile::TempDir);
impl std::ops::Deref for AutoCleanArcSession {
    type Target = Arc<ThreadSession>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

async fn fresh_title_session() -> AutoCleanArcSession {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(ThreadSession::for_path(dir.path().join("session.json")));
    AutoCleanArcSession(store, dir)
}

async fn await_title(session: &ThreadSession) -> (Option<String>, bool) {
    for _ in 0..200 {
        let (title, manual) = session.title().await;
        if title.is_some() {
            return (title, manual);
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    session.title().await
}

#[tokio::test]
async fn session_title_spawned_concurrently_on_first_prompt() {
    let session = fresh_title_session().await;
    let provider = Arc::new(TitleProvider {
        consults: AtomicUsize::new(0),
    });
    let agent = Arc::new(Agent::new(
        provider.clone(),
        Vec::new(),
        AgentIdentity::default(),
    ));

    let (title, has_title) = session.title().await;
    assert!(title.is_none());
    assert!(!has_title);

    agent.spawn_session_titler(session.clone(), "Fix production memory leak".into());
    let (title, has_title) = await_title(&session).await;
    assert_eq!(title.as_deref(), Some("Fixing the build"));
    assert!(has_title);
}

#[tokio::test]
async fn manual_title_lock_is_not_overwritten_by_spawn_session_titler() {
    let session = fresh_title_session().await;
    session
        .set_title(Some("My own title".into()), true)
        .await
        .unwrap();
    let provider = Arc::new(TitleProvider {
        consults: AtomicUsize::new(0),
    });
    let agent = Arc::new(Agent::new(
        provider.clone(),
        Vec::new(),
        AgentIdentity::default(),
    ));
    agent.spawn_session_titler(session.clone(), "Hello".into());
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let (title, manual) = session.title().await;
    assert_eq!(title.as_deref(), Some("My own title"));
    assert!(manual);
    assert_eq!(provider.consults.load(Ordering::SeqCst), 0);
}

/// An armed `/debug trace` writes one full-context capture per round-trip
/// and nothing ever removed them — on a long session the capture dir grew
/// faster than every other data path combined. Retention keeps the newest
/// [`MAX_CAPTURE_FILES`] and deletes older ones in age order.
#[test]
fn capture_prune_keeps_newest_max_files() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for i in 0..(MAX_CAPTURE_FILES + 5) {
        let name = format!("20260101-0000{i:02}.000_0001_anon_m.json");
        std::fs::write(root.join(name), b"x").unwrap();
    }
    // A foreign file must not count toward or be affected by retention.
    std::fs::write(root.join("notes.txt"), b"x").unwrap();

    prune_capture_dir(root);

    let mut remaining: Vec<String> = std::fs::read_dir(root)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    remaining.sort();
    assert_eq!(remaining.len(), MAX_CAPTURE_FILES + 1);
    assert!(remaining.contains(&"notes.txt".to_string()));
    // The five oldest captures are gone; the newest survives.
    assert!(!remaining.contains(&"20260101-000000.000_0001_anon_m.json".to_string()));
    assert!(remaining.contains(&format!(
        "20260101-0000{}.000_0001_anon_m.json",
        MAX_CAPTURE_FILES + 4
    )));
}

#[test]
fn capture_prune_is_noop_at_or_below_limit() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("20260101-000000.000_0001_anon_m.json"),
        b"x",
    )
    .unwrap();
    prune_capture_dir(dir.path());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}
