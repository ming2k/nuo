use super::*;

#[tokio::test]
async fn stream_loop_verdict_after_natural_end_still_recovers() {
    // Dwell-scaled runaway: ~138 x 23-char lines clears the 3K dwell bar.
    // The Stream Sentinel consult is async now: an instantly-ready scripted
    // stream finishes before the spawned verdict can run, so this exercises
    // the natural-end settle path — the stream keeps rendering to the very
    // end, and the late `yes` still trims, discloses, and recovers.
    let degenerative_stream = (0..140).fold(
        vec![
            ProviderStreamEvent::TextDelta("Initial reasoning.\n".to_string()),
            ProviderStreamEvent::TextDelta("repeating line of code\n".to_string()),
        ],
        |mut acc, _| {
            acc.push(ProviderStreamEvent::TextDelta(
                "repeating line of code\n".to_string(),
            ));
            acc
        },
    );
    // Deltas streamed after the consult spawned keep rendering until the
    // verdict lands — that is the async contract — so the tail sentinel is
    // retained in the partial response. (The paced variant below pins the
    // opposite: a verdict landing mid-stream beats the remaining deltas.)
    let mut degenerative_stream = degenerative_stream;
    degenerative_stream.push(ProviderStreamEvent::TextDelta(
        "never reached extra text".to_string(),
    ));

    let agent = Arc::new(Agent::new(
        Arc::new(
            ScriptedProvider::new(vec![degenerative_stream, text_turn("Recovered summary.")])
                .with_cognitive_replies(&["yes"]),
        ),
        Vec::new(),
        crate::AgentIdentity::default(),
    ));

    let (events, outcome) = run_golden_round(&agent, "test", PermissionDecision::Once).await;
    let round_outcome = outcome.expect("round completes");

    assert_eq!(round_outcome.message.content, "Recovered summary.");
    assert!(
        events.iter().any(|event| matches!(
            event,
            AgentEvent::AssistantEnd(content) if content.contains("[... stream truncated")
        )),
        "the partial looping response is retained before recovery"
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            AgentEvent::Notice(notice)
                if notice.severity == NoticeSeverity::Warning
                    && notice.title == "Repetitive output stopped; recovery requested"
        )),
        "the soft intervention must be visible"
    );
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::AssistantEnd(content) if content.contains("never reached extra text")
    )));
}

#[tokio::test]
async fn tool_argument_loop_is_cut_and_recovered() {
    // A single call whose argument stream keeps repeating a 3-char period.
    // Once the dwell bar is crossed the Sentinel confirms and cuts it.
    let mut degenerate = vec![ProviderStreamEvent::ToolCallDelta {
        index: 0,
        id: Some("c1".to_string()),
        name: Some("alpha".to_string()),
        arguments: String::new(),
    }];
    for _ in 0..800 {
        degenerate.push(ProviderStreamEvent::ToolCallDelta {
            index: 0,
            id: None,
            name: None,
            arguments: "abcabc".to_string(),
        });
    }
    let agent = Arc::new(Agent::new(
        Arc::new(
            ScriptedProvider::new(vec![degenerate, text_turn("Recovered summary.")])
                .with_cognitive_replies(&["yes"]),
        ),
        vec![Arc::new(RecordingTool::read("alpha", "A-out"))],
        crate::AgentIdentity::default(),
    ));

    let (events, outcome) = run_golden_round(&agent, "go", PermissionDecision::Reject).await;

    assert_eq!(
        outcome.expect("recovery completes").message.content,
        "Recovered summary."
    );
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::Notice(notice)
            if notice.title == "Repetitive output stopped; recovery requested"
    )));
    assert!(
        events
            .iter()
            .all(|event| !matches!(event, AgentEvent::ToolCall { .. })),
        "a cut argument stream must never dispatch its call"
    );
}

