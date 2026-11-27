//! Driver-owned background session titler (ADR-0262 `[INV-TITLE-02]`).
//!
//! `nuo-engine` owns only pure, stateless title distillation
//! (`Agent::generate_session_title`); this module owns the background task
//! handle, per-session deduplication, cancellation across session switches /
//! manual renames / driver shutdown, durable persistence, and live client
//! overview broadcast.

use std::sync::Arc;
use tokio::sync::mpsc;

use nuo_engine::Agent;
use nuo_persistence::thread::ThreadSession;
use nuo_protocol::AgentResponse;

/// Supervised background titler scoped to one [`super::ThreadDriver`].
///
/// At most one title-generation task is live at any instant. Switching
/// sessions, manually renaming the active thread, or shutting down the driver
/// aborts any in-flight task deterministically instead of leaving detached
/// `tokio::spawn` tasks behind.
#[derive(Default)]
pub(crate) struct SessionTitler {
    active: Option<(String, tokio::task::JoinHandle<()>)>,
}

impl SessionTitler {
    pub(crate) fn new() -> Self {
        Self { active: None }
    }

    /// Spawn a background title-generation task for `session` once `admitted`
    /// resolves to `true`, if the session currently has no title, `prompt` is
    /// non-blank, and no task is already in flight for the same session.
    ///
    /// Awaiting the round-admission oneshot inside the background task ensures
    /// a prompt rejected by a `UserPromptSubmit` hook never wastes a provider
    /// request or persists a title onto an empty thread, while admitted rounds
    /// still run title distillation concurrently with streaming.
    pub(crate) async fn maybe_spawn_on_admission(
        &mut self,
        agent: &Arc<Agent>,
        session: &Arc<ThreadSession>,
        resp_tx: &mpsc::UnboundedSender<AgentResponse>,
        prompt: &str,
        admitted: tokio::sync::oneshot::Receiver<bool>,
    ) {
        if prompt.trim().is_empty() {
            return;
        }
        let expected_session_id = session.id().await;
        let (_, has_title) = session.title().await;
        if has_title {
            return;
        }
        if let Some((active_id, handle)) = &self.active {
            if active_id == &expected_session_id && !handle.is_finished() {
                return;
            }
            handle.abort();
        }
        self.active = None;

        let agent = Arc::clone(agent);
        let session = Arc::clone(session);
        let resp_tx = resp_tx.clone();
        let prompt = prompt.to_string();
        let sid_for_task = expected_session_id.clone();

        let handle = tokio::spawn(async move {
            if !admitted.await.unwrap_or(false) {
                return;
            }
            let Some(title) = agent.generate_session_title(&prompt).await else {
                return;
            };
            // Re-verify identity and title lock after the async provider call:
            // the driver may have switched sessions or the operator may have
            // renamed the thread manually while distillation was in flight.
            if session.id().await != sid_for_task {
                return;
            }
            let (_, has_title_after) = session.title().await;
            if has_title_after {
                return;
            }
            if let Err(error) = session.set_title(Some(title.clone()), false).await {
                tracing::warn!(session = %sid_for_task, %error, "could not persist session title");
                return;
            }
            tracing::info!(session = %sid_for_task, %title, "session title established");
            // Push a fresh threads overview directly from the server layer so
            // every attached client (TUI tabs, scene head, dashboard, picker)
            // updates live — the exact refresh path manual `RenameThread` uses.
            crate::handlers::handlers_thread::overview(&session, &resp_tx).await;
        });

        self.active = Some((expected_session_id, handle));
    }

    /// Abort any in-flight title generation task (driver shutdown or session switch).
    pub(crate) fn abort(&mut self) {
        if let Some((_, handle)) = self.active.take() {
            handle.abort();
        }
    }

    /// Abort an in-flight title task if it targets `session_id` (manual rename).
    pub(crate) fn abort_if_session(&mut self, session_id: &str) {
        if self
            .active
            .as_ref()
            .is_some_and(|(active_id, _)| active_id == session_id)
        {
            self.abort();
        }
    }
}

