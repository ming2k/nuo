//! Core TranscriptMessage entity and lifecycle mutations.
use nuo_tool::Role;

use super::blocks::Block;
use super::helpers::*;
use super::inlines::Inline;
use super::types::*;
use crate::model::markdown::{ParseResume, parse_blocks, parse_blocks_plain, parse_blocks_tracked};
use crate::model::search::{self, Facets};

/// Monotonic source of per-message identities. A message keeps its `id` across
/// the per-frame clone into `App::messages`, so the renderer
/// can use it as a stable cache key for the message's laid-out height (see the
/// height cache in `render`). Ids are process-unique; cloning a message copies
/// its id (a clone represents the same logical message), which is exactly what
/// the height cache wants.
static NEXT_MESSAGE_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

pub(super) fn next_message_id() -> u64 {
    NEXT_MESSAGE_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

#[derive(Debug, Clone)]
pub struct TranscriptMessage {
    /// Stable, process-unique identity used as the renderer's height-cache key.
    /// Assigned at construction and preserved across clones.
    pub id: u64,
    pub role: Role,
    pub blocks: Vec<Block>,
    /// The original raw markdown/text, preserved byte-for-byte. Copy resolves
    /// against the parsed `blocks` (rendered plain text), so this is retained
    /// for re-parsing, layout, and any consumer that needs the source form.
    pub raw: String,
    pub kind: MessageKind,
    /// What kind of user message this `Role::User` message is. Defaults to
    /// [`UserMessageOrigin::Chat`]; slash commands and shell passthroughs mark
    /// themselves so they are not mistaken for the round's driving prompt.
    pub origin: UserMessageOrigin,
    /// Lifecycle of this message from the user's point of view. Only
    /// `Role::User` messages ever carry anything but the default
    /// [`DeliveryStatus::Delivered`]. The renderer keys off this to draw a
    /// sending or cancelled panel; there is no pending/queued variant because
    /// a pending message is not in the document at all (ADR-0137).
    pub delivery: DeliveryStatus,
    /// The harness-side input id this row was admitted under
    /// (`AgentRequest::Steer` / `AgentRequest::FollowUp` -> `SteerAdmitted` /
    /// `FollowUpStarted`). It is set **at admission**, never before, and it
    /// exists so the outbox can drop the matching pending item and so a
    /// restored transcript keeps the correlation. `None` for every row the
    /// harness did not admit from the queue.
    pub insert_id: Option<String>,
    /// Admission provenance for the renderer (ADR-0137): where this message
    /// landed causally. An `InRound` steer renders as a one-body-row
    /// interjection; `RoundOpening` (or no tags) renders at full round-panel
    /// weight. This is **render metadata, never dialogue** — it is not
    /// serialized, and the restore engine reconstructs it from the durable
    /// `InjectionKind`.
    pub origin_tags: Vec<SteerOriginTag>,
    /// Provider/solution id that produced this message, mirrored from the
    /// core [`nuo_tool::Message`] so the transcript stays traceable across
    /// model switches. `None` for messages that don't carry attribution.
    pub provider: Option<String>,
    /// Model id that produced this message, companion to [`TranscriptMessage::provider`].
    pub model: Option<String>,
    /// The reasoning effort (depth) this message's model request ran with
    /// (`"high"`, `"max"`, …), when the active channel exposes one. Stamped at
    /// the same point as [`TranscriptMessage::model`] so the turn header can
    /// show the depth a given turn actually ran at. `None` for non-reasoning
    /// channels and messages that carry no attribution.
    pub effort: Option<String>,
    /// The user-visible round this message belongs to (1-indexed). Driving
    /// user messages open a round; assistant-side messages inherit it.
    pub round: Option<u64>,
    /// The ReAct turn this assistant-side message belongs to within its round
    /// (1-indexed, stamped from `TurnStarted`). The renderer uses the
    /// `(round, turn)` position to identify compact tool batches. `None`
    /// means the position is unknown; legacy tool batches retain a compatible
    /// flush-stack fallback.
    pub turn: Option<u64>,
    /// Wall-clock send time for transcript headers, in Unix epoch milliseconds.
    /// Restored messages use the persisted millisecond value when available and
    /// fall back to the durable core-message timestamp for legacy sessions.
    ///
    /// This is the **only** temporal fact a header carries, so an entry that
    /// never gets stamped renders `› turn N  model effort` with no clock at all —
    /// the reader then cannot tell when the message was produced. Two rules keep
    /// it from going missing:
    ///
    /// - Every component of one turn (thinking, assistant text, tool steps)
    ///   carries the **same** value: the band paints one `HH:MM`, so the stamp is
    ///   a property of the turn, not of whichever component opened the band.
    /// - Admission repairs a missing stamp from the turn that already has one
    ///   ([`TranscriptMessage::adopt_sent_at_ms`]), so a builder call site that
    ///   forgets to stamp itself cannot silently drop the header's clock.
    ///
    /// The wire keeps it out of provider requests (`Message::to_wire` zeroes it):
    /// it is storage/UI metadata, never conversation content.
    pub sent_at_ms: Option<u64>,
    /// Durable provenance / injection origin stamped by the harness (ADR-0050).
    pub injection_origin: Option<nuo_tool::InjectionOrigin>,
    /// Component state revision. Incremented on mutations so the cascade
    /// layout cache instantly detects if remeasurement is required.
    pub rev: u64,
    /// Incremental parse state: byte offset in `raw` where the
    /// still-open final construct begins. Everything before it is a frozen
    /// prefix whose blocks can never change; `push_stream` re-parses only
    /// from here, making streaming parse cost O(delta) amortized.
    resume_byte: usize,
    /// Number of trailing blocks in `blocks` produced by that open construct
    /// (the region a suffix parse replaces). The rest of `blocks` is frozen.
    live_blocks: usize,
}

/// Borrowed view over a compaction checkpoint card: how many messages were
/// folded into the checkpoint, the token window before/after compaction, the
/// summary body, the tracked-file list, and whether the card is expanded.
pub struct CompactedCardData<'a> {
    pub archived_messages: usize,
    pub tokens_before: usize,
    pub tokens_after: usize,
    pub summary: Option<&'a str>,
    pub tracked_files: &'a [String],
    pub expanded: bool,
}

