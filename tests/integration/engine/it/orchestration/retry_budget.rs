//! Retry budgets: exhaustion, mid-stream restarts, delay math, and resume.

use super::*;

use super::retry::AlwaysRetryableProvider;

use super::common::provider_history_snapshot;
#[tokio::test]
async fn turn_exhaustion_message_explains_retry_budget() {
    let directory =
        std::env::temp_dir().join(format!("nuo-retry-exhaust-{}", uuid::Uuid::new_v4()));
    let _ = std::fs::create_dir_all(&directory);
    let session = Arc::new(ThreadSession::for_path(directory.join("session.json")));
    let agent = Arc::new(Agent::new(
        Arc::new(AlwaysRetryableProvider),
        Vec::new(),
        nuo_harness::AgentIdentity::default(),
    ));
    let (tx, mut rx) = mpsc::unbounded_channel();

    let error = execute_round(
        RoundContext {
            agent,
            tx,
            token: CancellationToken::new(),
            session_id: session.id().await,
            session,
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
    .unwrap_err();

    let error_string = error.to_string();
    assert!(
        error_string.starts_with("OpenAI HTTP 429 Too Many Requests"),
        "should surface the provider message: {error_string}"
    );
    // All attempts but the last must announce a retry; the final failure
    // surfaces as the error above instead.
    let scheduled = std::iter::from_fn(|| rx.try_recv().ok())
        .filter(|response| {
            matches!(
                response,
                AgentResponse::Round { scope, .. }
                    if matches!(scope.event(), RoundEvent::RetryScheduled { .. })
            )
        })
        .count();
    assert_eq!(
        scheduled, 2,
        "should schedule retries for every attempt before giving up"
    );
    let _ = std::fs::remove_dir_all(directory);
}

/// A failure after visible output is a **stream restart**, not a silent
/// attempt: it consumes the small restart budget, never the 30-attempt
/// connection budget, and every failed attempt's streamed text is discarded.
#[tokio::test]
async fn mid_stream_failures_consume_only_the_restart_budget() {
    let directory =
        std::env::temp_dir().join(format!("nuo-restart-exhaust-{}", uuid::Uuid::new_v4()));
    let _ = std::fs::create_dir_all(&directory);
    let session = Arc::new(ThreadSession::for_path(directory.join("session.json")));
    let attempts = Arc::new(AtomicUsize::new(0));
    let agent = Arc::new(Agent::new(
        Arc::new(VisibleFailProvider(attempts.clone())),
        Vec::new(),
        nuo_harness::AgentIdentity::default(),
    ));
    let (tx, mut rx) = mpsc::unbounded_channel();

    let error = execute_round(
        RoundContext {
            agent,
            tx,
            token: CancellationToken::new(),
            session_id: session.id().await,
            session,
            projection: ContextProjectionSettings {
                request_overhead_tokens: 0,
                budget: nuo_protocol::CompactionPolicy::default().resolve(100_000),
                preserve_rounds: 6,
                summarize: false,
                prune: false,
                prune_protect_tokens: 0,
            },
            retry_max_attempts: 30,
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
    .unwrap_err();

    assert_eq!(
        attempts.load(Ordering::SeqCst),
        3,
        "the 30-attempt connection budget must not apply to visible failures"
    );
    let error_string = error.to_string();
    assert!(
        error_string.contains("gave up after 2 restart(s)"),
        "the exhaustion message must name the restart budget: {error_string}"
    );

    let responses = std::iter::from_fn(|| rx.try_recv().ok()).collect::<Vec<_>>();
    let scheduled = responses
        .iter()
        .filter_map(|response| match response {
            AgentResponse::Round { scope, .. } => match scope.event() {
                RoundEvent::RetryScheduled {
                    attempt,
                    max_attempts,
                    ..
                } => Some((*attempt, *max_attempts)),
                _ => None,
            },
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(scheduled, vec![(2, 3), (3, 3)]);
    let discards = responses
        .iter()
        .filter(|response| {
            matches!(
                response,
                AgentResponse::Round { scope, .. }
                    if matches!(scope.event(), RoundEvent::StreamDiscard)
            )
        })
        .count();
    assert_eq!(
        discards, 3,
        "every failed attempt's partial text is discarded, including the last"
    );
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn retry_delay_honors_headers_and_stepped_bounds() {
    // Stepped backoff: 1s -> 2s -> 5s -> 10s -> 10s -> continuously 10s
    assert_eq!(retry_delay_ms(1, None, 1_000, 30_000), 1_000);
    assert_eq!(retry_delay_ms(2, None, 1_000, 30_000), 2_000);
    assert_eq!(retry_delay_ms(3, None, 1_000, 30_000), 5_000);
    assert_eq!(retry_delay_ms(4, None, 1_000, 30_000), 10_000);
    assert_eq!(retry_delay_ms(5, None, 1_000, 30_000), 10_000);
    assert_eq!(retry_delay_ms(10, None, 1_000, 30_000), 10_000);
    // Server retry-after header takes priority (bounded by base and max)
    assert_eq!(retry_delay_ms(2, Some(45_000), 1_000, 30_000), 30_000);
    assert_eq!(retry_delay_ms(1, Some(0), 1_000, 30_000), 1_000);
    // Capped by max_ms
    assert_eq!(retry_delay_ms(3, None, 1_000, 3_000), 3_000);
}

#[test]
fn apply_jitter_stays_within_half_to_full_range() {
    // Equal jitter: result ∈ [base/2, base]. A roll of 0 yields the floor,
    // a roll of the full span yields the ceiling — both bounds are closed.
    assert_eq!(apply_jitter_ms(1_000, |_| 0), 500);
    assert_eq!(apply_jitter_ms(1_000, |span| span), 1_000);
    // A mid-range roll lands exactly halfway between floor and ceiling.
    assert_eq!(apply_jitter_ms(1_000, |span| span / 2), 750);
    // Odd base still floors cleanly: [1500/2 .. 1500].
    assert_eq!(apply_jitter_ms(1_500, |_| 0), 750);
    assert_eq!(apply_jitter_ms(1_500, |span| span), 1_500);
}

#[test]
fn apply_jitter_never_exceeds_base() {
    // A pathological roll larger than the span must be clamped back to the
    // ceiling, so jitter can never push a delay past the configured cap.
    assert_eq!(apply_jitter_ms(1_000, |_| u64::MAX), 1_000);
}

#[test]
fn apply_jitter_passes_zero_through_unchanged() {
    assert_eq!(apply_jitter_ms(0, |_| 1_000), 0);
}

/// A provider that fails *terminally* (a non-retryable error) on the first
/// round and succeeds on the second — the exact shape a `/retry` resumes
/// from. `requests` records the message-history shape of every provider
/// request so the test can assert the resume re-sent exactly the committed
/// checkpoint (no duplicate user message, no half-streamed assistant text).
struct FailThenSucceedProvider {
    attempts: AtomicUsize,
    requests: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl Provider for FailThenSucceedProvider {
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
        if self.attempts.fetch_add(1, Ordering::SeqCst) == 0 {
            // Terminal: `parse_retryable_error` finds no envelope, so the
            // harness surfaces it and (with) arms the resume point.
            Ok(Box::pin(stream::iter(vec![Err(
                nuo_protocol::ProviderError::new(
                    "mock",
                    nuo_protocol::ProviderErrorKind::Other,
                    "terminal: model refused the request",
                ),
            )])))
        } else {
            Ok(Box::pin(stream::iter(vec![
                Ok(ProviderStreamEvent::TextDelta("recovered".to_string())),
                Ok(ProviderStreamEvent::Completed(
                    nuo_protocol::ProviderCompletionMeta::default(),
                )),
            ])))
        }
    }
}

/// The round-level `/retry` contract: a round whose provider
/// fails terminally parks a durable resume point, and resuming through it
/// *continues the same round* — the round counter never advances, no second
/// user message is appended, and the turn sequence numbers onward instead of
/// restarting. Meanwhile a round that completed naturally leaves no point,
/// so a second `/retry` has nothing to resume.
#[tokio::test]
async fn retry_resumes_stopped_round_without_breaking_turn_sequence() {
    let directory = std::env::temp_dir().join(format!("nuo-retry-resume-{}", uuid::Uuid::new_v4()));
    let _ = std::fs::create_dir_all(&directory);
    let session = Arc::new(ThreadSession::for_path(directory.join("session.json")));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let agent = Arc::new(Agent::new(
        Arc::new(FailThenSucceedProvider {
            attempts: AtomicUsize::new(0),
            requests: requests.clone(),
        }),
        Vec::new(),
        nuo_harness::AgentIdentity::default(),
    ));
    let (tx, _rx) = mpsc::unbounded_channel();
    let session_id = session.id().await;
    let context = |tx| RoundContext {
        agent: Arc::clone(&agent),
        tx,
        token: CancellationToken::new(),
        session_id: session_id.clone(),
        session: Arc::clone(&session),
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
    };

    // 1. The fresh round fails terminally.
    let error = execute_round(
        context(tx.clone()),
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
    .unwrap_err();
    assert!(error.to_string().contains("terminal"), "{error}");

    // The failed round parked its resume point: same round, committed
    // history watermark, zero committed turns (nothing streamed through).
    let point = session
        .retry_pending()
        .await
        .expect("failed round must arm a retry point");
    assert_eq!(point.round, 1, "the parked point names round 1");
    assert_eq!(point.turns_committed, 0);
    assert_eq!(point.history_watermark, 1, "user message only");
    assert_eq!(session.round_counter().await, 1);

    // 2. `/retry` resumes and completes the *same* round.
    let round_before = session.round_counter().await;
    execute_round(
        context(tx),
        RoundInput::resume(nuo_protocol::RetryPoint {
            round: point.round,
            turns_committed: point.turns_committed,
            history_watermark: point.history_watermark,
            paused_ms: point.paused_ms,
            at_ms: point.at_ms,
        }),
    )
    .await
    .unwrap();

    // Round counter never advanced: the resume completed round 1, it did
    // not mint round 2. This is the headline invariant of /retry.
    assert_eq!(
        session.round_counter().await,
        round_before,
        "retry must not advance the round counter"
    );
    // The window holds user + assistant — exactly one user message (no
    // duplicate from the resume re-sending it as a fresh prompt).
    let window = session.model_window().await;
    let user_count = window
        .iter()
        .filter(|message| message.role == Role::User)
        .count();
    assert_eq!(
        user_count, 1,
        "resume must not append a second user message"
    );
    assert!(
        window.iter().any(|message| message.content == "recovered"),
        "the resumed round's answer is committed"
    );
    // Both provider requests carried the same semantic history. The system
    // prompt is projected afresh for each request and its local diagnostic
    // timestamp may advance; timestamp is not provider protocol content and
    // is removed by the request recorder above.
    let logged = requests.lock().expect("request log lock poisoned").clone();
    assert_eq!(logged.len(), 2);
    assert_eq!(
        logged[0], logged[1],
        "resume re-sends the committed checkpoint, not a mutated history"
    );

    // 3. Completion retired the point: a subsequent `/retry` has nothing
    //    to resume (the completed-round rule).
    assert!(
        session.retry_pending().await.is_none(),
        "a naturally completed round leaves no retry point"
    );

    let _ = std::fs::remove_dir_all(directory);
}

// Round-interrupt projection: only a genuinely stopped round may leave a
// durable `RoundInterrupt` record. Two regressions are pinned here:
//
// 1. Stop sites park their reason unconditionally — even while idle — so a
//    reason parked with no live round must not leak into the next round and
//    label a naturally completed round as "interrupted | <reason>".
// 2. An Esc Esc landing after the round passed its last cancellation
//    checkpoint (the model already converged, the history already committed)
//    parks a reason without changing the outcome. The completed round must
//    not be re-labelled as an interrupt.