#[tokio::test]
async fn in_flight_streaming_loop_cuts_mid_stream_when_verdict_lands() {
    // With a paced stream (1ms gaps — the realistic network cadence) the
    // runtime interleaves the detached Stream Sentinel consult with stream
    // consumption: the `yes` verdict lands while the degenerate stream is
    // still flowing, and the cut beats the remaining deltas.
    let degenerative_stream = || {
        (0..160)
            .map(|_| ProviderStreamEvent::TextDelta("repeating line of code\n".to_string()))
            .chain(std::iter::once(ProviderStreamEvent::TextDelta(
                "never reached extra text".to_string(),
            )))
            .collect::<Vec<_>>()
    };
    let provider = Arc::new(
        ScriptedProvider::new(vec![degenerative_stream(), text_turn("Recovered summary.")])
            .with_cognitive_replies(&["yes"])
            .paced(),
    );
    let agent = Arc::new(Agent::new(
        provider.clone(),
        Vec::new(),
        crate::AgentIdentity::default(),
    ));

    let (events, outcome) = run_golden_round(&agent, "test", PermissionDecision::Once).await;
    let round_outcome = outcome.expect("mid-stream cut recovers");

    assert_eq!(round_outcome.message.content, "Recovered summary.");
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::AssistantEnd(content) if content.contains("[... stream truncated")
    )));
    // The cut beat the tail: deltas scheduled after the verdict were never
    // consumed.
    assert!(events.iter().all(|event| !matches!(
        event,
        AgentEvent::AssistantEnd(content) if content.contains("never reached extra text")
    )));
    // Exactly one cognitive consult ran for the confirmed candidate.
    assert_eq!(
        provider
            .cognitive_requests
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .len(),
        1
    );
}

#[tokio::test]
async fn repeated_stream_loop_hard_stop_is_visible() {
    // The detector now escalates on *continuous dwell* (~3KB of unbroken
    // periodic tail), so the scripted runaway must be long enough to
    // legitimately clear that bar — a genuine doom loop does so easily.
    let degenerative_stream = || {
        (0..160)
            .map(|_| ProviderStreamEvent::TextDelta("repeating line of code\n".to_string()))
            .collect::<Vec<_>>()
    };
    let agent = Arc::new(Agent::new(
        Arc::new(
            ScriptedProvider::new(vec![degenerative_stream(), degenerative_stream()])
                .with_cognitive_replies(&["yes", "yes"]),
        ),
        Vec::new(),
        crate::AgentIdentity::default(),
    ));

    let (events, outcome) = run_golden_round(&agent, "test", PermissionDecision::Once).await;
    let round_outcome = outcome.expect("hard stream stop retains a terminal partial response");

    assert!(
        round_outcome
            .message
            .content
            .contains("[... stream truncated")
    );
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::Notice(notice)
            if notice.severity == NoticeSeverity::Error
                && notice.title == "Repetitive output stopped again; round ended"
    )));
}

#[tokio::test]
async fn a_tool_argument_loop_cut_retracts_the_call_it_announced() {
    // ADR-0166: a cut stream drops its call fragments, but the retraction must
    // run *first* — the ids it names live on those fragments. Clearing before
    // retracting emitted an empty retraction and left the frontend holding a
    // `receiving input` row forever.
    let looping_tool_args = {
        let mut events = vec![ProviderStreamEvent::ToolCallDelta {
            index: 0,
            id: Some("call_loop".to_string()),
            name: Some("big_tool".to_string()),
            arguments: String::new(),
        }];
        for _ in 0..160 {
            events.push(ProviderStreamEvent::ToolCallDelta {
                index: 0,
                id: None,
                name: None,
                arguments: "repeating argument fragment\n".to_string(),
            });
        }
        events
    };
    let agent = Arc::new(Agent::new(
        Arc::new(
            ScriptedProvider::new(vec![looping_tool_args, text_turn("recovered")])
                .with_cognitive_replies(&["yes"]),
        ),
        Vec::new(),
        crate::AgentIdentity::default(),
    ));

    let (events, outcome) = run_golden_round(&agent, "go", PermissionDecision::Once).await;
    assert_eq!(
        outcome.expect("the cut recovers").message.content,
        "recovered"
    );

    let announced = events
        .iter()
        .find_map(|event| match event {
            AgentEvent::ToolCallStarted { id, name } if name == "big_tool" => Some(id.clone()),
            _ => None,
        })
        .expect("the call was announced before the argument loop was cut");
    let retracted: Vec<String> = events
        .iter()
        .flat_map(|event| match event {
            AgentEvent::ToolCallsRetracted { ids } => ids.clone(),
            _ => Vec::new(),
        })
        .collect();
    assert_eq!(
        retracted,
        vec![announced.clone()],
        "the cut attempt's announced call is retracted by its own id"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::ToolCall { id, .. } if *id == announced)),
        "a loop-cut call fragment is never dispatched"
    );
}