mod commands;
mod notices;
mod subagents;
mod tools;

impl TranscriptMessage {
    pub fn new(role: Role, raw: impl Into<String>) -> Self {
        let raw = sanitize_text(&raw.into()).into_owned();
        // User messages are rendered verbatim as plain text — no markdown
        // interpretation — so pasted text containing markdown-like syntax
        // does not get mangled into headings/code fences/lists and the
        // transcript stays readable. The raw text becomes a single `Text`
        // block; `wrap_text` preserves intra-block line breaks.
        let (blocks, resume) = if role == Role::User {
            let blocks = parse_blocks_plain(&raw);
            let resume = ParseResume {
                resume_offset: raw.len(),
                live_len: 0,
            };
            (blocks, resume)
        } else {
            parse_blocks_tracked(&raw)
        };
        Self {
            id: next_message_id(),
            role,
            blocks,
            raw,
            kind: MessageKind::Text,
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
            resume_byte: resume.resume_offset,
            live_blocks: resume.live_len,
        }
    }

    /// Increment state revision after a mutation to invalidate stale height cache entries.
    pub fn bump_rev(&mut self) {
        self.rev = self.rev.wrapping_add(1);
    }

    /// Label this `Role::User` message with its turn origin (slash command /
    /// shell passthrough). No-op for non-user messages, which never surface
    /// an origin. Builder-style, used alongside `queued`.
    pub fn with_origin(mut self, origin: UserMessageOrigin) -> Self {
        self.origin = origin;
        self
    }

    /// Stamp admission provenance tags (ADR-0137). Render metadata only —
    /// never serialized, reconstructed on restore from the durable
    /// `InjectionKind`.
    pub fn with_origin_tags(mut self, tags: Vec<SteerOriginTag>) -> Self {
        self.origin_tags = tags;
        self
    }

    /// Whether this message carries the given admission-provenance tag.
    pub fn has_origin_tag(&self, tag: SteerOriginTag) -> bool {
        self.origin_tags.contains(&tag)
    }

