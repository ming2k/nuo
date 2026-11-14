use super::*;

impl Agent {
    /// Execute any tool calls carried by `response`, emitting events and
    /// appending tool results to `messages`. The single dispatch point of the
    /// streaming ReAct loop, so the dispatch contract — repeated-call guard,
    /// up-front `ToolCall` events, concurrent execution with FIFO-ordered
    /// results, and pursuit/mode updates — lives in exactly one place.
    ///
    /// `text_streamed` is true when the response text was already streamed to
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
    pub(super) async fn dispatch_tool_calls<F>(
        self: &Arc<Self>,
        response: &Message,
        messages: &mut Vec<Message>,
        state: &mut RoundState,
        text_streamed: bool,
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
                Some(
                    self.schedule_tool_calls(&exec_calls, cancel, on_event)
                        .await?,
                )
            };
            return self
                .dispatch_finalize(tool_calls, prepared, outcome, messages, state, on_event)
                .await;
        }

        // Text-based fallback: any provider may emit a JSON tool call as text.
        if let Some(call) = crate::tool_call::parse_text_tool_call(&response.content) {
            if text_streamed {
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
                        NoticeKind::CheckpointReplay,
                        NoticeSeverity::Warning,
                        "Completed tool call not repeated",
                        NoticeSource::Harness,
                    )
                    .with_body(
                        "The retried model request repeated a tool call that already completed. \
                         The checkpointed result remains authoritative; the tool was not run again.",
                    )
                ));
            }
            // Pre-dispatch trajectory check, mirroring the native path: catch a repeat
            // *before* the text-fallback tool runs.
            let trajectory_action = if checkpoint_replay {
                nuo_guardrails::guard::GuardAction::Continue
            } else {
                state
                    .guards
                    .check_trajectory_ahead(&[(call.name.as_str(), call.arguments.as_str())])
            };
            let trajectory_message: Option<String> = match &trajectory_action {
                nuo_guardrails::guard::GuardAction::Block { message, .. } => {
                    tracing::warn!(
                        tool = %call.name,
                        args = %call.arguments,
                        "text-fallback call blocked by trajectory guard before execution"
                    );
                    Some(message.clone())
                }
                _ => None,
            };
            // ADR-0166: the fallback call carries its own minted id; every
            // event and the persisted result read that one value.
            on_event(AgentEvent::ToolCall {
                id: call.id.clone(),
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
                    id: call.id.clone(),
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
                    )),
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
                    id: call.id.clone(),
                    name: call.name.clone(),
                    output: output.to_text(),
                    structured: output.clone(),
                    duration_ms: 0,
                });
                output
            } else {
                let outcome = self.execute_tool_evented(&call, cancel, on_event).await?;
                if outcome.interrupted {
                    // Record a drained result (an interrupted subagent's partial
                    // transcript) before ending the round as interrupted.
                    if let Some(result) = outcome.result {
                        let duration_ms = std::time::Instant::now().elapsed().as_millis() as u64;
                        self.record_tool_result(
                            ToolResultRecord {
                                call: &call,
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
                messages.push(crate::runtime::message::hidden_user(
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
            result,
            duration_ms,
            checkpoint_replay,
            emit_event,
        } = record;
        // ADR-0166: the call id is the call's identity — events, the journal,
        // and the persisted `tool_call_id` all read the same value.
        let call_id = call.id.as_str();
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
            None => format_tool_result_message(call, result, &text),
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
            messages.push(crate::runtime::message::tool_image(
                &call.name,
                mime.clone(),
                data.clone(),
            ));
        }
    }
}

pub(crate) fn format_tool_result_message(
    call: &ToolCall,
    result: &ToolOutput,
    text: &str,
) -> Message {
    if let ToolOutput::Shell {
        truncated: true,
        stdout,
        stderr,
        exit,
        ..
    } = result
    {
        // ADR-0093 [INV-SPOOL-01]: Ephemeral Session Spool.
        // Store full raw un-truncated shell output in OS-native temporary spool file
        // without pseudo-VFS resolver interception and without redundant triplication.
        let raw_full = nuo_tool::tool_output::shell_inner_text(stdout, stderr, *exit);
        let short_hash = nuo_tool::vfs_short_hash(raw_full.as_bytes());

        // Single atomic write to canonical spool directory
        let spool_dir = nuo_tool::spool_root_dir();
        let _ = std::fs::create_dir_all(&spool_dir);
        let spool_file = spool_dir.join(format!("{short_hash}.log"));
        let _ = std::fs::write(&spool_file, raw_full.as_bytes());

        let formatted = format!("[{} result]:\n{}", call.name, text);
        let spool_path_str = spool_file.display().to_string();
        // ADR-0093 [INV-TRUNC-01]: Objective metadata envelope; zero imperative pseudo-guidance
        let message_content = format!("{formatted}\n\n- Spool File: \"{spool_path_str}\"");
        Message::tool_result(call, message_content)
    } else if call.name == "read"
        && (call.arguments.contains("/tmp/nuo/vfs")
            || call.arguments.contains("/tmp/nuo/spool")
            || call.arguments.contains("/dev/shm/nuo/spool")
            || call.arguments.contains("/tmp/vfs")
            || call.arguments.contains("/virtual")
            || call.arguments.contains(".nuo/virtual")
            || call.arguments.contains("virtual://"))
    {
        // ADR-0070 / ADR-0093: Spool and VFS virtual reads are line-budgeted in-process and immune to secondary spilling
        let formatted = format!("[{} result]:\n{}", call.name, text);
        Message::tool_result(call, formatted)
    } else if text.len() > nuo_tool::tool_output::SHELL_MAX_OUTPUT_CHARS {
        // ADR-0093: Ephemeral spooling & tail-heavy folding for oversized generic tool outputs.
        let short_hash = nuo_tool::vfs_short_hash(text.as_bytes());
        let spool_dir = nuo_tool::spool_root_dir();
        let _ = std::fs::create_dir_all(&spool_dir);
        let spool_file = spool_dir.join(format!("{short_hash}.log"));
        let _ = std::fs::write(&spool_file, text.as_bytes());

        // ADR-0093 [INV-FOLD-01]: Tail-heavy biasing (25% head, 75% tail)
        let head_len = nuo_tool::tool_output::SHELL_TRUNCATED_CHARS / 4;
        let tail_len = (nuo_tool::tool_output::SHELL_TRUNCATED_CHARS * 3) / 4;
        let head_part: String = text.chars().take(head_len).collect();
        let tail_part: String = text
            .chars()
            .rev()
            .take(tail_len)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let middle_chars = text.chars().count().saturating_sub(head_len + tail_len);
        let middle_kb = (middle_chars as f64) / 1024.0;
        let middle_lines = text
            .lines()
            .count()
            .saturating_sub(head_part.lines().count() + tail_part.lines().count());

        // Extract salient error signatures from middle omitted section
        let middle_text: String = text.chars().skip(head_len).take(middle_chars).collect();
        let salient = nuo_tool::extract_salient_signatures(&middle_text, 3);
        let salient_section = if !salient.is_empty() {
            let mut s = format!(". {} salient signature(s) detected:\n", salient.len());
            for (rel_line, sig) in salient {
                s.push_str(&format!(">> [line +{rel_line}]: {sig}\n"));
            }
            s
        } else {
            String::new()
        };

        let banner = if middle_lines > 1 {
            if salient_section.is_empty() {
                format!(
                    "--- [Middle {middle_lines} lines ({middle_kb:.1} KB) omitted to protect context budget] ---"
                )
            } else {
                format!(
                    "--- [Middle {middle_lines} lines ({middle_kb:.1} KB) omitted to protect context budget{salient_section}---]"
                )
            }
        } else {
            format!(
                "--- [Middle {middle_chars} characters ({middle_kb:.1} KB) omitted to protect context budget] ---"
            )
        };

        let spool_path_str = spool_file.display().to_string();
        let stub_content = format!(
            "[{name} result]:\n{head_part}\n\n{banner}\n\n{tail_part}\n\n- Spool File: \"{spool_path_str}\"",
            name = call.name
        );
        Message::tool_result(call, stub_content)
    } else {
        let formatted = format!("[{} result]:\n{}", call.name, text);
        Message::tool_result(call, formatted)
    }
}
