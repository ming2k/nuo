use super::*;

/// A stream-loop cutoff is recoverable once per user round. A second cutoff
/// ends generation so a model that ignores the guidance cannot consume the
/// context indefinitely.
const MAX_STREAM_LOOP_RECOVERIES: u8 = 1;
const STREAM_LOOP_TURN_FIELD_CHARS: usize = 24_000;
const STREAM_LOOP_PRECEDING_CONTEXT_CHARS: usize = 12_000;

#[derive(Debug, Clone)]
struct StreamLoopIncident {
    pattern: crate::stream_loop_detector::DegeneratePattern,
    channel: nuo_wire::StreamLoopChannel,
}

/// An in-flight Stream Sentinel review spawned off the hot stream loop.
/// Carrying the candidate's identity lets the verdict be applied — or
/// memo-cleared — whenever it lands: mid-stream at the next event boundary,
/// or after the stream ends naturally.
struct PendingStreamLoopReview {
    candidate_key: String,
    channel: nuo_wire::StreamLoopChannel,
    pattern: crate::stream_loop_detector::DegeneratePattern,
    verdict_rx: tokio::sync::oneshot::Receiver<nuo_wire::StreamLoopVerdict>,
}

/// Memo key identifying an L1 candidate: its channel and pattern identity.
fn stream_loop_candidate_key(
    channel: nuo_wire::StreamLoopChannel,
    pattern: &crate::stream_loop_detector::DegeneratePattern,
) -> String {
    format!("{channel:?}:{pattern:?}")
}

/// Keep the beginning and live tail of a large field. Most turns fit in full;
/// the bound prevents one diagnostic call from exceeding the cognitive model's
/// context on an already-pathological stream. The explicit omission marker is
/// evidence too: the reviewer knows it received a projection, not a seamless
/// string.
fn project_stream_loop_field(text: &str, max_chars: usize) -> String {
    let char_count = text.chars().count();
    if char_count <= max_chars {
        return text.to_string();
    }
    let head_chars = max_chars / 3;
    let tail_chars = max_chars.saturating_sub(head_chars);
    let head: String = text.chars().take(head_chars).collect();
    let tail: String = text
        .chars()
        .rev()
        .take(tail_chars)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!(
        "{head}\n\n[... {} characters omitted by harness projection ...]\n\n{tail}",
        char_count.saturating_sub(max_chars)
    )
}

/// Render the immediate conversational evidence that makes repeated output
/// meaningful: the latest user request, assistant/tool observations, and
/// command output. System and hidden control messages are excluded so a
/// reviewer judges task content rather than harness instructions.
fn stream_loop_preceding_context(messages: &[Message]) -> String {
    let mut blocks = Vec::new();
    let mut used = 0usize;
    for message in messages.iter().rev() {
        if message.hidden || message.role == Role::System || message.content.trim().is_empty() {
            continue;
        }
        let role = match message.role {
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
            Role::System => continue,
        };
        let remaining = STREAM_LOOP_PRECEDING_CONTEXT_CHARS.saturating_sub(used);
        if remaining == 0 {
            break;
        }
        let body = project_stream_loop_field(message.content.trim(), remaining.min(6_000));
        used = used.saturating_add(body.chars().count() + role.len() + 2);
        blocks.push(format!("{role}: {body}"));
    }
    blocks.reverse();
    blocks.join("\n\n")
}

pub(crate) struct ToolResultRecord<'a> {
    pub call: &'a ToolCall,
    pub call_id: &'a str,
    pub result: &'a ToolOutput,
    pub duration_ms: u64,
    pub checkpoint_replay: bool,
    pub emit_event: bool,
}

impl Agent {
    /// Gate an L1 mechanical candidate through the Stream Sentinel
    /// *without pausing the stream*. UX-first: harness cognition must never
    /// freeze delta rendering, so the consult is spawned detached and its
    /// verdict is applied at the next event boundary. The detector is
    /// level-triggered — it keeps firing while the pattern continues — so a
    /// still-looping stream pays only a few extra degenerate tokens instead
    /// of a frozen UI.
    ///
    /// Decisions mirror the retired blocking consult exactly: a candidate
    /// key acquitted earlier in this provider response never re-consults
    /// (its detector re-arms from zero instead), at most one consult is in
    /// flight at a time, and any malformed/failed consultation fail-opens
    /// to `no` inside [`crate::cognitive::CognitivePipeline`].
    #[allow(clippy::too_many_arguments)] // mirrors the two call sites' evidence set; bundling would obscure the channel symmetry
    fn dispatch_stream_loop_candidate(
        &self,
        detector: &mut crate::stream_loop_detector::StreamLoopDetector,
        messages: &[Message],
        assistant_text: &str,
        reasoning_text: &str,
        channel: nuo_wire::StreamLoopChannel,
        pattern: crate::stream_loop_detector::DegeneratePattern,
        cleared_candidates: &std::collections::HashSet<String>,
        pending: &mut Option<PendingStreamLoopReview>,
    ) {
        let candidate_key = stream_loop_candidate_key(channel, &pattern);
        if cleared_candidates.contains(&candidate_key) {
            // Acquitted earlier in this response: intentional repeated data
            // re-earns dwell from zero rather than re-arming suspicion.
            detector.reset();
            return;
        }
        if pending.is_some() {
            // One Stream Sentinel consult in flight; the level-triggered
            // detector re-fires on the next push once this one resolves.
            return;
        }
        *pending = Some(self.spawn_stream_loop_review(
            messages,
            assistant_text,
            reasoning_text,
            channel,
            pattern,
        ));
    }

    /// Snapshot the evidence and spawn the cognitive loop review as a
    /// detached task. The task always resolves (the cognitive pipeline fail-opens on
    /// its own timeout, provider failures, and malformed answers) and sends
    /// exactly one verdict; a failed send merely means the round moved on
    /// without it. Running detached also guarantees the provider
    /// side-channel drain inside `CognitivePipeline::consult` completes even when the
    /// round exits before the verdict lands.
    fn spawn_stream_loop_review(
        &self,
        messages: &[Message],
        assistant_text: &str,
        reasoning_text: &str,
        channel: nuo_wire::StreamLoopChannel,
        pattern: crate::stream_loop_detector::DegeneratePattern,
    ) -> PendingStreamLoopReview {
        let input = nuo_wire::StreamLoopReviewInput {
            heuristic_candidate: pattern.description(),
            channel,
            preceding_context: stream_loop_preceding_context(messages),
            assistant_text: project_stream_loop_field(assistant_text, STREAM_LOOP_TURN_FIELD_CHARS),
            reasoning_text: project_stream_loop_field(reasoning_text, STREAM_LOOP_TURN_FIELD_CHARS),
        };
        let (verdict_tx, verdict_rx) = tokio::sync::oneshot::channel();
        let cognitive = self.cognitive();
        tokio::spawn(async move {
            let verdict = cognitive.review_stream_loop(input).await;
            let _ = verdict_tx.send(verdict);
        });
        PendingStreamLoopReview {
            candidate_key: stream_loop_candidate_key(channel, &pattern),
            channel,
            pattern,
            verdict_rx,
        }
    }

