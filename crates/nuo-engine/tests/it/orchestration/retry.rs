//! Provider retry behavior across turns, tool activity, and transport telemetry.

use super::*;

use super::common::provider_history_snapshot;
struct MockProvider;

#[async_trait]
impl Provider for MockProvider {
    async fn chat(
        &self,
        _request: &nuo_protocol::ModelRequest,
    ) -> Result<nuo_protocol::ProviderCompletion, nuo_protocol::ProviderError> {
        Ok(nuo_protocol::ProviderCompletion::message(Message::new(
            Role::Assistant,
            "Hello! I am a mock AI. How can I help you today?",
        )))
    }

    async fn stream_chat(
        &self,
        _request: &nuo_protocol::ModelRequest,
        _telemetry: nuo_protocol::TransportTelemetry,
    ) -> Result<
        futures::stream::BoxStream<'static, Result<String, nuo_protocol::ProviderError>>,
        nuo_protocol::ProviderError,
    > {
        Ok(Box::pin(stream::empty()))
    }
}

/// Most built-in tools self-register via `inventory` across the nuo-harness
/// and nuo-persistence crates. This test guards the one real
/// risk of that approach — that a crate's `inventory::submit!` nodes get
/// dropped by the linker — by asserting the assembled set contains every
/// expected built-in tool name.
struct RetryOnceProvider(AtomicUsize);
pub(super) struct PartialToolRetryProvider(pub(super) AtomicUsize);
pub(super) struct ToolThenRetryProvider {
    pub(super) attempts: AtomicUsize,
    pub(super) requests: Arc<Mutex<Vec<String>>>,
}
pub(super) struct AlwaysRetryableProvider;
pub(super) struct RetryReadTool(pub(super) Arc<AtomicUsize>);

#[async_trait]
impl Provider for RetryOnceProvider {
    async fn chat(
        &self,
        _request: &nuo_protocol::ModelRequest,
    ) -> Result<nuo_protocol::ProviderCompletion, nuo_protocol::ProviderError> {
        Err(nuo_protocol::ProviderError::new(
            "mock",
            nuo_protocol::ProviderErrorKind::Other,
            "non-streaming path should not be used",
        ))
    }

    async fn stream_chat(
        &self,
        _request: &nuo_protocol::ModelRequest,
        _telemetry: nuo_protocol::TransportTelemetry,
    ) -> Result<
        futures::stream::BoxStream<'static, Result<String, nuo_protocol::ProviderError>>,
        nuo_protocol::ProviderError,
    > {
        Ok(Box::pin(stream::empty()))
    }

    async fn stream_chat_events(
        &self,
        _request: &nuo_protocol::ModelRequest,
        _telemetry: nuo_protocol::TransportTelemetry,
    ) -> Result<
        futures::stream::BoxStream<
            'static,
            Result<ProviderStreamEvent, nuo_protocol::ProviderError>,
        >,
        nuo_protocol::ProviderError,
    > {
        if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
            Ok(Box::pin(stream::iter(vec![
                Ok(ProviderStreamEvent::TextDelta("partial".to_string())),
                Err(nuo_protocol::ProviderError::new(
                    "mock",
                    nuo_protocol::ProviderErrorKind::RateLimited,
                    "rate limited",
                )
                .retryable(Some(1))),
            ])))
        } else {
            Ok(Box::pin(stream::iter(vec![
                Ok(ProviderStreamEvent::TextDelta("done".to_string())),
                Ok(ProviderStreamEvent::Completed(
                    nuo_protocol::ProviderCompletionMeta::default(),
                )),
            ])))
        }
    }
}

#[async_trait]
impl Provider for PartialToolRetryProvider {
    async fn chat(
        &self,
        _request: &nuo_protocol::ModelRequest,
    ) -> Result<nuo_protocol::ProviderCompletion, nuo_protocol::ProviderError> {
        Err(nuo_protocol::ProviderError::new(
            "mock",
            nuo_protocol::ProviderErrorKind::Other,
            "non-streaming path should not be used",
        ))
    }

    async fn stream_chat(
        &self,
        _request: &nuo_protocol::ModelRequest,
        _telemetry: nuo_protocol::TransportTelemetry,
    ) -> Result<
        futures::stream::BoxStream<'static, Result<String, nuo_protocol::ProviderError>>,
        nuo_protocol::ProviderError,
    > {
        Ok(Box::pin(stream::empty()))
    }

