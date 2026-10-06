//! Step interaction decisions: classifying pointer hits on step summaries.
//!
//! The transcript layout marks step summaries with sentinel `block_idx` values
//! (`TOOL_STEP_BLOCK_IDX` / `REASONING_BLOCK_IDX`) so the click/hover
//! machinery can tell them apart from prose, code, and table cells. This
//! module owns those sentinels and the "what kind of step is under the
//! pointer" classification, so the app's event loop (`lib.rs`) speaks in terms
//! of [`StepKind`] instead of raw layout sentinels scattered across match
//! arms.
//!
//! It depends only on the layout layer — no render or app-state dependency —
//! keeping the interaction vocabulary free of layering cycles and unit-testable
//! in isolation.

use crate::config::{TuiConfig, tool_default_expanded};
use crate::model::document::ToolStepStatus;
use crate::model::layout::{
    COMMAND_RESULT_BLOCK_IDX, COMPACTED_CARD_BLOCK_IDX, InteractiveTarget, NOTICE_BLOCK_IDX,
    REASONING_BLOCK_IDX, SemanticCursor, TOOL_STEP_BLOCK_IDX,
};
/// Which kind of step a pointer hit resolved to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StepKind {
    /// A tool step or subagent task summary.
    ToolStep,
    /// A reasoning trace summary.
    Reasoning,
    /// An expandable notice / live provider-retry entry.
    Notice,
    /// A command invocation with its result body.
    CommandResult,
    /// An expandable compaction checkpoint card.
    CompactedCard,
}

impl StepKind {
    /// The keyboard-focus target this summary kind maps to.
    pub fn focus_target(self, mi: usize) -> InteractiveTarget {
        match self {
            StepKind::ToolStep => InteractiveTarget::tool_step(mi),
            StepKind::Reasoning => InteractiveTarget::reasoning(mi),
            StepKind::Notice => InteractiveTarget::notice(mi),
            StepKind::CommandResult => InteractiveTarget::command_result(mi),
            StepKind::CompactedCard => InteractiveTarget::compacted_card(mi),
        }
    }
}

/// Classify a resolved cursor as a hit on a step summary, returning its
/// message index and kind. Returns `None` for non-summary regions (prose,
/// code blocks, table cells, the input box, …).
///
/// Drives click routing — toggle / subagent navigation / the detail overlay —
/// and the hover affordance, so every call site shares one notion of "what
/// counts as a step summary".
///
/// Every sentinel a renderer records for an interactive entry must be listed
/// here: a missing arm silently downgrades the entry's click to a plain text
/// selection while it still renders a disclosure marker and an activation hint.
pub fn summary_at(cursor: &SemanticCursor) -> Option<(usize, StepKind)> {
    let kind = match cursor.block_idx {
        TOOL_STEP_BLOCK_IDX => StepKind::ToolStep,
        REASONING_BLOCK_IDX => StepKind::Reasoning,
        NOTICE_BLOCK_IDX => StepKind::Notice,
        COMMAND_RESULT_BLOCK_IDX => StepKind::CommandResult,
        COMPACTED_CARD_BLOCK_IDX => StepKind::CompactedCard,
        _ => return None,
    };
    Some((cursor.message_idx, kind))
}

// Lifecycle-aware default disclosure
//
// A step's default disclosure is a pure function of (kind, lifecycle) — NOT
// set once at creation. Tool steps stay collapsed while running (no result
// yet) and expand on completion; failures force-expand so the error is
// visible. Reasoning traces do not auto-expand (their default disclosure is
// driven by `[tui.default_expanded] thinking`, collapsed by default); a manual
// user toggle pins the step (see `document::TranscriptMessage::pin_*`) and
// opts out of further automatic changes.