    #[tracing::instrument(skip_all, name = "round", fields(streaming = true))]
    pub async fn run_streaming_with_events<F>(
        self: &Arc<Self>,
        messages: &mut Vec<Message>,
        cancel: &CancellationToken,
        on_event: F,
    ) -> Result<RoundOutcome, HarnessError>
    where
        F: FnMut(AgentEvent) + Send,
    {
        let mut round = self.begin_streaming_round();
        self.resume_streaming_with_events(messages, cancel, &mut round, on_event)
            .await
    }

    /// Start the durable in-memory state for one streaming user round.
    ///
    /// The top-level orchestrator retains this across transient provider
    /// failures so it can retry the failed request without re-entering the
    /// round from scratch. Standalone callers use [`Self::run_streaming_with_events`],
    /// which creates and consumes the state in one call.
    pub(crate) fn begin_streaming_round(&self) -> StreamingRoundState {
        // Reset the human-decision pause accumulator for this round so the
        // tokens/sec derived at the exit gate excludes only *this* round's
        // permission/ask_user waits.
        self.round_paused_ms
            .store(0, std::sync::atomic::Ordering::Relaxed);
        StreamingRoundState {
            turn_context: Arc::default(),
            state: RoundState {
                guards: RoundState::guards_default(self.trajectory_guard_config()),
                ..RoundState::default()
            },
            turn_index: 0,
            stream_loop_recoveries: 0,
            inbox_rx: self
                .inbox_rx
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take(),
            started_at: std::time::Instant::now(),
            pending_request: None,
            session_queue_generation: self.session_queue_generation(),
        }
    }

    /// Resume a previously stopped round as *the same round* — the `/retry`
    /// path (ADR-0128). Unlike [`Self::begin_streaming_round`] this does not
    /// restart the round from scratch: it re-seeds the ReAct state from the
    /// durable [`RetryPoint`] captured when the round stopped, so the resumed
    /// execution
    ///
    /// - keeps numbering turns from `turns_committed` (the transcript's
    ///   `round N · turn M` sequence stays unbroken — turn M+1 follows the
    ///   committed M, never a duplicate),
    /// - re-arms the pause accumulator from `paused_ms` so a later
    ///   tokens/sec stays honest across the stop, and
    /// - never re-runs turn-start hooks or side-effecting tools for turns
    ///   that already completed: `pending_request` stays `None`, so the first
    ///   request is assembled fresh from the checkpointed history.
    ///
    /// The inbox receiver is taken here exactly as in `begin_streaming_round`
    /// — between rounds it is returned to the agent (`RoundState::Drop` puts
    /// it back), so a mid-resume steering insert still has a drain target.
    pub(crate) fn resume_streaming_round(
        &self,
        point: &nuo_wire::RetryPoint,
    ) -> StreamingRoundState {
        self.round_paused_ms
            .store(point.paused_ms, std::sync::atomic::Ordering::Relaxed);
        StreamingRoundState {
            turn_context: Arc::default(),
            state: RoundState {
                guards: RoundState::guards_default(self.trajectory_guard_config()),
                ..RoundState::default()
            },
            turn_index: point.turns_committed,
            stream_loop_recoveries: 0,
            inbox_rx: self
                .inbox_rx
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take(),
            started_at: std::time::Instant::now(),
            pending_request: None,
            session_queue_generation: self.session_queue_generation(),
        }
    }

    /// Run or resume a streaming round from its last provider-request boundary.
    ///
    /// A retryable [`HarnessError::Provider`] leaves `round` reusable. If the failed
    /// request followed completed tool calls, their messages and the complete
    /// per-round state are already present, so the next invocation sends the
    /// same pending provider request instead of executing those tools again.
    pub(crate) async fn resume_streaming_with_events<F>(
        self: &Arc<Self>,
        messages: &mut Vec<Message>,
        cancel: &CancellationToken,
        round: &mut StreamingRoundState,
        on_event: F,
    ) -> Result<RoundOutcome, HarnessError>
    where
        F: FnMut(AgentEvent) + Send,
    {
        let result = self
            .resume_streaming_inner(messages, cancel, round, on_event)
            .await;
        if result.is_err()
            && let Some(ledger) = self.token_ledger()
        {
            ledger
                .persist_pending(&self.thread_id().unwrap_or_default())
                .await
                .map_err(HarnessError::Other)?;
        }
        result
    }

