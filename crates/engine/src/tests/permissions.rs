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
        protocol::ToolSet::from_tools(vec![Arc::new(SubagentReadTool) as Arc<dyn Tool>]),
        &protocol::SubAgentProfile::EXPLORE,
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
    let _outcome = agent
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

    // Permission rejection is a tool observation; the model continues and the
    // denied call is recorded in history.
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
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "limit": { "type": "integer" }
                },
                "required": ["path"]
            })
        }
        async fn execute(
            &self,
            _ctx: &::tool::ToolContext,
            arguments: serde_json::Value,
        ) -> Result<::tool::ToolOutput, ::tool::ToolError> {
            self.calls
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(arguments.to_string());
            Ok(::tool::ToolOutput::text("ran".to_string()))
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
    assert!(text.contains("limit"), "{text}");

    // Missing a required field.
    let output = run(&agent, r#"{"limit": 3}"#).await;
    assert!(matches!(output, ToolOutput::Error { .. }));
    assert!(output.to_text().contains("path") && output.to_text().contains("required"));

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

struct PathBoundReadTool;

#[async_trait]
impl Tool for PathBoundReadTool {
    fn name(&self) -> &str {
        "path_bound_read"
    }

    fn description(&self) -> &str {
        "read-only path-bound test tool"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" }
            },
            "required": ["path"]
        })
    }

    fn risk_profile(&self) -> tool::RiskProfile {
        tool::RiskProfile::ReadOnly
    }

    fn path_bound(&self) -> bool {
        true
    }

    async fn execute(
        &self,
        _ctx: &tool::ToolContext,
        args: serde_json::Value,
    ) -> Result<ToolOutput, tool::ToolError> {
        let path = args["path"].as_str().unwrap_or_default();
        Ok(ToolOutput::Text(format!("read:{path}")))
    }
}

