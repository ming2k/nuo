//! Subagent event dispatching, hierarchy tracking, and status formatting.
use super::super::helpers::*;
use super::super::types::*;
use super::TranscriptMessage;
use nuo_protocol::{Role, SubagentEvent};

impl TranscriptMessage {
    /// Append a subagent event as a nested child of this tool step.
    ///
    /// `disclosure` is the same [`crate::disclosure_config::DisclosureSource`] the primary transcript
    /// uses, so a child trace / step resolves its default-expand state from one
    /// policy instead of a hardcoded guess. The folding path is not a second
    /// presentation authority: what a reloaded child shows
    /// (`transcript_children_from_core`, which already threads the config) and
    /// what a live child shows must be the same projection.
    ///
    /// Returns `true` if this message is a tool step and the event was stored.
    pub fn push_subagent_event(
        &mut self,
        event: &SubagentEvent,
        disclosure: &impl crate::disclosure_config::DisclosureSource,
    ) -> bool {
        let MessageKind::ToolStep {
            children,
            profile,
            awaiting,
            activity,
            retry,
            ..
        } = &mut self.kind
        else {
            return false;
        };
        // Progress events clear a parked human-decision wait; request events
        // (permission / ask-user / input) park it, so the peek row can say
        // `awaiting approval` instead of replaying the last tool activity.
        match event {
            SubagentEvent::PermissionRequest(_)
            | SubagentEvent::UserQuestionRequest(_)
            | SubagentEvent::StdinRequest(_) => *awaiting = true,
            SubagentEvent::ToolCall { .. }
            | SubagentEvent::ToolResult { .. }
            | SubagentEvent::StreamStart { .. }
            | SubagentEvent::StreamDelta(_)
            | SubagentEvent::StreamEnd(_)
            | SubagentEvent::StreamReasoningStart { .. }
            | SubagentEvent::StreamReasoningDelta(_)
            | SubagentEvent::StreamReasoningEnd(_) => {
                *awaiting = false;
                *retry = None;
            }
            _ => {}
        }
        match event {
            // The subagent announced its role — stamp it on the step so the
            // renderer can draw an `[explore]` / `[debug]` role badge in front
            // of the summary instead of a generic `[SUBAGENT]`.
            // No child message is produced.
            SubagentEvent::Started { profile: name } => {
                *profile = Some(name.clone());
            }
            SubagentEvent::StreamStart { round, turn } => {
                children.push(
                    TranscriptMessage::new(Role::Assistant, "")
                        .with_round(*round)
                        // `turn` is the subagent's 0-indexed model-request
                        // position; the transcript's `turn` is 1-indexed.
                        .with_turn((*turn as u64) + 1),
                );
            }
            SubagentEvent::StreamDelta(delta) => {
                // Identity-addressed: fold the delta into the
                // latest assistant-text child of the *same* stream turn, not
                // merely the last child — a tool-call/result child can be
                // appended between two deltas and would otherwise fork the
                // text into a second entry.
                let target = children
                    .iter_mut()
                    .rfind(|m| m.role == Role::Assistant && matches!(m.kind, MessageKind::Text));
                if let Some(last) = target {
                    last.push_stream(&sanitize_text(delta));
                } else {
                    let mut msg = TranscriptMessage::new(Role::Assistant, "");
                    msg.push_stream(&sanitize_text(delta));
                    children.push(msg);
                }
            }
            SubagentEvent::StreamEnd(content) => {
                if let Some(last) = children
                    .iter_mut()
                    .rfind(|m| m.role == Role::Assistant && matches!(m.kind, MessageKind::Text))
                {
                    last.raw = content.clone();
                    last.reparse();
                } else {
                    children.push(TranscriptMessage::new(Role::Assistant, content.clone()));
                }
            }
            // The subagent's live reasoning chain, folded into the same
            // `MessageKind::Reasoning` message a resumed session restores from
            // `reasoning_content` — so a live drill-in and a reloaded one show
            // the same children. Placement mirrors the wire order the child
            // emits (reasoning precedes its turn's assistant text and tool
            // calls), so the trace lands in the right turn band. Disclosed
            // chains only: the sender gates hidden-chain models out at the
            // source, so no phantom summary trace can appear here.
            SubagentEvent::StreamReasoningStart { round, turn } => {
                let mut trace = TranscriptMessage::reasoning("")
                    .with_round(*round)
                    .with_turn((*turn as u64) + 1);
                // `[tui.default_expanded] thinking` applies to a child trace
                // exactly as it does to the primary's — a resumed child already
                // honoured it (`transcript_children_from_core`), so the live
                // folding path must not be the one place the setting is dead.
                trace.set_reasoning_expanded(crate::disclosure_config::reasoning_expanded(
                    disclosure,
                ));
                children.push(trace);
            }
            SubagentEvent::StreamReasoningDelta(delta) => {
                // Identity-addressed: fold into the latest still-
                // streaming thinking child. `StreamReasoningStart` pushes a
                // stamped Thinking child; a tool-call child landing between
                // two deltas must not fork the trace into a second entry.
                if let Some(last) = children
                    .iter_mut()
                    .rfind(|m| m.is_reasoning() && m.is_reasoning_streaming())
                {
                    last.push_reasoning_delta(delta);
                } else {
                    let mut trace = TranscriptMessage::reasoning(delta);
                    trace.set_reasoning_expanded(crate::disclosure_config::reasoning_expanded(
                        disclosure,
                    ));
                    children.push(trace);
                }
            }
            SubagentEvent::StreamReasoningEnd(content) => {
                if let Some(last) = children
                    .iter_mut()
                    .rfind(|m| m.is_reasoning() && m.is_reasoning_streaming())
                {
                    last.finalize_reasoning(content);
                    // No wall clock is available on the folding path; 0 is
                    // the same terminal stamp a resumed session applies, and
                    // what matters is that the trace stops "streaming" so the
                    // spinner freezes.
                    last.set_reasoning_duration(0);
                } else if !content.is_empty() {
                    let mut trace = TranscriptMessage::reasoning(content);
                    trace.set_reasoning_duration(0);
                    trace.set_reasoning_expanded(crate::disclosure_config::reasoning_expanded(
                        disclosure,
                    ));
                    children.push(trace);
                }
            }
            SubagentEvent::ToolCall {
                id,
                name,
                arguments,
                round,
                turn,
            } => {
                children.push(
                    TranscriptMessage::tool_step(id.clone(), name.clone(), arguments.clone())
                        .with_round(*round)
                        .with_turn((*turn as u64) + 1),
                );
            }
            SubagentEvent::ToolResult {
                id,
                name,
                output,
                duration_ms,
                ..
            } => {
                if let Some(child) = children.iter_mut().find(|m| {
                    m.is_tool_step()
                        && if let MessageKind::ToolStep {
                            id: step_id,
                            output: None,
                            ..
                        } = &m.kind
                        {
                            step_id == id
                        } else {
                            false
                        }
                }) {
                    child.finish_tool_step(
                        id,
                        output.clone(),
                        nuo_protocol::ToolOutput::text(output.clone()),
                        *duration_ms,
                    );
                    if let Some(status) = child.tool_step_status() {
                        child.set_tool_step_expanded(crate::disclosure_config::tool_step_expanded(
                            disclosure, status, name,
                        ));
                    }
                } else {
                    let mut msg = TranscriptMessage::tool_step(id.clone(), "tool", "{}");
                    msg.finish_tool_step(
                        id,
                        output.clone(),
                        nuo_protocol::ToolOutput::text(output.clone()),
                        *duration_ms,
                    );
                    if let Some(status) = msg.tool_step_status() {
                        msg.set_tool_step_expanded(crate::disclosure_config::tool_step_expanded(
                            disclosure, status, name,
                        ));
                    }
                    children.push(msg);
                }
            }
            SubagentEvent::Notice(notice) => {
                children.push(TranscriptMessage::notice_from_core(notice));
            }
            // The subagent reported a free-text activity line (`waiting for
            // model`, `waiting to retry (3s)`). Stored for the peek row so a
            // stretch with no child events still reads as alive. No child
            // message is produced.
            SubagentEvent::Activity(text) => *activity = Some(text.clone()),
            // The child's setback, in the same typed vocabulary the principal
            // round uses. Rendering it from data (rather than from the
            // `waiting to retry (3s)` activity string) is what lets the peek
            // row show the same retry the child is actually on, phrased by the
            // same owner (`RetryProgress::label`) as the principal's clause.
            SubagentEvent::RetryScheduled {
                attempt,
                max_attempts,
                delay_ms,
                message,
            } => {
                let retry_at =
                    std::time::Instant::now() + std::time::Duration::from_millis(*delay_ms);
                let next = match retry.take() {
                    Some(progress) => progress.with_next_attempt(
                        *attempt,
                        *max_attempts,
                        retry_at,
                        message.clone(),
                    ),
                    None => crate::retry::RetryProgress::scheduled(
                        *attempt,
                        *max_attempts,
                        retry_at,
                        message.clone(),
                    ),
                };
                *retry = Some(Box::new(next));
            }
            // Full-duplex (ADR-0029): a subagent surfaced a permission /
            // ask_user request up through the subagent tool. The down-direction
            // reply (registry → handle → reply_permission / reply_user_question)
            // is wired at the agent layer; rendering the nested prompt in the
            // TUI and routing the user's answer back down is the harness↔TUI
            // integration step that follows. Until then these are observed but
            // not rendered as a nested child step (the request still reaches
            // the harness via the `RoundEvent::Subagent` envelope, so a future
            // handler can attach without changing the event shape).
            SubagentEvent::PermissionRequest(_)
            | SubagentEvent::UserQuestionRequest(_)
            | SubagentEvent::StdinRequest(_) => {}
        }
        true
    }

