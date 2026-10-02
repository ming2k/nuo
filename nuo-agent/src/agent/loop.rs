use crate::error::{AgentError, Result};
use crate::message::{Message, ToolCall};
use crate::provider::{ModelRequest, Provider};
use crate::session::{Session, SessionEvent, SteeringEffect, SteeringHandle};
use crate::token::Compactor;
use crate::tools::ToolRegistry;
use nuo_tool::{ToolContext, ToolPolicy, ToolScope};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;
use uuid::Uuid;

/// The agent's think → act → observe cycle.
///
/// Owned by [`Agent`](super::Agent) and deliberately private: the loop is not a
/// second entry point into the runtime, it is the implementation of the agent's
/// single entry point. Keeping it here avoids duplicating provider, tool, and
/// budget configuration in two places.
pub(super) struct CognitiveLoop {
    provider: Arc<dyn Provider>,
    tools: ToolRegistry,
    compactor: Compactor,
    steering: SteeringHandle,
    approval_handler: Option<Arc<dyn crate::tools::ApprovalHandler>>,
    policy: ToolPolicy,
    active_scopes: Option<Vec<ToolScope>>,
    skill_stack: Arc<std::sync::Mutex<crate::skill::SkillStack>>,
}

impl CognitiveLoop {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        provider: Arc<dyn Provider>,
        tools: ToolRegistry,
        compactor: Compactor,
        steering: SteeringHandle,
        approval_handler: Option<Arc<dyn crate::tools::ApprovalHandler>>,
        policy: ToolPolicy,
        active_scopes: Option<Vec<ToolScope>>,
        skill_stack: Arc<std::sync::Mutex<crate::skill::SkillStack>>,
    ) -> Self {
        Self {
            provider,
            tools,
            compactor,
            steering,
            approval_handler,
            policy,
            active_scopes,
            skill_stack,
        }
    }

    /// Runs the loop until the model produces a final answer or a limit trips.
    pub(super) async fn run(
        &self,
        session: &mut Session,
        correlation: Uuid,
        events: Option<mpsc::Sender<SessionEvent>>,
    ) -> Result<String> {
        let turn_span = tracing::info_span!(
            "gen_ai.agent.turn",
            session.id = %session.id,
            correlation_id = %correlation
        );
        let _enter = turn_span.enter();

        let mut round: u32 = 0;
        let mut total_tool_calls_executed = 0usize;
        let mut tool_call_counts: HashMap<String, usize> = HashMap::new();
        let mut last_call_sig: Option<(String, String)> = None;
        let mut consecutive_identical_count = 0usize;
        let mut trajectory_window: std::collections::VecDeque<(String, String)> =
            std::collections::VecDeque::new();

        loop {
            round += 1;
            if round > session.budget.max_rounds {
                return Err(AgentError::MaxRoundsExceeded(session.budget.max_rounds));
            }

            let round_span = tracing::info_span!(
                "gen_ai.agent.round",
                round = round,
                tokens = session.current_tokens()
            );
            let _round_enter = round_span.enter();

            emit(&events, SessionEvent::RoundStarted { round }).await;

            // ---------------------------------------------------------------
            // Steering boundary.
            // ---------------------------------------------------------------
            match self.steering.drain(correlation).await {
                None => {}
                Some(SteeringEffect::Notes(lines)) => {
                    for line in &lines {
                        session.add_message(Message::user(line.clone()));
                    }
                    emit(
                        &events,
                        SessionEvent::Steered {
                            round,
                            instruction: lines.join("\n"),
                            action: "note".into(),
                        },
                    )
                    .await;
                }
                Some(SteeringEffect::Redirect(lines)) => {
                    for line in &lines {
                        session.add_message(Message::user(line.clone()));
                    }
                    emit(
                        &events,
                        SessionEvent::Steered {
                            round,
                            instruction: lines.join("\n"),
                            action: "redirect".into(),
                        },
                    )
                    .await;

                    // Re-plan
                    continue;
                }
                Some(SteeringEffect::Cancel { notes }) => {
                    for line in &notes {
                        session.add_message(Message::user(line.clone()));
                    }
                    let partial = session
                        .last_assistant_reply()
                        .map(str::to_string)
                        .unwrap_or_default();
                    let settled = if partial.is_empty() {
                        "Stopped at the caller's request before producing a result.".to_string()
                    } else {
                        partial
                    };
                    session.add_assistant_message(settled.clone());

                    emit(
                        &events,
                        SessionEvent::Steered {
                            round,
                            instruction: notes.join("\n"),
                            action: "cancel".into(),
                        },
                    )
                    .await;
                    emit(
                        &events,
                        SessionEvent::Done {
                            final_content: settled.clone(),
                            total_rounds: round,
                            total_usage: session.total_usage,
                        },
                    )
                    .await;

                    return Ok(settled);
                }
            }

            // ---------------------------------------------------------------
            // Context hygiene: auto-offload oversized tool outputs (Append-Only).
            // Conversational dialogue is strictly immutable, preserving KV-cache
            // prefix stability and deterministic causality ([INV-CTX-05]).
            // ---------------------------------------------------------------
            if let Some(mode) = self.compactor.policy.auto_offload {
                self.compactor
                    .offload_messages(
                        &mut session.messages,
                        mode,
                        self.compactor.policy.max_tool_output_chars,
                    )
                    .await;
            }

            // ---------------------------------------------------------------
            // Infer: unified stream-driven state machine with scoped tools.
            // ---------------------------------------------------------------
            let (effective_scopes, active_instructions) = {
                let stack = self.skill_stack.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                (
                    stack.effective_scopes(self.active_scopes.as_ref()),
                    stack.active_instructions(),
                )
            };

            let mut messages = session.messages.clone();
            for (instr, _cache_control) in active_instructions {
                messages.push(Message::system(format!(
                    "[Active Procedural Skill SOP]\n{instr}"
                )));
            }

            let request = ModelRequest {
                messages,
                tools: self.tools.model_specs_scoped(effective_scopes.as_deref()),
                temperature: None,
                max_tokens: Some(session.budget.max_completion_tokens),
                thinking_budget: None,
            };

            use futures::StreamExt;
            let mut stream = self.provider.stream(request).await?;
            let mut content_acc = String::new();
            let mut thinking_acc = String::new();
            let mut explicit_tool_calls = Vec::new();
            let mut tool_accumulator: std::collections::BTreeMap<
                usize,
                (Option<String>, Option<String>, String),
            > = std::collections::BTreeMap::new();
            let mut usage = crate::provider::TokenUsage::default();

            #[cfg(feature = "wire")]
            let mut stream_guard = nuo_model_codec::StreamLoopDetector::new(1024);

            while let Some(delta_res) = stream.next().await {
                let delta = delta_res?;
                if let Some(content) = delta.content_delta {
                    #[cfg(feature = "wire")]
                    if let Some(pattern) = stream_guard.push_and_check(&content) {
                        tracing::warn!(pattern = %pattern.description(), "Runaway token loop intercepted in cognitive stream");
                        content_acc = nuo_model_codec::StreamLoopDetector::trim_suffix(&content_acc, &pattern);
                        emit(
                            &events,
                            SessionEvent::Steered {
                                round,
                                instruction: format!("[INTERCEPTED]: Streaming aborted due to {}", pattern.description()),
                                action: "stream_loop_abort".into(),
                            },
                        )
                        .await;
                        break;
                    }

                    emit(
                        &events,
                        SessionEvent::ContentDelta {
                            delta: content.clone(),
                        },
                    )
                    .await;
                    content_acc.push_str(&content);
                }
                if let Some(thinking) = delta.thinking_delta {
                    #[cfg(feature = "wire")]
                    if let Some(pattern) = stream_guard.push_and_check(&thinking) {
                        tracing::warn!(pattern = %pattern.description(), "Runaway thinking loop intercepted in cognitive stream");
                        thinking_acc = nuo_model_codec::StreamLoopDetector::trim_suffix(&thinking_acc, &pattern);
                        break;
                    }

                    emit(
                        &events,
                        SessionEvent::ThinkingDelta {
                            delta: thinking.clone(),
                        },
                    )
                    .await;
                    thinking_acc.push_str(&thinking);
                }

                if !delta.tool_calls.is_empty() {
                    explicit_tool_calls = delta.tool_calls;
                } else {
                    let mut deltas_to_process = delta.tool_call_deltas;
                    if deltas_to_process.is_empty()
                        && let Some((index, name, args)) = delta.tool_call_delta
                    {
                        deltas_to_process.push(crate::provider::ToolCallDelta {
                            index,
                            id: None,
                            name,
                            arguments_delta: args,
                        });
                    }

                    for tc in deltas_to_process {
                        emit(
                            &events,
                            SessionEvent::ToolCallDelta {
                                index: tc.index,
                                name: tc.name.clone(),
                                arguments_delta: tc.arguments_delta.clone(),
                            },
                        )
                        .await;

                        let entry = tool_accumulator
                            .entry(tc.index)
                            .or_insert_with(|| (None, None, String::new()));
                        if let Some(id) = tc.id {
                            entry.0 = Some(id);
                        }
                        if let Some(name) = tc.name {
                            entry.1 = Some(name);
                        }
                        if let Some(args) = tc.arguments_delta {
                            entry.2.push_str(&args);
                        }
                    }
                }

                if let Some(u) = delta.usage {
                    usage = u;
                }
            }

            let tool_calls = if !explicit_tool_calls.is_empty() {
                explicit_tool_calls
            } else {
                let mut assembled = Vec::new();
                for (_, (id_opt, name_opt, args_buf)) in tool_accumulator {
                    let name = name_opt.unwrap_or_default();
                    if name.is_empty() {
                        continue;
                    }
                    let id = id_opt.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                    let trimmed = args_buf.trim();
                    let arguments = if trimmed.is_empty() {
                        serde_json::json!({})
                    } else {
                        serde_json::from_str(trimmed)
                            .unwrap_or_else(|_| serde_json::json!({ "raw": args_buf }))
                    };
                    assembled.push(ToolCall {
                        id,
                        name,
                        arguments,
                    });
                }
                assembled
            };

            let response = crate::provider::ModelResponse {
                content: if content_acc.is_empty() {
                    None
                } else {
                    Some(content_acc)
                },
                thinking: if thinking_acc.is_empty() {
                    None
                } else {
                    Some(thinking_acc)
                },
                tool_calls,
                usage,
            };

            session.total_usage.accumulate(&response.usage);

            // ---------------------------------------------------------------
            // Terminal: the model answered instead of calling a tool.
            // ---------------------------------------------------------------
            if response.tool_calls.is_empty() {
                let content = response.content.unwrap_or_default();
                if let Some(thinking) = response.thinking {
                    session.add_assistant_with_thinking(content.clone(), thinking);
                } else {
                    session.add_assistant_message(content.clone());
                }

                emit(
                    &events,
                    SessionEvent::Done {
                        final_content: content.clone(),
                        total_rounds: round,
                        total_usage: session.total_usage,
                    },
                )
                .await;

                return Ok(content);
            }

            // ---------------------------------------------------------------
            // Act: apply anti-abuse guardrails, then execute each tool call.
            // ---------------------------------------------------------------
            let mut tool_calls = response.tool_calls;
            let mut assistant_msg = Message::assistant_with_tools(
                response.content.unwrap_or_default(),
                tool_calls.clone(),
            );
            if let Some(thinking) = response.thinking {
                assistant_msg = assistant_msg.with_thinking(thinking);
            }
            session.add_message(assistant_msg);

            // 1. Parallel flooding cap
            if tool_calls.len() > self.policy.max_calls_per_round {
                let excess = tool_calls.split_off(self.policy.max_calls_per_round);
                for dropped in excess {
                    session.add_tool_error(
                        dropped.id,
                        dropped.name.clone(),
                        format!(
                            "[FLOODING INTERCEPTED]: Tool call '{}' dropped because the round limit of {} parallel calls was reached. Reason with existing outputs first.",
                            dropped.name, self.policy.max_calls_per_round
                        ),
                    );
                }
            }

            for call in tool_calls {
                let tool_span = tracing::info_span!(
                    "gen_ai.tool.call",
                    gen_ai.tool.name = %call.name,
                    gen_ai.tool.call_id = %call.id
                );
                let _tool_enter = tool_span.enter();

                // 2. Active Scope Guard
                if !self
                    .tools
                    .is_allowed_in_scope(&call.name, self.active_scopes.as_deref())
                {
                    let err_msg = format!(
                        "[SCOPE VIOLATION]: Tool '{}' is not available in the active session scope.",
                        call.name
                    );
                    session.add_tool_error(call.id, call.name, err_msg);
                    continue;
                }

                // 3. Global Turn Budget Guard
                if total_tool_calls_executed >= self.policy.max_total_calls_per_turn {
                    let err_msg = format!(
                        "[BUDGET EXHAUSTED]: Maximum total tool calls ({}) exceeded for this turn.",
                        self.policy.max_total_calls_per_turn
                    );
                    session.add_tool_error(call.id, call.name, err_msg);
                    continue;
                }

                // 4. Per-tool Quota Guard
                if let Some(&quota) = self.policy.tool_quotas.get(&call.name) {
                    let current_count = tool_call_counts.get(&call.name).copied().unwrap_or(0);
                    if current_count >= quota {
                        let err_msg = format!(
                            "[QUOTA EXHAUSTED]: Tool '{}' invocation quota of {} has been reached for this turn.",
                            call.name, quota
                        );
                        session.add_tool_error(call.id, call.name, err_msg);
                        continue;
                    }
                }

                // 5. Anti-loop & Trajectory Circuit Breaker (Idempotent and Oscillating loop detection)
                let canonical_args = call.arguments.to_string();
                let current_sig = (call.name.clone(), canonical_args);

                // 5a. Immediate consecutive repetition
                if last_call_sig.as_ref() == Some(&current_sig) {
                    consecutive_identical_count += 1;
                    if consecutive_identical_count >= self.policy.max_identical_consecutive_calls {
                        let err_msg = format!(
                            "[CIRCUIT BREAKER TRIGGERED]: Tool '{}' called with identical arguments {} times consecutively. Execution halted to prevent an infinite loop. You MUST reconsider your approach and try an alternative strategy.",
                            call.name, consecutive_identical_count
                        );
                        session.add_tool_error(call.id, call.name, err_msg);
                        continue;
                    }
                } else {
                    last_call_sig = Some(current_sig.clone());
                    consecutive_identical_count = 1;
                }

                // 5b. Sliding-window trajectory and periodic oscillation detection
                trajectory_window.push_back(current_sig.clone());
                if trajectory_window.len() > 16 {
                    trajectory_window.pop_front();
                }

                // Check periodic oscillation across periods p in [2, 4]
                let mut oscillation_detected = None;
                for p in 2..=4 {
                    if trajectory_window.len() >= 2 * p {
                        let len = trajectory_window.len();
                        let is_periodic = (0..p).all(|i| {
                            trajectory_window[len - 2 * p + i] == trajectory_window[len - p + i]
                        });
                        if is_periodic {
                            oscillation_detected = Some(p);
                            break;
                        }
                    }
                }

                if let Some(period) = oscillation_detected {
                    let err_msg = format!(
                        "[TRAJECTORY OSCILLATION DETECTED]: Periodic cycle of length {} detected across recent tool invocations. Halting oscillation to prevent non-converging loops. You MUST try an alternative strategy.",
                        period
                    );
                    session.add_tool_error(call.id, call.name, err_msg);
                    continue;
                }

                // Check window recurrence count (max 3 occurrences of identical call in sliding window)
                let occurrences_in_window =
                    trajectory_window.iter().filter(|sig| **sig == current_sig).count();
                if occurrences_in_window > 3 {
                    let err_msg = format!(
                        "[TRAJECTORY EXHAUSTION]: Tool '{}' with identical arguments called {} times within sliding window. Halting execution.",
                        call.name, occurrences_in_window
                    );
                    session.add_tool_error(call.id, call.name, err_msg);
                    continue;
                }

                total_tool_calls_executed += 1;
                *tool_call_counts.entry(call.name.clone()).or_insert(0) += 1;

                emit(
                    &events,
                    SessionEvent::ToolCallStarted {
                        call_id: call.id.clone(),
                        name: call.name.clone(),
                        arguments: call.arguments.clone(),
                    },
                )
                .await;

                let ctx = ToolContext::new(call.id.clone())
                    .with_session_id(session.id.clone())
                    .with_correlation_id(correlation);

                if self
                    .tools
                    .requires_approval(&ctx, &call.name, &call.arguments)
                {
                    let req = crate::tools::ToolCallRequest {
                        call_id: call.id.clone(),
                        tool_name: call.name.clone(),
                        arguments: call.arguments.clone(),
                        risk_profile: self.tools.risk_profile(&call.name).unwrap_or_default(),
                    };

                    let approved = match &self.approval_handler {
                        Some(handler) => match handler.request_approval(&req, &ctx).await {
                            Ok(crate::tools::ApprovalDecision::Approved) => true,
                            Ok(crate::tools::ApprovalDecision::Rejected { reason }) => {
                                let err_msg =
                                    format!("Tool execution was rejected by policy: {reason}");
                                emit(
                                    &events,
                                    SessionEvent::ToolCallFinished {
                                        call_id: call.id.clone(),
                                        name: call.name.clone(),
                                        output: err_msg.clone(),
                                        is_error: true,
                                    },
                                )
                                .await;
                                session.add_tool_error(call.id.clone(), call.name.clone(), err_msg);
                                false
                            }
                            Err(err) => {
                                let err_msg = format!("Approval handler error: {err}");
                                emit(
                                    &events,
                                    SessionEvent::ToolCallFinished {
                                        call_id: call.id.clone(),
                                        name: call.name.clone(),
                                        output: err_msg.clone(),
                                        is_error: true,
                                    },
                                )
                                .await;
                                session.add_tool_error(call.id.clone(), call.name.clone(), err_msg);
                                false
                            }
                        },
                        None => {
                            let err_msg = "Tool execution was rejected: approval required but no handler configured".to_string();
                            emit(
                                &events,
                                SessionEvent::ToolCallFinished {
                                    call_id: call.id.clone(),
                                    name: call.name.clone(),
                                    output: err_msg.clone(),
                                    is_error: true,
                                },
                            )
                            .await;
                            session.add_tool_error(call.id.clone(), call.name.clone(), err_msg);
                            false
                        }
                    };

                    if !approved {
                        continue;
                    }
                }

                let outcome = self.tools.execute(&ctx, &call.name, call.arguments).await;
                let (output, is_error) = match outcome {
                    Ok(tool_output) => (tool_output.content, tool_output.is_error),
                    Err(err) => (err.to_string(), true),
                };

                emit(
                    &events,
                    SessionEvent::ToolCallFinished {
                        call_id: call.id.clone(),
                        name: call.name.clone(),
                        output: output.clone(),
                        is_error,
                    },
                )
                .await;

                // A failed tool is an observation the model can reason about,
                // not a reason to abort the session.
                if is_error {
                    session.add_tool_error(call.id, call.name, output);
                } else {
                    session.add_tool_result(call.id, call.name, output);
                }
            }
        }
    }
}

/// Best-effort event emission: a dropped receiver must not fail the session.
async fn emit(events: &Option<mpsc::Sender<SessionEvent>>, event: SessionEvent) {
    if let Some(tx) = events
        && tx.send(event).await.is_err()
    {
        tracing::debug!("session event receiver dropped");
    }
}
