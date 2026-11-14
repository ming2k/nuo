//! Truncated streams: discard the attempt, then retry without closing reasoning.

use super::*;

/// Emits a reasoning delta, then a text delta, then dies mid-stream — the shape
/// of a real SSE truncation. Used to pin the ordering contract that the shell's
/// transcript sweep depends on.
struct ReasoningThenTruncatedProvider(Arc<AtomicUsize>);

#[async_trait]
impl Provider for ReasoningThenTruncatedProvider {
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
        let attempt = self.0.fetch_add(1, Ordering::SeqCst);

        if attempt == 0 {
            // First attempt: the model thinks, starts answering, then the
            // connection dies. This is the exact shape that used to leave the
            // thinking trace stranded in the transcript.
            return Ok(Box::pin(stream::iter(vec![
                Ok(ProviderStreamEvent::ReasoningDelta(
                    "attempt one reasoning".to_string(),
                )),
                Ok(ProviderStreamEvent::TextDelta(
                    "attempt one answer".to_string(),
                )),
                Err(nuo_protocol::ProviderError::new(
                    "mock",
                    nuo_protocol::ProviderErrorKind::Transport,
                    "connection reset mid-stream",
                )
                .retryable(None)),
            ])));
        }

        // Second attempt: a clean, complete answer. `Completed` is what marks
        // the stream as whole — without it the agent treats the stream as
        // truncated and burns another restart.
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderStreamEvent::TextDelta(
                "attempt two answer".to_string(),
            )),
            Ok(ProviderStreamEvent::Completed(
                nuo_protocol::ProviderCompletionMeta::default(),
            )),
        ])))
    }
}

/// `[INV-RETRY-02]` — the ordering contract the shell's `StreamDiscard` sweep
/// depends on.
///
/// When a stream dies mid-flight the orchestration must, in this order:
///
/// 1. discard the attempt's visible output (`StreamDiscard`), then
/// 2. announce the retry (`RetryScheduled`), whose frame also carries the
///    pending-tool-step retraction.
///
/// That order is load-bearing. The shell sweeps the transcript on
/// `StreamDiscard`; if a still-`Ingesting` tool step were allowed to act as a
/// floor at that moment, the answer text beneath it would survive and the retry
/// would append into the failed attempt's row.
///
/// This test also pins a fact that is easy to get wrong: **no `ReasoningEnd` is
/// emitted on the failure path.** The agent returns the transport error directly
/// (rounds.rs, the `Err(error)` arm), so a thinking trace that streamed before
/// the failure is never closed. The shell therefore has to discard it by
/// position — there is no finalize event to lean on.
#[tokio::test]
async fn truncated_stream_discards_before_retrying_and_never_closes_reasoning() {
    let directory = std::env::temp_dir().join(format!("nuo-truncate-{}", uuid::Uuid::new_v4()));
    let _ = std::fs::create_dir_all(&directory);
    let session = Arc::new(ThreadSession::for_path(directory.join("session.json")));
    let attempts = Arc::new(AtomicUsize::new(0));
    let agent = Arc::new(Agent::new(
        Arc::new(ReasoningThenTruncatedProvider(attempts.clone())),
        Vec::new(),
        nuo_engine::AgentIdentity::default(),
    ));
    let (tx, mut rx) = mpsc::unbounded_channel();

    let result = execute_round(
        RoundContext {
            agent,
            tx,
            token: CancellationToken::new(),
            session_id: session.id().await,
            session: session.clone(),
            projection: ContextProjectionSettings {
                request_overhead_tokens: 0,
                budget: nuo_protocol::CompactionPolicy::default().resolve(100_000),
                preserve_rounds: 6,
                summarize: false,
                prune: false,
                prune_protect_tokens: 0,
            },
            retry_max_attempts: 5,
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
            driver: nuo_engine::orchestration::RoundDriver::Fresh,
        },
    )
    .await;

    assert!(
        result.is_ok(),
        "the retry must recover the round: {result:?}"
    );
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        2,
        "the truncation should cost exactly one restart"
    );

    // Collect the turn context and event in arrival order. The context is the
    // authoritative position; no `TurnStarted` replay is involved.
    let events: Vec<(Option<nuo_protocol::TurnContext>, RoundEvent)> =
        std::iter::from_fn(|| rx.try_recv().ok())
            .filter_map(|response| match response {
                AgentResponse::Round { scope, .. } => match scope {
                    nuo_protocol::RoundScope::Turn { context, event } => {
                        Some((Some(context), event))
                    }
                    nuo_protocol::RoundScope::Session { event } => Some((None, event)),
                },
                _ => None,
            })
            .collect();

    let discard_at = events
        .iter()
        .position(|(_, event)| matches!(event, RoundEvent::StreamDiscard))
        .expect("a truncated attempt must discard its visible output");
    let discard_position = events.iter().find_map(|(context, event)| {
        matches!(event, RoundEvent::StreamDiscard)
            .then_some(*context)
            .flatten()
            .map(|context| (context.round, context.turn))
    });
    let turn_positions: Vec<(u64, u64)> = events
        .iter()
        .filter_map(|(context, event)| {
            matches!(event, RoundEvent::TurnStarted)
                .then_some(*context)
                .flatten()
                .map(|context| (context.round, context.turn))
        })
        .collect();
    assert_eq!(
        turn_positions.len(),
        2,
        "the retry must open a second attempt at the same position: {events:?}"
    );
    assert_eq!(
        discard_position,
        turn_positions.first().copied(),
        "the discard must carry the failed attempt's own position: {events:?}"
    );
    assert!(
        turn_positions
            .iter()
            .all(|position| Some(*position) == discard_position),
        "both attempts must use the same (round, turn): {turn_positions:?}"
    );

    let retry_at = events
        .iter()
        .position(|(_, event)| matches!(event, RoundEvent::RetryScheduled { .. }))
        .expect("a truncated attempt must schedule a retry");

    assert!(
        discard_at < retry_at,
        "discard must precede the retry: got StreamDiscard@{discard_at}, \
         RetryScheduled@{retry_at} in {events:?}"
    );

    // The load-bearing negative: the failure path never closes the reasoning
    // block, so the shell cannot rely on a finalize event to clean it up.
    //
    // Guard the guard: if the reasoning delta never reached the client this
    // assertion would pass vacuously.
    assert!(
        events
            .iter()
            .any(|(_, event)| matches!(event, RoundEvent::StreamReasoningDelta(_))),
        "the attempt's thinking must actually have streamed: {events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|(_, event)| matches!(event, RoundEvent::StreamReasoningEnd { .. })),
        "the failure path must not emit a reasoning end: {events:?}"
    );

    let _ = std::fs::remove_dir_all(directory);
}