    /// A tool step that spawns a subagent — the read-only `subagent` tool or the
    /// write-capable `delegate_code` tool. Such steps render as a compact,
    /// non-expandable line that navigates into a dedicated subagent view on
    /// activation (see the TUI focus stack) rather than expanding inline.
    pub fn is_subagent_task(&self) -> bool {
        matches!(
            &self.kind,
            MessageKind::ToolStep { name, .. }
                if matches!(
                    name.as_str(),
                    "spawn_agent" | "delegate_code"
                )
        )
    }

    /// The bound subagent profile name (`explore` / `plan` / `verify` / …), used
    /// by the inline step's role badge. `None` until the `Started` event lands
    /// (or for non-subagent steps); the renderer falls back to a generic
    /// `[SUBAGENT]` badge then.
    pub fn subagent_profile(&self) -> Option<&str> {
        match &self.kind {
            MessageKind::ToolStep { profile, .. } => profile.as_deref(),
            _ => None,
        }
    }

    /// The nested child messages emitted by a subagent task. Returns `None`
    /// for non-tool-step messages.
    pub fn subagent_children(&self) -> Option<&[TranscriptMessage]> {
        match &self.kind {
            MessageKind::ToolStep { children, .. } => Some(children),
            _ => None,
        }
    }

    /// Mutable access to a tool step's child messages (used when the view is
    /// zoomed into a subagent and its children are the active message stream).
    pub fn subagent_children_mut(&mut self) -> Option<&mut Vec<TranscriptMessage>> {
        match &mut self.kind {
            MessageKind::ToolStep { children, .. } => Some(children),
            _ => None,
        }
    }

