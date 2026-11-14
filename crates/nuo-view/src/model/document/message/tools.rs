//! Tool step lifecycle, streaming stdout/stderr, and disclosure states.
use super::super::helpers::*;
use super::super::types::*;
use super::{TranscriptMessage, next_message_id};
use crate::model::interactive::InteractiveEntry;
use nuo_tool::Role;

impl TranscriptMessage {
    /// ADR-0166: whether this step is still ingesting its arguments (announced,
    /// not yet dispatched). This is a property of the lifecycle, not of a
    /// sentinel id: `Ingesting` is the only provisional state, and a settled
    /// attempt must not contain one. A retraction removes exactly the rows for
    /// which this is true; a dispatched call is real ([`Self::merge_tool_step`]).
    pub fn is_pending_tool_step(&self) -> bool {
        matches!(
            &self.kind,
            MessageKind::ToolStep {
                status: ToolStepStatus::Ingesting,
                ..
            }
        )
    }

    /// ADR-0166: merge an incoming tool-step fact into this row, monotonically.
    ///
    /// The reducer is idempotent and order-tolerant by construction: the
    /// lifecycle only advances (`Ingesting → Executing → terminal`), and a fact
    /// carries a field only when that field is known. Applying the same fact
    /// twice therefore equals applying it once, and a delayed duplicate
    /// (a replayed announcement, a retry that crossed a dispatch) can never
    /// regress a row that already moved on.
    ///
    /// Returns whether anything changed.
    pub fn merge_tool_step(&mut self, incoming: &TranscriptMessage) -> bool {
        let MessageKind::ToolStep {
            id: incoming_id,
            name: incoming_name,
            arguments: incoming_arguments,
            status: incoming_status,
            ..
        } = &incoming.kind
        else {
            return false;
        };
        let MessageKind::ToolStep {
            id,
            name,
            arguments,
            status,
            input_bytes,
            execution_started_at,
            ..
        } = &mut self.kind
        else {
            return false;
        };
        // The applier resolves by id, so this is a defensive equality check, not
        // a tie-break: two different calls can never be merged into one row.
        if id != incoming_id {
            return false;
        }
        // A fact from an earlier phase cannot rewrite a row that already moved
        // on. This is what stops a replayed announcement from overwriting the
        // full name a dispatch supplied with the partial prefix the
        // announcement happened to carry.
        let current_rank = status.rank();
        let incoming_rank = incoming_status.rank();
        if incoming_rank < current_rank {
            return false;
        }
        let mut changed = false;
        if !incoming_name.is_empty() && name != incoming_name {
            *name = incoming_name.clone();
            changed = true;
        }
        if !incoming_arguments.is_empty() && arguments != incoming_arguments {
            *arguments = incoming_arguments.clone();
            changed = true;
        }
        if incoming_rank > current_rank {
            *status = *incoming_status;
            changed = true;
            if status.is_executing() {
                // Arguments are complete; the pre-dispatch byte count stops
                // being meaningful and execution begins now.
                *input_bytes = None;
                *execution_started_at = Some(std::time::Instant::now());
            }
        }
        if changed {
            self.refresh_tool_step();
        }
        changed
    }

    /// ADR-0026: record count-only input progress on this announced step.
    /// The caller has already resolved the step by call id.
    ///
    /// ADR-0166: only an `Ingesting` step has a meaningful byte count. Once the
    /// call dispatches the arguments are complete, so a late or replayed tick
    /// must not resurrect the pre-dispatch state.
    /// Returns whether the count moved.
    pub fn set_tool_input_bytes(&mut self, bytes: usize) -> bool {
        let MessageKind::ToolStep {
            input_bytes,
            status,
            ..
        } = &mut self.kind
        else {
            return false;
        };
        if !status.is_ingesting() || *input_bytes == Some(bytes) {
            return false;
        }
        *input_bytes = Some(bytes);
        true
    }

