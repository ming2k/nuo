use super::*;
use futures::stream::{self, BoxStream};
use std::sync::atomic::{AtomicUsize, Ordering};

struct TestProvider;
struct HintProvider;
struct PermissionTestProvider(AtomicUsize);
struct StreamingToolProvider(AtomicUsize);
struct WriteTestTool;
struct ShadowTodoTool;
struct StreamingReadTool(Arc<AtomicUsize>);

#[async_trait]
impl Provider for TestProvider {
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
        BoxStream<'static, Result<String, nuo_wire::ProviderError>>,
        nuo_wire::ProviderError,
    > {
        // The default `stream_chat_events` wraps this into a single
        // `TextDelta("done")`, so the streaming ReAct loop sees the same
        // terminal answer as `chat()`.
        Ok(Box::pin(stream::once(async { Ok("done".to_string()) })))
    }
}

#[async_trait]
impl Provider for HintProvider {
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
        BoxStream<'static, Result<String, nuo_wire::ProviderError>>,
        nuo_wire::ProviderError,
    > {
        Ok(Box::pin(stream::empty()))
    }

    fn prompt_hints(&self) -> nuo_wire::ProviderPromptHints {
        nuo_wire::ProviderPromptHints {
            system_guidance: "Provider protocol hint.",
        }
    }
}

#[async_trait]
impl Provider for PermissionTestProvider {
    async fn chat(
        &self,
        _request: nuo_wire::ModelRequest,
    ) -> Result<nuo_wire::ProviderCompletion, nuo_wire::ProviderError> {
        unreachable!("streaming path should be used")
    }

    async fn stream_chat(
        &self,
        _request: nuo_wire::ModelRequest,
    ) -> Result<
        BoxStream<'static, Result<String, nuo_wire::ProviderError>>,
        nuo_wire::ProviderError,
    > {
        unreachable!("stream_chat_events should be called directly")
    }

    async fn stream_chat_events(
        &self,
        _request: nuo_wire::ModelRequest,
    ) -> Result<
        BoxStream<'static, Result<ProviderStreamEvent, nuo_wire::ProviderError>>,
        nuo_wire::ProviderError,
    > {
        let events = if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
            vec![
                Ok(ProviderStreamEvent::ToolCallDelta {
                    index: 0,
                    id: Some("call".to_string()),
                    name: Some("write_test".to_string()),
                    arguments: "{}".to_string(),
                }),
                Ok(ProviderStreamEvent::Completed(
                    nuo_wire::ProviderCompletionMeta::default(),
                )),
            ]
        } else {
            vec![
                Ok(ProviderStreamEvent::TextDelta("done".to_string())),
                Ok(ProviderStreamEvent::Completed(
                    nuo_wire::ProviderCompletionMeta::default(),
                )),
            ]
        };
        Ok(Box::pin(stream::iter(events)))
    }
}

#[async_trait]
impl Provider for StreamingToolProvider {
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
        BoxStream<'static, Result<String, nuo_wire::ProviderError>>,
        nuo_wire::ProviderError,
    > {
        Ok(Box::pin(stream::empty()))
    }

    async fn stream_chat_events(
        &self,
        _request: nuo_wire::ModelRequest,
    ) -> Result<
        BoxStream<'static, Result<ProviderStreamEvent, nuo_wire::ProviderError>>,
        nuo_wire::ProviderError,
    > {
        let events = if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
            vec![
                Ok(ProviderStreamEvent::ToolCallDelta {
                    index: 0,
                    id: Some("call_1".to_string()),
                    name: Some("stream_".to_string()),
                    arguments: "{\"value\":".to_string(),
                }),
                Ok(ProviderStreamEvent::ToolCallDelta {
                    index: 0,
                    id: None,
                    name: Some("read".to_string()),
                    arguments: "1}".to_string(),
                }),
                Ok(ProviderStreamEvent::Completed(
                    nuo_wire::ProviderCompletionMeta::default(),
                )),
            ]
        } else {
            vec![
                Ok(ProviderStreamEvent::TextDelta("do".to_string())),
                Ok(ProviderStreamEvent::TextDelta("ne".to_string())),
                Ok(ProviderStreamEvent::Completed(
                    nuo_wire::ProviderCompletionMeta::default(),
                )),
            ]
        };
        Ok(Box::pin(stream::iter(events)))
    }
}

#[async_trait]
impl Tool for WriteTestTool {
    fn name(&self) -> &str {
        "write_test"
    }

    fn description(&self) -> &str {
        "test write tool"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }

    fn scope_target(&self, _arguments: &str) -> nuo_wire::ScopeTarget {
        nuo_wire::ScopeTarget::Path(std::path::PathBuf::from("/tmp/test"))
    }

    async fn call(&self, _arguments: &str) -> Result<String, String> {
        Ok("should not run".to_string())
    }
}

#[async_trait]
impl Tool for StreamingReadTool {
    fn name(&self) -> &str {
        "stream_read"
    }

    fn description(&self) -> &str {
        "streaming test tool"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }

    async fn call(&self, arguments: &str) -> Result<String, String> {
        assert_eq!(arguments, "{\"value\":1}");
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok("read".to_string())
    }
}

#[async_trait]
impl Tool for ShadowTodoTool {
    fn name(&self) -> &str {
        "todo"
    }

    fn description(&self) -> &str {
        "caller-owned shadow"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }

    async fn call(&self, _arguments: &str) -> Result<String, String> {
        Ok("wrong state".to_string())
    }
}

