//! The copy pipeline: **what** gets copied, resolved in one place.
//!
//! # Why a pipeline instead of call-site formatting
//!
//! Copy used to be assembled at each trigger (a drag selection here, a focused
//! entry there, an OAuth field in a third place), so the same user gesture
//! could yield different bytes depending on which chord fired, an empty
//! selection could silently overwrite the clipboard with `""`, and a new
//! entry kind had no copy behaviour until someone remembered to add an arm.
//!
//! [`CopyRequest`] is now the single vocabulary:
//!
//! - **`Selection`** — the drag/whole-block selection the user made, resolved
//!   from the semantic document model (never from terminal-wrapped text).
//! - **`Entry`** — one transcript entry, verbatim.
//! - **`Diagnostic`** — one transcript entry as a paste-ready record: a
//!   one-line header naming route/attempt/timestamp/thread/log, a blank line,
//!   then the same verbatim body.
//!
//! Everything resolves through [`CopyRequest::resolve`], which returns
//! [`CopyPayload`] — owned text **plus the feedback sentence** — or
//! [`CopyError`] when there is nothing to copy. Callers therefore cannot write
//! an empty string to the clipboard, and the toast a user sees is derived from
//! the same decision that produced the bytes.
//!
//! # Why the clipboard write is not here
//!
//! Resolution is pure: it reads `App` and returns a value. The write is an
//! async, timeout-bounded, out-of-loop task
//! ([`super::clipboard_ops::spawn_clipboard_copy`]). Keeping them separate is
//! what lets every rule above be unit-tested without a clipboard or a terminal.

use nuo_view::model::diagnostic::{DiagnosticContext, DiagnosticRecord};
use nuo_view::model::document::TranscriptMessage;

use crate::shell::App;
use crate::shell::event_loop::transcript::extract_selection_text;

/// What the user asked to copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CopyRequest {
    /// The current semantic selection (drag, whole block, table cell, or the
    /// live input buffer).
    Selection,
    /// One transcript entry, verbatim.
    Entry { message_idx: usize },
    /// One transcript entry as a diagnostic record.
    Diagnostic { message_idx: usize },
}

/// A resolved, ready-to-write clipboard payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CopyPayload {
    pub text: String,
    /// The one-line confirmation the toast shows. Phrased as a receipt of what
    /// actually landed on the clipboard, so a user who copies the wrong thing
    /// finds out from the toast rather than from a paste.
    pub feedback: String,
}

/// Why a copy request produced nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CopyError {
    /// There is no selection to copy.
    NoSelection,
    /// The selected region resolved to no text (a chrome row, a spacer, a
    /// zero-width hit). Distinct from [`Self::NoSelection`] so the message can
    /// point at the right remedy.
    EmptySelection,
    /// The entry has no text of its own (a headless chrome row).
    EmptyEntry,
    /// The request names a message that is not in the current view — a stale
    /// focus left behind by a transcript rebuild.
    MissingEntry,
}

impl CopyError {
    /// The sentence to show the user. Deliberately actionable: each variant
    /// names the thing that *is* copyable, because the failure is almost always
    /// "you aimed at chrome, not at text".
    pub(crate) fn message(self) -> &'static str {
        match self {
            CopyError::NoSelection => {
                "nothing selected — drag over text, or focus an entry and press y"
            }
            CopyError::EmptySelection => "that row has no text — drag over the entry body",
            CopyError::EmptyEntry => "this entry has no text to copy",
            CopyError::MissingEntry => "that entry is no longer on screen",
        }
    }
}

