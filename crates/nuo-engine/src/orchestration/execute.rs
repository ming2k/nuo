//! The round executor: provider streaming, retries, tools, and persistence.

use super::compaction::send_compaction;
use super::relay::relay_agent_event;
use super::usage::{estimate_off_executor, persist_request_usage, send_context_projection};
use super::*;
use super::{compaction_insufficient_notice, image_withheld_notice, now_ms};

/// How [`execute_round`] ended when it did **not** fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]

pub enum RoundCompletion {
    /// The round reached its terminal path naturally — the model stopped
    /// calling tools, the full history committed, and `RoundCompleted` was
    /// emitted. A stop site may still have parked a reason in the window
    /// after the last cancellation checkpoint (an Esc Esc that landed too
    /// late to change the outcome); that park describes a stop that never
    /// happened and must **not** produce an interrupt record.
    Completed,
    /// A `UserPromptSubmit` hook denied the prompt: no round was opened, no
    /// model request was made. Nothing was interrupted, so no interrupt
    /// record applies.
    NotStarted,
}

pub async fn execute_round(
    context: RoundContext,
    input: RoundInput,
) -> Result<RoundCompletion, HarnessError> {
    let Some(mut round) = AdmittedRound::admit(context, input).await? else {
        return Ok(RoundCompletion::NotStarted);
    };
    round.prepare_request_context().await?;
    let provider_round = round.drive_provider_round().await?;
    round.finalize_round(provider_round).await
}

/// A round that passed admission and owns the working history it streams.
struct AdmittedRound {
    context: RoundContext,
    resumed_point: Option<nuo_protocol::RetryPoint>,
    admitted_round: u64,
    admitted_session_id: String,
    prompt_for_memory: Option<String>,
    round_history: Vec<Message>,
}

/// The terminal state of the provider attempt loop.
struct ProviderRound {
    result: Result<crate::RoundOutcome, HarnessError>,
    streaming_round: crate::cognition::agent::StreamingRoundState,
    relay_turn: TurnContext,
    streamed_text: Arc<std::sync::atomic::AtomicBool>,
    tool_activity: Arc<std::sync::atomic::AtomicBool>,
}

/// Borrowed round context shared by the provider-attempt phases.
struct RoundIo<'a> {
    agent: &'a Arc<Agent>,
    tx: &'a mpsc::UnboundedSender<AgentResponse>,
    token: &'a CancellationToken,
    session: &'a Arc<ThreadSession>,
    session_id: &'a str,
    projection: &'a ContextProjectionSettings,
}

/// What the image-refusal probe wants the attempt loop to do next.
enum ImageProbeAction {
    /// A recovery pass was armed (or the probe was already armed and fired);
    /// the loop retries without consuming either attempt budget.
    Continue,
    /// The failure is unrelated to attachments; use it unchanged.
    FallThrough,
    /// The probe failed, so the recorded failure is the user's request, not
    /// the harness's modified probe.
    Replace(HarnessError),
}

