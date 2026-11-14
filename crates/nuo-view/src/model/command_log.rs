//! The **full log** of one command step, as an addressable destination.
//!
//! A folded command body shows a head, a tail, and a count for what sits
//! between them (ADR-0115). The middle is still *held* — folding is a viewport,
//! never a data choice — so the transcript owes the user a way to read it. That
//! way is a **scene**: the same full-screen destination the dashboard and
//! settings occupy, pushed onto the tab's own scene stack, so `Alt+[` /
//! breadcrumbs / `Esc`-back all work because they already work for scenes.
//!
//! Keeping it a *scene* rather than a centered dialog is a residency decision
//! (ADR-0116 `[INV-LAYER-01]`): a log is content, and content belongs to the
//! content layer. A dialog is the layer for spatial configuration surfaces; a
//! thousand-line build log rendered inside a 60-column floating panel wastes the
//! terminal and fabricates a second reading position for text that already has
//! one home.
//!
//! This module owns **no styling and no drawing** (the presentation substrate
//! stays zero-IO): it answers which rows the log has, what the command was, how
//! the process ended, and what a copy of the whole log should contain. The
//! scene renderer attaches type and paint; the keymap decides chords.

use nuo_tool::ToolOutput;

use super::layout::SemanticCursor;
use crate::tools::shell_output::{OutputCoverage, ShellOutput, command_invocation};

/// Which rows the log scene shows.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum LogScope {
    /// Every row the step holds, including the middle the transcript folded
    /// away. The default: a user who asked to "view full log" asked for the log.
    #[default]
    Full,
    /// Only the rows the folded transcript body hid, so a reader can look at
    /// what the fold was hiding without walking past context they have already
    /// read.
    HiddenOnly,
}

impl LogScope {
    /// The scope label the scene bar shows after its context line.
    pub fn label(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::HiddenOnly => "hidden",
        }
    }

    /// Toggle: a second press of the same verb flips what is being read.
    pub fn toggled(self) -> Self {
        match self {
            Self::Full => Self::HiddenOnly,
            Self::HiddenOnly => Self::Full,
        }
    }

    /// The scope a log opens at.
    ///
    /// Always [`LogScope::Full`], folded or not: the user asked to *see the
    /// log*, and a scene that opened scoped to "only what was hidden" would be an
    /// empty page for an unfolded step and a fragment for a folded one. Narrowing
    /// is an act (`h`), never an opening guess.
    pub fn for_fold(_has_fold: bool) -> Self {
        Self::Full
    }
}

/// Everything the full-log scene needs about one command step.
///
/// Derived once, at open time, from the step's own payload: the scene is a
/// *reading* of the step, not a second copy of it, so it holds rows and an
/// outcome rather than borrowing transcript state.
#[derive(Clone, Debug, Default)]
pub struct CommandLog {
    /// The step this log was derived from, in the transcript it came from.
    ///
    /// Carried so the scene can state its provenance ("which step is this?")
    /// and so re-opening after a transcript rebuild can re-derive rather than
    /// show a stale snapshot.
    pub message_idx: usize,
    /// The command line the step ran, unwrapped (`$ ` is the renderer's job).
    pub command: String,
    /// Every logical output row, carriage-return/backspace normalized.
    pub rows: Vec<String>,
    /// How many leading rows the transcript's folded body kept visible.
    pub head_rows: usize,
    /// How many trailing rows the transcript's folded body kept visible.
    pub tail_rows: usize,
    /// The parsed exit status, ignoring the synthetic `-1` sentinels.
    pub exit: Option<i32>,
    /// What each audience holds (ADR-0115 §3) — the scene must not claim a
    /// completeness the transcript's own footer refuses to claim.
    pub coverage: OutputCoverage,
    /// Which rows the scene is showing.
    pub scope: LogScope,
}

