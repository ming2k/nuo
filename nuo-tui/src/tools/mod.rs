//! Per-tool presentation registry.
//!
//! Each tool maps to a `ToolPresenter` (defined below) that owns how that tool
//! looks in the transcript: the one-line collapsed summary and the declarative
//! classifications that drive its expanded body. This collapses the per-tool
//! `match name { … }` branches that were previously scattered across
//! `document.rs` (`argument_summary`) and `step/renderers.rs` (result
//! rendering) into one place — adding a tool means adding a file and one
//! registry arm.
//!
//! Each presenter owns a collapsed `summary` and declarative `result_kind` /
//! `arg_layout` classifications that drive the expanded body
//! (`step/renderers.rs` owns the drawing primitives; this module owns the
//! per-tool decisions). `document.rs` and `step/renderers.rs` call the
//! `*_for` entry points below instead of matching on tool names.
//!
//! [`TOOL_COMPONENTS`] is also the single source of truth for the Settings →
//! Components pane (ADR-0020).

mod ask_user;
mod diff;
mod edit_text;
mod execute_command;
mod fallback;
mod mcp;
mod meta;
mod read_image;
mod read_text;
mod search;
mod web;

pub(crate) use diff::DiffCache;
pub use diff::{DiffFrag, DiffHunk, DiffOp};

use nuotc::Color;
use serde_json::Value;

use super::Theme;
use crate::model::document::ToolStepStatus;

/// Resolved run state of a tool step. The model-side source of truth is
/// [`ToolStepStatus`]; this is its presentation classification. Kept separate
/// so the model does not depend on the render layer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ToolStatus {
    /// No output yet — the call is still in flight.
    Running,
    /// Output present and not an error.
    Ok,
    /// Output present and the call failed. Failure is determined by the
    /// structured [`ToolStepStatus`] (set from `ToolOutput::is_error()` in
    /// `document.rs`), not by string-sniffing the output text — subagent
    /// failures carry an explicit `failed` flag and tool errors use
    /// `ToolOutput::Error`.
    Failed,
    /// The user explicitly denied permission for this call.
    Denied,
    /// The call was aborted before producing a result (e.g. user interrupt).
    Cancelled,
    /// The user interrupted the turn while the call was in flight, but the
    /// call drained and its partial result was preserved (an interrupted
    /// subagent). More alive than `Cancelled`: there is recovered work to
    /// inspect and possibly resume.
    Interrupted,
}

impl ToolStatus {
    /// Classify a tool step from its stored lifecycle. This is the primary
    /// constructor now that the model carries an explicit status.
    pub fn from_status(status: ToolStepStatus) -> Self {
        match status {
            ToolStepStatus::Running => ToolStatus::Running,
            ToolStepStatus::Ok => ToolStatus::Ok,
            ToolStepStatus::Failed => ToolStatus::Failed,
            ToolStepStatus::Denied => ToolStatus::Denied,
            ToolStepStatus::Cancelled => ToolStatus::Cancelled,
            ToolStepStatus::Interrupted => ToolStatus::Interrupted,
        }
    }

    /// Theme color used for the status rail / step accent. Centralizes the
    /// status→color mapping that step headers, sticky pins, and subagent steps
    /// previously each duplicated.
    pub fn color(self, theme: &Theme) -> Color {
        match self {
            // Running reads as a neutral, in-flight gray — not a hue. A
            // pending call carries no success/failure semantics yet, so it
            // borrows the muted text tone rather than the blue `info` accent,
            // keeping the accent palette reserved for resolved outcomes.
            ToolStatus::Running => theme.muted(),
            ToolStatus::Ok => theme.ok(),
            ToolStatus::Failed => theme.err(),
            // Warn color distinguishes a user denial from a runtime failure.
            ToolStatus::Denied => theme.warn(),
            // Cancelled steps one rung dimmer than Running: the call was
            // aborted, so it reads as fully inert rather than merely idle.
            ToolStatus::Cancelled => theme.dim(),
            // Interrupted carries the same user-intervention tone as Denied,
            // but on a brighter accent: unlike a dropped call, an interrupted
            // subagent preserved partial work worth noticing.
            ToolStatus::Interrupted => theme.warn(),
        }
    }
}