impl AdmittedRound {
    /// Admission: hook gate, round counter, request/history assembly, durable
    /// admission commit, and the mid-round persistence sinks.
    async fn admit(
        context: RoundContext,
        mut input: RoundInput,
    ) -> Result<Option<Self>, HarnessError> {
        let agent = &context.agent;
        let session = &context.session;
        let session_id = context.session_id.clone();
        // Bind accounting to the session that admitted this round. The master
        // agent survives `/session open` and `/resume`, so its construction-time
        // thread id is not sufficient for attribution.
        agent.set_thread_id(session_id.clone());
        if let Some(ledger) = agent.token_ledger() {
            ledger.set_active_session(session_id.clone());
        }
        let _ = context.tx.send(round_response(
            &session_id,
            RoundEvent::Activity("saving request".to_string()),
        ));

        // UserPromptSubmit hooks (ADR-0025): a hook may deny the prompt or prepend
        // context. Hidden control prompts and retries bypass the gate.
        if !input.hidden && !input.is_retry() {
            match agent.fire_user_prompt_submit(&input.prompt).await {
                crate::runtime::hooks::UserPromptVerdict::Deny(reason) => {
                    let _ = context.tx.send(round_response(
                        &session_id,
                        RoundEvent::Text(format!("Prompt blocked by hook: {reason}")),
                    ));
                    return Ok(None);
                }
                crate::runtime::hooks::UserPromptVerdict::Prepend(context) => {
                    input.prompt = format!("{context}\n\n{}", input.prompt);
                }
                crate::runtime::hooks::UserPromptVerdict::Allow => {}
            }
        }

        // The prompt is now admitted. Bump exactly once before request assembly so
        // hooks, token accounting, todos, and emitted positions share one round
        // number. A prompt rejected by UserPromptSubmit never opens a round.
        // A `/retry` resume is the exception: it continues the round that already
        // bumped (its number is frozen in the resume point), so the counter stays
        // put — the transcript keeps one contiguous `round N` band.
        let resumed_point = match input.driver {
            RoundDriver::Resume { point } => Some(point),
            RoundDriver::Fresh => None,
        };
        if resumed_point.is_none() {
            agent.bump_round();
            // A freshly admitted round supersedes whatever was parked: the user
            // moved on (new prompt, `/compact` follow-up, scheduled job), so any
            // older `/retry` point is stale by construction and must stop being
            // offered.
            if let Err(error) = session.clear_retry_pending().await {
                tracing::warn!(%error, "could not clear stale retry point on fresh round");
            }
        }
        let admitted_round = resumed_point
            .as_ref()
            .map(|point| point.round)
            .unwrap_or_else(|| agent.round_count());

        let admitted_session_id = session.id().await;
        let prompt_for_memory = if !input.hidden {
            Some(input.prompt.clone())
        } else {
            None
        };
        let prompt_for_titler = if !input.hidden && resumed_point.is_none() {
            prompt_for_memory.clone()
        } else {
            None
        };
        // Build `round_history` — the round's working scratch — from the session's
        // authoritative `model_window` plus the new user message (ADR-0048). A
        // `/retry` resume instead re-seeds from the stopped round's checkpoint
        // watermark (see the branch below) and never pushes a user message.
        let round_history = if let Some(point) = resumed_point.as_ref() {
            // `/retry`: re-seed the round's history from the durable
            // checkpoint the stopped round left behind. The window may have moved
            // since (a compaction sharing the store), so
            // the watermark is a *cap*: never re-send content the stopped round
            // never committed (a partially streamed response was already
            // discarded with `StreamDiscard` before the point was armed).
            let window = session.model_window().await;
            let watermark = point.history_watermark.min(window.len());
            window[..watermark].to_vec()
        } else {
            let mut th = session.model_window().await;
            // No automatic turn-intake environment scan. Code structure
            // and workspace change information enter through scoped, on-demand tool
            // retrieval (`code_query`) and optional request-local reminders, never
            // as a silently-committed history injection.
            th.push(if input.hidden {
                crate::runtime::message::hidden_user(InjectionKind::HiddenRoundInput, input.prompt)
            } else {
                let message = Message::new(Role::User, input.prompt);
                let message = match input.display_prompt {
                    Some(display) => message.with_display_content(display),
                    None => message,
                };
                let message = match input.sent_at_ms {
                    Some(sent_at_ms) => message.with_sent_at_ms(sent_at_ms),
                    None => message,
                };
                if input.images.is_empty() {
                    message
                } else {
                    message.with_images(input.images)
                }
            });
            th
        };
        // Persist admission immediately. Mid-round crash recovery must not restore
        // the transcript from round N while leaving the session counter at N-1.
        session
            .commit_turn(CommitTurn {
                messages: &round_history,
                basis: CommitBasis::Appended,
                round_counter: Some(admitted_round),
                usage_records: &[],
                retry_point: None,
                round_interrupt: None,
                operation_id: None,
                expected_revision: None,
            })
            .await?;

        // Session Title (ADR-0022 on-demand refinement): asynchronously generate a concise
        // session title concurrently upon admission of the first user prompt. Runs in
        // the background without blocking TTFT or waiting for round convergence.
        if let Some(prompt) = prompt_for_titler {
            let (_, has_title) = session.title().await;
            if !has_title {
                agent.spawn_session_titler(Arc::clone(session), prompt);
            }
        }

        install_round_persistence(
            &context.agent,
            &context.session,
            &context.session_id,
            &context.tx,
        );
        let _ = context.tx.send(round_response(
            &session_id,
            RoundEvent::Activity("preparing context".to_string()),
        ));

        Ok(Some(Self {
            context,
            resumed_point,
            admitted_round,
            admitted_session_id,
            prompt_for_memory,
            round_history,
        }))
    }

    /// Pre-round projection gate: cheap pruning first, then a full compaction
    /// only when the request estimate still crosses the model-relative gate.
    async fn prepare_request_context(&mut self) -> Result<(), HarnessError> {
        let agent = &self.context.agent;
        let tx = &self.context.tx;
        let session = &self.context.session;
        let session_id = &self.context.session_id;
        let projection = &self.context.projection;
        // Cheap tool-result pruning to relieve pressure before considering a full
        // compaction. Gated by the model-relative `prune_utilization` threshold
        // (ADR-0019) so it engages only once pressure crosses that fraction of the
        // window — not every turn — mirroring the mid-turn gate. Pruning also
        // self-limits to runs that reclaim meaningful space.
        //
        // The estimate is BPE tokenization over the whole prepared request —
        // real CPU-bound work — so it runs on the blocking pool, never on the
        // async executor (starving it stalls TUI rendering and stream forwarding).
        // First estimate of a session pays full price once; later passes reuse
        // the content-addressed weights cache (O(new bytes), not O(session)).
        if self.context.token.is_cancelled() {
            return Err(HarnessError::Interrupted);
        }
        let mut request_estimate = estimate_off_executor(agent, &self.round_history).await;
        // Budget-driven pruning engages strictly when window pressure
        // breaches the model-relative prune threshold watermark. In the healthy cruise zone,
        // history mutation is completely deactivated to preserve 100% KV-cache prefix stability
        // and eliminate artificial context size drops across round boundaries.
        if projection.prune
            && request_estimate.total_tokens > projection.budget.prune_threshold_tokens
        {
            prune_and_commit(
                &mut self.round_history,
                session,
                projection,
                agent.token_weights_handle(),
            )
            .await?;
            request_estimate = estimate_off_executor(agent, &self.round_history).await;
        }
        if request_estimate.total_tokens > projection.budget.compaction_threshold_tokens {
            let _ = tx.send(round_response(
                session_id,
                RoundEvent::Activity("compacting context".to_string()),
            ));
            let extra = agent.fire_pre_compact().await;
            let compaction_settings = projection.for_request(request_estimate);
            if let Some(report) = compact_round_history(
                &mut self.round_history,
                session,
                &compaction_settings,
                Some(agent.provider.clone()),
                extra,
            )
            .await?
            {
                send_compaction(tx, session_id, &report.checkpoint);
                if !report.satisfied {
                    let _ = tx.send(round_response(
                        session_id,
                        RoundEvent::Notice(compaction_insufficient_notice(
                            report.checkpoint.window_tokens_after,
                            compaction_settings.budget.compaction_threshold_tokens,
                        )),
                    ));
                }
            }
            agent.fire_post_compact().await;
            let _ = tx.send(round_response(
                session_id,
                RoundEvent::Activity("preparing context".to_string()),
            ));
        }
        Ok(())
    }

