//! Orchestration-layer integration tests: provider retry behavior, the
//! proxy provider, retry-delay math, context-overflow classification, and
//! the self-registration of built-in tools via `inventory`. These live with
//! the code under test (they were historically parked in the `nuo`
//! binary, which exercised this layer end-to-end before ADR-0096 moved
//! session hosting into the daemon).

// Tests panic on assertion failure by design; the workspace's unwrap/expect
// warnings are meant for production code.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use nuo_harness::Agent;
use nuo_harness::RoundLifecycle;
use nuo_harness::orchestration::{
    ContextProjectionSettings, InteractiveRoundContext, ProxyProvider, RoundContext, RoundInput,
    apply_jitter_ms, execute_round, retry_delay_ms, start_interactive_round,
};
use nuo_wire::{
    AgentResponse, Message, Provider, ProviderStreamEvent, Role, RoundEvent, ToolContextBuilder,
    async_trait, collect_toolset,
};
use nuo_persistence::session::SessionStore;
use nuo_harness::SkillRegistry;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use futures::StreamExt;
use futures::stream;

struct RetryOnceProvider(AtomicUsize);
struct PartialToolRetryProvider(AtomicUsize);
struct ToolThenRetryProvider {
    attempts: AtomicUsize,
    requests: Arc<Mutex<Vec<String>>>,
}
struct AlwaysRetryableProvider;
struct RetryReadTool(Arc<AtomicUsize>);

/// Serialize the provider-visible history while excluding local diagnostic
/// metadata that is regenerated for every request projection.
///
/// These tests verify checkpoint semantics. A projected system message's
/// wall-clock timestamp is intentionally not part of that protocol contract
/// and may advance while an instrumented/slow platform executes the round.
fn provider_history_snapshot(messages: &[Message]) -> String {
    let mut messages =
        serde_json::to_value(messages).expect("messages should serialize to a JSON value");
    for message in messages
        .as_array_mut()
        .expect("serialized messages should be an array")
    {
        message
            .as_object_mut()
            .expect("serialized message should be an object")
            .remove("timestamp");
    }
    serde_json::to_string(&messages).expect("messages should serialize")
}

#[test]
fn provider_history_snapshot_ignores_only_diagnostic_timestamps() {
    let original = Message::new(Role::System, "stable prompt");
    let mut later_projection = original.clone();
    later_projection.timestamp = original.timestamp.map(|timestamp| timestamp + 60);
    assert_eq!(
        provider_history_snapshot(std::slice::from_ref(&original)),
        provider_history_snapshot(std::slice::from_ref(&later_projection))
    );

    later_projection.content = "changed prompt".to_string();
    assert_ne!(
        provider_history_snapshot(std::slice::from_ref(&original)),
        provider_history_snapshot(std::slice::from_ref(&later_projection)),
        "semantic provider history must remain part of the checkpoint assertion"
    );
}

/// Minimal provider whose `chat` returns a canned reply — used by the
/// proxy-provider test to verify it does not block the async runtime.
struct MockProvider;

#[async_trait]
impl Provider for MockProvider {
    async fn chat(
        &self,
        _request: nuo_wire::ModelRequest,
    ) -> Result<nuo_wire::ProviderCompletion, nuo_wire::ProviderError> {
        Ok(nuo_wire::ProviderCompletion::message(Message::new(
            Role::Assistant,
            "Hello! I am a mock AI. How can I help you today?",
        )))
    }

    async fn stream_chat(
        &self,
        _request: nuo_wire::ModelRequest,
    ) -> Result<
        futures::stream::BoxStream<'static, Result<String, nuo_wire::ProviderError>>,
        nuo_wire::ProviderError,
    > {
        Ok(Box::pin(stream::empty()))
    }
}

/// Most built-in tools self-register via `inventory` across the nuo-harness
/// and nuo-persistence crates. This test guards the one real
/// risk of that approach — that a crate's `inventory::submit!` nodes get
/// dropped by the linker — by asserting the assembled set contains every
/// expected built-in tool name.
#[test]
fn registry_collects_all_self_registered_tools() {
    let mut builder = ToolContextBuilder::new();
    builder.provide(Arc::new(SkillRegistry::empty()));
    builder.provide(nuo_harness::AgentIdentity::default());
    let ctx = builder.build();
    let mut collected = collect_toolset(&ctx);
    let sys_ctx = Arc::new(nuo_tool_fs::SystemToolContext::new("."));
    for tool in nuo_tool_fs::create_fs_tools(sys_ctx.clone()) {
        collected.upsert(tool);
    }
    for tool in nuo_tool_exec::create_exec_tools(sys_ctx) {
        collected.upsert(tool);
    }
    let names: std::collections::HashSet<&str> = collected.capability_names().collect();
    for expected in [
        "execute_command",
        "read_text",
        "write_file",
        "edit_text",
        "search_text",
        "find_files",
        "ask_user",
        "read_url",
        "search_web",
    ] {
        assert!(
            names.contains(expected),
            "tool '{expected}' missing from collected set"
        );
    }
}

#[async_trait]
impl Provider for RetryOnceProvider {
    async fn chat(
        &self,
        _request: nuo_wire::ModelRequest,
    ) -> Result<nuo_wire::ProviderCompletion, nuo_wire::ProviderError> {
        Err(nuo_wire::ProviderError::new(
            "mock",
            nuo_wire::ProviderErrorKind::Other,
            "non-streaming path should not be used",
        ))
    }

    async fn stream_chat(
        &self,
        _request: nuo_wire::ModelRequest,
    ) -> Result<
        futures::stream::BoxStream<'static, Result<String, nuo_wire::ProviderError>>,
        nuo_wire::ProviderError,
    > {
        Ok(Box::pin(stream::empty()))
    }

    async fn stream_chat_events(
        &self,
        _request: nuo_wire::ModelRequest,
    ) -> Result<
        futures::stream::BoxStream<
            'static,
            Result<ProviderStreamEvent, nuo_wire::ProviderError>,
        >,
        nuo_wire::ProviderError,
    > {
        if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
            Ok(Box::pin(stream::iter(vec![
                Ok(ProviderStreamEvent::TextDelta("partial".to_string())),
                Err(nuo_wire::ProviderError::new(
                    "mock",
                    nuo_wire::ProviderErrorKind::RateLimited,
                    "rate limited",
                )
                .retryable(Some(1))),
            ])))
        } else {
            Ok(Box::pin(stream::iter(vec![
                Ok(ProviderStreamEvent::TextDelta("done".to_string())),
                Ok(ProviderStreamEvent::Completed(
                    nuo_wire::ProviderCompletionMeta::default(),
                )),
            ])))
        }
    }
}

#[async_trait]
impl Provider for PartialToolRetryProvider {
    async fn chat(
        &self,
        _request: nuo_wire::ModelRequest,
    ) -> Result<nuo_wire::ProviderCompletion, nuo_wire::ProviderError> {
        Err(nuo_wire::ProviderError::new(
            "mock",
            nuo_wire::ProviderErrorKind::Other,
            "non-streaming path should not be used",
        ))
    }

    async fn stream_chat(
        &self,
        _request: nuo_wire::ModelRequest,
    ) -> Result<
        futures::stream::BoxStream<'static, Result<String, nuo_wire::ProviderError>>,
        nuo_wire::ProviderError,
    > {
        Ok(Box::pin(stream::empty()))
    }

    async fn stream_chat_events(
        &self,
        _request: nuo_wire::ModelRequest,
    ) -> Result<
        futures::stream::BoxStream<
            'static,
            Result<ProviderStreamEvent, nuo_wire::ProviderError>,
        >,
        nuo_wire::ProviderError,
    > {
        if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
            Ok(Box::pin(stream::iter(vec![
                Ok(ProviderStreamEvent::ToolCallDelta {
                    index: 0,
                    id: Some("partial-call".to_string()),
                    name: Some("retry_read".to_string()),
                    arguments: "{".to_string(),
                }),
                Err(nuo_wire::ProviderError::new(
                    "mock",
                    nuo_wire::ProviderErrorKind::Transport,
                    "stream dropped",
                )
                .retryable(None)),
            ])))
        } else {
            Ok(Box::pin(stream::iter(vec![
                Ok(ProviderStreamEvent::TextDelta("done".to_string())),
                Ok(ProviderStreamEvent::Completed(
                    nuo_wire::ProviderCompletionMeta::default(),
                )),
            ])))
        }
    }
}

#[async_trait]
impl Provider for ToolThenRetryProvider {
    async fn chat(
        &self,
        _request: nuo_wire::ModelRequest,
    ) -> Result<nuo_wire::ProviderCompletion, nuo_wire::ProviderError> {
        Err(nuo_wire::ProviderError::new(
            "mock",
            nuo_wire::ProviderErrorKind::Other,
            "non-streaming path should not be used",
        ))
    }

