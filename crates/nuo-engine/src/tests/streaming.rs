use super::*;

#[tokio::test]
async fn provider_turn_context_survives_tools_and_retries_but_not_new_rounds() {
    use std::sync::Mutex;
    struct RecordingProvider {
        inner: StreamingToolProvider,
        contexts: Mutex<Vec<Arc<nuo_protocol::ProviderTurnContext>>>,
    }
    #[async_trait]
    impl Provider for RecordingProvider {
        async fn chat(
            &self,
            request: &nuo_protocol::ModelRequest,
        ) -> Result<nuo_protocol::ProviderCompletion, nuo_protocol::ProviderError> {
            self.inner.chat(request).await
        }
        async fn stream_chat(
            &self,
            request: &nuo_protocol::ModelRequest,
            telemetry: nuo_protocol::TransportTelemetry,
        ) -> Result<
            BoxStream<'static, Result<String, nuo_protocol::ProviderError>>,
            nuo_protocol::ProviderError,
        > {
            self.inner.stream_chat(request, telemetry).await
        }
        async fn stream_chat_events(
            &self,
            request: &nuo_protocol::ModelRequest,
            telemetry: nuo_protocol::TransportTelemetry,
        ) -> Result<nuo_protocol::ProviderEventStream, nuo_protocol::ProviderError> {
            let first = {
                let mut contexts = self.contexts.lock().unwrap();
                contexts.push(Arc::clone(&request.turn_context));
                contexts.len() == 1
            };
            if first {
                return Err(nuo_protocol::ProviderError::new(
                    "mock",
                    nuo_protocol::ProviderErrorKind::Unavailable,
                    "retry",
                )
                .retryable(None));
            }
            self.inner.stream_chat_events(request, telemetry).await
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
async fn streamed_tool_input_is_announced_then_progressed_before_completion() {
    // ADR-0026: a tool call whose arguments stream is (1) announced with its
    // name before the arguments finish, and (2) ticked with a count-only byte
    // progress — both strictly before the whole-argument `ToolCall` that gates
    // dispatch ([INV-STREAM-TOOL-01]/[INV-STREAM-TOOL-02]).
    let arg_a = format!("{{\"value\":\"{}", "a".repeat(3000));
    let arg_b = format!("{}\"}}", "b".repeat(3000));
    let total = arg_a.len() + arg_b.len();
    let agent = Arc::new(Agent::new(
        Arc::new(ScriptedProvider::new(vec![
            vec![
                ProviderStreamEvent::ToolCallDelta {
                    index: 0,
                    id: Some("call_x".to_string()),
                    name: Some("big_tool".to_string()),
                    arguments: arg_a,
                },
                ProviderStreamEvent::ToolCallDelta {
                    index: 0,
                    id: None,
                    name: None,
                    arguments: arg_b,
                },
            ],
            text_turn("finished"),
        ])),
        vec![Arc::new(RecordingTool::read("big_tool", "ok"))],
        crate::AgentIdentity::default(),
    ));
    let mut messages = vec![Message::new(Role::User, "go")];
    let mut events = Vec::new();
    let _ = agent
        .run_streaming_with_events(&mut messages, &CancellationToken::new(), |event| {
            events.push(event)
        })
        .await;

    let started = events
        .iter()
        .position(
            |event| matches!(event, AgentEvent::ToolCallStarted { name, .. } if name == "big_tool"),
        )
        .expect("the call must be announced before its arguments finish");
    let completed = events
        .iter()
        .position(|event| matches!(event, AgentEvent::ToolCall { name, .. } if name == "big_tool"))
        .expect("the whole-argument ToolCall must still fire");
    let progress = events
        .iter()
        .position(
            |event| matches!(event, AgentEvent::ToolInputProgress { bytes, .. } if *bytes == total),
        )
        .expect("count-only progress must tick at the bounded cadence");

    assert!(started < completed, "announcement must precede completion");
    assert!(progress < completed, "progress must precede completion");
    // Exactly one announcement per provider slot, however fragmented the stream.
    let started_count = events
        .iter()
        .filter(|event| matches!(event, AgentEvent::ToolCallStarted { .. }))
        .count();
    assert_eq!(started_count, 1);

    // ADR-0166: the dispatch carries the *same* call id the announcement did,
    // so a frontend advances the row instead of guessing from slot order.
    let announced_id = events
        .iter()
        .find_map(|event| match event {
            AgentEvent::ToolCallStarted { id, name } if name == "big_tool" => Some(id.clone()),
            _ => None,
        })
        .expect("announced call id");
    let discharged_id = events
        .iter()
        .find_map(|event| match event {
            AgentEvent::ToolCall { id, name, .. } if name == "big_tool" => Some(id.clone()),
            _ => None,
        })
        .expect("tool call");
    assert_eq!(discharged_id, announced_id);
    assert!(!announced_id.is_empty(), "the call id is never empty");
}

#[tokio::test]
async fn a_provider_that_reuses_a_call_id_is_refused_not_merged() {
    // `[INV-TOOL-ID-01]`: an id identifies exactly one call. Merging two calls
    // under one row would silently give one of them the other's result, so the
    // attempt is refused as a protocol violation instead.
    let duplicate_ids = vec![
        ProviderStreamEvent::ToolCallDelta {
            index: 0,
            id: Some("call_same".to_string()),
            name: Some("alpha".to_string()),
            arguments: r#"{"k":1}"#.to_string(),
        },
        ProviderStreamEvent::ToolCallDelta {
            index: 1,
            id: Some("call_same".to_string()),
            name: Some("beta".to_string()),
            arguments: r#"{"k":2}"#.to_string(),
        },
    ];
    let agent = Arc::new(Agent::new(
        Arc::new(ScriptedProvider::new(vec![duplicate_ids])),
        vec![
            Arc::new(RecordingTool::read("alpha", "A-out")),
            Arc::new(RecordingTool::read("beta", "B-out")),
        ],
        crate::AgentIdentity::default(),
    ));

    let (events, outcome) = run_golden_round(&agent, "go", PermissionDecision::Once).await;
    let error = outcome.expect_err("a duplicate call id is a protocol violation");
    assert!(
        format!("{error}").contains("same id"),
        "the error must name the violation, got: {error}"
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            AgentEvent::ToolCallsRetracted { ids } if ids.iter().any(|id| id == "call_same")
        )),
        "both announced rows for the colliding id are retracted"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::ToolCall { .. })),
        "neither call is dispatched from an ambiguous response"
    );
}