    /// User prompt classification (Normal / Steer / FollowUp).
    pub fn prompt_kind(&self) -> UserPromptKind {
        match self.origin {
            UserMessageOrigin::Chat => UserPromptKind::Normal,
            UserMessageOrigin::Steer => UserPromptKind::Steer,
            UserMessageOrigin::FollowUp => UserPromptKind::FollowUp,
            _ => UserPromptKind::Normal,
        }
    }

    /// Whether this message is currently sending / waiting for response.
    pub fn is_sending(&self) -> bool {
        self.delivery == DeliveryStatus::Sending
    }

    /// Whether this message was cancelled / interrupted.
    pub fn is_cancelled(&self) -> bool {
        self.delivery == DeliveryStatus::Cancelled
    }

    /// Mark this message as sending (in-flight prompt waiting for response).
    pub fn sending(mut self) -> Self {
        self.delivery = DeliveryStatus::Sending;
        self
    }

    /// Mark this message as cancelled / interrupted.
    pub fn cancelled(mut self) -> Self {
        self.delivery = DeliveryStatus::Cancelled;
        self
    }

    /// Cancel this prompt in place if it is sending, queued, or an unpositioned user prompt.
    pub fn cancel_prompt(&mut self) {
        if self.delivery == DeliveryStatus::Sending
            || (self.role == Role::User && self.round.is_none())
        {
            self.delivery = DeliveryStatus::Cancelled;
            self.rev += 1;
        }
    }

    /// Settle an in-flight sending prompt to delivered when model starts responding.
    pub fn settle_delivered(&mut self) {
        if self.delivery == DeliveryStatus::Sending {
            self.delivery = DeliveryStatus::Delivered;
            self.rev += 1;
        }
    }

    /// Correlate this message with a busy-Enter steer by its
    /// harness-side input id, so the response listener can settle the entry
    /// when the insert is admitted or handed back. Builder-style companion of
    /// `queued`.
    pub fn with_insert_id(mut self, insert_id: impl Into<String>) -> Self {
        self.insert_id = Some(insert_id.into());
        self
    }

    /// Stamp the provider/solution id and model that produced this message.
    pub fn with_attribution(
        mut self,
        provider: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        self.provider = Some(provider.into());
        self.model = Some(model.into());
        self
    }

    /// Stamp the reasoning effort (depth) this message's model request ran
    /// with. Kept separate from [`Self::with_attribution`] so existing call
    /// sites (and their tests) keep their arity; pass `None` (or skip the
    /// call) when the channel exposes no effort.
    pub fn with_effort(mut self, effort: Option<impl Into<String>>) -> Self {
        self.effort = effort.map(Into::into);
        self
    }

    /// Stamp the enclosing user round.
    pub fn with_round(mut self, round: u64) -> Self {
        self.round = Some(round);
        self
    }

    /// Stamp the ReAct turn within the enclosing round.
    pub fn with_turn(mut self, turn: u64) -> Self {
        self.turn = Some(turn);
        self
    }

    /// Stamp the visible send time for a user-authored message.
    pub fn with_sent_at_ms(mut self, sent_at_ms: u64) -> Self {
        self.sent_at_ms = Some(sent_at_ms);
        self
    }

    /// The `(provider, model)` pair to show as an attribution badge, when this
    /// message carries at least a model. Used by the renderer to label which
    /// model produced a turn; `None` when the message has no attribution
    /// (user/system messages, or untagged history).
    pub fn attribution_label(&self) -> Option<(String, String)> {
        let model = self.model.clone()?;
        let provider = self.provider.clone().unwrap_or_default();
        Some((provider, model))
    }

