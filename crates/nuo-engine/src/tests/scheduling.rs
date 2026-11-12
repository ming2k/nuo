use super::*;

#[test]
fn hard_stop_turns_getter_round_trips_setter() {
    // The `/hard-stop` path (and config seed) writes via `set_hard_stop_turns`
    // and reads via `get_hard_stop_turns`; the pair must round-trip. Default
    // is 0 (uncapped, ADR-0009).
    let agent = agent();
    assert_eq!(agent.get_hard_stop_turns(), 0);
    agent.set_hard_stop_turns(99);
    assert_eq!(agent.get_hard_stop_turns(), 99);
    agent.set_hard_stop_turns(0);
    assert_eq!(agent.get_hard_stop_turns(), 0);
}

#[tokio::test]
async fn hard_stop_aborts_when_budget_configured() {
    // hard_stop_turns is the only opt-in execution cap. With it set to 3, the
    // The third tool-bearing turn trips the budget and the round aborts with the budget in
    // the message.
    let tool = RecordingTool::read("alpha", "A-out");
    let agent = Arc::new(Agent::new(
        Arc::new(ScriptedProvider::new(distinct_read_turns(10, None))),
        vec![Arc::new(tool)],
        crate::AgentIdentity::default(),
    ));
    agent.set_hard_stop_turns(3);

    let mut messages = vec![Message::new(Role::User, "go")];
    let error = agent
        .run_streaming_with_events(&mut messages, &CancellationToken::new(), |_| {})
        .await
        .expect_err("hard-stop budget must abort the round");

    let message = match error {
        HarnessError::Other(message) => message,
        other => panic!("expected HarnessError::Other, got {other:?}"),
    };
    assert!(
        message.contains("hard-stop budget of 3"),
        "error must name the budget, got: {message}"
    );
}

#[test]
fn agent_config_defaults_match_runtime_constants() {
    // The config struct's defaults must match the seeds the agent uses when
    // no config is loaded, so a missing `[agent]` table is indistinguishable
    // from one that explicitly sets the defaults (ADR-0018).
    use nuo_persistence::config::AgentConfig;
    let cfg = AgentConfig::default();
    assert_eq!(cfg.hard_stop_turns, 0);
    // The agent seeds the same hard-stop budget by default (uncapped).
    let agent = agent();
    assert_eq!(agent.get_hard_stop_turns(), 0);
}

// /debug trace

/// A provider whose `stream_chat_events` emits a fixed two-event sequence, so
/// the streaming capture path can be exercised deterministically.
struct TwoEventProvider;

#[async_trait]
impl Provider for TwoEventProvider {
    async fn chat(
        &self,
        _request: &nuo_protocol::ModelRequest,
    ) -> Result<nuo_protocol::ProviderCompletion, nuo_protocol::ProviderError> {
        Err(nuo_protocol::ProviderError::new(
            "mock",
            nuo_protocol::ProviderErrorKind::Other,
            "chat path not used by this test",
        ))
    }
    async fn stream_chat(
        &self,
        _request: &nuo_protocol::ModelRequest,
        _telemetry: nuo_protocol::TransportTelemetry,
    ) -> Result<
        BoxStream<'static, Result<String, nuo_protocol::ProviderError>>,
        nuo_protocol::ProviderError,
    > {
        Err(nuo_protocol::ProviderError::new(
            "mock",
            nuo_protocol::ProviderErrorKind::Other,
            "stream_chat path not used by this test",
        ))
    }
    async fn stream_chat_events(
        &self,
        _request: &nuo_protocol::ModelRequest,
        _telemetry: nuo_protocol::TransportTelemetry,
    ) -> Result<
        BoxStream<'static, Result<ProviderStreamEvent, nuo_protocol::ProviderError>>,
        nuo_protocol::ProviderError,
    > {
        Ok(Box::pin(futures::stream::iter([
            Ok(ProviderStreamEvent::TextDelta("hel".to_string())),
            Ok(ProviderStreamEvent::TextDelta("lo".to_string())),
        ])))
    }
}

fn capture_dir() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("nuo-capture-{}", uuid::Uuid::new_v4()))
}