impl CommandLog {
    /// Derive the log for a command step.
    ///
    /// `output` is the flat model-facing text (the legacy / restored fallback);
    /// `structured` wins whenever it carries a [`ToolOutput::Shell`], because
    /// its interleaved `lines` are the UI-authoritative arrival order.
    pub fn derive(
        message_idx: usize,
        arguments: &str,
        output: &str,
        structured: Option<&ToolOutput>,
        fold: Option<crate::tools::shell_output::BashFold>,
    ) -> Self {
        let view = ShellOutput::derive(output, structured);
        let (head_rows, tail_rows) = match fold {
            Some(fold) => (fold.head_end, view.rows.len() - fold.tail_start),
            None => (view.rows.len(), 0),
        };
        Self {
            message_idx,
            command: command_invocation(structured, arguments),
            rows: view.rows,
            head_rows,
            tail_rows,
            exit: view.exit,
            coverage: view.coverage,
            scope: LogScope::for_fold(fold.is_some()),
        }
    }

    /// The rows the current [`LogScope`] shows, as a slice of [`Self::rows`].
    ///
    /// `Full` is the whole log — including the middle the transcript folded
    /// away, which is the entire reason this destination exists. `HiddenOnly`
    /// is that middle: everything between the head and the tail the transcript
    /// keeps visible.
    pub fn visible_rows(&self) -> &[String] {
        match self.scope {
            LogScope::Full => &self.rows,
            LogScope::HiddenOnly => {
                let start = self.head_rows.min(self.rows.len());
                let end = self.rows.len().saturating_sub(self.tail_rows).max(start);
                &self.rows[start..end]
            }
        }
    }

    /// How many rows the current scope holds.
    pub fn len(&self) -> usize {
        self.visible_rows().len()
    }

    /// Whether the current scope holds no rows (an unfolded step switched to
    /// `HiddenOnly`, for instance) — the scene says so instead of painting an
    /// empty page that looks like a failure.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// How many rows the fold hid — `0` for an unfolded step.
    pub fn hidden_len(&self) -> usize {
        self.rows
            .len()
            .saturating_sub(self.head_rows)
            .saturating_sub(self.tail_rows)
    }

    /// Whether there is anything to switch between: a scope toggle is only
    /// offered when the two scopes differ, so no chord is spent revealing an
    /// identical page.
    pub fn can_scope_toggle(&self) -> bool {
        self.hidden_len() > 0
    }

    /// The text a copy of this log should contain.
    ///
    /// The `$ command` line leads: a pasted log without its invocation is a
    /// page of output with no question attached, and the user's reason for
    /// copying is almost always to show someone else both halves. The scope
    /// still applies — copying while reading the hidden middle copies the
    /// middle — because a copy must be a receipt of what is on screen, not a
    /// different answer than the one being read (ADR-0140).
    pub fn copy_text(&self) -> String {
        let mut text = String::new();
        if !self.command.is_empty() {
            text.push_str("$ ");
            text.push_str(&self.command);
            text.push('\n');
        }
        text.push_str(&self.visible_rows().join("\n"));
        if let Some(code) = self.exit
            && code != 0
        {
            text.push('\n');
            text.push_str(&format!("exit {code}"));
        }
        text
    }

    /// The one-line receipt the toast shows after a copy: which copy landed,
    /// not that "something" was copied. Grounded in the same state that
    /// produced the bytes, so the sentence cannot describe a different slice
    /// than the clipboard received.
    pub fn copy_feedback(&self) -> String {
        let rows = self.len();
        let noun = if rows == 1 { "row" } else { "rows" };
        match self.scope {
            LogScope::Full => format!("log copied — $ command + {rows} {noun}"),
            LogScope::HiddenOnly => format!("hidden rows copied — {rows} {noun}"),
        }
    }

    /// The trailing outcome row, or `None` when the process succeeded (a
    /// successful exit is not news; a failure is).
    ///
    /// Mirrors the transcript's own rule (`payloads::draw_command_content`):
    /// only a non-zero exit earns a footer, so the scene and the body cannot
    /// disagree about whether the step failed.
    pub fn outcome_line(&self) -> Option<String> {
        match self.exit {
            Some(code) if code != 0 => Some(format!("exit {code}")),
            _ => None,
        }
    }