    async fn resume_streaming_inner<F>(
        self: &Arc<Self>,
        messages: &mut Vec<Message>,
        cancel: &CancellationToken,
        round: &mut StreamingRoundState,
        mut on_event: F,
    ) -> Result<RoundOutcome, HarnessError>
    where
        F: FnMut(AgentEvent) + Send,
    {
        loop {
            if cancel.is_cancelled() {
                return Err(HarnessError::Interrupted);
            }

            let resuming_provider_request = round.pending_request.is_some();
            if resuming_provider_request {
                round.state.protect_completed_tools_for_retry();
            }
            if round.pending_request.is_none() {
                // Apply steering ops queued since the last turn before
                // preparing a new provider request. Replies bypass this (see
                // `drain_inbox`). A transient retry deliberately skips this
                // block: the already-prepared request is the checkpoint.
                if !self.drain_inbox(&mut round.inbox_rx, messages) {
                    return Err(HarnessError::Interrupted);
                }
                if self.drain_steering(round.session_queue_generation, messages, &mut on_event) > 0
                {
                    self.fire_turn_persist(messages).await?;
                }

                crate::conversation_context::inject_mentioned_skills(
                    &self.skills_registry,
                    messages,
                );
                // `@file:` injection happens exactly here — once per prompt,
                // into the live window — so the projected request (and every
                // estimate built on it) reuses the loaded bytes without ever
                // touching the filesystem again. Loading asynchronously keeps
                // the turn-preparation path off blocking reads.
                crate::conversation_context::inject_mentioned_files(
                    self.workspace_root().as_deref(),
                    messages,
                )
                .await;
                // TurnStart hooks belong to a logical ReAct turn, not to
                // each network attempt. Run them once before checkpointing the
                // request so retries cannot duplicate injected context or hook
                // side effects.
                self.run_turn_start_hooks(messages, &round.state, round.turn_index)
                    .await;
                // Assemble once per turn, then gate on context pressure with
                // the assembled request (ADR-0187 hot path): the estimate and
                // the provider call share one assembly. A projection that
                // replaces the window re-assembles exactly once.
                let mut request = self.model_request(messages);
                if self
                    .project_context_if_needed(messages, &request, cancel)
                    .await?
                {
                    request = self.model_request(messages);
                }
                request.turn_context = Arc::clone(&round.turn_context);
                round.pending_request = Some(request);
            }
            tracing::debug!(
                turn = round.turn_index,
                resumed = resuming_provider_request,
                "requesting model completion"
            );
            let Some(request) = round.pending_request.as_ref() else {
                return Err(HarnessError::from(
                    "internal error: provider request was not assembled".to_string(),
                ));
            };
            // The estimate target is the already-assembled pending request,
            // so with the content-addressed weights cache warm this is a
            // fingerprint walk (hash-rate, not BPE-rate) over unchanged bytes
            // — cheap enough to stay inline on the attempt path.
            let request_estimate = self.estimate_model_request(request);
            // ADR-0218: archive the freshly assembled request's projection once
            // per logical invocation. A transport retry reuses `pending_request`
            // (so `resuming_provider_request` is true) and must not re-record.
            if !resuming_provider_request {
                self.fire_request_projection_persist(
                    nuo_wire::RequestProjection::from_request(
                        request,
                        request_estimate.temporary_context_tokens,
                        self.round_count(),
                        round.turn_index as u64,
                        crate::orchestration::unix_epoch_ms(),
                    ),
                );
            }
            let request_projection = request_estimate.total_tokens;
            let request_provider = self.provider.provider_id();
            let request_model = self.provider.model();
            on_event(AgentEvent::ModelRequestStarted {
                round: self.round_count(),
                turn: round.turn_index,
                context_tokens: request_projection,
            });
            on_event(AgentEvent::ContextTokens(
                nuo_wire::ContextTokenSnapshot::from_estimate(
                    request_estimate,
                    nuo_wire::ContextTokenSource::Projection,
                ),
            ));
            // Allocate the ledger attempt and start its monotonic clock only
            // after local request-boundary events have been emitted. This is
            // the actual provider-call boundary used by TTFT/E2E telemetry.
            let mut request_accounting = RequestAccountingGuard::begin(
                self,
                cancel,
                &request_provider,
                &request_model,
                round.turn_index,
                request_projection,
            );
            if let (Some(ledger), Some(key)) = (&request_accounting.ledger, &request_accounting.key)
            {
                ledger
                    .persist_request(key)
                    .await
                    .map_err(HarnessError::Other)?;
            }
            request_accounting.start_request();
            // Stamp this attempt's own telemetry handle onto the request it
            // dispatches (ADR-0232). The turn's assembled request is reused by
            // retries, so the handle is assigned per attempt rather than
            // inherited: the transport fills the handle it is handed, and the
            // attempt that handed it over is the only reader.
            let mut attempt_request = request.clone();
            attempt_request.transport_telemetry = request_accounting.transport_telemetry();
            // Race the model request against cancellation so an interrupt
            // while we're waiting on the network resolves promptly instead of
            // blocking until the first stream chunk arrives. The idle-timeout
            // arm covers a provider endpoint that accepts the connection but
            // never sends HTTP response headers (overloaded upstream, dropped
            // proxy) — without it the select would hang forever on `.send()`.
            let mut stream = tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(HarnessError::Interrupted),
                result = tokio::time::timeout(
                    STREAM_IDLE_TIMEOUT,
                    self.provider.stream_chat_events(attempt_request),
                ) => match result {
                    Ok(Ok(stream)) => {
                        request_accounting.mark_stream_ready();
                        stream
                    },
                    Ok(Err(error)) => {
                        let err_msg = error.to_string();
                        request_accounting.record_error(&err_msg);
                        return Err(HarnessError::from(error));
                    }
                    Err(_elapsed) => {
                        tracing::warn!(
                            timeout_secs = STREAM_IDLE_TIMEOUT.as_secs(),
                            "stream request timed out before any response"
                        );
                        let err_msg = format!(
                            "Provider did not start streaming within {} seconds.",
                            STREAM_IDLE_TIMEOUT.as_secs()
                        );
                        request_accounting.record_error(&err_msg);
                        return Err(HarnessError::Provider(nuo_wire::ProviderError::new("harness", nuo_wire::ProviderErrorKind::Timeout, err_msg).retryable(None)));
                    }
                },
            };
            let mut content = String::new();
            let mut reasoning_content = String::new();
            let mut calls: Vec<ToolCall> = Vec::new();
            let mut emitted_text = false;
            let mut emitted_reasoning = false;
            // `Usage` remains a progress signal for protocols that report it
            // before their terminal frame. Provider-owned response state is
            // accepted only from the unique `Completed` event below.
            let mut streamed_usage: Option<TokenUsage> = None;
            let mut completion_meta: Option<nuo_wire::ProviderCompletionMeta> = None;
            let mut text_loop_detector = crate::stream_loop_detector::StreamLoopDetector::new(1024);
            let mut reasoning_loop_detector =
                crate::stream_loop_detector::StreamLoopDetector::new(1024);
            let mut stream_loop_incident: Option<StreamLoopIncident> = None;
            let mut cognitive_cleared_candidates = std::collections::HashSet::new();
            // Cognitive loop reviews run detached from this loop: the stream
            // keeps rendering while the cognitive pipeline deliberates, and the verdict is
            // applied at the next event boundary. At most one consult is in
            // flight at a time; the level-triggered detector re-fires after an
            // acquittal, so a still-looping stream is never left unreviewed.
            let mut pending_stream_loop_review: Option<PendingStreamLoopReview> = None;
            // Finish-drain deadline (FINISH_DRAIN_GRACE). Once this turn's
            // stream has produced at least one delta, a cancellation no
            // longer wins the biased select outright: the stream gets one
            // short, strictly bounded window to reach its natural end. A
            // model that finished its answer right as the user sent the
            // next message (or hit Esc Esc) then commits that answer as a
            // completed round instead of unwinding as `Interrupted` after
            // every delta was already rendered — the false
            // "▲ interrupted · new message" marker over a round the user
            // watched finish. A stream that is still silent when the
            // deadline expires was not settling: the interrupt stands, so
            // an interrupt is delayed by at most FINISH_DRAIN_GRACE and a
            // genuinely mid-generation answer is still cut.
            let mut finish_drain: Option<std::time::Instant> = None;
            let mut turn_streamed = false;
            // ADR-0026: which tool-call slots have been announced (their name
            // has arrived) and the argument-byte count last reported per slot.
            // Announcement fires once per slot at the first named fragment;
            // progress ticks fire only when the byte count advances by
            // `TOOL_INPUT_PROGRESS_STEP`, never per fragment
            // ([INV-STREAM-TOOL-04]). Nothing here weakens the execution gate:
            // dispatch still waits for the whole object after the stream ends
            // ([INV-STREAM-TOOL-01]).
            const TOOL_INPUT_PROGRESS_STEP: usize = 4096;
            let mut announced_tool_calls: std::collections::HashSet<usize> =
                std::collections::HashSet::new();
            let mut reported_tool_input_bytes: std::collections::HashMap<usize, usize> =
                std::collections::HashMap::new();

            loop {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled(), if finish_drain.is_none() => {
                        if !turn_streamed {
                            return Err(HarnessError::Interrupted);
                        }
                        // The stream already produced output this turn; open
                        // the bounded settle window instead of cutting now.
                        finish_drain = Some(std::time::Instant::now()
                            + crate::FINISH_DRAIN_GRACE);
                        continue;
                    }
                    // Stream Sentinel verdicts land off the hot path — the
                    // stream kept rendering while the consult ran. A
                    // confirmation cuts here at the next event boundary; an
                    // acquittal clears the candidate for the rest of this
                    // provider response and re-arms its channel's detector.
                    // The branch outranks the stream arm so a pending cut
                    // beats rendering one more chunk.
                    verdict = async {
                        match pending_stream_loop_review.as_mut() {
                            Some(review) => {
                                (&mut review.verdict_rx)
                                    .await
                                    .unwrap_or(nuo_wire::StreamLoopVerdict::No)
                            }
                            // Unreachable behind the branch guard; stay
                            // pending rather than panicking.
                            None => std::future::pending().await,
                        }
                    }, if pending_stream_loop_review.is_some() => {
                        let Some(review) = pending_stream_loop_review.take() else {
                            continue;
                        };
                        tracing::debug!(
                            channel = ?review.channel,
                            pattern = %review.pattern.description(),
                            verdict = ?verdict,
                            "stream-loop candidate reviewed by cognitive pipeline"
                        );
                        if verdict.is_loop() {
                            match review.channel {
                                nuo_wire::StreamLoopChannel::AssistantText => {
                                    tracing::warn!(
                                        pattern = %review.pattern.description(),
                                        "Cognitive pipeline confirmed in-flight text stream loop; aborting stream early"
                                    );
                                }
                                nuo_wire::StreamLoopChannel::Reasoning => {
                                    tracing::warn!(
                                        pattern = %review.pattern.description(),
                                        "Cognitive pipeline confirmed in-flight reasoning stream loop; aborting stream early"
                                    );
                                }
                            }
                            stream_loop_incident = Some(StreamLoopIncident {
                                pattern: review.pattern,
                                channel: review.channel,
                            });
                            break;
                        }
                        cognitive_cleared_candidates.insert(review.candidate_key);
                        match review.channel {
                            nuo_wire::StreamLoopChannel::AssistantText => {
                                text_loop_detector.reset();
                            }
                            nuo_wire::StreamLoopChannel::Reasoning => {
                                reasoning_loop_detector.reset();
                            }
                        }
                    }
                    // Guard against a stalled SSE stream: providers share a
                    // pooled client whose connect timeout covers the handshake
                    // but which deliberately sets no read timeout on streaming
                    // responses, so without this bound a connection that stays
                    // open but stops sending (common with overloaded
                    // reasoning-model endpoints) blocks the turn forever. The
                    // idle clock resets on every chunk, so a legitimately slow
                    // reasoning model that keeps trickling deltas is never cut
                    // off.
                    event = tokio::time::timeout(
                        finish_drain.map_or(STREAM_IDLE_TIMEOUT, |deadline| {
                            deadline.saturating_duration_since(std::time::Instant::now())
                        }),
                        stream.next(),
                    ) => {
                        let event = match event {
                            Ok(Some(event)) => event,
                            Ok(None) => {
                                request_accounting.mark_stream_end();
                                break;
                            },
                            Err(_elapsed) if finish_drain.is_some() => {
                                // The settle window expired with the stream
                                // neither ended nor producing a chunk: the
                                // answer was not finishing after all. Honour
                                // the interrupt.
                                return Err(HarnessError::Interrupted);
                            }
                            Err(_elapsed) => {
                                tracing::warn!(
                                    idle_timeout_secs = STREAM_IDLE_TIMEOUT.as_secs(),
                                    "stream stalled: no data received within idle timeout"
                                );
                                let err_msg = format!(
                                    "Provider stream stalled — no data received \
                                     for {} seconds.",
                                    STREAM_IDLE_TIMEOUT.as_secs()
                                );
                                request_accounting.record_error(&err_msg);
                                return Err(HarnessError::Provider(nuo_wire::ProviderError::new("harness", nuo_wire::ProviderErrorKind::Timeout, err_msg).retryable(None)));
                            }
                        };
                        let event = match event {
                            Ok(event) => event,
                            Err(error) => {
                                request_accounting.record_error(error.to_string());
                                return Err(HarnessError::from(error));
                            }
                        };
                        // Content, tool-call bytes, usage, or completion proves
                        // that this turn was delivering. Transport control
                        // metadata does not: a catalog ETag arriving before any
                        // model output must not convert an immediate interrupt
                        // into a settle-window drain.
                        if !matches!(&event, ProviderStreamEvent::ModelCatalogEtag(_)) {
                            turn_streamed = true;
                        }
                        if completion_meta.is_some() {
                            let err_msg = "Provider emitted data after the terminal completion event."
                                .to_string();
                            request_accounting.record_error(&err_msg);
                            return Err(HarnessError::Provider(nuo_wire::ProviderError::new("harness", nuo_wire::ProviderErrorKind::Protocol, err_msg).retryable(None)));
                        }
                        request_accounting.observe_stream_event(
                            &event,
                            std::time::Instant::now(),
                        );
                        match event {
                            ProviderStreamEvent::ModelCatalogEtag(etag) => {
                                let connection_id = self.provider.provider_id();
                                let outcome = crate::catalog::refresh_connection_models_for_etag(
                                    &connection_id,
                                    &etag,
                                )
                                .await;
                                if outcome.changed {
                                    crate::catalog::sync_fitted_model_registry();
                                    crate::catalog::prune_stale_models_on_disk();
                                    on_event(AgentEvent::CatalogInvalidated);
                                }
                                for failure in outcome.failures {
                                    tracing::warn!(
                                        connection_id = %failure.connection,
                                        error = %failure.message,
                                        refused = failure.refused,
                                        "model catalog ETag refresh failed"
                                    );
                                }
                            }
                            ProviderStreamEvent::TextDelta(delta) => {
                                content.push_str(&delta);
                                on_event(AgentEvent::AssistantDelta {
                                    delta: delta.clone(),
                                    start: !emitted_text,
                                });
                                emitted_text = true;
                                if let Some(pat) = text_loop_detector.push_and_check(&delta) {
                                    // Async consult: keep consuming deltas
                                    // while the Stream Sentinel deliberates.
                                    self.dispatch_stream_loop_candidate(
                                        &mut text_loop_detector,
                                        messages,
                                        &content,
                                        &reasoning_content,
                                        nuo_wire::StreamLoopChannel::AssistantText,
                                        pat,
                                        &cognitive_cleared_candidates,
                                        &mut pending_stream_loop_review,
                                    );
                                }
                            }
                            ProviderStreamEvent::ReasoningDelta(delta) => {
                                reasoning_content.push_str(&delta);
                                on_event(AgentEvent::ReasoningDelta {
                                    delta: delta.clone(),
                                    start: !emitted_reasoning,
                                });
                                emitted_reasoning = true;
                                if let Some(pat) = reasoning_loop_detector.push_and_check(&delta) {
                                    // Async consult: keep consuming deltas
                                    // while the cognitive reviewer deliberates.
                                    self.dispatch_stream_loop_candidate(
                                        &mut reasoning_loop_detector,
                                        messages,
                                        &content,
                                        &reasoning_content,
                                        nuo_wire::StreamLoopChannel::Reasoning,
                                        pat,
                                        &cognitive_cleared_candidates,
                                        &mut pending_stream_loop_review,
                                    );
                                }
                            }
                            ProviderStreamEvent::ToolCallDelta {
                                index,
                                id,
                                name,
                                arguments,
                            } => {
                                while calls.len() <= index {
                                    calls.push(ToolCall {
                                        id: String::new(),
                                        name: String::new(),
                                        arguments: String::new(),
                                    });
                                }
                                let call = &mut calls[index];
                                if let Some(id) = id {
                                    call.id.push_str(&id);
                                }
                                if let Some(name) = name {
                                    call.name.push_str(&name);
                                }
                                call.arguments.push_str(&arguments);
                                // ADR-0026: the moment this slot first has a
                                // name, announce the call so the frontend can
                                // show a running step and the tool phase while
                                // the arguments keep streaming. The execution
                                // gate is untouched — dispatch still waits for
                                // the whole object after the stream ends.
                                if !announced_tool_calls.contains(&index)
                                    && !call.name.is_empty()
                                {
                                    announced_tool_calls.insert(index);
                                    let announced_id =
                                        (!call.id.is_empty()).then(|| call.id.clone());
                                    let announced_name = call.name.clone();
                                    on_event(AgentEvent::ToolCallStarted {
                                        index,
                                        id: announced_id,
                                        name: announced_name,
                                    });
                                }
                                // Count-only progress, throttled to a bounded
                                // cadence — never per fragment, never the bytes
                                // ([INV-STREAM-TOOL-03] / [INV-STREAM-TOOL-04]).
                                let total = call.arguments.len();
                                let reported =
                                    reported_tool_input_bytes.get(&index).copied().unwrap_or(0);
                                if total.saturating_sub(reported) >= TOOL_INPUT_PROGRESS_STEP {
                                    reported_tool_input_bytes.insert(index, total);
                                    let progress_id =
                                        (!call.id.is_empty()).then(|| call.id.clone());
                                    on_event(AgentEvent::ToolInputProgress {
                                        index,
                                        id: progress_id,
                                        bytes: total,
                                    });
                                }
                            }
                            ProviderStreamEvent::Usage(usage) => {
                                // Take the last reported usage (providers may
                                // emit one final usage chunk). Prefer it over
                                // the local estimate at booking time.
                                streamed_usage = Some(usage);
                            }
                            ProviderStreamEvent::Completed(meta) => {
                                if let Some(usage) = meta.usage {
                                    streamed_usage = Some(usage);
                                }
                                completion_meta = Some(meta);
                            }
                        }
                    }
                }
            }
            request_accounting.mark_stream_end();