#[tokio::test]
async fn debug_trace_writes_one_file_per_chat() {
    use crate::orchestration::ProxyProvider;
    use std::sync::{Arc, RwLock};

    let dir = capture_dir();
    let holder: Arc<RwLock<Arc<dyn Provider>>> = Arc::new(RwLock::new(Arc::new(TestProvider)));
    let proxy = ProxyProvider::new(holder);

    // Off by default, and a call while off writes nothing.
    assert!(!proxy.debug_capture_enabled());
    proxy
        .chat(&nuo_provider::ModelRequest::new(vec![Message::new(
            Role::User,
            "hi",
        )]))
        .await
        .unwrap();
    let off_count = std::fs::read_dir(&dir).map(|entries| entries.count()).ok();
    assert_eq!(off_count, None, "no directory created while capture is off");

    // Arming creates exactly one JSON file per round-trip on the chat path.
    proxy.set_debug_capture(true, dir.clone());
    assert!(proxy.debug_capture_enabled());
    proxy
        .chat(&nuo_provider::ModelRequest::new(vec![Message::new(
            Role::User,
            "hello",
        )]))
        .await
        .unwrap();
    proxy
        .chat(&nuo_provider::ModelRequest::new(vec![Message::new(
            Role::User,
            "again",
        )]))
        .await
        .unwrap();
    let entries: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(entries.len(), 2, "one file per round-trip");
    for entry in entries {
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(entry.path()).unwrap()).unwrap();
        assert_eq!(value["kind"], "chat");
        assert_eq!(value["provider"], "");
        assert_eq!(value["request"]["messages"][0]["role"], "User");
        assert_eq!(value["response"]["items"][0]["status"], "ok");
        assert_eq!(
            value["response"]["items"][0]["completion"]["message"]["role"],
            "Assistant"
        );
        assert_eq!(
            value["response"]["items"][0]["completion"]["message"]["content"],
            "done"
        );
    }

    // Disarming stops further writes.
    proxy.set_debug_capture(false, dir.clone());
    assert!(!proxy.debug_capture_enabled());
    proxy
        .chat(&nuo_provider::ModelRequest::new(vec![Message::new(
            Role::User,
            "after off",
        )]))
        .await
        .unwrap();
    let after: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(after.len(), 2, "no new file after disabling");

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn debug_trace_aggregates_a_full_stream_into_one_file() {
    use crate::orchestration::ProxyProvider;
    use futures::StreamExt;
    use std::sync::{Arc, RwLock};

    let dir = capture_dir();
    let holder: Arc<RwLock<Arc<dyn Provider>>> = Arc::new(RwLock::new(Arc::new(TwoEventProvider)));
    let proxy = ProxyProvider::new(holder);
    proxy.set_debug_capture(true, dir.clone());

    // Drive the stream fully; on completion the wrapper drops and flushes the
    // aggregated record.
    let stream = proxy
        .stream_chat_events(
            &nuo_provider::ModelRequest::new(vec![Message::new(Role::User, "hi")]),
            nuo_provider::TransportTelemetry::default(),
        )
        .await
        .unwrap();
    let items: Vec<_> = stream.collect::<Vec<_>>().await;
    assert_eq!(items.len(), 2);

    let entries: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(entries.len(), 1, "one streaming round-trip -> one file");
    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(entries[0].path()).unwrap()).unwrap();
    assert_eq!(value["kind"], "stream_chat_events");
    let captured = value["response"]["items"].as_array().unwrap();
    assert_eq!(captured.len(), 2);
    assert_eq!(captured[0]["Ok"]["TextDelta"], "hel");
    assert_eq!(captured[1]["Ok"]["TextDelta"], "lo");

    let _ = std::fs::remove_dir_all(&dir);
}

