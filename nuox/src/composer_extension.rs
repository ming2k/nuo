//! Composer Extension Architecture
//!
//! Floating overlays that project above or below the Composer (History Search,
//! Slash Commands, Mention/Path completions) are strictly **Composer Extension States**,
//! never self-contained dialogs.
//!
//! ### Architectural Principles:
//! 1. **Composer as Single Surface Host**: Floating extension cards render only data
//!    and candidates. They must never draw self-contained footers or headers with redundant hint bars.
//! 2. **Single Origin of Hints (Zero Dual Footers)**: The Composer Hint bar is the sole
//!    authoritative renderer of active keyboard affordances across all extensions.
//! 3. **Strict 3-Layer Hierarchical Key Dispatch Pipeline**:
//!    - **Layer 1 (Global Lifecycle)**: Modal dismissal, double Ctrl+C exits.
//!    - **Layer 2 (Active Extension Interceptor)**: Candidate navigation (↑/↓), item pruning (Shift+Delete), selection acceptance (Enter/Tab).
//!    - **Layer 3 (Composer Text Engine)**: Unbounded text editing (Backspace, DeleteForward, typing printable characters, Ctrl+C buffer clear).

use crate::input::InputAction;
use crate::keymap::{Key, LiveHint};

/// Exhaustive discriminator for extensions attaching to the Composer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ComposerExtensionKind {
    /// Cross-session prompt history search (Ctrl+R).
    HistorySearch,
    /// Slash command autocomplete popup (/model, /new, etc.).
    SlashCompletion,
    /// Path and mention autocomplete popup (@file, @dir).
    PathCompletion,
}

impl ComposerExtensionKind {
    /// Returns the static implementation of [`ComposerExtension`] for this kind.
    pub fn extension(&self) -> &'static dyn ComposerExtension {
        match self {
            Self::HistorySearch => &HistorySearchExtension,
            Self::SlashCompletion => &SlashCompletionExtension,
            Self::PathCompletion => &PathCompletionExtension,
        }
    }
}

/// The architectural contract governing all Composer Extensions.
pub trait ComposerExtension: Send + Sync {
    /// Distinct identifier for active state comparison.
    fn kind(&self) -> ComposerExtensionKind;

    /// Key affordances advertised in the Composer Hint bar.
    fn live_hints(&self) -> &'static [LiveHint];

    /// Layer 2 Key Interceptor: returns `Some(action)` if handled,
    /// or `None` to let the key fall through to the Layer 3 text editing engine.
    fn intercept_key(&self, key: Key, keys: &crate::session::SceneKeys) -> Option<InputAction>;
}

/// History Search (`Ctrl+R`) extension implementation.
pub struct HistorySearchExtension;

impl ComposerExtension for HistorySearchExtension {
    fn kind(&self) -> ComposerExtensionKind {
        ComposerExtensionKind::HistorySearch
    }

    fn live_hints(&self) -> &'static [LiveHint] {
        crate::modal_keys::live_history_hints()
    }

    fn intercept_key(&self, key: Key, _keys: &crate::session::SceneKeys) -> Option<InputAction> {
        crate::modal_keys::resolve_history_search_key(key)
    }
}

/// Slash command completion extension implementation.
pub struct SlashCompletionExtension;

impl ComposerExtension for SlashCompletionExtension {
    fn kind(&self) -> ComposerExtensionKind {
        ComposerExtensionKind::SlashCompletion
    }

    fn live_hints(&self) -> &'static [LiveHint] {
        const SLASH_HINTS: &[LiveHint] = &[
            LiveHint::nav(Key::ESC, "dismiss"),
            LiveHint::action(Key::TAB, "select"),
            LiveHint::action(Key::ENTER, "select"),
        ];
        SLASH_HINTS
    }

    fn intercept_key(&self, key: Key, keys: &crate::session::SceneKeys) -> Option<InputAction> {
        match key.code {
            crossterm::event::KeyCode::Esc if !keys.completion_dismissed => {
                Some(InputAction::CloseCompletion)
            }
            crossterm::event::KeyCode::Tab
                if keys.suggestion_count > 0 && !keys.completion_dismissed =>
            {
                let idx = keys.suggestion_index.unwrap_or(0);
                Some(InputAction::CommitSuggestion(idx.to_string()))
            }
            _ => None,
        }
    }
}

/// Path / Mention completion extension implementation.
pub struct PathCompletionExtension;

