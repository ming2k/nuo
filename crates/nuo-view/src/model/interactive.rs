//! Component-driven interactive transcript entry architecture.
//!
//! Every entry in the transcript declares its interactive capabilities,
//! focusability, custom activation semantics, text extraction for copying,
//! and stack-top key event interception.

use crate::model::document::{MessageKind, TranscriptMessage};
use crate::model::layout::InteractiveTargetKind;

/// Outcome of dispatching a key event to the focused transcript component.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryKeyOutcome {
    /// The component consumed the key event and changed internal/display state.
    Handled { changed: bool },
    /// The component requests entering a subagent child view (e.g. `enter_subagent`).
    EnterSubagent(String),
    /// The key was not handled by this component; the event falls through to
    /// focus navigation (prev/next target), viewport scrolling, or scene/global shortcuts.
    Unhandled,
}

/// Trait implemented by interactive transcript entry components.
pub trait InteractiveEntry {
    /// Whether this entry is currently interactive and can accept focus.
    fn is_focusable(&self) -> bool;

    /// The target category for this entry (e.g. ToolStep, Reasoning, Notice, etc.).
    fn target_kind(&self) -> Option<InteractiveTargetKind>;

    /// Extract readable plain text for clipboard copy (`y` / `c`).
    fn extract_copy_text(&self) -> Option<String>;

    /// Whether this entry is in an expanded state (if collapsible).
    fn is_expanded(&self) -> Option<bool>;

    /// Toggle expansion state (user-pinned). Returns true if the state changed.
    fn toggle_expanded(&mut self) -> bool;

    /// Force expansion state (user-pinned). Returns true if the state changed.
    fn pin_expanded(&mut self, expanded: bool) -> bool;
}

/// The entry's verbatim copy text — **exactly what it displays**.
///
/// A notice's body is stored, not re-derived at copy time
/// (`crate::model::notice_text`), so this returns the same bytes the entry
/// paints and the selection hit-test addresses. That equality is the whole
/// point: a user who drags over an error and presses copy must get that error,
/// and a user who focuses the entry and copies must get the same bytes.
pub fn copy_text(message: &TranscriptMessage) -> Option<String> {
    let text = match &message.kind {
        MessageKind::ToolStep {
            output,
            arguments,
            name,
            ..
        } => output
            .as_ref()
            .cloned()
            .unwrap_or_else(|| format!("{name} {arguments}")),
        MessageKind::Reasoning { content, .. } => content.clone(),
        MessageKind::CommandResult {
            invocation, result, ..
        } => {
            let inv_str = format!("{} {}", invocation.name, invocation.args)
                .trim()
                .to_string();
            result
                .as_ref()
                .map(|r| format!("{inv_str}: {r:?}"))
                .unwrap_or(inv_str)
        }
        // Notices and retries carry their display text verbatim: `raw` *is* the
        // normalized body (`notice_text::display_text`), and the per-line
        // blocks are that same text split at its own newlines.
        MessageKind::Notice { .. } => message.raw.clone(),
        MessageKind::CompactedCard { summary, .. } => {
            summary.clone().unwrap_or_else(|| message.raw.clone())
        }
        MessageKind::Text => message.raw.clone(),
    };
    Some(text)
}

impl InteractiveEntry for TranscriptMessage {
    fn is_focusable(&self) -> bool {
        match &self.kind {
            MessageKind::ToolStep { .. }
            | MessageKind::Reasoning { .. }
            | MessageKind::CommandResult { .. }
            | MessageKind::Notice { .. }
            | MessageKind::CompactedCard { .. } => true,
            MessageKind::Text => false,
        }
    }

    fn target_kind(&self) -> Option<InteractiveTargetKind> {
        match &self.kind {
            MessageKind::ToolStep { .. } => Some(InteractiveTargetKind::ToolStep),
            MessageKind::Reasoning { .. } => Some(InteractiveTargetKind::Reasoning),
            MessageKind::CommandResult { .. } => Some(InteractiveTargetKind::CommandResult),
            MessageKind::Notice { .. } => Some(InteractiveTargetKind::Notice),
            MessageKind::CompactedCard { .. } => Some(InteractiveTargetKind::CompactedCard),
            MessageKind::Text => None,
        }
    }