    async fn stream_chat_events(
        &self,
        _request: &nuo_protocol::ModelRequest,
        _telemetry: nuo_protocol::TransportTelemetry,
    ) -> Result<
        futures::stream::BoxStream<
            'static,
            Result<ProviderStreamEvent, nuo_protocol::ProviderError>,
        >,
        nuo_protocol::ProviderError,
    > {
        if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
            Ok(Box::pin(stream::iter(vec![
                Ok(ProviderStreamEvent::ToolCallDelta {
                    index: 0,
                    id: Some("partial-call".to_string()),
                    name: Some("retry_read".to_string()),
                    arguments: "{".to_string(),
                }),
                Err(nuo_protocol::ProviderError::new(
                    "mock",
                    nuo_protocol::ProviderErrorKind::Transport,
                    "stream dropped",
                )
                .retryable(None)),
            ])))
        } else {
            Ok(Box::pin(stream::iter(vec![
                Ok(ProviderStreamEvent::TextDelta("done".to_string())),
                Ok(ProviderStreamEvent::Completed(
                    nuo_protocol::ProviderCompletionMeta::default(),
                )),
            ])))
        }
    }
}

#[async_trait]
impl Provider for ToolThenRetryProvider {
    async fn chat(
        &self,
        _request: &nuo_protocol::ModelRequest,
    ) -> Result<nuo_protocol::ProviderCompletion, nuo_protocol::ProviderError> {
        Err(nuo_protocol::ProviderError::new(
            "mock",
            nuo_protocol::ProviderErrorKind::Other,
            "non-streaming path should not be used",
        ))
    }

    async fn stream_chat(
        &self,
        _request: &nuo_protocol::ModelRequest,
        _telemetry: nuo_protocol::TransportTelemetry,
    ) -> Result<
        futures::stream::BoxStream<'static, Result<String, nuo_protocol::ProviderError>>,
        nuo_protocol::ProviderError,
    > {
        Ok(Box::pin(stream::empty()))
    }

    async fn stream_chat_events(
        &self,
        request: &nuo_protocol::ModelRequest,
        _telemetry: nuo_protocol::TransportTelemetry,
    ) -> Result<
        futures::stream::BoxStream<
            'static,
            Result<ProviderStreamEvent, nuo_protocol::ProviderError>,
        >,
        nuo_protocol::ProviderError,
    > {
        self.requests
            .lock()
            .expect("request log lock poisoned")
            .push(provider_history_snapshot(&request.messages));
        let attempt = self.attempts.fetch_add(1, Ordering::SeqCst);
        match attempt {
            0 | 2 => Ok(Box::pin(stream::iter(vec![
                Ok(ProviderStreamEvent::ToolCallDelta {
                    index: 0,
                    id: Some(if attempt == 0 { "call" } else { "retry-call" }.to_string()),
                    name: Some("retry_read".to_string()),
                    arguments: "{}".to_string(),
                }),
                Ok(ProviderStreamEvent::Completed(
                    nuo_protocol::ProviderCompletionMeta::default(),
                )),
            ]))),
            1 => Ok(Box::pin(stream::iter(vec![Err(
                nuo_protocol::ProviderError::new(
                    "mock",
                    nuo_protocol::ProviderErrorKind::Unavailable,
                    "upstream unavailable",
                )
                .retryable(None),
            )]))),
            _ => Ok(Box::pin(stream::iter(vec![
                Ok(ProviderStreamEvent::TextDelta("done".to_string())),
                Ok(ProviderStreamEvent::Completed(
                    nuo_protocol::ProviderCompletionMeta::default(),
                )),
            ]))),
        }
    }
}

#[async_trait]
impl Provider for AlwaysRetryableProvider {
    async fn chat(
        &self,
        _request: &nuo_protocol::ModelRequest,
    ) -> Result<nuo_protocol::ProviderCompletion, nuo_protocol::ProviderError> {
        Err(nuo_protocol::ProviderError::new(
            "mock",
            nuo_protocol::ProviderErrorKind::Other,
            "non-streaming path should not be used",
        ))
    }

    async fn stream_chat(
        &self,
        _request: &nuo_protocol::ModelRequest,
        _telemetry: nuo_protocol::TransportTelemetry,
    ) -> Result<
        futures::stream::BoxStream<'static, Result<String, nuo_protocol::ProviderError>>,
        nuo_protocol::ProviderError,
    > {
        Ok(Box::pin(stream::empty()))
    }