#[tokio::test]
async fn reasoning_stream_loop_trims_reasoning_without_rewriting_answer() {
    // Dwell-scaled scripted runaway: 160 x 24-char lines (~3.8K > dwell bar).
    let looping_reasoning = (0..160).fold(
        vec![ProviderStreamEvent::TextDelta("Answer prefix.".to_string())],
        |mut acc, _| {
            acc.push(ProviderStreamEvent::ReasoningDelta(
                "repeating private trace\n".to_string(),
            ));
            acc
        },
    );
    let agent = Arc::new(Agent::new(
        Arc::new(
            ScriptedProvider::new(vec![looping_reasoning, text_turn("Recovered answer.")])
                .with_cognitive_replies(&["yes"]),
        ),
        Vec::new(),
        crate::AgentIdentity::default(),
    ));

    let (events, outcome) = run_golden_round(&agent, "test", PermissionDecision::Once).await;
    assert_eq!(
        outcome.expect("reasoning loop recovers").message.content,
        "Recovered answer."
    );
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::AssistantEnd(content) if content == "Answer prefix."
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::ReasoningEnd(content) if content.contains("[... stream truncated")
    )));
}

#[tokio::test]
async fn cognitive_clears_legitimate_repeated_reverse_engineering_data() {
    // Under dwell semantics this data never escalates at all: each line ends
    // in "nop", so the periodic trail acquits every push (the digits do not
    // dominate density either). The mechanical layer resolves legitimate
    // content locally — the cognitive pipeline must not even be consulted.
    let repeated_dump = "00401000: 90 90 90 90    nop nop nop nop\n";
    let provider = Arc::new(
        ScriptedProvider::new(vec![vec![
            ProviderStreamEvent::TextDelta(repeated_dump.to_string()),
            ProviderStreamEvent::TextDelta(repeated_dump.to_string()),
            ProviderStreamEvent::TextDelta(repeated_dump.to_string()),
            ProviderStreamEvent::TextDelta("Dump complete; continuing analysis.".to_string()),
        ]])
        .with_cognitive_replies(&["no"]),
    );
    let agent = Arc::new(Agent::new(
        provider.clone(),
        Vec::new(),
        crate::AgentIdentity::default(),
    ));

    let (events, outcome) = run_golden_round(
        &agent,
        "Inspect this repeated disassembly block.",
        PermissionDecision::Once,
    )
    .await;
    let content = outcome
        .expect("Cognitive-cleared stream completes")
        .message
        .content;
    assert!(content.contains("Dump complete; continuing analysis."));
    assert!(!content.contains("[... stream truncated"));
    assert!(
        events
            .iter()
            .all(|event| !matches!(event, AgentEvent::Notice(notice) if notice.source == NoticeSource::TurnGuard)),
        "an L1 candidate cleared by cognitive check must not surface as an intervention"
    );

    let requests = provider
        .cognitive_requests
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    // Zero consultations: the mechanical layer never escalated, so the
    // cognitive check was not woken — the entire point of dwell semantics.
    assert!(
        requests.is_empty(),
        "legitimate repeated data must resolve locally without an L2 review"
    );
}

#[tokio::test]
async fn agent_assembles_request_from_a_thread_branch() {
    let provider: Arc<dyn Provider> = Arc::new(TestProvider);
    let agent = Agent::new(provider, Vec::new(), crate::AgentIdentity::default());

    let mut thread = nuo_protocol::Thread::new(nuo_protocol::ThreadId::new("test-thread"), 1000);
    thread
        .append(
            nuo_protocol::FactPayload::Message {
                message: Message::new(Role::User, "Hello from the durable thread to Agent"),
            },
            nuo_protocol::FactContext::turn(1, 0),
            1001,
        )
        .unwrap();

    let request = agent
        .model_request_from_thread(&thread, &nuo_protocol::BranchId::main())
        .expect("Agent must assemble a request from a thread branch");

    assert_eq!(request.messages.len(), 1);
    assert_eq!(
        request.messages[0].content,
        "Hello from the durable thread to Agent"
    );
}