impl Drop for SessionTitler {
    fn drop(&mut self) {
        self.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use nuo_protocol::{ModelRequest, Provider, ProviderCompletion, ProviderError};
    use nuo_tool::{Message, Role};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    struct MockTitleProvider {
        consults: AtomicUsize,
        reply: String,
        delay: Duration,
    }

    #[async_trait]
    impl Provider for MockTitleProvider {
        async fn chat(&self, _request: &ModelRequest) -> Result<ProviderCompletion, ProviderError> {
            self.consults.fetch_add(1, Ordering::SeqCst);
            if !self.delay.is_zero() {
                tokio::time::sleep(self.delay).await;
            }
            Ok(ProviderCompletion::message(Message::new(
                Role::Assistant,
                self.reply.clone(),
            )))
        }

        async fn stream_chat(
            &self,
            _request: &ModelRequest,
            _telemetry: nuo_protocol::TransportTelemetry,
        ) -> Result<futures::stream::BoxStream<'static, Result<String, ProviderError>>, ProviderError>
        {
            Ok(Box::pin(futures::stream::empty()))
        }
    }

    fn test_agent(provider: Arc<MockTitleProvider>) -> Arc<Agent> {
        Arc::new(Agent::new(
            provider,
            Vec::new(),
            nuo_protocol::AgentIdentity::default(),
        ))
    }

    async fn await_title(session: &ThreadSession) -> (Option<String>, bool) {
        for _ in 0..100 {
            let (title, has_title) = session.title().await;
            if has_title {
                return (title, has_title);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        session.title().await
    }

    fn admitted_rx(admitted: bool) -> tokio::sync::oneshot::Receiver<bool> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let _ = tx.send(admitted);
        rx
    }

    #[tokio::test]
    async fn establishes_title_and_broadcasts_threads_overview() {
        let provider = Arc::new(MockTitleProvider {
            consults: AtomicUsize::new(0),
            reply: "Fix auth retry loop".to_string(),
            delay: Duration::ZERO,
        });
        let agent = test_agent(provider.clone());
        let dir = tempfile::tempdir().unwrap();
        let session = Arc::new(ThreadSession::for_path(dir.path().join("session.json")));
        let (tx, mut rx) = mpsc::unbounded_channel();

        let mut titler = SessionTitler::new();
        titler
            .maybe_spawn_on_admission(
                &agent,
                &session,
                &tx,
                "Fix the auth retry loop in login.rs",
                admitted_rx(true),
            )
            .await;

        let (title, has_title) = await_title(&session).await;
        assert!(has_title);
        assert_eq!(title.as_deref(), Some("Fix auth retry loop"));
        assert_eq!(provider.consults.load(Ordering::SeqCst), 1);

        let overview = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("overview broadcast arrives")
            .expect("channel open");
        assert!(
            matches!(overview, AgentResponse::ThreadsOverview(_)),
            "expected ThreadsOverview broadcast, got {overview:?}"
        );
    }

    #[tokio::test]
    async fn skips_provider_and_title_when_round_admission_is_denied() {
        let provider = Arc::new(MockTitleProvider {
            consults: AtomicUsize::new(0),
            reply: "Should never run".to_string(),
            delay: Duration::ZERO,
        });
        let agent = test_agent(provider.clone());
        let dir = tempfile::tempdir().unwrap();
        let session = Arc::new(ThreadSession::for_path(dir.path().join("session.json")));
        let (tx, mut rx) = mpsc::unbounded_channel();

        let mut titler = SessionTitler::new();
        titler
            .maybe_spawn_on_admission(
                &agent,
                &session,
                &tx,
                "Blocked by UserPromptSubmit hook",
                admitted_rx(false),
            )
            .await;

        tokio::time::sleep(Duration::from_millis(30)).await;
        let (title, has_title) = session.title().await;
        assert!(!has_title);
        assert!(title.is_none());
        assert_eq!(provider.consults.load(Ordering::SeqCst), 0);
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn preserves_existing_manual_title_without_consulting_provider() {
        let provider = Arc::new(MockTitleProvider {
            consults: AtomicUsize::new(0),
            reply: "Overwritten".to_string(),
            delay: Duration::ZERO,
        });
        let agent = test_agent(provider.clone());
        let dir = tempfile::tempdir().unwrap();
        let session = Arc::new(ThreadSession::for_path(dir.path().join("session.json")));
        session
            .set_title(Some("Manual Title".into()), true)
            .await
            .unwrap();
        let (tx, _rx) = mpsc::unbounded_channel();

        let mut titler = SessionTitler::new();
        titler
            .maybe_spawn_on_admission(&agent, &session, &tx, "Some prompt", admitted_rx(true))
            .await;

        tokio::time::sleep(Duration::from_millis(30)).await;
        let (title, _) = session.title().await;
        assert_eq!(title.as_deref(), Some("Manual Title"));
        assert_eq!(provider.consults.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn deduplicates_concurrent_spawns_for_same_session() {
        let provider = Arc::new(MockTitleProvider {
            consults: AtomicUsize::new(0),
            reply: "Single title".to_string(),
            delay: Duration::from_millis(60),
        });
        let agent = test_agent(provider.clone());
        let dir = tempfile::tempdir().unwrap();
        let session = Arc::new(ThreadSession::for_path(dir.path().join("session.json")));
        let (tx, _rx) = mpsc::unbounded_channel();

        let mut titler = SessionTitler::new();
        titler
            .maybe_spawn_on_admission(&agent, &session, &tx, "First prompt", admitted_rx(true))
            .await;
        titler
            .maybe_spawn_on_admission(
                &agent,
                &session,
                &tx,
                "Second prompt while first in flight",
                admitted_rx(true),
            )
            .await;

        let (title, has_title) = await_title(&session).await;
        assert!(has_title);
        assert_eq!(title.as_deref(), Some("Single title"));
        assert_eq!(provider.consults.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn abort_cancels_in_flight_title_task() {
        let provider = Arc::new(MockTitleProvider {
            consults: AtomicUsize::new(0),
            reply: "Late title".to_string(),
            delay: Duration::from_millis(80),
        });
        let agent = test_agent(provider);
        let dir = tempfile::tempdir().unwrap();
        let session = Arc::new(ThreadSession::for_path(dir.path().join("session.json")));
        let (tx, mut rx) = mpsc::unbounded_channel();

        let mut titler = SessionTitler::new();
        titler
            .maybe_spawn_on_admission(&agent, &session, &tx, "Aborted prompt", admitted_rx(true))
            .await;
        titler.abort();

        tokio::time::sleep(Duration::from_millis(120)).await;
        let (title, has_title) = session.title().await;
        assert!(!has_title);
        assert!(title.is_none());
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn skips_blank_prompts() {
        let provider = Arc::new(MockTitleProvider {
            consults: AtomicUsize::new(0),
            reply: "Should not run".to_string(),
            delay: Duration::ZERO,
        });
        let agent = test_agent(provider.clone());
        let dir = tempfile::tempdir().unwrap();
        let session = Arc::new(ThreadSession::for_path(dir.path().join("session.json")));
        let (tx, _rx) = mpsc::unbounded_channel();

        let mut titler = SessionTitler::new();
        titler
            .maybe_spawn_on_admission(&agent, &session, &tx, "   \n\t ", admitted_rx(true))
            .await;
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(provider.consults.load(Ordering::SeqCst), 0);
    }
}