    /// Provider dispatch, stream ingestion, and the retry/restart budgets.
    /// Reuses one `StreamingRoundState` across network attempts so completed
    /// ReAct turns (and tool side effects) are never replayed.
    async fn drive_provider_round(&mut self) -> Result<ProviderRound, HarnessError> {
        let agent = Arc::clone(&self.context.agent);
        let tx = self.context.tx.clone();
        let token = self.context.token.clone();
        let session = Arc::clone(&self.context.session);
        let session_id = self.context.session_id.clone();
        let projection = self.context.projection.clone();
        let retry_base_ms = self.context.retry_base_ms;
        let retry_max_ms = self.context.retry_max_ms;
        let io = RoundIo {
            agent: &agent,
            tx: &tx,
            token: &token,
            session: &session,
            session_id: &session_id,
            projection: &projection,
        };

        let tool_activity = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let streamed_text = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let retry_limit = self.context.retry_max_attempts.clamp(1, 60);
        // Mid-stream failures get their own, much smaller budget: every one of
        // them burns visible output that the retry must discard, and an upstream
        // that dies mid-response usually repeats the failure. `visible_failures`
        // counts attempts that emitted visible output; `stream_restart_limit` is
        // the number of restarts allowed after the first such attempt.
        let stream_restart_limit = self.context.stream_restart_max_attempts.min(10);
        let mut silent_failures: usize = 0;
        let mut visible_failures: usize = 0;
        let mut compacted_after_overflow = false;
        // An in-flight **image-cause probe**: `true` means the attempt now
        // running had its attachments withheld speculatively, because a request
        // refusal occurred while they were attached. If that attempt succeeds, images
        // were the cause and the route is latched; if it fails, the hypothesis is
        // disproved, nothing is latched, and the refusal of the request the user
        // actually composed is what surfaces. No vendor text is consulted at any
        // point — the probe's outcome is the evidence.
        let mut image_probe = false;
        // Keep the ReAct turn alive across network attempts. Completed prior turns
        // are already durably checkpointed above; retaining this state means a
        // retry resumes the pending provider request with the same history, guard
        // registry, hooks, and accounting instead of replaying side effects.
        //
        // A `/retry` resume re-seeds the state from the durable point
        // instead of starting at turn 0: the stopped round's committed turns stay
        // committed, so the resumed execution numbers its next turn M+1 and the
        // transcript's `round N | turn M` sequence is never broken. `attempt`
        // still restarts at 0 for the resume itself — the provider retry budget
        // is per user-visible attempt to complete the round, and the user asking
        // to retry is exactly a new budget.
        let mut streaming_round = match self.resumed_point.as_ref() {
            Some(point) => agent.resume_streaming_round(point),
            None => agent.begin_streaming_round(),
        };
        // Relay-side producer state: the agent knows which turn it is running, and
        // every turn-scoped wire event carries this context. The client never
        // reconstructs it (ADR-0168); the value is refreshed at each
        // `ModelRequestStarted`, the only point a turn can begin.
        let mut relay_turn = TurnContext::new(
            agent.round_count(),
            streaming_round.committed_turns() as u64,
            now_ms(),
        );
        let result = loop {
            let attempt_visible = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let attempt = run_provider_attempt(
                &io,
                &mut self.round_history,
                &mut streaming_round,
                &mut relay_turn,
                &tool_activity,
                &streamed_text,
                &attempt_visible,
            )
            .await;

            let Err(mut error) = attempt else {
                // The probe is confirmed by its *outcome*: the identical
                // turn succeeded once the attachments were withheld, which is
                // evidence enough to latch the route — the vendor never had to
                // explain itself in a format we can parse.
                if std::mem::take(&mut image_probe) {
                    agent.suppress_images_for_current_route();
                    let _ = tx.send(round_response(
                        &session_id,
                        RoundEvent::Notice(image_withheld_notice(&agent.provider.model())),
                    ));
                    tracing::warn!(
                        model = %agent.provider.model(),
                        "confirmed by retry: this route refuses image input; withheld from now on"
                    );
                }
                break attempt;
            };
            if let Err(error) = persist_request_usage(&agent, &session, &session_id).await {
                tracing::warn!(%error, "could not persist request usage after failed attempt");
            }
            let is_context_overflow = matches!(&error, HarnessError::Provider(err) if err.kind() == nuo_protocol::ProviderErrorKind::ContextOverflow);
            if is_context_overflow
                && !compacted_after_overflow
                && !tool_activity.load(Ordering::SeqCst)
                && recover_context_overflow(
                    &io,
                    &mut self.round_history,
                    relay_turn,
                    &streamed_text,
                    &mut compacted_after_overflow,
                )
                .await?
            {
                // A recovery is not a fault: it must not consume either
                // attempt budget.
                continue;
            }

            // An image refusal is *information*, not a wall.
            //
            // Vision is the capability vendors do not reliably publish, so an
            // undeclared route is attempted with images. When the provider refuses
            // for that reason, the failure must not end the round: the durable
            // transcript keeps its images (forbids rewriting history), so
            // the identical request would fail forever — and `/retry` would re-send
            // it unchanged. So the route learns, the *same* turn is re-projected
            // without attachments, and the conversation continues.
            //
            // **How the refusal is recognized is deliberately not a parsing
            // problem.** Every vendor formats its error envelope differently, and
            // their prose drifts, so recognition rests on the only two facts we
            // control: the request was refused (`is_request_refusal`), and it
            // carried attachments. From there the round loop runs the experiment —
            // retry the identical turn without the attachments. **Success is the
            // only evidence**, and no vendor error text is parsed anywhere: the
            // classification that remains (`is_request_refusal`) is derived from the
            // HTTP status the transport already mapped (INV-VISION-6).
            match probe_image_refusal(
                &io,
                &self.round_history,
                &mut streaming_round,
                &streamed_text,
                relay_turn,
                &mut image_probe,
                &error,
            ) {
                ImageProbeAction::Continue => {
                    // A recovery, not a fault: a learned limitation must not consume
                    // either attempt budget, exactly like the overflow
                    // compaction above.
                    continue;
                }
                ImageProbeAction::FallThrough => {}
                ImageProbeAction::Replace(replacement) => error = replacement,
            }

            let (message, retry_after_ms) = match &error {
                HarnessError::Provider(err) => match err.retry_disposition() {
                    nuo_protocol::RetryDisposition::Retry { retry_after_ms } => {
                        (err.message().to_string(), retry_after_ms)
                    }
                    _ => break Err(error),
                },
                _ => break Err(error),
            };
            // Failure classes are budgets, not labels. A failure that emitted
            // visible output is a **stream restart**: the attempt's partial state
            // has to be discarded (the round already retracted its announced calls;
            // `StreamDiscard` drops the streamed text), and an upstream that died
            // mid-response gets only a few chances to do better. A failure that
            // emitted nothing is a **silent attempt**: cheap to repeat, so the
            // larger `retry_max_attempts` budget applies.
            let visible = attempt_visible.load(Ordering::SeqCst);
            // Drop the attempt's streamed text **before** deciding whether to
            // retry: the final failed attempt is just as partial as the ones that
            // get a restart, and leaving its row in the transcript would show an
            // answer that was never committed.
            if streamed_text.swap(false, Ordering::SeqCst) {
                let _ = tx.send(turn_response(
                    &session_id,
                    relay_turn,
                    RoundEvent::StreamDiscard,
                ));
            }
            let (next_attempt, max_attempts) = if visible {
                visible_failures += 1;
                if visible_failures > stream_restart_limit {
                    break Err(HarnessError::Other(format!(
                        "Provider stream failed after partial output {visible_failures} time(s); \
                         gave up after {stream_restart_limit} restart(s). The failed attempts' partial \
                         output was discarded. The upstream is not converging — resend the message, \
                         switch models, or raise `[resilience] stream_restart_max_attempts`."
                    )));
                }
                (visible_failures + 1, stream_restart_limit + 1)
            } else {
                silent_failures += 1;
                if silent_failures >= retry_limit {
                    break Err(HarnessError::Other(message));
                }
                (silent_failures + 1, retry_limit)
            };
            let base_ms = crate::runtime::retry::retry_delay_ms(
                silent_failures
                    .saturating_add(visible_failures)
                    .saturating_sub(1),
                retry_after_ms,
                retry_base_ms,
                retry_max_ms,
            );
            // Apply equal jitter (half fixed, half random) to de-synchronise
            // clients that fail in unison — e.g. many sessions behind one load
            // balancer all hitting a flaky upstream at once. The jittered value
            // stays within `[base/2, base]`, so the configured cap and any
            // server `Retry-After` are still honoured. `apply_jitter_ms` is a
            // pure function; the RNG is injected here so it stays out of tests.
            let delay_ms =
                crate::runtime::retry::apply_jitter_ms(base_ms, |_| fastrand::u64(0..base_ms));
            tracing::warn!(
                attempt = next_attempt,
                max_attempts,
                visible = visible,
                visible_failures,
                silent_failures,
                delay_ms,
                base_ms,
                resumed_after_tools = tool_activity.load(Ordering::SeqCst),
                "retrying after transient provider error"
            );
            // One event, one fact. The retry is announced by `RetryScheduled`
            // alone: it carries the attempt pair, the backoff and the fault, so
            // every consumer (bar clause, notice receipt, headless record) reads the
            // same numbers. A second `Notice(ProviderRetry)` used to be published
            // here with the same content formatted independently, and the two
            // drifted — the bar counted retries, the transcript counted attempts.
            //
            // "Completed tool results are preserved" has no home in
            // `RetryScheduled` because it is not retry state: it is a receipt
            // explaining why a replayed tool call did not run, and the dispatch
            // layer publishes it as `NoticeKind::CheckpointReplay`.
            let _ = tx.send(round_response(
                &session_id,
                RoundEvent::RetryScheduled {
                    attempt: next_attempt,
                    max_attempts,
                    delay_ms,
                    message,
                },
            ));
            tokio::select! {
                _ = token.cancelled() => return Err(HarnessError::Interrupted),
                _ = tokio::time::sleep(std::time::Duration::from_millis(delay_ms)) => {}
            }
        };
        Ok(ProviderRound {
            result,
            streaming_round,
            relay_turn,
            streamed_text,
            tool_activity,
        })
    }

