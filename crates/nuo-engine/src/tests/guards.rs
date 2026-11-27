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
    agent.set_trajectory_guard_config(nuo_tool::TrajectoryGuardConfig::disabled());

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
    agent.set_trajectory_guard_config(nuo_tool::TrajectoryGuardConfig {
        enabled: true,
        threshold: 3,
        cognitive_review: false,
        ..nuo_tool::TrajectoryGuardConfig::default()
    });
    agent.seed_permissions_from_config(
        &[nuo_persistence::config::PermissionRuleConfig {
            tool: "execute_command".to_string(),
            scope: "make test".to_string(),
        }],
        &[],
    );

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
    agent.set_trajectory_guard_config(nuo_tool::TrajectoryGuardConfig {
        enabled: true,
        threshold: 3,
        cognitive_review: false,
        ..nuo_tool::TrajectoryGuardConfig::default()
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
    agent.set_trajectory_guard_config(nuo_tool::TrajectoryGuardConfig::disabled());
    agent.seed_permissions_from_config(
        &[nuo_persistence::config::PermissionRuleConfig {
            tool: "execute_command".to_string(),
            scope: "make test".to_string(),
        }],
        &[],
    );

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
    agent.set_trajectory_guard_config(nuo_tool::TrajectoryGuardConfig::cognitive());
    agent.seed_permissions_from_config(
        &[nuo_persistence::config::PermissionRuleConfig {
            tool: "execute_command".to_string(),
            scope: "cargo test".to_string(),
        }],
        &[],
    );

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
    agent.set_trajectory_guard_config(nuo_tool::TrajectoryGuardConfig::cognitive());
    agent.seed_permissions_from_config(
        &[nuo_persistence::config::PermissionRuleConfig {
            tool: "execute_command".to_string(),
            scope: "cargo test".to_string(),
        }],
        &[],
    );

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
    agent.set_trajectory_guard_config(nuo_tool::TrajectoryGuardConfig::cognitive());
    agent.seed_permissions_from_config(
        &[nuo_persistence::config::PermissionRuleConfig {
            tool: "execute_command".to_string(),
            scope: "cargo test".to_string(),
        }],
        &[],
    );

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
async fn command_policy_confirm_auto_approves_when_auto_approve_enabled() {
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

    assert_eq!(
        calls.lock().unwrap().len(),
        1,
        "auto-approve must run confirmable commands without prompting"
    );
    assert!(
        agent.allowed_tools().is_empty(),
        "auto-approve must not persist commands into the always allowlist"
    );
    let tool_message = messages
        .iter()
        .find(|m| m.role == Role::Tool)
        .expect("command tool result should be recorded");
    assert!(tool_message.content.contains("COMMAND-OUT"));
    assert!(outcome.unwrap().message.content.contains("done"));
}

#[tokio::test]
async fn auto_approve_executes_normal_commands_and_enforces_hard_deny() {
    let command = RecordingTool::read("execute_command", "ECHO-OUT");
    let calls = command.calls_handle();
    let agent = Arc::new(Agent::new(
        Arc::new(ScriptedProvider::new(vec![
            turn(&[
                ("c1", "execute_command", r#"{"command":"echo hello"}"#),
                ("c2", "execute_command", r#"{"command":"rm -rf /"}"#),
            ]),
            text_turn("done"),
        ])),
        vec![Arc::new(command)],
        crate::AgentIdentity::default(),
    ));
    agent.seed_permissions_from_config(
        &[],
        &[nuo_persistence::config::PermissionRuleConfig {
            tool: "execute_command".to_string(),
            scope: "rm -rf /".to_string(),
        }],
    );
    agent.set_unattended(true);

    let mut messages = vec![Message::new(Role::User, "run echo and denied command")];
    let outcome = agent
        .run_streaming_with_events(&mut messages, &CancellationToken::new(), |_| {})
        .await;

    assert_eq!(calls.lock().unwrap().len(), 1, "only echo hello should run");
    assert!(
        agent.allowed_tools().is_empty(),
        "auto-approved commands must not be persisted into always rules"
    );
    let tool_messages: Vec<&str> = messages
        .iter()
        .filter(|m| m.role == Role::Tool)
        .map(|m| m.content.as_str())
        .collect();
    assert_eq!(tool_messages.len(), 2);
    assert!(tool_messages[0].contains("ECHO-OUT"));
    assert!(
        tool_messages[1].contains("denied"),
        "hard deny must still block under auto-approve: {}",
        tool_messages[1]
    );
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
    agent.seed_permissions_from_config(
        &[nuo_persistence::config::PermissionRuleConfig {
            tool: "execute_command".to_string(),
            scope: "git reset --hard".to_string(),
        }],
        &[],
    );
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
async fn golden_rejected_write_tool_feeds_back_and_continues() {
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

    // The denial is an observation: the model gets the next turn and can choose
    // another approach instead of the round ending on the spot.
    assert_eq!(outcome.unwrap().message.content, "stopped");
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

// Persistent permissions (cross-session via SQLite `permission_rules`)
//
// Verifies the `(role_id, optional workspace)` allowlist round-trips through
// `nuo.db`: approving `Always` on one agent is visible to a fresh agent
// constructed with the same `(role_id, workspace)`, workspace-free roles like
// `ops` persist globally, and revoking survives reopen.

#[tokio::test]
async fn always_permission_persists_across_agents_in_sqlite_for_role_and_workspace() {
    let tmp = tempfile::tempdir().expect("create temp data dir");
    let db_path = tmp.path().join("nuo.db");
    let handle = nuo_persistence::db::PersistenceHandle::spawn(db_path.clone(), None);
    let project_root = std::path::PathBuf::from("/tmp/nuo-perms-fixture-project");

    // First agent (`developer` + workspace): approve WriteTestTool with Always.
    let agent = Arc::new(Agent::new(
        Arc::new(TestProvider),
        vec![Arc::new(WriteTestTool)],
        crate::AgentIdentity::default(),
    ));
    agent.bind_permission_persistence(handle.clone());
    agent.set_permission_scope("developer", Some(project_root.clone()));
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

    // A brand-new agent for the same `(developer, project_root)` inherits it.
    let agent2 = Arc::new(Agent::new(
        Arc::new(TestProvider),
        vec![Arc::new(WriteTestTool)],
        crate::AgentIdentity::default(),
    ));
    agent2.bind_permission_persistence(handle.clone());
    agent2.set_permission_scope("developer", Some(project_root.clone()));
    assert_eq!(
        agent2.allowed_tools(),
        vec!["write_test /tmp/test".to_string()],
        "fresh agent in the same (role, workspace) should inherit persisted Always rule"
    );

    // A different role (`ops` with no workspace) must NOT see `developer`'s grant,
    // and can persist its own global rules.
    let ops_agent = Arc::new(Agent::new(
        Arc::new(TestProvider),
        vec![Arc::new(WriteTestTool)],
        crate::AgentIdentity::default(),
    ));
    ops_agent.bind_permission_persistence(handle.clone());
    ops_agent.set_permission_scope("ops", None);
    assert!(
        ops_agent.allowed_tools().is_empty(),
        "ops role must be isolated from developer rules"
    );

    // Revoking on agent2 must persist to SQLite too.
    assert!(agent2.revoke_allowed_tool("write_test", "/tmp/test"));
    let agent2_reopened = Arc::new(Agent::new(
        Arc::new(TestProvider),
        vec![Arc::new(WriteTestTool)],
        crate::AgentIdentity::default(),
    ));
    agent2_reopened.bind_permission_persistence(handle.clone());
    agent2_reopened.set_permission_scope("developer", Some(project_root.clone()));
    assert!(agent2_reopened.allowed_tools().is_empty());

    // A different workspace for `developer` must NOT see the first project's rules.
    let other_root = std::path::PathBuf::from("/tmp/nuo-perms-fixture-other-project");
    let agent3 = Agent::new(
        Arc::new(TestProvider),
        vec![Arc::new(WriteTestTool)],
        crate::AgentIdentity::default(),
    );
    agent3.bind_permission_persistence(handle);
    agent3.set_permission_scope("developer", Some(other_root));
    assert!(
        agent3.allowed_tools().is_empty(),
        "unrelated workspace must not inherit another workspace's rules"
    );
}

#[tokio::test]
async fn execute_command_always_generalizes_to_safe_prefix_and_blocks_chaining() {
    let command = RecordingTool::read("execute_command", "OK");
    let calls = command.calls_handle();
    let agent = Arc::new(Agent::new(
        Arc::new(ScriptedProvider::new(vec![
            turn(&[("c1", "execute_command", r#"{"command":"cargo test one"}"#)]),
            turn(&[("c2", "execute_command", r#"{"command":"cargo test two"}"#)]),
            text_turn("done"),
        ])),
        vec![Arc::new(command)],
        crate::AgentIdentity::default(),
    ));

    let mut messages = vec![Message::new(Role::User, "run cargo test twice")];
    let agent_clone = agent.clone();
    let outcome = agent
        .run_streaming_with_events(&mut messages, &CancellationToken::new(), |event| {
            if let AgentEvent::PermissionRequest(req) = event {
                assert_eq!(req.suggested_scope.as_deref(), Some("cargo test *"));
                agent_clone.reply_permission(&req.id, PermissionDecision::Always);
            }
        })
        .await;

    assert_eq!(outcome.unwrap().message.content, "done");
    assert_eq!(
        calls.lock().unwrap().len(),
        2,
        "second distinct cargo test invocation should execute without re-prompting"
    );
    assert_eq!(
        agent.allowed_tools(),
        vec!["execute_command cargo test *".to_string()]
    );
}

#[tokio::test]
async fn execute_command_always_exact_persists_literal_command_only() {
    let command = RecordingTool::read("execute_command", "OK");
    let calls = command.calls_handle();
    let agent = Arc::new(Agent::new(
        Arc::new(ScriptedProvider::new(vec![
            turn(&[("c1", "execute_command", r#"{"command":"cargo test one"}"#)]),
            turn(&[("c2", "execute_command", r#"{"command":"cargo test one"}"#)]),
            turn(&[("c3", "execute_command", r#"{"command":"cargo test two"}"#)]),
            text_turn("done"),
        ])),
        vec![Arc::new(command)],
        crate::AgentIdentity::default(),
    ));

    let prompted = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let prompted_clone = prompted.clone();
    let mut messages = vec![Message::new(Role::User, "test AlwaysExact")];
    let agent_clone = agent.clone();
    let outcome = agent
        .run_streaming_with_events(&mut messages, &CancellationToken::new(), |event| {
            if let AgentEvent::PermissionRequest(req) = event {
                prompted_clone.lock().unwrap().push(req.scope.clone());
                assert_eq!(req.suggested_scope.as_deref(), Some("cargo test *"));
                agent_clone.reply_permission(&req.id, PermissionDecision::AlwaysExact);
            }
        })
        .await;

    assert_eq!(outcome.unwrap().message.content, "done");
    assert_eq!(calls.lock().unwrap().len(), 3);
    assert_eq!(
        prompted.lock().unwrap().as_slice(),
        ["cargo test one", "cargo test two"]
    );
    assert_eq!(
        agent.allowed_tools(),
        vec![
            "execute_command cargo test one".to_string(),
            "execute_command cargo test two".to_string(),
        ]
    );
}

#[tokio::test]
async fn execute_command_safe_readonly_runs_without_prompt_while_escape_prompts() {
    let command = RecordingTool::read("execute_command", "OK");
    let calls = command.calls_handle();
    let agent = Arc::new(Agent::new(
        Arc::new(ScriptedProvider::new(vec![
            turn(&[(
                "c1",
                "execute_command",
                r#"{"command":"git status -s && ls -la crates"}"#,
            )]),
            turn(&[("c2", "execute_command", r#"{"command":"cat /etc/shadow"}"#)]),
            text_turn("done"),
        ])),
        vec![Arc::new(command)],
        crate::AgentIdentity::default(),
    ));

    let prompted = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let prompted_clone = prompted.clone();
    let mut messages = vec![Message::new(Role::User, "run readonly and escape")];
    let agent_clone = agent.clone();
    let outcome = agent
        .run_streaming_with_events(&mut messages, &CancellationToken::new(), |event| {
            if let AgentEvent::PermissionRequest(req) = event {
                prompted_clone.lock().unwrap().push(req.scope.clone());
                agent_clone.reply_permission(&req.id, PermissionDecision::Reject);
            }
        })
        .await;

    assert_eq!(outcome.unwrap().message.content, "done");
    assert_eq!(
        prompted.lock().unwrap().as_slice(),
        ["cat /etc/shadow"],
        "only the workspace-escaping command should prompt"
    );
    assert_eq!(calls.lock().unwrap().len(), 1);
    assert!(agent.allowed_tools().is_empty());
}

#[tokio::test]
async fn agent_without_backend_stays_ephemeral() {
    let agent = Arc::new(Agent::new(
        Arc::new(TestProvider),
        vec![Arc::new(WriteTestTool)],
        crate::AgentIdentity::default(),
    ));
    agent.clear_allowed_tools();
    assert!(!agent.revoke_allowed_tool("anything", "*"));
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
