//! Orchestration tests: projection budgets and capture pruning.

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
        instructions_tokens: 9_000,
        tool_schema_tokens: 3_000,
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

#[allow(dead_code)]
struct AutoCleanArcSession(Arc<ThreadSession>, tempfile::TempDir);
impl std::ops::Deref for AutoCleanArcSession {
    type Target = Arc<ThreadSession>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

async fn fresh_test_session() -> AutoCleanArcSession {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(ThreadSession::for_path(dir.path().join("session.json")));
    AutoCleanArcSession(store, dir)
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

struct AlwaysUnreachableProvider {
    stream_attempts: AtomicUsize,
}

#[async_trait]
impl nuo_protocol::Provider for AlwaysUnreachableProvider {
    async fn chat(
        &self,
        _request: &ModelRequest,
    ) -> Result<nuo_protocol::ProviderCompletion, nuo_protocol::ProviderError> {
        Err(nuo_protocol::ProviderError::new(
            "offline-test",
            nuo_protocol::ProviderErrorKind::Unreachable,
            "resolve failed: api.example.com: network is unreachable",
        )
        .retryable(None))
    }

    async fn stream_chat(
        &self,
        _request: &ModelRequest,
        _telemetry: nuo_protocol::TransportTelemetry,
    ) -> Result<
        futures::stream::BoxStream<'static, Result<String, nuo_protocol::ProviderError>>,
        nuo_protocol::ProviderError,
    > {
        self.stream_attempts.fetch_add(1, Ordering::SeqCst);
        Err(nuo_protocol::ProviderError::new(
            "offline-test",
            nuo_protocol::ProviderErrorKind::Unreachable,
            "resolve failed: api.example.com: network is unreachable",
        )
        .retryable(None))
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
        self.stream_attempts.fetch_add(1, Ordering::SeqCst);
        Err(nuo_protocol::ProviderError::new(
            "offline-test",
            nuo_protocol::ProviderErrorKind::Unreachable,
            "resolve failed: api.example.com: network is unreachable",
        )
        .retryable(None))
    }
}

/// `[INV-RESILIENCE-01]` (ADR-0224): consecutive `ProviderErrorKind::Unreachable`
/// errors must fast-fail after `UNREACHABLE_MAX_ATTEMPTS` (3) and park a durable
/// `/retry` point instead of burning the 30-attempt transient upstream budget.
#[tokio::test]
async fn unreachable_errors_fast_fail_and_park_retry_point() {
    let session = fresh_test_session().await;
    let session_id = session.id().await;
    let provider = Arc::new(AlwaysUnreachableProvider {
        stream_attempts: AtomicUsize::new(0),
    });
    let agent = Arc::new(Agent::new(
        provider.clone(),
        Vec::new(),
        AgentIdentity::default(),
    ));
    let (tx, mut rx) = mpsc::unbounded_channel();
    let token = CancellationToken::new();

    let result = execute_round(
        RoundContext {
            agent,
            tx,
            token,
            session: session.clone(),
            session_id,
            projection: ContextProjectionSettings::for_window(200_000),
            retry_max_attempts: 30,
            stream_restart_max_attempts: 3,
            retry_base_ms: 1,
            retry_max_ms: 5,
            emit_round_completed: true,
        },
        RoundInput {
            prompt: "hello offline".to_string(),
            hidden: false,
            display_prompt: None,
            sent_at_ms: None,
            images: Vec::new(),
            driver: RoundDriver::Fresh,
        },
    )
    .await;

    let error = result.expect_err("offline round must fail cleanly");
    assert_eq!(
        provider.stream_attempts.load(Ordering::SeqCst),
        crate::runtime::retry::UNREACHABLE_MAX_ATTEMPTS,
        "must stop at UNREACHABLE_MAX_ATTEMPTS instead of burning retry_max_attempts=30"
    );
    assert!(
        error.to_string().contains("Network unreachable") && error.to_string().contains("/retry"),
        "error should explain offline state and /retry recovery: {error}"
    );
    assert!(
        session.retry_pending().await.is_some(),
        "fast-failed offline round must park a durable RetryPoint"
    );

    let mut scheduled = 0usize;
    while let Ok(resp) = rx.try_recv() {
        if let AgentResponse::Round { scope, .. } = resp
            && matches!(scope.event(), RoundEvent::RetryScheduled { .. })
        {
            scheduled += 1;
        }
    }
    assert_eq!(
        scheduled,
        crate::runtime::retry::UNREACHABLE_MAX_ATTEMPTS - 1,
        "3 total attempts means 2 scheduled retries"
    );
}

async fn seed_compaction_rounds(session: &ThreadSession, rounds: usize) {
    for index in 0..rounds {
        let delta = [
            Message::new(
                Role::User,
                format!(
                    "user round {index} {}",
                    "lorem ipsum dolor sit amet ".repeat(6)
                ),
            ),
            Message::new(
                Role::Assistant,
                format!(
                    "assistant round {index} {}",
                    "consectetur adipiscing elit sed do ".repeat(6)
                ),
            ),
        ];
        session
            .commit_turn(CommitTurn {
                appended: &delta,
                round_counter: Some(index as u64 + 1),
                usage_records: &[],
                retry_point: None,
                round_interrupt: None,
                round_summary: None,
                operation_id: None,
                expected_revision: None,
            })
            .await
            .unwrap();
    }
}

/// `[INV-THREADSSOT-02]` / `[INV-CMP-08]`: Mid-turn context projection (Tier-1
/// prune + Tier-2 compaction) followed by subsequent `commit_turn` calls and a
/// post-compaction prune must NEVER overwrite or lose pre-compaction turns on
/// disk, and must preserve the canonical `CompactionCheckpoint` provenance.
#[tokio::test]
async fn mid_turn_gate_compaction_and_subsequent_turns_never_erase_history() {
    use crate::PruningProjectionGate;
    use crate::cognition::context_projection::ContextProjectionGate;

    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("session.json");
    let session = Arc::new(ThreadSession::for_path(path.clone()));

    seed_compaction_rounds(&session, 6).await;
    let call = nuo_tool::ToolCall::new("call-big", "execute_command", "{}");
    let mut tool_turn = Message::new(Role::Assistant, "running diagnostics");
    tool_turn.tool_calls = Some(vec![call.clone()]);
    let big_output = "diagnostic line with enough payload to exceed budget\n".repeat(120);
    let tool_res = Message::tool_result(&call, &big_output);
    session
        .commit_turn(CommitTurn::new(&[
            Message::new(Role::User, "user round 6 diagnostics"),
            tool_turn,
            tool_res,
            Message::new(Role::User, "user round 7 follow-up"),
            Message::new(Role::Assistant, "assistant round 7 reply"),
        ]))
        .await
        .unwrap();

    // Simulate a live window with an uncommitted tail (e.g. @file injection)
    // appended after `committed_len`.
    let mut live = session.model_window().await;
    let committed_len = live.len();
    live.push(Message::new(Role::User, "UNCOMMITTED-TURN-TAIL"));

    let tokens_before = nuo_protocol::pressure::estimate_tokens(&live);
    let mut settings =
        ContextProjectionSettings::from_policy(&nuo_protocol::CompactionPolicy::default(), 200_000);
    settings.budget.prune_threshold_tokens = (tokens_before / 8).max(32);
    settings.budget.compaction_threshold_tokens = (tokens_before / 4).max(64);
    settings.budget.target_tokens = (tokens_before / 4).max(64);
    settings.prune = true;
    settings.prune_protect_tokens = 0;
    settings.summarize = false;

    let settings_for_gate = settings.clone();
    let gate = PruningProjectionGate::new(
        Arc::clone(&session),
        Arc::new(move || settings_for_gate.clone()),
        Arc::new(nuo_protocol::MessageTokenWeights::new()),
        None,
    );

    let outcome = gate
        .project_context(live, committed_len)
        .await
        .expect("mid-turn gate must engage");
    let mut window = outcome.messages;
    let mut cursor = outcome.committed_len;

    assert_eq!(
        window[0].origin.as_ref().map(|o| o.kind),
        Some(nuo_tool::InjectionKind::CompactionCheckpoint),
        "projected window must lead with canonical CompactionCheckpoint"
    );
    assert!(
        cursor < window.len()
            && window[cursor..]
                .iter()
                .any(|m| m.content == "UNCOMMITTED-TURN-TAIL"),
        "uncommitted tail must remain after committed_len cursor"
    );

    window.push(Message::new(Role::Assistant, "POST-COMPACT-ANSWER-1"));
    session
        .commit_turn(CommitTurn::new(&window[cursor..]))
        .await
        .unwrap();
    cursor = window.len();

    let call2 = nuo_tool::ToolCall::new("call-post", "execute_command", "{}");
    let mut post_assistant = Message::new(Role::Assistant, "post-compact tool turn");
    post_assistant.tool_calls = Some(vec![call2.clone()]);
    let post_tool = Message::tool_result(&call2, "post tool output\n".repeat(400));
    let next_delta = [
        Message::new(Role::User, "POST-COMPACT-USER-2"),
        post_assistant,
        post_tool,
    ];
    window.extend(next_delta.iter().cloned());
    session
        .commit_turn(CommitTurn::new(&window[cursor..]))
        .await
        .unwrap();
    cursor = window.len();

    // Second projection (Tier-1 prune) AFTER compaction, followed by another turn.
    let mut prune_only_settings = settings.clone();
    prune_only_settings.budget.prune_threshold_tokens = usize::MAX;
    let prune_gate = PruningProjectionGate::new(
        Arc::clone(&session),
        Arc::new(move || prune_only_settings.clone()),
        Arc::new(nuo_protocol::MessageTokenWeights::new()),
        None,
    );
    if let Some(pruned) = prune_gate.project_context(window.clone(), cursor).await {
        window = pruned.messages;
        cursor = pruned.committed_len;
    }
    window.push(Message::new(Role::Assistant, "FINAL-POST-PRUNE-ANSWER"));
    session
        .commit_turn(CommitTurn::new(&window[cursor..]))
        .await
        .unwrap();

    drop(gate);
    drop(prune_gate);
    drop(session);

    let reopened = ThreadSession::for_path(path);
    let full = reopened.full_transcript().await;
    for round_idx in 0..6 {
        let needle = format!("user round {round_idx}");
        assert!(
            full.iter().any(|m| m.content.contains(&needle)),
            "pre-compaction {needle} vanished after mid-turn compaction + commit_turn"
        );
    }
    for expected in [
        "user round 6 diagnostics",
        "user round 7 follow-up",
        "UNCOMMITTED-TURN-TAIL",
        "POST-COMPACT-ANSWER-1",
        "POST-COMPACT-USER-2",
        "FINAL-POST-PRUNE-ANSWER",
    ] {
        assert!(
            full.iter().any(|m| m.content.contains(expected)),
            "missing `{expected}` in reopened full_transcript"
        );
    }
    assert!(
        full.iter().any(|m| {
            m.origin
                .as_ref()
                .is_some_and(|o| o.kind == nuo_tool::InjectionKind::CompactionCheckpoint)
        }),
        "compaction checkpoint must remain visible with CompactionCheckpoint provenance"
    );

    let model_window = reopened.model_window().await;
    assert_eq!(
        model_window[0].origin.as_ref().map(|o| o.kind),
        Some(nuo_tool::InjectionKind::CompactionCheckpoint)
    );
    assert!(
        !model_window[1..]
            .iter()
            .any(|m| m.content.contains("user round 0")),
        "folded round 0 must stay excluded from model_window body (only summarized in checkpoint)"
    );
    assert_eq!(
        model_window.last().unwrap().content,
        "FINAL-POST-PRUNE-ANSWER"
    );
}

/// End-to-end `execute_round`: when a provider returns `ContextOverflow` on
/// attempt 1, `recover_context_overflow` compacts the request window,
/// reprojects `pending_request` so attempt 2 receives the compacted window,
/// and preserves every pre-compaction round in `full_transcript()` on disk.
#[tokio::test]
async fn execute_round_overflow_recovery_compacts_request_and_preserves_full_history() {
    use nuo_protocol::{
        Provider, ProviderCompletion, ProviderError, ProviderErrorKind, TransportTelemetry,
    };
    use std::sync::Mutex;

    struct OverflowThenSucceedProvider {
        attempts: AtomicUsize,
        seen_requests: Mutex<Vec<Vec<Message>>>,
    }

    #[async_trait]
    impl Provider for OverflowThenSucceedProvider {
        async fn chat(&self, _request: &ModelRequest) -> Result<ProviderCompletion, ProviderError> {
            Ok(ProviderCompletion::message(Message::new(
                Role::Assistant,
                "Overflow Test Title",
            )))
        }

        async fn stream_chat(
            &self,
            _request: &ModelRequest,
            _telemetry: TransportTelemetry,
        ) -> Result<BoxStream<'static, Result<String, ProviderError>>, ProviderError> {
            Ok(Box::pin(futures::stream::empty()))
        }

        async fn stream_chat_events(
            &self,
            request: &ModelRequest,
            _telemetry: TransportTelemetry,
        ) -> Result<BoxStream<'static, Result<ProviderStreamEvent, ProviderError>>, ProviderError>
        {
            self.seen_requests
                .lock()
                .unwrap()
                .push(request.messages.clone());
            let attempt = self.attempts.fetch_add(1, Ordering::SeqCst);
            if attempt == 0 {
                return Err(ProviderError::new(
                    "test",
                    ProviderErrorKind::ContextOverflow,
                    "context length exceeded",
                ));
            }
            Ok(Box::pin(futures::stream::iter(vec![
                Ok(ProviderStreamEvent::TextDelta(
                    "recovered answer".to_string(),
                )),
                Ok(ProviderStreamEvent::Completed(
                    nuo_protocol::ProviderCompletionMeta::default(),
                )),
            ])))
        }

        fn model(&self) -> String {
            "overflow-model".to_string()
        }
    }

    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("session.json");
    let session = Arc::new(ThreadSession::for_path(path.clone()));
    seed_compaction_rounds(&session, 8).await;

    let provider = Arc::new(OverflowThenSucceedProvider {
        attempts: AtomicUsize::new(0),
        seen_requests: Mutex::new(Vec::new()),
    });
    let agent = Arc::new(Agent::new(
        provider.clone(),
        Vec::new(),
        AgentIdentity::default(),
    ));
    let (tx, _rx) = mpsc::unbounded_channel();

    let mut projection =
        ContextProjectionSettings::from_policy(&nuo_protocol::CompactionPolicy::default(), 200_000);
    projection.budget.compaction_threshold_tokens = usize::MAX;
    projection.budget.prune_threshold_tokens = usize::MAX;
    projection.budget.target_tokens = 128;
    projection.prune = false;
    projection.summarize = false;

    execute_round(
        RoundContext {
            agent,
            tx,
            token: CancellationToken::new(),
            session_id: session.id().await,
            session: Arc::clone(&session),
            projection,
            retry_max_attempts: 3,
            stream_restart_max_attempts: 2,
            retry_base_ms: 1,
            retry_max_ms: 10,
            emit_round_completed: false,
        },
        RoundInput {
            prompt: "NEW-OVERFLOW-PROMPT".to_string(),
            hidden: false,
            display_prompt: None,
            sent_at_ms: None,
            images: Vec::new(),
            driver: RoundDriver::Fresh,
        },
    )
    .await
    .expect("overflow recovery must succeed on attempt 2");

    let requests = provider.seen_requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 2, "expected overflow attempt + retry");
    assert!(
        requests[1].len() < requests[0].len(),
        "retry request must be compacted ({} vs {})",
        requests[1].len(),
        requests[0].len()
    );
    assert!(
        requests[1].iter().any(|m| {
            m.origin
                .as_ref()
                .is_some_and(|o| o.kind == nuo_tool::InjectionKind::CompactionCheckpoint)
        }),
        "retry request must carry the canonical CompactionCheckpoint"
    );

