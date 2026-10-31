//! Pure disclosure policy: the default-expand resolution shared by the
//! transcript and step renderers, decoupled from the shell configuration type.
//!
//! Presentation code must not name the shell's `TuiConfig`; it depends on this
//! narrow, pure [`DisclosureSource`] port instead, and the shell implements it
//! for its configuration ([INV-ARCH-UI-ZERO-IO], ADR-0072). Keeping the
//! resolution here lets the transcript/step modules become pure projection.

/// The `[tui.default_expanded]` key controlling the reasoning-trace default.
pub const THINKING_KEY: &str = "thinking";

/// A read-only source of per-name disclosure defaults. The shell implements it
/// for its configuration so presentation code never depends on the shell type.
pub trait DisclosureSource {
    /// The configured default-expand state for `name`, if the user set one.
    fn disclosure_default(&self, name: &str) -> Option<bool>;
}

impl DisclosureSource for std::collections::HashMap<String, bool> {
    fn disclosure_default(&self, name: &str) -> Option<bool> {
        self.get(name).copied()
    }
}

impl<T: DisclosureSource + ?Sized> DisclosureSource for std::sync::Arc<T> {
    fn disclosure_default(&self, name: &str) -> Option<bool> {
        (**self).disclosure_default(name)
    }
}

/// Effective default-expand state for a tool step: an explicit setting wins;
/// otherwise the declared component default applies; a tool with no declared
/// component (MCP / unknown) stays collapsed.
pub fn tool_expanded(source: &impl DisclosureSource, name: &str) -> bool {
    source.disclosure_default(name).unwrap_or_else(|| {
        crate::tools::component_for(name)
            .map(|component| component.default_expanded())
            .unwrap_or(false)
    })
}

/// Effective default-expand state for a reasoning trace (collapsed by default).
pub fn reasoning_expanded(source: &impl DisclosureSource) -> bool {
    source.disclosure_default(THINKING_KEY).unwrap_or(false)
}

/// Effective default-expand state for a tool step at a given lifecycle.
///
/// A step's default disclosure is a pure function of `(status, tool name,
/// source)` — never set once at creation, because the relevant answer changes
/// as the step moves through its lifecycle. One resolver serves every
/// producer of a tool step: the live applier, the transcript restore pass, and
/// the subagent child-folding path. Those three used to be able to disagree,
/// and the folding path simply had no resolver at all — which is how a
/// subagent's restored children lost the `[tui.default_expanded]` policy its
/// live siblings honoured.
///
/// - **Ingesting / Executing** → the declared component default: there is no
///   result yet, so a step whose output *is* the point (a shell command) still
///   opens, while a step whose output is incidental stays folded.
/// - **Failed / Denied** → expanded: the error or denial message is the whole
///   point and must be visible without an extra click.
/// - **Cancelled / Interrupted** → collapsed: an aborted call, or a subagent
///   the user stopped deliberately, reads as inert. The summary line and the
///   drill-in view still surface the recovered work.
/// - **Ok** → the declared component default.
///
/// Callers apply this through the *system* setter (`set_*_expanded`), which
/// no-ops once the user has pinned the step, so a lifecycle change never
/// overrides an explicit choice.
pub fn tool_step_expanded(
    source: &impl DisclosureSource,
    status: crate::model::document::ToolStepStatus,
    name: &str,
) -> bool {
    use crate::model::document::ToolStepStatus;
    match status {
        ToolStepStatus::Ingesting | ToolStepStatus::Executing | ToolStepStatus::Ok => {
            tool_expanded(source, name)
        }
        ToolStepStatus::Failed | ToolStepStatus::Denied => true,
        ToolStepStatus::Cancelled | ToolStepStatus::Interrupted => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::document::ToolStepStatus;

    fn source(defaults: &[(&str, bool)]) -> std::collections::HashMap<String, bool> {
        defaults
            .iter()
            .map(|(k, v)| ((*k).to_string(), *v))
            .collect()
    }

    /// The lifecycle resolver is the single origin for every producer of a tool
    /// step. A running step follows its declared component default; a failure
    /// or denial always opens; an aborted or interrupted step stays folded.
    #[test]
    fn tool_step_expanded_is_lifecycle_aware() {
        let cfg = source(&[]);
        // `edit_text` declares expanded-by-default; `execute_command` and `read_text` declare collapsed.
        assert!(!tool_step_expanded(
            &cfg,
            ToolStepStatus::Executing,
            "execute_command"
        ));
        assert!(tool_step_expanded(
            &cfg,
            ToolStepStatus::Executing,
            "edit_text"
        ));
        assert!(!tool_step_expanded(
            &cfg,
            ToolStepStatus::Executing,
            "read_text"
        ));
        for (status, expected) in [
            (ToolStepStatus::Failed, true),
            (ToolStepStatus::Denied, true),
            (ToolStepStatus::Ingesting, false),
        ] {
            assert_eq!(
                tool_step_expanded(&cfg, status, "search_text"),
                expected,
                "{status:?} must follow the lifecycle rule"
            );
        }
        assert!(tool_step_expanded(
            &cfg,
            ToolStepStatus::Failed,
            "search_text"
        ));
        assert!(tool_step_expanded(
            &cfg,
            ToolStepStatus::Denied,
            "execute_command"
        ));
        assert!(!tool_step_expanded(
            &cfg,
            ToolStepStatus::Cancelled,
            "execute_command"
        ));
        assert!(!tool_step_expanded(
            &cfg,
            ToolStepStatus::Interrupted,
            "spawn_agent"
        ));
    }

    /// An explicit `[tui.default_expanded]` entry beats the declared component
    /// default for every non-terminal lifecycle.
    #[test]
    fn explicit_entry_overrides_declared_default() {
        let cfg = source(&[("read_text", true), ("execute_command", true)]);
        assert!(tool_step_expanded(&cfg, ToolStepStatus::Ok, "read_text"));
        assert!(tool_step_expanded(
            &cfg,
            ToolStepStatus::Executing,
            "execute_command"
        ));
        // Terminal failure still force-expands, override or not.
        assert!(tool_step_expanded(
            &cfg,
            ToolStepStatus::Failed,
            "execute_command"
        ));
    }

    /// A tool with no declared component (MCP / unknown) stays collapsed while
    /// healthy, and the link to the reasoning key stays exact.
    #[test]
    fn undeclared_components_and_reasoning_key() {
        let cfg = source(&[]);
        assert!(!tool_step_expanded(
            &cfg,
            ToolStepStatus::Ok,
            "mcp__unknown__tool"
        ));
        assert!(!reasoning_expanded(&cfg));
        assert!(reasoning_expanded(&source(&[(THINKING_KEY, true)])));
    }
}
