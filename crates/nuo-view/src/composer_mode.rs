//! Composer Canonical Quadrant Modality and Draft Capsule (ADR-0051)
//!
//! Enforces the four mutually exclusive canonical interaction modes of the
//! Composer ([`ComposerMode`]: `Edit`, `Recall`, `Search`, `Complete`), isolates
//! in-place sequential history stepping from overlay search, encapsulates draft
//! preservation in [`DraftCapsule`], and provides unified mode dispatch.

use nuo_protocol::ImagePart;

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

/// Immutable draft snapshot captured atomically on entering any transient sub-mode ([INV-COMP-03]).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DraftCapsule {
    /// Input prompt text buffer.
    pub text: String,
    /// Cursor character/byte position.
    pub cursor_position: usize,
    /// Staged pending image attachments.
    pub pending_images: Vec<ImagePart>,
    /// Staged pending text paste blocks.
    pub pending_text_pastes: Vec<String>,
}

impl DraftCapsule {
    /// Capture a snapshot from current prompt state.
    pub fn capture(
        text: String,
        cursor_position: usize,
        pending_images: Vec<ImagePart>,
        pending_text_pastes: Vec<String>,
    ) -> Self {
        Self {
            text,
            cursor_position,
            pending_images,
            pending_text_pastes,
        }
    }

    /// Whether the capsule contains any non-empty draft data.
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
            && self.pending_images.is_empty()
            && self.pending_text_pastes.is_empty()
    }

    /// Restore the captured snapshot into the live prompt state and clear the capsule.
    pub fn restore_into(
        &mut self,
        input: &mut String,
        pending_images: &mut Vec<ImagePart>,
        pending_text_pastes: &mut Vec<String>,
        cursor_position: &mut usize,
    ) {
        *input = std::mem::take(&mut self.text);
        *pending_images = std::mem::take(&mut self.pending_images);
        *pending_text_pastes = std::mem::take(&mut self.pending_text_pastes);
        *cursor_position = input.chars().count();
        self.cursor_position = 0;
    }

    /// Clear all data in the capsule.
    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor_position = 0;
        self.pending_images.clear();
        self.pending_text_pastes.clear();
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

    #[test]
    fn test_draft_capsule_lifecycle() {
        let mut capsule =
            DraftCapsule::capture("my draft".into(), 8, vec![], vec!["paste block".into()]);
        assert!(!capsule.is_empty());
        assert_eq!(capsule.text, "my draft");
        assert_eq!(capsule.cursor_position, 8);
        assert_eq!(capsule.pending_text_pastes.len(), 1);

        capsule.clear();
        assert!(capsule.is_empty());
    }
}