    drop(session);
    let reopened = ThreadSession::for_path(path);
    let full = reopened.full_transcript().await;
    for round_idx in 0..8 {
        let needle = format!("user round {round_idx}");
        assert!(
            full.iter().any(|m| m.content.contains(&needle)),
            "pre-compaction {needle} vanished after overflow recovery"
        );
    }
    assert!(
        full.iter()
            .any(|m| m.content.contains("NEW-OVERFLOW-PROMPT")),
        "new prompt must survive"
    );
    assert!(
        full.iter().any(|m| m.content.contains("recovered answer")),
        "recovered answer must survive"
    );
    assert!(
        full.iter().any(|m| {
            m.origin
                .as_ref()
                .is_some_and(|o| o.kind == nuo_tool::InjectionKind::CompactionCheckpoint)
        }),
        "compaction checkpoint must survive in full_transcript"
    );
}

/// End-to-end `execute_round` with `PruningProjectionGate` installed:
/// turn 0 executes a tool returning a large payload and commits via
/// `turn_persist`; before turn 1, `project_context_if_needed` triggers
/// mid-turn compaction (replacing the live window and rebasing `committed_len`);
/// turn 1 produces the final answer and commits via `finalize_round`.
/// Cold-reopening the session must return EVERY turn from round 0 through
/// turn 1's final answer with zero data loss and zero duplicates.
#[tokio::test]
async fn execute_round_mid_turn_compaction_preserves_full_history_across_reload() {
    use crate::PruningProjectionGate;
    use nuo_protocol::{Provider, ProviderCompletion, ProviderError, TransportTelemetry};
    use nuo_tool::{Tool, ToolContext, ToolError, ToolOutput};
    use std::sync::Mutex;

    struct HeavyTool;

    #[async_trait]
    impl Tool for HeavyTool {
        fn name(&self) -> &str {
            "heavy_probe"
        }

        fn description(&self) -> &str {
            "Returns a large diagnostic output to trigger mid-turn compaction."
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({ "type": "object", "properties": {} })
        }

        async fn execute(
            &self,
            _ctx: &ToolContext,
            _arguments: serde_json::Value,
        ) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput::text(
                "heavy diagnostic payload line for mid-turn compaction test\n".repeat(120),
            ))
        }
    }

    struct TwoTurnProvider {
        calls: AtomicUsize,
        seen_requests: Mutex<Vec<Vec<Message>>>,
    }

    #[async_trait]
    impl Provider for TwoTurnProvider {
        async fn chat(&self, _request: &ModelRequest) -> Result<ProviderCompletion, ProviderError> {
            Ok(ProviderCompletion::message(Message::new(
                Role::Assistant,
                "Midturn Test Title",
            )))
        }

        async fn stream_chat(
            &self,
            _request: &ModelRequest,
            _telemetry: TransportTelemetry,
        ) -> Result<BoxStream<'static, Result<String, ProviderError>>, ProviderError> {
            Ok(Box::pin(futures::stream::empty()))
        }

        async fn stream_chat_events(
            &self,
            request: &ModelRequest,
            _telemetry: TransportTelemetry,
        ) -> Result<BoxStream<'static, Result<ProviderStreamEvent, ProviderError>>, ProviderError>
        {
            self.seen_requests
                .lock()
                .unwrap()
                .push(request.messages.clone());
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            if call == 0 {
                Ok(Box::pin(futures::stream::iter(vec![
                    Ok(ProviderStreamEvent::ToolCallDelta {
                        index: 0,
                        id: Some("call-midturn-1".to_string()),
                        name: Some("heavy_probe".to_string()),
                        arguments: "{}".to_string(),
                    }),
                    Ok(ProviderStreamEvent::Completed(
                        nuo_protocol::ProviderCompletionMeta::default(),
                    )),
                ])))
            } else {
                Ok(Box::pin(futures::stream::iter(vec![
                    Ok(ProviderStreamEvent::TextDelta(
                        "MIDTURN-COMPACTED-FINAL-ANSWER".to_string(),
                    )),
                    Ok(ProviderStreamEvent::Completed(
                        nuo_protocol::ProviderCompletionMeta::default(),
                    )),
                ])))
            }
        }

        fn model(&self) -> String {
            "midturn-model".to_string()
        }
    }

    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("session.json");
    let session = Arc::new(ThreadSession::for_path(path.clone()));
    seed_compaction_rounds(&session, 8).await;

    let provider = Arc::new(TwoTurnProvider {
        calls: AtomicUsize::new(0),
        seen_requests: Mutex::new(Vec::new()),
    });
    let agent = Arc::new(Agent::new(
        provider.clone(),
        vec![Arc::new(HeavyTool)],
        AgentIdentity::default(),
    ));

    // Pre-round thresholds stay high so turn 0 runs uncompacted; mid-turn gate
    // fires right before turn 1 once HeavyTool's output enters the window.
    let pre_round_tokens = nuo_protocol::pressure::estimate_tokens(&session.model_window().await);
    let mut mid_turn_settings =
        ContextProjectionSettings::from_policy(&nuo_protocol::CompactionPolicy::default(), 200_000);
    mid_turn_settings.budget.prune_threshold_tokens = pre_round_tokens + 250;
    mid_turn_settings.budget.compaction_threshold_tokens = pre_round_tokens + 300;
    mid_turn_settings.budget.target_tokens = (pre_round_tokens / 3).max(96);
    mid_turn_settings.prune = true;
    mid_turn_settings.prune_protect_tokens = usize::MAX;
    mid_turn_settings.summarize = false;

    agent.set_context_prune_threshold(mid_turn_settings.budget.prune_threshold_tokens);
    let gate_settings = mid_turn_settings.clone();
    agent.set_context_projection_gate(Some(Arc::new(PruningProjectionGate::new(
        Arc::clone(&session),
        Arc::new(move || gate_settings.clone()),
        agent.token_weights_handle(),
        None,
    ))));

    let mut pre_round_projection =
        ContextProjectionSettings::from_policy(&nuo_protocol::CompactionPolicy::default(), 200_000);
    pre_round_projection.budget.compaction_threshold_tokens = usize::MAX;
    pre_round_projection.budget.prune_threshold_tokens = usize::MAX;
    pre_round_projection.prune = false;
    pre_round_projection.summarize = false;

    let (tx, mut rx) = mpsc::unbounded_channel();
    execute_round(
        RoundContext {
            agent,
            tx,
            token: CancellationToken::new(),
            session_id: session.id().await,
            session: Arc::clone(&session),
            projection: pre_round_projection,
            retry_max_attempts: 2,
            stream_restart_max_attempts: 2,
            retry_base_ms: 1,
            retry_max_ms: 10,
            emit_round_completed: false,
        },
        RoundInput {
            prompt: "TRIGGER-MIDTURN-COMPACTION".to_string(),
            hidden: false,
            display_prompt: None,
            sent_at_ms: None,
            images: Vec::new(),
            driver: RoundDriver::Fresh,
        },
    )
    .await
    .expect("two-turn round with mid-turn compaction must succeed");

    // Live client must have received `RoundEvent::Compacted` at the mid-turn
    // seam so live and resumed views agree row-for-row (`[INV-CMP-10]`).
    let mut saw_live_compacted = false;
    while let Ok(resp) = rx.try_recv() {
        if let AgentResponse::Round { scope, .. } = resp
            && matches!(scope.event(), RoundEvent::Compacted { .. })
        {
            saw_live_compacted = true;
        }
    }
    assert!(
        saw_live_compacted,
        "mid-turn compaction must emit RoundEvent::Compacted to live clients"
    );

    let requests = provider.seen_requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 2, "expected turn 0 + turn 1");
    assert!(
        requests[1].iter().any(|m| {
            m.origin
                .as_ref()
                .is_some_and(|o| o.kind == nuo_tool::InjectionKind::CompactionCheckpoint)
        }),
        "turn 1 must have run against the mid-turn compacted window"
    );

    drop(session);
    let reopened = ThreadSession::for_path(path);
    let full = reopened.full_transcript().await;
    let dialogue_only: Vec<&Message> = full
        .iter()
        .filter(|m| {
            m.origin.as_ref().map(|o| o.kind) != Some(nuo_tool::InjectionKind::CompactionCheckpoint)
        })
        .collect();
    for round_idx in 0..8 {
        let needle = format!("user round {round_idx}");
        let count = dialogue_only
            .iter()
            .filter(|m| m.content.contains(&needle))
            .count();
        assert_eq!(
            count, 1,
            "pre-compaction `{needle}` must appear exactly once in dialogue after mid-turn compaction + reload"
        );
    }
    assert_eq!(
        dialogue_only
            .iter()
            .filter(|m| m.content == "TRIGGER-MIDTURN-COMPACTION")
            .count(),
        1
    );
    assert_eq!(
        dialogue_only
            .iter()
            .filter(|m| m.content == "MIDTURN-COMPACTED-FINAL-ANSWER")
            .count(),
        1
    );
    assert!(
        full.iter().any(|m| {
            m.origin
                .as_ref()
                .is_some_and(|o| o.kind == nuo_tool::InjectionKind::CompactionCheckpoint)
        }),
        "compaction checkpoint must survive in full_transcript"
    );

    let model_window = reopened.model_window().await;
    assert_eq!(
        model_window[0].origin.as_ref().map(|o| o.kind),
        Some(nuo_tool::InjectionKind::CompactionCheckpoint)
    );
    assert!(
        !model_window[1..]
            .iter()
            .any(|m| m.content.contains("user round 0")),
        "folded round 0 must stay excluded from model_window body"
    );
    assert_eq!(
        model_window.last().unwrap().content,
        "MIDTURN-COMPACTED-FINAL-ANSWER"
    );
}