    /// The cursor of `row` in the log, addressed as a real document position.
    ///
    /// The log scene registers its rows against a [`BlockRegion`] keyed by
    /// `block_idx = row index`, so a drag over the scene copies through the
    /// same layout-map machinery the transcript uses — one selection path, not
    /// a second one that could drift.
    ///
    /// [`BlockRegion`]: crate::model::layout::BlockRegion
    pub fn row_cursor(&self, message_idx: usize, row: usize) -> SemanticCursor {
        SemanticCursor::new(message_idx, row, 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shell(stdout: &str, exit: Option<i32>) -> ToolOutput {
        ToolOutput::Shell {
            command: "cargo build".into(),
            stdout: stdout.into(),
            stderr: String::new(),
            lines: Vec::new(),
            exit,
            truncated: false,
            truncated_note: None,
            termination: nuo_tool::ShellTermination::Exited,
            detached_job_id: None,
        }
    }

    /// The flat model-facing text of a `Shell` payload, which is what a
    /// restored session hands the derivation.
    fn log_of(rows: usize, exit: Option<i32>) -> CommandLog {
        let stdout = (1..=rows)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let structured = shell(&stdout, exit);
        let text = structured.to_text();
        let fold = ShellOutput::derive(&text, Some(&structured)).fold();
        CommandLog::derive(
            0,
            r#"{"command":"cargo build"}"#,
            &text,
            Some(&structured),
            fold,
        )
    }

    #[test]
    fn the_full_scope_holds_every_row_the_step_has() {
        let log = log_of(40, Some(0));
        assert_eq!(log.scope, LogScope::Full);
        assert_eq!(log.len(), log.rows.len());
        assert_eq!(log.len(), 40);
    }

    #[test]
    fn the_hidden_scope_holds_exactly_what_the_fold_hides() {
        let mut log = log_of(40, Some(0));
        let hidden = log.hidden_len();
        assert!(hidden > 0, "a 40-row body must fold");
        log.scope = LogScope::HiddenOnly;
        assert_eq!(log.len(), hidden);
        // The two scopes partition the log: head + hidden + tail = all rows.
        assert_eq!(log.head_rows + hidden + log.tail_rows, log.rows.len());
        // And the hidden slice is the middle, in order.
        assert_eq!(log.visible_rows()[0], log.rows[log.head_rows]);
    }

    #[test]
    fn an_unfolded_step_refuses_the_hidden_scope() {
        let log = log_of(3, Some(0));
        assert_eq!(log.hidden_len(), 0);
        assert!(
            !log.can_scope_toggle(),
            "no chord is spent revealing an identical page"
        );
        assert_eq!(log.scope, LogScope::Full);
    }

    #[test]
    fn the_copy_text_leads_with_the_invocation_it_receipts() {
        let log = log_of(3, Some(0));
        let text = log.copy_text();
        assert_eq!(
            text, "$ cargo build\nline 1\nline 2\nline 3",
            "a pasted log without its command is output with no question attached"
        );
    }

    #[test]
    fn the_copy_text_appends_only_a_failing_exit() {
        assert!(!log_of(2, Some(0)).copy_text().contains("exit"));
        assert!(log_of(2, Some(1)).copy_text().ends_with("exit 1"));
    }

    #[test]
    fn the_copy_receipt_names_the_slice_that_was_copied() {
        let mut log = log_of(40, Some(0));
        assert!(
            log.copy_feedback().starts_with("log copied"),
            "{}",
            log.copy_feedback()
        );
        log.scope = LogScope::HiddenOnly;
        assert!(
            log.copy_feedback().starts_with("hidden rows copied"),
            "{}",
            log.copy_feedback()
        );
    }

    #[test]
    fn the_command_falls_back_to_the_arguments_without_a_payload() {
        assert_eq!(
            command_invocation(None, r#"{"command":"ls -la"}"#),
            "ls -la",
            "a restored step has no structured command but still names its call"
        );
    }
}
