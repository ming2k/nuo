//! Command result presentation, output layout, and execution phase lifecycle.
use super::super::helpers::*;
use super::super::types::*;
use super::{TranscriptMessage, next_message_id};
use crate::model::interactive::InteractiveEntry;
use crate::model::markdown::{parse_blocks, parse_blocks_plain};
use nuo_protocol::Role;

impl TranscriptMessage {
    /// A slash-command invocation with its typed result. `name` is
    /// the command word without the leading slash (`"search"`), `"shell"` for
    /// a `!command` passthrough. The collapsed row shows the invocation; the
    /// expandable body shows `result.to_text()`. The row starts `Completed`.
    pub fn command_result(
        name: impl Into<String>,
        args: impl Into<String>,
        result: Option<nuo_protocol::CommandResult>,
    ) -> Self {
        Self::command_result_in_phase(name, args, result, CommandPhase::Completed)
    }

    /// The optimistic dispatch row: the user just sent the command
    /// and no result exists yet. Renders as the pending input half of the
    /// command component; the `RoundEvent::CommandResult` handler settles it
    /// in place via [`Self::settle_command_result`].
    pub fn pending_command(name: impl Into<String>, args: impl Into<String>) -> Self {
        Self::command_result_in_phase(name, args, None, CommandPhase::Pending)
    }