    /// ADR-0046: record streaming partial arguments on this announced step.
    /// The caller has already resolved the step by call id.
    ///
    /// ADR-0166: a partial argument stream is never the call's arguments. This
    /// is a no-op once the step dispatches, so a late or replayed fragment can
    /// never overwrite the complete object with a prefix.
    /// Returns whether the arguments changed.
    pub fn set_tool_streaming_arguments(&mut self, args: String) -> bool {
        let MessageKind::ToolStep {
            arguments, status, ..
        } = &mut self.kind
        else {
            return false;
        };
        if !status.is_ingesting() || *arguments == args {
            return false;
        }
        *arguments = args;
        self.refresh_tool_step();
        true
    }

    /// ADR-0026: collapse a pre-dispatch step onto its dispatch `call_id` and complete arguments —
    /// re-key the id, record arguments, and clear the slot/byte pre-dispatch state in one step.
    /// ADR-0046: transition from Ingesting to Executing phase by capturing `execution_started_at`.
    /// ADR-0125: attach the pure preview of what this call will do.
    ///
    /// Stored in `structured` — the same field a completed step renders its diff
    /// from — rather than in a parallel one. A preview *is* the structured output the
    /// step would produce, so reusing the field means the preview card and the result
    /// card are drawn by one renderer and cannot disagree about what the change
    /// looks like. The status stays `Executing` (or `Ingesting`), so nothing mistakes
    /// a preview for a completed result; the terminal `ToolResult` overwrites the field
    /// with what actually happened.
    ///
    /// Returns whether the step matched, so the caller can distinguish "previewed an
    /// in-flight step" from "no step exists yet".
    pub fn set_tool_preview(&mut self, call_id: &str, preview: nuo_tool::PreviewResult) -> bool {
        let MessageKind::ToolStep { id, structured, .. } = &mut self.kind else {
            return false;
        };
        if id != call_id {
            return false;
        }
        *structured = Some(Box::new(preview.as_patch()));
        self.refresh_tool_step();
        true
    }

    /// A dispatched tool step: the arguments are complete and local execution is
    /// (about to be) under way.
    ///
    /// ADR-0166: the call id is supplied by the harness at the announcement and
    /// is the row's only identity. The status is explicit — there is no
    /// empty-id sentinel to infer it from.
    pub fn tool_step(
        id: impl Into<String>,
        name: impl Into<String>,
        arguments: impl Into<String>,
    ) -> Self {
        Self::tool_step_in(id, name, arguments, ToolStepStatus::Executing)
    }

    /// ADR-0026 / ADR-0166: a tool call whose **name is known but whose
    /// arguments are still streaming**. It carries the same call id the later
    /// dispatched fact will carry, so the reducer merges into this row instead
    /// of inserting a second one.
    pub fn tool_step_announced(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self::tool_step_in(id, name, String::new(), ToolStepStatus::Ingesting)
    }

    fn tool_step_in(
        id: impl Into<String>,
        name: impl Into<String>,
        arguments: impl Into<String>,
        status: ToolStepStatus,
    ) -> Self {
        let execution_started_at = status.is_executing().then(std::time::Instant::now);
        let mut message = Self {
            id: next_message_id(),
            role: Role::Tool,
            blocks: Vec::new(),
            raw: String::new(),
            kind: MessageKind::ToolStep {
                id: id.into(),
                name: name.into(),
                profile: None,
                arguments: arguments.into(),
                output: None,
                structured: None,
                status,
                expanded: false,
                user_pinned: false,
                output_revealed: false,
                duration_ms: None,
                started_at: Some(std::time::Instant::now()),
                execution_started_at,
                awaiting: false,
                activity: None,
                retry: None,
                input_bytes: None,
                children: Vec::new(),
            },
            delivery: DeliveryStatus::default(),
            insert_id: None,
            origin_tags: Vec::new(),
            origin: UserMessageOrigin::Chat,
            provider: None,
            model: None,
            effort: None,
            round: None,
            turn: None,
            sent_at_ms: None,
            injection_origin: None,
            rev: 0,
            resume_byte: 0,
            live_blocks: 0,
        };
        message.refresh_tool_step();
        message
    }

