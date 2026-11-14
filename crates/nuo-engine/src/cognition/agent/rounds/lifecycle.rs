use super::*;

/// ADR-0026: tool-call input progress is reported only when the argument-byte
/// count advances by this step, never per fragment
/// ([INV-STREAM-TOOL-04]). Dispatch still waits for the whole object after the
/// stream ends ([INV-STREAM-TOOL-01]).
const TOOL_INPUT_PROGRESS_STEP: usize = 4096;

/// Mutable state for one provider-stream attempt.
///
/// Grouping the accumulators, loop detectors, announcement bookkeeping, and
/// finish-drain deadline keeps the ingestion helpers' signatures small and
/// makes the attempt's lifetime explicit: a retry builds a fresh value, so no
/// detector, id, or announcement leaks across transport attempts.
struct StreamAttempt {
    content: String,
    reasoning_content: String,
    calls: Vec<ToolCall>,
    emitted_text: bool,
    emitted_reasoning: bool,
    /// `Usage` remains a progress signal for protocols that report it before
    /// their terminal frame. Provider-owned response state is accepted only
    /// from the unique `Completed` event.
    streamed_usage: Option<TokenUsage>,
    completion_meta: Option<nuo_protocol::ProviderCompletionMeta>,
    text_loop_detector: nuo_guardrails::stream_loop_detector::StreamLoopDetector,
    reasoning_loop_detector: nuo_guardrails::stream_loop_detector::StreamLoopDetector,
    tool_loop_detector: nuo_guardrails::stream_loop_detector::StreamLoopDetector,
    stream_loop_incident: Option<StreamLoopIncident>,
    /// Bounded evidence tail for the Stream Sentinel when the tool-call
    /// argument channel is the one looping.
    tool_argument_text: String,
    cognitive_cleared_candidates: std::collections::HashSet<String>,
    /// Cognitive loop reviews run detached from this loop: the stream keeps
    /// rendering while the cognitive pipeline deliberates, and the verdict is
    /// applied at the next event boundary. At most one consult is in flight at
    /// a time; the level-triggered detector re-fires after an acquittal, so a
    /// still-looping stream is never left unreviewed.
    pending_stream_loop_review: Option<PendingStreamLoopReview>,
    /// Finish-drain deadline (FINISH_DRAIN_GRACE). Once this turn's stream has
    /// produced at least one delta, a cancellation no longer wins the biased
    /// select outright: the stream gets one short, strictly bounded window to
    /// reach its natural end. A model that finished its answer right as the
    /// user sent the next message (or hit Esc Esc) then commits that answer as
    /// a completed round instead of unwinding as `Interrupted` after every
    /// delta was already rendered — the false "▲ interrupted | new message"
    /// marker over a round the user watched finish. A stream that is still
    /// silent when the deadline expires was not settling: the interrupt stands,
    /// so an interrupt is delayed by at most FINISH_DRAIN_GRACE and a
    /// genuinely mid-generation answer is still cut.
    finish_drain: Option<std::time::Instant>,
    turn_streamed: bool,
    /// ADR-0166: provider slots whose call was announced to the frontend and
    /// not yet discharged by dispatch. The identity the frontend keys the row
    /// by is `calls[slot].id`; this set only answers "did this attempt announce
    /// a row that dispatch never reached?" A retry announces fresh calls (fresh
    /// ids), so the previous attempt's rows are retracted rather than re-used.
    announced_slots: std::collections::BTreeSet<usize>,
    reported_tool_input_bytes: std::collections::HashMap<usize, usize>,
}

impl StreamAttempt {
    fn new() -> Self {
        Self {
            content: String::new(),
            reasoning_content: String::new(),
            calls: Vec::new(),
            emitted_text: false,
            emitted_reasoning: false,
            streamed_usage: None,
            completion_meta: None,
            text_loop_detector: nuo_guardrails::stream_loop_detector::StreamLoopDetector::new(1024),
            reasoning_loop_detector: nuo_guardrails::stream_loop_detector::StreamLoopDetector::new(
                1024,
            ),
            tool_loop_detector: nuo_guardrails::stream_loop_detector::StreamLoopDetector::new(1024),
            stream_loop_incident: None,
            tool_argument_text: String::new(),
            cognitive_cleared_candidates: std::collections::HashSet::new(),
            pending_stream_loop_review: None,
            finish_drain: None,
            turn_streamed: false,
            announced_slots: std::collections::BTreeSet::new(),
            reported_tool_input_bytes: std::collections::HashMap::new(),
        }
    }

