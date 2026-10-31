//! Focused-entry key dispatch — the shell side of transcript interaction.
//!
//! Transcript entries declare their *content* capabilities in
//! [`nuo_view::model::interactive::InteractiveEntry`], which is pure. Deciding what
//! a key press means is an input concern, so the dispatch lives here in the
//! shell layer and the model stays free of the terminal driver
//! ([INV-ARCH-UI-ZERO-IO], ADR-0053).

use crate::shell::keymap::{KeyCode, KeyModifiers};

use crate::shell::keymap::Key;
use nuo_view::model::document::TranscriptMessage;
use nuo_view::model::interactive::{EntryKeyOutcome, InteractiveEntry};

/// Dispatch a key event to the stack-top focused transcript entry.
pub(crate) fn handle_focused_entry_key(
    message: &mut TranscriptMessage,
    key: Key,
) -> EntryKeyOutcome {
    if !message.is_focusable() {
        return EntryKeyOutcome::Unhandled;
    }

    // Standard activation on Enter, plus Space — the same toggle pair the
    // dialogs use, and what the compaction card's own hint advertises.
    let is_activate = (key.code == KeyCode::Enter && !key.modifiers.contains(KeyModifiers::ALT))
        || (key.code == KeyCode::Char(' ')
            && !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER));
    if is_activate {
        if message.is_subagent_task() {
            if let Some(id) = message.tool_step_call_id() {
                return EntryKeyOutcome::EnterSubagent(id.to_string());
            }
        }
        let changed = message.toggle_expanded();
        return EntryKeyOutcome::Handled { changed };
    }

    EntryKeyOutcome::Unhandled
}

#[cfg(test)]
mod tests {
    use super::*;
    use nuo_protocol::Role;
    use nuo_view::model::interactive::InteractiveEntry;

    fn space(modifiers: KeyModifiers) -> Key {
        Key {
            code: KeyCode::Char(' '),
            modifiers,
        }
    }

    /// A non-focusable entry must decline every key, including activation.
    #[test]
    fn non_focusable_entries_never_activate() {
        let mut text = TranscriptMessage::new(Role::User, "hello world");
        assert_eq!(
            handle_focused_entry_key(&mut text, Key::ENTER),
            EntryKeyOutcome::Unhandled
        );
        assert_eq!(
            handle_focused_entry_key(&mut text, space(KeyModifiers::NONE)),
            EntryKeyOutcome::Unhandled
        );
    }

    /// `Enter` and `Space` are the activation pair, matching the dialog toggle
    /// convention — and matching what the compaction card's own hint
    /// advertises. A modifier keeps Space available to the global layer.
    #[test]
    fn space_activates_a_focused_component_like_enter() {
        let mut by_enter = TranscriptMessage::reasoning("thinking step");
        let mut by_space = TranscriptMessage::reasoning("thinking step");
        assert_eq!(
            handle_focused_entry_key(&mut by_enter, Key::ENTER),
            EntryKeyOutcome::Handled { changed: true }
        );
        assert_eq!(
            handle_focused_entry_key(&mut by_space, space(KeyModifiers::NONE)),
            EntryKeyOutcome::Handled { changed: true }
        );
        assert_eq!(by_enter.is_expanded(), by_space.is_expanded());

        // A modified Space is not activation; it belongs to the global layer.
        let mut ctrl_space = TranscriptMessage::reasoning("thinking step");
        assert_eq!(
            handle_focused_entry_key(&mut ctrl_space, space(KeyModifiers::CONTROL)),
            EntryKeyOutcome::Unhandled
        );
    }
}
