use super::*;

#[tokio::test]
async fn golden_native_tool_turn_then_final_text() {
    let agent = Arc::new(Agent::new(
        Arc::new(ScriptedProvider::new(vec![
            turn(&[("c1", "alpha", "{\"k\":1}"), ("c2", "beta", "{\"k\":2}")]),
            text_turn("all done"),
        ])),
        vec![
            Arc::new(RecordingTool::read("alpha", "A-out")),
            Arc::new(RecordingTool::read("beta", "B-out")),
        ],
        crate::AgentIdentity::default(),
    ));

    let (events, outcome) = run_golden_round(&agent, "go", PermissionDecision::Reject).await;

    assert_eq!(outcome.unwrap().message.content, "all done");
    // Calls are announced up front, then results land in input (FIFO) order
    // regardless of concurrent execution.
    assert_eq!(
        transcript(&events),
        vec![
            "model-request turn=0",
            "context-tokens",
            "tool-call-started c1 alpha",
            "tool-call-started c2 beta",
            "tool-call alpha {\"k\":1}",
            "tool-call beta {\"k\":2}",
            "tool-result alpha \"A-out\"",
            "tool-result beta \"B-out\"",
            "model-request turn=1",
            "context-tokens",
            "assistant-delta start=true \"all done\"",
            "assistant-end \"all done\"",
        ]
    );
}

#[tokio::test]
async fn golden_text_fallback_tool_call_is_discarded_then_dispatched() {
    let agent = Arc::new(Agent::new(
        Arc::new(ScriptedProvider::new(vec![
            text_turn("{\"tool\":\"alpha\",\"arguments\":{\"k\":1}}"),
            text_turn("finished"),
        ])),
        vec![Arc::new(RecordingTool::read("alpha", "A-out"))],
        crate::AgentIdentity::default(),
    ));

    let (events, outcome) = run_golden_round(&agent, "go", PermissionDecision::Reject).await;

    assert_eq!(outcome.unwrap().message.content, "finished");
    // The streamed JSON is shown, then discarded once recognised as a tool
    // call, so the UI never leaves raw tool JSON on screen.
    assert_eq!(
        transcript(&events),
        vec![
            "model-request turn=0",
            "context-tokens",
            "assistant-delta start=true \"{\\\"tool\\\":\\\"alpha\\\",\\\"arguments\\\":{\\\"k\\\":1}}\"",
            "assistant-end \"{\\\"tool\\\":\\\"alpha\\\",\\\"arguments\\\":{\\\"k\\\":1}}\"",
            "assistant-discard",
            "tool-call alpha {\"k\":1}",
            "tool-result alpha \"A-out\"",
            "model-request turn=1",
            "context-tokens",
            "assistant-delta start=true \"finished\"",
            "assistant-end \"finished\"",
        ]
    );
}

#[tokio::test]
async fn golden_repeated_identical_tool_calls_run_without_hard_abort() {
    // The equality-guard hard abort was removed in favour of the soft
    // loop-review intervention. Identical calls now all execute; the turn
    // ends when the model stops calling tools (the scripted provider runs
    // out of turns).
    let tool = RecordingTool::read("alpha", "A-out");
    let calls = tool.calls_handle();
    let identical = || turn(&[("c", "alpha", "{}")]);
    let agent = Arc::new(Agent::new(
        Arc::new(ScriptedProvider::new(vec![
            identical(),
            identical(),
            identical(),
            identical(),
        ])),
        vec![Arc::new(tool)],
        crate::AgentIdentity::default(),
    ));
    agent.set_trajectory_guard_config(nuo_protocol::TrajectoryGuardConfig::disabled());

    let (_events, outcome) = run_golden_round(&agent, "go", PermissionDecision::Reject).await;

    // No hard abort — all 4 turns execute.
    assert_eq!(calls.lock().unwrap().len(), 4);
    // The round completes normally (provider exhausts its turns).
    let _ = outcome.unwrap();
}