    pub fn finish_tool_step(
        &mut self,
        id: &str,
        output: impl Into<String>,
        structured: nuo_tool::ToolOutput,
        duration_ms: u64,
    ) -> bool {
        let MessageKind::ToolStep {
            id: step_id,
            output: step_output,
            structured: step_structured,
            status,
            duration_ms: step_duration,
            awaiting,
            ..
        } = &mut self.kind
        else {
            return false;
        };
        if step_id != id || !status.is_running() {
            return false;
        }
        *awaiting = false;
        let output = output.into();
        // Classify from the structured result (data-level: a non-zero shell
        // exit, an explicit `ToolOutput::Error`, a `failed` subagent). The
        // legacy `starts_with("Error")` text fallback was removed once tool
        // error sites migrated to `ToolOutput::Error` and subagents carried
        // an explicit `failed` flag — classification is now fully data-driven.
        // Permission denial gets its own status so the UI shows it distinctly
        // from a runtime error.
        *status = if matches!(structured, nuo_tool::ToolOutput::PermissionDenied { .. }) {
            ToolStepStatus::Denied
        } else if matches!(
            &structured,
            nuo_tool::ToolOutput::Subagent {
                interrupted: true,
                ..
            }
        ) {
            // A cooperatively-drained subagent: the user interrupted the turn,
            // but the partial transcript was preserved. Classified before
            // `is_error()` because interruption is not a failure.
            ToolStepStatus::Interrupted
        } else if structured.is_error() {
            ToolStepStatus::Failed
        } else {
            ToolStepStatus::Ok
        };
        *step_output = Some(output);
        *step_structured = Some(Box::new(structured));
        *step_duration = Some(duration_ms);
        self.refresh_tool_step();
        true
    }

    /// Accumulate an incremental stream chunk into a still-running tool step,
    /// so the UI can render partial output (e.g. bash stdout) live. The first
    /// chunk initializes a partial [`nuo_tool::ToolOutput::Shell`]; the
    /// terminal `finish_tool_step` later overwrites it with the final result.
    /// Returns `false` if this isn't a matching running step.
    pub fn push_tool_stream(&mut self, id: &str, stream: &nuo_tool::ToolStream) -> bool {
        let MessageKind::ToolStep {
            id: step_id,
            arguments,
            structured,
            status,
            ..
        } = &mut self.kind
        else {
            return false;
        };
        if step_id != id || !status.is_running() {
            return false;
        }
        if !matches!(
            structured.as_deref(),
            Some(nuo_tool::ToolOutput::Shell { .. })
        ) {
            let cmd = parse_arguments_kv(arguments)
                .into_iter()
                .find(|(k, _)| k == "command")
                .map(|(_, v)| v)
                .unwrap_or_default();
            *structured = Some(Box::new(nuo_tool::ToolOutput::Shell {
                command: cmd,
                stdout: String::new(),
                stderr: String::new(),
                lines: Vec::new(),
                exit: None,
                truncated: false,
                // Still-streaming seed: the real termination lands with the
                // final result (`finish_tool_step`). Default until then.
                truncated_note: None,
                termination: nuo_tool::ShellTermination::default(),
                detached_job_id: None,
            }));
        }
        if let Some(nuo_tool::ToolOutput::Shell {
            stdout,
            stderr,
            lines,
            ..
        }) = structured.as_deref_mut()
        {
            // Build the TUI-authoritative `lines` view alongside the flat
            // strings so the streaming view matches the final result: stdout/stderr
            // keep their true arrival interleaving, instead of the all-stdout-then-all-stderr
            // degraded band the empty-`lines` fallback used to force.
            //
            // Each stream chunk is one complete `\n`-terminated line (bash's
            // capture is line-buffered and emits `format!("{text}\n")`), so
            // split on `\n` and tag each non-empty piece with its source
            // stream. Trailing empties (from the terminal `\n`) are dropped so
            // they don't paint phantom blank rows.
            let stream_tag = match stream {
                nuo_tool::ToolStream::Stdout(_) => nuo_tool::ShellStream::Out,
                nuo_tool::ToolStream::Stderr(_) => nuo_tool::ShellStream::Err,
            };
            let text = match stream {
                nuo_tool::ToolStream::Stdout(s) | nuo_tool::ToolStream::Stderr(s) => s,
            };
            for piece in text.split('\n') {
                if !piece.is_empty() {
                    lines.push(nuo_tool::ShellLine {
                        stream: stream_tag,
                        text: piece.to_string(),
                    });
                }
            }
            match stream {
                nuo_tool::ToolStream::Stdout(s) => stdout.push_str(s),
                nuo_tool::ToolStream::Stderr(s) => stderr.push_str(s),
            }
        }
        self.refresh_tool_step();
        true
    }