impl ComposerExtension for PathCompletionExtension {
    fn kind(&self) -> ComposerExtensionKind {
        ComposerExtensionKind::PathCompletion
    }

    fn live_hints(&self) -> &'static [LiveHint] {
        const PATH_HINTS: &[LiveHint] = &[
            LiveHint::nav(Key::ESC, "dismiss"),
            LiveHint::action(Key::TAB, "select"),
            LiveHint::action(Key::ENTER, "select"),
        ];
        PATH_HINTS
    }

    fn intercept_key(&self, key: Key, keys: &crate::session::SceneKeys) -> Option<InputAction> {
        match key.code {
            crossterm::event::KeyCode::Esc if !keys.completion_dismissed => {
                Some(InputAction::CloseCompletion)
            }
            crossterm::event::KeyCode::Tab
                if keys.suggestion_count > 0 && !keys.completion_dismissed =>
            {
                let idx = keys.suggestion_index.unwrap_or(0);
                Some(InputAction::CommitSuggestion(idx.to_string()))
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyCode;

    #[test]
    fn test_composer_extensions_have_live_hints() {
        for kind in [
            ComposerExtensionKind::HistorySearch,
            ComposerExtensionKind::SlashCompletion,
            ComposerExtensionKind::PathCompletion,
        ] {
            let ext = kind.extension();
            assert_eq!(ext.kind(), kind);
            let hints = ext.live_hints();
            assert!(!hints.is_empty(), "extension {kind:?} must provide hints");
        }
    }

    #[test]
    fn test_history_extension_intercepts_shift_delete_not_bare_delete() {
        let ext = HistorySearchExtension;
        let keys = crate::session::SceneKeys::default();

        // Shift+Delete is intercepted
        assert_eq!(
            ext.intercept_key(Key::SHIFT_DELETE, &keys),
            Some(InputAction::HistoryDeleteSelected)
        );

        // Bare Delete falls through (returns None)
        let bare_delete = Key {
            modifiers: crossterm::event::KeyModifiers::NONE,
            code: KeyCode::Delete,
        };
        assert_eq!(ext.intercept_key(bare_delete, &keys), None);
    }

    #[test]
    fn test_completion_extensions_intercept_esc_and_tab() {
        let slash_ext = SlashCompletionExtension;
        let path_ext = PathCompletionExtension;

        let keys = crate::session::SceneKeys {
            completion_dismissed: false,
            suggestion_count: 2,
            suggestion_index: Some(1),
            ..Default::default()
        };

        assert_eq!(
            slash_ext.intercept_key(Key::ESC, &keys),
            Some(InputAction::CloseCompletion)
        );
        assert_eq!(
            path_ext.intercept_key(Key::ESC, &keys),
            Some(InputAction::CloseCompletion)
        );

        assert_eq!(
            slash_ext.intercept_key(Key::TAB, &keys),
            Some(InputAction::CommitSuggestion("1".into()))
        );
        assert_eq!(
            path_ext.intercept_key(Key::TAB, &keys),
            Some(InputAction::CommitSuggestion("1".into()))
        );
    }

    #[test]
    fn test_compose_target_for_extension_mapping() {
        use crate::components::composer_hints::{ComposeTarget, compose_target_for_extension};

        let history_target = compose_target_for_extension(
            false,
            None,
            false,
            Some(ComposerExtensionKind::HistorySearch),
            false,
        );
        assert_eq!(history_target, ComposeTarget::HistorySearch);

        let slash_target = compose_target_for_extension(
            false,
            None,
            false,
            Some(ComposerExtensionKind::SlashCompletion),
            false,
        );
        assert_eq!(
            slash_target,
            ComposeTarget::Completion {
                kind: crate::completion::CompletionKind::Slash
            }
        );

        let path_target = compose_target_for_extension(
            false,
            None,
            false,
            Some(ComposerExtensionKind::PathCompletion),
            false,
        );
        assert_eq!(
            path_target,
            ComposeTarget::Completion {
                kind: crate::completion::CompletionKind::Path
            }
        );

        let prompt_target = compose_target_for_extension(false, None, false, None, false);
        assert_eq!(prompt_target, ComposeTarget::Prompt);

        // ADR-0192: the inline recall pointer outranks busy/slash derivation —
        // the buffer holds a history row, and that content fact wins.
        let recall_target = compose_target_for_extension(true, None, false, None, true);
        assert_eq!(recall_target, ComposeTarget::HistoryRecall);
    }
}