    async fn stream_chat_events(
        &self,
        _request: &nuo_protocol::ModelRequest,
        _telemetry: nuo_protocol::TransportTelemetry,
    ) -> Result<
        futures::stream::BoxStream<
            'static,
            Result<ProviderStreamEvent, nuo_protocol::ProviderError>,
        >,
        nuo_protocol::ProviderError,
    > {
        // Every request fails with a retryable error so the turn exhausts
        // its retry budget without ever touching a tool.
        Ok(Box::pin(stream::iter(vec![Err(
            nuo_protocol::ProviderError::new(
                "openai",
                nuo_protocol::ProviderErrorKind::RateLimited,
                "OpenAI HTTP 429 Too Many Requests",
            )
            .retryable(None),
        )])))
    }
}

/// Emits visible output on every attempt, then fails retryably. Used to pin
/// the split between the silent-attempt budget and the stream-restart budget.
pub(crate) struct VisibleFailProvider(pub(crate) Arc<AtomicUsize>);

#[async_trait]
impl Provider for VisibleFailProvider {
    async fn chat(
        &self,
        _request: &nuo_protocol::ModelRequest,
    ) -> Result<nuo_protocol::ProviderCompletion, nuo_protocol::ProviderError> {
        Err(nuo_protocol::ProviderError::new(
            "mock",
            nuo_protocol::ProviderErrorKind::Other,
            "non-streaming path should not be used",
        ))
    }

    async fn stream_chat(
        &self,
        _request: &nuo_protocol::ModelRequest,
        _telemetry: nuo_protocol::TransportTelemetry,
    ) -> Result<
        futures::stream::BoxStream<'static, Result<String, nuo_protocol::ProviderError>>,
        nuo_protocol::ProviderError,
    > {
        Ok(Box::pin(stream::empty()))
    }

    async fn stream_chat_events(
        &self,
        _request: &nuo_protocol::ModelRequest,
        _telemetry: nuo_protocol::TransportTelemetry,
    ) -> Result<
        futures::stream::BoxStream<
            'static,
            Result<ProviderStreamEvent, nuo_protocol::ProviderError>,
        >,
        nuo_protocol::ProviderError,
    > {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderStreamEvent::TextDelta("partial answer".to_string())),
            Err(nuo_protocol::ProviderError::new(
                "mock",
                nuo_protocol::ProviderErrorKind::RateLimited,
                "mid-stream rate limit",
            )
            .retryable(None)),
        ])))
    }
}

#[async_trait]
impl nuo_protocol::Tool for RetryReadTool {
    fn name(&self) -> &str {
        "retry_read"
    }

    fn description(&self) -> &str {
        "retry safety test"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }

    async fn call(&self, _arguments: &str) -> Result<String, String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok("read".to_string())
    }
}

#[tokio::test]
async fn proxy_provider_does_not_block_the_async_runtime() {
    let holder: Arc<RwLock<Arc<dyn Provider>>> = Arc::new(RwLock::new(Arc::new(MockProvider)));
    let proxy = ProxyProvider::new(holder);

    let response = proxy
        .chat(&nuo_protocol::ModelRequest::new(Vec::new()))
        .await
        .unwrap();

    assert!(response.message.content.contains("mock AI"));
}

#[test]
fn context_overflow_detection() {
    let err = nuo_protocol::ProviderError::new(
        "mock",
        nuo_protocol::ProviderErrorKind::ContextOverflow,
        "context exceeded",
    );
    assert_eq!(err.kind(), nuo_protocol::ProviderErrorKind::ContextOverflow);
}

/// Stands in for the transport: publishes known timings into whatever handle
/// the request carries, then reports failure on the first attempt and success on
/// the second — so one *turn* produces two attempts that must not share
/// telemetry.
struct TelemetryPublishingProvider(AtomicUsize);

#[async_trait]
impl Provider for TelemetryPublishingProvider {
    async fn chat(
        &self,
        _request: &nuo_protocol::ModelRequest,
    ) -> Result<nuo_protocol::ProviderCompletion, nuo_protocol::ProviderError> {
        Err(nuo_protocol::ProviderError::new(
            "mock",
            nuo_protocol::ProviderErrorKind::Other,
            "non-streaming path should not be used",
        ))
    }

    async fn stream_chat(
        &self,
        _request: &nuo_protocol::ModelRequest,
        _telemetry: nuo_protocol::TransportTelemetry,
    ) -> Result<
        futures::stream::BoxStream<'static, Result<String, nuo_protocol::ProviderError>>,
        nuo_protocol::ProviderError,
    > {
        Ok(Box::pin(stream::empty()))
    }