    /// Mark a still-running tool step as cancelled. Idempotent: a step that
    /// already reached a terminal state (`Ok` / `Failed` / `Cancelled`) is left
    /// untouched and returns `false`. When the step is a `task` (subagent),
    /// its still-running nested tool children are cancelled too, so an aborted
    /// subagent never leaves a "running" child step behind.
    pub fn cancel_tool_step(&mut self, id: &str) -> bool {
        let MessageKind::ToolStep {
            id: step_id,
            status,
            ..
        } = &mut self.kind
        else {
            return false;
        };
        if step_id != id || !status.is_running() {
            return false;
        }
        // Apply the transition through `cancel_all_running`, which also handles
        // the nested-children sweep and refreshes the rendered view in one
        // place.
        self.cancel_all_running()
    }

    /// Recursively cancel every still-running tool step within this message
    /// (used for subagent children and as a defensive sweep). Returns `true`
    /// if anything transitioned.
    pub fn cancel_all_running(&mut self) -> bool {
        let (step_running, child_changed) = {
            let MessageKind::ToolStep {
                status,
                started_at,
                duration_ms,
                awaiting,
                children,
                ..
            } = &mut self.kind
            else {
                return false;
            };
            let mut changed = false;
            if status.is_running() {
                *status = ToolStepStatus::Cancelled;
                *awaiting = false;
                // Freeze the elapsed time at the moment of cancellation so the
                // step stops showing a live-running timer.
                if duration_ms.is_none() {
                    *duration_ms = started_at
                        .map(|started| started.elapsed().as_millis() as u64)
                        .or(Some(0));
                }
                changed = true;
            }
            let mut child_changed = changed;
            for child in children.iter_mut() {
                child_changed |= child.cancel_all_running();
            }
            (changed, child_changed)
        };
        if step_running || child_changed {
            self.refresh_tool_step();
        }
        step_running || child_changed
    }

    /// The explicit lifecycle of a tool step, or `None` for non-tool messages.
    pub fn tool_step_status(&self) -> Option<ToolStepStatus> {
        match &self.kind {
            MessageKind::ToolStep { status, .. } => Some(*status),
            _ => None,
        }
    }

    /// The tool name a tool step carries, or `None` for non-tool messages.
    pub fn tool_step_name(&self) -> Option<&str> {
        match &self.kind {
            MessageKind::ToolStep { name, .. } => Some(name.as_str()),
            _ => None,
        }
    }

    /// The tool name a [`crate::model::search::Facet::ToolNamed`] selection can match.
    ///
    /// Subagent tasks report their bound profile (`explore`, `plan`, …) because
    /// that is the name the step is labelled with on screen and the one a user
    /// would type into a filter; a plain tool step reports its tool name.
    pub fn tool_label(&self) -> Option<&str> {
        match &self.kind {
            MessageKind::ToolStep { name, profile, .. } => {
                Some(profile.as_deref().unwrap_or(name.as_str()))
            }
            _ => None,
        }
    }

    pub fn is_tool_step(&self) -> bool {
        matches!(self.kind, MessageKind::ToolStep { .. })
    }

    pub fn tool_step_expanded(&self) -> Option<bool> {
        match &self.kind {
            MessageKind::ToolStep { expanded, .. } => Some(*expanded),
            _ => None,
        }
    }

    /// ADR-0115: whether this step's folded output middle is currently revealed.
    pub fn tool_step_output_revealed(&self) -> Option<bool> {
        match &self.kind {
            MessageKind::ToolStep {
                output_revealed, ..
            } => Some(*output_revealed),
            _ => None,
        }
    }

