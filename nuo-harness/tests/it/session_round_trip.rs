//! Integration test: panicking on assertion failure is the desired
//! behaviour here, so the workspace `unwrap_used`/`expect_used` lints
//! are relaxed for this file. (Lib/bin code stays linted.)
#![allow(clippy::unwrap_used, clippy::expect_used)]

//! End-to-end session-persistence round-trip.
//!
//! Inline unit tests inside `nuo-harness` and `nuo-persistence` cover each half
//! of this flow in isolation. The purpose of this file is to verify the seam
//! between them composes correctly: a turn driven against a fresh on-disk
//! `SessionStore` (via `execute_round`) must leave enough state on disk that a
//! brand-new `SessionStore` opened at the same path can `resume` the saved id
//! and recover the exact message sequence.
//!
//! This is the kind of regression that no inline test catches: a change to the
//! session event format or to `execute_round`'s save points can leave both
//! halves internally consistent while breaking the round-trip.

use std::sync::Arc;

use futures::stream::{BoxStream, StreamExt};
use nuo_wire::{Message, ModelRequest, Provider, Role, async_trait};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use nuo_harness::Agent;
use nuo_harness::orchestration::{
    ContextProjectionSettings, RoundContext, RoundInput, execute_round,
};
use nuo_persistence::session::SessionStore;

/// Concatenation of the chunks emitted by [`TestStreamProvider::stream_chat`].
const MOCK_REPLY: &str = "This is a streaming mock response from muta!";

/// Minimal provider whose `stream_chat` emits `MOCK_REPLY` in chunks.
struct TestStreamProvider;

#[async_trait]
impl Provider for TestStreamProvider {
    async fn chat(
        &self,
        _request: ModelRequest,
    ) -> Result<nuo_wire::ProviderCompletion, nuo_wire::ProviderError> {
        Ok(nuo_wire::ProviderCompletion::message(Message::new(
            Role::Assistant,
            MOCK_REPLY,
        )))
    }

    async fn stream_chat(
        &self,
        _request: ModelRequest,
    ) -> Result<
        BoxStream<'static, Result<String, nuo_wire::ProviderError>>,
        nuo_wire::ProviderError,
    > {
        let chunks = [
            "This ",
            "is ",
            "a ",
            "streaming ",
            "mock ",
            "response ",
            "from ",
            "muta!",
        ];
        Ok(futures::stream::iter(chunks.into_iter().map(|c| Ok(c.to_string()))).boxed())
    }
}

#[tokio::test]
async fn execute_round_persists_a_session_that_resume_reopens() {
    let directory = std::env::temp_dir().join(format!(
        "muta-it-session-roundtrip-{}",
        uuid::Uuid::new_v4()
    ));
    let session_path = directory.join("session.json");
    let session = Arc::new(SessionStore::for_path(session_path.clone()));
    let agent = Arc::new(Agent::new(
        Arc::new(TestStreamProvider),
        Vec::new(),
        nuo_harness::AgentIdentity::default(),
    ));
    let (tx, _rx) = mpsc::unbounded_channel();

    let prompt = "hello, mock";
    let sent_at_ms = 1_700_000_000_123;
    execute_round(
        RoundContext {
            agent: agent.clone(),
            tx,
            token: CancellationToken::new(),
            session: session.clone(),
            session_id: session.id().await,
            projection: ContextProjectionSettings {
                budget: nuo_wire::CompactionPolicy::default().resolve(100_000),
                preserve_rounds: 6,
                summarize: false,
                prune: false,
                prune_protect_tokens: 0,
            },
            retry_max_attempts: 1,
            retry_base_ms: 1,
            retry_max_ms: 1,
            emit_round_completed: false,
        },
        RoundInput {
            prompt: prompt.to_string(),
            hidden: false,
            display_prompt: None,
            sent_at_ms: Some(sent_at_ms),
            images: Vec::new(),
            driver: nuo_harness::orchestration::RoundDriver::Fresh,
        },
    )
    .await
    .expect("round completes with the mock provider");

    // Snapshot the live state before dropping everything.
    let saved_id = session.id().await;
    let live_messages = session.model_window().await;
    assert!(
        live_messages
            .iter()
            .any(|message| message.role == Role::User && message.content == prompt),
        "live session should contain the user prompt"
    );
    assert_eq!(
        live_messages
            .iter()
            .find(|message| message.role == Role::User && message.content == prompt)
            .and_then(|message| message.sent_at_ms),
        Some(sent_at_ms),
        "live session should retain the exact UI send timestamp"
    );
    assert!(
        live_messages
            .iter()
            .any(|message| message.role == Role::Assistant && message.content == MOCK_REPLY),
        "live session should contain the mock assistant reply"
    );

    // Drop all in-memory state. The next line intentionally drops `agent`,
    // `session`, and the channel so the only thing left is the on-disk file.
    drop(agent);
    drop(session);

    // Reopen from disk by id. This is the integration seam: a fresh
    // `SessionStore` at the same path should recover the prior turn when asked
    // to resume the saved id.
    let reopened = SessionStore::for_path(session_path.clone());
    let resumed_id = reopened
        .resume(Some(&saved_id))
        .await
        .expect("resume reopens the saved session by id");
    assert_eq!(resumed_id, saved_id);

    let reopened_messages = reopened.model_window().await;
    assert_eq!(
        reopened_messages.len(),
        live_messages.len(),
        "reopened session should have the same message count as the live one"
    );
    for (reopened_message, live_message) in reopened_messages.iter().zip(live_messages.iter()) {
        assert_eq!(reopened_message.role, live_message.role);
        assert_eq!(reopened_message.content, live_message.content);
        assert_eq!(reopened_message.sent_at_ms, live_message.sent_at_ms);
    }

    let _ = std::fs::remove_dir_all(directory);
}