struct DummyCustomProvider {
    id: String,
    model: String,
}

#[async_trait]
impl Provider for DummyCustomProvider {
    async fn chat(
        &self,
        _request: &nuo_protocol::ModelRequest,
    ) -> Result<nuo_protocol::ProviderCompletion, nuo_protocol::ProviderError> {
        Ok(nuo_protocol::ProviderCompletion::message(Message::new(
            Role::Assistant,
            "dummy",
        )))
    }

    async fn stream_chat(
        &self,
        _request: &nuo_protocol::ModelRequest,
        _telemetry: nuo_protocol::TransportTelemetry,
    ) -> Result<
        BoxStream<'static, Result<String, nuo_protocol::ProviderError>>,
        nuo_protocol::ProviderError,
    > {
        Ok(Box::pin(stream::empty()))
    }

    fn provider_id(&self) -> String {
        self.id.clone()
    }

    fn model(&self) -> String {
        self.model.clone()
    }
}

/// RAII sandbox for the process-wide XDG roots.
///
/// This test persists a throwaway connection through the real
/// [`nuo_persistence::connections::Connections`] store. Without an isolated
/// `Dirs`, `save()` rewrites the developer's real
/// `$XDG_STATE_HOME/nuo/connections.toml` and erases every connection they own.
/// [`nuo_persistence::paths::TestDirsGuard`] holds the shared override lock and
/// clears the roots on drop. The `TempDir` is returned so the roots outlive the
/// guard's body.
fn sandboxed_paths() -> (nuo_persistence::paths::TestDirsGuard, tempfile::TempDir) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dirs = nuo_persistence::paths::Dirs {
        config_dir: tmp.path().join("config"),
        data_dir: tmp.path().join("data"),
        state_dir: tmp.path().join("state"),
        cache_dir: tmp.path().join("cache"),
        runtime_dir: None,
    };
    (nuo_persistence::paths::TestDirsGuard::install(dirs), tmp)
}

#[tokio::test]
async fn test_runtime_spend_circuit_breaker_intercepts_over_budget_connection() {
    let _sandbox = sandboxed_paths();
    let conn_name = "guarded-test-conn";
    let mut conns = nuo_persistence::connections::Connections::default();
    conns
        .connections
        .push(nuo_persistence::connections::Connection {
            name: conn_name.to_string(),
            provider: "google".to_string(),
            budget: nuo_protocol::ConnectionBudget::local(
                0.05,
                nuo_protocol::BudgetPeriod::Session,
            ),
            ..Default::default()
        });
    conns.save().unwrap();

    let provider: Arc<dyn Provider> = Arc::new(DummyCustomProvider {
        id: conn_name.to_string(),
        model: "gemini-2.0-flash".to_string(),
    });
    let agent = Arc::new(Agent::new(
        provider,
        Vec::new(),
        crate::AgentIdentity::default(),
    ));

    let mut round = agent.begin_streaming_round();
    // 600,000 prompt tokens * $0.10/1M = $0.06 -> exceeds $0.05 limit
    round.set_token_usage_for_test(nuo_protocol::TokenUsage {
        prompt_tokens: 600_000,
        total_tokens: 600_000,
        ..Default::default()
    });

    let mut messages = vec![Message::new(Role::User, "Hello")];
    let cancel = tokio_util::sync::CancellationToken::new();

    let res = agent
        .resume_streaming_with_events(&mut messages, &cancel, &mut round, |_| {})
        .await;

    let err = res.unwrap_err();
    match err {
        HarnessError::Provider(provider_err) => {
            assert_eq!(
                provider_err.kind(),
                nuo_protocol::ProviderErrorKind::QuotaExhausted
            );
            assert!(provider_err.message().contains("[SPEND CIRCUIT BREAKER]"));
            assert!(provider_err.message().contains("$0.05"));
        }
        other => panic!("expected HarnessError::Provider(QuotaExhausted), got {other:?}"),
    }
}

// ── Turn-scoped previews (ADR-0125) ─────────────────────────────────────────

