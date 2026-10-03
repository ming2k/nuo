//! Session-level AI title generation: lightweight, fast, on-demand.
//!
//! Generates a concise title from the user's opening prompt concurrently
//! upon round admission, without blocking TTFT or tying to end-of-round digest tasks.

use crate::agent::Agent;
use nuo_wire::SessionTitleInput;
use std::sync::Arc;

/// Character budget for prompt excerpt used for title generation.
const TITLE_PROMPT_BUDGET_CHARS: usize = 1_000;

impl Agent {
    /// Generate a concise session title from `prompt`, or `None` on failure.
    pub async fn generate_session_title(&self, prompt: &str) -> Option<String> {
        let trimmed = prompt.trim();
        if trimmed.is_empty() {
            return None;
        }

        let excerpt: String = trimmed.chars().take(TITLE_PROMPT_BUDGET_CHARS).collect();
        self.cognitive()
            .generate_title(SessionTitleInput { excerpt })
            .await
    }

    /// Spawn session title generation as a detached, non-blocking background task.
    ///
    /// Fires concurrently when a session starts its first user round. If the session
    /// already has a title (manual override or previous generation), this is a no-op.
    pub fn spawn_session_titler(
        self: &Arc<Self>,
        session: Arc<nuo_persistence::SessionStore>,
        prompt: String,
    ) {
        let agent = Arc::clone(self);
        tokio::spawn(async move {
            let (_, has_title) = session.title().await;
            if has_title {
                return;
            }

            if let Some(title) = agent.generate_session_title(&prompt).await {
                let (_, has_title_after) = session.title().await;
                if !has_title_after {
                    let sid = session.id().await;
                    if let Err(error) = session.set_title(Some(title.clone()), false).await {
                        tracing::warn!(%error, "could not persist session title");
                    } else {
                        tracing::info!(session = %sid, %title, "session title established");
                        // Notify the embedding so it can push a sessions
                        // overview (absent observer = fire-and-forget no-op;
                        // the title is already durable, this is pure
                        // presentation). The guard is dropped before the
                        // await so the future stays `Send`.
                        let observer = agent
                            .title_established
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .clone();
                        if let Some(observer) = observer {
                            observer(&title).await;
                        }
                    }
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use nuo_wire::{Message, ModelRequest, Provider, Role};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct MockTitleProvider {
        consult_count: AtomicUsize,
        reply: String,
    }

    #[async_trait]
    impl Provider for MockTitleProvider {
        async fn chat(
            &self,
            _request: ModelRequest,
        ) -> Result<nuo_wire::ProviderCompletion, nuo_wire::ProviderError> {
            self.consult_count.fetch_add(1, Ordering::SeqCst);
            Ok(nuo_wire::ProviderCompletion::message(Message::new(
                Role::Assistant,
                self.reply.clone(),
            )))
        }

        async fn stream_chat(
            &self,
            _request: ModelRequest,
        ) -> Result<
            futures::stream::BoxStream<'static, Result<String, nuo_wire::ProviderError>>,
            nuo_wire::ProviderError,
        > {
            Ok(Box::pin(futures::stream::empty()))
        }
    }

    fn test_agent(provider: Arc<MockTitleProvider>) -> Agent {
        Agent::new(provider, Vec::new(), crate::AgentIdentity::default())
    }

    #[tokio::test]
    async fn generate_session_title_returns_clean_title() {
        let provider = Arc::new(MockTitleProvider {
            consult_count: AtomicUsize::new(0),
            reply: "\"Refactor user authentication\"".to_string(),
        });
        let agent = test_agent(provider.clone());

        let title = agent
            .generate_session_title("Can we refactor the user authentication in auth.rs?")
            .await;

        assert_eq!(title.as_deref(), Some("Refactor user authentication"));
        assert_eq!(provider.consult_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn generate_session_title_skips_empty_prompt() {
        let provider = Arc::new(MockTitleProvider {
            consult_count: AtomicUsize::new(0),
            reply: "Any".to_string(),
        });
        let agent = test_agent(provider);
        assert!(agent.generate_session_title("   \n\t ").await.is_none());
    }

    #[tokio::test]
    async fn spawn_session_titler_sets_title_if_none() {
        let provider = Arc::new(MockTitleProvider {
            consult_count: AtomicUsize::new(0),
            reply: "Fix broken build".to_string(),
        });
        let agent = Arc::new(test_agent(provider));

        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(nuo_persistence::SessionStore::for_path(
            dir.path().join("session.json"),
        ));
        let (title, has_title) = store.title().await;
        assert!(title.is_none());
        assert!(!has_title);

        agent.spawn_session_titler(store.clone(), "Cargo build is failing on Linux".into());

        // Await background task completion
        for _ in 0..30 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            let (title, has_title) = store.title().await;
            if has_title {
                assert_eq!(title.as_deref(), Some("Fix broken build"));
                return;
            }
        }
        panic!("titler did not populate title in time");
    }

    #[tokio::test]
    async fn spawn_session_titler_preserves_existing_title() {
        let provider = Arc::new(MockTitleProvider {
            consult_count: AtomicUsize::new(0),
            reply: "New generated title".to_string(),
        });
        let agent = Arc::new(test_agent(provider.clone()));

        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(nuo_persistence::SessionStore::for_path(
            dir.path().join("session.json"),
        ));
        store
            .set_title(Some("Manual Title".into()), true)
            .await
            .unwrap();

        agent.spawn_session_titler(store.clone(), "Some random prompt".into());

        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        let (title, _) = store.title().await;
        assert_eq!(title.as_deref(), Some("Manual Title"));
        assert_eq!(provider.consult_count.load(Ordering::SeqCst), 0);
    }
}