    /// Post-round accounting and telemetry: park a retry point on failure,
    /// commit terminal state atomically, publish completion, then mirror the
    /// task/round state into the durable session.
    async fn finalize_round(
        mut self,
        provider_round: ProviderRound,
    ) -> Result<RoundCompletion, HarnessError> {
        let ProviderRound {
            result,
            streaming_round,
            relay_turn,
            streamed_text,
            tool_activity,
        } = provider_round;
        let agent = Arc::clone(&self.context.agent);
        let tx = self.context.tx.clone();
        let session = Arc::clone(&self.context.session);
        let session_id = self.context.session_id.clone();

        if session.id().await != self.admitted_session_id {
            return Err(HarnessError::Interrupted);
        }
        // Only emit saving activity on natural completion. An interrupted or failed
        // round must never re-arm the activity bar that was already idled.
        if result.is_ok() {
            let _ = tx.send(round_response(
                &session_id,
                RoundEvent::Activity("saving response".to_string()),
            ));
        }

        let (retry_point, outcome) = match result {
            Ok(outcome) => (Some(None), Ok(outcome)),
            Err(error) => {
                // The round failed terminally with committed history — this is
                // exactly the "/retry me" state. Park the resume point
                // so the user's `/retry` continues *this* round: same number,
                // turns onward from what was committed, history at the watermark
                // the failed round durably left behind.
                let point = nuo_protocol::RetryPoint {
                    round: self.admitted_round,
                    turns_committed: streaming_round.committed_turns(),
                    history_watermark: self.round_history.len(),
                    paused_ms: agent.round_paused_ms(),
                    at_ms: crate::runtime::time::unix_epoch_ms(),
                };
                (Some(Some(point)), Err(error))
            }
        };

        let ledger = agent.token_ledger();
        let usage_records = ledger
            .as_ref()
            .map(|ledger| ledger.pending_records_for_session(&session_id));
        let usage_slice = usage_records.as_deref().unwrap_or(&[]);

        // Commit all terminal round state (messages, usage records, retry point)
        // in a single atomic persistence transaction instead of multiple full-snapshot writes.
        // The committed revision is the durable boundary this completion follows
        // (D5); it travels on the completion notification so a client can
        // order the completion against later state.
        let committed_revision = session
            .commit_turn(CommitTurn {
                messages: &self.round_history,
                basis: if agent.take_history_reprojected() {
                    CommitBasis::Reprojected
                } else {
                    CommitBasis::Appended
                },
                round_counter: None,
                usage_records: usage_slice,
                retry_point,
                round_interrupt: None,
                operation_id: None,
                expected_revision: None,
            })
            .await?;

        if let Some(ref ledger) = ledger {
            ledger.acknowledge_records(usage_slice);
        }

        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(error) => {
                // The final committed history still reconciles the meter on the
                // error path; there is no completion event, so ordering is free.
                send_context_projection(&tx, &session_id, &agent, &self.round_history).await;
                if matches!(error, HarnessError::Interrupted)
                    && (streamed_text.load(Ordering::SeqCst)
                        || tool_activity.load(Ordering::SeqCst))
                {
                    let _ = tx.send(turn_response(
                        &session_id,
                        relay_turn,
                        RoundEvent::Text("... [Interrupted]".to_string()),
                    ));
                }
                return Err(error);
            }
        };

