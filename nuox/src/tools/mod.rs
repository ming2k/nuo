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

mod ask_user;
mod diff;
mod edit_text;
mod execute_command;
mod fallback;
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
/// zero-sized unit structs resolved via [`presenter_for`].
pub trait ToolPresenter {
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

/// Resolve the presenter for a tool name, falling back to a generic presenter
/// for unknown / MCP tools.
pub fn presenter_for(name: &str) -> &'static dyn ToolPresenter {
    match name {
        "ask_user" => &ask_user::AskUserPresenter,
        "read_text" => &read_text::ReadPresenter,
        "read_image" => &read_image::ReadImagePresenter,
        "edit_text" => &edit_text::EditPresenter,
        "write_file" => &edit_text::WritePresenter,
        "run_command" | "execute_command" | "bash" => &execute_command::ExecuteCommandPresenter,
        "find_files" => &search::FindFilesPresenter,
        "list_dir" => &search::ListDirPresenter,
        "search_text" => &search::SearchTextPresenter,
        "read_url" => &web::WebReaderPresenter,
        "search_web" => &web::WebSearchPresenter,
        "write_todos" | "update_todo" | "todo" | "todo_update" => &meta::TodoPresenter,
        "spawn_agent" | "delegate_code" => &meta::SubagentPresenter,
        "use_skill" => &meta::UseSkillPresenter,
        _ => &fallback::FallbackPresenter,
    }
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
    truncate(&sanitize_single_line(&line.to_plain_text()), SUMMARY_BUDGET)
}

/// Build the structured semantic summary for a tool step from its raw JSON arguments (ADR-0206).
pub fn semantic_summary_for(
    name: &str,
    arguments: &str,
    profile: Option<&str>,
    workspace_root: Option<&std::path::Path>,
) -> SemanticLine<'static> {
    let parsed: Option<Value> = serde_json::from_str(arguments).ok();
    match parsed.as_ref().and_then(Value::as_object) {
        Some(obj) => {
            let view = ToolView {
                name,
                args: obj,
                profile,
                workspace_root,
            };
            presenter_for(name).render_summary(&view).into_owned()
        }
        None => SemanticLine::plain(arguments.to_string()),
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
    fn unknown_tool_leads_with_cleaned_name_then_key() {
        assert_eq!(
            summary("mcp__foo__bar", serde_json::json!({"query": "hello"})),
            "foo / bar hello"
        );
        // No recognizable argument: just the cleaned name.
        assert_eq!(
            summary("mcp__foo__bar", serde_json::json!({"unknown": 1})),
            "foo / bar"
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
}