    /// The subagent's role (`explore` / `plan` / `verify` / …), identified by
    /// the `Started` event. `None` for non-task steps and before the role is
    /// known. The Subagent page header renders this as the `[ROLE]` tag between
    /// the `SUBAGENT` identity and the task title.
    pub fn subagent_role(&self) -> Option<String> {
        match &self.kind {
            MessageKind::ToolStep { profile, .. } => profile.clone(),
            _ => None,
        }
    }

    /// The subagent's task description (the `description` argument), truncated
    /// for display. Shown as the title of the Subagent page header.
    pub fn subagent_description(&self) -> String {
        let MessageKind::ToolStep { arguments, .. } = &self.kind else {
            return "Subagent".to_string();
        };
        let label = parse_arguments_kv(arguments)
            .into_iter()
            .find(|(k, _)| k == "description")
            .map(|(_, v)| v)
            .unwrap_or_else(|| "Subagent".to_string());
        truncate(&label, 48)
    }

    /// One-line live "peek" at the subagent's current activity, e.g.
    /// `running Grep "foo"  12s` or `running thinking  8s`. Shown as the
    /// step's second row while the subagent runs and replaced in place by
    /// [`Self::subagent_outcome_line`] when the step terminates. Returns `None`
    /// for non-task steps and for terminal steps (the outcome row owns the
    /// second row then). The elapsed timer is derived from `started_at` at
    /// render time, so the line stays fresh on every animation tick without
    /// storing any ticking state.
    pub fn subagent_status_line(&self) -> Option<String> {
        if !self.is_subagent_task() {
            return None;
        }
        let MessageKind::ToolStep {
            status,
            started_at,
            awaiting,
            activity,
            retry,
            children,
            ..
        } = &self.kind
        else {
            return None;
        };
        if !status.is_running() {
            return None;
        }
        let elapsed = started_at.map(|started| {
            let ms = started.elapsed().as_millis() as u64;
            if ms < 1000 {
                format!("{}ms", ms)
            } else if ms < 60_000 {
                format!("{}s", ms / 1000)
            } else {
                duration_text(Some(ms))
            }
        });
        // Rank order for the peek row, highest first:
        //
        // 1. a parked human-decision wait — the subagent is blocked on the
        //    user, and nothing it says about its own progress is actionable;
        // 2. a live setback — the child is not running, it is backing off, and
        //    the countdown is the fact the user needs. Read from the typed
        //    [`crate::retry::RetryProgress`], never parsed back out of the
        //    `waiting to retry (3s)` activity string, so the child's count is
        //    the child's actual attempt;
        // 3. the derived activity (`running <tool>` / `thinking` / the child's
        //    own reported line).
        if *awaiting {
            return Some(match elapsed {
                Some(elapsed) => format!("awaiting approval  {elapsed}"),
                None => "awaiting approval".to_string(),
            });
        }
        if let Some(progress) = retry.as_ref() {
            return Some(progress.label(std::time::Instant::now()));
        }
        let current = match children.last() {
            Some(child)
                if child.is_tool_step()
                    && child.tool_step_status().is_some_and(|s| s.is_running()) =>
            {
                // A tool step still in flight — name the tool so the row says
                // *what* is being done, not just that the parent is busy.
                Some(
                    child
                        .tool_step_summary()
                        .unwrap_or_else(|| "tool".to_string()),
                )
            }
            // Assistant text has streamed but no tool call followed it: the
            // subagent is composing between tools. A bare `starting` here read
            // as "possibly stuck" during long model calls, which is exactly
            // what the `running` prefix disambiguates.
            Some(child) if child.role == Role::Assistant && !child.raw.is_empty() => {
                Some("thinking".to_string())
            }
            // Nothing observable has landed yet. Prefer the subagent's own
            // reported activity (`waiting for model`, …) over the generic
            // `starting`: it proves the subagent is alive during the model call
            // that precedes the first child event.
            _ => activity.clone(),
        };
        // A transport wait that is not a live setback is still a pause, not progress:
        // bare phrase, same rule as `awaiting approval` above — `running` would
        // falsely claim forward motion.
        let activity = match current {
            Some(current) if current.starts_with("waiting to retry") => current,
            Some(current) => format!("running {current}"),
            None => "running".to_string(),
        };
        // The activity and its elapsed time are same-rank metadata — plain
        // whitespace (R2 on the join ladder), never a separator glyph.
        Some(match elapsed {
            Some(elapsed) => format!("{activity}  {elapsed}"),
            None => activity,
        })
    }

    /// One-line outcome replacing the peek row once the subagent terminates: the
    /// first non-empty line of its conclusion (`ToolOutput::Subagent.summary`,
    /// falling back to the legacy `output` text for restored sessions).
    /// Returns `None` for non-task steps, running steps, and terminal steps
    /// with no conclusion text.
    pub fn subagent_outcome_line(&self) -> Option<String> {
        if !self.is_subagent_task() {
            return None;
        }
        let MessageKind::ToolStep {
            status,
            output,
            structured,
            ..
        } = &self.kind
        else {
            return None;
        };
        if status.is_running() {
            return None;
        }
        let source: &str = match structured.as_deref() {
            Some(nuo_protocol::ToolOutput::Subagent { summary, .. }) => summary,
            _ => output.as_deref()?,
        };
        let conclusion = source
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .map(str::to_string)?;
        Some(conclusion)
    }
}