/// How a tool's result output is rendered in the expanded step body. The
/// drawing primitives live in `step/renderers.rs`; presenters only declare
/// which one applies, so the per-tool dispatch lives in one place (the registry).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ResultKind {
    /// Line-numbered code block (default / unknown tools, `read_text`).
    Code,
    /// Directory or file-search listing.
    Listing,
    /// `path:line:match` search-result rendering.
    Matches,
    /// Shell output with `$ command` framing and exit/section markers.
    Command,
    /// A red/green line diff derived from a structured patch result. Legacy
    /// restored sessions may fall back to the original tool arguments.
    Diff,
    /// An interactive checklist (todo / task list) with [✓], [•], [☐], [✕] status glyphs.
    Checklist,
    /// Interactive search result cards with domain pills and clickable URLs.
    WebSearch,
    /// Article reader view for web pages, rendering clean markdown prose without code gutters.
    WebArticle,
    /// An `ask_user` clarifying-question request resolved to a question→answer
    /// list. Unlike the code-style default, the body is driven by the call's
    /// `arguments` (the question headers, texts, option counts, and
    /// multi-select flags) paired with the recorded selection from the result
    /// text — so the questions stay traceable after the answer lands instead of
    /// collapsing into a bare JSON array.
    Questions,
}

/// How a tool's arguments are rendered in the expanded step body.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ArgLayout {
    /// No arguments section — the header summary already captures the inputs
    /// (the default for tools whose summary names their key argument, e.g.
    /// `Read path`, `Search "query" in path`). Edit/write also use this: the path
    /// is in the header and the content is in the diff.
    None,
    /// A single wrapped command string, shown under an `Arguments`
    /// label without the `key:` prefix.
    Command,
    /// Flat `key: value` lines. Used for unknown / MCP tools whose generic
    /// header doesn't spell out the arguments.
    KeyValue,
}

use crate::components::inline_layout::SemanticLine;

/// A read-only view of a tool step, handed to a [`ToolPresenter`]. Arguments
/// are pre-parsed into a JSON object by the registry entry points so each
/// presenter can pull typed fields without re-parsing.
pub struct ToolView<'a> {
    pub name: &'a str,
    pub args: &'a serde_json::Map<String, Value>,
    /// The subagent profile name (`explore` / `plan` / …) when this step is a
    /// subagent run that has announced its role; `None` otherwise. Lets the
    /// `SubagentPresenter` label the step by role instead of "Subagent".
    pub profile: Option<&'a str>,
    /// The active session workspace root directory, if known. Used by
    /// path-aware presenters to resolve absolute paths relative to the project.
    pub workspace_root: Option<&'a std::path::Path>,
}

impl ToolView<'_> {
    /// Fetch a string-valued argument, or `None` when absent / non-string.
    pub fn str(&self, key: &str) -> Option<&str> {
        self.args.get(key).and_then(Value::as_str)
    }

    /// Fetch a non-negative integer argument, or `None` when absent /
    /// non-numeric. Used by presenters that surface numeric params such as
    /// `read_text`'s `offset` / `limit` in their collapsed header.
    pub fn u64(&self, key: &str) -> Option<u64> {
        self.args.get(key).and_then(Value::as_u64)
    }
}

/// How a single tool renders in the transcript. Stateless: implementors are
/// zero-sized unit structs resolved via [`presenter_for`] and referenced from
/// the [`TOOL_COMPONENTS`] table, hence the `Sync` bound.
pub trait ToolPresenter: Sync {
    /// One-line, human-readable summary for the collapsed header. The registry
    /// truncates the result to the header budget, so implementors only need to
    /// truncate individual interpolated fields where it improves readability.
    fn summary(&self, view: &ToolView) -> String;

