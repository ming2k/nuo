//! Component-driven interactive transcript entry architecture.
//!
//! Every entry in the transcript declares its interactive capabilities,
//! focusability, custom activation semantics, text extraction for copying,
//! and stack-top key event interception.

use crossterm::event::{KeyCode, KeyModifiers};

use crate::keymap::Key;
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

    /// Handle a key event when this component sits at the stack top of focus.
    fn handle_focused_key(&mut self, key: Key) -> EntryKeyOutcome;
}

impl InteractiveEntry for TranscriptMessage {
    fn is_focusable(&self) -> bool {
        match &self.kind {
            MessageKind::ToolStep { .. }
            | MessageKind::Reasoning { .. }
            | MessageKind::CommandResult { .. }
            | MessageKind::ProviderRetry { .. }
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
            MessageKind::ProviderRetry { .. } => Some(InteractiveTargetKind::ProviderRetry),
            MessageKind::Notice { .. } => Some(InteractiveTargetKind::Notice),
            MessageKind::CompactedCard { .. } => Some(InteractiveTargetKind::CompactedCard),
            MessageKind::Text => None,
        }
    }

    fn extract_copy_text(&self) -> Option<String> {
        let text = match &self.kind {
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
            MessageKind::ProviderRetry { failure, .. } => failure.clone(),
            MessageKind::Notice { parts, .. } => parts
                .as_ref()
                .map(|p| {
                    if let Some(detail) = &p.detail {
                        format!("{}: {detail}", p.title)
                    } else {
                        p.title.clone()
                    }
                })
                .unwrap_or_else(|| self.raw.clone()),
            MessageKind::CompactedCard { summary, .. } => summary
                .clone()
                .unwrap_or_else(|| self.raw.clone()),
            MessageKind::Text => self.raw.clone(),
        };
        Some(text)
    }

    fn is_expanded(&self) -> Option<bool> {
        match &self.kind {
            MessageKind::ToolStep { expanded, .. }
            | MessageKind::Reasoning { expanded, .. }
            | MessageKind::CommandResult { expanded, .. }
            | MessageKind::ProviderRetry { expanded, .. }
            | MessageKind::Notice { expanded, .. }
            | MessageKind::CompactedCard { expanded, .. } => Some(*expanded),
            MessageKind::Text => None,
        }
    }

    fn toggle_expanded(&mut self) -> bool {
        match &mut self.kind {
            MessageKind::ToolStep {
                expanded,
                user_pinned,
                ..
            } => {
                *expanded = !*expanded;
                *user_pinned = true;
                self.refresh_tool_step();
                true
            }
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
            | MessageKind::ProviderRetry {
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
                ..
            } => {
                let changed = *cur != expanded;
                *cur = expanded;
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
            | MessageKind::ProviderRetry {
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

    fn handle_focused_key(&mut self, key: Key) -> EntryKeyOutcome {
        if !self.is_focusable() {
            return EntryKeyOutcome::Unhandled;
        }

        // Standard activation on Enter
        if key.code == KeyCode::Enter && !key.modifiers.contains(KeyModifiers::ALT) {
            if self.is_subagent_task() {
                if let Some(id) = self.tool_step_call_id() {
                    return EntryKeyOutcome::EnterSubagent(id.to_string());
                }
            }
            let changed = self.toggle_expanded();
            return EntryKeyOutcome::Handled { changed };
        }

        EntryKeyOutcome::Unhandled
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nuo_wire::Role;

    #[test]
    fn interactive_entry_contract_on_messages() {
        let mut text = TranscriptMessage::new(Role::User, "hello world");
        assert!(!text.is_focusable());
        assert_eq!(text.target_kind(), None);
        assert_eq!(text.is_expanded(), None);
        assert_eq!(text.handle_focused_key(Key::ENTER), EntryKeyOutcome::Unhandled);

        let mut reasoning = TranscriptMessage::reasoning("thinking step");
        assert!(reasoning.is_focusable());
        assert_eq!(reasoning.target_kind(), Some(InteractiveTargetKind::Reasoning));
        assert_eq!(reasoning.is_expanded(), Some(false));
        assert_eq!(
            reasoning.handle_focused_key(Key::ENTER),
            EntryKeyOutcome::Handled { changed: true }
        );
        assert_eq!(reasoning.is_expanded(), Some(true));
        assert_eq!(reasoning.extract_copy_text(), Some("thinking step".to_string()));
    }
}