    async fn stream_chat_events(
        &self,
        _request: &nuo_protocol::ModelRequest,
        _telemetry: nuo_protocol::TransportTelemetry,
    ) -> Result<
        futures::stream::BoxStream<
            'static,
            Result<ProviderStreamEvent, nuo_protocol::ProviderError>,
        >,
        nuo_protocol::ProviderError,
    > {
        // What the owned transport does: fill the handle it was handed, without
        // ever asking whose attempt it was serving.
        let cold = self.0.fetch_add(1, Ordering::SeqCst) == 0;
        _telemetry.publish(nuo_protocol::TransportTimings {
            dns_us: cold.then_some(12_000),
            tls_us: cold.then_some(90_000),
            tcp_us: Some(if cold { 40_000 } else { 7_000 }),
            connected_us: Some(45_000),
            request_sent_us: Some(60_000),
            stream_ready_us: Some(120_000),
            rtt_us: Some(42_000),
            retransmits: if cold { 3 } else { 0 },
            observation: if cold {
                nuo_protocol::TransportObservation::ColdConnection
            } else {
                nuo_protocol::TransportObservation::PooledConnection
            },
            dispatch_at: Some(std::time::Instant::now()),
        });

        if cold {
            Ok(Box::pin(stream::iter(vec![
                Ok(ProviderStreamEvent::TextDelta("partial".to_string())),
                Err(nuo_protocol::ProviderError::new(
                    "mock",
                    nuo_protocol::ProviderErrorKind::RateLimited,
                    "rate limited",
                )
                .retryable(Some(1))),
            ])))
        } else {
            Ok(Box::pin(stream::iter(vec![
                Ok(ProviderStreamEvent::TextDelta("done".to_string())),
                Ok(ProviderStreamEvent::Completed(
                    nuo_protocol::ProviderCompletionMeta::default(),
                )),
            ])))
        }
    }
}