impl CopyRequest {
    /// Resolve the request against the live app state.
    ///
    /// Pure: reads `App`, returns owned text. Every copy trigger in the shell
    /// funnels through here, so the clipboard can never receive text from one
    /// path that another path would describe differently.
    pub(crate) fn resolve(
        self,
        app: &App,
        viewed_session_id: &str,
    ) -> Result<CopyPayload, CopyError> {
        match self {
            CopyRequest::Selection => {
                let text = extract_selection_text(
                    &app.selection,
                    app.focused_messages(),
                    &app.input,
                    &app.ui.document,
                    app.drag.cell_info.as_ref(),
                )
                .ok_or(CopyError::NoSelection)?;
                if text.is_empty() {
                    return Err(CopyError::EmptySelection);
                }
                Ok(CopyPayload {
                    text,
                    feedback: "selection copied to clipboard".to_string(),
                })
            }
            CopyRequest::Entry { message_idx } => {
                let message = app
                    .focused_messages()
                    .get(message_idx)
                    .ok_or(CopyError::MissingEntry)?;
                let text = nuo_view::model::interactive::copy_text(message)
                    .filter(|text| !text.trim().is_empty())
                    .ok_or(CopyError::EmptyEntry)?;
                Ok(CopyPayload {
                    text,
                    feedback: format!("{} copied to clipboard", entry_noun(message)),
                })
            }
            CopyRequest::Diagnostic { message_idx } => {
                let message = app
                    .focused_messages()
                    .get(message_idx)
                    .ok_or(CopyError::MissingEntry)?;
                let record =
                    DiagnosticRecord::from_message(message).ok_or(CopyError::EmptyEntry)?;
                let log_path = client_log_path_for_report();
                let text = record.render(&DiagnosticContext {
                    thread_id: Some(viewed_session_id),
                    log_path: log_path.as_deref(),
                });
                Ok(CopyPayload {
                    text,
                    feedback: "diagnostic copied — paste it where the problem is read".to_string(),
                })
            }
        }
    }

    /// The request for "copy the thing the keyboard is on", with the fallbacks
    /// that make one chord work everywhere.
    ///
    /// A live selection wins over a focused entry (it is the more specific
    /// gesture), and a focused entry wins over nothing. A notice-only fallback
    /// is deliberately absent: guessing which entry the user meant is how copy
    /// lands on the wrong bytes, which is exactly what this pipeline exists to
    /// prevent.
    pub(crate) fn for_current_focus(app: &App) -> Option<Self> {
        if app.selection.is_active() {
            return Some(CopyRequest::Selection);
        }
        if let Some(target) = app.focused_target {
            return Some(CopyRequest::Entry {
                message_idx: target.message_idx,
            });
        }
        if matches!(
            app.selection,
            nuo_view::model::selection::SelectionState::Block { .. }
                | nuo_view::model::selection::SelectionState::TableCell { .. }
        ) {
            return Some(CopyRequest::Selection);
        }
        None
    }
}

/// The diagnostic sibling of [`CopyRequest::for_current_focus`].
pub(crate) fn diagnostic_for_current_focus(app: &App) -> Option<CopyRequest> {
    app.focused_target.map(|target| CopyRequest::Diagnostic {
        message_idx: target.message_idx,
    })
}

/// Where this client writes its rolling log, if logging is enabled.
///
/// The record carries the path because "now check the log" is useless advice
/// without it, and `NUO_LOG=off` legitimately means there is no file to name —
/// in which case the field is omitted rather than pointing at a path that does
/// not exist.
///
/// The path is composed, never probed: `RollingFileAppender` has no stable
/// accessor for the file it currently holds, and constructing one to ask would
/// create a file as a side effect of merely *copying* something. The glob
/// suffix is deliberate — the appender rotates daily, so the exact filename is
/// a function of the day the paste is read, not the day it was copied.
pub(crate) fn client_log_path_for_report() -> Option<String> {
    if std::env::var("NUO_LOG").is_ok_and(|level| level.eq_ignore_ascii_case("off")) {
        return None;
    }
    let dir = nuo_host::paths::get().log_dir();
    Some(format!("{}/client.log*", dir.display()))
}