    /// Consume one provider response, applying cancellation/drain, loop
    /// verdicts, and delta ingestion until the stream ends or is cut.
    async fn run<F>(
        &mut self,
        agent: &Agent,
        mut stream: nuo_protocol::ProviderEventStream,
        messages: &[Message],
        cancel: &CancellationToken,
        request_accounting: &mut RequestAccountingGuard,
        on_event: &mut F,
    ) -> Result<(), HarnessError>
    where
        F: FnMut(AgentEvent) + Send,
    {
        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled(), if self.finish_drain.is_none() => {
                    self.enter_finish_drain()?;
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
                    match self.pending_stream_loop_review.as_mut() {
                        Some(review) => {
                            (&mut review.verdict_rx)
                                .await
                                .unwrap_or(nuo_protocol::StreamLoopVerdict::No)
                        }
                        // Unreachable behind the branch guard; stay
                        // pending rather than panicking.
                        None => std::future::pending().await,
                    }
                }, if self.pending_stream_loop_review.is_some() => {
                    if self.apply_stream_loop_verdict(verdict) {
                        break;
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
                    self.finish_drain.map_or(STREAM_IDLE_TIMEOUT, |deadline| {
                        deadline.saturating_duration_since(std::time::Instant::now())
                    }),
                    stream.next(),
                ) => {
                    let event = match event {
                        Ok(Some(event)) => event,
                        Ok(None) => {
                            request_accounting.mark_stream_end();
                            break;
                        }
                        Err(_elapsed) if self.finish_drain.is_some() => {
                            // The settle window expired with the stream
                            // neither ended nor producing a chunk: the
                            // answer was not finishing after all. Honour
                            // the interrupt.
                            retract_announced_calls(
                                &mut self.announced_slots,
                                &self.calls,
                                on_event,
                            );
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
                            retract_announced_calls(
                                &mut self.announced_slots,
                                &self.calls,
                                on_event,
                            );
                            return Err(HarnessError::Provider(nuo_protocol::ProviderError::new("harness", nuo_protocol::ProviderErrorKind::Timeout, err_msg).retryable(None)));
                        }
                    };
                    let event = match event {
                        Ok(event) => event,
                        Err(error) => {
                            request_accounting.record_error(error.to_string());
                            retract_announced_calls(
                                &mut self.announced_slots,
                                &self.calls,
                                on_event,
                            );
                            return Err(HarnessError::from(error));
                        }
                    };
                    // Content, tool-call bytes, usage, or completion proves
                    // that this turn was delivering. Transport control
                    // metadata does not: a catalog ETag arriving before any
                    // model output must not convert an immediate interrupt
                    // into a settle-window drain.
                    if !matches!(&event, ProviderStreamEvent::ModelCatalogEtag(_)) {
                        self.turn_streamed = true;
                    }
                    if self.completion_meta.is_some() {
                        let err_msg = "Provider emitted data after the terminal completion event."
                            .to_string();
                        request_accounting.record_error(&err_msg);
                        retract_announced_calls(
                            &mut self.announced_slots,
                            &self.calls,
                            on_event,
                        );
                        return Err(HarnessError::Provider(nuo_protocol::ProviderError::new("harness", nuo_protocol::ProviderErrorKind::Protocol, err_msg).retryable(None)));
                    }
                    request_accounting.observe_stream_event(
                        &event,
                        std::time::Instant::now(),
                    );
                    self.ingest_event(agent, event, messages, on_event).await?;
                }
            }
        }
        Ok(())
    }

    /// Handle a cancellation that lands while the stream is open.
    ///
    /// An attempt that produced nothing is cut immediately. One that already
    /// produced output gets the bounded finish-drain window instead: the
    /// stream may be settling right as the user interrupted, and committing
    /// that answer beats unwinding after every delta was already rendered.
    fn enter_finish_drain(&mut self) -> Result<(), HarnessError> {
        if !self.turn_streamed {
            return Err(HarnessError::Interrupted);
        }
        // The stream already produced output this turn; open the bounded
        // settle window instead of cutting now.
        self.finish_drain = Some(std::time::Instant::now() + crate::FINISH_DRAIN_GRACE);
        Ok(())
    }

    /// Apply one provider stream event to this attempt's accumulators.
    async fn ingest_event<F>(
        &mut self,
        agent: &Agent,
        event: ProviderStreamEvent,
        messages: &[Message],
        on_event: &mut F,
    ) -> Result<(), HarnessError>
    where
        F: FnMut(AgentEvent) + Send,
    {
        match event {
            ProviderStreamEvent::ModelCatalogEtag(etag) => {
                let connection_id = agent.provider.provider_id();
                let outcome =
                    nuo_model_routing::refresh_connection_models_for_etag(&connection_id, &etag)
                        .await;
                if outcome.changed {
                    nuo_model_routing::sync_fitted_model_registry();
                    nuo_model_routing::prune_stale_models_on_disk();
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
                self.ingest_text_delta(agent, delta, messages, on_event);
            }
            ProviderStreamEvent::ReasoningDelta(delta) => {
                self.ingest_reasoning_delta(agent, delta, messages, on_event);
            }
            ProviderStreamEvent::ToolCallDelta {
                index,
                id,
                name,
                arguments,
            } => {
                self.ingest_tool_call_delta(agent, index, id, name, arguments, messages, on_event);
            }
            ProviderStreamEvent::Usage(usage) => {
                // Take the last reported usage (providers may emit one final
                // usage chunk). Prefer it over the local estimate at booking
                // time.
                self.streamed_usage = Some(usage);
            }
            ProviderStreamEvent::Completed(meta) => {
                if let Some(usage) = meta.usage {
                    self.streamed_usage = Some(usage);
                }
                self.completion_meta = Some(meta);
            }
        }
        Ok(())
    }

    /// Append assistant text, emit its delta, and run the text-loop detector.
    fn ingest_text_delta<F>(
        &mut self,
        agent: &Agent,
        delta: String,
        messages: &[Message],
        on_event: &mut F,
    ) where
        F: FnMut(AgentEvent) + Send,
    {
        self.content.push_str(&delta);
        on_event(AgentEvent::AssistantDelta {
            delta: delta.clone(),
            start: !self.emitted_text,
        });
        self.emitted_text = true;
        if let Some(pat) = self.text_loop_detector.push_and_check(&delta) {
            // Async consult: keep consuming deltas
            // while the Stream Sentinel deliberates.
            agent.dispatch_stream_loop_candidate(
                &mut self.text_loop_detector,
                messages,
                &self.content,
                &self.reasoning_content,
                &self.tool_argument_text,
                nuo_protocol::StreamLoopChannel::AssistantText,
                pat,
                &self.cognitive_cleared_candidates,
                &mut self.pending_stream_loop_review,
            );
        }
    }

    /// Append reasoning text, emit its delta, and run the reasoning-loop
    /// detector.
    fn ingest_reasoning_delta<F>(
        &mut self,
        agent: &Agent,
        delta: String,
        messages: &[Message],
        on_event: &mut F,
    ) where
        F: FnMut(AgentEvent) + Send,
    {
        self.reasoning_content.push_str(&delta);
        on_event(AgentEvent::ReasoningDelta {
            delta: delta.clone(),
            start: !self.emitted_reasoning,
        });
        self.emitted_reasoning = true;
        if let Some(pat) = self.reasoning_loop_detector.push_and_check(&delta) {
            // Async consult: keep consuming deltas
            // while the cognitive reviewer deliberates.
            agent.dispatch_stream_loop_candidate(
                &mut self.reasoning_loop_detector,
                messages,
                &self.content,
                &self.reasoning_content,
                &self.tool_argument_text,
                nuo_protocol::StreamLoopChannel::Reasoning,
                pat,
                &self.cognitive_cleared_candidates,
                &mut self.pending_stream_loop_review,
            );
        }
    }

    /// Accumulate one tool-call fragment, announce the slot once it is named,
    /// and report throttled count-only progress.
    #[allow(clippy::too_many_arguments)]
    fn ingest_tool_call_delta<F>(
        &mut self,
        agent: &Agent,
        index: usize,
        id: Option<String>,
        name: Option<String>,
        arguments: String,
        messages: &[Message],
        on_event: &mut F,
    ) where
        F: FnMut(AgentEvent) + Send,
    {
        while self.calls.len() <= index {
            self.calls.push(ToolCall {
                id: String::new(),
                name: String::new(),
                arguments: String::new(),
            });
        }
        let call = &mut self.calls[index];
        // ADR-0166: the provider's id is a scalar, not a fragment stream —
        // every lane that supplies one supplies it whole, alongside the name.
        // Freeze the first non-empty value so a provider that repeats it cannot
        // grow a garbled identity that no longer matches the announcement
        // already on the wire.
        if call.id.is_empty() {
            if let Some(id) = id.filter(|id| !id.is_empty()) {
                call.id = id;
            }
        } else if let Some(late) = id
            && !late.is_empty()
            && late != call.id
        {
            // This lane's wire shape is outside what the workspace models: the
            // announcement (and the persisted `tool_call_id`) already carry the
            // frozen id. Re-keying a row the frontend holds is strictly worse,
            // so keep the frozen identity and make the anomaly visible instead
            // of dropping it silently.
            tracing::warn!(
                tool = %call.name,
                frozen_id = %call.id,
                late_id = %late,
                "provider supplied a tool-call id after the call was \
                 announced; keeping the announced id"
            );
        }
        if let Some(name) = name {
            call.name.push_str(&name);
        }
        call.arguments.push_str(&arguments);
        push_bounded_tail(
            &mut self.tool_argument_text,
            &arguments,
            TOOL_INPUT_EVIDENCE_CHARS,
        );
        if !arguments.is_empty()
            && let Some(pat) = self.tool_loop_detector.push_and_check(&arguments)
        {
            agent.dispatch_stream_loop_candidate(
                &mut self.tool_loop_detector,
                messages,
                &self.content,
                &self.reasoning_content,
                &self.tool_argument_text,
                nuo_protocol::StreamLoopChannel::ToolCall,
                pat,
                &self.cognitive_cleared_candidates,
                &mut self.pending_stream_loop_review,
            );
        }
        // ADR-0026 / ADR-0166: the moment this slot first has a name, announce
        // the call so the frontend can show a running step and the tool phase
        // while the arguments keep streaming. The announcement carries the
        // call's single identity, minted here if the provider supplied none, so
        // every later event (progress, dispatch, stream, result, retraction)
        // addresses the same row. The execution gate is untouched — dispatch
        // still waits for the whole object after the stream ends.
        if !self.announced_slots.contains(&index) && !call.name.is_empty() {
            if call.id.is_empty() {
                call.id = format!("call_{}", uuid::Uuid::new_v4());
            }
            self.announced_slots.insert(index);
            on_event(AgentEvent::ToolCallStarted {
                id: call.id.clone(),
                name: call.name.clone(),
            });
        }
        // Count-only progress, throttled to a bounded cadence — never per
        // fragment, never the bytes ([INV-STREAM-TOOL-03] /
        // [INV-STREAM-TOOL-04]).
        let total = call.arguments.len();
        let reported = self
            .reported_tool_input_bytes
            .get(&index)
            .copied()
            .unwrap_or(0);
        if total.saturating_sub(reported) >= TOOL_INPUT_PROGRESS_STEP {
            self.reported_tool_input_bytes.insert(index, total);
            if self.announced_slots.contains(&index) {
                on_event(AgentEvent::ToolInputProgress {
                    id: call.id.clone(),
                    bytes: total,
                    arguments: Some(call.arguments.clone()),
                });
            }
        }
    }

    /// Apply a Stream Sentinel verdict that landed at an event boundary.
    ///
    /// Returns `true` when the stream must be cut; `false` means the candidate
    /// was acquitted, its detector re-armed, and consumption should continue.
    fn apply_stream_loop_verdict(&mut self, verdict: nuo_protocol::StreamLoopVerdict) -> bool {
        let Some(review) = self.pending_stream_loop_review.take() else {
            return false;
        };
        tracing::debug!(
            channel = ?review.channel,
            pattern = %review.pattern.description(),
            verdict = ?verdict,
            "stream-loop candidate reviewed by cognitive pipeline"
        );
        if verdict.is_loop() {
            match review.channel {
                nuo_protocol::StreamLoopChannel::AssistantText => {
                    tracing::warn!(
                        pattern = %review.pattern.description(),
                        "Cognitive pipeline confirmed in-flight text stream loop; aborting stream early"
                    );
                }
                nuo_protocol::StreamLoopChannel::Reasoning => {
                    tracing::warn!(
                        pattern = %review.pattern.description(),
                        "Cognitive pipeline confirmed in-flight reasoning stream loop; aborting stream early"
                    );
                }
                nuo_protocol::StreamLoopChannel::ToolCall => {
                    tracing::warn!(
                        pattern = %review.pattern.description(),
                        "Cognitive pipeline confirmed in-flight tool-argument stream loop; aborting stream early"
                    );
                }
            }
            self.stream_loop_incident = Some(StreamLoopIncident {
                pattern: review.pattern,
                channel: review.channel,
            });
            return true;
        }
        self.cognitive_cleared_candidates
            .insert(review.candidate_key);
        match review.channel {
            nuo_protocol::StreamLoopChannel::AssistantText => {
                self.text_loop_detector.reset();
            }
            nuo_protocol::StreamLoopChannel::Reasoning => {
                self.reasoning_loop_detector.reset();
            }
            nuo_protocol::StreamLoopChannel::ToolCall => {
                self.tool_loop_detector.reset();
            }
        }
        false
    }

    /// Settle a Stream Sentinel consult still in flight when the stream ended
    /// naturally.
    ///
    /// A consult still in flight when the stream ended naturally still owes a
    /// verdict: settling it here (bounded by the consult's own 2s timeout)
    /// preserves the blocking consult's guarantee — a confirmed loop is
    /// trimmed, disclosed, and recovered instead of silently committing the
    /// degenerate tail. It must settle **before** the strict tool-call
    /// finalization in [`Self::validate_stream_finalization`]: a confirmed
    /// tool-argument loop explains exactly why the captured arguments are
    /// truncated, and judging them as a transport failure first would turn the
    /// intervention into a retry storm. When the verdict is `no`, the
    /// validation still reports truncation.
    async fn settle_pending_review(&mut self) {
        if self.stream_loop_incident.is_none()
            && let Some(review) = self.pending_stream_loop_review.take()
        {
            let verdict = review
                .verdict_rx
                .await
                .unwrap_or(nuo_protocol::StreamLoopVerdict::No);
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
                self.stream_loop_incident = Some(StreamLoopIncident {
                    pattern: review.pattern,
                    channel: review.channel,
                });
            } else {
                self.cognitive_cleared_candidates
                    .insert(review.candidate_key);
            }
        }
    }

    /// Enforce strict stream finalization (the same discipline praxion's
    /// accumulator enforces at `finish()`): the stream must not end
    /// mid-tool-call, and it must have delivered a terminal completion event.
    ///
    /// A slot that accumulated id/argument bytes but never received a name is
    /// the residue of a truncated stream — dropping it silently would mistake a
    /// connection failure for the model's intent. Surface it as retryable so
    /// the idempotent request retry re-runs the turn instead of committing a
    /// partial response. Slots that stayed completely empty (a provider delta
    /// that carried only an index) are still dropped below. A confirmed loop
    /// or an oversized batch explains its own truncation: those fragments are
    /// already known not to be executable intent, and the intervention below
    /// discards them. Do not misclassify them as a transport truncation.
    fn validate_stream_finalization<F>(
        &mut self,
        request_accounting: &mut RequestAccountingGuard,
        on_event: &mut F,
    ) -> Result<(), HarnessError>
    where
        F: FnMut(AgentEvent),
    {
        if self.stream_loop_incident.is_none() {
            for call in &self.calls {
                if call.name.is_empty() && (!call.id.is_empty() || !call.arguments.is_empty()) {
                    let err_msg =
                        "Provider stream ended mid-tool-call; the response was likely truncated."
                            .to_string();
                    request_accounting.record_error(&err_msg);
                    retract_announced_calls(&mut self.announced_slots, &self.calls, on_event);
                    return Err(HarnessError::Provider(
                        nuo_protocol::ProviderError::new(
                            "harness",
                            nuo_protocol::ProviderErrorKind::Transport,
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
                    retract_announced_calls(&mut self.announced_slots, &self.calls, on_event);
                    return Err(HarnessError::Provider(
                        nuo_protocol::ProviderError::new(
                            "harness",
                            nuo_protocol::ProviderErrorKind::Transport,
                            err_msg,
                        )
                        .retryable(None),
                    ));
                }
            }
        }

        if self.completion_meta.is_none() && self.stream_loop_incident.is_none() {
            let err_msg = "Provider stream ended without a terminal completion event; response state was not committed."
                .to_string();
            request_accounting.record_error(&err_msg);
            retract_announced_calls(&mut self.announced_slots, &self.calls, on_event);
            return Err(HarnessError::Provider(
                nuo_protocol::ProviderError::new(
                    "harness",
                    nuo_protocol::ProviderErrorKind::Transport,
                    err_msg,
                )
                .retryable(None),
            ));
        }
        Ok(())
    }

    /// ADR-0166 `[INV-TOOL-ID-01]`: one id identifies one call.
    ///
    /// A provider that reuses an id within a single response makes two calls
    /// indistinguishable to *every* consumer — the transcript row, the dispatch
    /// slot, the persisted `tool_call_id` — so one of them would silently wear
    /// the other's result. That is a protocol violation, not something to merge
    /// away: refuse the attempt.
    fn reject_duplicate_tool_call_ids<F>(
        &mut self,
        request_accounting: &mut RequestAccountingGuard,
        on_event: &mut F,
    ) -> Result<(), HarnessError>
    where
        F: FnMut(AgentEvent),
    {
        let mut seen = std::collections::HashSet::with_capacity(self.calls.len());
        if let Some(duplicate) = self
            .calls
            .iter()
            .filter(|call| !call.id.is_empty())
            .find(|call| !seen.insert(call.id.as_str()))
        {
            let err_msg = format!(
                "Provider returned two tool calls with the same id `{}`; a tool-call \
                 id must identify exactly one call.",
                duplicate.id
            );
            request_accounting.record_error(&err_msg);
            retract_announced_calls(&mut self.announced_slots, &self.calls, on_event);
            return Err(HarnessError::Provider(nuo_protocol::ProviderError::new(
                "harness",
                nuo_protocol::ProviderErrorKind::Protocol,
                err_msg,
            )));
        }
        Ok(())
    }
}

/// The accounting guard, live event stream, and request identity for one
/// provider attempt. The provider/model are captured at request time so the
/// assistant message is stamped with the identity the request actually ran
/// against, even if the proxy's active connection changes mid-stream.
struct StreamingAttemptStart {
    accounting: RequestAccountingGuard,
    stream: nuo_protocol::ProviderEventStream,
    provider: String,
    model: String,
}

/// Recovery decision attached to a cut or naturally-looped stream.
struct StreamLoopSettlement {
    notice: Option<AgentNotice>,
    recovery_prompt: Option<String>,
    hard_stopped: bool,
}

/// How a completed assistant turn leaves the round-exit gate.
enum TurnExit {
    /// Another ReAct turn (loop recovery, stop hook, or follow-up input).
    Continue,
    /// The user round is complete; carries the duration sampled before the
    /// follow-up drain.
    Finish(u64),
}

impl Agent {
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
        self.queues
            .round_paused_ms
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
                .queues
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
    /// path. Unlike [`Self::begin_streaming_round`] this does not
    /// restart the round from scratch: it re-seeds the ReAct state from the
    /// durable [`RetryPoint`] captured when the round stopped, so the resumed
    /// execution
    ///
    /// - keeps numbering turns from `turns_committed` (the transcript's
    ///   `round N | turn M` sequence stays unbroken — turn M+1 follows the
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
        point: &nuo_protocol::RetryPoint,
    ) -> StreamingRoundState {
        self.queues
            .round_paused_ms
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
                .queues
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

    /// Thin orchestrator for one streaming round: prepare or resume the pending
    /// provider request, drive exactly one stream attempt, settle its terminal
    /// state, and let the round-exit gate decide whether another turn follows.
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
                self.prepare_pending_stream_request(messages, cancel, round, &mut on_event)
                    .await?;
            }

            let StreamingAttemptStart {
                mut accounting,
                stream,
                provider,
                model,
            } = self
                .open_streaming_attempt(round, resuming_provider_request, cancel, &mut on_event)
                .await?;
            let mut attempt = self
                .drive_provider_stream(stream, messages, cancel, &mut accounting, &mut on_event)
                .await?;

            let mut loop_settlement = self.settle_stream_loop_cut(round, &mut attempt);

            // A loop cut can leave no assistant text at all — a pure
            // tool-call response. There is then no valid assistant message to
            // commit, so the aborted attempt never existed: the directive
            // becomes the next turn's input.
            let cut_without_content =
                attempt.stream_loop_incident.is_some() && attempt.content.trim().is_empty();
            if cut_without_content {
                // ADR-0166: this attempt is over, so anything it announced and
                // never dispatched must go — including on the hard-stop path,
                // which used to return before the retraction below and leave the
                // frontend holding rows that could never finish.
                retract_announced_calls(
                    &mut attempt.announced_slots,
                    &attempt.calls,
                    &mut on_event,
                );
                if loop_settlement.hard_stopped {
                    return Err(HarnessError::Other(
                        "Generation was stopped after a repeated intervention; no assistant \
                         message was committed."
                            .to_string(),
                    ));
                }
                if let Some(notice) = loop_settlement.notice.take() {
                    on_event(AgentEvent::Notice(notice));
                }
                if let Some(prompt) = loop_settlement.recovery_prompt.take() {
                    messages.push(crate::runtime::message::hidden_user(
                        InjectionKind::LoopReviewNudge,
                        prompt,
                    ));
                }
                round.pending_request = None;
                round.turn_index += 1;
                self.set_current_turn(round.turn_index as u64);
                if self.check_hard_stop(round.turn_index).is_break() {
                    return Err(self.hard_stop_error());
                }
                self.fire_turn_persist(messages).await?;
                self.run_turn_hooks(messages, &round.state, round.turn_index)
                    .await;
                self.restore_scoped_turn_end();
                continue;
            }

            // A cut stream's tool-call fragments are never executable intent, so
            // anything it announced is retracted here — before the recovery
            // request announces its own calls. The retraction runs **before**
            // the fragments are dropped: the ids it names live on those calls.
            if attempt.stream_loop_incident.is_some() {
                retract_announced_calls(
                    &mut attempt.announced_slots,
                    &attempt.calls,
                    &mut on_event,
                );
                attempt.calls.clear();
            }

            if attempt.emitted_text {
                on_event(AgentEvent::AssistantEnd(attempt.content.clone()));
            }
            if attempt.emitted_reasoning {
                on_event(AgentEvent::ReasoningEnd(attempt.reasoning_content.clone()));
            }
            if let Some(notice) = loop_settlement.notice.take() {
                // Inline notices are retained in the transcript. A transient
                // toast is too easy to miss for an action that changed the
                // generated answer.
                on_event(AgentEvent::Notice(notice));
            }

            let response = self.finalize_assistant_response(
                &mut attempt,
                provider,
                model,
                &mut accounting,
                messages,
                &mut on_event,
            )?;

            // The request checkpoint is consumed only after a complete,
            // valid response is available. Any earlier return leaves it set
            // so orchestration can retry this exact request.
            round.pending_request = None;
            let performance =
                self.settle_turn_usage(round, &response, &mut attempt, &mut accounting);

            on_event(AgentEvent::TurnPerformance(performance));
            messages.push(response.clone());

            // `emitted_text` means assistant text was already streamed to the
            // UI; a text-fallback tool call must then retract it via a discard.
            let dispatched = self
                .dispatch_tool_calls(
                    &response,
                    messages,
                    &mut round.state,
                    attempt.emitted_text,
                    cancel,
                    &mut on_event,
                )
                .await;
            // The preflight emits a `ToolCall` for every retained call, so each
            // announcement is discharged here regardless of the dispatch
            // outcome. Clearing prevents a later abort from retracting rows the
            // frontend already collapsed onto live calls.
            attempt.announced_slots.clear();
            if dispatched? {
                round.turn_index += 1;
                self.set_current_turn(round.turn_index as u64);
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
            match self
                .finish_turn_or_continue(
                    &response,
                    messages,
                    round,
                    &mut loop_settlement,
                    &mut on_event,
                )
                .await?
            {
                TurnExit::Continue => continue,
                TurnExit::Finish(duration_ms) => {
                    // User-round end: clear every scoped disable so the toolset
                    // is fresh for the next user request. No persist here: the
                    // terminal round commit is the authority for the final
                    // answer (D5), and the mid-round save points have already
                    // persisted every continuing turn.
                    self.restore_scoped_round_end();
                    return Ok(RoundOutcome {
                        message: response,
                        token_usage: round.state.token_usage,
                        duration_ms,
                        generation_ms: round.state.generation_ms,
                        paused_ms: self
                            .queues
                            .round_paused_ms
                            .load(std::sync::atomic::Ordering::Relaxed),
                    });
                }
            }
        }
    }

    /// Assemble the next logical ReAct turn's provider request and checkpoint
    /// it in `round.pending_request`.
    ///
    /// Steering ops, mention injection, TurnStart hooks, and context projection
    /// all belong to the logical turn, not to each network attempt: running
    /// them once here is what lets a transport retry reuse the exact request
    /// without duplicating side effects.
    async fn prepare_pending_stream_request<F>(
        &self,
        messages: &mut Vec<Message>,
        cancel: &CancellationToken,
        round: &mut StreamingRoundState,
        on_event: &mut F,
    ) -> Result<(), HarnessError>
    where
        F: FnMut(AgentEvent) + Send,
    {
        // Apply steering ops queued since the last turn before preparing a new
        // provider request. Replies bypass this (see `drain_inbox`). A
        // transient retry deliberately skips this block: the already-prepared
        // request is the checkpoint.
        if !self.drain_inbox(&mut round.inbox_rx, messages) {
            return Err(HarnessError::Interrupted);
        }
        if self.drain_steering(
            round.session_queue_generation,
            self.round_count(),
            round.turn_index,
            messages,
            on_event,
        ) > 0
        {
            self.fire_turn_persist(messages).await?;
        }

        crate::cognition::conversation_context::inject_mentioned_skills(
            &self.context.skills_registry,
            messages,
        );
        // `@file:` injection happens exactly here — once per prompt, into the
        // live window — so the projected request (and every estimate built on
        // it) reuses the loaded bytes without ever touching the filesystem
        // again. Loading asynchronously keeps the turn-preparation path off
        // blocking reads.
        crate::cognition::conversation_context::inject_mentioned_files(
            self.workspace_root().as_deref(),
            messages,
        )
        .await;
        // TurnStart hooks belong to a logical ReAct turn, not to each network
        // attempt. Run them once before checkpointing the request so retries
        // cannot duplicate injected context or hook side effects.
        self.run_turn_start_hooks(messages, &round.state, round.turn_index)
            .await;
        // Publish the turn coordinate for the workspace journal's rows
        // (ADR-0125). Set here — once per logical ReAct turn, before any tool
        // dispatch in it — so a row's `turn_id` names the turn the mutation
        // happened in rather than always the round's first.
        self.set_current_turn(round.turn_index as u64);
        // Assemble once per turn, then gate on context pressure with the
        // assembled request (hot path): the estimate and the provider call
        // share one assembly. A projection that replaces the window
        // re-assembles exactly once.
        let mut request = self.model_request(messages);
        if self
            .project_context_if_needed(messages, &request, cancel)
            .await?
        {
            request = self.model_request(messages);
        }
        request.turn_context = Arc::clone(&round.turn_context);
        round.pending_request = Some(request);
        Ok(())
    }

    /// Emit request-boundary events, open the accounting attempt, enforce the
    /// local spend breaker, and race the provider handshake against
    /// cancellation and the idle timeout.
    async fn open_streaming_attempt<F>(
        &self,
        round: &mut StreamingRoundState,
        resuming_provider_request: bool,
        cancel: &CancellationToken,
        on_event: &mut F,
    ) -> Result<StreamingAttemptStart, HarnessError>
    where
        F: FnMut(AgentEvent),
    {
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
        // Archive the freshly assembled request's projection once
        // per logical invocation. A transport retry reuses `pending_request`
        // (so `resuming_provider_request` is true) and must not re-record.
        if !resuming_provider_request {
            self.fire_request_projection_persist(nuo_protocol::RequestProjection::from_request(
                request,
                request_estimate.temporary_context_tokens,
                self.round_count(),
                round.turn_index as u64,
                crate::runtime::time::unix_epoch_ms(),
            ));
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
            nuo_protocol::ContextTokenSnapshot::from_estimate(
                request_estimate,
                nuo_protocol::ContextTokenSource::Projection,
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
        if let (Some(ledger), Some(key)) = (&request_accounting.ledger, &request_accounting.key) {
            ledger
                .persist_request(key)
                .await
                .map_err(HarnessError::Other)?;
        }
        request_accounting.start_request();
        // The provider borrows the turn's assembled request and receives
        // this attempt's telemetry as a separate handle. A retry therefore
        // costs no copy of the conversation, and one attempt's timings can
        // never be attributed to another.
        let attempt_telemetry = request_accounting.transport_telemetry();

        self.enforce_local_spend_budget(&round.state, &mut request_accounting)?;

        let stream = self
            .open_provider_event_stream(request, attempt_telemetry, cancel, &mut request_accounting)
            .await?;
        Ok(StreamingAttemptStart {
            accounting: request_accounting,
            stream,
            provider: request_provider,
            model: request_model,
        })
    }

    /// ADR-0065: pre-flight local spend circuit breaker check.
    fn enforce_local_spend_budget(
        &self,
        state: &RoundState,
        request_accounting: &mut RequestAccountingGuard,
    ) -> Result<(), HarnessError> {
        let connection_id = self.provider.provider_id();
        if let Some(conn) = nuo_persistence::connections::Connections::load().get(&connection_id) {
            if let nuo_protocol::ConnectionBudget::Local { limit, period, .. } = &conn.budget {
                let current_spend = match period {
                    nuo_protocol::BudgetPeriod::Session => {
                        if state.spend > nuo_protocol::pricing::NanoUsd::ZERO {
                            state.spend
                        } else if let Some(pricing) = nuo_protocol::pricing::resolve_model_pricing(
                            &conn.provider,
                            &self.provider.model(),
                        ) {
                            pricing
                                .calculate_cost(
                                    &state.token_usage,
                                    nuo_protocol::pricing::RatingInstant::UNKNOWN,
                                )
                                .amount
                        } else {
                            nuo_protocol::pricing::NanoUsd::ZERO
                        }
                    }
                    nuo_protocol::BudgetPeriod::Daily | nuo_protocol::BudgetPeriod::Total => {
                        let total_nano = nuo_persistence::connection_usage::ConnectionUsage::load()
                            .connection_spend(&connection_id);
                        nuo_protocol::pricing::NanoUsd(total_nano)
                    }
                };

                if conn.budget.is_exceeded(current_spend) {
                    let err_msg = format!(
                        "[SPEND CIRCUIT BREAKER]: Connection `{}` reached its {} budget limit of {} (accumulated: {}). Execution halted to protect your account balance.",
                        connection_id,
                        period.label(),
                        limit.format_usd(),
                        current_spend.format_usd(),
                    );
                    request_accounting.record_error(&err_msg);
                    return Err(HarnessError::Provider(
                        nuo_protocol::ProviderError::new(
                            &connection_id,
                            nuo_protocol::ProviderErrorKind::QuotaExhausted,
                            err_msg,
                        )
                        .retryable(None),
                    ));
                }
            }
        }
        Ok(())
    }

    /// Race the model request against cancellation so an interrupt while we're
    /// waiting on the network resolves promptly instead of blocking until the
    /// first stream chunk arrives. The idle-timeout arm covers a provider
    /// endpoint that accepts the connection but never sends HTTP response
    /// headers (overloaded upstream, dropped proxy) — without it the select
    /// would hang forever on `.send()`.
    async fn open_provider_event_stream(
        &self,
        request: &nuo_protocol::ModelRequest,
        attempt_telemetry: nuo_protocol::TransportTelemetry,
        cancel: &CancellationToken,
        request_accounting: &mut RequestAccountingGuard,
    ) -> Result<nuo_protocol::ProviderEventStream, HarnessError> {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(HarnessError::Interrupted),
            result = tokio::time::timeout(
                STREAM_IDLE_TIMEOUT,
                self.provider.stream_chat_events(request, attempt_telemetry),
            ) => match result {
                Ok(Ok(stream)) => {
                    request_accounting.mark_stream_ready();
                    Ok(stream)
                },
                Ok(Err(error)) => {
                    let err_msg = error.to_string();
                    request_accounting.record_error(&err_msg);
                    Err(HarnessError::from(error))
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
                    Err(HarnessError::Provider(nuo_protocol::ProviderError::new("harness", nuo_protocol::ProviderErrorKind::Timeout, err_msg).retryable(None)))
                }
            },
        }
    }

    /// Drive one provider stream to its terminal state, then settle a pending
    /// Stream Sentinel consult and enforce the strict finalization gates.
    async fn drive_provider_stream<F>(
        &self,
        stream: nuo_protocol::ProviderEventStream,
        messages: &[Message],
        cancel: &CancellationToken,
        request_accounting: &mut RequestAccountingGuard,
        on_event: &mut F,
    ) -> Result<StreamAttempt, HarnessError>
    where
        F: FnMut(AgentEvent) + Send,
    {
        let mut attempt = StreamAttempt::new();
        attempt
            .run(self, stream, messages, cancel, request_accounting, on_event)
            .await?;
        request_accounting.mark_stream_end();

        attempt.settle_pending_review().await;
        attempt.validate_stream_finalization(request_accounting, on_event)?;
        attempt.reject_duplicate_tool_call_ids(request_accounting, on_event)?;
        Ok(attempt)
    }

    /// Trim a detected loop's degenerate tail and decide between one guided
    /// recovery and a hard stop.
    ///
    /// A detected loop cuts this provider stream, but it does not silently end
    /// the user's round. The first cutoff is a soft intervention: retain the
    /// partial response, visibly disclose the action, inject guidance, and make
    /// one fresh model request. If the recovery request loops too, stop after
    /// retaining its partial response and disclose the hard stop. Tool-call
    /// fragments from a cut stream are never executable intent.
    fn settle_stream_loop_cut(
        &self,
        round: &mut StreamingRoundState,
        attempt: &mut StreamAttempt,
    ) -> StreamLoopSettlement {
        let mut recovery_prompt = None;
        let mut hard_stopped = false;
        let notice = attempt.stream_loop_incident.as_ref().map(|incident| {
            match incident.channel {
                nuo_protocol::StreamLoopChannel::AssistantText => {
                    attempt.content =
                        nuo_guardrails::stream_loop_detector::StreamLoopDetector::trim_suffix(
                            &attempt.content,
                            &incident.pattern,
                        );
                }
                nuo_protocol::StreamLoopChannel::Reasoning => {
                    attempt.reasoning_content =
                        nuo_guardrails::stream_loop_detector::StreamLoopDetector::trim_suffix(
                            &attempt.reasoning_content,
                            &incident.pattern,
                        );
                }
                nuo_protocol::StreamLoopChannel::ToolCall => {
                    // Tool-call arguments are never rendered as text; the
                    // partial calls are dropped after their retraction below,
                    // which is the whole trimming this channel needs.
                }
            }

            if round.stream_loop_recoveries < MAX_STREAM_LOOP_RECOVERIES {
                round.stream_loop_recoveries += 1;
                let directive = match incident.channel {
                    nuo_protocol::StreamLoopChannel::AssistantText
                    | nuo_protocol::StreamLoopChannel::Reasoning => {
                        "Synthesize the conclusions already reached, then take the next \
                         distinct action or complete the task. Do not repeat the prior text."
                    }
                    nuo_protocol::StreamLoopChannel::ToolCall => {
                        "Do not re-emit the same tool-call arguments; change the approach, \
                         reduce the payload, or complete the task with a different call."
                    }
                };
                recovery_prompt = Some(format!(
                    "[System Directive: Your previous output was stopped because it entered a {}. \
                     {directive}]",
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
                hard_stopped = true;
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
        StreamLoopSettlement {
            notice,
            recovery_prompt,
            hard_stopped,
        }
    }

    /// Retain executable tool calls, validate their argument payloads, bind the
    /// provider continuation cursor, and build the assistant message.
    ///
    /// Calls that streamed but do not parse as JSON mean the connection died
    /// mid-payload; fail retryable here instead of executing half a call and
    /// surfacing the parse error as if the model had emitted bad JSON.
    /// (`arguments == ""` is the legitimate shape of a zero-argument tool and
    /// must not trip this check.) The request checkpoint is consumed by the
    /// caller only after this returns a valid response.
    fn finalize_assistant_response<F>(
        &self,
        attempt: &mut StreamAttempt,
        request_provider: String,
        request_model: String,
        request_accounting: &mut RequestAccountingGuard,
        messages: &[Message],
        on_event: &mut F,
    ) -> Result<Message, HarnessError>
    where
        F: FnMut(AgentEvent),
    {
        let content = std::mem::take(&mut attempt.content);
        let reasoning_content = std::mem::take(&mut attempt.reasoning_content);
        let mut calls = std::mem::take(&mut attempt.calls);

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
                retract_announced_calls(&mut attempt.announced_slots, &calls, on_event);
                return Err(HarnessError::Provider(
                    nuo_protocol::ProviderError::new(
                        "harness",
                        nuo_protocol::ProviderErrorKind::Transport,
                        err_msg,
                    )
                    .retryable(None),
                ));
            }
            if call.id.is_empty() {
                call.id = format!("call_{}", uuid::Uuid::new_v4());
            }
        }
        let bound_cursor = if let Some(cursor) = attempt
            .completion_meta
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
            };
            cursor.local_head = nuo_protocol::semantic_context_head(
                messages.iter().chain(std::iter::once(&prospective)),
            );
            Some(cursor.clone())
        } else {
            None
        };
        if let (Some(cursor), Some(meta)) = (bound_cursor, attempt.completion_meta.as_mut()) {
            let artifacts = meta.artifacts.get_or_insert_with(serde_json::Map::new);
            nuo_protocol::write_continuation_cursor(artifacts, &cursor);
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
            provider_meta: attempt
                .completion_meta
                .as_mut()
                .and_then(|meta| meta.artifacts.take()),
            hidden: false,
            children: None,
            subagent_meta: None,
            origin: None,
            timestamp: Some(nuo_tool::todos::unix_now()),
            // The durable send time of this turn, at the same millisecond
            // granularity the live client stamps its band header with. The
            // provider-call boundary is the moment this turn's content began
            // arriving, and it is the only wall-clock reading the harness
            // actually took — the `timestamp` above truncates to seconds, so
            // a resumed band would otherwise show a different minute from the
            // live one it replaced. Stamped from the attempt's own recorded
            // boundary so live and resume read the same instant.
            sent_at_ms: Some(request_accounting.started_at_epoch_ms()),
        };
        if !valid_assistant_response(&response) {
            // `calls` has moved into the response; the ids are the same.
            retract_announced_calls(
                &mut attempt.announced_slots,
                response.tool_calls.as_deref().unwrap_or(&[]),
                on_event,
            );
            return Err(empty_response_error(&response));
        }
        Ok(response)
    }

    /// Book the turn's token usage and settle its financial spend.
    fn settle_turn_usage(
        &self,
        round: &mut StreamingRoundState,
        response: &Message,
        attempt: &mut StreamAttempt,
        request_accounting: &mut RequestAccountingGuard,
    ) -> nuo_protocol::TurnPerformanceSnapshot {
        let performance = self.book_turn_usage(
            &mut round.state,
            response,
            attempt
                .completion_meta
                .as_ref()
                .and_then(|meta| meta.usage)
                .or_else(|| attempt.streamed_usage.take()),
            request_accounting,
        );

        // ADR-0065: Post-flight financial spend settlement.
        let settled_usage = attempt
            .completion_meta
            .as_ref()
            .and_then(|meta| meta.usage)
            .or(attempt.streamed_usage.take());
        if let Some(usage) = settled_usage {
            let conn_id = self.provider.provider_id();
            let model_id = self.provider.model();
            if let Some(pricing) = nuo_protocol::pricing::resolve_model_pricing(&conn_id, &model_id)
            {
                let cost = pricing.calculate_cost(
                    &usage,
                    nuo_protocol::pricing::RatingInstant::from_epoch_ms(
                        request_accounting.started_at_epoch_ms(),
                    ),
                );
                round.state.spend += cost.amount;
                let mut conn_usage = nuo_persistence::connection_usage::ConnectionUsage::load();
                conn_usage.record_spend(&conn_id, &model_id, cost.amount.0);
                let _ = conn_usage.save();
            }
        }
        performance
    }

    /// The round-exit gate: loop recovery, Stop hooks, and follow-up input can
    /// each force another turn in this same round; otherwise the round ends.
    ///
    /// The duration is sampled before the follow-up drain, so a drain that
    /// takes time cannot inflate the finished round's wall clock.
    async fn finish_turn_or_continue<F>(
        &self,
        response: &Message,
        messages: &mut Vec<Message>,
        round: &mut StreamingRoundState,
        loop_settlement: &mut StreamLoopSettlement,
        on_event: &mut F,
    ) -> Result<TurnExit, HarnessError>
    where
        F: FnMut(AgentEvent) + Send,
    {
        let duration_ms = round.started_at.elapsed().as_millis() as u64;
        let mut continue_round = false;
        if let Some(prompt) = loop_settlement.recovery_prompt.take() {
            messages.push(crate::runtime::message::hidden_user(
                InjectionKind::LoopReviewNudge,
                prompt,
            ));
            continue_round = true;
        } else if !loop_settlement.hard_stopped
            && let Some((prompt, kind)) = self.stop_gate(response).await
        {
            messages.push(crate::runtime::message::hidden_user(kind, prompt));
            continue_round = true;
        }
        let admitted = self.drain_follow_up(
            round.session_queue_generation,
            self.round_count(),
            round.turn_index + 1,
            messages,
            !continue_round,
            on_event,
        );
        if admitted > 0 {
            continue_round = true;
        }
        if !continue_round {
            return Ok(TurnExit::Finish(duration_ms));
        }
        round.turn_index += 1;
        self.set_current_turn(round.turn_index as u64);
        if self.check_hard_stop(round.turn_index).is_break() {
            return Err(self.hard_stop_error());
        }
        self.fire_turn_persist(messages).await?;
        self.run_turn_hooks(messages, &round.state, round.turn_index)
            .await;
        self.restore_scoped_turn_end();
        Ok(TurnExit::Continue)
    }
}