/// The transport's timings reach the attempt that caused them.
///
/// One turn, two attempts — the first fails transiently and is retried, and a
/// retry reuses the turn's assembled request. Each attempt must therefore carry
/// its *own* telemetry: the retry's `PooledConnection` may not overwrite the
/// first attempt's `ColdConnection`, and the first attempt's settled record may
/// not pick up the retry's numbers. That holds only because the handle is created
/// per attempt rather than inherited from the shared request.
#[tokio::test]
async fn each_attempt_carries_its_own_transport_telemetry_across_a_retry() {
    let directory =
        std::env::temp_dir().join(format!("nuo-telemetry-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&directory).expect("create test directory");
    let session = Arc::new(SessionStore::for_path(directory.join("session.json")));
    let agent = Arc::new(Agent::new(
        Arc::new(TelemetryPublishingProvider(AtomicUsize::new(0))),
        Vec::new(),
        nuo_harness::AgentIdentity::default(),
    ));
    let ledger = nuo_protocol::TokenSourceLedger::shared();
    agent.install_token_ledger(ledger.clone());
    let (tx, mut rx) = mpsc::unbounded_channel();
    let session_id = session.id().await;

    execute_round(
        RoundContext {
            agent,
            tx,
            token: CancellationToken::new(),
            session_id: session_id.clone(),
            session: session.clone(),
            projection: ContextProjectionSettings {
                request_overhead_tokens: 0,
                budget: nuo_protocol::CompactionPolicy::default().resolve(100_000),
                preserve_rounds: 6,
                summarize: false,
                prune: false,
                prune_protect_tokens: 0,
            },
            retry_max_attempts: 3,
            stream_restart_max_attempts: 2,

            retry_base_ms: 1,
            retry_max_ms: 10,
            emit_round_completed: false,
        },
        RoundInput {
            prompt: "hello".to_string(),
            hidden: false,
            display_prompt: None,
            sent_at_ms: None,
            images: Vec::new(),
            driver: nuo_harness::orchestration::RoundDriver::Fresh,
        },
    )
    .await
    .unwrap();
    while rx.try_recv().is_ok() {}

    let attempts = ledger.records_for_session(&session_id);
    assert_eq!(attempts.len(), 2, "one turn, two attempts: {attempts:#?}");

    let first = attempts[0]
        .performance
        .expect("the first attempt must keep its telemetry");
    assert_eq!(
        first.observation,
        nuo_protocol::TransportObservation::ColdConnection,
        "the retry must not overwrite the first attempt's regime"
    );
    assert_eq!(first.tcp_us, Some(40_000));
    assert_eq!(first.retransmits, 3);
    assert!(first.tcp_info_sampled());
    // The offsets arrive anchored on the transport's dispatch and are re-anchored
    // onto this attempt's clock, so the ladder can only have advanced.
    let connected = first.connected_us.expect("connect instant");
    let sent = first.request_sent_us.expect("upload anchor");
    let ready = first.stream_ready_us.expect("head anchor");
    assert!(
        connected <= sent && sent <= ready,
        "the attempt's ladder must advance: {connected} → {sent} → {ready}"
    );

    let second = attempts[1]
        .performance
        .expect("the retry must keep its own telemetry");
    assert_eq!(
        second.observation,
        nuo_protocol::TransportObservation::PooledConnection
    );
    assert_eq!(second.tcp_us, Some(7_000), "the retry's own TCP phase");
    assert_eq!(second.retransmits, 0, "the retry's own socket sample");
    assert_eq!(second.dns_us, None);

    let _ = std::fs::remove_dir_all(directory);
}

#[tokio::test]
async fn turn_retries_transient_provider_failure_before_tool_activity() {
    let directory = std::env::temp_dir().join(format!("nuo-retry-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&directory).expect("create test directory");
    let session = Arc::new(SessionStore::for_path(directory.join("session.json")));
    let agent = Arc::new(Agent::new(
        Arc::new(RetryOnceProvider(AtomicUsize::new(0))),
        Vec::new(),
        nuo_harness::AgentIdentity::default(),
    ));
    let ledger = nuo_protocol::TokenSourceLedger::shared();
    agent.install_token_ledger(ledger.clone());
    let (tx, mut rx) = mpsc::unbounded_channel();
    let session_id = session.id().await;

    execute_round(
        RoundContext {
            agent,
            tx,
            token: CancellationToken::new(),
            session_id: session_id.clone(),
            session: session.clone(),
            projection: ContextProjectionSettings {
                request_overhead_tokens: 0,
                budget: nuo_protocol::CompactionPolicy::default().resolve(100_000),
                preserve_rounds: 6,
                summarize: false,
                prune: false,
                prune_protect_tokens: 0,
            },
            retry_max_attempts: 3,
            stream_restart_max_attempts: 2,

            retry_base_ms: 1,
            retry_max_ms: 10,
            emit_round_completed: false,
        },
        RoundInput {
            prompt: "work".to_string(),
            hidden: false,
            display_prompt: None,
            sent_at_ms: None,
            images: Vec::new(),
            driver: nuo_harness::orchestration::RoundDriver::Fresh,
        },
    )
    .await
    .unwrap();
    assert!(
        session
            .model_window()
            .await
            .iter()
            .any(|message| message.content == "done")
    );
    let responses = std::iter::from_fn(|| rx.try_recv().ok()).collect::<Vec<_>>();
    let activities = responses
        .iter()
        .filter_map(|response| match response {
            AgentResponse::Round { scope, .. } => match scope.event() {
                RoundEvent::Activity(status) => Some(status.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(
        activities
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .starts_with(&["saving request", "preparing context"])
    );
    assert_eq!(
        activities
            .iter()
            .filter(|status| **status == "waiting for model")
            .count(),
        2
    );
    assert_eq!(
        activities.last().map(String::as_str),
        Some("saving response")
    );
    assert!(responses.iter().any(|response| matches!(
        response,
        AgentResponse::Round { scope, .. }
            if matches!(
                scope.event(),
                RoundEvent::RetryScheduled {
                    attempt: 2,
                    max_attempts: 3,
                    ..
                }
            )
    )));
    assert!(responses.iter().any(|response| matches!(
        response,
        AgentResponse::Round { scope, .. }
            if matches!(scope.event(), RoundEvent::StreamDiscard)
    )));
    let attempts = ledger.records_for_session(&session_id);
    assert_eq!(attempts.len(), 2);
    assert_eq!(attempts[0].key.attempt, 1);
    assert_eq!(attempts[0].status, nuo_protocol::RequestUsageStatus::Failed);
    assert_eq!(attempts[1].key.attempt, 2);
    assert_eq!(
        attempts[1].status,
        nuo_protocol::RequestUsageStatus::Completed
    );
    assert_eq!(session.request_usage_records().await, attempts);
    let _ = std::fs::remove_dir_all(directory);
}