#[tokio::test]
async fn a_late_provider_id_cannot_re_key_an_announced_call() {
    // The first fragment names the call without an id, so the harness mints one
    // and announces it. A later id fragment must not replace it: the frontend
    // already holds the row under the announced id. The anomaly is logged, not
    // silently merged.
    let late_id_turn = vec![
        ProviderStreamEvent::ToolCallDelta {
            index: 0,
            id: None,
            name: Some("big_tool".to_string()),
            arguments: r#"{"va"#.to_string(),
        },
        ProviderStreamEvent::ToolCallDelta {
            index: 0,
            id: Some("late_id".to_string()),
            name: None,
            arguments: r#"lue":1}"#.to_string(),
        },
    ];
    let agent = Arc::new(Agent::new(
        Arc::new(ScriptedProvider::new(vec![late_id_turn, text_turn("done")])),
        vec![Arc::new(RecordingTool::read("big_tool", "ok"))],
        crate::AgentIdentity::default(),
    ));

    let (events, outcome) = run_golden_round(&agent, "go", PermissionDecision::Once).await;
    assert_eq!(outcome.expect("the call executes").message.content, "done");

    let announced = events
        .iter()
        .find_map(|event| match event {
            AgentEvent::ToolCallStarted { id, name } if name == "big_tool" => Some(id.clone()),
            _ => None,
        })
        .expect("announced");
    let dispatched = events
        .iter()
        .find_map(|event| match event {
            AgentEvent::ToolCall { id, name, .. } if name == "big_tool" => Some(id.clone()),
            _ => None,
        })
        .expect("dispatched");
    assert_eq!(
        dispatched, announced,
        "the announced id is the call's identity for its whole life"
    );
    assert_ne!(
        announced, "late_id",
        "a late provider id must not re-key a row the frontend already holds"
    );
}