            // Strict stream finalization (the same discipline praxion's
            // accumulator enforces at `finish()`): the stream must not end
            // mid-tool-call. A slot that accumulated id/argument bytes but
            // never received a name is the residue of a truncated stream —
            // dropping it silently would mistake a connection failure for
            // the model's intent. Surface it as retryable so the idempotent
            // request retry re-runs the turn instead of committing a partial
            // response. Slots that stayed completely empty (a provider delta
            // that carried only an index) are still dropped below.
            for call in &calls {
                if call.name.is_empty() && (!call.id.is_empty() || !call.arguments.is_empty()) {
                    let err_msg =
                        "Provider stream ended mid-tool-call; the response was likely truncated."
                            .to_string();
                    request_accounting.record_error(&err_msg);
                    return Err(HarnessError::Provider(
                        nuo_wire::ProviderError::new(
                            "harness",
                            nuo_wire::ProviderErrorKind::Transport,
                            err_msg,
                        )
                        .retryable(None),
                    ));
                }
                if !call.arguments.is_empty()
                    && serde_json::from_str::<serde_json::Value>(&call.arguments).is_err()
                {
                    let err_msg = format!(
                        "Provider stream ended with truncated arguments for tool \
                         call `{}`; the response was likely cut off.",
                        call.name
                    );
                    request_accounting.record_error(&err_msg);
                    return Err(HarnessError::Provider(
                        nuo_wire::ProviderError::new(
                            "harness",
                            nuo_wire::ProviderErrorKind::Transport,
                            err_msg,
                        )
                        .retryable(None),
                    ));
                }
            }

