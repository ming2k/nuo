//! The composer draft: the one value type for unsent input.
//!
//! A draft is *one kind of thing* — text, a caret, staged attachments, and the
//! composer's own view offset — and it is captured and installed by exactly two
//! owners:
//!
//! - the **transient stash** ([ADR-0051](../../../docs/adr/0051-composer-canonical-quadrant-modality-atomic-draft-capsule-and-in-place-recall-topology.md)),
//!   taken while a sub-mode (history recall, history search, an AI sheet)
//!   borrows the composer line and given back when it ends;
//! - the **parked draft registry**
//!   ([ADR-0247](../../../docs/adr/0247-a-draft-belongs-to-a-thread-and-outlives-every-view-transition.md)),
//!   which holds one draft per thread across view switches, re-attaches, and
//!   process restarts.
//!
//! Two lifetimes, one type. The previous shape had a partial copy per tab
//! (`TabContextState::input` / `cursor_position` / `input_scroll`) that silently
//! dropped the staged attachments and re-derived the caret, so the same concept
//! had three encodings and two of them were lossy.
//!
//! # Units
//!
//! `cursor_position` is a **byte** offset into `text`, matching
//! `ComposerState::cursor_position` — the unit every editor motion, the composer
//! grid, and the `@`-mention scanner already use. Installs clamp it to a
//! `char` boundary so a stale offset can never split a scalar.

use nuo_tool::ImagePart;
use serde::{Deserialize, Serialize};

/// Unsent composer input, captured atomically.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ComposerDraft {
    /// The prompt text, including any `[Image #N]` / `[Pasted text #N]` chips
    /// that pair 1-to-1 with the staged vectors below.
    pub text: String,
    /// Caret position as a **byte** offset into `text`.
    pub cursor_position: usize,
    /// Staged image attachments, in chip order.
    pub pending_images: Vec<ImagePart>,
    /// Staged large-paste payloads, in chip order.
    pub pending_text_pastes: Vec<String>,
    /// The composer viewport's vertical scroll offset in rows, so a restored
    /// multi-line draft lands where the user left it instead of snapping to the
    /// first row.
    pub input_scroll: usize,
}

impl ComposerDraft {
    /// Capture a draft from live composer buffers.
    pub fn capture(
        text: String,
        cursor_position: usize,
        pending_images: Vec<ImagePart>,
        pending_text_pastes: Vec<String>,
        input_scroll: usize,
    ) -> Self {
        Self {
            text,
            cursor_position,
            pending_images,
            pending_text_pastes,
            input_scroll,
        }
    }

    /// Whether this draft carries nothing the user would miss.
    ///
    /// The caret and the scroll offset are *positions*, not content: an empty
    /// text with no staged payloads is an empty draft no matter where the caret
    /// happened to rest, which is what lets a parked draft be deleted outright
    /// rather than kept as a tombstone.
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
            && self.pending_images.is_empty()
            && self.pending_text_pastes.is_empty()
    }

    /// Drop every field back to its empty state.
    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor_position = 0;
        self.pending_images.clear();
        self.pending_text_pastes.clear();
        self.input_scroll = 0;
    }

    /// The stored caret, clamped to `text`'s length and floored to a `char`
    /// boundary.
    ///
    /// A draft can outlive the shape of its own text: it is serialized to disk,
    /// folded into a JSON file by a different process, and restored after an
    /// upgrade. A raw offset would then be able to split a scalar and panic the
    /// editor, so every install goes through this.
    pub fn clamped_cursor(&self) -> usize {
        floor_char_boundary(&self.text, self.cursor_position.min(self.text.len()))
    }

    /// The number of bytes this draft occupies, for the store's size bound.
    ///
    /// Attachment payloads dominate (base64), so they are counted at full
    /// weight; the accounting only has to be *monotone and conservative*, not
    /// exact, because it decides whether a draft is admitted at all.
    pub fn size_bytes(&self) -> usize {
        self.text.len()
            + self
                .pending_images
                .iter()
                .map(|image| image.mime.len() + image.data.len())
                .sum::<usize>()
            + self
                .pending_text_pastes
                .iter()
                .map(String::len)
                .sum::<usize>()
    }
}