/// A write tool that *does* have a pure preview, so a test can assert the ordering
/// guarantee that matters: the diff is emitted before the bytes change.
struct PreviewableWriteTool {
    name: &'static str,
    path: &'static str,
    before: &'static str,
    after: &'static str,
}

#[async_trait]
impl Tool for PreviewableWriteTool {
    fn name(&self) -> &str {
        self.name
    }
    fn description(&self) -> &str {
        "previewable write test tool"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }
    fn risk_profile(&self) -> nuo_tool::RiskProfile {
        nuo_tool::RiskProfile::IdempotentMutation
    }
    fn scope_target(&self, _arguments: &str) -> nuo_protocol::ScopeTarget {
        nuo_protocol::ScopeTarget::Path(std::path::PathBuf::from(self.path))
    }
    fn accesses(&self, _arguments: &str) -> nuo_protocol::ToolAccesses {
        nuo_protocol::ToolAccesses::write_file(self.path.to_string())
    }
    fn preview(&self, _arguments: &serde_json::Value) -> Option<nuo_protocol::PreviewResult> {
        Some(nuo_protocol::PreviewResult {
            path: self.path.to_string(),
            op: nuo_protocol::PatchOp::Edit,
            old: self.before.to_string(),
            new: self.after.to_string(),
            start_line: 1,
            warnings: Vec::new(),
        })
    }
    async fn call(&self, _arguments: &str) -> Result<String, String> {
        Ok(format!("wrote {}", self.path))
    }
}

/// The preview is emitted up front, alongside the calls themselves, so a whole
/// batch's footprint is visible before anything runs.
///
/// This is the turn-scoped preview's reason for existing: with the diff attached
/// only to permission prompts, a batch of five authorized writes gives the operator
/// five separable decisions and no view of the aggregate. Emitting at preflight means
/// every mutating card carries its diff before execution begins, which is what makes
/// a single batch-level decision possible at all.
#[tokio::test]
async fn previews_are_emitted_up_front_for_every_mutating_call() {
    let agent = Arc::new(Agent::new(
        Arc::new(ScriptedProvider::new(vec![
            turn(&[("c1", "write_a", "{}"), ("c2", "write_b", "{}")]),
            text_turn("done"),
        ])),
        vec![
            Arc::new(PreviewableWriteTool {
                name: "write_a",
                path: "a.rs",
                before: "old a\n",
                after: "new a\n",
            }),
            Arc::new(PreviewableWriteTool {
                name: "write_b",
                path: "b.rs",
                before: "",
                after: "brand new\n",
            }),
        ],
        crate::AgentIdentity::default(),
    ));

    let (events, _) = run_golden_round(&agent, "go", PermissionDecision::Session).await;
    let lines = transcript(&events);

    let first_preview = lines
        .iter()
        .position(|line| line.starts_with("tool-preview"))
        .expect("a mutating call must be previewed");
    let first_result = lines
        .iter()
        .position(|line| line.starts_with("tool-result"))
        .expect("the calls must have run");
    assert!(
        first_preview < first_result,
        "every preview must precede the first result, got:\n{lines:#?}"
    );

    // Both are present, with counts derived from the preview content itself.
    assert!(
        lines.contains(&"tool-preview write_a a.rs +1 -1".to_string()),
        "the edit's diff is reported: {lines:#?}"
    );
    assert!(
        lines.contains(&"tool-preview write_b b.rs +1 -0".to_string()),
        "a creation counts only additions: {lines:#?}"
    );
}

/// A read-only call is not previewed: there is no change to describe, and a
/// "preview" card for every `read` would bury the ones that matter.
#[tokio::test]
async fn read_only_calls_are_not_previewed() {
    let agent = Arc::new(Agent::new(
        Arc::new(ScriptedProvider::new(vec![
            turn(&[("c1", "alpha", "{\"k\":1}")]),
            text_turn("done"),
        ])),
        vec![Arc::new(RecordingTool::read("alpha", "A-out"))],
        crate::AgentIdentity::default(),
    ));

    let (events, _) = run_golden_round(&agent, "go", PermissionDecision::Reject).await;
    assert!(
        !transcript(&events)
            .iter()
            .any(|line| line.starts_with("tool-preview")),
        "a read has nothing to preview"
    );
}