        let visible = outcome.message.content.trim().to_string();
        if !visible.is_empty() && !streamed_text.load(Ordering::SeqCst) {
            let _ = tx.send(turn_response(
                &session_id,
                relay_turn,
                RoundEvent::Text(visible.clone()),
            ));
        }

        // Record dialogue into role-scoped cognitive memory (pure user <-> role turns).
        // Only roles equipped with `recall_memory` (e.g. philosophist) record dialogue memory.
        // Engineering and operational roles (developer, ops) are strictly excluded from dialogue memory.
        if agent.tool_manager().find("recall_memory").is_some()
            && let (Some(prompt), Some(role)) = (self.prompt_for_memory.take(), agent.active_role())
            && !prompt.trim().is_empty()
            && !visible.is_empty()
            && let Ok(store) = nuo_persistence::get_role_memory_store()
        {
            let _ = store.record_dialogue(&role, Some(&session_id), &prompt, &visible);
        }

        // D5 / invariant #5: publish the authoritative completion as soon
        // as the commit is acknowledged, *before* any optional projection (the
        // context estimate below, the state mirrors further down). None of those
        // may gate a completed round.
        if self.context.emit_round_completed {
            let _ = tx.send(round_response(
                &session_id,
                RoundEvent::RoundCompleted(nuo_protocol::RoundSummary {
                    round: agent.round_count(),
                    output_tokens: outcome.token_usage.completion_tokens.max(0) as u64,
                    duration_ms: outcome.duration_ms,
                    paused_ms: outcome.paused_ms,
                    generation_ms: outcome.generation_ms,
                    session_revision: committed_revision,
                }),
            ));
            // Tenet II: Proactive push of token ledger updates on turn
            // boundary. The token source report is an authoritative in-memory
            // projection from the ledger, streamed immediately so open telemetry
            // dialogs update live without polling.
            if let Some(ledger) = agent.token_ledger() {
                let report = ledger.snapshot_for_session(&session_id);
                let _ = tx.send(AgentResponse::TokenUsageReport {
                    thread_id: session_id.clone(),
                    report,
                });
            }
        }