/// Largest `char` boundary at or below `index`, in bytes.
fn floor_char_boundary(text: &str, index: usize) -> usize {
    if index >= text.len() {
        return text.len();
    }
    let mut byte = index;
    while byte > 0 && !text.is_char_boundary(byte) {
        byte -= 1;
    }
    byte
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image() -> ImagePart {
        ImagePart {
            mime: "image/png".to_string(),
            data: "AAAA".to_string(),
        }
    }

    #[test]
    fn capture_round_trips_every_field() {
        let draft = ComposerDraft::capture(
            "ship it".to_string(),
            4,
            vec![image()],
            vec!["paste".to_string()],
            2,
        );
        assert_eq!(draft.text, "ship it");
        assert_eq!(draft.cursor_position, 4);
        assert_eq!(draft.pending_images.len(), 1);
        assert_eq!(draft.pending_text_pastes, vec!["paste".to_string()]);
        assert_eq!(draft.input_scroll, 2);
        assert!(!draft.is_empty());
    }

    #[test]
    fn positions_alone_do_not_make_a_draft_non_empty() {
        // A caret and a scroll offset are where the user was, not what they
        // wrote: neither may keep a parked draft alive.
        let draft = ComposerDraft {
            cursor_position: 12,
            input_scroll: 4,
            ..Default::default()
        };
        assert!(draft.is_empty());
    }

    #[test]
    fn staged_payloads_alone_keep_a_draft_alive() {
        // An attachment with no text is content — the "paste an image first"
        // flow must not be discarded as empty.
        let draft = ComposerDraft {
            pending_images: vec![image()],
            ..Default::default()
        };
        assert!(!draft.is_empty());
        assert!(draft.size_bytes() > 0);
    }

    #[test]
    fn clamped_cursor_floors_to_a_char_boundary() {
        // "é" is two bytes: offset 1 splits it, offset 2 is the boundary.
        let draft = ComposerDraft::capture("é".to_string(), 1, vec![], vec![], 0);
        assert_eq!(draft.clamped_cursor(), 0);
        let draft = ComposerDraft::capture("é".to_string(), 2, vec![], vec![], 0);
        assert_eq!(draft.clamped_cursor(), 2);
    }

    #[test]
    fn clamped_cursor_cannot_exceed_the_text() {
        let draft = ComposerDraft::capture("short".to_string(), 999, vec![], vec![], 0);
        assert_eq!(draft.clamped_cursor(), 5);
        let empty = ComposerDraft::capture(String::new(), 7, vec![], vec![], 0);
        assert_eq!(empty.clamped_cursor(), 0);
    }

    #[test]
    fn clear_returns_every_field_to_empty() {
        let mut draft =
            ComposerDraft::capture("x".to_string(), 1, vec![image()], vec!["y".to_string()], 3);
        draft.clear();
        assert_eq!(draft, ComposerDraft::default());
        assert!(draft.is_empty());
    }

    #[test]
    fn serialization_round_trips_and_defaults_missing_fields() {
        let draft = ComposerDraft::capture(
            "persist me".to_string(),
            3,
            vec![image()],
            vec!["block".to_string()],
            1,
        );
        let encoded = serde_json::to_string(&draft).expect("encode");
        let decoded: ComposerDraft = serde_json::from_str(&encoded).expect("decode");
        assert_eq!(decoded, draft);

        // A file written before `input_scroll` existed must still load.
        let legacy: ComposerDraft =
            serde_json::from_str(r#"{"text":"old","cursor_position":2}"#).expect("legacy decode");
        assert_eq!(legacy.text, "old");
        assert_eq!(legacy.input_scroll, 0);
        assert!(legacy.pending_images.is_empty());
    }
}