    /// Structured, layout-aware semantic summary line evaluated at render time against
    /// the physical terminal viewport width (ADR-0206).
    fn render_summary<'a>(&self, view: &'a ToolView) -> SemanticLine<'a> {
        SemanticLine::plain(self.summary(view))
    }

    /// Which result renderer the expanded body uses for this tool's output.
    fn result_kind(&self) -> ResultKind {
        ResultKind::Code
    }

    /// How the expanded body renders this tool's arguments.
    fn arg_layout(&self) -> ArgLayout {
        ArgLayout::None
    }

    /// Whether a freshly created (or restored) step of this tool spawns
    /// expanded. The global Ctrl+T density still overrides this when the user
    /// has toggled it; this is only the per-tool default for compact mode.
    fn default_expanded(&self) -> bool {
        false
    }
}

/// A declared, tool-backed **interactive component**: the settings-facing
/// grouping of one disclosure policy over every tool name that renders
/// through it.
///
/// This table is the single source of truth for three things that previously
/// each kept their own copy of the tool-name vocabulary:
///
/// 1. name → presenter resolution ([`presenter_for`]);
/// 2. the Settings → Components panel rows (one row per entry, in table
///    order) and their default disclosure state;
/// 3. the config keys a panel toggle writes — the `[tui.default_expanded]`
///    fan-out covers every alias by construction, so a step recorded under a
///    legacy spelling (`bash`, `write_todos`) honours the user's choice too.
///
/// Adding a tool therefore means adding a presenter file and one entry here;
/// the panel row appears automatically (ADR-0020).
///
/// A component may span **several presenters** (Search groups the text, glob,
/// and directory presenters; Diffs groups edit and write) because the row's
/// meaning is the user-facing disclosure policy, not the drawing routine. Its
/// [`Self::default_expanded`] is therefore the declared default all member
/// presenters must agree on, asserted in tests.
pub struct ToolComponent {
    /// Stable identity used by the Settings panel and by tests.
    pub id: &'static str,
    /// Panel label.
    pub label: &'static str,
    /// Panel description (one concise line).
    pub description: &'static str,
    /// Member tools: canonical name first, then aliases and legacy persisted
    /// spellings — each paired with the presenter that renders it.
    pub members: &'static [(&'static str, &'static dyn ToolPresenter)],
    /// The disclosure default for the whole component, applied to every member
    /// name unless the user configures otherwise.
    pub expanded_by_default: bool,
}

impl std::fmt::Debug for ToolComponent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolComponent")
            .field("id", &self.id)
            .field("names", &self.names().collect::<Vec<_>>())
            .finish()
    }
}

impl ToolComponent {
    /// Every tool name this component claims.
    pub fn names(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.members.iter().map(|(name, _)| *name)
    }

    /// The presenter for a name this component claims.
    pub fn presenter_for(&self, name: &str) -> Option<&'static dyn ToolPresenter> {
        self.members
            .iter()
            .find(|(member, _)| *member == name)
            .map(|(_, presenter)| *presenter)
    }

    /// The name whose configuration is authoritative for the row. Every member
    /// name resolves to the same policy, so their built-in defaults agree.
    pub fn primary_name(&self) -> &'static str {
        self.members[0].0
    }

    /// The built-in (unconfigured) disclosure default for this component.
    pub fn default_expanded(&self) -> bool {
        self.expanded_by_default
    }
}

