use super::*;

#[tokio::test]
async fn write_tool_waits_for_permission_and_always_is_cached() {
    let agent = Arc::new(Agent::new(
        Arc::new(TestProvider),
        vec![Arc::new(WriteTestTool)],
        crate::AgentIdentity::default(),
    ));
    let call = ToolCall {
        id: "call".to_string(),
        name: "write_test".to_string(),
        arguments: "{}".to_string(),
    };
    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
    let task_agent = agent.clone();
    let task_call = call.clone();
    let task = tokio::spawn(async move {
        task_agent
            .execute_tool_evented(&task_call, &CancellationToken::new(), &mut |event| {
                let _ = event_tx.send(event);
            })
            .await
    });

    let request = match event_rx.recv().await.unwrap() {
        AgentEvent::PermissionRequest(request) => request,
        event => panic!("unexpected event: {:?}", event),
    };
    // The default `permission_label`/`permission_description` fall back to
    // the tool's name/description, which the request must carry verbatim
    // (regression for the `PermissionRequest.label` wiring).
    assert_eq!(request.tool, "write_test");
    assert_eq!(request.label, "write_test");
    assert_eq!(request.description, "test write tool");
    assert!(!task.is_finished());
    assert!(agent.reply_permission(&request.id, PermissionDecision::Always));
    assert_eq!(
        task.await
            .unwrap()
            .unwrap()
            .result
            .expect("non-interrupted outcome carries a result")
            .to_text(),
        "should not run"
    );
    assert_eq!(
        agent.allowed_tools(),
        vec!["write_test /tmp/test".to_string()]
    );

    let mut prompted_again = false;
    let output = agent
        .execute_tool_evented(&call, &CancellationToken::new(), &mut |event| {
            if matches!(event, AgentEvent::PermissionRequest(_)) {
                prompted_again = true;
            }
        })
        .await
        .unwrap()
        .result
        .expect("non-interrupted outcome carries a result");
    assert_eq!(output.to_text(), "should not run");
    assert!(!prompted_again);
}

#[tokio::test]
async fn rejected_permission_does_not_execute_tool() {
    let agent = Arc::new(Agent::new(
        Arc::new(TestProvider),
        vec![Arc::new(WriteTestTool)],
        crate::AgentIdentity::default(),
    ));
    let call = ToolCall {
        id: "call".to_string(),
        name: "write_test".to_string(),
        arguments: "{}".to_string(),
    };
    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
    let task_agent = agent.clone();
    let task = tokio::spawn(async move {
        task_agent
            .execute_tool_evented(&call, &CancellationToken::new(), &mut |event| {
                let _ = event_tx.send(event);
            })
            .await
    });

    let request = match event_rx.recv().await.unwrap() {
        AgentEvent::PermissionRequest(request) => request,
        event => panic!("unexpected event: {:?}", event),
    };
    assert!(agent.reply_permission(&request.id, PermissionDecision::Reject));
    assert!(
        task.await
            .unwrap()
            .unwrap()
            .result
            .expect("non-interrupted outcome carries a result")
            .to_text()
            .contains("Permission denied")
    );
}

/// The executor's cooperative drain: when the user cancels a turn while an
/// subagent is in flight, `execute_tool_evented` signals the subagent, waits for it
/// to return its partial transcript, and reports the recovered result with
/// `interrupted: true` — instead of dropping the future and losing the work.
#[tokio::test]
async fn execute_tool_evented_drains_interrupted_subagent() {
    let (gate_tx, gate_rx) = tokio::sync::watch::channel(false);
    let subagent: Arc<crate::SubagentTool> = Arc::new(crate::SubagentTool::new(
        Arc::new(GatedSubagentProvider {
            requests: AtomicUsize::new(0),
            gate: gate_tx,
        }),
        nuo_protocol::ToolSet::from_tools(vec![Arc::new(SubagentReadTool) as Arc<dyn Tool>]),
        &nuo_protocol::SubAgentProfile::EXPLORE,
    ));
    let agent = Arc::new(Agent::new(
        Arc::new(TestProvider),
        vec![subagent.clone() as Arc<dyn Tool>],
        crate::AgentIdentity::default(),
    ));

    let cancel = CancellationToken::new();
    let call = ToolCall {
        id: "call_subagent".to_string(),
        name: "spawn_agent".to_string(),
        arguments: r#"{"description":"d","prompt":"p"}"#.to_string(),
    };
    let agent_for_run = agent.clone();
    let cancel_for_run = cancel.clone();
    let task = tokio::spawn(async move {
        agent_for_run
            .execute_tool_evented(&call, &cancel_for_run, &mut |_event| {})
            .await
    });

    // Wait until the subagent is genuinely mid-flight, then interrupt the turn.
    let mut gate_rx = gate_rx;
    gate_rx
        .changed()
        .await
        .expect("subagent reached second request");
    cancel.cancel();

    let outcome = task
        .await
        .expect("executor task")
        .expect("no harness error");
    assert!(outcome.interrupted, "interruption must be reported");
    let result = outcome.result.expect("drained result must be recovered");
    match result {
        ToolOutput::Subagent {
            interrupted,
            failed,
            messages,
            ..
        } => {
            assert!(
                interrupted,
                "recovered subagent must be flagged interrupted"
            );
            assert!(!failed, "interruption is not a failure");
            assert_eq!(
                messages.iter().filter(|m| m.role == Role::Tool).count(),
                1,
                "the child's completed tool call must survive the drain"
            );
        }
        other => panic!("expected a drained Subagent output, got {other:?}"),
    }
}