    /// ADR-0115: the logical output rows this step's expanded body would fold
    /// away, or `None` when the body renders verbatim. Asked by the activation
    /// ladder so a key press never promises a reveal that reveals nothing.
    ///
    /// The row derivation is `crate::tools::shell_output`'s — the same one
    /// the renderer paints — so the ladder and the drawing cannot disagree.
    pub fn tool_step_folded_rows(&self) -> Option<usize> {
        let MessageKind::ToolStep {
            name,
            output,
            structured,
            ..
        } = &self.kind
        else {
            return None;
        };
        crate::tools::shell_output::command_step_fold(
            name,
            output.as_deref(),
            structured.as_deref(),
        )
        .map(|fold| fold.hidden)
    }

    /// The ADR-0115 activation ladder for a command step's disclosure:
    ///
    /// ```text
    /// collapsed ──activate──▶ expanded, folded
    /// expanded, folded ──activate──▶ expanded, revealed   (whole log on screen)
    /// expanded, revealed ──activate──▶ collapsed
    /// expanded, verbatim ──activate──▶ collapsed          (nothing to reveal)
    /// ```
    ///
    /// The reveal rung only exists when the body actually has a folded middle,
    /// so activation never costs a key press that changes nothing. Collapsing
    /// forgets the reveal, so re-expanding always starts from the folded
    /// viewport rather than from an arbitrarily tall body.
    pub fn bash_disclosure_ladder(expanded: bool, revealed: bool, has_fold: bool) -> (bool, bool) {
        match (expanded, revealed, has_fold) {
            (false, _, _) => (true, false),
            (true, false, true) => (true, true),
            (true, _, _) => (false, false),
        }
    }

    /// Auto/system disclosure setter: sets `expanded` **unless** the user has
    /// pinned the step (in which case it's a no-op). This is what lifecycle
    /// transitions (start / finish / cancel) and step creation call, so the
    /// derived default never fights a manual choice. User-driven toggles go
    /// through [`Self::pin_tool_step_expanded`].
    pub fn set_tool_step_expanded(&mut self, expanded: bool) {
        if let MessageKind::ToolStep {
            expanded: current,
            user_pinned,
            output_revealed,
            ..
        } = &mut self.kind
        {
            if *user_pinned {
                return;
            }
            *current = expanded;
            if !expanded {
                *output_revealed = false;
            }
            self.refresh_tool_step();
        }
    }

    /// User-driven disclosure change: force `expanded` and mark it pinned so
    /// later lifecycle transitions leave it alone.
    pub fn pin_tool_step_expanded(&mut self, expanded: bool) {
        self.pin_expanded(expanded);
    }

    /// The call id of a tool step, used as the addressable identity of a
    /// subagent task for the focus stack.
    pub fn tool_step_call_id(&self) -> Option<&str> {
        match &self.kind {
            MessageKind::ToolStep { id, .. } => Some(id),
            _ => None,
        }
    }

    /// Human-readable header for the tool step (always one line).
    ///
    /// Shows only what the tool did and a duration suffix for finished
    /// states — the technical tool name lives inside the expanded body to
    /// reduce cognitive load.
    pub fn tool_step_summary(&self) -> Option<String> {
        let MessageKind::ToolStep {
            name,
            profile,
            arguments,
            status,
            duration_ms,
            input_bytes,
            ..
        } = &self.kind
        else {
            return None;
        };
        let summary = crate::tools::summary_for(name, arguments, profile.as_deref());
        // ADR-0026 / ADR-0166: while the call's arguments are still streaming
        // (`Ingesting`) append a count-only progress clause. It is a static
        // clause — never a `+`/`-` disclosure marker, never animated. The state
        // is read from the lifecycle, not from a second announce flag: one
        // truth, shared with `tool_step_semantic_summary`.
        let streaming = if status.is_ingesting() {
            match input_bytes {
                Some(bytes) => format!(" (receiving input {} KB)", bytes / 1024),
                None => " (receiving input)".to_string(),
            }
        } else {
            String::new()
        };
        Some(match status {
            ToolStepStatus::Ingesting => format!("{summary}{streaming}"),
            ToolStepStatus::Executing => {
                let verb = match name.as_str() {
                    "edit_text" | "edit" | "edit_file" | "write_file" | "write" => "applying",
                    "execute_command" => "running",
                    _ => "executing",
                };
                format!("{summary} ({verb})")
            }
            ToolStepStatus::Ok => match duration_ms {
                Some(ms) => format!("{summary}{streaming} ({})", duration_text(Some(*ms))),
                None => format!("{summary}{streaming}"),
            },
            ToolStepStatus::Failed => {
                format!(
                    "{summary}{streaming} (failed {})",
                    duration_text(*duration_ms)
                )
            }
            ToolStepStatus::Denied => {
                format!(
                    "{summary}{streaming} (denied {})",
                    duration_text(*duration_ms)
                )
            }
            ToolStepStatus::Cancelled => {
                format!(
                    "{summary}{streaming} (cancelled {})",
                    duration_text(*duration_ms)
                )
            }
            ToolStepStatus::Interrupted => {
                format!(
                    "{summary}{streaming} (interrupted {})",
                    duration_text(*duration_ms)
                )
            }
        })
    }