        // Optional: reconcile the pre-wire estimate from the final committed
        // history after completion has been published. This reconciles the meter
        // after interruption, tool cancellation, or response commit instead of
        // leaving it anchored to a request shape that is no longer AI-visible.
        send_context_projection(&tx, &session_id, &agent, &self.round_history).await;

        // Mirror the unified task list so resume restores the sticky panel. The
        // value is compared against the session's current list to skip the write
        // (and avoid an event-log entry) on turns where nothing changed — the
        // common case.
        //
        // Auto-clear: once every item reaches a terminal status (completed or
        // cancelled), the task is finished and the list is dropped so a done list
        // does not linger in the panel (and the prompt) indefinitely. An empty
        // list is a no-op here.
        let agent_todos = agent.todos();
        if !agent_todos.items.is_empty() && agent_todos.is_all_done() {
            agent.clear_todos();
            let _ = tx.send(round_response(
                &session_id,
                RoundEvent::TodosUpdated(nuo_tool::TodoList::default()),
            ));
            if let Err(err) = session.set_todos(nuo_tool::TodoList::default()).await {
                tracing::warn!(error = %err, "could not clear todos");
            }
        } else {
            let stored_todos = session.todos().await;
            if agent_todos != stored_todos
                && let Err(err) = session.set_todos(agent_todos).await
            {
                tracing::warn!(error = %err, "could not persist todos");
            }
        }

        // Mirror session-scoped runtime state to the durable session (ADR-0048
        // Phase 2): the disabled-tool mask and the round counter. Each is compared
        // against the durable value and skipped on a match to avoid a no-op
        // event-log entry (mirroring the todos
        // diff above).
        let agent_disabled = agent.disabled_tools_snapshot();
        if agent_disabled != session.disabled_tools().await
            && let Err(err) = session.set_disabled_tools(agent_disabled).await
        {
            tracing::warn!(error = %err, "could not persist disabled tools");
        }
        let agent_round = agent.round_count();
        if agent_round != session.round_counter().await
            && let Err(err) = session.set_round_counter(agent_round).await
        {
            tracing::warn!(error = %err, "could not persist round counter");
        }

        // The cross-session usage aggregate is deliberately NOT computed here. It
        // folds every day blob in the 400-day window (~85-110 ms release, ~410-430
        // ms debug against a year of history) and would sit on the critical path to
        // the tail's idle snapshot, which is what clears the activity bar off
        // `finalizing response`. The `/usage` overlay fetches it on demand instead
        // (`AgentRequest::QueryUsageStats`); live telemetry is carried by the
        // in-memory `TokenUsageReport` published with the completion above.
        Ok(RoundCompletion::Completed)
    }
}

