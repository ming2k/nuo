use super::*;

#[test]
fn agent_installs_its_stateful_todo_tools() {
    let agent = agent();
    let names = agent
        .installed_tools()
        .into_iter()
        .map(|tool| tool.name().to_string())
        .collect::<std::collections::HashSet<_>>();
    assert!(names.contains("todo"));
}

#[test]
fn model_request_bands_historical_tool_outputs() {
    let agent = agent();
    agent.set_tool_output_projection(10, 200);
    let newest = Message::tool_result(&ToolCall::new("c1", "read", "{}"), "n".repeat(200));
    let middle = Message::tool_result(
        &ToolCall::new("c2", "read", "{}"),
        format!(
            "[read result]:\n{}\n- Spool File: \"/tmp/middle.log\"",
            (0..120)
                .map(|i| format!("middle line {i}"))
                .collect::<Vec<_>>()
                .join("\n")
        ),
    );
    let oldest = Message::tool_result(
        &ToolCall::new("c3", "execute_command", "{}"),
        format!(
            "[execute_command result]:\n{}\n- Spool File: \"/tmp/oldest.log\"",
            "old output\n".repeat(200)
        ),
    );
    let messages = vec![
        Message::new(Role::User, "start"),
        oldest,
        Message::new(Role::Assistant, "working"),
        middle,
        newest,
    ];

    let request = agent.model_request(&messages);
    let tools: Vec<&Message> = request
        .messages
        .iter()
        .filter(|message| message.role == Role::Tool)
        .collect();
    assert_eq!(tools.len(), 3);
    // Oldest band: cleared to a one-line placeholder, pointer preserved.
    assert!(tools[0].content.starts_with("[cleared tool result:"));
    assert!(
        tools[0]
            .content
            .contains("- Spool File: \"/tmp/oldest.log\"")
    );
    // Middle band: frozen head, pointer preserved.
    assert!(tools[1].content.contains("Previous turn output compacted"));
    assert!(
        tools[1]
            .content
            .contains("- Spool File: \"/tmp/middle.log\"")
    );
    // Newest band: verbatim.
    assert_eq!(tools[2].content, "n".repeat(200));
}