    fn extract_copy_text(&self) -> Option<String> {
        copy_text(self)
    }

    fn is_expanded(&self) -> Option<bool> {
        match &self.kind {
            MessageKind::ToolStep { expanded, .. }
            | MessageKind::Reasoning { expanded, .. }
            | MessageKind::CommandResult { expanded, .. }
            | MessageKind::Notice { expanded, .. }
            | MessageKind::CompactedCard { expanded, .. } => Some(*expanded),
            MessageKind::Text => None,
        }
    }

    fn toggle_expanded(&mut self) -> bool {
        // Command steps walk a three-rung ladder (ADR-0115): expand → reveal the
        // folded output middle → collapse. Folded-ness is asked of the same
        // derivation the renderer paints, so the reveal rung exists exactly
        // when there is something hidden to reveal.
        let ladder = match &self.kind {
            MessageKind::ToolStep {
                expanded,
                output_revealed,
                ..
            } => Some(TranscriptMessage::bash_disclosure_ladder(
                *expanded,
                *output_revealed,
                self.tool_step_folded_rows().is_some(),
            )),
            _ => None,
        };
        if let Some((next_expanded, next_revealed)) = ladder {
            if let MessageKind::ToolStep {
                expanded,
                user_pinned,
                output_revealed,
                ..
            } = &mut self.kind
            {
                *expanded = next_expanded;
                *output_revealed = next_revealed;
                *user_pinned = true;
            }
            self.refresh_tool_step();
            return true;
        }
        match &mut self.kind {
            // Handled by the ladder above.
            MessageKind::ToolStep { .. } => false,
            MessageKind::Reasoning {
                expanded,
                user_pinned,
                ..
            }
            | MessageKind::CommandResult {
                expanded,
                user_pinned,
                ..
            }
            | MessageKind::Notice {
                expanded,
                user_pinned,
                ..
            }
            | MessageKind::CompactedCard {
                expanded,
                user_pinned,
                ..
            } => {
                *expanded = !*expanded;
                *user_pinned = true;
                self.bump_rev();
                true
            }
            MessageKind::Text => false,
        }
    }

    fn pin_expanded(&mut self, expanded: bool) -> bool {
        match &mut self.kind {
            MessageKind::ToolStep {
                expanded: cur,
                user_pinned,
                output_revealed,
                ..
            } => {
                let changed = *cur != expanded;
                *cur = expanded;
                // ADR-0115: collapsing forgets the reveal, so re-expanding always
                // starts from the folded viewport.
                if !expanded {
                    *output_revealed = false;
                }
                *user_pinned = true;
                if changed {
                    self.refresh_tool_step();
                }
                changed
            }
            MessageKind::Reasoning {
                expanded: cur,
                user_pinned,
                ..
            }
            | MessageKind::CommandResult {
                expanded: cur,
                user_pinned,
                ..
            }
            | MessageKind::Notice {
                expanded: cur,
                user_pinned,
                ..
            }
            | MessageKind::CompactedCard {
                expanded: cur,
                user_pinned,
                ..
            } => {
                let changed = *cur != expanded;
                *cur = expanded;
                *user_pinned = true;
                if changed {
                    self.bump_rev();
                }
                changed
            }
            MessageKind::Text => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nuo_protocol::Role;

    #[test]
    fn interactive_entry_contract_on_messages() {
        let text = TranscriptMessage::new(Role::User, "hello world");
        assert!(!text.is_focusable());
        assert_eq!(text.target_kind(), None);
        assert_eq!(text.is_expanded(), None);

        let mut reasoning = TranscriptMessage::reasoning("thinking step");
        assert!(reasoning.is_focusable());
        assert_eq!(
            reasoning.target_kind(),
            Some(InteractiveTargetKind::Reasoning)
        );
        assert_eq!(reasoning.is_expanded(), Some(false));
        assert!(reasoning.toggle_expanded());
        assert_eq!(reasoning.is_expanded(), Some(true));
        assert_eq!(
            reasoning.extract_copy_text(),
            Some("thinking step".to_string())
        );
    }
}