/// Install the two mid-round persistence sinks: the per-ReAct-turn save point
/// and the request-projection forensic archive.
fn install_round_persistence(
    agent: &Arc<Agent>,
    session: &Arc<ThreadSession>,
    session_id: &str,
    tx: &mpsc::UnboundedSender<AgentResponse>,
) {
    // Install the mid-round save point (ADR-0048) so every ReAct-turn boundary
    // durably appends its new messages to the session log. This is the fix for
    // the resume-after-crash gap: without it, a round that ran side-effecting
    // tools and then crashed rewinds the transcript to the previous round,
    // leaving it out of sync with the filesystem. The closure clones the
    // session `Arc` and the message slice (the `BoxFuture` is `'static`), then
    // delegates to `ThreadSession::append_turn`, which writes only the delta.
    {
        let session_for_round = Arc::clone(session);
        let agent_for_round = Arc::clone(agent);
        let accounting_ledger = agent.token_ledger();
        let accounting_session_id = session_id.to_string();
        let tx_for_turn = tx.clone();
        agent.set_turn_persist(Arc::new(move |messages: &[Message]| {
            let session = Arc::clone(&session_for_round);
            let agent = Arc::clone(&agent_for_round);
            let snapshot = messages.to_vec();
            let ledger = accounting_ledger.clone();
            let session_id = accounting_session_id.clone();
            let tx = tx_for_turn.clone();
            Box::pin(async move {
                // One lock acquisition, one event batch, at most one snapshot
                // write per turn — the three mutations a turn produces (new
                // message tail, round counter, settled usage attempts) are a
                // single persistence transaction, not three full-snapshot
                // setters.
                let usage_records = ledger
                    .as_ref()
                    .map(|ledger| ledger.pending_records_for_session(&session_id));
                let usage_slice = usage_records.as_deref().unwrap_or(&[]);
                // A mid-turn context projection may have rewritten committed
                // messages; only then does the store need to compare history.
                let basis = if agent.take_history_reprojected() {
                    CommitBasis::Reprojected
                } else {
                    CommitBasis::Appended
                };
                let outcome = session
                    .commit_turn(CommitTurn {
                        messages: &snapshot,
                        basis,
                        round_counter: Some(agent.round_count()),
                        usage_records: usage_slice,
                        retry_point: None,
                        round_interrupt: None,
                        operation_id: None,
                        expected_revision: None,
                    })
                    .await
                    .map(|_| ());
                if outcome.is_ok()
                    && let Some(ref ledger) = ledger
                {
                    ledger.acknowledge_records(usage_slice);
                }
                // Proactive push of token ledger updates on mid-round turn boundary.
                // When an LLM turn produces tool calls or outputs and commits, stream the live
                // report immediately so open telemetry dialogs update turn-by-turn without polling.
                if let Some(ref ledger) = ledger {
                    let report = ledger.snapshot_for_session(&session_id);
                    let _ = tx.send(AgentResponse::TokenUsageReport {
                        thread_id: session_id.clone(),
                        report,
                    });
                }
                outcome
            })
        }));
    }
    // Install the request-projection archive sink: each freshly
    // assembled request is enqueued to the session's forensic archive, stored
    // outside the transcript and never replayed into a later request. The sink
    // is fire-and-forget so it never blocks request dispatch.
    {
        let session_for_projection = Arc::clone(session);
        let projection_session_id = session_id.to_string();
        agent.set_request_projection_persist(Arc::new(move |record| {
            session_for_projection
                .try_record_request_projection(projection_session_id.clone(), record);
        }));
    }
}

/// One provider request/stream attempt. The event callback keeps the round's
/// visibility flags current and relays every event in wire order; the
/// `StreamingRoundState` retains completed turns so a retry resumes rather
/// than replays them.
async fn run_provider_attempt(
    io: &RoundIo<'_>,
    round_history: &mut Vec<Message>,
    streaming_round: &mut crate::cognition::agent::StreamingRoundState,
    relay_turn: &mut TurnContext,
    tool_activity: &Arc<std::sync::atomic::AtomicBool>,
    streamed_text: &Arc<std::sync::atomic::AtomicBool>,
    attempt_visible: &Arc<std::sync::atomic::AtomicBool>,
) -> Result<crate::RoundOutcome, HarnessError> {
    let activity_for_run = Arc::clone(tool_activity);
    let streamed_for_run = Arc::clone(streamed_text);
    let visible_for_run = Arc::clone(attempt_visible);
    let accounting_ledger = io.agent.token_ledger();
    let accounting_session = Arc::clone(io.session);
    let accounting_session_id = io.session_id.to_string();
    io.agent
        .resume_streaming_with_events(round_history, io.token, streaming_round, |event| {
            if matches!(event, AgentEvent::ToolCall { .. }) {
                activity_for_run.store(true, Ordering::SeqCst);
            }
            // "Visible" is wider than text: an announced tool call is
            // already a transcript row, so a retry that follows it must
            // be classified as a stream restart (discard visible state,
            // small budget), not as a silent request retry.
            if matches!(
                event,
                AgentEvent::AssistantDelta { .. }
                    | AgentEvent::ReasoningDelta { .. }
                    | AgentEvent::ToolCallStarted { .. }
            ) {
                visible_for_run.store(true, Ordering::SeqCst);
            }
            if matches!(event, AgentEvent::ModelRequestStarted { .. })
                && let Some(ledger) = accounting_ledger.clone()
            {
                // Persist the in-flight state without blocking streaming.
                // The task reads the ledger when it runs, so if completion
                // races ahead it writes the newer terminal record rather
                // than overwriting it with a stale in-flight snapshot.
                let session = Arc::clone(&accounting_session);
                let session_id = accounting_session_id.clone();
                tokio::spawn(async move {
                    let records = ledger.pending_records_for_session(&session_id);
                    if let Err(error) = session.set_request_usage_records(records).await {
                        tracing::warn!(
                            %error,
                            "could not persist in-flight request usage"
                        );
                    }
                });
            }
            relay_agent_event(io.tx, io.session_id, event, &streamed_for_run, relay_turn);
        })
        .await
}