#[tokio::test]
async fn headless_run_rejects_write_tools_without_hanging() {
    let agent = Arc::new(Agent::new(
        Arc::new(PermissionTestProvider(AtomicUsize::new(0))),
        vec![Arc::new(WriteTestTool)],
        crate::AgentIdentity::default(),
    ));
    let mut messages = vec![Message::new(Role::User, "write something")];

    // Non-interactive event handling: permission requests are rejected and
    // user questions get an empty answer, so nothing can park on a human.
    let outcome = agent
        .run_streaming_with_events(
            &mut messages,
            &CancellationToken::new(),
            |event| match event {
                AgentEvent::PermissionRequest(request) => {
                    agent.reply_permission(&request.id, PermissionDecision::Reject);
                }
                AgentEvent::UserQuestionRequest(request) => {
                    agent.reply_user_question(&request.id, Vec::new());
                }
                _ => {}
            },
        )
        .await
        .unwrap();

    // Permission rejection now terminates the turn instead of letting the
    // model continue, so the final assistant message is empty.
    assert!(outcome.message.content.is_empty());
    assert!(
        messages
            .iter()
            .any(|message| message.content.contains("Permission denied"))
    );
}

/// A call whose arguments violate the tool's declared `parameters` schema is
/// rejected by dispatch-level pre-validation with the same error shape a
/// failing tool returns — and the Tool impl never runs (the recording mock
/// stays empty). A well-formed call still passes the gate and executes.
#[tokio::test]
async fn schema_violating_call_never_reaches_the_tool() {
    use std::sync::Mutex;

    struct StrictReadTool {
        calls: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl Tool for StrictReadTool {
        fn name(&self) -> &str {
            "strict_read"
        }
        fn description(&self) -> &str {
            "recording tool with a typed schema"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "limit": { "type": "integer" }
                },
                "required": ["path"]
            })
        }
        async fn call(&self, arguments: &str) -> Result<String, String> {
            self.calls
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(arguments.to_string());
            Ok("ran".to_string())
        }
    }

    async fn run(agent: &Agent, arguments: &str) -> ToolOutput {
        agent
            .execute_tool_evented(
                &ToolCall {
                    id: "call".to_string(),
                    name: "strict_read".to_string(),
                    arguments: arguments.to_string(),
                },
                &CancellationToken::new(),
                &mut |_| {},
            )
            .await
            .expect("dispatch should not fail")
            .result
            .expect("non-interrupted outcome carries a result")
    }

    let calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::new(
        Arc::new(TestProvider),
        vec![Arc::new(StrictReadTool {
            calls: calls.clone(),
        })],
        crate::AgentIdentity::default(),
    );

    // Wrong primitive type for a declared property.
    let output = run(&agent, r#"{"path": "f.rs", "limit": "soon"}"#).await;
    assert!(
        matches!(output, ToolOutput::Error { .. }),
        "schema violation must produce ToolOutput::Error, got {output:?}"
    );
    let text = output.to_text();
    assert!(text.contains("invalid argument `limit`"), "{text}");

    // Missing a required field.
    let output = run(&agent, r#"{"limit": 3}"#).await;
    assert!(matches!(output, ToolOutput::Error { .. }));
    assert!(
        output
            .to_text()
            .contains("missing required field(s): `path`")
    );

    // Wrong top-level type.
    let output = run(&agent, "[1, 2]").await;
    assert!(matches!(output, ToolOutput::Error { .. }));

    assert!(
        calls.lock().unwrap_or_else(|e| e.into_inner()).is_empty(),
        "no schema-violating call may reach the tool"
    );

    // A well-formed call still runs, proving the gate only rejects violations.
    let output = run(&agent, r#"{"path": "f.rs", "limit": 3}"#).await;
    assert_eq!(output.to_text(), "ran");
    assert_eq!(calls.lock().unwrap_or_else(|e| e.into_inner()).len(), 1);
}