    /// The set of roles this entry plays, for facet filtering.
    ///
    /// Classification lives here, beside the enum it classifies, so a new
    /// [`MessageKind`] cannot be added without the compiler pointing at this
    /// function. It is a *set* because entries genuinely are several things at
    /// once: a failed shell call is a tool step and an error, a subagent task is
    /// a tool step carrying other entries.
    pub fn facets(&self) -> Facets {
        match &self.kind {
            MessageKind::Text => {
                if self.role == Role::User {
                    Facets::USER
                } else {
                    Facets::TEXT
                }
            }
            MessageKind::Reasoning { .. } => Facets::THOUGHT,
            MessageKind::ToolStep {
                status, structured, ..
            } => {
                let failed_status =
                    matches!(status, ToolStepStatus::Failed | ToolStepStatus::Denied);
                let failed_output = structured
                    .as_deref()
                    .is_some_and(nuo_tool::ToolOutput::is_error);
                if failed_status || failed_output {
                    Facets::TOOL.union(Facets::ERROR)
                } else {
                    Facets::TOOL
                }
            }
            MessageKind::Notice { severity, .. } => {
                if *severity == NoticeSeverity::Error {
                    Facets::NOTICE.union(Facets::ERROR)
                } else {
                    Facets::NOTICE
                }
            }
            MessageKind::CommandResult { .. } => Facets::COMMAND,
            // A compaction checkpoint is harness-emitted status about the
            // thread, not dialogue from either party.
            MessageKind::CompactedCard { .. } => Facets::NOTICE,
        }
    }

