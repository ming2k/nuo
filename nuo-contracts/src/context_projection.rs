//! Context-projection vocabulary: what a prune or a compaction reports.
//!
//! Shared vocabulary rather than store state (ADR-0300 §1). The *kernel*
//! produces a projection — it decides what to prune, what to compact, and what
//! the checkpoint says about it — and the *host* persists the checkpoint beside
//! the conversation. Both sides therefore name these types, so they live in the
//! contract layer and neither crate owns the other's.
//!
//! The numbers here are measurements taken at one instant. They are recorded as
//! observations, never recomputed: a checkpoint states what the window measured
//! *before* and *after* the projection, and the difference is what the
//! projection reclaimed.

use serde::{Deserialize, Serialize};

/// Which projection ran.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContextProjectionKind {
    /// Cheap tool-result pruning: drop reclaimable payloads, keep the shape.
    Prune,
    /// Summarizing compaction: replace a prefix of the window with a generative
    /// summary (ADR-0296).
    Compact,
}

/// The record of one projection, as the kernel measured it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContextProjectionCheckpoint {
    pub operation: ContextProjectionKind,
    pub archived_messages: usize,
    pub active_messages: usize,
    /// Token size of the active model window **sampled immediately before
    /// the projection was applied**. A point-in-time sample, not a live
    /// value: the window keeps growing after this checkpoint.
    pub window_tokens_before: usize,
    /// Token size of the active model window immediately **after** the
    /// projection. Same point-in-time caveat; the difference to
    /// [`Self::window_tokens_before`] is what the projection reclaimed.
    pub window_tokens_after: usize,
    /// Generative summary produced during compaction (ADR-0296).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Artifact file paths touched across the compacted lineage (ADR-0296).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tracked_files: Vec<String>,
}

/// What one projection produced: the new window, the originals it displaced,
/// and the checkpoint describing the change.
///
/// `archived_originals` are handed back rather than dropped so the caller can
/// persist them: a projection that loses its own inputs cannot be audited, and
/// the kernel does not own storage (ADR-0303 §1).
#[derive(Debug, Clone)]
pub struct ContextProjectionResult {
    pub model_window: Vec<crate::Message>,
    pub archived_originals: Vec<crate::Message>,
    pub checkpoint: ContextProjectionCheckpoint,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_checkpoint_round_trips_without_recomputing_its_measurements() {
        let checkpoint = ContextProjectionCheckpoint {
            operation: ContextProjectionKind::Compact,
            archived_messages: 40,
            active_messages: 12,
            window_tokens_before: 180_000,
            window_tokens_after: 24_000,
            summary: Some("earlier rounds compacted".into()),
            tracked_files: vec!["src/main.rs".into()],
        };
        let json = serde_json::to_string(&checkpoint).unwrap();
        let parsed: ContextProjectionCheckpoint = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, checkpoint);
        assert_eq!(
            parsed.window_tokens_before - parsed.window_tokens_after,
            156_000,
            "the reclaimed figure is a difference of recorded samples, not a new estimate"
        );
    }

    #[test]
    fn the_operation_serializes_as_a_stable_name() {
        assert_eq!(
            serde_json::to_string(&ContextProjectionKind::Prune).unwrap(),
            "\"prune\""
        );
        assert_eq!(
            serde_json::to_string(&ContextProjectionKind::Compact).unwrap(),
            "\"compact\""
        );
    }

    #[test]
    fn optional_fields_are_omitted_rather_than_nulled() {
        let checkpoint = ContextProjectionCheckpoint {
            operation: ContextProjectionKind::Prune,
            archived_messages: 3,
            active_messages: 9,
            window_tokens_before: 1_000,
            window_tokens_after: 800,
            summary: None,
            tracked_files: Vec::new(),
        };
        let json = serde_json::to_string(&checkpoint).unwrap();
        assert!(!json.contains("summary"), "{json}");
        assert!(!json.contains("tracked_files"), "{json}");
    }
}