    fn command_result_in_phase(
        name: impl Into<String>,
        args: impl Into<String>,
        result: Option<nuo_protocol::CommandResult>,
        phase: CommandPhase,
    ) -> Self {
        let name = name.into();
        let args = args.into();
        let invocation = if args.is_empty() {
            format!("/{}", name)
        } else {
            format!("/{} {}", name, args)
        };
        let result_text = result
            .as_ref()
            .map(|result| result.to_text())
            .unwrap_or_default();
        // An ack's newlines are its chosen structure (headline + muted detail
        // lines), so it parses plain — the markdown parser's
        // soft-break rule would squeeze those lines onto one row. Every other
        // result keeps the markdown block renderer (lists, tables, code).
        let is_ack = matches!(result, Some(nuo_protocol::CommandResult::Ack { .. }));
        Self {
            id: next_message_id(),
            // A harness artifact, not user or model prose — the renderer gives
            // it its own dimmed command-row treatment.
            role: Role::Tool,
            blocks: if is_ack {
                parse_blocks_plain(&result_text)
            } else {
                parse_blocks(&result_text)
            },
            raw: sanitize_text(&invocation).into_owned(),
            kind: MessageKind::CommandResult {
                invocation: CommandInvocation { name, args },
                result: result.map(Box::new),
                phase,
                expanded: false,
                user_pinned: false,
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
        }
    }

    /// Settle a pending command component with its typed result:
    /// this is the *only* live path that turns [`CommandPhase::Pending`] into
    /// [`CommandPhase::Completed`], and it reuses the existing message id so
    /// the row is updated in place — one component, input and output, no
    /// second row and no seam in the transcript. Returns `false` when the
    /// message is not a pending command (an id mismatch — the pending row was
    /// dropped by a transcript rebuild — so the caller may push a fresh
    /// completed row instead).
    pub fn settle_command_result(&mut self, result: nuo_protocol::CommandResult) -> bool {
        let MessageKind::CommandResult {
            result: slot,
            phase,
            ..
        } = &mut self.kind
        else {
            return false;
        };
        if *phase != CommandPhase::Pending {
            return false;
        }
        // Keep the parsed body in sync with the stored typed result, the same
        // way the constructor derives it (acks parse plain — their newlines
        // are structure, not markdown soft breaks).
        let result_text = result.to_text();
        let is_ack = matches!(result, nuo_protocol::CommandResult::Ack { .. });
        self.blocks = if is_ack {
            parse_blocks_plain(&result_text)
        } else {
            parse_blocks(&result_text)
        };
        // Command results never stream; freeze the whole body so a stray
        // `push_stream` degrades to a safe full re-parse.
        self.resume_byte = self.raw.len();
        self.live_blocks = 0;
        *slot = Some(Box::new(result));
        *phase = CommandPhase::Completed;
        true
    }

    /// Mark a pending command component as never receiving a reply
    /// the input half stays readable but stops promising an
    /// output. Returns whether the transition applied.
    pub fn cancel_pending_command(&mut self) -> bool {
        if let MessageKind::CommandResult { phase, .. } = &mut self.kind
            && *phase == CommandPhase::Pending
        {
            *phase = CommandPhase::Cancelled;
            true
        } else {
            false
        }
    }

    pub fn is_command_result(&self) -> bool {
        matches!(self.kind, MessageKind::CommandResult { .. })
    }

    /// The command invocation metadata.
    pub fn command_invocation(&self) -> Option<&CommandInvocation> {
        match &self.kind {
            MessageKind::CommandResult { invocation, .. } => Some(invocation),
            _ => None,
        }
    }

    pub fn command_name(&self) -> Option<&str> {
        self.command_invocation().map(|c| c.name.as_str())
    }

    pub fn command_args(&self) -> Option<&str> {
        self.command_invocation().map(|c| c.args.as_str())
    }

    pub fn command_result_expanded(&self) -> Option<bool> {
        match &self.kind {
            MessageKind::CommandResult { expanded, .. } => Some(*expanded),
            _ => None,
        }
    }

    /// The lifecycle phase of a command component.
    pub fn command_result_phase(&self) -> Option<CommandPhase> {
        match &self.kind {
            MessageKind::CommandResult { phase, .. } => Some(*phase),
            _ => None,
        }
    }

    /// The render layout for this command row: `Plain` when there
    /// is no result, `Inline` when a single-line reply fits beside the
    /// invocation, `Disclose` otherwise. A `Pending` row has no result yet and
    /// always classifies `Plain` — the phase owns its presentation until the
    /// reply settles. `available_width` is the row's usable columns.
    ///
    /// A present `sent_at_ms` renders a trailing `HH:MM` timestamp,
    /// so the method reserves that span before classifying — the free
    /// classifier stays purely shape-based and timestamp-blind.
    pub fn command_row_layout(&self, available_width: usize) -> Option<CommandRowLayout> {
        match &self.kind {
            MessageKind::CommandResult { result, phase, .. } => {
                if *phase == CommandPhase::Pending {
                    return Some(CommandRowLayout::Plain);
                }
                let usable = if self.sent_at_ms.is_some() {
                    available_width.saturating_sub(SENT_TIME_LABEL_COLS)
                } else {
                    available_width
                };
                Some(command_row_layout(result.as_deref(), &self.raw, usable))
            }
            _ => None,
        }
    }

    /// User-driven disclosure change: force `expanded` and mark it pinned so
    /// later transitions leave it alone.
    pub fn pin_command_result_expanded(&mut self, expanded: bool) {
        self.pin_expanded(expanded);
    }

    /// The invocation text shown on the collapsed command row (`/search foo`,
    /// `!ls -la`), from the message `raw`.
    pub fn command_result_summary(&self) -> Option<String> {
        if self.is_command_result() {
            Some(self.raw.clone())
        } else {
            None
        }
    }

    /// The typed result body text (`to_text`), when the record
    /// carries a result.
    pub fn command_result_text(&self) -> Option<String> {
        match &self.kind {
            MessageKind::CommandResult {
                result: Some(result),
                ..
            } => Some(result.to_text()),
            _ => None,
        }
    }

    /// The typed result itself, when this row carries one.
    pub fn command_result_payload(&self) -> Option<&nuo_protocol::CommandResult> {
        match &self.kind {
            MessageKind::CommandResult { result, .. } => result.as_deref(),
            _ => None,
        }
    }

    /// The headline/detail split for an ack reply (two-tone ack):
    /// the title alone when there is no detail, `None` for non-acks.
    pub fn command_ack_split(&self) -> Option<(&str, &[String])> {
        command_ack_split(self.command_result_payload())
    }
}