            // A Stream Sentinel consult still in flight when the stream ended
            // naturally still owes a verdict: settling it here (bounded by the
            // consult's own 2s timeout) preserves the blocking consult's
            // guarantee — a confirmed loop is trimmed, disclosed, and
            // recovered instead of silently committing the degenerate tail.
            // A verdict after an incident cut has nothing left to decide.
            if stream_loop_incident.is_none()
                && let Some(review) = pending_stream_loop_review.take()
            {
                let verdict = review
                    .verdict_rx
                    .await
                    .unwrap_or(nuo_wire::StreamLoopVerdict::No);
                tracing::debug!(
                    channel = ?review.channel,
                    pattern = %review.pattern.description(),
                    verdict = ?verdict,
                    office = "stream_sentinel",
                    "stream-loop candidate reviewed by Stream Sentinel after natural stream end"
                );
                if verdict.is_loop() {
                    tracing::warn!(
                        pattern = %review.pattern.description(),
                        "Cognitive pipeline confirmed a stream loop that ended naturally; trimming the degenerate tail"
                    );
                    stream_loop_incident = Some(StreamLoopIncident {
                        pattern: review.pattern,
                        channel: review.channel,
                    });
                } else {
                    cognitive_cleared_candidates.insert(review.candidate_key);
                }
            }

            if completion_meta.is_none() && stream_loop_incident.is_none() {
                let err_msg = "Provider stream ended without a terminal completion event; response state was not committed."
                    .to_string();
                request_accounting.record_error(&err_msg);
                return Err(HarnessError::Provider(
                    nuo_wire::ProviderError::new(
                        "harness",
                        nuo_wire::ProviderErrorKind::Transport,
                        err_msg,
                    )
                    .retryable(None),
                ));
            }

            // A detected loop cuts this provider stream, but it does not
            // silently end the user's round. The first cutoff is a soft
            // intervention: retain the partial response, visibly disclose the
            // action, inject guidance, and make one fresh model request. If
            // the recovery request loops too, stop after retaining its partial
            // response and disclose the hard stop. Tool-call fragments from a
            // cut stream are never executable intent.
            let mut stream_loop_recovery_prompt = None;
            let mut stream_loop_hard_stopped = false;
            let stream_loop_notice = stream_loop_incident.as_ref().map(|incident| {
                match incident.channel {
                    nuo_wire::StreamLoopChannel::AssistantText => {
                        content = crate::stream_loop_detector::StreamLoopDetector::trim_suffix(
                            &content,
                            &incident.pattern,
                        );
                    }
                    nuo_wire::StreamLoopChannel::Reasoning => {
                        reasoning_content =
                            crate::stream_loop_detector::StreamLoopDetector::trim_suffix(
                                &reasoning_content,
                                &incident.pattern,
                            );
                    }
                }
                calls.clear();

                if round.stream_loop_recoveries < MAX_STREAM_LOOP_RECOVERIES {
                    round.stream_loop_recoveries += 1;
                    stream_loop_recovery_prompt = Some(format!(
                        "[System Directive: Your previous output was stopped because it entered a {}. \
                         Synthesize the conclusions already reached, then take the next distinct action \
                         or complete the task. Do not repeat the prior text.]",
                        incident.pattern.description()
                    ));
                    AgentNotice::new(
                        NoticeKind::NudgeInjected,
                        NoticeSeverity::Warning,
                        "Repetitive output stopped; recovery requested",
                        NoticeSource::TurnGuard,
                    )
                    .with_body(format!(
                        "The Harness cognitive check confirmed a {}. Its partial output was retained, and the agent received guidance to retry once without repeating it.",
                        incident.pattern.description()
                    ))
                } else {
                    stream_loop_hard_stopped = true;
                    AgentNotice::new(
                        NoticeKind::NudgeInjected,
                        NoticeSeverity::Error,
                        "Repetitive output stopped again; round ended",
                        NoticeSource::TurnGuard,
                    )
                    .with_body(format!(
                        "The Harness cognitive check confirmed that the guided recovery entered a {}. Generation was stopped to prevent a runaway stream; its partial output was retained.",
                        incident.pattern.description()
                    ))
                }
            });