/// Every dispatched tool component, in Settings-panel order.
///
/// Anything not listed here resolves to [`fallback::FallbackPresenter`] —
/// unknown tools, MCP-provided tools — and stays out of the panel: their
/// presentation defaults to the conservative collapsed state because a
/// dynamic tool has no declared component identity to configure.
pub static TOOL_COMPONENTS: &[ToolComponent] = &[
    ToolComponent {
        id: "command",
        label: "Command Execution Logs",
        description: "Expand shell execution output logs and terminal commands by default",
        members: &[
            ("execute_command", &execute_command::ExecuteCommandPresenter),
        ],
        expanded_by_default: true,
    },
    ToolComponent {
        id: "diff",
        label: "File Changes (Diffs)",
        description: "Expand file modifications, patch diffs, and write previews by default",
        members: &[
            ("edit_text", &edit_text::EditPresenter),
            ("write_file", &edit_text::WritePresenter),
        ],
        expanded_by_default: true,
    },
    ToolComponent {
        id: "read",
        label: "File Content Previews",
        description: "Expand file content views and source inspections by default",
        members: &[
            ("read_text", &read_text::ReadPresenter),
            ("read_file", &read_text::ReadPresenter),
            ("read", &read_text::ReadPresenter),
        ],
        expanded_by_default: false,
    },
    ToolComponent {
        id: "read_image",
        label: "Image Reads",
        description: "Expand image read confirmations and their delivery notices by default",
        members: &[("read_image", &read_image::ReadImagePresenter)],
        expanded_by_default: false,
    },
    ToolComponent {
        id: "search",
        label: "Search & Grep Results",
        description: "Expand search findings, regex matches, and directory listings by default",
        members: &[
            ("search_text", &search::SearchTextPresenter),
            ("find_files", &search::FindFilesPresenter),
            ("list_dir", &search::ListDirPresenter),
        ],
        expanded_by_default: false,
    },
    ToolComponent {
        id: "web_article",
        label: "Web Article Reads",
        description: "Expand page reads and their extracted prose by default",
        members: &[("read_url", &web::WebReaderPresenter)],
        expanded_by_default: false,
    },
    ToolComponent {
        id: "web_search",
        label: "Web Search Results",
        description: "Expand web search result cards and source pills by default",
        members: &[("search_web", &web::WebSearchPresenter)],
        expanded_by_default: false,
    },
    ToolComponent {
        id: "todo",
        label: "Todo & Task Checklists",
        description: "Expand task checklists and progress status updates by default",
        members: &[
            ("todo", &meta::TodoPresenter),
            ("write_todos", &meta::TodoPresenter),
            ("update_todo", &meta::TodoPresenter),
            ("todo_update", &meta::TodoPresenter),
        ],
        expanded_by_default: false,
    },
    ToolComponent {
        id: "subagent",
        label: "Subagent Delegations",
        description: "Expand subagent execution traces and delegation steps by default",
        members: &[
            ("spawn_agent", &meta::SubagentPresenter),
            ("delegate_code", &meta::SubagentPresenter),
        ],
        expanded_by_default: false,
    },
    ToolComponent {
        id: "skill",
        label: "Skill Activations",
        description: "Expand skill activations and their loaded guidance by default",
        members: &[("use_skill", &meta::UseSkillPresenter)],
        expanded_by_default: false,
    },
    ToolComponent {
        id: "ask_user",
        label: "Clarifying Questions",
        description: "Expand agent questions and their recorded answers by default",
        members: &[("ask_user", &ask_user::AskUserPresenter)],
        expanded_by_default: false,
    },
    ToolComponent {
        id: "mcp",
        label: "External MCP Tools",
        description: "Expand Model Context Protocol tool calls and responses by default",
        members: &[("mcp", &mcp::McpPresenter)],
        expanded_by_default: false,
    },
];

/// Resolve the component owning a tool name, or `None` for unregistered /
/// dynamic tools.
pub fn component_for(name: &str) -> Option<&'static ToolComponent> {
    if let Some(component) = TOOL_COMPONENTS
        .iter()
        .find(|component| component.members.iter().any(|(member, _)| *member == name))
    {
        return Some(component);
    }
    if let Some((_, base)) = name.split_once(':') {
        if let Some(component) = component_for(base) {
            return Some(component);
        }
    }
    if name.starts_with("mcp__") {
        return TOOL_COMPONENTS.iter().find(|c| c.id == "mcp");
    }
    None
}

/// Resolve the presenter for a tool name, falling back to a generic presenter
/// for unknown tools.
pub fn presenter_for(name: &str) -> &'static dyn ToolPresenter {
    if let Some(component) = component_for(name) {
        if let Some(presenter) = component.presenter_for(name) {
            return presenter;
        }
        if let Some((_, base)) = name.split_once(':') {
            if let Some(presenter) = component.presenter_for(base) {
                return presenter;
            }
        }
        if component.id == "mcp" {
            return &mcp::McpPresenter;
        }
    }
    &fallback::FallbackPresenter
}

