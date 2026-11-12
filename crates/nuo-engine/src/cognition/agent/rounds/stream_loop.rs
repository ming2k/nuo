use super::*;

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
    /// to `no` inside [`crate::cognition::pipeline::CognitivePipeline`].
    #[allow(clippy::too_many_arguments)] // mirrors the two call sites' evidence set; bundling would obscure the channel symmetry
    pub(super) fn dispatch_stream_loop_candidate(
        &self,
        detector: &mut crate::stream_loop_detector::StreamLoopDetector,
        messages: &[Message],
        assistant_text: &str,
        reasoning_text: &str,
        tool_input: &str,
        channel: nuo_protocol::StreamLoopChannel,
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
            tool_input,
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
        tool_input: &str,
        channel: nuo_protocol::StreamLoopChannel,
        pattern: crate::stream_loop_detector::DegeneratePattern,
    ) -> PendingStreamLoopReview {
        let input = nuo_protocol::StreamLoopReviewInput {
            heuristic_candidate: pattern.description(),
            channel,
            preceding_context: stream_loop_preceding_context(messages),
            assistant_text: project_stream_loop_field(assistant_text, STREAM_LOOP_TURN_FIELD_CHARS),
            reasoning_text: project_stream_loop_field(reasoning_text, STREAM_LOOP_TURN_FIELD_CHARS),
            tool_input: project_stream_loop_field(tool_input, STREAM_LOOP_TURN_FIELD_CHARS),
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
}