            if emitted_text {
                on_event(AgentEvent::AssistantEnd(content.clone()));
            }
            if emitted_reasoning {
                on_event(AgentEvent::ReasoningEnd(reasoning_content.clone()));
            }
            if let Some(notice) = stream_loop_notice {
                // Inline notices are retained in the transcript. A transient
                // toast is too easy to miss for an action that changed the
                // generated answer.
                on_event(AgentEvent::Notice(notice));
            }

            calls.retain(|call| !call.name.is_empty());
            for call in &mut calls {
                // Arguments that streamed but do not parse as JSON mean the
                // connection died mid-payload; fail retryable here instead of
                // executing half a call and surfacing the parse error as if
                // the model had emitted bad JSON. (`arguments == ""` is the
                // legitimate shape of a zero-argument tool and must not trip
                // this check.)
                if !call.arguments.is_empty()
                    && serde_json::from_str::<serde_json::Value>(&call.arguments).is_err()
                {
                    let err_msg = format!(
                        "Provider stream ended with truncated arguments for tool \
                         call `{}`; the response was likely cut off.",
                        call.name
                    );
                    request_accounting.record_error(&err_msg);
                    return Err(HarnessError::Provider(
                        nuo_wire::ProviderError::new(
                            "harness",
                            nuo_wire::ProviderErrorKind::Transport,
                            err_msg,
                        )
                        .retryable(None),
                    ));
                }
                if call.id.is_empty() {
                    call.id = format!("call_{}", uuid::Uuid::new_v4());
                }
            }
            let bound_cursor = if let Some(cursor) = completion_meta
                .as_mut()
                .and_then(|meta| meta.continuation.as_mut())
            {
                let prospective = Message {
                    role: Role::Assistant,
                    content: content.clone(),
                    content_blob: None,
                    display_content: None,
                    reasoning_content: (!reasoning_content.is_empty())
                        .then_some(reasoning_content.clone()),
                    tool_calls: (!calls.is_empty()).then_some(calls.clone()),
                    tool_call_id: None,
                    images: None,
                    provider: None,
                    model: None,
                    effort: None,
                    provider_meta: None,
                    hidden: false,
                    children: None,
                    subagent_meta: None,
                    origin: None,
                    timestamp: None,
                    sent_at_ms: None,
                    cache_frozen: false,
                };
                cursor.local_head = nuo_wire::semantic_context_head(
                    messages.iter().chain(std::iter::once(&prospective)),
                );
                Some(cursor.clone())
            } else {
                None
            };
            if let (Some(cursor), Some(meta)) = (bound_cursor, completion_meta.as_mut()) {
                let artifacts = meta.artifacts.get_or_insert_with(serde_json::Map::new);
                nuo_wire::write_continuation_cursor(artifacts, &cursor);
            }
            let response = Message {
                role: Role::Assistant,
                content,
                content_blob: None,
                display_content: None,
                reasoning_content: (!reasoning_content.is_empty()).then_some(reasoning_content),
                tool_calls: (!calls.is_empty()).then_some(calls),
                tool_call_id: None,
                images: None,
                // Stamp which provider/model produced this turn so a session
                // that mixes models stays traceable after resume. The proxy
                // provider delegates to whichever concrete provider is active.
                provider: Some(request_provider),
                model: Some(request_model),
                // Reasoning depth the channel actually ran this request at
                // (thinking-gated per protocol), so the transcript can label
                // each turn with the effort it truly used.
                effort: self.provider.effort().map(|e| e.as_str().to_string()),
                // Provider-private replay material is valid only at terminal
                // completion and belongs to this exact assistant node.
                provider_meta: completion_meta
                    .as_mut()
                    .and_then(|meta| meta.artifacts.take()),
                hidden: false,
                children: None,
                subagent_meta: None,
                origin: None,
                timestamp: Some(nuo_wire::todos::unix_now()),
                sent_at_ms: None,
                cache_frozen: false,
            };
            if !valid_assistant_response(&response) {
                return Err(empty_response_error(&response));
            }
            // The request checkpoint is consumed only after a complete,
            // valid response is available. Any earlier return leaves it set
            // so orchestration can retry this exact request.
            round.pending_request = None;
            let performance = self.book_turn_usage(
                &mut round.state,
                &response,
                completion_meta
                    .as_ref()
                    .and_then(|meta| meta.usage)
                    .or_else(|| streamed_usage.take()),
                &mut request_accounting,
            );
            on_event(AgentEvent::TurnPerformance(performance));
            messages.push(response.clone());

            // `emitted_text` means assistant text was already streamed to the
            // UI; a text-fallback tool call must then retract it via a discard.
            if self
                .dispatch_tool_calls(
                    &response,
                    messages,
                    &mut round.state,
                    emitted_text,
                    cancel,
                    &mut on_event,
                )
                .await?
            {
                round.turn_index += 1;
                if self.check_hard_stop(round.turn_index).is_break() {
                    return Err(self.hard_stop_error());
                }
                // The context-pressure gate lives at the request-assembly
                // boundary (top of loop, with the assembled request) — one
                // decision point, one assembly. Projection therefore lands
                // after this turn's persist; its own commit carries the
                // projected window durably.
                // Mid-round save point (ADR-0048): persist this turn's new
                // messages (the assistant response + all tool results) before
                // any further work, so a crash leaves the transcript in sync
                // with filesystem side effects.
                self.fire_turn_persist(messages).await?;
                // The next loop iteration publishes the estimate from the
                // assembled request after Turn/TurnStart hooks. Rebuilding it
                // here repeats full-history work and projects stale hook state.
                self.run_turn_hooks(messages, &round.state, round.turn_index)
                    .await;
                // Restore TurnEnd-scoped disables now that the ReAct turn is
                // over. RoundEnd-scoped disables survive until user-round end.
                self.restore_scoped_turn_end();
                continue;
            }

            // Round-exit gates. The insert drain happens after the provider
            // response commits, so an input typed during a would-be final
            // answer can still force one more turn in this same round.
            let duration_ms = round.started_at.elapsed().as_millis() as u64;
            let mut continue_round = false;
            if let Some(prompt) = stream_loop_recovery_prompt {
                messages.push(crate::conversation_context::hidden_user(
                    InjectionKind::LoopReviewNudge,
                    prompt,
                ));
                continue_round = true;
            } else if !stream_loop_hard_stopped
                && let Some((prompt, kind)) = self.stop_gate(&response).await
            {
                messages.push(crate::conversation_context::hidden_user(kind, prompt));
                continue_round = true;
            }
            let admitted = self.drain_follow_up(
                round.session_queue_generation,
                messages,
                !continue_round,
                &mut on_event,
            );
            if admitted > 0 {
                continue_round = true;
            }
            if continue_round {
                round.turn_index += 1;
                if self.check_hard_stop(round.turn_index).is_break() {
                    return Err(self.hard_stop_error());
                }
                self.fire_turn_persist(messages).await?;
                self.run_turn_hooks(messages, &round.state, round.turn_index)
                    .await;
                self.restore_scoped_turn_end();
                continue;
            }