/// ADR-0050: non-driving command echoes are durable + visible on resume/export
/// but must be **projected out before the provider wire**. Model-request
/// assembly is the single pre-wire funnel; this proves echoes are dropped
/// while genuine user prompts and assistant messages survive.
#[test]
fn prepare_request_messages_projects_out_command_echoes() {
    let agent = agent();
    let mut messages: Vec<Message> = vec![
        Message::new(Role::User, "first real prompt"),
        Message::command_echo("/pursue ship it"),
        Message::new(Role::Assistant, "working on it"),
        Message::command_echo("!ls -la"),
        Message::new(Role::User, "second real prompt"),
    ];
    agent.prepare_request_messages_debug(&mut messages);

    // The funnel also prepends a system prompt and removes empty assistant
    // tails; the ADR-0050 concern is specifically the echo projection. Assert
    // no echo leaked through and the driving content survived in order.
    assert!(
        messages.iter().all(|m| !m.is_command_echo()),
        "no command echo must leak through model-request assembly: {messages:?}"
    );
    let contents: Vec<&str> = messages
        .iter()
        .filter(|m| m.role != Role::System)
        .map(|m| m.content.as_str())
        .collect();
    assert_eq!(
        contents,
        vec!["first real prompt", "working on it", "second real prompt"],
        "driving prompts + assistant replies must survive in order, \
         echoes dropped: {contents:?}"
    );
}

#[test]
fn request_pressure_includes_system_prompt_and_tool_schemas() {
    let without_tools = Agent::new(
        Arc::new(TestProvider),
        Vec::new(),
        crate::AgentIdentity::from_directive("test system prompt directive"),
    );
    let with_tools = Agent::new(
        Arc::new(TestProvider),
        vec![Arc::new(WriteTestTool)],
        crate::AgentIdentity::from_directive("test system prompt directive"),
    );
    let messages = vec![Message::new(Role::User, "inspect the request budget")];

    let plain = without_tools.estimate_next_request_tokens(&messages);
    let with_schema = with_tools.estimate_next_request_tokens(&messages);

    assert_eq!(
        plain.total_tokens,
        plain.history_tokens + plain.overhead_tokens
    );
    assert!(
        plain.overhead_tokens > 0,
        "the system prompt is request overhead"
    );
    assert!(
        with_schema.total_tokens > plain.total_tokens,
        "an advertised tool schema must increase projected request pressure"
    );
}

// ToolScheduler dispatch driver (stage-3 pipeline)
//
// Contract-level coverage for the scheduler-driven `schedule` stage: result
// recording stays input-ordered under out-of-order completion, an interrupt
// preserves drained work while pairing unproduced calls with `ToolCancelled`,
// and conflicting writes never overlap in flight.

/// A read-tier tool with a configurable per-call delay, used to control the
/// completion order of a concurrent batch.
struct DelayedReadTool {
    name: &'static str,
    output: String,
    delay: std::time::Duration,
}

#[async_trait]
impl Tool for DelayedReadTool {
    fn name(&self) -> &str {
        self.name
    }
    fn description(&self) -> &str {
        "delayed read probe"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }
    async fn call(&self, _arguments: &str) -> Result<String, String> {
        tokio::time::sleep(self.delay).await;
        Ok(self.output.clone())
    }
}

/// Recording stays input-ordered even when a later call finishes first: the
/// first call sleeps while the second completes, so the live `ToolResult`
/// events arrive out of order, but the transcript messages are appended
/// strictly in call order.
#[tokio::test]
async fn scheduler_records_results_in_input_order_despite_completion_order() {
    let agent = Arc::new(Agent::new(
        Arc::new(ScriptedProvider::new(vec![
            turn(&[("c1", "slow_read", "{}"), ("c2", "fast_read", "{}")]),
            text_turn("done"),
        ])),
        vec![
            Arc::new(DelayedReadTool {
                name: "slow_read",
                output: "S-out".to_string(),
                delay: std::time::Duration::from_millis(120),
            }),
            Arc::new(DelayedReadTool {
                name: "fast_read",
                output: "F-out".to_string(),
                delay: std::time::Duration::ZERO,
            }),
        ],
        crate::AgentIdentity::default(),
    ));
    let mut messages = vec![Message::new(Role::User, "go")];
    let mut events = Vec::new();
    agent
        .run_streaming_with_events(&mut messages, &CancellationToken::new(), |event| {
            events.push(event)
        })
        .await
        .expect("round completes");

    // Live events follow completion order: fast before slow.
    let result_names: Vec<&str> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ToolResult { name, .. } => Some(name.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        result_names,
        vec!["fast_read", "slow_read"],
        "live ToolResult events follow completion order"
    );

    // The recorded transcript is strictly input order: slow (call 1) then
    // fast (call 2).
    let recorded: Vec<&str> = messages
        .iter()
        .filter(|m| m.role == Role::Tool)
        .map(|m| m.content.as_str())
        .collect();
    assert_eq!(recorded.len(), 2, "both results recorded: {recorded:?}");
    assert!(
        recorded[0].starts_with("[slow_read result]:\nS-out"),
        "first recorded message is the first call: {recorded:?}"
    );
    assert!(
        recorded[1].starts_with("[fast_read result]:\nF-out"),
        "second recorded message is the second call: {recorded:?}"
    );
}

