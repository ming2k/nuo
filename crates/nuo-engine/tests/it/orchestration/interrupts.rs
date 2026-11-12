//! Interactive round interrupt/supersede recording.

use super::*;

/// A provider whose stream never terminates until the test cancels the
/// round token — the "model is still generating" state an Esc Esc lands in.
struct HangingProvider;

#[async_trait]
impl Provider for HangingProvider {
    async fn chat(
        &self,
        _request: &nuo_protocol::ModelRequest,
    ) -> Result<nuo_protocol::ProviderCompletion, nuo_protocol::ProviderError> {
        Err(nuo_protocol::ProviderError::new(
            "mock",
            nuo_protocol::ProviderErrorKind::Other,
            "chat is not used by the streaming path",
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
        Ok(Box::pin(futures::stream::pending()))
    }
}

/// A provider that answers immediately — the "model converged" state.
pub(super) struct InstantProvider;

#[async_trait]
impl Provider for InstantProvider {
    async fn chat(
        &self,
        _request: &nuo_protocol::ModelRequest,
    ) -> Result<nuo_protocol::ProviderCompletion, nuo_protocol::ProviderError> {
        Ok(nuo_protocol::ProviderCompletion::message(Message::new(
            Role::Assistant,
            "done",
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
        Ok(Box::pin(stream::iter([Ok("done".to_string())])))
    }
}

/// A provider that streams one text delta, then *finishes on a delay* —
/// closing the stream (the terminal event a real provider sends after its
/// last delta + usage chunk) only after `settle_ms`. This is the exact shape
/// of the end-of-answer race: the answer's content has all arrived, but the
/// stream's `Ok(None)` terminator is still in flight when the user's next
/// message (or Esc Esc) lands. Whether the round completes or unwinds as
/// `Interrupted` is decided by the finish-drain window, not by which signal
/// happened to poll first.
struct SettlingProvider {
    settle_ms: u64,
}

#[async_trait]
impl Provider for SettlingProvider {
    async fn chat(
        &self,
        _request: &nuo_protocol::ModelRequest,
    ) -> Result<nuo_protocol::ProviderCompletion, nuo_protocol::ProviderError> {
        Err(nuo_protocol::ProviderError::new(
            "mock",
            nuo_protocol::ProviderErrorKind::Other,
            "chat is not used by the streaming path",
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
        let settle_ms = self.settle_ms;
        Ok(Box::pin(stream::unfold(0u8, move |state| async move {
            match state {
                0 => Some((Ok("done".to_string()), 1)),
                // Terminal: close the stream shortly after the delta.
                1 => {
                    tokio::time::sleep(std::time::Duration::from_millis(settle_ms)).await;
                    None
                }
                _ => None,
            }
        })))
    }
}

/// A provider that streams one delta and then goes **silent forever** — an
/// answer that was *not* settling when the cancel landed. The finish-drain
/// window must expire and honour the interrupt.
struct TrickleThenSilentProvider;

#[async_trait]
impl Provider for TrickleThenSilentProvider {
    async fn chat(
        &self,
        _request: &nuo_protocol::ModelRequest,
    ) -> Result<nuo_protocol::ProviderCompletion, nuo_protocol::ProviderError> {
        Err(nuo_protocol::ProviderError::new(
            "mock",
            nuo_protocol::ProviderErrorKind::Other,
            "chat is not used by the streaming path",
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
        Ok(Box::pin(
            stream::once(async { Ok("partial answer".to_string()) }).chain(stream::pending()),
        ))
    }
}

/// A provider whose single stream item is gated: it signals `started` when
/// the round reaches the model request, then holds the stream open until
/// `release` fires, and finally converges ("done", no tool calls). This lets
/// a test park an interrupt reason at a chosen instant *while the round is
/// live* and then let the round complete anyway — the exact shape of an Esc
/// Esc that lands too late to change the outcome.
struct GatedProvider {
    started: tokio::sync::mpsc::UnboundedSender<()>,
    release: Arc<tokio::sync::Notify>,
}

#[async_trait]
impl Provider for GatedProvider {
    async fn chat(
        &self,
        _request: &nuo_protocol::ModelRequest,
    ) -> Result<nuo_protocol::ProviderCompletion, nuo_protocol::ProviderError> {
        Err(nuo_protocol::ProviderError::new(
            "mock",
            nuo_protocol::ProviderErrorKind::Other,
            "chat is not used by the streaming path",
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
        let started = self.started.clone();
        let release = Arc::clone(&self.release);
        Ok(Box::pin(stream::once(async move {
            let _ = started.send(());
            release.notified().await;
            Ok("done".to_string())
        })))
    }
}

/// Shared scaffolding: build a session + agent + channel for one interactive
/// round through `start_interactive_round` (the production entry whose tail
/// owns the interrupt-record decision). The caller parks/releases via the
/// returned lifecycle and channel.
struct InteractiveRoundFixture {
    session: Arc<SessionStore>,
    rx: mpsc::UnboundedReceiver<AgentResponse>,
    lifecycle: Arc<RoundLifecycle>,
    agent: Arc<Agent>,
    directory: std::path::PathBuf,
}

async fn interactive_round_fixture(provider: Arc<dyn Provider>) -> InteractiveRoundFixture {
    let directory = std::env::temp_dir().join(format!("nuo-intr-{}", uuid::Uuid::new_v4()));
    let _ = std::fs::create_dir_all(&directory);
    let session = Arc::new(SessionStore::for_path(directory.join("session.json")));
    let agent = Arc::new(Agent::new(
        provider,
        Vec::new(),
        nuo_harness::AgentIdentity::default(),
    ));
    let lifecycle = Arc::new(RoundLifecycle::new());
    let (tx, rx) = mpsc::unbounded_channel();
    let session_id = session.id().await;
    start_interactive_round(
        InteractiveRoundContext {
            agent: Arc::clone(&agent),
            tx,
            lifecycle: Arc::clone(&lifecycle),
            session: Arc::clone(&session),
            session_id,
            projection: ContextProjectionSettings {
                request_overhead_tokens: 0,
                budget: nuo_protocol::CompactionPolicy::default().resolve(100_000),
                preserve_rounds: 6,
                summarize: false,
                prune: false,
                prune_protect_tokens: 0,
            },
            retry_max_attempts: 1,
            stream_restart_max_attempts: 0,

            retry_base_ms: 1,
            retry_max_ms: 1,
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
    .await;
    InteractiveRoundFixture {
        session,
        rx,
        lifecycle,
        agent,
        directory,
    }
}

/// Drain the channel until a specific event kind appears (bounded wait so a
/// broken round fails the test instead of hanging it).
async fn next_event_where(
    rx: &mut mpsc::UnboundedReceiver<AgentResponse>,
    mut predicate: impl FnMut(&RoundEvent) -> bool,
) -> Option<RoundEvent> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while std::time::Instant::now() < deadline {
        match tokio::time::timeout(std::time::Duration::from_millis(500), rx.recv()).await {
            Ok(Some(AgentResponse::Round { scope, .. })) if predicate(scope.event()) => {
                return Some(scope.into_event());
            }
            Ok(Some(_)) => continue,
            Ok(None) => return None,
            Err(_elapsed) => continue,
        }
    }
    None
}

#[tokio::test]
async fn idle_parked_interrupt_reason_does_not_label_the_next_round() {
    // Regression 1: Esc Esc / a session switch parks a reason while idle
    // (no live round). The next round completes naturally and must leave NO
    // interrupt record and NO RoundInterrupted event.
    let InteractiveRoundFixture {
        session,
        mut rx,
        lifecycle,
        agent,
        directory,
    } = interactive_round_fixture(Arc::new(InstantProvider)).await;

    // Park as the interrupt handler does while idle, then wait for the
    // round's natural completion.
    lifecycle.record_interrupt(nuo_protocol::RoundInterruptReason::User);
    let completed = next_event_where(&mut rx, |event| {
        matches!(event, RoundEvent::RoundCompleted(_))
    })
    .await;
    assert!(completed.is_some(), "round must complete");

    // Give the tail (which runs after RoundCompleted) a moment, then verify
    // no interrupt was recorded or emitted.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(
        session.round_interrupts().await.is_empty(),
        "a naturally completed round must not leave an interrupt record"
    );
    let leaked = std::iter::from_fn(|| rx.try_recv().ok()).any(|response| {
        matches!(
            response,
            AgentResponse::Round { scope, .. }
                if matches!(scope.event(), RoundEvent::RoundInterrupted(_))
        )
    });
    assert!(
        !leaked,
        "no RoundInterrupted event may follow a natural completion"
    );
    assert_eq!(
        lifecycle.take_interrupt(),
        None,
        "the tail consumed the stale park; nothing may leak further"
    );
    assert_eq!(agent.round_count(), 1);
    let _ = std::fs::remove_dir_all(directory);
}

#[tokio::test]
async fn late_interrupt_while_live_then_completes_is_not_recorded() {
    // Regression 2 (deterministic): the round is live and streaming when
    // the Esc Esc parks its reason, but the cancellation arrives after the
    // last checkpoint — the token is never observed before the model
    // converges, so the round completes. The tail must not write a record
    // for a round that succeeded.
    let release = Arc::new(tokio::sync::Notify::new());
    let (started_tx, mut started_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let provider = Arc::new(GatedProvider {
        started: started_tx,
        release: Arc::clone(&release),
    });
    let InteractiveRoundFixture {
        session,
        mut rx,
        lifecycle,
        agent,
        directory,
    } = interactive_round_fixture(provider).await;

    // Wait until the model request is actually in flight.
    started_rx
        .recv()
        .await
        .expect("round must reach the model request");

    // Esc Esc lands now — the reason is parked while the round is live...
    lifecycle.record_interrupt(nuo_protocol::RoundInterruptReason::User);
    // ...but this Esc Esc is modeled as arriving too late: no token
    // cancellation is observed before convergence (we simply release the
    // stream). This is the "server-side LLM converged and the round
    // finished normally" completion.
    release.notify_one();

    let completed = next_event_where(&mut rx, |event| {
        matches!(event, RoundEvent::RoundCompleted(_))
    })
    .await;
    assert!(completed.is_some(), "round must still complete");
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(
        session.round_interrupts().await.is_empty(),
        "a round that completed despite a late parked reason must not gain an interrupt record"
    );
    assert_eq!(agent.round_count(), 1);
    let _ = std::fs::remove_dir_all(directory);
}

#[tokio::test]
async fn real_interrupt_of_a_live_round_still_records() {
    // Control: a genuine mid-generation Esc Esc must keep its record — the
    // fix must not swallow real interrupts.
    let InteractiveRoundFixture {
        session,
        mut rx,
        lifecycle,
        directory,
        ..
    } = interactive_round_fixture(Arc::new(HangingProvider)).await;

    // Wait until the round is actually streaming (it admitted the prompt),
    // then cancel exactly like the interrupt handler.
    let started = next_event_where(&mut rx, |event| matches!(event, RoundEvent::TurnStarted)).await;
    assert!(started.is_some(), "round must start");
    lifecycle.record_interrupt(nuo_protocol::RoundInterruptReason::User);
    lifecycle.cancel_current().await;

    let interrupted = next_event_where(&mut rx, |event| {
        matches!(event, RoundEvent::RoundInterrupted(_))
    })
    .await;
    assert!(
        interrupted.is_some(),
        "a genuinely interrupted round emits its RoundInterrupted event"
    );
    let records = session.round_interrupts().await;
    assert_eq!(records.len(), 1, "exactly one durable record: {records:?}");
    assert_eq!(records[0].reason, nuo_protocol::RoundInterruptReason::User);
    // The HangingProvider round was interrupted before model output.
    // The prompt is preserved in session history (never retracted) and records
    // its admitted round number (round 1 for the first round).
    assert_eq!(records[0].round, Some(1));
    let messages = session.model_window().await;
    assert!(
        messages.iter().any(|m| m.role == nuo_protocol::Role::User),
        "sent prompt is preserved in transcript"
    );
    let _ = std::fs::remove_dir_all(directory);
}

#[tokio::test]
async fn superseded_live_round_still_records() {
    // Control: a supersede (new message replacing a live round) still
    // records its reason via the generation-suppressed arm.
    let InteractiveRoundFixture {
        session,
        mut rx,
        lifecycle,
        directory,
        ..
    } = interactive_round_fixture(Arc::new(HangingProvider)).await;

    let started = next_event_where(&mut rx, |event| matches!(event, RoundEvent::TurnStarted)).await;
    assert!(started.is_some(), "round must start");

    // Mirror the replacement path: park, bump the generation, cancel.
    lifecycle.record_interrupt(nuo_protocol::RoundInterruptReason::Superseded);
    lifecycle.supersede();
    lifecycle.cancel_current().await;

    let interrupted = next_event_where(&mut rx, |event| {
        matches!(event, RoundEvent::RoundInterrupted(_))
    })
    .await;
    assert!(
        interrupted.is_some(),
        "a superseded round still records why it died"
    );
    let records = session.round_interrupts().await;
    assert_eq!(records.len(), 1, "exactly one durable record: {records:?}");
    assert_eq!(
        records[0].reason,
        nuo_protocol::RoundInterruptReason::Superseded
    );
    let _ = std::fs::remove_dir_all(directory);
}

// The end-of-answer supersede race (the false "▲ interrupted | new message"
// over a round that finished): the model's final delta has arrived and only
// the stream terminator is in flight when the next message lands.

#[tokio::test]
async fn late_supersede_on_a_settling_stream_completes_the_round() {
    // The answer's content fully arrived; the stream closes 100 ms later. A
    // new message cancelling the round inside that window must NOT unwind it
    // as interrupted — the user watched the answer finish, so the round
    // commits and no marker is recorded.
    let InteractiveRoundFixture {
        session,
        mut rx,
        lifecycle,
        agent,
        directory,
    } = interactive_round_fixture(Arc::new(SettlingProvider { settle_ms: 100 })).await;

    let delta =
        next_event_where(&mut rx, |event| matches!(event, RoundEvent::StreamDelta(_))).await;
    assert!(delta.is_some(), "round must stream its answer delta");

    // The user sends the next message: park + cancel exactly as
    // `start_interactive_round`'s replacement arm does.
    lifecycle.record_interrupt(nuo_protocol::RoundInterruptReason::Superseded);
    lifecycle.cancel_current().await;

    let outcome = next_event_where(&mut rx, |event| {
        matches!(
            event,
            RoundEvent::RoundCompleted(_) | RoundEvent::RoundInterrupted(_)
        )
    })
    .await;
    match outcome {
        Some(RoundEvent::RoundCompleted(_)) => {}
        other => panic!("a fully streamed answer must complete, got {other:?}"),
    }
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert!(
        session.round_interrupts().await.is_empty(),
        "no interrupt record for a round that committed its answer"
    );
    assert_eq!(agent.round_count(), 1);
    let _ = std::fs::remove_dir_all(directory);
}

#[tokio::test]
async fn supersede_on_a_silent_stream_still_interrupts() {
    // Control: a delta arrived but the stream then went silent — the answer
    // was NOT settling when the cancel landed. The drain window expires and
    // the interrupt stands (a genuinely unfinished answer is interrupted
    // work). Parked at the stop moment, the record must still be written.
    let InteractiveRoundFixture {
        session,
        mut rx,
        lifecycle,
        directory,
        ..
    } = interactive_round_fixture(Arc::new(TrickleThenSilentProvider)).await;

    let delta =
        next_event_where(&mut rx, |event| matches!(event, RoundEvent::StreamDelta(_))).await;
    assert!(delta.is_some(), "round must stream its first delta");

    lifecycle.record_interrupt(nuo_protocol::RoundInterruptReason::Superseded);
    lifecycle.cancel_current().await;

    let interrupted = next_event_where(&mut rx, |event| {
        matches!(event, RoundEvent::RoundInterrupted(_))
    })
    .await;
    assert!(
        interrupted.is_some(),
        "a stream that never settles is genuinely interrupted"
    );
    let records = session.round_interrupts().await;
    assert_eq!(records.len(), 1, "exactly one durable record: {records:?}");
    assert_eq!(
        records[0].reason,
        nuo_protocol::RoundInterruptReason::Superseded
    );
    assert_eq!(
        records[0].detail.as_deref(),
        None,
        "clean stop carries no error detail and does not bloat records with abandoned drafts"
    );
    let _ = std::fs::remove_dir_all(directory);
}

#[tokio::test]
async fn supersede_record_is_stamped_at_the_message_send_time() {
    // Seam regression: the marker's `at_ms` must not postdate the superseding
    // message's `sent_at_ms`, or the resume merge drops the marker below the
    // newer round's answer — reading as an interrupt of a round that
    // completed normally. The park API is exercised directly here (the full
    // replacement flow is runtime-level); what it must guarantee is that the
    // explicit send time wins over the park-moment clock.
    let lifecycle = RoundLifecycle::new();
    lifecycle.begin().await;
    lifecycle.record_interrupt_at(nuo_protocol::RoundInterruptReason::Superseded, Some(42_000));
    let parked = lifecycle
        .take_interrupt()
        .expect("park survives until taken");
    assert_eq!(parked.at_ms, 42_000);
    assert!(
        parked.at_ms < unix_epoch_ms_for_test(),
        "an explicit send time must win over the wall clock"
    );
}

/// Wall clock for [`supersede_record_is_stamped_at_the_message_send_time`]:
/// guaranteed past the fixed send time the test parks.
fn unix_epoch_ms_for_test() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(u64::MAX)
}