            // User-round end: clear every scoped disable so the toolset is
            // fresh for the next user request. No persist here: the terminal
            // round commit is the authority for the final answer (ADR-0236 D5),
            // and the mid-round save points have already persisted every
            // continuing turn.
            self.restore_scoped_round_end();
            return Ok(RoundOutcome {
                message: response,
                token_usage: round.state.token_usage,
                duration_ms,
                generation_ms: round.state.generation_ms,
                paused_ms: self
                    .round_paused_ms
                    .load(std::sync::atomic::Ordering::Relaxed),
            });
        }
    }

    /// Execute any tool calls carried by `response`, emitting events and
    /// appending tool results to `messages`. The single dispatch point of the
    /// streaming ReAct loop, so the dispatch contract — repeated-call guard,
    /// up-front `ToolCall` events, concurrent execution with FIFO-ordered
    /// results, and pursuit/mode updates — lives in exactly one place.
    ///
    /// `streamed_text` is true when the response text was already streamed to
    /// the UI, so a recognised text-fallback tool call retracts it with an
    /// `AssistantDiscard`. Returns `true` when a tool-carrying ReAct turn ran
    /// (the caller should loop again), `false` when the round is complete.
    ///
    /// `cancel` makes tool execution cooperative: if the turn is interrupted
    /// mid-flight, cooperatively-cancellable calls (subagents) are drained and
    /// their partial results recorded into `messages` before this returns
    /// `Err(HarnessError::Interrupted)` — so interrupted subagent work survives
    /// into the persisted transcript. Every other already-announced
    /// [`AgentEvent::ToolCall`] is paired with a terminal
    /// [`AgentEvent::ToolCancelled`] before this returns.
    async fn dispatch_tool_calls<F>(
        self: &Arc<Self>,
        response: &Message,
        messages: &mut Vec<Message>,
        state: &mut RoundState,
        streamed_text: bool,
        cancel: &CancellationToken,
        on_event: &mut F,
    ) -> Result<bool, HarnessError>
    where
        F: FnMut(AgentEvent) + Send,
    {
        // Native tool calls (OpenAI-style function calling). An empty list is
        // treated as "no tool calls" so we fall through to the text fallback.
        if let Some(tool_calls) = response
            .tool_calls
            .as_ref()
            .filter(|calls| !calls.is_empty())
        {
            // The four-stage pipeline (see `dispatch_pipeline`):
            //   1. preflight — turn classification, checkpoint-replay scan,
            //      doom-guard check, up-front ToolCall events, short-circuits;
            //   2. prepare — the per-call gate sequence, evaluated in-task
            //      inside `execute_tool` (never serialised across the batch);
            //   3. schedule — concurrent execution through the ToolScheduler;
            //   4. finalize — input-ordered recording, post-tool hooks, nudge.
            let prepared = self
                .dispatch_preflight(tool_calls, messages, state, on_event)
                .await;
            let outcome = if prepared.exec_indices.is_empty() {
                None
            } else {
                let exec_calls: Vec<ToolCall> = prepared
                    .exec_indices
                    .iter()
                    .map(|&i| tool_calls[i].clone())
                    .collect();
                let exec_ids: Vec<String> = prepared
                    .exec_indices
                    .iter()
                    .map(|&i| prepared.call_ids[i].clone())
                    .collect();
                Some(
                    self.schedule_tool_calls(&exec_calls, &exec_ids, cancel, on_event)
                        .await?,
                )
            };
            return self
                .dispatch_finalize(tool_calls, prepared, outcome, messages, state, on_event)
                .await;
        }

        // Text-based fallback: any provider may emit a JSON tool call as text.
        if let Some(call) = crate::tool_call::parse_text_tool_call(&response.content) {
            if streamed_text {
                on_event(AgentEvent::AssistantDiscard);
            }
            tracing::debug!(tool = %call.name, "tool call (text fallback)");
            crate::tool_call::attach_fallback_tool_call(messages, &call);
            // Classify + feed this turn to the guard, mirroring the native
            // path. The text-fallback emits one call per turn, so a read-only
            // turn is exactly "this single call is read-tier". Without this the
            // guard would never see text-fallback turns and a model on such a
            // provider could loop with zero coverage.
            let all_read = self.tool_target_is_unspecified(&call.name, &call.arguments);
            if all_read {
                state.consecutive_readonly_turns =
                    state.consecutive_readonly_turns.saturating_add(1);
            } else {
                state.consecutive_readonly_turns = 0;
            }
            let checkpoint_replay = state.is_checkpoint_replay(&call);
            if checkpoint_replay {
                on_event(AgentEvent::Notice(
                    AgentNotice::new(
                        NoticeKind::ProviderRetry,
                        NoticeSeverity::Warning,
                        "Completed tool call not repeated",
                        NoticeSource::Harness,
                    )
                    .with_body(
                        "The retried model request repeated a tool call that already completed. \
                         The checkpointed result remains authoritative; the tool was not run again.",
                    )
                    .with_surface(NoticeSurface::Toast),
                ));
            }
            // Pre-dispatch trajectory check, mirroring the native path: catch a repeat
            // *before* the text-fallback tool runs.
            let trajectory_action = if checkpoint_replay {
                crate::guard::GuardAction::Continue
            } else {
                state
                    .guards
                    .check_trajectory_ahead(&[(call.name.as_str(), call.arguments.as_str())])
            };
            let trajectory_message: Option<String> = match &trajectory_action {
                crate::guard::GuardAction::Block { message, .. } => {
                    tracing::warn!(
                        tool = %call.name,
                        args = %call.arguments,
                        "text-fallback call blocked by trajectory guard before execution"
                    );
                    Some(message.clone())
                }
                _ => None,
            };
            let call_id = format!("call_{}", uuid::Uuid::new_v4());
            on_event(AgentEvent::ToolCall {
                id: call_id.clone(),
                name: call.name.clone(),
                arguments: call.arguments.clone(),
            });
            // Signature-level loop guard: short-circuit a blocked call before it
            // executes (ADR-0036). Same contract as the native path — the model
            // gets an explanatory error instead of the content. Covers any tool
            // masked by either the read-loop guard or the doom guard above.
            let guard_blocked = state.guards.is_blocked(&call.name, &call.arguments);
            let result = if checkpoint_replay {
                tracing::warn!(
                    tool = %call.name,
                    args = %call.arguments,
                    "provider retry repeated a completed text-fallback tool call"
                );
                let output = ToolOutput::Text(format!(
                    "[retry checkpoint] This exact {} call already completed before the \
                     provider retry. Its result is present earlier in the conversation and \
                     remains authoritative. The tool was not executed again.",
                    call.name,
                ));
                on_event(AgentEvent::ToolResult {
                    id: call_id.clone(),
                    name: call.name.clone(),
                    output: output.to_text(),
                    structured: output.clone(),
                    duration_ms: 0,
                });
                output
            } else if guard_blocked {
                tracing::warn!(
                    tool = %call.name,
                    args = %call.arguments,
                    "text-fallback call blocked by turn-loop guard signature mask"
                );
                on_event(AgentEvent::Notice(
                    AgentNotice::new(
                        NoticeKind::NudgeInjected,
                        NoticeSeverity::Warning,
                        "Blocked repeating tool call",
                        NoticeSource::TurnGuard,
                    )
                    .with_body(format!(
                        "A tool call ({}) was blocked by the loop guard — it is a repeat \
                         of a call already issued this round. Use the result already in \
                         context, or try a different call.",
                        call.name,
                    ))
                    .with_surface(NoticeSurface::Toast),
                ));
                let output = ToolOutput::Text(format!(
                    "[loop guard] This call ({}) is blocked for the rest of the turn \
                     because it was a repeat of one already issued this round. Re-running it \
                     cannot help: the result is already in context above. Act on it now \
                     (use what you already have, try a *different* command/file/query), or, \
                     if you cannot proceed, say so explicitly or call `abort`.",
                    call.name,
                ));
                on_event(AgentEvent::ToolResult {
                    id: call_id.clone(),
                    name: call.name.clone(),
                    output: output.to_text(),
                    structured: output.clone(),
                    duration_ms: 0,
                });
                output
            } else {
                let outcome = self
                    .execute_tool_evented(&call, &call_id, cancel, on_event)
                    .await?;
                if outcome.interrupted {
                    // Record a drained result (an interrupted subagent's partial
                    // transcript) before ending the round as interrupted.
                    if let Some(result) = outcome.result {
                        let duration_ms = std::time::Instant::now().elapsed().as_millis() as u64;
                        self.record_tool_result(
                            ToolResultRecord {
                                call: &call,
                                call_id: &call_id,
                                result: &result,
                                duration_ms,
                                checkpoint_replay,
                                emit_event: true,
                            },
                            messages,
                            state,
                            on_event,
                        );
                    }
                    return Err(HarnessError::Interrupted);
                }
                match outcome.result {
                    Some(result) => result,
                    // Defensive: a non-interrupted outcome must carry a
                    // result — the executor guarantees it. Surface an
                    // internal error rather than panicking the harness.
                    None => {
                        return Err(HarnessError::Other(
                            "internal error: non-interrupted tool outcome lost its result"
                                .to_string(),
                        ));
                    }
                }
            };
            if !checkpoint_replay && !guard_blocked {
                state.remember_completed_tool(&call);
            }
            let denied = matches!(result, ToolOutput::PermissionDenied { .. });
            let duration_ms = std::time::Instant::now().elapsed().as_millis() as u64;
            self.record_tool_result(
                ToolResultRecord {
                    call: &call,
                    call_id: &call_id,
                    result: &result,
                    duration_ms,
                    checkpoint_replay,
                    emit_event: true,
                },
                messages,
                state,
                on_event,
            );
            if !checkpoint_replay {
                self.run_post_tool_hooks(&call, &result, duration_ms, messages)
                    .await;
            }
            if let Some(message) = trajectory_message {
                messages.push(crate::conversation_context::hidden_user(
                    InjectionKind::LoopReviewNudge,
                    message,
                ));
            }
            return Ok(!denied);
        }

        Ok(false)
    }

    /// Account for, surface, and persist a single tool result.
    pub(crate) fn record_tool_result<F>(
        &self,
        record: ToolResultRecord<'_>,
        messages: &mut Vec<Message>,
        state: &mut RoundState,
        on_event: &mut F,
    ) where
        F: FnMut(AgentEvent) + Send,
    {
        let ToolResultRecord {
            call,
            call_id,
            result,
            duration_ms,
            checkpoint_replay,
            emit_event,
        } = record;
        let text = result.to_text();
        // Cost attribution: a subagent's true token consumption can be 100x
        // the byte-estimate of its final summary, so accumulate the real
        // `TokenUsage` it reported. For every other tool the byte-estimate
        // remains the only signal we have.
        if checkpoint_replay {
            // The short checkpoint reference is new model-visible context,
            // but the original tool (especially a subagent) did no new work, so
            // do not attribute its nested usage a second time.
            state.token_usage.total_tokens += pressure::estimate_string_tokens(&text);
        } else if let Some((_sub_messages, sub_usage)) = result.subagent_payload() {
            state.token_usage.total_tokens += sub_usage.total_tokens;
            state.token_usage.prompt_tokens += sub_usage.prompt_tokens;
            state.token_usage.completion_tokens += sub_usage.completion_tokens;
            // The subagent's output tokens are in the numerator above; fold its
            // own generation time into the denominator too, so the round's
            // throughput stays scoped-consistent (no inflated tok/s for
            // delegating rounds). Tool execution inside the subagent is already
            // excluded from this figure.
            state.generation_ms = state
                .generation_ms
                .saturating_add(result.subagent_generation_ms());
            // Still count the summary bytes that the parent model will
            // actually re-read on the next turn.
            state.token_usage.total_tokens += pressure::estimate_string_tokens(&text);
        } else {
            state.token_usage.total_tokens += pressure::estimate_string_tokens(&text);
        }
        tracing::info!(
            tool = %call.name,
            duration_ms,
            bytes = text.len(),
            checkpoint_replay,
            "tool result"
        );
        if !checkpoint_replay {
            self.emit_todos_change(call, on_event);
        }
        if emit_event {
            on_event(AgentEvent::ToolResult {
                id: call_id.to_string(),
                name: call.name.clone(),
                output: text.clone(),
                structured: result.clone(),
                duration_ms,
            });
        }
        // For subagent results, attach the nested transcript as `children` on
        // the persisted Tool-role message so resume can rebuild the subagent
        // view without a live event stream. The nested `Message`s already
        // self-contain their own tool_calls / tool_call_id / children, so
        // arbitrarily deep subagent trees round-trip through session.json.
        // Sidecar `subagent_meta` captures what the live event stream knew but
        // the bare transcript cannot reconstruct on resume: duration, the
        // task description, the toolset size, and explicit failed /
        // interrupted flags. The subagent result text is built by
        // `subagent_result_text`, which appends a deterministic
        // role-reanchoring note at this single choke point (see its doc for
        // the "role bleed" rationale). For non-subagent results the plain header
        // is used unchanged.
        let tool_message = match result.subagent_payload() {
            Some((sub_messages, _)) => {
                let meta = crate::message::SubagentMeta {
                    duration_ms: Some(duration_ms),
                    failed: result.is_error(),
                    interrupted: result.subagent_interrupted(),
                    ..Default::default()
                };
                Message::tool_result(
                    call,
                    subagent_result_text(
                        &call.name,
                        &text,
                        result.is_error(),
                        result.subagent_interrupted(),
                    ),
                )
                .with_children(sub_messages.to_vec())
                .with_subagent_meta(meta)
            }
            None => {
                let formatted = format!("[{} result]:\n{}", call.name, text);
                let message_content = if let ToolOutput::Shell {
                    truncated: true, ..
                } = result
                {
                    // ADR-0264: If the shell command output was truncated, ensure the model message
                    // contains the explicit call: handle so the model can inspect offstream immediately.
                    format!(
                        "{formatted}\n\n[Epistemic virtual memory: inspect full output with handle \"call:{call_id}\"]"
                    )
                } else {
                    formatted
                };
                Message::tool_result(call, message_content)
            }
        };
        messages.push(tool_message);

        // Image peel-out (mirrors opencode's OpenAI-Chat lowering). The tool
        // message only carries text (OpenAI Chat Completions requires tool
        // content to be a string), so the actual image is injected as a
        // follow-up user-role message with the image attached — the same
        // channel paste-up uses. The provider serialises it to `image_url`
        // (OpenAI-compat) / `inline_data` (Google), letting the model see the
        // pixels. A short textual link ties the two messages together.
        if let ToolOutput::Image { mime, data } = result {
            messages.push(crate::conversation_context::tool_image(
                &call.name,
                mime.clone(),
                data.clone(),
            ));
        }
    }
}