/// Default disclosure for a tool step at its current lifecycle. The caller
/// applies this through the system setter, which no-ops once the user has
/// pinned the step.
///
/// - **Running** → collapsed: there's no result yet, so an open body would
///   just be noise. (Live-streaming tools like `bash` still accumulate output
///   via `push_tool_stream`; the user can expand manually to watch it.)
/// - **Failed / Denied** → expanded: the error/denial message is the whole
///   point and must be visible without an extra click.
/// - **Cancelled** → collapsed: an aborted call reads as inert.
/// - **Ok** → the per-tool default (from the tool's `[tui.default_expanded]` entry
///   or built-in component default): `edit_text`, `write_file`, and `execute_command`
///   show their bodies; `read_text` and the rest stay collapsed.
pub fn default_tool_expanded(
    status: ToolStepStatus,
    name: &str,
    config: &TuiConfig,
) -> bool {
    match status {
        ToolStepStatus::Running => tool_default_expanded(config, name),
        ToolStepStatus::Failed | ToolStepStatus::Denied => true,
        ToolStepStatus::Cancelled => false,
        // An interrupted subagent preserved partial work, but the user stopped
        // it deliberately — leave it collapsed (like Cancelled); the summary
        // line and the drill-in view surface the recovered findings.
        ToolStepStatus::Interrupted => false,
        ToolStepStatus::Ok => tool_default_expanded(config, name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cursor(block_idx: usize, mi: usize) -> SemanticCursor {
        SemanticCursor::new(mi, block_idx, 0)
    }

    #[test]
    fn tool_step_summary_classifies() {
        let (mi, kind) = summary_at(&cursor(TOOL_STEP_BLOCK_IDX, 7)).unwrap();
        assert_eq!(mi, 7);
        assert_eq!(kind, StepKind::ToolStep);
    }

    #[test]
    fn thinking_summary_classifies() {
        let (mi, kind) = summary_at(&cursor(REASONING_BLOCK_IDX, 3)).unwrap();
        assert_eq!(mi, 3);
        assert_eq!(kind, StepKind::Reasoning);
    }

    #[test]
    fn non_summary_is_none() {
        assert!(summary_at(&cursor(5, 0)).is_none());
        assert!(summary_at(&cursor(0, 0)).is_none());
    }

    fn config(defaults: &[(&str, bool)]) -> TuiConfig {
        let mut map = std::collections::HashMap::new();
        for (k, v) in defaults {
            map.insert((*k).to_string(), *v);
        }
        TuiConfig {
            default_expanded: map,
            ..TuiConfig::default()
        }
    }

    /// A click on a marked entry must classify as that entry, and the focus
    /// target a click produces must match the one the same entry declares when
    /// focus walks the transcript — the pairing that makes "clicked it" and
    /// "focused it" the same component (ADR-0020 §6).
    #[test]
    fn every_marked_entry_classifies_and_focuses_identically() {
        use crate::model::layout::InteractiveTarget;
        let cases = [
            (TOOL_STEP_BLOCK_IDX, StepKind::ToolStep),
            (REASONING_BLOCK_IDX, StepKind::Reasoning),
            (NOTICE_BLOCK_IDX, StepKind::Notice),
            (COMMAND_RESULT_BLOCK_IDX, StepKind::CommandResult),
            (COMPACTED_CARD_BLOCK_IDX, StepKind::CompactedCard),
        ];
        for (block_idx, expected_kind) in cases {
            let (mi, kind) = summary_at(&cursor(block_idx, 9))
                .unwrap_or_else(|| panic!("block sentinel {block_idx} must classify"));
            assert_eq!(mi, 9);
            assert_eq!(kind, expected_kind);
            assert_eq!(
                kind.focus_target(mi),
                InteractiveTarget::for_block(block_idx, mi)
                    .expect("classified entry must have a focus target"),
                "click and focus must agree for {expected_kind:?}"
            );
        }
    }

    /// A marked entry must never classify when the pointer is on prose — the
    /// inverse guard, so the classifier cannot claim ordinary text.
    #[test]
    fn prose_regions_never_classify_as_entries() {
        assert!(summary_at(&cursor(0, 4)).is_none());
        assert!(summary_at(&cursor(12, 4)).is_none());
    }

    #[test]
    fn tool_running_follows_declared_default_failures_expand() {
        let cfg = config(&[]);
        // Running steps follow the declared component default: a shell command
        // opens (its output is the point), a file read with no result yet stays
        // collapsed; failures always force-expand.
        assert!(default_tool_expanded(
            ToolStepStatus::Running,
            "execute_command",
            &cfg
        ));
        assert!(!default_tool_expanded(
            ToolStepStatus::Running,
            "read_text",
            &cfg
        ));
        assert!(default_tool_expanded(
            ToolStepStatus::Failed,
            "search_text",
            &cfg
        ));
        assert!(default_tool_expanded(
            ToolStepStatus::Denied,
            "execute_command",
            &cfg
        ));
        assert!(!default_tool_expanded(
            ToolStepStatus::Cancelled,
            "execute_command",
            &cfg
        ));
    }

    #[test]
    fn tool_ok_follows_declared_default() {
        let cfg = config(&[("read_text", true)]);
        assert!(default_tool_expanded(
            ToolStepStatus::Ok,
            "read_text",
            &cfg
        ));
        // An explicit config entry overrides the declared default, and the
        // alias family follows the canonical name's choice.
        assert!(default_tool_expanded(
            ToolStepStatus::Ok,
            "edit_text",
            &cfg
        ));
        assert!(default_tool_expanded(
            ToolStepStatus::Ok,
            "write_file",
            &cfg
        ));
        assert!(default_tool_expanded(
            ToolStepStatus::Ok,
            "execute_command",
            &cfg
        ));
        assert!(!default_tool_expanded(
            ToolStepStatus::Ok,
            "search_text",
            &cfg
        ));
    }
}