/// Sanitize a string to guarantee single-line presentation:
/// collapses newlines, carriage returns, and consecutive whitespace into single spaces,
/// and strips non-printable control characters.
pub fn sanitize_single_line(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_whitespace = false;
    for ch in s.chars() {
        if ch == '\n' || ch == '\r' || ch.is_whitespace() {
            if !in_whitespace && !out.is_empty() {
                out.push(' ');
                in_whitespace = true;
            }
        } else if !ch.is_control() {
            out.push(ch);
            in_whitespace = false;
        }
    }
    if out.ends_with(' ') {
        out.pop();
    }
    out
}

/// Header budget for collapsed summaries (chars). Matches the previous
/// `argument_summary` cap so the migration is visually identical.
const SUMMARY_BUDGET: usize = 72;

/// Build the collapsed summary for a tool step from its raw JSON arguments.
///
/// Parses the arguments once: non-object / invalid JSON falls back to a
/// truncated raw string (preserving the pre-refactor behavior for malformed
/// or scalar argument payloads). This is the entry point step 2 will call from
/// `document.rs` in place of `argument_summary`.
pub fn summary_for(name: &str, arguments: &str, profile: Option<&str>) -> String {
    let line = semantic_summary_for(name, arguments, profile, None);
    let sanitized = sanitize_single_line(&line.to_plain_text());
    let trimmed = sanitized.trim();
    if trimmed.is_empty() {
        if !name.is_empty() {
            fallback::prettify_tool_name(name)
        } else {
            "Tool".to_string()
        }
    } else {
        truncate(trimmed, SUMMARY_BUDGET)
    }
}

/// Build the structured semantic summary for a tool step from its raw JSON arguments (ADR-0206).
pub fn semantic_summary_for(
    name: &str,
    arguments: &str,
    profile: Option<&str>,
    workspace_root: Option<&std::path::Path>,
) -> SemanticLine<'static> {
    let parsed: Option<Value> = serde_json::from_str(arguments).ok();
    let line = match parsed.as_ref().and_then(Value::as_object) {
        Some(obj) => {
            let view = ToolView {
                name,
                args: obj,
                profile,
                workspace_root,
            };
            presenter_for(name).render_summary(&view).into_owned()
        }
        None => {
            if arguments.trim().is_empty() {
                let empty = serde_json::Map::new();
                let view = ToolView {
                    name,
                    args: &empty,
                    profile,
                    workspace_root,
                };
                presenter_for(name).render_summary(&view).into_owned()
            } else {
                SemanticLine::plain(arguments.to_string())
            }
        }
    };

    // Invariant: every tool summary MUST have a non-empty head.
    if line.to_plain_text().trim().is_empty() {
        let fallback_head = if !name.is_empty() {
            fallback::prettify_tool_name(name)
        } else {
            "Tool".to_string()
        };
        SemanticLine::plain(fallback_head)
    } else {
        line
    }
}

/// Build explicit renderable hunks from legacy tool arguments. Current
/// completed edits use their structured Patch result instead; this path keeps
/// restored sessions created before structured results were persisted usable.
pub fn diff_hunks_for(name: &str, arguments: &str) -> Vec<DiffHunk> {
    let Ok(value) = serde_json::from_str::<Value>(arguments) else {
        return Vec::new();
    };
    let get = |key: &str| value.get(key).and_then(Value::as_str).unwrap_or("");
    match name {
        "edit_text" => diff::line_diff_hunks(get("old_string"), get("new_string"), 0),
        "write_file" => diff::line_diff_hunks("", get("content"), 0),
        _ => Vec::new(),
    }
}