#[test]
fn remote_stored_continuations_are_exempt_from_tool_output_projection() {
    struct RemoteStoredProvider;
    #[async_trait]
    impl Provider for RemoteStoredProvider {
        async fn chat(
            &self,
            _request: &nuo_protocol::ModelRequest,
        ) -> Result<nuo_protocol::ProviderCompletion, nuo_protocol::ProviderError> {
            Ok(nuo_protocol::ProviderCompletion::message(Message::new(
                Role::Assistant,
                "ok",
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
        fn continuation_mode(&self) -> nuo_protocol::ContinuationMode {
            nuo_protocol::ContinuationMode::RemoteStored
        }
    }

    let agent = Arc::new(Agent::new(
        Arc::new(RemoteStoredProvider),
        Vec::new(),
        crate::AgentIdentity::default(),
    ));
    agent.set_tool_output_projection(10, 200);
    let oldest = Message::tool_result(
        &ToolCall::new("c1", "execute_command", "{}"),
        format!(
            "[execute_command result]:\n{}\n- Spool File: \"/tmp/old.log\"",
            "old output\n".repeat(200)
        ),
    );
    let messages = vec![
        Message::new(Role::User, "start"),
        oldest,
        Message::new(Role::Assistant, "done"),
    ];
    let request = agent.model_request(&messages);
    let tool = request
        .messages
        .iter()
        .find(|message| message.role == Role::Tool)
        .expect("tool result present");
    // Freezing an old message would change the semantic head and invalidate
    // `previous_response_id`; the remote-stored route must see it verbatim.
    assert!(tool.content.starts_with("[execute_command result]:"));
    assert!(!tool.content.contains("Previous turn output compacted"));
    assert!(!tool.content.starts_with("[cleared tool result:"));
}

#[test]
fn todo_state_is_scoped_to_one_agent() {
    let first = agent();
    let second = agent();
    let mut list = nuo_tool::TodoList::new();
    list.reconcile(
        &[("only first".to_string(), nuo_tool::TodoStatus::Pending)],
        1,
        1,
    );
    first.set_todos(list);

    assert_eq!(first.todos().len(), 1);
    assert!(second.todos().is_empty());
}

#[test]
fn agent_builder_accepts_additional_tools() {
    let agent = Agent::builder(
        Arc::new(TestProvider),
        Vec::new(),
        crate::AgentIdentity::default(),
    )
    .with_tool(Arc::new(WriteTestTool))
    .build();

    let names = agent
        .installed_tools()
        .into_iter()
        .map(|tool| tool.name().to_string())
        .collect::<std::collections::HashSet<_>>();
    assert!(names.contains("write_test"));
    assert!(names.contains("todo"));
}

#[test]
fn agent_owned_tool_identity_replaces_a_caller_shadow() {
    let agent = Agent::builder(
        Arc::new(TestProvider),
        vec![Arc::new(ShadowTodoTool)],
        crate::AgentIdentity::default(),
    )
    .build();

    let todo = agent
        .installed_tools()
        .into_iter()
        .find(|tool| tool.name() == "todo")
        .expect("todo should be installed");
    assert_ne!(todo.description(), "caller-owned shadow");
}

#[test]
fn dynamic_tool_sources_publish_toggle_and_remove_without_leaking_a_lock() {
    let agent = agent();
    agent.replace_dynamic_tools("plugin:test", vec![Arc::new(WriteTestTool)]);

    assert!(
        agent
            .installed_tools()
            .iter()
            .any(|tool| tool.name() == "write_test")
    );
    let snapshot = agent.snapshot_tools();
    assert!(
        snapshot.iter().any(|tool| {
            tool.name == "write_test" && tool.source == "plugin:test" && tool.enabled
        })
    );
    assert!(agent.set_tool_enabled("write_test", false));
    assert!(!agent.is_tool_enabled("write_test"));

    agent.remove_dynamic_tools("plugin:test");
    assert!(
        !agent
            .installed_tools()
            .iter()
            .any(|tool| tool.name() == "write_test")
    );
}

#[test]
fn static_tool_identity_shadows_a_dynamic_collision() {
    let agent = agent();
    agent.replace_dynamic_tools("plugin:shadow", vec![Arc::new(ShadowTodoTool)]);

    let todos: Vec<_> = agent
        .installed_tools()
        .into_iter()
        .filter(|tool| tool.name() == "todo")
        .collect();
    assert_eq!(todos.len(), 1);
    assert_ne!(todos[0].description(), "caller-owned shadow");
}

fn queued_user(id: &str, text: &str) -> nuo_protocol::QueuedMessage {
    nuo_protocol::QueuedMessage {
        id: id.to_string(),
        text: text.to_string(),
        display_text: Some(text.to_string()),
        images: Vec::new(),
        sent_at_ms: Some(123),
    }
}

#[test]
fn session_queues_are_session_and_generation_scoped() {
    let agent = agent();
    let (stale_s, stale_f) = agent.begin_session_queues("session-a", 1);
    assert!(stale_s.is_empty() && stale_f.is_empty());
    assert!(!agent.steer("session-b", queued_user("wrong", "no")));
    assert!(agent.steer("session-a", queued_user("old", "keep me")));

    let (stale_s2, stale_f2) = agent.begin_session_queues("session-a", 2);
    assert_eq!(stale_s2.len(), 1);
    assert_eq!(stale_s2[0].id, "old");
    assert!(stale_f2.is_empty());
    let (c1_s, c1_f) = agent.close_session_queues(1);
    assert!(c1_s.is_empty() && c1_f.is_empty());

    assert!(agent.steer("session-a", queued_user("new", "hello")));
    let (c2_s, c2_f) = agent.close_session_queues(2);
    assert_eq!(c2_s[0].id, "new");
    assert!(c2_f.is_empty());
    assert!(!agent.steer("session-a", queued_user("late", "no")));
}

#[tokio::test]
async fn queued_user_input_is_admitted_as_visible_user_steer() {
    let agent = agent();
    agent.begin_session_queues("session-a", 1);
    assert!(agent.steer("session-a", queued_user("insert-1", "more context")));
    let mut messages = vec![Message::new(Role::User, "start")];
    let mut events = Vec::new();

    agent
        .run_streaming_with_events(&mut messages, &CancellationToken::new(), |event| {
            events.push(event)
        })
        .await
        .expect("turn succeeds");

    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::SteerAdmitted { input, .. } if input.id == "insert-1"
    )));
    assert!(messages.iter().any(|message| {
        message.role == Role::User
            && message.content == "more context"
            && message
                .origin
                .as_ref()
                .is_some_and(|origin| origin.kind == InjectionKind::UserSteer)
    }));
}

#[test]
fn provider_prompt_hints_are_injected_into_system_prompt() {
    let agent = Agent::new(
        Arc::new(HintProvider),
        Vec::new(),
        crate::AgentIdentity::default(),
    );

    let mut messages: Vec<Message> = Vec::new();
    agent.prepare_request_messages_debug(&mut messages);

    assert!(messages[0].content.contains("Provider protocol hint."));
}

/// The shipped baseline agent has no default identity and no nanny sections;
/// baseline request preparation emits no system message clutter, reserving
/// the context window and attention entirely for the user's task.
#[test]
fn default_agent_has_clean_uncluttered_baseline_prompt() {
    let agent = agent();
    let mut messages: Vec<Message> = Vec::new();
    agent.prepare_request_messages_debug(&mut messages);
    assert!(
        messages.is_empty(),
        "baseline agent with no identity or project rules must emit no system prompt clutter"
    );
}

#[test]
fn project_instructions_render_matching_role_entries_in_scope_order() {
    let agent = agent();
    agent.set_active_role(Some("reviewer".to_string()));
    agent.set_instruction_provider_kind(Some("commandcode-goat".to_string()));
    agent.set_project_instructions(vec![
        nuo_protocol::ScopedInstruction {
            id: ".nuo/instructions/role.md".to_string(),
            source: nuo_protocol::InstructionSource::Project,
            ordinal: 0,
            selector: nuo_protocol::InstructionSelector {
                role: Some("reviewer".to_string()),
                ..Default::default()
            },
            text: "Reviewer rule.".to_string(),
        },
        nuo_protocol::ScopedInstruction {
            id: ".nuo/instructions/global.md".to_string(),
            source: nuo_protocol::InstructionSource::Project,
            ordinal: 1,
            selector: Default::default(),
            text: "Global rule.".to_string(),
        },
        nuo_protocol::ScopedInstruction {
            id: ".nuo/instructions/provider.md".to_string(),
            source: nuo_protocol::InstructionSource::Project,
            ordinal: 2,
            selector: nuo_protocol::InstructionSelector {
                provider: Some("commandcode-goat".to_string()),
                ..Default::default()
            },
            text: "Provider rule.".to_string(),
        },
        nuo_protocol::ScopedInstruction {
            id: ".nuo/instructions/other-provider.md".to_string(),
            source: nuo_protocol::InstructionSource::Project,
            ordinal: 3,
            selector: nuo_protocol::InstructionSelector {
                provider: Some("openai".to_string()),
                ..Default::default()
            },
            text: "Other provider rule.".to_string(),
        },
        nuo_protocol::ScopedInstruction {
            id: ".nuo/instructions/ops.md".to_string(),
            source: nuo_protocol::InstructionSource::Project,
            ordinal: 4,
            selector: nuo_protocol::InstructionSelector {
                role: Some("ops".to_string()),
                ..Default::default()
            },
            text: "Ops rule.".to_string(),
        },
    ]);

    let mut messages: Vec<Message> = Vec::new();
    agent.prepare_request_messages_debug(&mut messages);
    let prompt = &messages[0].content;
    assert!(
        prompt.contains(".nuo/instructions/global.md:\nGlobal rule."),
        "{prompt}"
    );
    assert!(
        prompt.contains(".nuo/instructions/role.md:\nReviewer rule."),
        "{prompt}"
    );
    assert!(
        prompt.contains(".nuo/instructions/provider.md:\nProvider rule."),
        "{prompt}"
    );
    assert!(
        !prompt.contains("Ops rule."),
        "a non-matching role must not be rendered: {prompt}"
    );
    assert!(
        !prompt.contains("Other provider rule."),
        "a non-matching provider must not be rendered: {prompt}"
    );
    let global = prompt.find("global.md").unwrap();
    let role = prompt.find("role.md").unwrap();
    let provider = prompt.find("provider.md").unwrap();
    assert!(
        global < role && role < provider,
        "scope order must be global -> role -> provider: {prompt}"
    );
}

#[test]
fn apply_preset_switches_identity_into_the_system_prompt() {
    // Switching the agent role live re-rolls the system-prompt identity line,
    // so the next request speaks with the new role directive.
    let agent = agent();
    let mut baseline: Vec<Message> = Vec::new();
    agent.prepare_request_messages_debug(&mut baseline);
    assert!(
        baseline.is_empty() || !baseline[0].content.starts_with("You are"),
        "the shipped baseline must carry no self-description",
    );

    let philosophist = nuo_protocol::AgentRoleProfile::from_role(
        nuo_protocol::MainAgentRole::Philosophist,
        &crate::AgentIdentity::default(),
    );
    agent.apply_preset(&philosophist);

    // The next assembled request must open with the philosophist role directive.
    let mut messages: Vec<Message> = Vec::new();
    agent.prepare_request_messages_debug(&mut messages);
    let prompt = &messages[0].content;
    assert!(
        prompt.starts_with("Role: philosophist."),
        "switched prompt should open with the philosophist role directive; got: {prompt}"
    );
    assert!(
        !prompt.starts_with("You are a coding assistant."),
        "the old identity preamble must be replaced, not appended; got: {prompt}"
    );

    let ops = nuo_protocol::AgentRoleProfile::from_role(
        nuo_protocol::MainAgentRole::Ops,
        &crate::AgentIdentity::default(),
    );
    agent.apply_preset(&ops);

    let mut ops_messages: Vec<Message> = Vec::new();
    agent.prepare_request_messages_debug(&mut ops_messages);
    let ops_prompt = &ops_messages[0].content;
    assert!(
        ops_prompt.starts_with("Role: ops."),
        "switched prompt should open with the ops role directive; got: {ops_prompt}"
    );
}

#[test]
fn retry_metadata_is_data_on_provider_error() {
    let err = nuo_protocol::ProviderError::new(
        "test",
        nuo_protocol::ProviderErrorKind::RateLimited,
        "rate limited",
    )
    .retryable(Some(500));
    assert_eq!(err.message(), "rate limited");
    assert_eq!(
        err.retry_disposition(),
        nuo_protocol::RetryDisposition::Retry {
            retry_after_ms: Some(500)
        }
    );
}