    /// Structured semantic header for the tool step and optional trailing status badge.
    pub fn tool_step_semantic_summary(
        &self,
        workspace_root: Option<&std::path::Path>,
    ) -> Option<(
        nuo_ui_primitives::components::inline_layout::SemanticLine<'static>,
        Option<String>,
    )> {
        let MessageKind::ToolStep {
            name,
            profile,
            arguments,
            status,
            duration_ms,
            structured,
            input_bytes,
            ..
        } = &self.kind
        else {
            return None;
        };
        let effective_args_buf;
        let effective_args = if arguments.trim().is_empty() {
            if let Some(box_structured) = structured {
                match &**box_structured {
                    nuo_tool::ToolOutput::Patch { path, old, new, .. } => {
                        effective_args_buf = serde_json::json!({
                            "path": path,
                            "old_string": old,
                            "new_string": new,
                        })
                        .to_string();
                        &effective_args_buf
                    }
                    _ => arguments.as_str(),
                }
            } else {
                arguments.as_str()
            }
        } else {
            arguments.as_str()
        };
        let semantic_line = crate::tools::semantic_summary_for(
            name,
            effective_args,
            profile.as_deref(),
            workspace_root,
        );
        let suffix = match status {
            ToolStepStatus::Ingesting => {
                let s = match input_bytes {
                    Some(bytes) => format!(" (receiving input {} KB)", bytes / 1024),
                    None => " (receiving input)".to_string(),
                };
                Some(s)
            }
            ToolStepStatus::Executing => {
                let verb = match name.as_str() {
                    "edit_text" | "edit" | "edit_file" | "write_file" | "write" => "applying",
                    "execute_command" => "running",
                    _ => "executing",
                };
                Some(format!(" ({verb})"))
            }
            ToolStepStatus::Ok => duration_ms.map(|ms| format!(" ({})", duration_text(Some(ms)))),
            ToolStepStatus::Failed => Some(format!(" (failed {})", duration_text(*duration_ms))),
            ToolStepStatus::Denied => Some(format!(" (denied {})", duration_text(*duration_ms))),
            ToolStepStatus::Cancelled => {
                Some(format!(" (cancelled {})", duration_text(*duration_ms)))
            }
            ToolStepStatus::Interrupted => {
                Some(format!(" (interrupted {})", duration_text(*duration_ms)))
            }
        };
        Some((semantic_line, suffix))
    }

    /// ADR-0046: the typed execution phase of this tool step.
    pub fn tool_execution_phase(&self) -> Option<ToolExecutionPhase> {
        let MessageKind::ToolStep {
            status,
            input_bytes,
            execution_started_at,
            started_at,
            ..
        } = &self.kind
        else {
            return None;
        };
        match status {
            ToolStepStatus::Ingesting => Some(ToolExecutionPhase::Ingesting {
                bytes: *input_bytes,
            }),
            ToolStepStatus::Executing => {
                let elapsed_ms = execution_started_at
                    .or(*started_at)
                    .map(|t| t.elapsed().as_millis() as u64)
                    .unwrap_or(0);
                Some(ToolExecutionPhase::Executing { elapsed_ms })
            }
            terminal => Some(ToolExecutionPhase::Terminal(*terminal)),
        }
    }
}