    async fn stream_chat(
        &self,
        _request: nuo_wire::ModelRequest,
    ) -> Result<
        futures::stream::BoxStream<'static, Result<String, nuo_wire::ProviderError>>,
        nuo_wire::ProviderError,
    > {
        Ok(Box::pin(stream::empty()))
    }

    async fn stream_chat_events(
        &self,
        request: nuo_wire::ModelRequest,
    ) -> Result<
        futures::stream::BoxStream<
            'static,
            Result<ProviderStreamEvent, nuo_wire::ProviderError>,
        >,
        nuo_wire::ProviderError,
    > {
        self.requests
            .lock()
            .expect("request log lock poisoned")
            .push(provider_history_snapshot(&request.messages));
        let attempt = self.attempts.fetch_add(1, Ordering::SeqCst);
        match attempt {
            0 | 2 => Ok(Box::pin(stream::iter(vec![
                Ok(ProviderStreamEvent::ToolCallDelta {
                    index: 0,
                    id: Some(if attempt == 0 { "call" } else { "retry-call" }.to_string()),
                    name: Some("retry_read".to_string()),
                    arguments: "{}".to_string(),
                }),
                Ok(ProviderStreamEvent::Completed(
                    nuo_wire::ProviderCompletionMeta::default(),
                )),
            ]))),
            1 => Ok(Box::pin(stream::iter(vec![Err(
                nuo_wire::ProviderError::new(
                    "mock",
                    nuo_wire::ProviderErrorKind::Unavailable,
                    "upstream unavailable",
                )
                .retryable(None),
            )]))),
            _ => Ok(Box::pin(stream::iter(vec![
                Ok(ProviderStreamEvent::TextDelta("done".to_string())),
                Ok(ProviderStreamEvent::Completed(
                    nuo_wire::ProviderCompletionMeta::default(),
                )),
            ]))),
        }
    }
}

#[async_trait]
impl Provider for AlwaysRetryableProvider {
    async fn chat(
        &self,
        _request: nuo_wire::ModelRequest,
    ) -> Result<nuo_wire::ProviderCompletion, nuo_wire::ProviderError> {
        Err(nuo_wire::ProviderError::new(
            "mock",
            nuo_wire::ProviderErrorKind::Other,
            "non-streaming path should not be used",
        ))
    }

    async fn stream_chat(
        &self,
        _request: nuo_wire::ModelRequest,
    ) -> Result<
        futures::stream::BoxStream<'static, Result<String, nuo_wire::ProviderError>>,
        nuo_wire::ProviderError,
    > {
        Ok(Box::pin(stream::empty()))
    }

    async fn stream_chat_events(
        &self,
        _request: nuo_wire::ModelRequest,
    ) -> Result<
        futures::stream::BoxStream<
            'static,
            Result<ProviderStreamEvent, nuo_wire::ProviderError>,
        >,
        nuo_wire::ProviderError,
    > {
        // Every request fails with a retryable error so the turn exhausts
        // its retry budget without ever touching a tool.
        Ok(Box::pin(stream::iter(vec![Err(
            nuo_wire::ProviderError::new(
                "openai",
                nuo_wire::ProviderErrorKind::RateLimited,
                "OpenAI HTTP 429 Too Many Requests",
            )
            .retryable(None),
        )])))
    }
}

#[async_trait]
impl nuo_wire::Tool for RetryReadTool {
    fn name(&self) -> &str {
        "retry_read"
    }

    fn description(&self) -> &str {
        "retry safety test"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }

    async fn call(&self, _arguments: &str) -> Result<String, String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok("read".to_string())
    }
}

#[tokio::test]
async fn proxy_provider_does_not_block_the_async_runtime() {
    let holder: Arc<RwLock<Arc<dyn Provider>>> = Arc::new(RwLock::new(Arc::new(MockProvider)));
    let proxy = ProxyProvider::new(holder);

    let response = proxy
        .chat(nuo_wire::ModelRequest::new(Vec::new()))
        .await
        .unwrap();

    assert!(response.message.content.contains("mock AI"));
}

#[test]
fn context_overflow_detection() {
    let err = nuo_wire::ProviderError::new(
        "mock",
        nuo_wire::ProviderErrorKind::ContextOverflow,
        "context exceeded",
    );
    assert_eq!(
        err.kind(),
        nuo_wire::ProviderErrorKind::ContextOverflow
    );
}

/// Stands in for the transport: publishes known timings into whatever handle
/// the request carries, then reports failure on the first attempt and success on
/// the second — so one *turn* produces two attempts that must not share
/// telemetry.
struct TelemetryPublishingProvider(AtomicUsize);

#[async_trait]
impl Provider for TelemetryPublishingProvider {
    async fn chat(
        &self,
        _request: nuo_wire::ModelRequest,
    ) -> Result<nuo_wire::ProviderCompletion, nuo_wire::ProviderError> {
        Err(nuo_wire::ProviderError::new(
            "mock",
            nuo_wire::ProviderErrorKind::Other,
            "non-streaming path should not be used",
        ))
    }

    async fn stream_chat(
        &self,
        _request: nuo_wire::ModelRequest,
    ) -> Result<
        futures::stream::BoxStream<'static, Result<String, nuo_wire::ProviderError>>,
        nuo_wire::ProviderError,
    > {
        Ok(Box::pin(stream::empty()))
    }

    async fn stream_chat_events(
        &self,
        request: nuo_wire::ModelRequest,
    ) -> Result<
        futures::stream::BoxStream<
            'static,
            Result<ProviderStreamEvent, nuo_wire::ProviderError>,
        >,
        nuo_wire::ProviderError,
    > {
        // What the owned transport does: fill the handle it was handed, without
        // ever asking whose attempt it was serving.
        let cold = self.0.fetch_add(1, Ordering::SeqCst) == 0;
        request
            .transport_telemetry
            .publish(nuo_wire::TransportTimings {
                dns_us: cold.then_some(12_000),
                tls_us: cold.then_some(90_000),
                tcp_us: Some(if cold { 40_000 } else { 7_000 }),
                connected_us: Some(45_000),
                request_sent_us: Some(60_000),
                stream_ready_us: Some(120_000),
                rtt_us: Some(42_000),
                retransmits: if cold { 3 } else { 0 },
                observation: if cold {
                    nuo_wire::TransportObservation::ColdConnection
                } else {
                    nuo_wire::TransportObservation::PooledConnection
                },
                dispatch_at: Some(std::time::Instant::now()),
            });

        if cold {
            Ok(Box::pin(stream::iter(vec![
                Ok(ProviderStreamEvent::TextDelta("partial".to_string())),
                Err(nuo_wire::ProviderError::new(
                    "mock",
                    nuo_wire::ProviderErrorKind::RateLimited,
                    "rate limited",
                )
                .retryable(Some(1))),
            ])))
        } else {
            Ok(Box::pin(stream::iter(vec![
                Ok(ProviderStreamEvent::TextDelta("done".to_string())),
                Ok(ProviderStreamEvent::Completed(
                    nuo_wire::ProviderCompletionMeta::default(),
                )),
            ])))
        }
    }
}

