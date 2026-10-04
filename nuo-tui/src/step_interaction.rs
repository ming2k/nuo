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
    COMMAND_RESULT_BLOCK_IDX, InteractiveTarget, NOTICE_BLOCK_IDX, PROVIDER_RETRY_BLOCK_IDX,
    REASONING_BLOCK_IDX, SemanticCursor, TOOL_STEP_BLOCK_IDX,
};

/// Which kind of step a pointer hit resolved to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StepKind {
    /// A tool step or subagent task summary.
    ToolStep,
    /// A reasoning trace summary.
    Reasoning,
    /// The live provider-retry summary.
    ProviderRetry,
    /// A command invocation with expandable result.
    CommandResult,
    /// An expandable notice (e.g. error with JSON).
    Notice,
}

impl StepKind {
    /// The keyboard-focus target this summary kind maps to.
    pub fn focus_target(self, mi: usize) -> InteractiveTarget {
        match self {
            StepKind::ToolStep => InteractiveTarget::tool_step(mi),
            StepKind::Reasoning => InteractiveTarget::reasoning(mi),
            StepKind::ProviderRetry => InteractiveTarget::provider_retry(mi),
            StepKind::CommandResult => InteractiveTarget::command_result(mi),
            StepKind::Notice => InteractiveTarget::notice(mi),
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
pub fn summary_at(cursor: &SemanticCursor) -> Option<(usize, StepKind)> {
    let kind = match cursor.block_idx {
        TOOL_STEP_BLOCK_IDX => StepKind::ToolStep,
        REASONING_BLOCK_IDX => StepKind::Reasoning,
        PROVIDER_RETRY_BLOCK_IDX => StepKind::ProviderRetry,
        COMMAND_RESULT_BLOCK_IDX => StepKind::CommandResult,
        NOTICE_BLOCK_IDX => StepKind::Notice,
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
/// - **Ok** → the per-tool default (`density` Comfortable mode, else the
///   tool's `[tui.default_expanded]` entry): `edit_text` and `write_file` show their diff;
///   `bash`/`read_text` and the rest stay collapsed.
pub fn default_tool_expanded(
    status: ToolStepStatus,
    name: &str,
    config: &TuiConfig,
    density: bool,
) -> bool {
    match status {
        ToolStepStatus::Running => density || tool_default_expanded(config, name),
        ToolStepStatus::Failed | ToolStepStatus::Denied => true,
        ToolStepStatus::Cancelled => false,
        // An interrupted subagent preserved partial work, but the user stopped
        // it deliberately — leave it collapsed (like Cancelled); the summary
        // line and the drill-in view surface the recovered findings.
        ToolStepStatus::Interrupted => false,
        ToolStepStatus::Ok => density || tool_default_expanded(config, name),
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
            transcript_layout: String::new(),
            ..TuiConfig::default()
        }
    }

    #[test]
    fn tool_running_follows_per_tool_default_failures_expand() {
        let cfg = config(&[]);
        // execute_command collapses even while running (live output would
        // otherwise dominate the transcript); failures still force-expand.
        assert!(!default_tool_expanded(
            ToolStepStatus::Running,
            "execute_command",
            &cfg,
            false
        ));
        assert!(!default_tool_expanded(
            ToolStepStatus::Running,
            "read_text",
            &cfg,
            false
        ));
        assert!(default_tool_expanded(
            ToolStepStatus::Failed,
            "search_text",
            &cfg,
            false
        ));
        assert!(default_tool_expanded(
            ToolStepStatus::Denied,
            "execute_command",
            &cfg,
            false
        ));
        assert!(!default_tool_expanded(
            ToolStepStatus::Cancelled,
            "execute_command",
            &cfg,
            false
        ));
    }

    #[test]
    fn tool_ok_follows_per_tool_default_then_density() {
        let cfg = config(&[("edit_text", true)]);
        assert!(default_tool_expanded(
            ToolStepStatus::Ok,
            "edit_text",
            &cfg,
            false
        ));
        assert!(default_tool_expanded(
            ToolStepStatus::Ok,
            "write_file",
            &cfg,
            false
        ));
        // execute_command collapses on success too; only failures expand it.
        assert!(!default_tool_expanded(
            ToolStepStatus::Ok,
            "execute_command",
            &cfg,
            false
        ));
        assert!(!default_tool_expanded(
            ToolStepStatus::Ok,
            "read_text",
            &cfg,
            false
        ));
        // Comfortable density expands every Ok step regardless of per-tool default.
        assert!(default_tool_expanded(
            ToolStepStatus::Ok,
            "read_text",
            &cfg,
            true
        ));
    }
}