fn agent() -> Arc<Agent> {
    Arc::new(Agent::new(
        Arc::new(TestProvider),
        Vec::new(),
        crate::AgentIdentity::default(),
    ))
}

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
fn todo_state_is_scoped_to_one_agent() {
    let first = agent();
    let second = agent();
    let mut list = nuo_wire::TodoList::new();
    list.reconcile(
        &[(
            "only first".to_string(),
            nuo_wire::TodoStatus::Pending,
        )],
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

fn queued_user(id: &str, text: &str) -> nuo_wire::QueuedMessage {
    nuo_wire::QueuedMessage {
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
        AgentEvent::SteerAdmitted(input) if input.id == "insert-1"
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

    let philosophist = nuo_wire::AgentRoleProfile::from_role(
        nuo_wire::MainAgentRole::Philosophist,
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

    let ops = nuo_wire::AgentRoleProfile::from_role(
        nuo_wire::MainAgentRole::Ops,
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
    let err = nuo_wire::ProviderError::new(
        "test",
        nuo_wire::ProviderErrorKind::RateLimited,
        "rate limited",
    )
    .retryable(Some(500));
    assert_eq!(err.message(), "rate limited");
    assert_eq!(
        err.retry_disposition(),
        nuo_wire::RetryDisposition::Retry {
            retry_after_ms: Some(500)
        }
    );
}

#[tokio::test]
async fn provider_turn_context_survives_tools_and_retries_but_not_new_rounds() {
    use std::sync::Mutex;
    struct RecordingProvider {
        inner: StreamingToolProvider,
        contexts: Mutex<Vec<Arc<nuo_wire::ProviderTurnContext>>>,
    }
    #[async_trait]
    impl Provider for RecordingProvider {
        async fn chat(
            &self,
            request: nuo_wire::ModelRequest,
        ) -> Result<nuo_wire::ProviderCompletion, nuo_wire::ProviderError> {
            self.inner.chat(request).await
        }
        async fn stream_chat(
            &self,
            request: nuo_wire::ModelRequest,
        ) -> Result<
            BoxStream<'static, Result<String, nuo_wire::ProviderError>>,
            nuo_wire::ProviderError,
        > {
            self.inner.stream_chat(request).await
        }
        async fn stream_chat_events(
            &self,
            request: nuo_wire::ModelRequest,
        ) -> Result<nuo_wire::ProviderEventStream, nuo_wire::ProviderError> {
            let first = {
                let mut contexts = self.contexts.lock().unwrap();
                contexts.push(Arc::clone(&request.turn_context));
                contexts.len() == 1
            };
            if first {
                return Err(nuo_wire::ProviderError::new(
                    "mock",
                    nuo_wire::ProviderErrorKind::Unavailable,
                    "retry",
                )
                .retryable(None));
            }
            self.inner.stream_chat_events(request).await
        }
    }
    let provider = Arc::new(RecordingProvider {
        inner: StreamingToolProvider(AtomicUsize::new(0)),
        contexts: Mutex::default(),
    });
    let agent = Arc::new(Agent::new(
        provider.clone(),
        vec![Arc::new(StreamingReadTool(Arc::new(AtomicUsize::new(0))))],
        crate::AgentIdentity::default(),
    ));
    let mut messages = vec![Message::new(Role::User, "run")];
    let cancel = CancellationToken::new();
    let mut round = agent.begin_streaming_round();
    assert!(
        agent
            .resume_streaming_with_events(&mut messages, &cancel, &mut round, |_| {})
            .await
            .is_err()
    );
    agent
        .resume_streaming_with_events(&mut messages, &cancel, &mut round, |_| {})
        .await
        .unwrap();
    messages.push(Message::new(Role::User, "again"));
    agent
        .run_streaming_with_events(&mut messages, &cancel, |_| {})
        .await
        .unwrap();
    let contexts = provider.contexts.lock().unwrap();
    assert_eq!(contexts.len(), 4);
    assert!(
        Arc::ptr_eq(&contexts[0], &contexts[1]),
        "retry preserves routing"
    );
    assert!(
        Arc::ptr_eq(&contexts[1], &contexts[2]),
        "tool continuation preserves routing"
    );
    assert!(
        !Arc::ptr_eq(&contexts[2], &contexts[3]),
        "new user round resets routing"
    );
}

#[tokio::test]
async fn streaming_tool_deltas_are_reassembled_and_executed() {
    let calls = Arc::new(AtomicUsize::new(0));
    let agent = Arc::new(Agent::new(
        Arc::new(StreamingToolProvider(AtomicUsize::new(0))),
        vec![Arc::new(StreamingReadTool(calls.clone()))],
        crate::AgentIdentity::default(),
    ));
    let mut messages = vec![Message::new(Role::User, "run")];
    let mut events = Vec::new();

    let response = agent
        .run_streaming_with_events(&mut messages, &CancellationToken::new(), |event| {
            events.push(event)
        })
        .await
        .unwrap();

    assert_eq!(response.message.content, "done");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let model_turns = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ModelRequestStarted { turn, .. } => Some(*turn),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(model_turns, vec![0, 1]);
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::ToolCall { name, arguments, .. }
            if name == "stream_read" && arguments == "{\"value\":1}"
    )));
    // The turn-performance telemetry now trails the terminal AssistantEnd
    // (emitted at the ReAct-turn boundary, after the response is sealed), so
    // assert on the last *content* event rather than the literal last event.
    assert!(events.iter().rev().any(|event| matches!(
        event,
        AgentEvent::AssistantEnd(content) if content == "done"
    )));
}

#[tokio::test]
async fn turn_persist_fires_at_each_react_turn_boundary() {
    // ADR-0048: the mid-round save point must fire once per completed
    // tool-carrying turn, carrying the full history including that turn's
    // tool results. `StreamingToolProvider` produces two turns (turn 0 = tool
    // call, turn 1 = terminal text), so exactly one continuing-turn boundary is crossed and the
    // callback should see three messages: user prompt + assistant + tool
    // result. The final turn (plain text, no tools) does not cross a
    // boundary and must not fire the callback.
    let calls = Arc::new(AtomicUsize::new(0));
    let seen_lengths: Arc<std::sync::Mutex<Vec<usize>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let agent = Arc::new(Agent::new(
        Arc::new(StreamingToolProvider(AtomicUsize::new(0))),
        vec![Arc::new(StreamingReadTool(calls.clone()))],
        crate::AgentIdentity::default(),
    ));
    let seen_for_cb = Arc::clone(&seen_lengths);
    agent.set_turn_persist(Arc::new(move |messages: &[Message]| {
        let len = messages.len();
        seen_for_cb.lock().unwrap().push(len);
        // Snapshot the slice for the 'static future (the closure itself does
        // not borrow; the persistence target is external in production).
        let _ = messages.to_vec();
        Box::pin(async { Ok(()) })
    }));

    let mut messages = vec![Message::new(Role::User, "run")];
    let outcome = agent
        .run_streaming_with_events(&mut messages, &CancellationToken::new(), |_| {})
        .await
        .unwrap();
    assert_eq!(outcome.message.content, "done");

    // Exactly one boundary crossing (after turn 0's tool result). The
    // callback receives the full live history: [user, assistant, tool_result]
    // = 3. Request-scoped system policy is deliberately absent. The final
    // turn (plain text, no tools) does not cross a boundary and must not fire.
    let recorded = seen_lengths.lock().unwrap().clone();
    assert_eq!(
        recorded,
        vec![3],
        "turn persist fires once with the full history"
    );
}

/// A provider whose SSE stream never yields and never ends simulates a stalled
/// connection (server stops sending but keeps the socket open). Without an idle
/// timeout the turn loop blocks on `stream.next()` forever — the UI spins/// "running · responding" and only a user interrupt can break it. The
/// `STREAM_IDLE_TIMEOUT` guard surfaces this as a retryable error instead.
/// `start_paused` makes tokio auto-advance the clock past the 120 s bound so
/// the test is instantaneous.
#[tokio::test(start_paused = true)]
async fn stalled_provider_stream_times_out_as_retryable() {
    struct StalledStreamProvider;
    #[async_trait]
    impl Provider for StalledStreamProvider {
        async fn chat(
            &self,
            _request: nuo_wire::ModelRequest,
        ) -> Result<nuo_wire::ProviderCompletion, nuo_wire::ProviderError> {
            unreachable!("streaming path should be used")
        }
        async fn stream_chat(
            &self,
            _request: nuo_wire::ModelRequest,
        ) -> Result<
            BoxStream<'static, Result<String, nuo_wire::ProviderError>>,
            nuo_wire::ProviderError,
        > {
            Ok(Box::pin(stream::empty()))
        }
        async fn stream_chat_events(
            &self,
            _request: nuo_wire::ModelRequest,
        ) -> Result<
            BoxStream<'static, Result<ProviderStreamEvent, nuo_wire::ProviderError>>,
            nuo_wire::ProviderError,
        > {
            Ok(Box::pin(stream::pending()))
        }
    }

    let agent = Arc::new(Agent::new(
        Arc::new(StalledStreamProvider),
        Vec::new(),
        crate::AgentIdentity::default(),
    ));
    let mut messages = vec![Message::new(Role::User, "hello")];

    let result = agent
        .run_streaming_with_events(&mut messages, &CancellationToken::new(), |_| {})
        .await;

    assert!(
        matches!(result, Err(nuo_wire::HarnessError::Provider(ref err)) if matches!(err.retry_disposition(), nuo_wire::RetryDisposition::Retry { .. })),
        "a stalled stream should surface as a retryable error, not hang forever; got: {result:?}"
    );
}

/// A stream that ends after delivering only part of a tool call (id/argument
/// bytes arrived but the name never did) leaves residue in the call slots.
/// Dropping it silently would mistake a truncated connection for the model's
/// intent, so stream finalization must surface it as a retryable error and
/// refuse to commit the partial response.
#[tokio::test]
async fn stream_ending_mid_tool_call_is_retryable() {
    struct TruncatedToolCallProvider;
    #[async_trait]
    impl Provider for TruncatedToolCallProvider {
        async fn chat(
            &self,
            _request: nuo_wire::ModelRequest,
        ) -> Result<nuo_wire::ProviderCompletion, nuo_wire::ProviderError> {
            unreachable!("streaming path should be used")
        }
        async fn stream_chat(
            &self,
            _request: nuo_wire::ModelRequest,
        ) -> Result<
            BoxStream<'static, Result<String, nuo_wire::ProviderError>>,
            nuo_wire::ProviderError,
        > {
            Ok(Box::pin(stream::empty()))
        }
        async fn stream_chat_events(
            &self,
            _request: nuo_wire::ModelRequest,
        ) -> Result<
            BoxStream<'static, Result<ProviderStreamEvent, nuo_wire::ProviderError>>,
            nuo_wire::ProviderError,
        > {
            Ok(Box::pin(stream::iter(vec![Ok(
                ProviderStreamEvent::ToolCallDelta {
                    index: 0,
                    id: Some("call_1".to_string()),
                    name: None,
                    arguments: "{\"value\":".to_string(),
                },
            )])))
        }
    }

    let agent = Arc::new(Agent::new(
        Arc::new(TruncatedToolCallProvider),
        Vec::new(),
        crate::AgentIdentity::default(),
    ));
    let mut messages = vec![Message::new(Role::User, "hello")];

    let result = agent
        .run_streaming_with_events(&mut messages, &CancellationToken::new(), |_| {})
        .await;

    match result {
        Err(nuo_wire::HarnessError::Provider(err))
            if matches!(
                err.retry_disposition(),
                nuo_wire::RetryDisposition::Retry { .. }
            ) =>
        {
            let message = err.message();
            assert!(
                message.contains("mid-tool-call"),
                "message should name the mid-tool-call truncation: {message}"
            );
        }
        other => panic!("mid-call truncation must be retryable, got: {other:?}"),
    }
    assert_eq!(
        messages.len(),
        1,
        "the truncated response must not be committed to history"
    );
}

/// An entirely empty assistant frame (no content, no reasoning, no tool call
/// — a real production incident on `gogo`/omen-alpha, 2026-09-07) is a
/// transient upstream glitch, not a terminal round failure. Before the fix it
/// surfaced as `HarnessError::Other` and killed the round, leaving an
/// unattended session idle until a human retried. It must instead surface as
/// a *retryable* provider error so the orchestration loop resends the same
/// (checkpoint-armed) request automatically.
#[tokio::test]
async fn empty_assistant_response_is_retryable_not_terminal() {
    struct EmptyFrameProvider;
    #[async_trait]
    impl Provider for EmptyFrameProvider {
        async fn chat(
            &self,
            _request: nuo_wire::ModelRequest,
        ) -> Result<nuo_wire::ProviderCompletion, nuo_wire::ProviderError> {
            unreachable!("streaming path should be used")
        }
        async fn stream_chat(
            &self,
            _request: nuo_wire::ModelRequest,
        ) -> Result<
            BoxStream<'static, Result<String, nuo_wire::ProviderError>>,
            nuo_wire::ProviderError,
        > {
            Ok(Box::pin(stream::empty()))
        }
        async fn stream_chat_events(
            &self,
            _request: nuo_wire::ModelRequest,
        ) -> Result<
            BoxStream<'static, Result<ProviderStreamEvent, nuo_wire::ProviderError>>,
            nuo_wire::ProviderError,
        > {
            Ok(Box::pin(stream::iter(vec![Ok(
                ProviderStreamEvent::Completed(nuo_wire::ProviderCompletionMeta::default()),
            )])))
        }
    }

    let agent = Arc::new(Agent::new(
        Arc::new(EmptyFrameProvider),
        Vec::new(),
        crate::AgentIdentity::default(),
    ));
    let mut messages = vec![Message::new(Role::User, "hello")];

    let result = agent
        .run_streaming_with_events(&mut messages, &CancellationToken::new(), |_| {})
        .await;

    match result {
        Err(nuo_wire::HarnessError::Provider(err))
            if matches!(
                err.retry_disposition(),
                nuo_wire::RetryDisposition::Retry { .. }
            ) =>
        {
            let message = err.message();
            assert!(
                message.contains("empty assistant response"),
                "message should name the empty-response fault: {message}"
            );
            assert_eq!(
                err.kind(),
                nuo_wire::ProviderErrorKind::Upstream,
                "an empty frame is an upstream fault"
            );
        }
        other => panic!("empty assistant response must be retryable, got: {other:?}"),
    }
    assert_eq!(
        messages.len(),
        1,
        "the empty response must not be committed to history"
    );
}

/// The same truncation one delta later: the name arrived but the argument
/// JSON was cut off mid-payload. The half-written call must not reach
/// execution — where its parse error would be indistinguishable from the
/// model emitting bad JSON — so finalization fails retryable instead.
#[tokio::test]
async fn stream_with_truncated_tool_arguments_is_retryable() {
    struct TruncatedArgumentsProvider;
    #[async_trait]
    impl Provider for TruncatedArgumentsProvider {
        async fn chat(
            &self,
            _request: nuo_wire::ModelRequest,
        ) -> Result<nuo_wire::ProviderCompletion, nuo_wire::ProviderError> {
            unreachable!("streaming path should be used")
        }
        async fn stream_chat(
            &self,
            _request: nuo_wire::ModelRequest,
        ) -> Result<
            BoxStream<'static, Result<String, nuo_wire::ProviderError>>,
            nuo_wire::ProviderError,
        > {
            Ok(Box::pin(stream::empty()))
        }
        async fn stream_chat_events(
            &self,
            _request: nuo_wire::ModelRequest,
        ) -> Result<
            BoxStream<'static, Result<ProviderStreamEvent, nuo_wire::ProviderError>>,
            nuo_wire::ProviderError,
        > {
            Ok(Box::pin(stream::iter(vec![Ok(
                ProviderStreamEvent::ToolCallDelta {
                    index: 0,
                    id: Some("call_1".to_string()),
                    name: Some("read".to_string()),
                    arguments: "{\"value\":".to_string(),
                },
            )])))
        }
    }

    let agent = Arc::new(Agent::new(
        Arc::new(TruncatedArgumentsProvider),
        Vec::new(),
        crate::AgentIdentity::default(),
    ));
    let mut messages = vec![Message::new(Role::User, "hello")];

    let result = agent
        .run_streaming_with_events(&mut messages, &CancellationToken::new(), |_| {})
        .await;

    match result {
        Err(nuo_wire::HarnessError::Provider(err))
            if matches!(
                err.retry_disposition(),
                nuo_wire::RetryDisposition::Retry { .. }
            ) =>
        {
            let message = err.message();
            assert!(
                message.contains("truncated arguments") && message.contains("read"),
                "message should name the call whose arguments were cut off: {message}"
            );
        }
        other => panic!("truncated tool arguments must be retryable, got: {other:?}"),
    }
    assert_eq!(
        messages.len(),
        1,
        "the truncated response must not be committed to history"
    );
}

/// `arguments == ""` is the legitimate shape of a zero-argument tool call:
/// the truncation guard must not reject it, and the call must dispatch
/// normally. (Complete streams with valid JSON arguments are covered by the
/// golden-transcript tests below.)
#[tokio::test]
async fn zero_argument_tool_call_survives_stream_finalization() {
    let tool = RecordingTool::read("alpha", "A-out");
    let calls = tool.calls_handle();
    let agent = Arc::new(Agent::new(
        Arc::new(ScriptedProvider::new(vec![
            turn(&[("c1", "alpha", "")]),
            text_turn("done"),
        ])),
        vec![Arc::new(tool)],
        crate::AgentIdentity::default(),
    ));
    let mut messages = vec![Message::new(Role::User, "go")];

    let outcome = agent
        .run_streaming_with_events(&mut messages, &CancellationToken::new(), |_| {})
        .await;

    assert_eq!(outcome.unwrap().message.content, "done");
    assert_eq!(
        calls.lock().unwrap_or_else(|e| e.into_inner()).as_slice(),
        &[String::new()],
        "the zero-argument call must reach the tool with empty arguments"
    );
}

#[tokio::test]
async fn interrupt_settles_in_flight_request_with_estimated_prompt() {
    struct PendingProvider;
    #[async_trait]
    impl Provider for PendingProvider {
        async fn chat(
            &self,
            _: nuo_wire::ModelRequest,
        ) -> Result<nuo_wire::ProviderCompletion, nuo_wire::ProviderError> {
            unreachable!("streaming path should be used")
        }
        async fn stream_chat(
            &self,
            _: nuo_wire::ModelRequest,
        ) -> Result<
            BoxStream<'static, Result<String, nuo_wire::ProviderError>>,
            nuo_wire::ProviderError,
        > {
            Ok(Box::pin(stream::pending()))
        }
        async fn stream_chat_events(
            &self,
            _: nuo_wire::ModelRequest,
        ) -> Result<
            BoxStream<'static, Result<ProviderStreamEvent, nuo_wire::ProviderError>>,
            nuo_wire::ProviderError,
        > {
            Ok(Box::pin(stream::pending()))
        }
    }

    let agent = Arc::new(Agent::new(
        Arc::new(PendingProvider),
        Vec::new(),
        crate::AgentIdentity::default(),
    ));
    agent.set_thread_id("interrupt-session");
    agent.bump_round();
    let ledger = nuo_wire::TokenSourceLedger::shared();
    agent.install_token_ledger(ledger.clone());
    let token = CancellationToken::new();
    let cancel_on_start = token.clone();
    let mut messages = vec![Message::new(Role::User, "hello")];

    let result = agent
        .run_streaming_with_events(&mut messages, &token, |event| {
            if matches!(event, AgentEvent::ModelRequestStarted { .. }) {
                cancel_on_start.cancel();
            }
        })
        .await;

    assert!(matches!(result, Err(HarnessError::Interrupted)));
    let records = ledger.records_for_session("interrupt-session");
    assert_eq!(records.len(), 1);
    assert_eq!(
        records[0].status,
        nuo_wire::RequestUsageStatus::Interrupted
    );
    assert_eq!(
        records[0].source,
        nuo_wire::RequestUsageSource::Estimated
    );
    assert!(records[0].prompt_tokens > 0);
    assert_eq!(records[0].completion_tokens, 0);
}

/// Regression for the quadratic streamed-output bug: a stream of many text
/// deltas that is interrupted mid-flight must book an *exact* completion
/// count (one whole-text tokenization of what streamed), not the running
/// total of `StreamingCounter::push` summed once per delta. The bug booked
/// 14.7M "completion tokens" (a 130 050 tok/s rate) for a real interrupted
/// turn whose actual output was a few thousand tokens.
///
/// The corpus and pacing are tuned to stay under the ledger's physical-rate
/// ceiling (10 000 tok/s): ~9 600 tokens over a ≥1 s wall-clock span. The
/// bug, by contrast, blows past the ceiling almost immediately (its sum
/// grows quadratically), so this test catches both the arithmetic error and
/// the clamp with the same stream.
#[tokio::test]
async fn interrupted_stream_books_exact_not_cumulative_completion_tokens() {
    // The provider streams N deltas, then parks. Cancellation is armed only
    // after the deltas have all been *consumed* (the event closure counts
    // AssistantDeltas), so the accounting guard observes the full N-delta
    // stream before it settles as interrupted — the exact shape of the real
    // bug, where a long stream was interrupted mid-generation.
    struct SlowStreamProvider {
        deltas: usize,
    }
    #[async_trait]
    impl Provider for SlowStreamProvider {
        async fn chat(
            &self,
            _: nuo_wire::ModelRequest,
        ) -> Result<nuo_wire::ProviderCompletion, nuo_wire::ProviderError> {
            unreachable!("streaming path should be used")
        }
        async fn stream_chat(
            &self,
            _: nuo_wire::ModelRequest,
        ) -> Result<
            BoxStream<'static, Result<String, nuo_wire::ProviderError>>,
            nuo_wire::ProviderError,
        > {
            Ok(Box::pin(stream::pending()))
        }
        async fn stream_chat_events(
            &self,
            _: nuo_wire::ModelRequest,
        ) -> Result<
            BoxStream<'static, Result<ProviderStreamEvent, nuo_wire::ProviderError>>,
            nuo_wire::ProviderError,
        > {
            let n = self.deltas;
            // Pace the stream like a real model (~9 600 tokens over ≈1.2 s of
            // wall clock ⇒ ≈8 000 tok/s, under the ledger's 10 000 tok/s
            // physical ceiling) so the *fixed* code settles the exact count,
            // while the bug's quadratic sum still rockets past the ceiling
            // almost immediately and gets clamped — either way the test
            // detects it.
            let per_delta = std::time::Duration::from_micros(500);
            let deltas = futures::stream::iter((0..n).map(|i| {
                ProviderStreamEvent::TextDelta(format!("delta {i} of the interrupted stream. "))
            }))
            .then(move |delta| async move {
                tokio::time::sleep(per_delta).await;
                Ok(delta)
            });
            let parked = futures::stream::pending::<
                Result<ProviderStreamEvent, nuo_wire::ProviderError>,
            >();
            Ok(Box::pin(deltas.chain(parked)))
        }
    }

    const DELTAS: usize = 2_400;
    // ~9 600 tokens over ≈1.2 s (500 µs/delta) ⇒ ≈8 000 tok/s — realistic
    // and under the ledger's physical ceiling.
    // The delta counter is file-static (the event closure is a plain FnMut
    // with no environment capture); reset it so repeated runs of this test
    // in one process start from zero.
    SEEN_DELTAS.store(0, Ordering::SeqCst);
    let agent = Arc::new(Agent::new(
        Arc::new(SlowStreamProvider { deltas: DELTAS }),
        Vec::new(),
        crate::AgentIdentity::default(),
    ));
    agent.set_thread_id("interrupt-quadratic");
    agent.bump_round();
    let ledger = nuo_wire::TokenSourceLedger::shared();
    agent.install_token_ledger(ledger.clone());
    let token = CancellationToken::new();
    let cancel_on_start = token.clone();
    let mut messages = vec![Message::new(Role::User, "hello")];

    let result = agent
        .run_streaming_with_events(&mut messages, &token, |event| {
            if let AgentEvent::AssistantDelta { .. } = event {
                // Cancel only once every delta has been observed by the
                // accounting guard (the guard's observe_output runs before
                // this event is emitted). The stream then parks forever, so
                // the turn ends through this cancellation.
                SEEN_DELTAS.fetch_add(1, Ordering::SeqCst);
                if SEEN_DELTAS.load(Ordering::SeqCst) >= DELTAS {
                    cancel_on_start.cancel();
                }
            }
        })
        .await;

    assert!(matches!(result, Err(HarnessError::Interrupted)));
    let records = ledger.records_for_session("interrupt-quadratic");
    assert_eq!(records.len(), 1);
    let record = &records[0];
    assert_eq!(
        record.status,
        nuo_wire::RequestUsageStatus::Interrupted
    );
    // The exact expectation: a whole-text tokenization of the very deltas
    // the attempt streamed — the same predictor the implementation uses, so
    // this pins the *arithmetic* (no cumulative summation), not the corpus.
    let mut streamed = String::new();
    for i in 0..DELTAS {
        streamed.push_str(&format!("delta {i} of the interrupted stream. "));
    }
    let expected = nuo_wire::tokenizer::Tokenizer::new().count(&streamed) as i64;
    assert_eq!(
        record.completion_tokens, expected,
        "the estimate must be the exact whole-stream count, not a running-total sum"
    );
    // And the total must be prompt + completion, not an independent blob.
    assert_eq!(
        record.total_tokens,
        record
            .prompt_tokens
            .saturating_add(record.completion_tokens)
    );
}

/// Delta counter shared by [`interrupted_stream_books_exact_not_cumulative`]
/// so the test's event closure knows when the scripted stream is exhausted.
static SEEN_DELTAS: AtomicUsize = AtomicUsize::new(0);

/// A provider whose `stream_chat_events` future never resolves simulates a
/// server that accepts the TCP connection but never sends HTTP response
/// headers (overloaded upstream, dropped proxy). Without the idle-timeout on
/// the outer select the turn would hang on `.send()` forever. `start_paused`
/// advances the clock past `STREAM_IDLE_TIMEOUT` instantly.
#[tokio::test(start_paused = true)]
async fn stream_request_that_never_resolves_times_out() {
    use std::future::pending;

    struct PendingStreamProvider;
    #[async_trait]
    impl Provider for PendingStreamProvider {
        async fn chat(
            &self,
            _: nuo_wire::ModelRequest,
        ) -> Result<nuo_wire::ProviderCompletion, nuo_wire::ProviderError> {
            unreachable!("streaming path should be used")
        }
        async fn stream_chat(
            &self,
            _: nuo_wire::ModelRequest,
        ) -> Result<
            BoxStream<'static, Result<String, nuo_wire::ProviderError>>,
            nuo_wire::ProviderError,
        > {
            unreachable!("stream_chat_events should be called directly")
        }
        async fn stream_chat_events(
            &self,
            _: nuo_wire::ModelRequest,
        ) -> Result<
            BoxStream<'static, Result<ProviderStreamEvent, nuo_wire::ProviderError>>,
            nuo_wire::ProviderError,
        > {
            // Never resolves.
            pending().await
        }
    }

    let agent = Arc::new(Agent::new(
        Arc::new(PendingStreamProvider),
        Vec::new(),
        crate::AgentIdentity::default(),
    ));
    let mut messages = vec![Message::new(Role::User, "hello")];

    let result = agent
        .run_streaming_with_events(&mut messages, &CancellationToken::new(), |_| {})
        .await;

    assert!(
        matches!(result, Err(nuo_wire::HarnessError::Provider(ref err)) if matches!(err.retry_disposition(), nuo_wire::RetryDisposition::Retry { .. })),
        "a stream request that never resolves should time out as retryable; got: {result:?}"
    );
}

/// A reasoning model may emit reasoning deltas but no text and no tool call
/// (e.g. a truncated or cut-off response). Before the fix to
/// [`valid_assistant_response`], such a response was incorrectly classified
/// as an empty assistant response and surfaced as a terminal error.
/// Reasoning is a legitimate payload from reasoning-model providers, so the
/// turn should complete normally instead of erroring.
#[tokio::test]
async fn reasoning_only_response_is_accepted_not_treated_as_empty() {
    struct ReasoningOnlyProvider;
    #[async_trait]
    impl Provider for ReasoningOnlyProvider {
        async fn chat(
            &self,
            _request: nuo_wire::ModelRequest,
        ) -> Result<nuo_wire::ProviderCompletion, nuo_wire::ProviderError> {
            unreachable!("streaming path should be used")
        }
        async fn stream_chat(
            &self,
            _request: nuo_wire::ModelRequest,
        ) -> Result<
            BoxStream<'static, Result<String, nuo_wire::ProviderError>>,
            nuo_wire::ProviderError,
        > {
            Ok(Box::pin(stream::empty()))
        }
        async fn stream_chat_events(
            &self,
            _request: nuo_wire::ModelRequest,
        ) -> Result<
            BoxStream<'static, Result<ProviderStreamEvent, nuo_wire::ProviderError>>,
            nuo_wire::ProviderError,
        > {
            Ok(Box::pin(stream::iter(vec![
                Ok(ProviderStreamEvent::ReasoningDelta(
                    "let me think...".to_string(),
                )),
                Ok(ProviderStreamEvent::Completed(
                    nuo_wire::ProviderCompletionMeta::default(),
                )),
            ])))
        }
    }

    let agent = Arc::new(Agent::new(
        Arc::new(ReasoningOnlyProvider),
        Vec::new(),
        crate::AgentIdentity::default(),
    ));
    agent.set_trajectory_guard_config(nuo_wire::TrajectoryGuardConfig {
        enabled: true,
        ..nuo_wire::TrajectoryGuardConfig::default()
    });

    let mut messages = vec![Message::new(Role::User, "go")];
    let outcome = agent
        .run_streaming_with_events(&mut messages, &CancellationToken::new(), |_| {})
        .await;

    let outcome = outcome.expect("reasoning-only response should not be treated as empty");
    assert_eq!(outcome.message.content, "");
    assert_eq!(
        outcome.message.reasoning_content.as_deref(),
        Some("let me think...")
    );
}

#[tokio::test]
async fn cancelling_during_tool_execution_emits_tool_cancelled() {
    use std::future::pending;
    use std::sync::Mutex;
    use tokio::sync::Notify;

    struct BlockingTool {
        started: Arc<Notify>,
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

    let started = Arc::new(Notify::new());
    let events: Arc<Mutex<Vec<AgentEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let agent = Arc::new(Agent::new(
        Arc::new(StreamingToolProvider(AtomicUsize::new(0))),
        vec![Arc::new(BlockingTool {
            started: started.clone(),
        })],
        crate::AgentIdentity::default(),
    ));
    let token = CancellationToken::new();
    let mut messages = vec![Message::new(Role::User, "run")];
    let events_for_run = events.clone();

    let run_token = token.clone();
    let handle = tokio::spawn(async move {
        agent
            .run_streaming_with_events(&mut messages, &run_token, |event| {
                if let Ok(mut guard) = events_for_run.lock() {
                    guard.push(event);
                }
            })
            .await
    });

    // Wait until the tool is actually in flight, then interrupt.
    started.notified().await;
    token.cancel();

    let outcome = handle.await.expect("round task panicked");
    assert!(
        matches!(outcome, Err(HarnessError::Interrupted)),
        "expected the turn to be interrupted, got {outcome:?}"
    );

    let recorded = events.lock().expect("events lock poisoned").clone();
    // Every announced ToolCall converges on a terminal event: here a
    // ToolCancelled, never a ToolResult (the turn was aborted).
    assert!(recorded.iter().any(|event| matches!(
        event,
        AgentEvent::ToolCancelled { name, .. } if name == "stream_read"
    )));
    assert!(
        !recorded
            .iter()
            .any(|event| matches!(event, AgentEvent::ToolResult { .. }))
    );
    assert!(
        recorded.iter().any(
            |event| matches!(event, AgentEvent::ToolCall { name, .. } if name == "stream_read")
        )
    );
}

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
            .execute_tool_evented(
                &task_call,
                "call",
                &CancellationToken::new(),
                &mut |event| {
                    let _ = event_tx.send(event);
                },
            )
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
        .execute_tool_evented(&call, "call", &CancellationToken::new(), &mut |event| {
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
            .execute_tool_evented(&call, "call", &CancellationToken::new(), &mut |event| {
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

/// A read-only tool for the subagent child in the drain test.
struct SubagentReadTool;

#[async_trait]
impl Tool for SubagentReadTool {
    fn name(&self) -> &str {
        "read_text"
    }
    fn description(&self) -> &str {
        "test read tool"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }
    async fn call(&self, _arguments: &str) -> Result<String, String> {
        Ok("file contents".to_string())
    }
}

/// A provider whose first request returns a `read_text` tool call and whose
/// second request flips the gate and then stalls forever — so the subagent is
/// parked mid-flight and can only stop when its cancellation token fires.
struct GatedSubagentProvider {
    requests: AtomicUsize,
    gate: tokio::sync::watch::Sender<bool>,
}

#[async_trait]
impl Provider for GatedSubagentProvider {
    async fn chat(
        &self,
        _request: nuo_wire::ModelRequest,
    ) -> Result<nuo_wire::ProviderCompletion, nuo_wire::ProviderError> {
        Ok(nuo_wire::ProviderCompletion::message(Message::new(
            Role::Assistant,
            "gated",
        )))
    }
    async fn stream_chat(
        &self,
        _request: nuo_wire::ModelRequest,
    ) -> Result<
        BoxStream<'static, Result<String, nuo_wire::ProviderError>>,
        nuo_wire::ProviderError,
    > {
        Ok(Box::pin(stream::empty()))
    }
    async fn stream_chat_events(
        &self,
        _request: nuo_wire::ModelRequest,
    ) -> Result<
        BoxStream<
            'static,
            Result<nuo_wire::ProviderStreamEvent, nuo_wire::ProviderError>,
        >,
        nuo_wire::ProviderError,
    > {
        if self.requests.fetch_add(1, Ordering::SeqCst) == 0 {
            Ok(Box::pin(stream::iter(vec![
                Ok(nuo_wire::ProviderStreamEvent::ToolCallDelta {
                    index: 0,
                    id: Some("inner_1".to_string()),
                    name: Some("read_text".to_string()),
                    arguments: "{}".to_string(),
                }),
                Ok(nuo_wire::ProviderStreamEvent::Completed(
                    nuo_wire::ProviderCompletionMeta::default(),
                )),
            ])))
        } else {
            let _ = self.gate.send(true);
            Ok(Box::pin(stream::pending()))
        }
    }
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
        nuo_wire::ToolSet::from_tools(vec![Arc::new(SubagentReadTool) as Arc<dyn Tool>]),
        &nuo_wire::SubAgentProfile::EXPLORE,
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
            .execute_tool_evented(&call, "call_subagent", &cancel_for_run, &mut |_event| {})
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
                "call",
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

// Golden-transcript harness
//
// `ScriptedProvider` replays a fixed list of streamed events — one script
// per ReAct turn — so a whole agent round runs deterministically and its
// emitted `AgentEvent` stream can be asserted as a stable golden
// transcript. This pins the loop's externally-visible contract (tool-call
// ordering, native vs text-fallback dispatch, concurrent result ordering,
// the repeated-call guard, and permission gating) independently of any real
// provider, so the refactors that follow can lean on it as a safety net.

/// A ReAct turn that streams a single chunk of assistant text.
fn text_turn(text: &str) -> Vec<ProviderStreamEvent> {
    vec![ProviderStreamEvent::TextDelta(text.to_string())]
}

/// A ReAct turn that streams native tool calls as `(id, name, arguments)`.
fn turn(calls: &[(&str, &str, &str)]) -> Vec<ProviderStreamEvent> {
    calls
        .iter()
        .enumerate()
        .map(
            |(index, (id, name, arguments))| ProviderStreamEvent::ToolCallDelta {
                index,
                id: Some(id.to_string()),
                name: Some(name.to_string()),
                arguments: arguments.to_string(),
            },
        )
        .collect()
}

struct ScriptedProvider {
    turns: std::sync::Mutex<std::collections::VecDeque<Vec<ProviderStreamEvent>>>,
    cognitive_replies: std::sync::Mutex<std::collections::VecDeque<String>>,
    cognitive_requests: std::sync::Mutex<Vec<nuo_wire::ModelRequest>>,
    /// Pace every stream event with a small gap so the runtime interleaves
    /// spawned cognitive loop consults with stream consumption — the
    /// network cadence an instantly-ready scripted stream omits. Without
    /// it, a current-thread test runtime consumes the whole script before
    /// the detached consult ever runs, so mid-stream verdict cuts can never
    /// be exercised.
    paced: bool,
}

impl ScriptedProvider {
    fn new(turns: Vec<Vec<ProviderStreamEvent>>) -> Self {
        Self {
            turns: std::sync::Mutex::new(turns.into_iter().collect()),
            cognitive_replies: std::sync::Mutex::new(std::collections::VecDeque::new()),
            cognitive_requests: std::sync::Mutex::new(Vec::new()),
            paced: false,
        }
    }

    fn with_cognitive_replies(self, replies: &[&str]) -> Self {
        *self
            .cognitive_replies
            .lock()
            .unwrap_or_else(|error| error.into_inner()) =
            replies.iter().map(|reply| (*reply).to_string()).collect();
        self
    }

    fn paced(mut self) -> Self {
        self.paced = true;
        self
    }
}

#[async_trait]
impl Provider for ScriptedProvider {
    async fn chat(
        &self,
        request: nuo_wire::ModelRequest,
    ) -> Result<nuo_wire::ProviderCompletion, nuo_wire::ProviderError> {
        self.cognitive_requests
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(request);
        let reply = self
            .cognitive_replies
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .pop_front()
            .unwrap_or_else(|| "no".to_string());
        Ok(nuo_wire::ProviderCompletion::message(Message::new(
            Role::Assistant,
            reply,
        )))
    }

    async fn stream_chat(
        &self,
        _request: nuo_wire::ModelRequest,
    ) -> Result<
        BoxStream<'static, Result<String, nuo_wire::ProviderError>>,
        nuo_wire::ProviderError,
    > {
        Ok(Box::pin(stream::empty()))
    }

    async fn stream_chat_events(
        &self,
        _request: nuo_wire::ModelRequest,
    ) -> Result<
        BoxStream<'static, Result<ProviderStreamEvent, nuo_wire::ProviderError>>,
        nuo_wire::ProviderError,
    > {
        // A turn that runs past its script gets a terminal "done" so the
        // loop exits rather than hanging on a missing turn.
        let mut turn = self
            .turns
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop_front()
            .unwrap_or_else(|| text_turn("done"));
        if !turn
            .iter()
            .any(|e| matches!(e, ProviderStreamEvent::Completed(_)))
        {
            turn.push(ProviderStreamEvent::Completed(
                nuo_wire::ProviderCompletionMeta::default(),
            ));
        }
        if self.paced {
            use futures::StreamExt as _;
            let paced = stream::iter(turn.into_iter().map(Ok)).then(|event| async move {
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                event
            });
            return Ok(Box::pin(paced));
        }
        Ok(Box::pin(stream::iter(turn.into_iter().map(Ok))))
    }
}

/// A tool that records every invocation's arguments and returns canned
/// output. The `write` variant declares a [`ScopeTarget::Path`] so the
/// permission broker fires for it; the `read` variant leaves the default
/// [`ScopeTarget::Unspecified`] and skips the broker.
struct RecordingTool {
    name: &'static str,
    output: String,
    declares_target: bool,
    calls: Arc<std::sync::Mutex<Vec<String>>>,
}

impl RecordingTool {
    fn read(name: &'static str, output: &str) -> Self {
        Self {
            name,
            output: output.to_string(),
            declares_target: false,
            calls: Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }

    fn write(name: &'static str, output: &str) -> Self {
        Self {
            declares_target: true,
            ..Self::read(name, output)
        }
    }

    fn calls_handle(&self) -> Arc<std::sync::Mutex<Vec<String>>> {
        Arc::clone(&self.calls)
    }
}

#[async_trait]
impl Tool for RecordingTool {
    fn name(&self) -> &str {
        self.name
    }
    fn description(&self) -> &str {
        "recording test tool"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }
    fn scope_target(&self, arguments: &str) -> nuo_wire::ScopeTarget {
        if self.name == "execute_command" {
            let command = serde_json::from_str::<serde_json::Value>(arguments)
                .ok()
                .and_then(|v| {
                    v.get("command")
                        .and_then(|c| c.as_str())
                        .map(str::to_string)
                })
                .unwrap_or_else(|| arguments.to_string());
            nuo_wire::ScopeTarget::Command(command)
        } else if self.declares_target {
            // Pull a path from the args if present, else a fixed sentinel, so
            // the broker fires for the `write` variant.
            let path = serde_json::from_str::<serde_json::Value>(arguments)
                .ok()
                .and_then(|v| v.get("path").and_then(|p| p.as_str()).map(str::to_string))
                .unwrap_or_else(|| "/tmp/recording".to_string());
            nuo_wire::ScopeTarget::Path(std::path::PathBuf::from(path))
        } else {
            nuo_wire::ScopeTarget::Unspecified
        }
    }
    async fn call(&self, arguments: &str) -> Result<String, String> {
        self.calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(arguments.to_string());
        Ok(self.output.clone())
    }
}

/// Normalise an event stream into a stable, assertable transcript by
/// dropping non-deterministic fields (generated call ids and durations).
fn transcript(events: &[AgentEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event {
            // High-resolution performance samples are non-deterministic by
            // construction; dropped so transcripts stay stable across runs.
            AgentEvent::TurnPerformance(_) => None,
            AgentEvent::BackgroundJobReady { .. } => None,
            AgentEvent::Notice(notice) => {
                Some(format!("notice {:?} {:?}", notice.kind, notice.title))
            }
            AgentEvent::ModelRequestStarted { turn, .. } => {
                Some(format!("model-request turn={turn}"))
            }
            AgentEvent::ContextTokens(_) => Some("context-tokens".to_string()),
            AgentEvent::SteerAdmitted(input) => Some(format!("steer-admitted {:?}", input.text)),
            AgentEvent::AssistantDelta { delta, start } => {
                Some(format!("assistant-delta start={start} {delta:?}"))
            }
            AgentEvent::AssistantEnd(content) => Some(format!("assistant-end {content:?}")),
            AgentEvent::AssistantDiscard => Some("assistant-discard".to_string()),
            AgentEvent::ReasoningDelta { delta, start } => {
                Some(format!("reasoning-delta start={start} {delta:?}"))
            }
            AgentEvent::ReasoningEnd(content) => Some(format!("reasoning-end {content:?}")),
            AgentEvent::ToolCall {
                name, arguments, ..
            } => Some(format!("tool-call {name} {arguments}")),
            AgentEvent::ToolResult { name, output, .. } => {
                Some(format!("tool-result {name} {output:?}"))
            }
            AgentEvent::ToolStream { id, stream } => {
                Some(format!("tool-stream {} {:?}", id, stream))
            }
            AgentEvent::ToolCancelled { name, .. } => Some(format!("tool-cancelled {name}")),
            AgentEvent::UnattendedChanged(enabled) => Some(format!("unattended {enabled}")),
            AgentEvent::ConfinementChanged(enabled) => Some(format!("confinement {enabled}")),
            AgentEvent::PermissionRequest(request) => Some(format!(
                "permission-request {} {}",
                request.tool, request.scope
            )),
            AgentEvent::UserQuestionRequest(request) => {
                Some(format!("user-question {}", request.questions.len()))
            }
            AgentEvent::StdinRequest(request) => Some(format!(
                "stdin-request {} (secret={})",
                request.command, request.secret
            )),
            AgentEvent::Subagent { .. } => Some("subtask".to_string()),
            AgentEvent::TodosUpdated(list) => Some(format!("todos {} items", list.len())),
            AgentEvent::BackgroundJobStarted(info) => {
                Some(format!("background-job-started {}", info.id.0))
            }
            AgentEvent::BackgroundJobProgress { job_id, .. } => {
                Some(format!("background-job-progress {}", job_id.0))
            }
            AgentEvent::BackgroundJobCompleted(outcome) => {
                Some(format!("background-job-completed {}", outcome.job_id.0))
            }
            AgentEvent::CatalogInvalidated => Some("catalog-invalidated".to_string()),
        })
        .collect()
}

/// Drive one full round, auto-answering any permission prompt with `decision`
/// so write-capable tools don't deadlock the loop.
async fn run_golden_round(
    agent: &Arc<Agent>,
    prompt: &str,
    decision: PermissionDecision,
) -> (Vec<AgentEvent>, Result<RoundOutcome, HarnessError>) {
    let mut messages = vec![Message::new(Role::User, prompt)];
    let mut events = Vec::new();
    let outcome = agent
        .run_streaming_with_events(&mut messages, &CancellationToken::new(), |event| {
            if let AgentEvent::PermissionRequest(request) = &event {
                agent.reply_permission(&request.id, decision);
            }
            events.push(event);
        })
        .await;
    (events, outcome)
}

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
    agent.set_trajectory_guard_config(nuo_wire::TrajectoryGuardConfig::disabled());

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
            cmd(), // 2nd: same-signature re-run tolerated (ADR-0148), executes
            cmd(), // 3rd: threshold reached → blocked before execution
            text_turn("done"),
        ])),
        vec![Arc::new(command)],
        crate::AgentIdentity::default(),
    ));
    agent.set_trajectory_guard_config(nuo_wire::TrajectoryGuardConfig {
        enabled: true,
        threshold: 3,
        cognitive_review: false,
        ..nuo_wire::TrajectoryGuardConfig::default()
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
    let reader = RecordingTool::read("read_text", "READ");
    let reader_calls = reader.calls_handle();
    let lister = RecordingTool::read("list_dir", "LIST");
    let lister_calls = lister.calls_handle();
    // Three reads of big.rs (3rd reaches the default threshold → blocked),
    // then a read of small.rs (must succeed — different path), then a list_dir
    // (must succeed — different tool), then done.
    let read_big = || turn(&[("c", "read_text", r#"{"path":"big.rs"}"#)]);
    let read_small = || turn(&[("c", "read_text", r#"{"path":"small.rs"}"#)]);
    let list = || turn(&[("c", "list_dir", r#"{"path":"."}"#)]);
    let agent = Arc::new(Agent::new(
        Arc::new(ScriptedProvider::new(vec![
            read_big(),
            read_big(),
            read_big(),
            read_small(),
            list(),
            text_turn("done"),
        ])),
        vec![Arc::new(reader), Arc::new(lister)],
        crate::AgentIdentity::default(),
    ));
    agent.set_trajectory_guard_config(nuo_wire::TrajectoryGuardConfig {
        enabled: true,
        threshold: 3,
        cognitive_review: false,
        ..nuo_wire::TrajectoryGuardConfig::default()
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
        lister_calls.lock().unwrap().len(),
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
    agent.set_trajectory_guard_config(nuo_wire::TrajectoryGuardConfig::disabled());
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

/// ADR-0247: Trajectory Loop Guard with Cognitive Arbitration confirms loop and blocks surgically.
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
    agent.set_trajectory_guard_config(nuo_wire::TrajectoryGuardConfig::cognitive());
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

/// ADR-0247: Trajectory Loop Guard with Cognitive Arbitration acquits and escalates ladder.
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
    agent.set_trajectory_guard_config(nuo_wire::TrajectoryGuardConfig::cognitive());
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

/// ADR-0247: Trajectory Loop Guard fails open on provider error [INV-LOOP-02] and escalates ladder.
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
    agent.set_trajectory_guard_config(nuo_wire::TrajectoryGuardConfig::cognitive());
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
    let child_policy = nuo_wire::ExecutionPolicy {
        depth: 1,
        max_depth: 1,
        allow_human_interaction: false,
        lifecycle: nuo_wire::ContextLifecycle::EphemeralScratchpad,
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
            .execute_tool_evented(&call, "call", &CancellationToken::new(), &mut |event| {
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

/// Build N distinct read-only `alpha` turns (each with a different
/// path so they count as distinct calls rather than repeats), optionally
/// followed by a final text turn. Drives the turn count past a review line
/// without accumulating repeated-call counts.
fn distinct_read_turns(n: usize, suffix: Option<&str>) -> Vec<Vec<ProviderStreamEvent>> {
    let mut turns: Vec<Vec<ProviderStreamEvent>> = (0..n)
        .map(|i| turn(&[("c", "alpha", &format!("{{\"path\":\"f{i}\"}}"))]))
        .collect();
    if let Some(s) = suffix {
        turns.push(text_turn(s));
    }
    turns
}

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
        _request: nuo_wire::ModelRequest,
    ) -> Result<nuo_wire::ProviderCompletion, nuo_wire::ProviderError> {
        Err(nuo_wire::ProviderError::new(
            "mock",
            nuo_wire::ProviderErrorKind::Other,
            "chat path not used by this test",
        ))
    }
    async fn stream_chat(
        &self,
        _request: nuo_wire::ModelRequest,
    ) -> Result<
        BoxStream<'static, Result<String, nuo_wire::ProviderError>>,
        nuo_wire::ProviderError,
    > {
        Err(nuo_wire::ProviderError::new(
            "mock",
            nuo_wire::ProviderErrorKind::Other,
            "stream_chat path not used by this test",
        ))
    }
    async fn stream_chat_events(
        &self,
        _request: nuo_wire::ModelRequest,
    ) -> Result<
        BoxStream<'static, Result<ProviderStreamEvent, nuo_wire::ProviderError>>,
        nuo_wire::ProviderError,
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
        .chat(vec![Message::new(Role::User, "hi")].into())
        .await
        .unwrap();
    let off_count = std::fs::read_dir(&dir).map(|entries| entries.count()).ok();
    assert_eq!(off_count, None, "no directory created while capture is off");

    // Arming creates exactly one JSON file per round-trip on the chat path.
    proxy.set_debug_capture(true, dir.clone());
    assert!(proxy.debug_capture_enabled());
    proxy
        .chat(vec![Message::new(Role::User, "hello")].into())
        .await
        .unwrap();
    proxy
        .chat(vec![Message::new(Role::User, "again")].into())
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
        .chat(vec![Message::new(Role::User, "after off")].into())
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
        .stream_chat_events(vec![Message::new(Role::User, "hi")].into())
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
        nuo_wire::ToolSet::from_tools(vec![Arc::new(SubagentReadTool) as Arc<dyn Tool>]),
        &nuo_wire::SubAgentProfile::EXPLORE,
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
        fn scope_target(&self, arguments: &str) -> nuo_wire::ScopeTarget {
            let path = serde_json::from_str::<serde_json::Value>(arguments)
                .ok()
                .and_then(|v| v.get("path").and_then(|p| p.as_str()).map(str::to_string))
                .unwrap_or_else(|| "/tmp/probe".to_string());
            nuo_wire::ScopeTarget::Path(std::path::PathBuf::from(path))
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
async fn agent_compiles_request_directly_from_session_ir() {
    let provider: Arc<dyn Provider> = Arc::new(TestProvider);
    let agent = Agent::new(provider, Vec::new(), crate::AgentIdentity::default());

    let policy = nuo_wire::SessionPolicy::default();
    let mut ir = nuo_wire::SessionIR::new("test-session-agent", policy, 1000);
    ir.append_message(
        "n1",
        1001,
        Message::new(Role::User, "Hello from native SessionIR to Agent"),
    );

    let artifact = agent
        .model_request_from_ir(&ir)
        .expect("Agent must compile model request from SessionIR without error");

    assert_eq!(artifact.request.messages.len(), 1);
    assert_eq!(
        artifact.request.messages[0].content,
        "Hello from native SessionIR to Agent"
    );
    assert_eq!(artifact.stats.nodes_traversed, 1);
    assert!(!artifact.cache_boundary.prefix_fingerprint.is_empty());
}
