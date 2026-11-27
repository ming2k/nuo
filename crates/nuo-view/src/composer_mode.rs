//! Composer Canonical Quadrant Modality (ADR-0051).
//!
//! Enforces the four mutually exclusive canonical interaction modes of the
//! Composer ([`ComposerMode`]: `Edit`, `Recall`, `Search`, `Complete`) and
//! provides the unified mode dispatch.
//!
//! The *draft* half of ADR-0051 no longer lives here. It had grown a second,
//! lossy encoding in this file (`DraftCapsule`: no view offset, caret re-derived
//! on restore) while the per-tab context carried a third. Both were retired by
//! [ADR-0247](../../../docs/adr/0247-a-draft-belongs-to-a-thread-and-outlives-every-view-transition.md):
//! unsent input is one value type, [`crate::composer_draft::ComposerDraft`],
//! shared by the transient stash and the parked-draft registry.

/// Canonical completion category for [`ComposerMode::Complete`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CompleteKind {
    /// Slash command autocomplete (`/model`, `/new`, etc.).
    Command,
    /// Path and context mention autocomplete (`@file`, `@dir`, etc.).
    Context,
}

impl CompleteKind {
    /// Human-readable label for status/hints.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Command => "Command",
            Self::Context => "Context",
        }
    }
}

/// The four canonical mutually exclusive interaction modes of the Composer ([INV-COMP-01]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ComposerMode {
    /// 1. Edit: The foundational free-text drafting state (Readline key conventions).
    #[default]
    Edit,

    /// 2. Recall: In-place session prompt stepping via ↑/↓/Alt-P/Alt-N ([INV-COMP-02]).
    Recall {
        /// 0-based index into the session history newest-first order.
        history_index: usize,
    },

    /// 3. Search: Global fuzzy history query via Ctrl-R ([INV-COMP-04]).
    Search {
        /// Current filter query text.
        query: String,
        /// Highlighted index in ranked candidates.
        selected_index: usize,
    },

    /// 4. Complete: Inline token-attached completion.
    Complete {
        /// Category of completion.
        kind: CompleteKind,
        /// Current trigger/filter needle.
        needle: String,
        /// Highlighted completion item index.
        selected_index: usize,
    },
}

impl ComposerMode {
    /// Whether the composer is in the default editing posture.
    pub fn is_edit(&self) -> bool {
        matches!(self, Self::Edit)
    }

    /// Whether the composer is in in-place history stepping.
    pub fn is_recall(&self) -> bool {
        matches!(self, Self::Recall { .. })
    }

    /// Whether the composer is in history search mode.
    pub fn is_search(&self) -> bool {
        matches!(self, Self::Search { .. })
    }

    /// Whether the composer is in inline completion mode.
    pub fn is_complete(&self) -> bool {
        matches!(self, Self::Complete { .. })
    }

    /// Human-readable semantic mode label for status indicators and chrome.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Edit => "Edit",
            Self::Recall { .. } => "Recall",
            Self::Search { .. } => "Search",
            Self::Complete { kind, .. } => kind.label(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_composer_mode_defaults_and_predicates() {
        let mode = ComposerMode::default();
        assert_eq!(mode, ComposerMode::Edit);
        assert!(mode.is_edit());
        assert!(!mode.is_recall());
        assert!(!mode.is_search());
        assert!(!mode.is_complete());
        assert_eq!(mode.label(), "Edit");

        let recall = ComposerMode::Recall { history_index: 2 };
        assert!(recall.is_recall());
        assert_eq!(recall.label(), "Recall");

        let search = ComposerMode::Search {
            query: "git".into(),
            selected_index: 0,
        };
        assert!(search.is_search());
        assert_eq!(search.label(), "Search");

        let complete = ComposerMode::Complete {
            kind: CompleteKind::Command,
            needle: "mod".into(),
            selected_index: 1,
        };
        assert!(complete.is_complete());
        assert_eq!(complete.label(), "Command");
    }
}