/// ADR-0232: the transport's timings reach the attempt that caused them.
///
/// One turn, two attempts — the first fails transiently and is retried, and a
/// retry reuses the turn's assembled request. Each attempt must therefore carry
/// its *own* telemetry: the retry's `PooledConnection` may not overwrite the
/// first attempt's `ColdConnection`, and the first attempt's settled record may
/// not pick up the retry's numbers. That holds only because the handle is created
/// per attempt rather than inherited from the shared request.
#[tokio::test]
async fn each_attempt_carries_its_own_transport_telemetry_across_a_retry() {
    let directory =
        std::env::temp_dir().join(format!("nuo-telemetry-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&directory).expect("create test directory");
    let session = Arc::new(SessionStore::for_path(directory.join("session.json")));
    let agent = Arc::new(Agent::new(
        Arc::new(TelemetryPublishingProvider(AtomicUsize::new(0))),
        Vec::new(),
        nuo_harness::AgentIdentity::default(),
    ));
    let ledger = nuo_wire::TokenSourceLedger::shared();
    agent.install_token_ledger(ledger.clone());
    let (tx, mut rx) = mpsc::unbounded_channel();
    let session_id = session.id().await;

    execute_round(
        RoundContext {
            agent,
            tx,
            token: CancellationToken::new(),
            session_id: session_id.clone(),
            session: session.clone(),
            projection: ContextProjectionSettings {
                budget: nuo_wire::CompactionPolicy::default().resolve(100_000),
                preserve_rounds: 6,
                summarize: false,
                prune: false,
                prune_protect_tokens: 0,
            },
            retry_max_attempts: 3,
            retry_base_ms: 1,
            retry_max_ms: 10,
            emit_round_completed: false,
        },
        RoundInput {
            prompt: "hello".to_string(),
            hidden: false,
            display_prompt: None,
            sent_at_ms: None,
            images: Vec::new(),
            driver: nuo_harness::orchestration::RoundDriver::Fresh,
        },
    )
    .await
    .unwrap();
    while rx.try_recv().is_ok() {}

    let attempts = ledger.records_for_session(&session_id);
    assert_eq!(attempts.len(), 2, "one turn, two attempts: {attempts:#?}");

    let first = attempts[0]
        .performance
        .expect("the first attempt must keep its telemetry");
    assert_eq!(
        first.observation,
        nuo_wire::TransportObservation::ColdConnection,
        "the retry must not overwrite the first attempt's regime"
    );
    assert_eq!(first.tcp_us, Some(40_000));
    assert_eq!(first.retransmits, 3);
    assert!(first.tcp_info_sampled());
    // The offsets arrive anchored on the transport's dispatch and are re-anchored
    // onto this attempt's clock, so the ladder can only have advanced.
    let connected = first.connected_us.expect("connect instant");
    let sent = first.request_sent_us.expect("upload anchor");
    let ready = first.stream_ready_us.expect("head anchor");
    assert!(
        connected <= sent && sent <= ready,
        "the attempt's ladder must advance: {connected} → {sent} → {ready}"
    );

    let second = attempts[1]
        .performance
        .expect("the retry must keep its own telemetry");
    assert_eq!(
        second.observation,
        nuo_wire::TransportObservation::PooledConnection
    );
    assert_eq!(second.tcp_us, Some(7_000), "the retry's own TCP phase");
    assert_eq!(second.retransmits, 0, "the retry's own socket sample");
    assert_eq!(second.dns_us, None);

    let _ = std::fs::remove_dir_all(directory);
}

#[tokio::test]
async fn turn_retries_transient_provider_failure_before_tool_activity() {
    let directory = std::env::temp_dir().join(format!("nuo-retry-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&directory).expect("create test directory");
    let session = Arc::new(SessionStore::for_path(directory.join("session.json")));
    let agent = Arc::new(Agent::new(
        Arc::new(RetryOnceProvider(AtomicUsize::new(0))),
        Vec::new(),
        nuo_harness::AgentIdentity::default(),
    ));
    let ledger = nuo_wire::TokenSourceLedger::shared();
    agent.install_token_ledger(ledger.clone());
    let (tx, mut rx) = mpsc::unbounded_channel();
    let session_id = session.id().await;

    execute_round(
        RoundContext {
            agent,
            tx,
            token: CancellationToken::new(),
            session_id: session_id.clone(),
            session: session.clone(),
            projection: ContextProjectionSettings {
                budget: nuo_wire::CompactionPolicy::default().resolve(100_000),
                preserve_rounds: 6,
                summarize: false,
                prune: false,
                prune_protect_tokens: 0,
            },
            retry_max_attempts: 3,
            retry_base_ms: 1,
            retry_max_ms: 10,
            emit_round_completed: false,
        },
        RoundInput {
            prompt: "work".to_string(),
            hidden: false,
            display_prompt: None,
            sent_at_ms: None,
            images: Vec::new(),
            driver: nuo_harness::orchestration::RoundDriver::Fresh,
        },
    )
    .await
    .unwrap();
    assert!(
        session
            .model_window()
            .await
            .iter()
            .any(|message| message.content == "done")
    );
    let responses = std::iter::from_fn(|| rx.try_recv().ok()).collect::<Vec<_>>();
    let activities = responses
        .iter()
        .filter_map(|response| match response {
            AgentResponse::Round {
                event: RoundEvent::Activity(status),
                ..
            } => Some(status.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(activities.starts_with(&["saving request", "preparing context"]));
    assert_eq!(
        activities
            .iter()
            .filter(|status| **status == "waiting for model")
            .count(),
        2
    );
    assert_eq!(activities.last(), Some(&"saving response"));
    assert!(responses.iter().any(|response| matches!(
        response,
        AgentResponse::Round {
            event: RoundEvent::RetryScheduled {
                attempt: 2,
                max_attempts: 3,
                ..
            },
            ..
        }
    )));
    assert!(responses.iter().any(|response| matches!(
        response,
        AgentResponse::Round {
            event: RoundEvent::StreamDiscard,
            ..
        }
    )));
    let attempts = ledger.records_for_session(&session_id);
    assert_eq!(attempts.len(), 2);
    assert_eq!(attempts[0].key.attempt, 1);
    assert_eq!(
        attempts[0].status,
        nuo_wire::RequestUsageStatus::Failed
    );
    assert_eq!(attempts[1].key.attempt, 2);
    assert_eq!(
        attempts[1].status,
        nuo_wire::RequestUsageStatus::Completed
    );
    assert_eq!(session.request_usage_records().await, attempts);
    let _ = std::fs::remove_dir_all(directory);
}

/// ADR-0218: a real round archives one request projection into the
/// key-addressed archive, and that archive never enters the model window.
#[tokio::test]
async fn execute_round_archives_a_request_projection_outside_the_window() {
    let directory = std::env::temp_dir().join(format!("nuo-projection-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&directory).expect("create test directory");
    let session = Arc::new(SessionStore::for_path(directory.join("session.json")));
    let agent = Arc::new(Agent::new(
        Arc::new(InstantProvider),
        Vec::new(),
        nuo_harness::AgentIdentity::default(),
    ));
    let (tx, _rx) = mpsc::unbounded_channel();

    execute_round(
        RoundContext {
            agent,
            tx,
            token: CancellationToken::new(),
            session_id: session.id().await,
            session: session.clone(),
            projection: ContextProjectionSettings {
                budget: nuo_wire::CompactionPolicy::default().resolve(100_000),
                preserve_rounds: 6,
                summarize: false,
                prune: false,
                prune_protect_tokens: 0,
            },
            retry_max_attempts: 3,
            retry_base_ms: 1,
            retry_max_ms: 10,
            emit_round_completed: false,
        },
        RoundInput {
            prompt: "work".to_string(),
            hidden: false,
            display_prompt: None,
            sent_at_ms: None,
            images: Vec::new(),
            driver: nuo_harness::orchestration::RoundDriver::Fresh,
        },
    )
    .await
    .unwrap();

    // The archive insert is fire-and-forget; poll briefly for the writer.
    let mut projections = Vec::new();
    for _ in 0..200 {
        projections = session.request_projections().await;
        if !projections.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert_eq!(
        projections.len(),
        1,
        "one logical invocation must archive exactly one projection"
    );
    assert!(!projections[0].prefix_fingerprint.is_empty());
    assert!(projections[0].conversation_messages >= 1);

    // The archive is not history.
    let window = session.model_window().await;
    assert!(
        window
            .iter()
            .all(|m| !m.content.contains("temporary-context"))
    );
    let _ = std::fs::remove_dir_all(directory);
}

#[tokio::test]
async fn partial_tool_stream_is_not_executed_before_provider_retry() {
    let directory =
        std::env::temp_dir().join(format!("nuo-retry-partial-tool-{}", uuid::Uuid::new_v4()));
    let _ = std::fs::create_dir_all(&directory);
    let session = Arc::new(SessionStore::for_path(directory.join("session.json")));
    let tool_calls = Arc::new(AtomicUsize::new(0));
    let agent = Arc::new(Agent::new(
        Arc::new(PartialToolRetryProvider(AtomicUsize::new(0))),
        vec![Arc::new(RetryReadTool(tool_calls.clone()))],
        nuo_harness::AgentIdentity::default(),
    ));
    let (tx, mut rx) = mpsc::unbounded_channel();

    execute_round(
        RoundContext {
            agent,
            tx,
            token: CancellationToken::new(),
            session_id: session.id().await,
            session: session.clone(),
            projection: ContextProjectionSettings {
                budget: nuo_wire::CompactionPolicy::default().resolve(100_000),
                preserve_rounds: 6,
                summarize: false,
                prune: false,
                prune_protect_tokens: 0,
            },
            retry_max_attempts: 3,
            retry_base_ms: 1,
            retry_max_ms: 10,
            emit_round_completed: false,
        },
        RoundInput {
            prompt: "work".to_string(),
            hidden: false,
            display_prompt: None,
            sent_at_ms: None,
            images: Vec::new(),
            driver: nuo_harness::orchestration::RoundDriver::Fresh,
        },
    )
    .await
    .unwrap();
    assert_eq!(tool_calls.load(Ordering::SeqCst), 0);
    assert!(
        session
            .model_window()
            .await
            .iter()
            .any(|message| message.content == "done")
    );
    let responses = std::iter::from_fn(|| rx.try_recv().ok()).collect::<Vec<_>>();
    assert!(responses.iter().any(|response| matches!(
        response,
        AgentResponse::Round {
            event: RoundEvent::RetryScheduled { attempt: 2, .. },
            ..
        }
    )));
    assert!(!responses.iter().any(|response| matches!(
        response,
        AgentResponse::Round {
            event: RoundEvent::ToolCall { .. },
            ..
        }
    )));
    let _ = std::fs::remove_dir_all(directory);
}

#[tokio::test]
async fn turn_resumes_provider_request_after_completed_tool_activity() {
    let directory = std::env::temp_dir().join(format!("nuo-retry-tool-{}", uuid::Uuid::new_v4()));
    let _ = std::fs::create_dir_all(&directory);
    let session = Arc::new(SessionStore::for_path(directory.join("session.json")));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let tool_calls = Arc::new(AtomicUsize::new(0));
    let agent = Arc::new(Agent::new(
        Arc::new(ToolThenRetryProvider {
            attempts: AtomicUsize::new(0),
            requests: requests.clone(),
        }),
        vec![Arc::new(RetryReadTool(tool_calls.clone()))],
        nuo_harness::AgentIdentity::default(),
    ));
    let (tx, mut rx) = mpsc::unbounded_channel();

    execute_round(
        RoundContext {
            agent,
            tx,
            token: CancellationToken::new(),
            session_id: session.id().await,
            session: session.clone(),
            projection: ContextProjectionSettings {
                budget: nuo_wire::CompactionPolicy::default().resolve(100_000),
                preserve_rounds: 6,
                summarize: false,
                prune: false,
                prune_protect_tokens: 0,
            },
            retry_max_attempts: 4,
            retry_base_ms: 1,
            retry_max_ms: 10,
            emit_round_completed: false,
        },
        RoundInput {
            prompt: "work".to_string(),
            hidden: false,
            display_prompt: None,
            sent_at_ms: None,
            images: Vec::new(),
            driver: nuo_harness::orchestration::RoundDriver::Fresh,
        },
    )
    .await
    .unwrap();
    assert_eq!(tool_calls.load(Ordering::SeqCst), 1);
    assert!(
        session
            .model_window()
            .await
            .iter()
            .any(|message| message.content == "done")
    );
    let requests = requests.lock().expect("request log lock poisoned");
    assert_eq!(requests.len(), 4);
    assert_eq!(
        requests[1], requests[2],
        "the retry must resend the exact request checkpoint after the tool result"
    );
    assert!(
        std::iter::from_fn(|| rx.try_recv().ok()).any(|response| matches!(
            response,
            AgentResponse::Round {
                event: RoundEvent::RetryScheduled { attempt: 2, .. },
                ..
            }
        ))
    );
    let _ = std::fs::remove_dir_all(directory);
}

#[tokio::test]
async fn turn_exhaustion_message_explains_retry_budget() {
    let directory =
        std::env::temp_dir().join(format!("nuo-retry-exhaust-{}", uuid::Uuid::new_v4()));
    let _ = std::fs::create_dir_all(&directory);
    let session = Arc::new(SessionStore::for_path(directory.join("session.json")));
    let agent = Arc::new(Agent::new(
        Arc::new(AlwaysRetryableProvider),
        Vec::new(),
        nuo_harness::AgentIdentity::default(),
    ));
    let (tx, mut rx) = mpsc::unbounded_channel();

    let error = execute_round(
        RoundContext {
            agent,
            tx,
            token: CancellationToken::new(),
            session_id: session.id().await,
            session,
            projection: ContextProjectionSettings {
                budget: nuo_wire::CompactionPolicy::default().resolve(100_000),
                preserve_rounds: 6,
                summarize: false,
                prune: false,
                prune_protect_tokens: 0,
            },
            retry_max_attempts: 3,
            retry_base_ms: 1,
            retry_max_ms: 10,
            emit_round_completed: false,
        },
        RoundInput {
            prompt: "work".to_string(),
            hidden: false,
            display_prompt: None,
            sent_at_ms: None,
            images: Vec::new(),
            driver: nuo_harness::orchestration::RoundDriver::Fresh,
        },
    )
    .await
    .unwrap_err();

    let error_string = error.to_string();
    assert!(
        error_string.starts_with("OpenAI HTTP 429 Too Many Requests"),
        "should surface the provider message: {error_string}"
    );
    // All attempts but the last must announce a retry; the final failure
    // surfaces as the error above instead.
    let scheduled = std::iter::from_fn(|| rx.try_recv().ok())
        .filter(|response| {
            matches!(
                response,
                AgentResponse::Round {
                    event: RoundEvent::RetryScheduled { .. },
                    ..
                }
            )
        })
        .count();
    assert_eq!(
        scheduled, 2,
        "should schedule retries for every attempt before giving up"
    );
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn retry_delay_honors_headers_and_stepped_bounds() {
    // Stepped backoff: 1s -> 2s -> 5s -> 10s -> 10s -> continuously 10s
    assert_eq!(retry_delay_ms(1, None, 1_000, 30_000), 1_000);
    assert_eq!(retry_delay_ms(2, None, 1_000, 30_000), 2_000);
    assert_eq!(retry_delay_ms(3, None, 1_000, 30_000), 5_000);
    assert_eq!(retry_delay_ms(4, None, 1_000, 30_000), 10_000);
    assert_eq!(retry_delay_ms(5, None, 1_000, 30_000), 10_000);
    assert_eq!(retry_delay_ms(10, None, 1_000, 30_000), 10_000);
    // Server retry-after header takes priority (bounded by base and max)
    assert_eq!(retry_delay_ms(2, Some(45_000), 1_000, 30_000), 30_000);
    assert_eq!(retry_delay_ms(1, Some(0), 1_000, 30_000), 1_000);
    // Capped by max_ms
    assert_eq!(retry_delay_ms(3, None, 1_000, 3_000), 3_000);
}

#[test]
fn apply_jitter_stays_within_half_to_full_range() {
    // Equal jitter: result ∈ [base/2, base]. A roll of 0 yields the floor,
    // a roll of the full span yields the ceiling — both bounds are closed.
    assert_eq!(apply_jitter_ms(1_000, |_| 0), 500);
    assert_eq!(apply_jitter_ms(1_000, |span| span), 1_000);
    // A mid-range roll lands exactly halfway between floor and ceiling.
    assert_eq!(apply_jitter_ms(1_000, |span| span / 2), 750);
    // Odd base still floors cleanly: [1500/2 .. 1500].
    assert_eq!(apply_jitter_ms(1_500, |_| 0), 750);
    assert_eq!(apply_jitter_ms(1_500, |span| span), 1_500);
}

#[test]
fn apply_jitter_never_exceeds_base() {
    // A pathological roll larger than the span must be clamped back to the
    // ceiling, so jitter can never push a delay past the configured cap.
    assert_eq!(apply_jitter_ms(1_000, |_| u64::MAX), 1_000);
}

#[test]
fn apply_jitter_passes_zero_through_unchanged() {
    assert_eq!(apply_jitter_ms(0, |_| 1_000), 0);
}

/// A provider that fails *terminally* (a non-retryable error) on the first
/// round and succeeds on the second — the exact shape a `/retry` resumes
/// from. `requests` records the message-history shape of every provider
/// request so the test can assert the resume re-sent exactly the committed
/// checkpoint (no duplicate user message, no half-streamed assistant text).
struct FailThenSucceedProvider {
    attempts: AtomicUsize,
    requests: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl Provider for FailThenSucceedProvider {
    async fn chat(
        &self,
        _request: nuo_wire::ModelRequest,
    ) -> Result<nuo_wire::ProviderCompletion, nuo_wire::ProviderError> {
        Err(nuo_wire::ProviderError::new(
            "mock",
            nuo_wire::ProviderErrorKind::Other,
            "non-streaming path should not be used",
        ))
    }

    async fn stream_chat(
        &self,
        _request: nuo_wire::ModelRequest,
    ) -> Result<
        futures::stream::BoxStream<'static, Result<String, nuo_wire::ProviderError>>,
        nuo_wire::ProviderError,
    > {
        Ok(Box::pin(stream::empty()))
    }

    async fn stream_chat_events(
        &self,
        request: nuo_wire::ModelRequest,
    ) -> Result<
        futures::stream::BoxStream<
            'static,
            Result<ProviderStreamEvent, nuo_wire::ProviderError>,
        >,
        nuo_wire::ProviderError,
    > {
        self.requests
            .lock()
            .expect("request log lock poisoned")
            .push(provider_history_snapshot(&request.messages));
        if self.attempts.fetch_add(1, Ordering::SeqCst) == 0 {
            // Terminal: `parse_retryable_error` finds no envelope, so the
            // harness surfaces it and (with ADR-0128) arms the resume point.
            Ok(Box::pin(stream::iter(vec![Err(
                nuo_wire::ProviderError::new(
                    "mock",
                    nuo_wire::ProviderErrorKind::Other,
                    "terminal: model refused the request",
                ),
            )])))
        } else {
            Ok(Box::pin(stream::iter(vec![
                Ok(ProviderStreamEvent::TextDelta("recovered".to_string())),
                Ok(ProviderStreamEvent::Completed(
                    nuo_wire::ProviderCompletionMeta::default(),
                )),
            ])))
        }
    }
}

/// The round-level `/retry` contract (ADR-0128): a round whose provider
/// fails terminally parks a durable resume point, and resuming through it
/// *continues the same round* — the round counter never advances, no second
/// user message is appended, and the turn sequence numbers onward instead of
/// restarting. Meanwhile a round that completed naturally leaves no point,
/// so a second `/retry` has nothing to resume.
#[tokio::test]
async fn retry_resumes_stopped_round_without_breaking_turn_sequence() {
    let directory =
        std::env::temp_dir().join(format!("nuo-retry-resume-{}", uuid::Uuid::new_v4()));
    let _ = std::fs::create_dir_all(&directory);
    let session = Arc::new(SessionStore::for_path(directory.join("session.json")));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let agent = Arc::new(Agent::new(
        Arc::new(FailThenSucceedProvider {
            attempts: AtomicUsize::new(0),
            requests: requests.clone(),
        }),
        Vec::new(),
        nuo_harness::AgentIdentity::default(),
    ));
    let (tx, _rx) = mpsc::unbounded_channel();
    let session_id = session.id().await;
    let context = |tx| RoundContext {
        agent: Arc::clone(&agent),
        tx,
        token: CancellationToken::new(),
        session_id: session_id.clone(),
        session: Arc::clone(&session),
        projection: ContextProjectionSettings {
            budget: nuo_wire::CompactionPolicy::default().resolve(100_000),
            preserve_rounds: 6,
            summarize: false,
            prune: false,
            prune_protect_tokens: 0,
        },
        retry_max_attempts: 3,
        retry_base_ms: 1,
        retry_max_ms: 10,
        emit_round_completed: false,
    };

    // 1. The fresh round fails terminally.
    let error = execute_round(
        context(tx.clone()),
        RoundInput {
            prompt: "work".to_string(),
            hidden: false,
            display_prompt: None,
            sent_at_ms: None,
            images: Vec::new(),
            driver: nuo_harness::orchestration::RoundDriver::Fresh,
        },
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("terminal"), "{error}");

    // The failed round parked its resume point: same round, committed
    // history watermark, zero committed turns (nothing streamed through).
    let point = session
        .retry_pending()
        .await
        .expect("failed round must arm a retry point");
    assert_eq!(point.round, 1, "the parked point names round 1");
    assert_eq!(point.turns_committed, 0);
    assert_eq!(point.history_watermark, 1, "user message only");
    assert_eq!(session.round_counter().await, 1);

    // 2. `/retry` resumes and completes the *same* round.
    let round_before = session.round_counter().await;
    execute_round(
        context(tx),
        RoundInput::resume(nuo_wire::RetryPoint {
            round: point.round,
            turns_committed: point.turns_committed,
            history_watermark: point.history_watermark,
            paused_ms: point.paused_ms,
            at_ms: point.at_ms,
        }),
    )
    .await
    .unwrap();

    // Round counter never advanced: the resume completed round 1, it did
    // not mint round 2. This is the headline invariant of /retry.
    assert_eq!(
        session.round_counter().await,
        round_before,
        "retry must not advance the round counter"
    );
    // The window holds user + assistant — exactly one user message (no
    // duplicate from the resume re-sending it as a fresh prompt).
    let window = session.model_window().await;
    let user_count = window
        .iter()
        .filter(|message| message.role == Role::User)
        .count();
    assert_eq!(
        user_count, 1,
        "resume must not append a second user message"
    );
    assert!(
        window.iter().any(|message| message.content == "recovered"),
        "the resumed round's answer is committed"
    );
    // Both provider requests carried the same semantic history. The system
    // prompt is projected afresh for each request and its local diagnostic
    // timestamp may advance; timestamp is not provider protocol content and
    // is removed by the request recorder above.
    let logged = requests.lock().expect("request log lock poisoned").clone();
    assert_eq!(logged.len(), 2);
    assert_eq!(
        logged[0], logged[1],
        "resume re-sends the committed checkpoint, not a mutated history"
    );

    // 3. Completion retired the point: a subsequent `/retry` has nothing
    //    to resume (the completed-round rule).
    assert!(
        session.retry_pending().await.is_none(),
        "a naturally completed round leaves no retry point"
    );

    let _ = std::fs::remove_dir_all(directory);
}

// Round-interrupt projection: only a genuinely stopped round may leave a
// durable `RoundInterrupt` record. Two regressions are pinned here:
//
// 1. Stop sites park their reason unconditionally — even while idle — so a
//    reason parked with no live round must not leak into the next round and
//    label a naturally completed round as "interrupted · <reason>".
// 2. An Esc Esc landing after the round passed its last cancellation
//    checkpoint (the model already converged, the history already committed)
//    parks a reason without changing the outcome. The completed round must
//    not be re-labelled as an interrupt.

/// A provider whose stream never terminates until the test cancels the
/// round token — the "model is still generating" state an Esc Esc lands in.
struct HangingProvider;

#[async_trait]
impl Provider for HangingProvider {
    async fn chat(
        &self,
        _request: nuo_wire::ModelRequest,
    ) -> Result<nuo_wire::ProviderCompletion, nuo_wire::ProviderError> {
        Err(nuo_wire::ProviderError::new(
            "mock",
            nuo_wire::ProviderErrorKind::Other,
            "chat is not used by the streaming path",
        ))
    }
    async fn stream_chat(
        &self,
        _request: nuo_wire::ModelRequest,
    ) -> Result<
        futures::stream::BoxStream<'static, Result<String, nuo_wire::ProviderError>>,
        nuo_wire::ProviderError,
    > {
        Ok(Box::pin(futures::stream::pending()))
    }
}

/// A provider that answers immediately — the "model converged" state.
struct InstantProvider;

#[async_trait]
impl Provider for InstantProvider {
    async fn chat(
        &self,
        _request: nuo_wire::ModelRequest,
    ) -> Result<nuo_wire::ProviderCompletion, nuo_wire::ProviderError> {
        Ok(nuo_wire::ProviderCompletion::message(Message::new(
            Role::Assistant,
            "done",
        )))
    }
    async fn stream_chat(
        &self,
        _request: nuo_wire::ModelRequest,
    ) -> Result<
        futures::stream::BoxStream<'static, Result<String, nuo_wire::ProviderError>>,
        nuo_wire::ProviderError,
    > {
        Ok(Box::pin(stream::iter([Ok("done".to_string())])))
    }
}

/// A provider that streams one text delta, then *finishes on a delay* —
/// closing the stream (the terminal event a real provider sends after its
/// last delta + usage chunk) only after `settle_ms`. This is the exact shape
/// of the end-of-answer race: the answer's content has all arrived, but the
/// stream's `Ok(None)` terminator is still in flight when the user's next
/// message (or Esc Esc) lands. Whether the round completes or unwinds as
/// `Interrupted` is decided by the finish-drain window, not by which signal
/// happened to poll first.
struct SettlingProvider {
    settle_ms: u64,
}

#[async_trait]
impl Provider for SettlingProvider {
    async fn chat(
        &self,
        _request: nuo_wire::ModelRequest,
    ) -> Result<nuo_wire::ProviderCompletion, nuo_wire::ProviderError> {
        Err(nuo_wire::ProviderError::new(
            "mock",
            nuo_wire::ProviderErrorKind::Other,
            "chat is not used by the streaming path",
        ))
    }
    async fn stream_chat(
        &self,
        _request: nuo_wire::ModelRequest,
    ) -> Result<
        futures::stream::BoxStream<'static, Result<String, nuo_wire::ProviderError>>,
        nuo_wire::ProviderError,
    > {
        let settle_ms = self.settle_ms;
        Ok(Box::pin(stream::unfold(0u8, move |state| async move {
            match state {
                0 => Some((Ok("done".to_string()), 1)),
                // Terminal: close the stream shortly after the delta.
                1 => {
                    tokio::time::sleep(std::time::Duration::from_millis(settle_ms)).await;
                    None
                }
                _ => None,
            }
        })))
    }
}

/// A provider that streams one delta and then goes **silent forever** — an
/// answer that was *not* settling when the cancel landed. The finish-drain
/// window must expire and honour the interrupt.
struct TrickleThenSilentProvider;

#[async_trait]
impl Provider for TrickleThenSilentProvider {
    async fn chat(
        &self,
        _request: nuo_wire::ModelRequest,
    ) -> Result<nuo_wire::ProviderCompletion, nuo_wire::ProviderError> {
        Err(nuo_wire::ProviderError::new(
            "mock",
            nuo_wire::ProviderErrorKind::Other,
            "chat is not used by the streaming path",
        ))
    }
    async fn stream_chat(
        &self,
        _request: nuo_wire::ModelRequest,
    ) -> Result<
        futures::stream::BoxStream<'static, Result<String, nuo_wire::ProviderError>>,
        nuo_wire::ProviderError,
    > {
        Ok(Box::pin(
            stream::once(async { Ok("partial answer".to_string()) }).chain(stream::pending()),
        ))
    }
}

/// A provider whose single stream item is gated: it signals `started` when
/// the round reaches the model request, then holds the stream open until
/// `release` fires, and finally converges ("done", no tool calls). This lets
/// a test park an interrupt reason at a chosen instant *while the round is
/// live* and then let the round complete anyway — the exact shape of an Esc
/// Esc that lands too late to change the outcome.
struct GatedProvider {
    started: tokio::sync::mpsc::UnboundedSender<()>,
    release: Arc<tokio::sync::Notify>,
}

#[async_trait]
impl Provider for GatedProvider {
    async fn chat(
        &self,
        _request: nuo_wire::ModelRequest,
    ) -> Result<nuo_wire::ProviderCompletion, nuo_wire::ProviderError> {
        Err(nuo_wire::ProviderError::new(
            "mock",
            nuo_wire::ProviderErrorKind::Other,
            "chat is not used by the streaming path",
        ))
    }
    async fn stream_chat(
        &self,
        _request: nuo_wire::ModelRequest,
    ) -> Result<
        futures::stream::BoxStream<'static, Result<String, nuo_wire::ProviderError>>,
        nuo_wire::ProviderError,
    > {
        let started = self.started.clone();
        let release = Arc::clone(&self.release);
        Ok(Box::pin(stream::once(async move {
            let _ = started.send(());
            release.notified().await;
            Ok("done".to_string())
        })))
    }
}

/// Shared scaffolding: build a session + agent + channel for one interactive
/// round through `start_interactive_round` (the production entry whose tail
/// owns the interrupt-record decision). The caller parks/releases via the
/// returned lifecycle and channel.
struct InteractiveRoundFixture {
    session: Arc<SessionStore>,
    rx: mpsc::UnboundedReceiver<AgentResponse>,
    lifecycle: Arc<RoundLifecycle>,
    agent: Arc<Agent>,
    directory: std::path::PathBuf,
}

async fn interactive_round_fixture(provider: Arc<dyn Provider>) -> InteractiveRoundFixture {
    let directory = std::env::temp_dir().join(format!("nuo-intr-{}", uuid::Uuid::new_v4()));
    let _ = std::fs::create_dir_all(&directory);
    let session = Arc::new(SessionStore::for_path(directory.join("session.json")));
    let agent = Arc::new(Agent::new(
        provider,
        Vec::new(),
        nuo_harness::AgentIdentity::default(),
    ));
    let lifecycle = Arc::new(RoundLifecycle::new());
    let (tx, rx) = mpsc::unbounded_channel();
    let session_id = session.id().await;
    start_interactive_round(
        InteractiveRoundContext {
            agent: Arc::clone(&agent),
            tx,
            lifecycle: Arc::clone(&lifecycle),
            session: Arc::clone(&session),
            session_id,
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
        },
        RoundInput {
            prompt: "hello".to_string(),
            hidden: false,
            display_prompt: None,
            sent_at_ms: None,
            images: Vec::new(),
            driver: nuo_harness::orchestration::RoundDriver::Fresh,
        },
    )
    .await;
    InteractiveRoundFixture {
        session,
        rx,
        lifecycle,
        agent,
        directory,
    }
}

/// Drain the channel until a specific event kind appears (bounded wait so a
/// broken round fails the test instead of hanging it).
async fn next_event_where(
    rx: &mut mpsc::UnboundedReceiver<AgentResponse>,
    mut predicate: impl FnMut(&RoundEvent) -> bool,
) -> Option<RoundEvent> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while std::time::Instant::now() < deadline {
        match tokio::time::timeout(std::time::Duration::from_millis(500), rx.recv()).await {
            Ok(Some(AgentResponse::Round { event, .. })) if predicate(&event) => {
                return Some(event);
            }
            Ok(Some(_)) => continue,
            Ok(None) => return None,
            Err(_elapsed) => continue,
        }
    }
    None
}

#[tokio::test]
async fn idle_parked_interrupt_reason_does_not_label_the_next_round() {
    // Regression 1: Esc Esc / a session switch parks a reason while idle
    // (no live round). The next round completes naturally and must leave NO
    // interrupt record and NO RoundInterrupted event.
    let InteractiveRoundFixture {
        session,
        mut rx,
        lifecycle,
        agent,
        directory,
    } = interactive_round_fixture(Arc::new(InstantProvider)).await;

    // Park as the interrupt handler does while idle, then wait for the
    // round's natural completion.
    lifecycle.record_interrupt(nuo_wire::RoundInterruptReason::User);
    let completed = next_event_where(&mut rx, |event| {
        matches!(event, RoundEvent::RoundCompleted(_))
    })
    .await;
    assert!(completed.is_some(), "round must complete");

    // Give the tail (which runs after RoundCompleted) a moment, then verify
    // no interrupt was recorded or emitted.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(
        session.round_interrupts().await.is_empty(),
        "a naturally completed round must not leave an interrupt record"
    );
    let leaked = std::iter::from_fn(|| rx.try_recv().ok()).any(|response| {
        matches!(
            response,
            AgentResponse::Round {
                event: RoundEvent::RoundInterrupted(_),
                ..
            }
        )
    });
    assert!(
        !leaked,
        "no RoundInterrupted event may follow a natural completion"
    );
    assert_eq!(
        lifecycle.take_interrupt(),
        None,
        "the tail consumed the stale park; nothing may leak further"
    );
    assert_eq!(agent.round_count(), 1);
    let _ = std::fs::remove_dir_all(directory);
}

#[tokio::test]
async fn late_interrupt_while_live_then_completes_is_not_recorded() {
    // Regression 2 (deterministic): the round is live and streaming when
    // the Esc Esc parks its reason, but the cancellation arrives after the
    // last checkpoint — the token is never observed before the model
    // converges, so the round completes. The tail must not write a record
    // for a round that succeeded.
    let release = Arc::new(tokio::sync::Notify::new());
    let (started_tx, mut started_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let provider = Arc::new(GatedProvider {
        started: started_tx,
        release: Arc::clone(&release),
    });
    let InteractiveRoundFixture {
        session,
        mut rx,
        lifecycle,
        agent,
        directory,
    } = interactive_round_fixture(provider).await;

    // Wait until the model request is actually in flight.
    started_rx
        .recv()
        .await
        .expect("round must reach the model request");

    // Esc Esc lands now — the reason is parked while the round is live...
    lifecycle.record_interrupt(nuo_wire::RoundInterruptReason::User);
    // ...but this Esc Esc is modeled as arriving too late: no token
    // cancellation is observed before convergence (we simply release the
    // stream). This is the "server-side LLM converged and the round
    // finished normally" completion.
    release.notify_one();

    let completed = next_event_where(&mut rx, |event| {
        matches!(event, RoundEvent::RoundCompleted(_))
    })
    .await;
    assert!(completed.is_some(), "round must still complete");
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(
        session.round_interrupts().await.is_empty(),
        "a round that completed despite a late parked reason must not gain an interrupt record"
    );
    assert_eq!(agent.round_count(), 1);
    let _ = std::fs::remove_dir_all(directory);
}

#[tokio::test]
async fn real_interrupt_of_a_live_round_still_records() {
    // Control: a genuine mid-generation Esc Esc must keep its record — the
    // fix must not swallow real interrupts.
    let InteractiveRoundFixture {
        session,
        mut rx,
        lifecycle,
        directory,
        ..
    } = interactive_round_fixture(Arc::new(HangingProvider)).await;

    // Wait until the round is actually streaming (it admitted the prompt),
    // then cancel exactly like the interrupt handler.
    let started = next_event_where(&mut rx, |event| {
        matches!(event, RoundEvent::TurnStarted { .. })
    })
    .await;
    assert!(started.is_some(), "round must start");
    lifecycle.record_interrupt(nuo_wire::RoundInterruptReason::User);
    lifecycle.cancel_current().await;

    let interrupted = next_event_where(&mut rx, |event| {
        matches!(event, RoundEvent::RoundInterrupted(_))
    })
    .await;
    assert!(
        interrupted.is_some(),
        "a genuinely interrupted round emits its RoundInterrupted event"
    );
    let records = session.round_interrupts().await;
    assert_eq!(records.len(), 1, "exactly one durable record: {records:?}");
    assert_eq!(
        records[0].reason,
        nuo_wire::RoundInterruptReason::User
    );
    // The HangingProvider round was interrupted before model output.
    // The prompt is preserved in session history (never retracted) and records
    // its admitted round number (round 1 for the first round).
    assert_eq!(records[0].round, Some(1));
    let messages = session.model_window().await;
    assert!(
        messages
            .iter()
            .any(|m| m.role == nuo_wire::Role::User),
        "sent prompt is preserved in transcript"
    );
    let _ = std::fs::remove_dir_all(directory);
}

#[tokio::test]
async fn superseded_live_round_still_records() {
    // Control: a supersede (new message replacing a live round) still
    // records its reason via the generation-suppressed arm.
    let InteractiveRoundFixture {
        session,
        mut rx,
        lifecycle,
        directory,
        ..
    } = interactive_round_fixture(Arc::new(HangingProvider)).await;

    let started = next_event_where(&mut rx, |event| {
        matches!(event, RoundEvent::TurnStarted { .. })
    })
    .await;
    assert!(started.is_some(), "round must start");

    // Mirror the replacement path: park, bump the generation, cancel.
    lifecycle.record_interrupt(nuo_wire::RoundInterruptReason::Superseded);
    lifecycle.supersede();
    lifecycle.cancel_current().await;

    let interrupted = next_event_where(&mut rx, |event| {
        matches!(event, RoundEvent::RoundInterrupted(_))
    })
    .await;
    assert!(
        interrupted.is_some(),
        "a superseded round still records why it died"
    );
    let records = session.round_interrupts().await;
    assert_eq!(records.len(), 1, "exactly one durable record: {records:?}");
    assert_eq!(
        records[0].reason,
        nuo_wire::RoundInterruptReason::Superseded
    );
    let _ = std::fs::remove_dir_all(directory);
}

// The end-of-answer supersede race (the false "▲ interrupted · new message"
// over a round that finished): the model's final delta has arrived and only
// the stream terminator is in flight when the next message lands.

#[tokio::test]
async fn late_supersede_on_a_settling_stream_completes_the_round() {
    // The answer's content fully arrived; the stream closes 100 ms later. A
    // new message cancelling the round inside that window must NOT unwind it
    // as interrupted — the user watched the answer finish, so the round
    // commits and no marker is recorded.
    let InteractiveRoundFixture {
        session,
        mut rx,
        lifecycle,
        agent,
        directory,
    } = interactive_round_fixture(Arc::new(SettlingProvider { settle_ms: 100 })).await;

    let delta =
        next_event_where(&mut rx, |event| matches!(event, RoundEvent::StreamDelta(_))).await;
    assert!(delta.is_some(), "round must stream its answer delta");

    // The user sends the next message: park + cancel exactly as
    // `start_interactive_round`'s replacement arm does.
    lifecycle.record_interrupt(nuo_wire::RoundInterruptReason::Superseded);
    lifecycle.cancel_current().await;

    let outcome = next_event_where(&mut rx, |event| {
        matches!(
            event,
            RoundEvent::RoundCompleted(_) | RoundEvent::RoundInterrupted(_)
        )
    })
    .await;
    match outcome {
        Some(RoundEvent::RoundCompleted(_)) => {}
        other => panic!("a fully streamed answer must complete, got {other:?}"),
    }
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert!(
        session.round_interrupts().await.is_empty(),
        "no interrupt record for a round that committed its answer"
    );
    assert_eq!(agent.round_count(), 1);
    let _ = std::fs::remove_dir_all(directory);
}

#[tokio::test]
async fn supersede_on_a_silent_stream_still_interrupts() {
    // Control: a delta arrived but the stream then went silent — the answer
    // was NOT settling when the cancel landed. The drain window expires and
    // the interrupt stands (a genuinely unfinished answer is interrupted
    // work). Parked at the stop moment, the record must still be written.
    let InteractiveRoundFixture {
        session,
        mut rx,
        lifecycle,
        directory,
        ..
    } = interactive_round_fixture(Arc::new(TrickleThenSilentProvider)).await;

    let delta =
        next_event_where(&mut rx, |event| matches!(event, RoundEvent::StreamDelta(_))).await;
    assert!(delta.is_some(), "round must stream its first delta");

    lifecycle.record_interrupt(nuo_wire::RoundInterruptReason::Superseded);
    lifecycle.cancel_current().await;

    let interrupted = next_event_where(&mut rx, |event| {
        matches!(event, RoundEvent::RoundInterrupted(_))
    })
    .await;
    assert!(
        interrupted.is_some(),
        "a stream that never settles is genuinely interrupted"
    );
    let records = session.round_interrupts().await;
    assert_eq!(records.len(), 1, "exactly one durable record: {records:?}");
    assert_eq!(
        records[0].reason,
        nuo_wire::RoundInterruptReason::Superseded
    );
    assert_eq!(
        records[0].detail.as_deref(),
        None,
        "clean stop carries no error detail and does not bloat records with abandoned drafts"
    );
    let _ = std::fs::remove_dir_all(directory);
}

#[tokio::test]
async fn supersede_record_is_stamped_at_the_message_send_time() {
    // Seam regression: the marker's `at_ms` must not postdate the superseding
    // message's `sent_at_ms`, or the resume merge drops the marker below the
    // newer round's answer — reading as an interrupt of a round that
    // completed normally. The park API is exercised directly here (the full
    // replacement flow is runtime-level); what it must guarantee is that the
    // explicit send time wins over the park-moment clock.
    let lifecycle = RoundLifecycle::new();
    lifecycle.begin().await;
    lifecycle.record_interrupt_at(
        nuo_wire::RoundInterruptReason::Superseded,
        Some(42_000),
    );
    let parked = lifecycle
        .take_interrupt()
        .expect("park survives until taken");
    assert_eq!(parked.at_ms, 42_000);
    assert!(
        parked.at_ms < unix_epoch_ms_for_test(),
        "an explicit send time must win over the wall clock"
    );
}

/// Wall clock for [`supersede_record_is_stamped_at_the_message_send_time`]:
/// guaranteed past the fixed send time the test parks.
fn unix_epoch_ms_for_test() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(u64::MAX)
}

// ─────────────────────────────────────────────────────────────────────────────
// Route image refusal (ADR-0230)
// ─────────────────────────────────────────────────────────────────────────────

/// A provider that refuses the first N requests the way a relay that cannot take
/// attachments does, then serves every later request — while recording how many
/// attachments each request carried.
///
/// `rejection` is the vendor's error text. The harness never reads it: the
/// recovery is decided by the *outcome* of retrying without the attachments
/// (ADR-0230), so these tests can use prose the harness has no knowledge of.
struct ImageRejectingProvider {
    /// Attachment count seen per request, in order. Its length is also the
    /// attempt counter.
    images_per_request: Arc<Mutex<Vec<usize>>>,
    rejection: &'static str,
    /// How many requests to refuse; `usize::MAX` for a route that never works.
    rejections: usize,
}

#[async_trait]
impl Provider for ImageRejectingProvider {
    async fn chat(
        &self,
        _request: nuo_wire::ModelRequest,
    ) -> Result<nuo_wire::ProviderCompletion, nuo_wire::ProviderError> {
        Err(nuo_wire::ProviderError::new(
            "mock",
            nuo_wire::ProviderErrorKind::Other,
            "non-streaming path should not be used",
        ))
    }

    async fn stream_chat(
        &self,
        _request: nuo_wire::ModelRequest,
    ) -> Result<
        futures::stream::BoxStream<'static, Result<String, nuo_wire::ProviderError>>,
        nuo_wire::ProviderError,
    > {
        Ok(Box::pin(stream::empty()))
    }

    async fn stream_chat_events(
        &self,
        request: nuo_wire::ModelRequest,
    ) -> Result<
        futures::stream::BoxStream<
            'static,
            Result<ProviderStreamEvent, nuo_wire::ProviderError>,
        >,
        nuo_wire::ProviderError,
    > {
        let images = request
            .messages
            .iter()
            .filter_map(|message| message.images.as_ref().map(Vec::len))
            .sum::<usize>();
        let attempt = {
            let mut log = self
                .images_per_request
                .lock()
                .expect("image log lock poisoned");
            log.push(images);
            log.len()
        };
        if attempt <= self.rejections {
            let error = nuo_wire::ProviderError::new(
                "mock",
                nuo_wire::ProviderErrorKind::InvalidRequest,
                self.rejection,
            )
            .with_status(400);
            return Ok(Box::pin(stream::iter(vec![Err(error)])));
        }
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderStreamEvent::TextDelta("read it".to_string())),
            Ok(ProviderStreamEvent::Completed(
                nuo_wire::ProviderCompletionMeta::default(),
            )),
        ])))
    }
}

/// Switching to a model that cannot see images must not brick the session
/// (ADR-0230).
///
/// The durable transcript keeps the image the user pasted while a multimodal
/// model was active — history is append-only (ADR-0186), so no projection may
/// rewrite it. What changes is the *request*: the refusal teaches the harness
/// that this route cannot take attachments (because withholding them made the
/// identical turn succeed), and later requests stop carrying them.
#[tokio::test]
async fn image_refusal_is_learned_and_the_round_continues_without_images() {
    let directory = std::env::temp_dir().join(format!("nuo-image-learn-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&directory).expect("create test directory");
    let session = Arc::new(SessionStore::for_path(directory.join("session.json")));
    let images_per_request = Arc::new(Mutex::new(Vec::new()));
    let agent = Arc::new(Agent::new(
        Arc::new(ImageRejectingProvider {
            images_per_request: Arc::clone(&images_per_request),
            rejection: "mock HTTP 400: unknown variant `image_url`, expected `text`",
            rejections: 1,
        }),
        Vec::new(),
        nuo_harness::AgentIdentity::default(),
    ));
    let (tx, mut rx) = mpsc::unbounded_channel();
    let session_id = session.id().await;
    let images = vec![nuo_wire::ImagePart {
        mime: "image/png".to_string(),
        data: "aGk=".to_string(),
    }];

    execute_round(
        image_round_context(&agent, tx.clone(), &session, &session_id),
        image_round_input("what is in this picture?", images.clone()),
    )
    .await
    .expect("the round must recover, not fail");

    // Attempt 1 carried the image; the learned retry carried none.
    assert_eq!(
        *images_per_request.lock().expect("image log lock poisoned"),
        vec![1, 0],
        "attempt 1 carries the attachment, the probed retry carries none"
    );

    // The image is still in the durable window: withholding is a projection,
    // never a history rewrite.
    let window = session.model_window().await;
    assert!(
        window.iter().any(|message| message
            .images
            .as_ref()
            .is_some_and(|images| !images.is_empty())),
        "the transcript must keep the pasted image: {window:?}"
    );

    // The user is told what happened, and how to override it.
    let notices = std::iter::from_fn(|| rx.try_recv().ok())
        .filter_map(|response| match response {
            AgentResponse::Round {
                event: RoundEvent::Notice(notice),
                ..
            } => Some(notice),
            _ => None,
        })
        .filter(|notice| notice.kind == nuo_wire::NoticeKind::ImageInputWithheld)
        .collect::<Vec<_>>();
    assert_eq!(notices.len(), 1, "exactly one withholding notice");
    assert!(
        notices[0]
            .body
            .as_deref()
            .is_some_and(|body| body.contains("history") && body.contains("model editor")),
        "the notice must explain that history keeps the image and name the override: {:?}",
        notices[0].body
    );
    assert!(agent.images_withheld_from_current_route());

    // A second round on the same route must not walk into the wall again.
    execute_round(
        image_round_context(&agent, tx, &session, &session_id),
        image_round_input("and now, in words?", Vec::new()),
    )
    .await
    .expect("a latched route must complete without another refusal");

    assert_eq!(
        *images_per_request.lock().expect("image log lock poisoned"),
        vec![1, 0, 0],
        "the second round sends one request, already without the history's image"
    );
}

/// Recognition must not depend on any vendor's error format (ADR-0230).
///
/// This provider refuses with prose the harness has no knowledge of — the shape
/// of every relay whose wording nobody anticipated. The recovery rests entirely
/// on the differential probe: retry the identical turn without the attachments
/// and observe the outcome. Recognition by *experiment*, not by reading the
/// vendor's envelope.
#[tokio::test]
async fn an_unrecognized_refusal_is_probed_and_confirmed_by_outcome() {
    let directory = std::env::temp_dir().join(format!("nuo-image-probe-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&directory).expect("create test directory");
    let session = Arc::new(SessionStore::for_path(directory.join("session.json")));
    let images_per_request = Arc::new(Mutex::new(Vec::new()));
    let agent = Arc::new(Agent::new(
        Arc::new(ImageRejectingProvider {
            images_per_request: Arc::clone(&images_per_request),
            rejection: "mock HTTP 400: unsupported content type for the second message part",
            rejections: 1,
        }),
        Vec::new(),
        nuo_harness::AgentIdentity::default(),
    ));
    let (tx, mut rx) = mpsc::unbounded_channel();
    let session_id = session.id().await;

    execute_round(
        image_round_context(&agent, tx, &session, &session_id),
        image_round_input(
            "describe it",
            vec![nuo_wire::ImagePart {
                mime: "image/png".to_string(),
                data: "aGk=".to_string(),
            }],
        ),
    )
    .await
    .expect("the probe must recover a refusal whose wording means nothing to us");

    assert_eq!(
        *images_per_request.lock().expect("image log lock poisoned"),
        vec![1, 0],
        "the probe re-sent the same turn without the attachments"
    );
    assert!(
        agent.images_withheld_from_current_route(),
        "a successful probe is the evidence that latches the route"
    );

    // The notice reports the empirical finding, not a claim about what the
    // provider said: the harness never read the provider's wording.
    let notices = std::iter::from_fn(|| rx.try_recv().ok())
        .filter_map(|response| match response {
            AgentResponse::Round {
                event: RoundEvent::Notice(notice),
                ..
            } => Some(notice),
            _ => None,
        })
        .filter(|notice| notice.kind == nuo_wire::NoticeKind::ImageInputWithheld)
        .collect::<Vec<_>>();
    assert_eq!(notices.len(), 1);
    assert!(
        notices[0]
            .body
            .as_deref()
            .is_some_and(|body| body.contains("retrying it without the attachments succeeded")),
        "the notice must describe the empirical finding: {:?}",
        notices[0].body
    );
}

/// When the probe **disproves** the hypothesis, nothing may be latched and the
/// user must see the refusal of the request they actually composed (ADR-0230).
///
/// The refusal here is unrelated to images — it recurs with the attachments
/// withheld — and its text even *mentions* `image_url`, exactly like a relay
/// that echoes part of the payload it rejected. That is deliberate: it proves no
/// amount of vendor prose can latch anything, because only the probe's outcome
/// is evidence (INV-VISION-6). Were text allowed to decide, this route would
/// silently lose every future image.
#[tokio::test]
async fn an_unrelated_refusal_is_not_blamed_on_images() {
    let directory =
        std::env::temp_dir().join(format!("nuo-image-disprove-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&directory).expect("create test directory");
    let session = Arc::new(SessionStore::for_path(directory.join("session.json")));
    let images_per_request = Arc::new(Mutex::new(Vec::new()));
    let agent = Arc::new(Agent::new(
        Arc::new(ImageRejectingProvider {
            images_per_request: Arc::clone(&images_per_request),
            rejection: "mock HTTP 400: invalid tool schema for 'write'; the payload echoes \
                        image_url parts",
            rejections: usize::MAX,
        }),
        Vec::new(),
        nuo_harness::AgentIdentity::default(),
    ));
    let (tx, _rx) = mpsc::unbounded_channel();
    let session_id = session.id().await;

    let error = execute_round(
        image_round_context(&agent, tx, &session, &session_id),
        image_round_input(
            "describe it",
            vec![nuo_wire::ImagePart {
                mime: "image/png".to_string(),
                data: "aGk=".to_string(),
            }],
        ),
    )
    .await
    .expect_err("an unrelated refusal must remain terminal");

    assert!(
        error
            .to_string()
            .contains("invalid tool schema for 'write'"),
        "the original refusal must surface, got: {error}"
    );
    assert!(
        !agent.images_withheld_from_current_route(),
        "a disproved hypothesis must not latch anything"
    );
    assert_eq!(
        *images_per_request.lock().expect("image log lock poisoned"),
        vec![1, 0],
        "exactly one probe, and no further retries"
    );
}

/// The `RoundContext` every ADR-0230 test runs with: a tight retry budget so a
/// probe is unmistakable in the request log.
fn image_round_context(
    agent: &Arc<Agent>,
    tx: mpsc::UnboundedSender<AgentResponse>,
    session: &Arc<SessionStore>,
    session_id: &str,
) -> RoundContext {
    RoundContext {
        agent: Arc::clone(agent),
        tx,
        token: CancellationToken::new(),
        session_id: session_id.to_string(),
        session: Arc::clone(session),
        projection: ContextProjectionSettings {
            budget: nuo_wire::CompactionPolicy::default().resolve(100_000),
            preserve_rounds: 6,
            summarize: false,
            prune: false,
            prune_protect_tokens: 0,
        },
        retry_max_attempts: 3,
        retry_base_ms: 1,
        retry_max_ms: 10,
        emit_round_completed: false,
    }
}

/// The `RoundInput` every ADR-0230 test runs with: a fresh round carrying
/// `images`.
fn image_round_input(prompt: &str, images: Vec<nuo_wire::ImagePart>) -> RoundInput {
    RoundInput {
        prompt: prompt.to_string(),
        hidden: false,
        display_prompt: None,
        sent_at_ms: None,
        images,
        driver: nuo_harness::orchestration::RoundDriver::Fresh,
    }
}