/// End-to-end: the trajectory guard intercepts a repeating command before it
/// executes. The blocked repeat's side effect and output never enter context.
#[tokio::test]
async fn trajectory_guard_blocks_repeating_command_before_execution() {
    // Tool name is `execute_command` so it lands in the trajectory guard's watched set; the
    // command locator makes identical calls share a signature.
    let command = RecordingTool::read("execute_command", "COMMAND-OUT");
    let calls = command.calls_handle();
    let cmd = || turn(&[("c", "execute_command", r#"{"command":"make test"}"#)]);
    let agent = Arc::new(Agent::new(
        Arc::new(ScriptedProvider::new(vec![
            cmd(), // 1st: allowed, executes
            cmd(), // 2nd: same-signature re-run tolerated, executes
            cmd(), // 3rd: threshold reached → blocked before execution
            text_turn("done"),
        ])),
        vec![Arc::new(command)],
        crate::AgentIdentity::default(),
    ));
    agent.set_trajectory_guard_config(nuo_protocol::TrajectoryGuardConfig {
        enabled: true,
        threshold: 3,
        cognitive_review: false,
        ..nuo_protocol::TrajectoryGuardConfig::default()
    });
    agent.seed_permissions_from_config(&[nuo_persistence::config::PermissionRuleConfig {
        tool: "execute_command".to_string(),
        scope: "make test".to_string(),
    }]);

    let mut messages = vec![Message::new(Role::User, "go")];
    let outcome = agent
        .run_streaming_with_events(&mut messages, &CancellationToken::new(), |_| {})
        .await;
    assert_eq!(outcome.unwrap().message.content, "done");

    // The tool body ran exactly twice — the tolerated re-run (2nd) executed,
    // the 3rd call was intercepted pre-dispatch.
    let executed = calls.lock().unwrap().len();
    assert_eq!(
        executed, 2,
        "one re-run is tolerated, the third identical call must be blocked before execution; tool ran {executed} times"
    );

    // The model received a [loop guard] refusal for the blocked call.
    let blocked: Vec<&Message> = messages
        .iter()
        .filter(|m| m.role == Role::Tool && m.content.contains("[loop guard]"))
        .collect();
    assert!(
        !blocked.is_empty(),
        "the blocked command call must surface a [loop guard] result to the model"
    );

    // A steering note was injected explaining the block.
    let nudge = messages
        .iter()
        .find(|m| m.origin.as_ref().map(|o| o.kind) == Some(InjectionKind::LoopReviewNudge));
    assert!(
        nudge.is_some(),
        "the doom guard must inject a steering note alongside the block"
    );
}

/// End-to-end: the doom block is surgical. A call blocked for `big.rs` does NOT
/// block a different tool or a different file in the same turn — the model can
/// still make progress, which is exactly the behavior that lets it recover.
#[tokio::test]
async fn doom_block_is_surgical_across_files() {
    // Two distinct watched tools so the ToolSet routes them separately; the
    // block is keyed on the full signature (`name|path`), so neither a
    // different file under the same tool nor a different tool is masked.
    let reader = RecordingTool::read("read", "READ");
    let reader_calls = reader.calls_handle();
    let finder = RecordingTool::read("find_files", "FIND");
    let finder_calls = finder.calls_handle();
    // Three reads of big.rs (3rd reaches the default threshold → blocked),
    // then a read of small.rs (must succeed — different path), then a find_files
    // (must succeed — different tool), then done.
    let read_big = || turn(&[("c", "read", r#"{"path":"big.rs"}"#)]);
    let read_small = || turn(&[("c", "read", r#"{"path":"small.rs"}"#)]);
    let find = || turn(&[("c", "find_files", r#"{"path":"."}"#)]);
    let agent = Arc::new(Agent::new(
        Arc::new(ScriptedProvider::new(vec![
            read_big(),
            read_big(),
            read_big(),
            read_small(),
            find(),
            text_turn("done"),
        ])),
        vec![Arc::new(reader), Arc::new(finder)],
        crate::AgentIdentity::default(),
    ));
    agent.set_trajectory_guard_config(nuo_protocol::TrajectoryGuardConfig {
        enabled: true,
        threshold: 3,
        cognitive_review: false,
        ..nuo_protocol::TrajectoryGuardConfig::default()
    });

    let mut messages = vec![Message::new(Role::User, "go")];
    let outcome = agent
        .run_streaming_with_events(&mut messages, &CancellationToken::new(), |_| {})
        .await;
    assert_eq!(outcome.unwrap().message.content, "done");

    // The reader ran exactly twice (big.rs 1st + tolerated 2nd; 3rd blocked)
    // and the small.rs read went through the same tool unblocked — so
    // reader_calls is 3.
    assert_eq!(
        reader_calls.lock().unwrap().len(),
        3,
        "big.rs twice (one tolerated re-run) + small.rs once; the third repeat is blocked, the new file is not"
    );
    // The different tool is entirely outside the big.rs block mask.
    assert_eq!(
        finder_calls.lock().unwrap().len(),
        1,
        "a different tool must not be blocked by a big.rs block"
    );
    // And the small.rs read returned its real content, not a block error.
    assert!(
        messages.iter().any(|m| m.content.contains("READ")),
        "the unblocked read should return its real content"
    );
}

/// The trajectory guard is disabled: a repeating call is neither blocked nor injected.
#[tokio::test]
async fn trajectory_guard_suppressed_when_disabled() {
    let cmd = || turn(&[("c", "execute_command", r#"{"command":"make test"}"#)]);
    let agent = Arc::new(Agent::new(
        Arc::new(ScriptedProvider::new(vec![
            cmd(),
            cmd(),
            cmd(),
            text_turn("done"),
        ])),
        vec![Arc::new(RecordingTool::read(
            "execute_command",
            "COMMAND-OUT",
        ))],
        crate::AgentIdentity::default(),
    ));
    agent.set_trajectory_guard_config(nuo_protocol::TrajectoryGuardConfig::disabled());
    agent.seed_permissions_from_config(&[nuo_persistence::config::PermissionRuleConfig {
        tool: "execute_command".to_string(),
        scope: "make test".to_string(),
    }]);

    let mut messages = vec![Message::new(Role::User, "go")];
    let _ = agent
        .run_streaming_with_events(&mut messages, &CancellationToken::new(), |_| {})
        .await;

    // Disabled guard injects no steering note and blocks nothing.
    assert!(
        messages
            .iter()
            .all(|m| m.origin.as_ref().map(|o| &o.kind) != Some(&InjectionKind::LoopReviewNudge)),
        "disabled guard must not inject"
    );
    assert!(
        !messages
            .iter()
            .any(|m| m.role == Role::Tool && m.content.contains("[loop guard]")),
        "disabled guard must not block"
    );
}

/// Trajectory Loop Guard with Cognitive Arbitration confirms loop and blocks surgically.
#[tokio::test]
async fn trajectory_guard_cognitive_arbitration_confirms_loop_and_blocks_surgically() {
    let command = RecordingTool::read("execute_command", "COMMAND-OUT");
    let calls = command.calls_handle();
    let cmd = || turn(&[("c", "execute_command", r#"{"command":"cargo test"}"#)]);
    let agent = Arc::new(Agent::new(
        Arc::new(
            ScriptedProvider::new(vec![
                cmd(), // 1st
                cmd(), // 2nd
                cmd(), // 3rd
                cmd(), // 4th: trips Tier 1 (threshold 4) -> Steward confirms loop ("yes") -> blocked
                text_turn("done"),
            ])
            .with_cognitive_replies(&["yes"]),
        ),
        vec![Arc::new(command)],
        crate::AgentIdentity::default(),
    ));
    agent.set_trajectory_guard_config(nuo_protocol::TrajectoryGuardConfig::cognitive());
    agent.seed_permissions_from_config(&[nuo_persistence::config::PermissionRuleConfig {
        tool: "execute_command".to_string(),
        scope: "cargo test".to_string(),
    }]);

    let mut messages = vec![Message::new(Role::User, "run tests")];
    let outcome = agent
        .run_streaming_with_events(&mut messages, &CancellationToken::new(), |_| {})
        .await;
    assert_eq!(outcome.unwrap().message.content, "done");

    // First 3 calls execute; the 4th is confirmed as a loop and blocked pre-dispatch.
    let executed = calls.lock().unwrap().len();
    assert_eq!(
        executed, 3,
        "first 3 calls must execute; 4th must be blocked upon cognitive confirmation"
    );

    let blocked: Vec<&Message> = messages
        .iter()
        .filter(|m| m.role == Role::Tool && m.content.contains("[loop guard]"))
        .collect();
    assert!(
        !blocked.is_empty(),
        "blocked call must receive loop guard notice"
    );
}

/// Trajectory Loop Guard with Cognitive Arbitration acquits and escalates ladder.
#[tokio::test]
async fn trajectory_guard_cognitive_arbitration_acquits_and_escalates_ladder() {
    let command = RecordingTool::read("execute_command", "COMMAND-OUT");
    let calls = command.calls_handle();
    let cmd = || turn(&[("c", "execute_command", r#"{"command":"cargo test"}"#)]);
    let agent = Arc::new(Agent::new(
        Arc::new(
            ScriptedProvider::new(vec![
                cmd(), // 1st
                cmd(), // 2nd
                cmd(), // 3rd
                cmd(), // 4th: trips Tier 1 (threshold 4) -> Steward acquits ("no") -> executes!
                text_turn("done"),
            ])
            .with_cognitive_replies(&["no"]),
        ),
        vec![Arc::new(command)],
        crate::AgentIdentity::default(),
    ));
    agent.set_trajectory_guard_config(nuo_protocol::TrajectoryGuardConfig::cognitive());
    agent.seed_permissions_from_config(&[nuo_persistence::config::PermissionRuleConfig {
        tool: "execute_command".to_string(),
        scope: "cargo test".to_string(),
    }]);

    let mut messages = vec![Message::new(Role::User, "run tests")];
    let outcome = agent
        .run_streaming_with_events(&mut messages, &CancellationToken::new(), |_| {})
        .await;
    assert_eq!(outcome.unwrap().message.content, "done");

    // All 4 calls execute because cognitive arbiter acquitted the 4th call, escalating the ladder to tier 2 (8).
    let executed = calls.lock().unwrap().len();
    assert_eq!(
        executed, 4,
        "all 4 calls must execute because cognitive arbiter acquitted repetition"
    );

    let blocked: Vec<&Message> = messages
        .iter()
        .filter(|m| m.role == Role::Tool && m.content.contains("[loop guard]"))
        .collect();
    assert!(
        blocked.is_empty(),
        "no call should be blocked when acquitted"
    );
}

/// Trajectory Loop Guard fails open on provider error [INV-LOOP-02] and escalates ladder.
#[tokio::test]
async fn trajectory_guard_fail_open_on_provider_error_and_escalates() {
    let command = RecordingTool::read("execute_command", "COMMAND-OUT");
    let calls = command.calls_handle();
    let cmd = || turn(&[("c", "execute_command", r#"{"command":"cargo test"}"#)]);
    let agent = Arc::new(Agent::new(
        Arc::new(
            // ScriptedProvider has NO cognitive replies configured -> provider chat will error.
            // By [INV-LOOP-02], it must fail open, not deadlock, and escalate the ladder.
            ScriptedProvider::new(vec![
                cmd(), // 1st
                cmd(), // 2nd
                cmd(), // 3rd
                cmd(), // 4th: trips Tier 1 (4) -> Steward consult errors -> fails open -> executes!
                text_turn("done"),
            ]),
        ),
        vec![Arc::new(command)],
        crate::AgentIdentity::default(),
    ));
    agent.set_trajectory_guard_config(nuo_protocol::TrajectoryGuardConfig::cognitive());
    agent.seed_permissions_from_config(&[nuo_persistence::config::PermissionRuleConfig {
        tool: "execute_command".to_string(),
        scope: "cargo test".to_string(),
    }]);

    let mut messages = vec![Message::new(Role::User, "run tests")];
    let outcome = agent
        .run_streaming_with_events(&mut messages, &CancellationToken::new(), |_| {})
        .await;
    assert_eq!(outcome.unwrap().message.content, "done");

    // The 4th call executes safely due to fail-open.
    let executed = calls.lock().unwrap().len();
    assert_eq!(
        executed, 4,
        "all 4 calls must execute because cognitive error fails open"
    );
}

#[tokio::test]
async fn command_policy_confirm_auto_approves_when_unattended() {
    let command = RecordingTool::read("execute_command", "COMMAND-OUT");
    let calls = command.calls_handle();
    let agent = Arc::new(Agent::new(
        Arc::new(ScriptedProvider::new(vec![
            turn(&[("c", "execute_command", r#"{"command":"git reset --hard"}"#)]),
            text_turn("done"),
        ])),
        vec![Arc::new(command)],
        crate::AgentIdentity::default(),
    ));
    agent.set_unattended(true);

    let mut messages = vec![Message::new(Role::User, "discard changes")];
    let outcome = agent
        .run_streaming_with_events(&mut messages, &CancellationToken::new(), |_| {})
        .await;

    assert_eq!(calls.lock().unwrap().len(), 1);
    let tool_message = messages
        .iter()
        .find(|m| m.role == Role::Tool)
        .expect("command tool result should be recorded");
    assert!(tool_message.content.contains("COMMAND-OUT"));
    assert!(outcome.unwrap().message.content.contains("done"));
}

#[tokio::test]
async fn command_policy_user_allow_overrides_builtin_confirm() {
    let command = RecordingTool::read("execute_command", "COMMAND-OUT");
    let calls = command.calls_handle();
    let agent = Arc::new(Agent::new(
        Arc::new(ScriptedProvider::new(vec![
            turn(&[("c", "execute_command", r#"{"command":"git reset --hard"}"#)]),
            text_turn("done"),
        ])),
        vec![Arc::new(command)],
        crate::AgentIdentity::default(),
    ));
    agent.seed_permissions_from_config(&[nuo_persistence::config::PermissionRuleConfig {
        tool: "execute_command".to_string(),
        scope: "git reset --hard".to_string(),
    }]);
    let mut config = nuo_persistence::config::BashPolicyConfig::default();
    config
        .rules
        .push(nuo_persistence::config::BashPolicyRuleConfig {
            name: "fixture allows reset".to_string(),
            matcher: nuo_persistence::config::BashPolicyMatcherConfig::Contains,
            pattern: "git reset --hard".to_string(),
            action: nuo_persistence::config::BashPolicyActionConfig::Allow,
            reason: None,
        });
    agent.set_bash_policy(&config);

    let mut messages = vec![Message::new(Role::User, "discard changes")];
    let outcome = agent
        .run_streaming_with_events(&mut messages, &CancellationToken::new(), |_| {})
        .await;

    assert_eq!(calls.lock().unwrap().len(), 1);
    assert_eq!(outcome.unwrap().message.content, "done");
}

#[tokio::test]
async fn golden_rejected_write_tool_terminates_round() {
    let tool = RecordingTool::write("writer", "WROTE");
    let calls = tool.calls_handle();
    let agent = Arc::new(Agent::new(
        Arc::new(ScriptedProvider::new(vec![
            turn(&[("c1", "writer", "{\"path\":\"x\"}")]),
            text_turn("stopped"),
        ])),
        vec![Arc::new(tool)],
        crate::AgentIdentity::default(),
    ));

    let (events, outcome) = run_golden_round(&agent, "go", PermissionDecision::Reject).await;

    // The round ends immediately after the denied permission; the second
    // ReAct turn ("stopped") is never reached.
    assert_eq!(outcome.unwrap().message.content, "");
    assert!(
        calls.lock().unwrap().is_empty(),
        "rejected write tool must not execute"
    );
    let lines = transcript(&events);
    assert!(
        lines
            .iter()
            .any(|line| line == "permission-request writer x")
    );
    assert!(
        lines.iter().any(
            |line| line.starts_with("tool-result writer") && line.contains("Permission denied")
        )
    );
}

#[tokio::test]
async fn golden_reasoning_precedes_text_in_the_same_turn() {
    let agent = Arc::new(Agent::new(
        Arc::new(ScriptedProvider::new(vec![vec![
            ProviderStreamEvent::ReasoningDelta("think".to_string()),
            ProviderStreamEvent::TextDelta("answer".to_string()),
        ]])),
        Vec::new(),
        crate::AgentIdentity::default(),
    ));

    let (events, outcome) = run_golden_round(&agent, "go", PermissionDecision::Reject).await;

    assert_eq!(outcome.unwrap().message.content, "answer");
    // Deltas surface in stream-arrival order (reasoning first here), but the
    // round closes with AssistantEnd before ReasoningEnd.
    assert_eq!(
        transcript(&events),
        vec![
            "model-request turn=0",
            "context-tokens",
            "reasoning-delta start=true \"think\"",
            "assistant-delta start=true \"answer\"",
            "assistant-end \"answer\"",
            "reasoning-end \"think\"",
        ]
    );
}

#[tokio::test]
async fn ask_user_tool_blocks_and_returns_selected_answers() {
    let ask_args = serde_json::json!({
        "questions": [{
            "header": "style",
            "question": "Which error handling style?",
            "options": [
                { "label": "anyhow (Recommended)", "description": "Simple" },
                { "label": "thiserror", "description": "Structured" }
            ],
            "multi_select": false
        }]
    });
    let agent = Arc::new(Agent::new(
        Arc::new(ScriptedProvider::new(vec![
            turn(&[("c1", "ask_user", &ask_args.to_string())]),
            text_turn("done"),
        ])),
        vec![Arc::new(crate::tools::AskUserTool)],
        crate::AgentIdentity::default(),
    ));

    let mut messages = vec![Message::new(Role::User, "choose")];
    let mut events = Vec::new();
    let outcome = agent
        .run_streaming_with_events(&mut messages, &CancellationToken::new(), |event| {
            if let AgentEvent::UserQuestionRequest(request) = &event {
                agent.reply_user_question(&request.id, vec![vec!["thiserror".to_string()]]);
            }
            events.push(event);
        })
        .await;

    assert_eq!(outcome.unwrap().message.content, "done");
    let lines = transcript(&events);
    assert!(lines.iter().any(|line| line.starts_with("user-question")));
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("tool-result ask_user") && line.contains("thiserror"))
    );
}

#[tokio::test]
async fn ask_user_tool_unblocks_with_a_cancelled_result() {
    let ask_args = serde_json::json!({
        "questions": [{
            "header": "style",
            "question": "Which error handling style?",
            "options": [
                { "label": "anyhow (Recommended)", "description": "Simple" },
                { "label": "thiserror", "description": "Structured" }
            ],
            "multi_select": false
        }]
    });
    let agent = Arc::new(Agent::new(
        Arc::new(ScriptedProvider::new(vec![
            turn(&[("c1", "ask_user", &ask_args.to_string())]),
            text_turn("acknowledged"),
        ])),
        vec![Arc::new(crate::tools::AskUserTool)],
        crate::AgentIdentity::default(),
    ));

    let mut messages = vec![Message::new(Role::User, "choose")];
    let mut events = Vec::new();
    let outcome = agent
        .run_streaming_with_events(&mut messages, &CancellationToken::new(), |event| {
            if let AgentEvent::UserQuestionRequest(request) = &event {
                agent.reply_user_question(&request.id, Vec::new());
            }
            events.push(event);
        })
        .await;

    assert_eq!(outcome.unwrap().message.content, "acknowledged");
    assert!(transcript(&events).iter().any(|line| {
        line.starts_with("tool-result ask_user") && line.contains("User cancelled the question")
    }));
}

#[tokio::test]
async fn unattended_reclaims_ask_user_and_short_circuits_stale_calls() {
    // The model still names ask_user (carried from an older tool list), but
    // in unattended mode the harness must not park on it. The call short-
    // circuits with a refusal, no user-question event fires, and the round
    // completes without a human.
    let ask_args = serde_json::json!({
        "questions": [{
            "header": "style",
            "question": "Which error handling style?",
            "options": [
                { "label": "anyhow (Recommended)", "description": "Simple" },
                { "label": "thiserror", "description": "Structured" }
            ],
            "multi_select": false
        }]
    });
    let agent = Arc::new(Agent::new(
        Arc::new(ScriptedProvider::new(vec![
            turn(&[("c1", "ask_user", &ask_args.to_string())]),
            text_turn("decided on my own"),
        ])),
        vec![Arc::new(crate::tools::AskUserTool)],
        crate::AgentIdentity::default(),
    ));
    agent.set_unattended(true);

    let mut messages = vec![Message::new(Role::User, "choose")];
    let mut events = Vec::new();
    let outcome = agent
        .run_streaming_with_events(&mut messages, &CancellationToken::new(), |event| {
            events.push(event);
        })
        .await;

    assert_eq!(outcome.unwrap().message.content, "decided on my own");
    let lines = transcript(&events);
    // No question was ever surfaced to a human.
    assert!(
        !lines.iter().any(|line| line.starts_with("user-question")),
        "ask_user should not surface a question in delegated mode"
    );
    // The stale call was answered with the delegated-mode refusal.
    assert!(
        lines.iter().any(|line| {
            line.starts_with("tool-result ask_user") && line.contains("unavailable")
        })
    );
}

#[tokio::test]
async fn execution_policy_blocks_ask_user_for_child_agents() {
    let ask_args = serde_json::json!({
        "questions": [{
            "header": "choice",
            "question": "Which option?",
            "options": [
                { "label": "A (Recommended)", "description": "Option A" },
                { "label": "B", "description": "Option B" }
            ],
            "multi_select": false
        }]
    });
    let agent = Arc::new(Agent::new(
        Arc::new(ScriptedProvider::new(vec![
            turn(&[("c1", "ask_user", &ask_args.to_string())]),
            text_turn("proceeded autonomously"),
        ])),
        vec![Arc::new(crate::tools::AskUserTool)],
        crate::AgentIdentity::default(),
    ));

    // Configure as a child agent with allow_human_interaction = false
    let child_policy = nuo_protocol::ExecutionPolicy {
        depth: 1,
        max_depth: 1,
        allow_human_interaction: false,
        lifecycle: nuo_protocol::ContextLifecycle::EphemeralScratchpad,
        tool_policy: None,
        max_children_budget: 0,
    };
    agent.set_execution_policy(child_policy);

    let mut messages = vec![Message::new(Role::User, "run task")];
    let mut events = Vec::new();
    let outcome = agent
        .run_streaming_with_events(&mut messages, &CancellationToken::new(), |event| {
            events.push(event);
        })
        .await;

    assert_eq!(outcome.unwrap().message.content, "proceeded autonomously");
    let lines = transcript(&events);
    assert!(
        !lines.iter().any(|line| line.starts_with("user-question")),
        "ask_user must never prompt the human when allow_human_interaction is false"
    );
    assert!(
        lines.iter().any(|line| {
            line.starts_with("tool-result ask_user")
                && line.contains("forbidden by ExecutionPolicy")
        }),
        "tool result should contain ExecutionPolicy rejection: {lines:?}"
    );
}

#[tokio::test]
async fn unattended_preserves_schema_and_intercepts_ask_user_at_runtime() {
    let agent = Agent::new(
        Arc::new(ScriptedProvider::new(vec![text_turn("ok")])),
        vec![Arc::new(crate::tools::AskUserTool)],
        crate::AgentIdentity::default(),
    );
    let visible_before = agent.visible_tools();
    let names_before: Vec<&str> = visible_before.iter().map(|t| t.name()).collect();
    assert!(names_before.contains(&"ask_user"));
    agent.set_unattended(true);
    let visible_after = agent.visible_tools();
    let names_after: Vec<&str> = visible_after.iter().map(|t| t.name()).collect();
    assert!(
        names_after.contains(&"ask_user"),
        "ask_user schema is preserved in unattended mode for KV-cache stability, got {names_after:?}"
    );
}

// Persistent permissions (cross-session)
//
// Verifies the per-project `Always` allowlist round-trips through disk:
// approving `Always` on one agent is visible to a fresh agent constructed
// against the same project root, and revoking is mirrored to disk too.
// Subagents (no project root) stay ephemeral and never touch the file.

#[tokio::test]
async fn always_permission_persists_across_agents_for_same_project() {
    let tmp = std::env::temp_dir().join(format!("nuo-perms-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp).expect("create temp data dir");
    let dirs = nuo_persistence::paths::Dirs {
        config_dir: tmp.join("config"),
        data_dir: tmp.join("data"),
        state_dir: tmp.join("state"),
        cache_dir: tmp.join("cache"),
        runtime_dir: None,
    };
    let project_root = std::path::PathBuf::from("/tmp/nuo-perms-fixture-project");
    let perms_path = dirs.project_permissions(&project_root);

    // First agent: prompt for a write_test permission and approve Always.
    let agent = Arc::new(Agent::new(
        Arc::new(TestProvider),
        vec![Arc::new(WriteTestTool)],
        crate::AgentIdentity::default(),
    ));
    agent.set_project_root_with_dirs(Some(project_root.clone()), &dirs);
    assert!(agent.allowed_tools().is_empty());

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
    assert!(agent.reply_permission(&request.id, PermissionDecision::Always));
    let _ = task.await;

    // The Always decision should have triggered an atomic write to disk.
    assert!(
        perms_path.exists(),
        "permissions file should exist at {}",
        perms_path.display()
    );
    let on_disk: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&perms_path).unwrap()).unwrap();
    // Version 2 added the `revoked` list (#3); the allowlist shape is otherwise
    // unchanged. A freshly-approved rule writes an empty revoked array.
    assert_eq!(on_disk["version"].as_u64(), Some(2));
    assert_eq!(on_disk["rules"].as_array().unwrap().len(), 1);
    assert_eq!(on_disk["rules"][0]["tool"], "write_test");
    assert_eq!(on_disk["rules"][0]["scope"], "/tmp/test");
    assert_eq!(
        on_disk["revoked"].as_array().unwrap().len(),
        0,
        "a fresh approval writes an empty revoked list"
    );

    // A brand-new agent in the same project should inherit the rule without
    // ever prompting — that is the whole point of cross-session persistence.
    let agent2 = Arc::new(Agent::new(
        Arc::new(TestProvider),
        vec![Arc::new(WriteTestTool)],
        crate::AgentIdentity::default(),
    ));
    agent2.set_project_root_with_dirs(Some(project_root.clone()), &dirs);
    assert_eq!(
        agent2.allowed_tools(),
        vec!["write_test /tmp/test".to_string()],
        "fresh agent in the same project should inherit persisted Always rule"
    );

    // Revoking on agent2 must remove the rule from disk as well, so the next
    // session doesn't silently resurrect it.
    assert!(agent2.revoke_allowed_tool("write_test", "/tmp/test"));
    let after_revoke: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&perms_path).unwrap()).unwrap();
    assert_eq!(after_revoke["rules"].as_array().unwrap().len(), 0);

    // A different project root must NOT see the first project's rules.
    let other_root = std::path::PathBuf::from("/tmp/nuo-perms-fixture-other-project");
    let agent3 = Agent::new(
        Arc::new(TestProvider),
        vec![Arc::new(WriteTestTool)],
        crate::AgentIdentity::default(),
    );
    agent3.set_project_root_with_dirs(Some(other_root), &dirs);
    assert!(
        agent3.allowed_tools().is_empty(),
        "unrelated project must not inherit another project's rules"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[tokio::test]
async fn agent_without_project_root_never_writes_permissions_file() {
    let tmp = std::env::temp_dir().join(format!("nuo-perms-noset-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp).expect("create temp data dir");
    let dirs = nuo_persistence::paths::Dirs {
        config_dir: tmp.join("config"),
        data_dir: tmp.join("data"),
        state_dir: tmp.join("state"),
        cache_dir: tmp.join("cache"),
        runtime_dir: None,
    };
    let project_root = std::path::PathBuf::from("/tmp/nuo-perms-noset-fixture");
    let perms_path = dirs.project_permissions(&project_root);

    // No set_project_root call: the agent stays ephemeral, so an Always
    // approval must not write any file (subagents behave the same way).
    let agent = Arc::new(Agent::new(
        Arc::new(TestProvider),
        vec![Arc::new(WriteTestTool)],
        crate::AgentIdentity::default(),
    ));
    // Mutations of the allowlist must be no-ops on disk when no project root
    // is set: no panic, no file created.
    agent.clear_allowed_tools();
    assert!(!agent.revoke_allowed_tool("anything", "*"));
    assert!(
        !perms_path.exists(),
        "ephemeral agent must not create a permissions file"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

// Uncapped ReAct turns
//
// The per-round turn cap was removed (along with the soft convergence
// nudge) to align with the codex / claude-code agentic-loop model: the
// round runs until the model stops calling tools, with context compaction
// as the backstop. This test pins the new behaviour — a long sequence of
// distinct tool calls runs well past the previous hard cap of 32 and only
// stops when the model finally emits a text answer.

#[tokio::test]
async fn round_runs_uncapped_until_model_emits_text() {
    // 64 distinct tool-carrying turns — well past any historical cap —
    // followed by a text answer. Each read turn uses a distinct argument so
    // the repeated-call guard never trips, and every fourth turn is a Write.
    // This mirrors the uncapped contract: the round is bounded by the model
    // choosing to stop, not by raw turn count (ADR-0009). Session review is
    // on-demand only (`/review`), so the ReAct loop never fires a diagnostic
    // to consume the shared scripted stream (ADR-0018).
    let write = RecordingTool::write("writer", "WROTE");
    let read = RecordingTool::read("alpha", "out");
    let mut turns: Vec<Vec<ProviderStreamEvent>> = Vec::new();
    for i in 0..64 {
        if i > 0 && i % 4 == 0 {
            turns.push(turn(&[("cw", "writer", &format!("{{\"i\":{i}}}"))]));
        } else {
            turns.push(turn(&[("c", "alpha", &format!("{{\"i\":{i}}}"))]));
        }
    }
    turns.push(text_turn("all done"));
    let agent = Arc::new(Agent::new(
        Arc::new(ScriptedProvider::new(turns)),
        vec![Arc::new(read), Arc::new(write)],
        crate::AgentIdentity::default(),
    ));

    let (_events, outcome) = run_golden_round(&agent, "go", PermissionDecision::Always).await;

    assert_eq!(outcome.unwrap().message.content, "all done");
}

// Session review (ADR-0018, superseding the periodic ADR-0016 design)