    /// Visit each searchable text region of this entry, in *field-priority*
    /// order, borrowing the source text.
    ///
    /// This is the single declaration of "what is searchable" for the whole
    /// application: [`search::find`] keeps no copy of the document, it walks
    /// these borrowed segments. A new [`MessageKind`] therefore gets correct
    /// search behaviour by adding one arm here — and gets *no* search behaviour
    /// if it is forgotten, rather than silently searching stale state.
    ///
    /// `visit` returns `true` to stop early (used to keep the first match in
    /// each entry). Returns whether the walk was stopped.
    pub fn visit_search_segments(
        &self,
        visit: &mut impl FnMut(search::Field, &str) -> bool,
    ) -> bool {
        match &self.kind {
            MessageKind::Text => {
                if !self.raw.is_empty() && visit(search::Field::Prose, &self.raw) {
                    return true;
                }
            }
            MessageKind::Reasoning { content, .. } => {
                if visit(search::Field::Thought, content) {
                    return true;
                }
            }
            MessageKind::ToolStep {
                name,
                arguments,
                output,
                children,
                ..
            } => {
                if visit(search::Field::ToolName, name) {
                    return true;
                }
                if !arguments.is_empty() && visit(search::Field::ToolArgs, arguments) {
                    return true;
                }
                if let Some(output) = output.as_deref().filter(|text| !text.is_empty())
                    && visit(search::Field::ToolOutput, output)
                {
                    return true;
                }
                // A subagent's own transcript is where its work actually lives;
                // excluding it would make a delegated task unfindable by its
                // content, which is the opposite of the point.
                for child in children {
                    if child.visit_search_segments(visit) {
                        return true;
                    }
                }
            }
            MessageKind::Notice { parts, .. } => {
                if let Some(parts) = parts.as_deref() {
                    if let Some(topic) = parts.topic.as_deref()
                        && visit(search::Field::Notice, topic)
                    {
                        return true;
                    }
                    if visit(search::Field::Notice, &parts.title) {
                        return true;
                    }
                    if let Some(detail) = parts.detail.as_deref()
                        && visit(search::Field::Notice, detail)
                    {
                        return true;
                    }
                }
                if !self.raw.is_empty() && visit(search::Field::Notice, &self.raw) {
                    return true;
                }
            }
            MessageKind::CommandResult { invocation, .. } => {
                if visit(search::Field::Command, &invocation.name) {
                    return true;
                }
                if !invocation.args.is_empty() && visit(search::Field::Command, &invocation.args) {
                    return true;
                }
                if let Some(text) = self.command_result_text()
                    && visit(search::Field::Command, &text)
                {
                    return true;
                }
            }
            MessageKind::CompactedCard {
                summary,
                tracked_files,
                ..
            } => {
                if let Some(summary) = summary.as_deref()
                    && visit(search::Field::Prose, summary)
                {
                    return true;
                }
                for file in tracked_files {
                    if visit(search::Field::Prose, file) {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Adopt `sent_at_ms` if this entry has no send time yet.
    ///
    /// The band header shows **one** `HH:MM` per turn, and every component of
    /// that turn (thinking, assistant text, tool steps) must therefore read the
    /// same instant. A component created by an arm that forgot to stamp itself
    /// adopts the turn's already-established time instead of silently rendering
    /// a header with no clock — the failure this method exists to make
    /// unreachable. An already-stamped entry is never rewritten: its own moment
    /// is a fact, not a default.
    pub fn adopt_sent_at_ms(&mut self, sent_at_ms: Option<u64>) {
        if self.sent_at_ms.is_none() {
            self.sent_at_ms = sent_at_ms;
        }
    }

    /// The turn this entry belongs to, as a `(round, turn)` pair. `None` when
    /// either position is unknown (legacy entries), which is also exactly when
    /// the renderer falls back to unbanded flow.
    pub fn turn_position(&self) -> Option<(u64, u64)> {
        Some((self.round?, self.turn?))
    }

    pub(crate) fn refresh_tool_step(&mut self) {
        let MessageKind::ToolStep {
            id: _,
            name,
            profile,
            arguments,
            output,
            structured,
            status,
            expanded,
            user_pinned: _,
            output_revealed: _,
            duration_ms,
            started_at: _,
            execution_started_at: _,
            awaiting: _,
            activity: _,
            retry: _,
            input_bytes,
            children: _,
        } = &self.kind
        else {
            return;
        };
        if *expanded {
            // Expanded tool-step bodies are rendered directly from the
            // structured data (see draw_tool_step), not from parsed
            // markdown. We still populate `blocks` so semantic selection and
            // copy work.
            //
            // A **command** step is the one case where the painted rows are a
            // derivation rather than the raw payload: the renderer draws
            // `$ invocation` and then the rows
            // [`ShellOutput`](crate::tools::shell_output::ShellOutput) derived —
            // marker-stripped, carriage-return-resolved, in arrival order. The
            // blocks must be *that same derivation*, or a drag over a row would
            // slice a different string than the one on screen: the offsets the
            // layout registers are offsets into the derived rows, so storing
            // anything else here makes every selection in a command body land on
            // the wrong bytes. ADR-0115's "one derivation, three audiences" —
            // painter, clipboard, and now the document model.
            if crate::tools::shell_output::is_command_body(name, structured.as_deref()) {
                let command = crate::tools::shell_output::command_invocation(
                    structured.as_deref(),
                    arguments,
                );
                let rows = crate::tools::shell_output::ShellOutput::derive(
                    output.as_deref().unwrap_or(""),
                    structured.as_deref(),
                )
                .rows;
                // Block 0 is the invocation, block 1 the output rows. The
                // renderer addresses them the same way, so a selection that
                // starts on the `$` line and runs into the log copies the
                // command and the rows under it as one ordinary range.
                self.raw = command.clone();
                let mut blocks = vec![Block::Text(Inline::plain(command))];
                if !rows.is_empty() {
                    let body = rows.join("\n");
                    self.raw.push_str("\n\n");
                    self.raw.push_str(&body);
                    blocks.push(Block::Text(Inline::plain(body)));
                }
                self.blocks = blocks;
                self.resume_byte = self.raw.len();
                self.live_blocks = 0;
                return;
            }
            let kv = parse_arguments_kv(arguments);
            let display_args: String = kv
                .iter()
                .map(|(k, v)| format!("{}: {}", k, v))
                .collect::<Vec<_>>()
                .join("\n");
            self.raw = display_args.clone();
            let mut blocks = vec![Block::Text(Inline::plain(display_args))];
            if let Some(out) = output {
                self.raw.push_str("\n\n");
                self.raw.push_str(out);
                blocks.push(Block::Text(Inline::plain(out.clone())));
            }
            self.blocks = blocks;
            self.resume_byte = self.raw.len();
            self.live_blocks = 0;
        } else {
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
            let summary = crate::tools::summary_for(name, effective_args, profile.as_deref());
            let suffix = match status {
                ToolStepStatus::Ingesting => match input_bytes {
                    Some(bytes) => format!(" (receiving input {} KB)", bytes / 1024),
                    None => " (receiving input)".to_string(),
                },
                ToolStepStatus::Executing => {
                    let verb = match name.as_str() {
                        "edit_text" | "edit" | "edit_file" | "write_file" | "write" => "applying",
                        "execute_command" => "running",
                        _ => "executing",
                    };
                    format!(" ({verb})")
                }
                ToolStepStatus::Ok => format!(" ({})", duration_text(*duration_ms)),
                ToolStepStatus::Failed => format!(" (failed {})", duration_text(*duration_ms)),
                ToolStepStatus::Denied => format!(" (denied {})", duration_text(*duration_ms)),
                ToolStepStatus::Cancelled => {
                    format!(" (cancelled {})", duration_text(*duration_ms))
                }
                ToolStepStatus::Interrupted => {
                    format!(" (interrupted {})", duration_text(*duration_ms))
                }
            };
            self.raw = format!("{}{}", summary, suffix);
            self.blocks = parse_blocks(&self.raw);
            // Tool summaries never stream; freeze the body (safe default for
            // any stray suffix parse).
            self.resume_byte = self.raw.len();
            self.live_blocks = 0;
        }
    }

    /// Re-parse blocks from raw text (e.g. after streaming append).
    pub fn reparse(&mut self) {
        self.relayout();
        self.bump_rev();
    }

    /// Full tracked re-layout of `raw` without bumping the revision (used at
    /// construction, where the rev is still fresh).
    fn relayout(&mut self) {
        let (blocks, resume) = parse_blocks_tracked(&self.raw);
        self.blocks = blocks;
        self.resume_byte = resume.resume_offset;
        self.live_blocks = resume.live_len;
    }

    /// Append streaming text and re-parse **incrementally**.
    ///
    /// Only the still-open tail construct is re-parsed: everything before
    /// `resume_byte` is a frozen prefix whose blocks can never be modified by
    /// future input (markdown blocks are line-oriented and, once terminated
    /// by a blank line / closing fence / non-continuation line, immutable).
    /// Parsing every accumulated chunk keeps the live layout structurally
    /// consistent with the final layout — the previous append-only Text block
    /// path delayed all Markdown structure until StreamEnd, causing the whole
    /// response to jump when headings, lists, and code fences were discovered
    /// — but per-frame cost now grows with the delta, not the message.
    pub fn push_stream(&mut self, delta: &str) {
        self.raw.push_str(&sanitize_text(delta));
        self.refresh_stream_tail();
    }

    /// Re-parse only the live tail region and splice it onto the frozen
    /// prefix. The junction [`Block::Break`] between the frozen prefix and
    /// the suffix is re-decided here with the same pair rule the parser's
    /// `push_block` applies, so the result is byte-for-byte what a full
    /// `parse_blocks(&self.raw)` would produce.
    fn refresh_stream_tail(&mut self) {
        if self.resume_byte > self.raw.len() {
            // Defensive: raw was replaced without a relayout — fall back to
            // a full re-parse.
            self.resume_byte = 0;
            self.live_blocks = 0;
        }
        let frozen = self.blocks.len().saturating_sub(self.live_blocks);
        // Immutable borrow of `raw` while `blocks` is mutated — disjoint
        // fields, so the live region parses in place without a copy.
        let (tail, resume) = parse_blocks_tracked(&self.raw[self.resume_byte..]);
        let junction_break = match (
            frozen.checked_sub(1).and_then(|i| self.blocks.get(i)),
            tail.first(),
        ) {
            (Some(prev), Some(next)) => {
                !(matches!(prev, Block::Break)
                    || matches!(prev, Block::ListItem { .. })
                        && matches!(next, Block::ListItem { .. }))
            }
            _ => false,
        };
        self.blocks.truncate(frozen);
        let live_starts_at_zero = tail.len() == resume.live_len;
        if junction_break {
            self.blocks.push(Block::Break);
        }
        self.blocks.extend(tail);
        // The junction Break belongs to the live region only when it sits
        // directly in front of it (the suffix's live region starts at 0);
        // otherwise it precedes frozen suffix content.
        self.live_blocks = resume.live_len + usize::from(junction_break && live_starts_at_zero);
        self.resume_byte += resume.resume_offset;
        self.bump_rev();
    }
}