/// Truncate to `max_chars` characters, appending an ellipsis when clipped.
/// Local copy of `document::truncate`; the document-side copy is removed in
/// step 2 once `argument_summary` is gone.
pub fn truncate(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let prefix = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{}...", prefix)
    } else {
        prefix
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(name: &str, args: serde_json::Value) -> String {
        summary_for(name, &args.to_string(), None)
    }

    #[test]
    fn dispatches_known_tools_to_named_summaries() {
        assert_eq!(
            summary("read_text", serde_json::json!({"path": "src/main.rs"})),
            "Read src/main.rs"
        );
        assert_eq!(
            summary("edit_text", serde_json::json!({"path": "a.rs"})),
            "Edit a.rs"
        );
        assert_eq!(
            summary("write_file", serde_json::json!({"path": "a.rs"})),
            "Write a.rs"
        );
        assert_eq!(
            summary(
                "search_text",
                serde_json::json!({"query": "ToolStep", "path": "src"})
            ),
            "Search \"ToolStep\" in src"
        );
        assert_eq!(
            summary(
                "search_web",
                serde_json::json!({"query": "rust async channels"})
            ),
            "Web search \"rust async channels\""
        );
        assert_eq!(
            summary(
                "read_url",
                serde_json::json!({"url": "https://example.com"})
            ),
            "Read https://example.com"
        );
    }

    #[test]
    fn execute_command_summary_uses_comm_name() {
        assert_eq!(
            summary(
                "execute_command",
                serde_json::json!({"command": "cargo build\nmore"})
            ),
            "Run cargo"
        );
    }

    #[test]
    fn empty_arguments_fallback_to_named_presenter_defaults() {
        assert_eq!(summary_for("read_text", "", None), "Read file");
        assert_eq!(summary_for("edit_text", "", None), "Edit text");
        assert_eq!(summary_for("write_file", "", None), "Write file");
        assert_eq!(summary_for("execute_command", "", None), "Run command");
    }

    #[test]
    fn namespaced_tool_names_resolve_to_canonical_presenters() {
        assert_eq!(
            summary("default_api:read_text", serde_json::json!({"path": "src/main.rs"})),
            "Read src/main.rs"
        );
        assert_eq!(
            summary("default_api:execute_command", serde_json::json!({"command": "cargo test"})),
            "Run cargo"
        );
        assert_eq!(
            summary("google:edit_text", serde_json::json!({"path": "foo.rs"})),
            "Edit foo.rs"
        );
    }

    #[test]
    fn whitespace_and_invalid_arguments_never_produce_empty_summary() {
        assert_eq!(summary_for("read_text", "   ", None), "Read file");
        assert_eq!(summary_for("execute_command", "   ", None), "Run command");
        assert_eq!(summary_for("unknown_custom", "   ", None), "unknown_custom");
        assert_eq!(summary_for("", "   ", None), "Tool");
    }

    #[test]
    fn unknown_tool_leads_with_cleaned_name_then_key() {
        assert_eq!(
            summary("mcp__foo__bar", serde_json::json!({"query": "hello"})),
            "⚡ foo · bar \"hello\""
        );
        assert_eq!(
            summary("mcp__foo__bar", serde_json::json!({"unknown": 1})),
            "⚡ foo · bar unknown: 1"
        );
        assert_eq!(
            summary("custom__foo__bar", serde_json::json!({"query": "hello"})),
            "custom / foo / bar hello"
        );
        assert_eq!(
            summary("custom__foo__bar", serde_json::json!({"unknown": 1})),
            "custom / foo / bar"
        );
    }

    #[test]
    fn non_object_arguments_truncate_raw() {
        assert_eq!(summary_for("execute_command", "not json", None), "not json");
    }

    #[test]
    fn from_status_classifies_every_lifecycle_including_cancelled() {
        use crate::model::document::ToolStepStatus;
        assert_eq!(
            ToolStatus::from_status(ToolStepStatus::Running),
            ToolStatus::Running
        );
        assert_eq!(ToolStatus::from_status(ToolStepStatus::Ok), ToolStatus::Ok);
        assert_eq!(
            ToolStatus::from_status(ToolStepStatus::Failed),
            ToolStatus::Failed
        );
        // The new terminal state must round-trip so an aborted step can never
        // be misclassified as still running.
        assert_eq!(
            ToolStatus::from_status(ToolStepStatus::Cancelled),
            ToolStatus::Cancelled
        );
    }

    #[test]
    fn sanitize_single_line_collapses_newlines_and_whitespace() {
        assert_eq!(
            sanitize_single_line("python3 -c\n'import os\nprint(1)'"),
            "python3 -c 'import os print(1)'"
        );
        assert_eq!(
            sanitize_single_line("  hello \r\n  world\t\t!  "),
            "hello world !"
        );
    }

    /// The component table is the single source of truth for name → presenter
    /// resolution: `presenter_for` must agree with it for every declared name,
    /// and no declared name may be unreachable.
    #[test]
    fn component_table_is_authoritative_for_presenter_resolution() {
        for component in TOOL_COMPONENTS {
            assert!(
                component.names().next().is_some(),
                "component {} declares no tool names",
                component.id
            );
            for name in component.names() {
                let resolved = presenter_for(name);
                let expected = component
                    .presenter_for(name)
                    .expect("component must resolve its own member");
                assert!(
                    std::ptr::eq(resolved, expected),
                    "presenter_for({name}) disagrees with component {}",
                    component.id
                );
                assert_eq!(
                    component_for(name).map(|c| c.id),
                    Some(component.id),
                    "component_for({name}) must resolve to {}",
                    component.id
                );
            }
        }
    }

    /// A member presenter's own built-in default must agree with the
    /// component's declared policy. Two sources of truth for the same fact is
    /// exactly the drift the table exists to prevent, so the disagreement is
    /// caught here rather than by a user noticing an inconsistent toggle.
    #[test]
    fn member_presenter_defaults_agree_with_the_component_policy() {
        for component in TOOL_COMPONENTS {
            for (name, presenter) in component.members {
                assert_eq!(
                    presenter.default_expanded(),
                    component.default_expanded(),
                    "{name}'s presenter default disagrees with component {}",
                    component.id
                );
            }
        }
    }

    /// Component ids are stable identities (config keys / tests depend on
    /// them), no two components may claim the same tool name, and the primary
    /// name of each is unique — so no two rows can claim the same panel slot.
    #[test]
    fn component_ids_and_tool_names_are_disjoint() {
        let mut ids = std::collections::HashSet::new();
        let mut names = std::collections::HashSet::new();
        for component in TOOL_COMPONENTS {
            assert!(
                ids.insert(component.id),
                "duplicate component id {}",
                component.id
            );
            assert!(
                !component.label.trim().is_empty() && !component.description.trim().is_empty(),
                "component {} must carry panel copy",
                component.id
            );
            for name in component.names() {
                assert!(
                    names.insert(name),
                    "{name} is claimed by more than one component"
                );
            }
        }
    }

    /// A legacy/alias spelling must reach the same presenter *and* the same
    /// configured disclosure, so a persisted `bash` step honours the
    /// `execute_command` choice. This is the regression for the previous
    /// hand-maintained alias fan-out.
    #[test]
    fn every_alias_honours_the_component_configuration() {
        for component in TOOL_COMPONENTS {
            let mut config = crate::config::TuiConfig::default();
            // Flip the row through the same entry point the Settings panel
            // uses, then require every member name to observe the change.
            crate::config::set_component_default_expanded(&mut config, component, true);
            for name in component.names() {
                assert!(
                    crate::config::tool_default_expanded(&config, name),
                    "{name} ignored the {} configuration",
                    component.id
                );
            }
        }
    }

    /// Adding a presenter without declaring it is the drift this table exists
    /// to prevent: an undeclared tool must stay out of the panel *and* off the
    /// expanded default, so an unrecognized tool can never surprise the user
    /// with a wide-open body it never advertised.
    #[test]
    fn unknown_tools_stay_unconfigured_and_collapsed() {
        let config = crate::config::TuiConfig::default();
        for name in ["totally_new_tool", "custom_unknown_plugin"] {
            assert!(component_for(name).is_none(), "{name} must not be declared");
            assert!(
                !crate::config::tool_default_expanded(&config, name),
                "{name} must default to collapsed"
            );
            assert!(!presenter_for(name).default_expanded());
        }
    }

    #[test]
    fn mcp_tools_are_associated_with_mcp_component_and_default_to_collapsed() {
        let config = crate::config::TuiConfig::default();
        let name = "mcp__github__create_issue";
        let component = component_for(name).expect("mcp component must match");
        assert_eq!(component.id, "mcp");
        assert!(
            !crate::config::tool_default_expanded(&config, name),
            "mcp tools must default to collapsed"
        );
        assert!(!presenter_for(name).default_expanded());
    }
}