/// A turn interrupted mid-batch records the drained subagent's partial result
/// and pairs the never-produced sibling call with `ToolCancelled` — the
/// batch-level counterpart of `execute_tool_evented_drains_interrupted_subagent`.
#[tokio::test]
async fn interrupted_batch_records_subagent_drain_and_cancels_unproduced_calls() {
    use std::future::pending;

    struct BlockingTool {
        started: Arc<tokio::sync::Notify>,
    }

    #[async_trait]
    impl Tool for BlockingTool {
        fn name(&self) -> &str {
            "stream_read"
        }
        fn description(&self) -> &str {
            "blocks until the turn is cancelled"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }
        async fn call(&self, _arguments: &str) -> Result<String, String> {
            self.started.notify_one();
            let _: () = pending().await;
            unreachable!("the turn is cancelled before this returns")
        }
    }

    let (gate_tx, gate_rx) = tokio::sync::watch::channel(false);
    let subagent: Arc<crate::SubagentTool> = Arc::new(crate::SubagentTool::new(
        Arc::new(GatedSubagentProvider {
            requests: AtomicUsize::new(0),
            gate: gate_tx,
        }),
        nuo_protocol::ToolSet::from_tools(vec![Arc::new(SubagentReadTool) as Arc<dyn Tool>]),
        &nuo_protocol::SubAgentProfile::EXPLORE,
    ));
    let started = Arc::new(tokio::sync::Notify::new());
    let agent = Arc::new(Agent::new(
        Arc::new(ScriptedProvider::new(vec![
            turn(&[
                ("c1", "spawn_agent", r#"{"description":"d","prompt":"p"}"#),
                ("c2", "stream_read", "{}"),
            ]),
            text_turn("done"),
        ])),
        vec![
            subagent as Arc<dyn Tool>,
            Arc::new(BlockingTool {
                started: started.clone(),
            }),
        ],
        crate::AgentIdentity::default(),
    ));

    let token = CancellationToken::new();
    let events: Arc<std::sync::Mutex<Vec<AgentEvent>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let events_for_run = events.clone();
    let mut messages = vec![Message::new(Role::User, "go")];
    let run_token = token.clone();
    let handle = tokio::spawn(async move {
        let outcome = agent
            .run_streaming_with_events(&mut messages, &run_token, |event| {
                if let Ok(mut guard) = events_for_run.lock() {
                    guard.push(event);
                }
            })
            .await;
        (outcome, messages)
    });

    // Wait until BOTH calls are genuinely in flight, then interrupt.
    let mut gate_rx = gate_rx;
    gate_rx
        .changed()
        .await
        .expect("subagent reached its stalled second request");
    started.notified().await;
    token.cancel();

    let (outcome, messages) = handle.await.expect("round task panicked");
    assert!(
        matches!(outcome, Err(HarnessError::Interrupted)),
        "expected the turn to be interrupted, got {outcome:?}"
    );

    let recorded = events.lock().unwrap_or_else(|e| e.into_inner()).clone();
    // The subagent drained within the grace period: its ToolResult landed and no
    // ToolCancelled was emitted for it.
    assert!(
        recorded.iter().any(
            |event| matches!(event, AgentEvent::ToolResult { name, .. } if name == "spawn_agent")
        ),
        "drained subagent must emit ToolResult"
    );
    assert!(
        !recorded.iter().any(
            |event| matches!(event, AgentEvent::ToolCancelled { name, .. } if name == "spawn_agent")
        ),
        "a produced (drained) call must never be paired with ToolCancelled"
    );
    // The never-produced blocking call is paired with ToolCancelled and never
    // emitted a ToolResult.
    assert!(recorded.iter().any(
        |event| matches!(event, AgentEvent::ToolCancelled { name, .. } if name == "stream_read")
    ));
    assert!(!recorded.iter().any(
        |event| matches!(event, AgentEvent::ToolResult { name, .. } if name == "stream_read")
    ));
    // The drained work is recorded into the transcript (interrupted
    // re-anchor); the cancelled call is not recorded.
    assert!(
        messages
            .iter()
            .any(|m| m.role == Role::Tool && m.content.contains("interrupted mid-task")),
        "drained subagent result must be recorded: {messages:?}"
    );
    assert!(
        !messages
            .iter()
            .any(|m| m.role == Role::Tool && m.content.starts_with("[stream_read result]")),
        "an unproduced call must not be recorded: {messages:?}"
    );
}

/// Two calls declaring a write access to the same path must never overlap in
/// flight: the scheduler serializes them even though the batch asks for both
/// at once.
#[tokio::test]
async fn scheduler_serializes_conflicting_writes() {
    struct WriteProbe {
        active: Arc<AtomicUsize>,
        max_concurrent: Arc<AtomicUsize>,
        log: Arc<std::sync::Mutex<Vec<&'static str>>>,
    }

    #[async_trait]
    impl Tool for WriteProbe {
        fn name(&self) -> &str {
            "probe_write"
        }
        fn description(&self) -> &str {
            "write probe"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }
        fn scope_target(&self, arguments: &str) -> nuo_protocol::ScopeTarget {
            let path = serde_json::from_str::<serde_json::Value>(arguments)
                .ok()
                .and_then(|v| v.get("path").and_then(|p| p.as_str()).map(str::to_string))
                .unwrap_or_else(|| "/tmp/probe".to_string());
            nuo_protocol::ScopeTarget::Path(std::path::PathBuf::from(path))
        }
        async fn call(&self, _arguments: &str) -> Result<String, String> {
            let prev = self.active.fetch_add(1, Ordering::SeqCst);
            self.max_concurrent.fetch_max(prev + 1, Ordering::SeqCst);
            self.log
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push("enter");
            tokio::time::sleep(std::time::Duration::from_millis(60)).await;
            self.log
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push("exit");
            self.active.fetch_sub(1, Ordering::SeqCst);
            Ok("wrote".to_string())
        }
    }

    let active = Arc::new(AtomicUsize::new(0));
    let max_concurrent = Arc::new(AtomicUsize::new(0));
    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    let probe = WriteProbe {
        active: active.clone(),
        max_concurrent: max_concurrent.clone(),
        log: log.clone(),
    };
    // Distinct argument signatures but the SAME declared access path, so the
    // two calls conflict (the default `accesses` derives `read_write_file`
    // from the Path scope target).
    let agent = Arc::new(Agent::new(
        Arc::new(ScriptedProvider::new(vec![
            turn(&[
                ("c1", "probe_write", r#"{"path":"/tmp/probe","tag":1}"#),
                ("c2", "probe_write", r#"{"path":"/tmp/probe","tag":2}"#),
            ]),
            text_turn("done"),
        ])),
        vec![Arc::new(probe)],
        crate::AgentIdentity::default(),
    ));
    let (events, outcome) = run_golden_round(&agent, "go", PermissionDecision::Once).await;
    outcome.expect("round completes");

    assert_eq!(
        max_concurrent.load(Ordering::SeqCst),
        1,
        "conflicting writes must never overlap in flight"
    );
    assert_eq!(
        log.lock().unwrap_or_else(|e| e.into_inner()).as_slice(),
        ["enter", "exit", "enter", "exit"],
        "the queued write starts only after the first one exits"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, AgentEvent::ToolResult { .. }))
            .count(),
        2,
        "both writes eventually produce a result"
    );
}