/// One noun per entry kind, for the confirmation sentence.
fn entry_noun(message: &TranscriptMessage) -> &'static str {
    match message.diagnostic().map(|record| record.kind) {
        Some(nuo_view::model::diagnostic::EntryKind::Notice) => "notice",
        Some(nuo_view::model::diagnostic::EntryKind::ProviderRetry) => "retry notice",
        Some(nuo_view::model::diagnostic::EntryKind::ToolStep) => "step output",
        Some(nuo_view::model::diagnostic::EntryKind::Reasoning) => "thinking",
        Some(nuo_view::model::diagnostic::EntryKind::CommandResult) => "command result",
        Some(nuo_view::model::diagnostic::EntryKind::CompactedCard) => "summary",
        Some(nuo_view::model::diagnostic::EntryKind::Text) | None => "entry",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::app::App;
    use nuo_view::model::document::{NoticeSeverity, TranscriptMessage};
    use nuo_view::model::layout::InteractiveTarget;
    use nuo_view::model::selection::SelectionState;

    fn app_with(messages: Vec<TranscriptMessage>) -> App {
        let mut app = crate::shell::tests::new_app_for_relay_tests();
        app.messages = messages;
        app
    }

    #[test]
    fn an_entry_that_cannot_produce_text_is_never_reported_as_copied() {
        let mut message = TranscriptMessage::new(nuo_wire::Role::Assistant, "text");
        message.raw = String::new();
        message.blocks = Vec::new();
        let app = app_with(vec![message]);

        // An entry with no text must fail loudly rather than succeed with "".
        assert_eq!(
            CopyRequest::Entry { message_idx: 0 }.resolve(&app, "t"),
            Err(CopyError::EmptyEntry)
        );
    }

    #[test]
    fn an_empty_selection_is_a_distinct_failure_from_no_selection() {
        let app = app_with(vec![]);
        assert_eq!(
            CopyRequest::Selection.resolve(&app, "t"),
            Err(CopyError::NoSelection)
        );
    }

    #[test]
    fn a_stale_focus_names_a_missing_entry_instead_of_silently_copying_nothing() {
        let app = app_with(vec![]);
        assert_eq!(
            CopyRequest::Entry { message_idx: 3 }.resolve(&app, "t"),
            Err(CopyError::MissingEntry)
        );
        assert_eq!(
            CopyRequest::Diagnostic { message_idx: 3 }.resolve(&app, "t"),
            Err(CopyError::MissingEntry)
        );
    }

    #[test]
    fn a_notice_copies_its_verbatim_body() {
        let message = TranscriptMessage::notice(
            NoticeSeverity::Error,
            "OpenAI HTTP 429 Too Many Requests\n{\"error\":{\"code\":\"1305\"}}",
        );
        let app = app_with(vec![message.clone()]);

        let payload = CopyRequest::Entry { message_idx: 0 }
            .resolve(&app, "t")
            .expect("notice copies");
        assert_eq!(payload.text, message.raw);
        assert_eq!(payload.feedback, "notice copied to clipboard");
    }

    #[test]
    fn a_diagnostic_payload_is_a_header_blank_line_then_the_same_body() {
        let mut message = TranscriptMessage::provider_retry(
            2,
            6,
            std::time::Instant::now(),
            "OpenAI HTTP 429 Too Many Requests: {\"error\":{\"code\":\"1305\"}}",
        );
        message.provider = Some("openai".into());
        message.model = Some("gpt-5".into());
        let app = app_with(vec![message.clone()]);

        let payload = CopyRequest::Diagnostic { message_idx: 0 }
            .resolve(&app, "01HXYZ")
            .expect("diagnostic copies");
        let (header, body) = payload.text.split_once("\n\n").expect("header/body");
        assert!(header.starts_with("[nuo diagnostic] kind=provider-retry"));
        assert!(header.contains("severity=warning"));
        assert!(header.contains("attempt=2/6"));
        assert!(header.contains("thread=01HXYZ"));
        assert_eq!(body, message.raw, "the body stays verbatim");
    }

    #[test]
    fn an_active_selection_wins_over_a_focused_entry() {
        let app = app_with(vec![TranscriptMessage::new(nuo_wire::Role::User, "hi")]);
        let mut app = app;
        app.selection = SelectionState::Block {
            message_idx: 0,
            block_idx: 0,
        };
        app.focused_target = Some(InteractiveTarget::notice(0));
        assert_eq!(
            CopyRequest::for_current_focus(&app),
            Some(CopyRequest::Selection)
        );
    }

    #[test]
    fn with_nothing_focused_there_is_no_request_to_guess_from() {
        let app = app_with(vec![TranscriptMessage::notice(NoticeSeverity::Error, "x")]);
        assert_eq!(CopyRequest::for_current_focus(&app), None);
        assert_eq!(diagnostic_for_current_focus(&app), None);
    }
}