/// Overflow recovery: fold the history and drop the partial attempt output.
/// Returns `true` when a recovery pass ran (the caller must retry without
/// consuming either attempt budget).
async fn recover_context_overflow(
    io: &RoundIo<'_>,
    round_history: &mut Vec<Message>,
    relay_turn: TurnContext,
    streamed_text: &Arc<std::sync::atomic::AtomicBool>,
    compacted_after_overflow: &mut bool,
) -> Result<bool, HarnessError> {
    let overflow_settings = ContextProjectionSettings {
        request_overhead_tokens: 0,
        preserve_rounds: io.projection.preserve_rounds.max(1),
        ..io.projection.clone()
    }
    .for_request(io.agent.estimate_next_request_tokens(round_history));
    if let Some(report) = compact_round_history(
        round_history,
        io.session,
        &overflow_settings,
        Some(io.agent.provider.clone()),
        Vec::new(),
    )
    .await?
    {
        *compacted_after_overflow = true;
        if streamed_text.swap(false, Ordering::SeqCst) {
            let _ = io.tx.send(turn_response(
                io.session_id,
                relay_turn,
                RoundEvent::StreamDiscard,
            ));
        }
        send_compaction(io.tx, io.session_id, &report.checkpoint);
        if !report.satisfied {
            let _ = io.tx.send(round_response(
                io.session_id,
                RoundEvent::Notice(compaction_insufficient_notice(
                    report.checkpoint.window_tokens_after,
                    overflow_settings.budget.compaction_threshold_tokens,
                )),
            ));
        }
        return Ok(true);
    }
    Ok(false)
}

/// The image-cause probe: when a request carrying attachments is refused, retry
/// the *same* turn (same checkpoint, same accounting) with the attachments
/// stripped. Success latches the route; a second refusal disproves the
/// hypothesis and restores the original error. No vendor error text is read.
fn probe_image_refusal(
    io: &RoundIo<'_>,
    round_history: &[Message],
    streaming_round: &mut crate::cognition::agent::StreamingRoundState,
    streamed_text: &Arc<std::sync::atomic::AtomicBool>,
    relay_turn: TurnContext,
    image_probe: &mut bool,
    error: &HarnessError,
) -> ImageProbeAction {
    if !io.agent.images_suppressed_for_current_route()
        && let HarnessError::Provider(provider_error) = error
        && provider_error.is_request_refusal()
        && round_history.iter().any(|message| {
            message
                .images
                .as_ref()
                .is_some_and(|images| !images.is_empty())
        })
    {
        // The armed checkpoint is what the retry re-sends, so re-project
        // *it* — the turn's history, tools, hooks, and accounting stay
        // exactly as assembled, and only the attachments disappear. The strip
        // is unconditional (`strip_images`): the route is not latched yet,
        // because withholding them to see what happens IS the experiment. It
        // must actually happen, or the "retry" would be byte-identical and
        // prove nothing.
        let stripped = streaming_round
            .project_pending_request(|request| {
                crate::cognition::agent::strip_images(&mut request.messages)
            })
            .unwrap_or(0);
        if stripped == 0 {
            // Nothing to withhold — the checkpoint carried no attachments
            // (the refusal arrived before assembly, say). Re-sending would be
            // byte-identical, so this is not an experiment; fall through to
            // the ordinary error path.
        } else if !*image_probe {
            // Hypothesis armed. Note what the round is now running: an
            // experiment whose *outcome* is the evidence. Nothing here reads
            // the provider's error text, so no vendor format can mislead the
            // recovery — and no wording can withhold a capability by itself.
            *image_probe = true;
            if streamed_text.swap(false, Ordering::SeqCst) {
                let _ = io.tx.send(turn_response(
                    io.session_id,
                    relay_turn,
                    RoundEvent::StreamDiscard,
                ));
            }
            tracing::warn!(
                model = %io.agent.provider.model(),
                withheld = stripped,
                "request refused while carrying attachments; probing without them"
            );
            return ImageProbeAction::Continue;
        } else if std::mem::take(image_probe) {
            // The probe failed, so images were *not* the cause. Surface the
            // refusal of the request the user actually composed rather than
            // a second failure of the harness's own modified probe.
            tracing::warn!(
                model = %io.agent.provider.model(),
                "retrying without images did not help; the refusal is not about them"
            );
            return ImageProbeAction::Replace(HarnessError::Provider(provider_error.clone()));
        }
    }
    ImageProbeAction::FallThrough
}