#[tokio::test]
async fn an_aborted_stream_retracts_its_announced_calls_rather_than_cancelling_them() {
    // ADR-0026 lifecycle: a call announced by an attempt that dies before
    // dispatch (here: truncated arguments, which is exactly the fast-model
    // failure shape) must be **retracted**. It must not surface as a
    // `ToolCall`, and no `ToolCancelled` may be synthesized for a call that
    // never existed.
    let agent = Arc::new(Agent::new(
        Arc::new(ScriptedProvider::new(vec![vec![
            ProviderStreamEvent::ToolCallDelta {
                index: 0,
                id: Some("call_dead".to_string()),
                name: Some("big_tool".to_string()),
                arguments: "{\"value\":\"unterminated".to_string(),
            },
        ]])),
        vec![Arc::new(RecordingTool::read("big_tool", "ok"))],
        crate::AgentIdentity::default(),
    ));
    let mut messages = vec![Message::new(Role::User, "go")];
    let mut events = Vec::new();
    let result = agent
        .run_streaming_with_events(&mut messages, &CancellationToken::new(), |event| {
            events.push(event)
        })
        .await;

    assert!(
        result.is_err(),
        "a truncated tool argument aborts the attempt"
    );

    let announced = events
        .iter()
        .find_map(|event| match event {
            AgentEvent::ToolCallStarted { id, name } if name == "big_tool" => Some(id.clone()),
            _ => None,
        })
        .expect("the call must have been announced before the stream died");
    let retracted: Vec<String> = events
        .iter()
        .flat_map(|event| match event {
            AgentEvent::ToolCallsRetracted { ids } => ids.clone(),
            _ => Vec::new(),
        })
        .collect();
    assert_eq!(
        retracted,
        vec![announced],
        "exactly the dead attempt's call is retracted, by its own id"
    );
    assert!(
        !events.iter().any(|event| matches!(
            event,
            AgentEvent::ToolCall { name, .. } if name == "big_tool"
        )),
        "a never-executable call must not be dispatched"
    );
    assert!(
        !events.iter().any(|event| matches!(
            event,
            AgentEvent::ToolCancelled { name, .. } if name == "big_tool"
        )),
        "retraction is not cancellation: no cancel event for a call that never existed"
    );
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
/// timeout the turn loop blocks on `stream.next()` forever — the UI spins/// "running | responding" and only a user interrupt can break it. The
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
            _request: &nuo_protocol::ModelRequest,
        ) -> Result<nuo_protocol::ProviderCompletion, nuo_protocol::ProviderError> {
            unreachable!("streaming path should be used")
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
        async fn stream_chat_events(
            &self,
            _request: &nuo_protocol::ModelRequest,
            _telemetry: nuo_protocol::TransportTelemetry,
        ) -> Result<
            BoxStream<'static, Result<ProviderStreamEvent, nuo_protocol::ProviderError>>,
            nuo_protocol::ProviderError,
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
        matches!(result, Err(nuo_protocol::HarnessError::Provider(ref err)) if matches!(err.retry_disposition(), nuo_protocol::RetryDisposition::Retry { .. })),
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
            _request: &nuo_protocol::ModelRequest,
        ) -> Result<nuo_protocol::ProviderCompletion, nuo_protocol::ProviderError> {
            unreachable!("streaming path should be used")
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
        async fn stream_chat_events(
            &self,
            _request: &nuo_protocol::ModelRequest,
            _telemetry: nuo_protocol::TransportTelemetry,
        ) -> Result<
            BoxStream<'static, Result<ProviderStreamEvent, nuo_protocol::ProviderError>>,
            nuo_protocol::ProviderError,
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
        Err(nuo_protocol::HarnessError::Provider(err))
            if matches!(
                err.retry_disposition(),
                nuo_protocol::RetryDisposition::Retry { .. }
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
            _request: &nuo_protocol::ModelRequest,
        ) -> Result<nuo_protocol::ProviderCompletion, nuo_protocol::ProviderError> {
            unreachable!("streaming path should be used")
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
        async fn stream_chat_events(
            &self,
            _request: &nuo_protocol::ModelRequest,
            _telemetry: nuo_protocol::TransportTelemetry,
        ) -> Result<
            BoxStream<'static, Result<ProviderStreamEvent, nuo_protocol::ProviderError>>,
            nuo_protocol::ProviderError,
        > {
            Ok(Box::pin(stream::iter(vec![Ok(
                ProviderStreamEvent::Completed(nuo_protocol::ProviderCompletionMeta::default()),
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
        Err(nuo_protocol::HarnessError::Provider(err))
            if matches!(
                err.retry_disposition(),
                nuo_protocol::RetryDisposition::Retry { .. }
            ) =>
        {
            let message = err.message();
            assert!(
                message.contains("empty assistant response"),
                "message should name the empty-response fault: {message}"
            );
            assert_eq!(
                err.kind(),
                nuo_protocol::ProviderErrorKind::Upstream,
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
            _request: &nuo_protocol::ModelRequest,
        ) -> Result<nuo_protocol::ProviderCompletion, nuo_protocol::ProviderError> {
            unreachable!("streaming path should be used")
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
        async fn stream_chat_events(
            &self,
            _request: &nuo_protocol::ModelRequest,
            _telemetry: nuo_protocol::TransportTelemetry,
        ) -> Result<
            BoxStream<'static, Result<ProviderStreamEvent, nuo_protocol::ProviderError>>,
            nuo_protocol::ProviderError,
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
        Err(nuo_protocol::HarnessError::Provider(err))
            if matches!(
                err.retry_disposition(),
                nuo_protocol::RetryDisposition::Retry { .. }
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
            _: &nuo_protocol::ModelRequest,
        ) -> Result<nuo_protocol::ProviderCompletion, nuo_protocol::ProviderError> {
            unreachable!("streaming path should be used")
        }
        async fn stream_chat(
            &self,
            _: &nuo_protocol::ModelRequest,
            _telemetry: nuo_protocol::TransportTelemetry,
        ) -> Result<
            BoxStream<'static, Result<String, nuo_protocol::ProviderError>>,
            nuo_protocol::ProviderError,
        > {
            Ok(Box::pin(stream::pending()))
        }
        async fn stream_chat_events(
            &self,
            _: &nuo_protocol::ModelRequest,
            _telemetry: nuo_protocol::TransportTelemetry,
        ) -> Result<
            BoxStream<'static, Result<ProviderStreamEvent, nuo_protocol::ProviderError>>,
            nuo_protocol::ProviderError,
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
    let ledger = nuo_protocol::TokenSourceLedger::shared();
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
        nuo_protocol::RequestUsageStatus::Interrupted
    );
    assert_eq!(
        records[0].source,
        nuo_protocol::RequestUsageSource::Estimated
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
            _: &nuo_protocol::ModelRequest,
        ) -> Result<nuo_protocol::ProviderCompletion, nuo_protocol::ProviderError> {
            unreachable!("streaming path should be used")
        }
        async fn stream_chat(
            &self,
            _: &nuo_protocol::ModelRequest,
            _telemetry: nuo_protocol::TransportTelemetry,
        ) -> Result<
            BoxStream<'static, Result<String, nuo_protocol::ProviderError>>,
            nuo_protocol::ProviderError,
        > {
            Ok(Box::pin(stream::pending()))
        }
        async fn stream_chat_events(
            &self,
            _: &nuo_protocol::ModelRequest,
            _telemetry: nuo_protocol::TransportTelemetry,
        ) -> Result<
            BoxStream<'static, Result<ProviderStreamEvent, nuo_protocol::ProviderError>>,
            nuo_protocol::ProviderError,
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
                Result<ProviderStreamEvent, nuo_protocol::ProviderError>,
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
    let ledger = nuo_protocol::TokenSourceLedger::shared();
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
    assert_eq!(record.status, nuo_protocol::RequestUsageStatus::Interrupted);
    // The exact expectation: a whole-text tokenization of the very deltas
    // the attempt streamed — the same predictor the implementation uses, so
    // this pins the *arithmetic* (no cumulative summation), not the corpus.
    let mut streamed = String::new();
    for i in 0..DELTAS {
        streamed.push_str(&format!("delta {i} of the interrupted stream. "));
    }
    let expected = nuo_protocol::tokenizer::Tokenizer::new().count(&streamed) as i64;
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
            _: &nuo_protocol::ModelRequest,
        ) -> Result<nuo_protocol::ProviderCompletion, nuo_protocol::ProviderError> {
            unreachable!("streaming path should be used")
        }
        async fn stream_chat(
            &self,
            _: &nuo_protocol::ModelRequest,
            _telemetry: nuo_protocol::TransportTelemetry,
        ) -> Result<
            BoxStream<'static, Result<String, nuo_protocol::ProviderError>>,
            nuo_protocol::ProviderError,
        > {
            unreachable!("stream_chat_events should be called directly")
        }
        async fn stream_chat_events(
            &self,
            _: &nuo_protocol::ModelRequest,
            _telemetry: nuo_protocol::TransportTelemetry,
        ) -> Result<
            BoxStream<'static, Result<ProviderStreamEvent, nuo_protocol::ProviderError>>,
            nuo_protocol::ProviderError,
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
        matches!(result, Err(nuo_protocol::HarnessError::Provider(ref err)) if matches!(err.retry_disposition(), nuo_protocol::RetryDisposition::Retry { .. })),
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
            _request: &nuo_protocol::ModelRequest,
        ) -> Result<nuo_protocol::ProviderCompletion, nuo_protocol::ProviderError> {
            unreachable!("streaming path should be used")
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
        async fn stream_chat_events(
            &self,
            _request: &nuo_protocol::ModelRequest,
            _telemetry: nuo_protocol::TransportTelemetry,
        ) -> Result<
            BoxStream<'static, Result<ProviderStreamEvent, nuo_protocol::ProviderError>>,
            nuo_protocol::ProviderError,
        > {
            Ok(Box::pin(stream::iter(vec![
                Ok(ProviderStreamEvent::ReasoningDelta(
                    "let me think...".to_string(),
                )),
                Ok(ProviderStreamEvent::Completed(
                    nuo_protocol::ProviderCompletionMeta::default(),
                )),
            ])))
        }
    }

    let agent = Arc::new(Agent::new(
        Arc::new(ReasoningOnlyProvider),
        Vec::new(),
        crate::AgentIdentity::default(),
    ));
    agent.set_trajectory_guard_config(nuo_protocol::TrajectoryGuardConfig {
        enabled: true,
        ..nuo_protocol::TrajectoryGuardConfig::default()
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