fn cross_workspace_scratch(tag: &str) -> std::path::PathBuf {
    let base = std::env::current_dir()
        .unwrap()
        .join("target/test-scratch/cross-workspace-prompt")
        .join(format!("{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    base
}

#[tokio::test]
async fn cross_workspace_prompt_session_admits_without_persisting() {
    let base = cross_workspace_scratch("session");
    let primary = base.join("primary");
    let external = base.join("external");
    std::fs::create_dir_all(&primary).unwrap();
    std::fs::create_dir_all(external.join(".git")).unwrap();
    std::fs::create_dir_all(external.join("src")).unwrap();
    let target = external.join("src/lib.rs");
    std::fs::write(&target, "pub fn hi() {}").unwrap();

    let shared_roots = protocol::SharedAdditionalRoots::empty();
    let mut agent = Agent::new(
        Arc::new(TestProvider),
        vec![Arc::new(PathBoundReadTool)],
        crate::AgentIdentity::default(),
    );
    agent.set_permission_scope("developer", Some(primary.clone()));
    agent.bind_shared_additional_roots(shared_roots.clone());
    let agent = Arc::new(agent);

    let call = ToolCall {
        id: "cw-session".to_string(),
        name: "path_bound_read".to_string(),
        arguments: serde_json::json!({ "path": target.to_string_lossy() }).to_string(),
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
        AgentEvent::PermissionRequest(req) => req,
        other => panic!("expected PermissionRequest, got {other:?}"),
    };
    assert!(request.elevation);
    assert_eq!(request.label, "Cross-workspace access");
    assert_eq!(
        request.suggested_scope.as_deref(),
        Some(std::fs::canonicalize(&external).unwrap().to_string_lossy().as_ref())
    );

    assert!(agent.reply_permission(&request.id, PermissionDecision::Session));
    let output = task
        .await
        .unwrap()
        .unwrap()
        .result
        .expect("result");
    assert!(output.to_text().starts_with("read:"), "{output:?}");

    // Admitted in live SharedAdditionalRoots, but `.nuo/config.toml` was NOT created.
    assert_eq!(
        shared_roots.snapshot(),
        vec![std::fs::canonicalize(&external).unwrap()]
    );
    assert!(!primary.join(".nuo/config.toml").exists());

    // Second call into the same external project does not prompt again.
    let mut prompted_again = false;
    let second = agent
        .execute_tool_evented(&call, &CancellationToken::new(), &mut |event| {
            if matches!(event, AgentEvent::PermissionRequest(_)) {
                prompted_again = true;
            }
        })
        .await
        .unwrap()
        .result
        .unwrap();
    assert!(second.to_text().starts_with("read:"));
    assert!(!prompted_again);

    let _ = std::fs::remove_dir_all(&base);
}

#[tokio::test]
async fn cross_workspace_prompt_always_persists_to_dot_nuo_and_reject_refuses() {
    let tmp_dirs = tempfile::tempdir().unwrap();
    let _dirs = host::paths::TestDirsGuard::install(host::paths::Dirs {
        config_dir: tmp_dirs.path().join("config"),
        data_dir: tmp_dirs.path().join("data"),
        state_dir: tmp_dirs.path().join("state"),
        cache_dir: tmp_dirs.path().join("cache"),
        runtime_dir: None,
    });
    let base = cross_workspace_scratch("persist");
    let primary = base.join("primary");
    let external = base.join("external");
    let denied_dir = base.join("denied");
    std::fs::create_dir_all(&primary).unwrap();
    std::fs::create_dir_all(&external).unwrap();
    std::fs::create_dir_all(&denied_dir).unwrap();
    let allowed_file = external.join("README.md");
    let denied_file = denied_dir.join("secret.md");
    std::fs::write(&allowed_file, "ok").unwrap();
    std::fs::write(&denied_file, "no").unwrap();

    let shared_roots = protocol::SharedAdditionalRoots::empty();
    let mut agent = Agent::new(
        Arc::new(TestProvider),
        vec![Arc::new(PathBoundReadTool)],
        crate::AgentIdentity::default(),
    );
    agent.set_permission_scope("developer", Some(primary.clone()));
    agent.bind_shared_additional_roots(shared_roots.clone());
    let agent = Arc::new(agent);

    // 1. Always -> persists to `<primary>/.nuo/config.toml` and trusts `ExWorkspace`.
    let call = ToolCall {
        id: "cw-persist".to_string(),
        name: "path_bound_read".to_string(),
        arguments: serde_json::json!({ "path": allowed_file.to_string_lossy() }).to_string(),
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
        AgentEvent::PermissionRequest(req) => req,
        other => panic!("expected PermissionRequest, got {other:?}"),
    };
    assert!(agent.reply_permission(&request.id, PermissionDecision::Always));
    let output = task.await.unwrap().unwrap().result.unwrap();
    assert!(output.to_text().starts_with("read:"));

    let persisted = persistence::config::Config::load_project_additional_roots(&primary);
    assert_eq!(persisted, vec!["../external"]);
    assert_eq!(
        agent.workspace_security().ex_workspace,
        protocol::WorkspaceTrustState::Trusted
    );

    // 2. Reject -> returns error and does not admit `denied_dir`.
    let deny_call = ToolCall {
        id: "cw-deny".to_string(),
        name: "path_bound_read".to_string(),
        arguments: serde_json::json!({ "path": denied_file.to_string_lossy() }).to_string(),
    };
    let (deny_tx, mut deny_rx) = tokio::sync::mpsc::unbounded_channel();
    let task_agent = agent.clone();
    let deny_task = tokio::spawn(async move {
        task_agent
            .execute_tool_evented(&deny_call, &CancellationToken::new(), &mut |event| {
                let _ = deny_tx.send(event);
            })
            .await
    });
    let deny_req = match deny_rx.recv().await.unwrap() {
        AgentEvent::PermissionRequest(req) => req,
        other => panic!("expected PermissionRequest, got {other:?}"),
    };
    assert!(agent.reply_permission(&deny_req.id, PermissionDecision::Reject));
    let deny_out = deny_task.await.unwrap().unwrap().result.unwrap();
    assert!(matches!(deny_out, ToolOutput::Error { .. }));
    assert!(!shared_roots.snapshot().contains(&std::fs::canonicalize(&denied_dir).unwrap()));

    let _ = std::fs::remove_dir_all(&base);
}
