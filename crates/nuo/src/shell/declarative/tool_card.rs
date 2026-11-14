//! Pure Declarative ToolStep / ToolCard Component (ADR-0072 / ADR-0073 / ADR-0110).
//!
//! Provides isolated, deterministic rendering for tool execution results,
//! including inline diffs, command outputs, streaming state, and error handling.
//!
//! Enforces:
//! - `[INV-TUI-RUNNING-01]` (Strict Lifecycle-Tone Separation): In-flight tools
//!   render with the dedicated in-flight hue (`theme.running()`), bold, never
//!   `theme.dim()` and never the *structural* `theme.info()`.
//! - `[INV-TUI-RUNNING-02]` (Action Permissibility Tri-Partition): Read inspection
//!   (toggle expand, scroll body, examine output) remains 100% operational during Running.
//! - `[INV-TUI-RUNNING-03]` (Progress Grammar & Frozen Fact Transition): Structured
//!   `(running | Xs)` header grammar transitions to frozen duration on completion.

use std::path::Path;

use super::{
    Component,
    story::{Story, StoryEventOutcome, StorybookComponent},
};
use crate::shell::declarative::key::Key;
use nuo_ui_primitives::theme::Theme;
use nuo_view::model::document::{MessageKind, ToolStepStatus, TranscriptMessage};
use nuo_view::model::layout::{BlockRegion, LayoutMap, TOOL_STEP_BLOCK_IDX};
use nuo_view::tools::diff::{DiffLine, DiffOp, line_diff, line_diff_counts};
use nuotc::{
    Color, Frame, Line, Modifier, Paragraph, Rect, Span, Style,
    widgets::{Block, Borders},
};

/// Execution lifecycle status of a tool invocation (ADR-0110).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolStatus {
    Running {
        spinner_frame: usize,
        elapsed_secs: Option<u64>,
    },
    Success {
        duration_ms: Option<u64>,
    },
    Failed {
        exit_code: Option<i32>,
        error_msg: String,
        duration_ms: Option<u64>,
    },
}

impl ToolStatus {
    pub fn running(spinner_frame: usize) -> Self {
        Self::Running {
            spinner_frame,
            elapsed_secs: None,
        }
    }

    pub fn running_with_elapsed(spinner_frame: usize, elapsed_secs: u64) -> Self {
        Self::Running {
            spinner_frame,
            elapsed_secs: Some(elapsed_secs),
        }
    }

    pub fn success() -> Self {
        Self::Success { duration_ms: None }
    }

    pub fn success_with_duration(duration_ms: u64) -> Self {
        Self::Success {
            duration_ms: Some(duration_ms),
        }
    }

    pub fn failed(error_msg: impl Into<String>, exit_code: Option<i32>) -> Self {
        Self::Failed {
            exit_code,
            error_msg: error_msg.into(),
            duration_ms: None,
        }
    }

    pub fn is_running(&self) -> bool {
        matches!(self, Self::Running { .. })
    }
}

/// Rich structured payload produced by the tool (ADR-0112).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolPayload {
    Diff {
        old_text: String,
        new_text: String,
        line_offset: usize,
    },
    Command {
        command: String,
        output: String,
    },
    Code {
        content: String,
        start_line: usize,
        language: Option<String>,
    },
    Listing {
        entries: Vec<String>,
    },
    Matches {
        query: String,
        matches: Vec<String>,
    },
    Generic {
        content: String,
    },
}

/// Pure immutable inputs pushed to ToolCard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCardProps {
    pub tool_name: String,
    pub target_path: Option<String>,
    pub status: ToolStatus,
    pub payload: ToolPayload,
}

impl ToolCardProps {
    /// Canonical domain extraction bridge: projects an active or persisted [`TranscriptMessage`]
    /// into immutable [`ToolCardProps`] (ADR-0112 [INV-TUI-TIER-02]).
    pub fn from_transcript_message(
        msg: &TranscriptMessage,
        workspace_root: Option<&Path>,
    ) -> Option<Self> {
        let (name, arguments, output, structured, raw_status) = match &msg.kind {
            MessageKind::ToolStep {
                name,
                arguments,
                output,
                structured,
                status,
                ..
            } => (name.clone(), arguments, output, structured, *status),
            _ => return None,
        };

        // Extract semantic target path (or file/url argument)
        let target_path = if let Ok(serde_json::Value::Object(map)) =
            serde_json::from_str::<serde_json::Value>(arguments)
        {
            map.get("path")
                .or_else(|| map.get("file"))
                .or_else(|| map.get("url"))
                .and_then(|v| v.as_str())
                .map(String::from)
        } else {
            let streaming = nuo_view::tools::parse_streaming_arguments(arguments);
            streaming
                .get("path")
                .or_else(|| streaming.get("file"))
                .or_else(|| streaming.get("url"))
                .and_then(|v| v.as_str())
                .map(String::from)
                .or_else(|| {
                    msg.tool_step_semantic_summary(workspace_root)
                        .and_then(|(line, _)| {
                            let plain = line.to_plain_text();
                            let parts: Vec<&str> = plain.split_whitespace().collect();
                            if parts.len() >= 2 {
                                Some(parts[1].to_string())
                            } else {
                                None
                            }
                        })
                })
        };

        let status = match raw_status {
            ToolStepStatus::Ingesting | ToolStepStatus::Executing => ToolStatus::Running {
                spinner_frame: 0,
                elapsed_secs: None,
            },
            ToolStepStatus::Ok => ToolStatus::Success { duration_ms: None },
            ToolStepStatus::Failed => ToolStatus::Failed {
                exit_code: None,
                error_msg: output.clone().unwrap_or_default(),
                duration_ms: None,
            },
            ToolStepStatus::Denied => ToolStatus::Failed {
                exit_code: None,
                error_msg: "Permission denied by user".to_string(),
                duration_ms: None,
            },
            ToolStepStatus::Cancelled => ToolStatus::Failed {
                exit_code: None,
                error_msg: "Aborted by user interrupt".to_string(),
                duration_ms: None,
            },
            ToolStepStatus::Interrupted => ToolStatus::Failed {
                exit_code: None,
                error_msg: "Interrupted in-flight".to_string(),
                duration_ms: None,
            },
        };

        let presenter = nuo_view::tools::presenter_for(&name);
        // `read` is polymorphic: its presenter declares `Auto`, and the
        // structured result picks the concrete renderer. The string output is
        // not inspected for this decision — a listing's model-facing header
        // must not become a bullet row.
        let result_kind = match presenter.result_kind() {
            nuo_view::tools::ResultKind::Auto => match structured.as_deref() {
                Some(nuo_protocol::ToolOutput::Listing { .. }) => {
                    nuo_view::tools::ResultKind::Listing
                }
                _ => nuo_view::tools::ResultKind::Code,
            },
            kind => kind,
        };
        let payload = match result_kind {
            nuo_view::tools::ResultKind::Diff => {
                if let Some(tool_output) = structured {
                    match &**tool_output {
                        nuo_protocol::ToolOutput::Patch {
                            old,
                            new,
                            start_line,
                            ..
                        } => {
                            let mut start = *start_line;
                            if start == 0
                                && let Some(target) = &target_path
                            {
                                if let Some(resolved) =
                                    crate::shell::disclosure::renderers::payloads::find_start_line_in_file(
                                        target,
                                        workspace_root,
                                        old,
                                        new,
                                    )
                                {
                                    start = resolved;
                                }
                            }
                            ToolPayload::Diff {
                                old_text: old.clone(),
                                new_text: new.clone(),
                                line_offset: start.saturating_sub(1),
                            }
                        }
                        _ => ToolPayload::Generic {
                            content: output.clone().unwrap_or_default(),
                        },
                    }
                } else {
                    let (old_buf, new_buf) =
                        crate::shell::disclosure::renderers::payloads::extract_diff_strings_for_tool(
                            &name, arguments,
                        );
                    if !old_buf.is_empty() || !new_buf.is_empty() {
                        let start = if let Some(target) = &target_path {
                            crate::shell::disclosure::renderers::payloads::find_start_line_in_file(
                                target,
                                workspace_root,
                                &old_buf,
                                &new_buf,
                            )
                            .unwrap_or(0)
                        } else {
                            0
                        };
                        ToolPayload::Diff {
                            old_text: old_buf,
                            new_text: new_buf,
                            line_offset: start.saturating_sub(1),
                        }
                    } else {
                        ToolPayload::Generic {
                            content: output.clone().unwrap_or_default(),
                        }
                    }
                }
            }
            nuo_view::tools::ResultKind::Command => {
                let cmd = if let Some(tool_output) = structured {
                    match &**tool_output {
                        nuo_protocol::ToolOutput::Shell { command, .. } => command.clone(),
                        _ => arguments.clone(),
                    }
                } else {
                    arguments.clone()
                };
                let out = if let Some(tool_output) = structured {
                    match &**tool_output {
                        nuo_protocol::ToolOutput::Shell { stdout, stderr, .. } => {
                            if !stderr.is_empty() {
                                format!("{stdout}\n{stderr}")
                            } else {
                                stdout.clone()
                            }
                        }
                        _ => output.clone().unwrap_or_default(),
                    }
                } else {
                    output.clone().unwrap_or_default()
                };
                ToolPayload::Command {
                    command: cmd,
                    output: out,
                }
            }
            nuo_view::tools::ResultKind::Code => {
                let (raw_content, start_line, explicit_lang) = match structured {
                    Some(tool_output) => match &**tool_output {
                        nuo_protocol::ToolOutput::Code {
                            text,
                            start_line,
                            lang,
                            ..
                        } => (text.clone(), *start_line, lang.clone()),
                        _ => (output.clone().unwrap_or_default(), 1, None),
                    },
                    None => {
                        let offset = serde_json::from_str::<serde_json::Value>(arguments)
                            .ok()
                            .and_then(|v| {
                                v.get("offset")
                                    .and_then(|o| o.as_u64())
                                    .or_else(|| v.get("start_line").and_then(|o| o.as_u64()))
                            })
                            .map(|o| o as usize)
                            .unwrap_or(1);
                        (output.clone().unwrap_or_default(), offset, None)
                    }
                };
                ToolPayload::Code {
                    content: raw_content,
                    start_line,
                    language: explicit_lang,
                }
            }
            nuo_view::tools::ResultKind::Listing => {
                let entries = match structured.as_deref() {
                    Some(nuo_protocol::ToolOutput::Listing { entries, .. }) => entries.clone(),
                    _ => output
                        .as_deref()
                        .unwrap_or("")
                        .lines()
                        .map(String::from)
                        .collect(),
                };
                ToolPayload::Listing { entries }
            }
            nuo_view::tools::ResultKind::Matches => {
                let (query, matches) = match structured.as_deref() {
                    Some(nuo_protocol::ToolOutput::Matches { pattern, lines }) => {
                        (pattern.clone(), lines.clone())
                    }
                    _ => (
                        arguments.clone(),
                        output
                            .as_deref()
                            .unwrap_or("")
                            .lines()
                            .map(String::from)
                            .collect(),
                    ),
                };
                ToolPayload::Matches { query, matches }
            }
            _ => ToolPayload::Generic {
                content: output.clone().unwrap_or_default(),
            },
        };

        Some(Self {
            tool_name: name,
            target_path,
            status,
            payload,
        })
    }
}

/// Sovereign volatile interaction state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCardState {
    pub expanded: bool,
    pub scroll: usize,
}

impl Default for ToolCardState {
    fn default() -> Self {
        Self {
            expanded: true,
            scroll: 0,
        }
    }
}

/// Discrete messages handled by ToolCard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolCardMsg {
    ToggleExpand,
    ScrollUp(usize),
    ScrollDown(usize),
    ScrollToTop,
    ScrollToBottom,
}

/// Outward outcomes emitted to parent orchestration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolCardCmd {
    ExpansionToggled(bool),
}

/// The declarative ToolCard component.
pub struct ToolCard;

impl ToolCard {
    /// Connected render bridge: paints ToolCard into a terminal frame and optionally
    /// registers hit regions into a layout map (ADR-0112 [INV-TUI-TIER-03]).
    pub fn render_with_layout(
        props: &ToolCardProps,
        state: &ToolCardState,
        frame: &mut Frame<'_>,
        area: Rect,
        theme: &Theme,
        mut layout_map: Option<&mut LayoutMap>,
        message_idx: Option<usize>,
    ) {
        if area.width < 12 || area.height < 2 {
            return;
        }

        // 1. Build Header Line
        let (status_badge, status_color) = match &props.status {
            ToolStatus::Running { spinner_frame, .. } => {
                let glyphs = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
                // Invariant [INV-TUI-RUNNING-01]: the in-flight hue, never
                // dimmed and never the structural `info` tone — a running card
                // and the `› turn N` header above it must not be the same color.
                (glyphs[spinner_frame % glyphs.len()], theme.running())
            }
            ToolStatus::Success { .. } => ("✓", theme.ok()),
            ToolStatus::Failed { .. } => ("✗", theme.error_fg),
        };

        let expand_indicator = if state.expanded { "▼" } else { "▶" };
        let mut header_spans = vec![
            Span::styled(
                format!("{expand_indicator} {status_badge} "),
                Style::default()
                    .fg(status_color)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                &props.tool_name,
                Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
            ),
        ];

        if let Some(target) = &props.target_path {
            header_spans.push(Span::raw(" "));
            header_spans.push(Span::styled(target, Style::default().fg(theme.system_fg)));
        }

        if let ToolPayload::Diff {
            old_text, new_text, ..
        } = &props.payload
        {
            let (added, removed) = line_diff_counts(old_text, new_text);
            header_spans.push(Span::raw(" ("));
            header_spans.push(Span::styled(
                format!("+{added}"),
                Style::default().fg(theme.ok()),
            ));
            header_spans.push(Span::raw(" "));
            header_spans.push(Span::styled(
                format!("-{removed}"),
                Style::default().fg(theme.error_fg),
            ));
            header_spans.push(Span::raw(")"));
        }

        // Status / elapsed progress formatting per [INV-TUI-RUNNING-03]
        match &props.status {
            ToolStatus::Running { elapsed_secs, .. } => {
                header_spans.push(Span::raw(" "));
                let label = match elapsed_secs {
                    Some(s) => format!("(running | {s}s)"),
                    None => "(running)".to_string(),
                };
                header_spans.push(Span::styled(label, Style::default().fg(theme.running())));
            }
            ToolStatus::Success {
                duration_ms: Some(ms),
            } => {
                header_spans.push(Span::raw(" "));
                header_spans.push(Span::styled(
                    format!("({ms}ms)"),
                    Style::default().fg(theme.text_muted),
                ));
            }
            ToolStatus::Failed {
                exit_code,
                duration_ms,
                ..
            } => {
                if let Some(code) = exit_code {
                    header_spans.push(Span::raw(" "));
                    header_spans.push(Span::styled(
                        format!("[exit {code}]"),
                        Style::default().fg(theme.error_fg),
                    ));
                }
                if let Some(ms) = duration_ms {
                    header_spans.push(Span::raw(" "));
                    header_spans.push(Span::styled(
                        format!("({ms}ms)"),
                        Style::default().fg(theme.text_muted),
                    ));
                }
            }
            _ => {}
        }

        let header_rect = Rect::new(area.x, area.y, area.width, 1);
        frame.render_widget(&mut Paragraph::new(Line::from(header_spans)), header_rect);

        if let (Some(lm), Some(mi)) = (&mut layout_map, message_idx) {
            lm.push(BlockRegion {
                message_idx: mi,
                block_idx: TOOL_STEP_BLOCK_IDX,
                start_byte: 0,
                end_byte: 0,
                text: props.tool_name.clone(),
                prefix_cols: 0,
                rect: header_rect,
                hidden_ranges: Vec::new(),
            });
        }

        if !state.expanded || area.height <= 2 {
            return;
        }

        // 2. Render Expanded Body Area
        // Invariant [INV-TUI-RUNNING-01]: Body is NOT dimmed; content retains full contrast
        let body_area = Rect::new(
            area.x.saturating_add(2),
            area.y.saturating_add(1),
            area.width.saturating_sub(3),
            area.height.saturating_sub(1),
        );

        let mut body_block = Block::default()
            .borders(Borders::LEFT)
            .border_style(Style::default().fg(theme.dim_fg))
            .style(Style::default().bg(theme.app_bg));
        let inner_body = body_block.inner(body_area);
        frame.render_widget(&mut body_block, body_area);

        match &props.payload {
            ToolPayload::Diff {
                old_text,
                new_text,
                line_offset,
            } => {
                let diff_lines = line_diff(old_text, new_text, *line_offset);
                let available_rows = inner_body.height as usize;
                let visible_lines: Vec<&DiffLine> = diff_lines
                    .iter()
                    .skip(state.scroll)
                    .take(available_rows)
                    .collect();

                let mut rendered_rows = Vec::new();
                for dl in visible_lines {
                    let (op_char, op_color, op_bg) = match dl.op {
                        DiffOp::Add => ("+", theme.ok(), Color::Reset),
                        DiffOp::Remove => ("-", theme.error_fg, Color::Reset),
                        DiffOp::Context => (" ", theme.text_muted, Color::Reset),
                    };

                    let old_no_str = dl
                        .old_no
                        .map(|n| format!("{n:>4}"))
                        .unwrap_or_else(|| "    ".to_string());
                    let new_no_str = dl
                        .new_no
                        .map(|n| format!("{n:>4}"))
                        .unwrap_or_else(|| "    ".to_string());

                    let mut spans = vec![
                        Span::styled(
                            format!("{old_no_str} {new_no_str} "),
                            Style::default().fg(theme.dim_fg),
                        ),
                        Span::styled(
                            format!("{op_char} "),
                            Style::default().fg(op_color).add_modifier(Modifier::BOLD),
                        ),
                    ];

                    for frag in &dl.frags {
                        let mut style = Style::default().fg(op_color).bg(op_bg);
                        if frag.changed {
                            style = style.add_modifier(Modifier::BOLD);
                        }
                        spans.push(Span::styled(&frag.text, style));
                    }

                    rendered_rows.push(Line::from(spans));
                }

                frame.render_widget(&mut Paragraph::new(rendered_rows), inner_body);
            }
            ToolPayload::Command { output, .. } => {
                let lines: Vec<Line> = output
                    .lines()
                    .skip(state.scroll)
                    .take(inner_body.height as usize)
                    .map(|l| Line::from(Span::styled(l, Style::default().fg(theme.text_muted))))
                    .collect();
                frame.render_widget(&mut Paragraph::new(lines), inner_body);
            }
            ToolPayload::Code {
                content,
                start_line,
                ..
            } => {
                let available_rows = inner_body.height as usize;
                let lines: Vec<Line> = content
                    .lines()
                    .enumerate()
                    .skip(state.scroll)
                    .take(available_rows)
                    .map(|(i, l)| {
                        let line_no = start_line + i;
                        Line::from(vec![
                            Span::styled(
                                format!("{line_no:>4} "),
                                Style::default().fg(theme.text_muted),
                            ),
                            Span::styled(l, Style::default().fg(theme.text)),
                        ])
                    })
                    .collect();
                frame.render_widget(&mut Paragraph::new(lines), inner_body);
            }
            ToolPayload::Listing { entries } => {
                let available_rows = inner_body.height as usize;
                let lines: Vec<Line> = entries
                    .iter()
                    .skip(state.scroll)
                    .take(available_rows)
                    .map(|entry| {
                        Line::from(vec![
                            Span::styled("• ", Style::default().fg(theme.info())),
                            Span::styled(entry, Style::default().fg(theme.text)),
                        ])
                    })
                    .collect();
                frame.render_widget(&mut Paragraph::new(lines), inner_body);
            }
            ToolPayload::Matches { query, matches } => {
                let available_rows = inner_body.height as usize;
                let mut lines: Vec<Line> = Vec::new();
                if state.scroll == 0 {
                    lines.push(Line::from(vec![
                        Span::styled("query: ", Style::default().fg(theme.dim_fg)),
                        Span::styled(
                            query,
                            Style::default()
                                .fg(theme.info())
                                .add_modifier(Modifier::BOLD),
                        ),
                    ]));
                }
                let skip = state.scroll.saturating_sub(1);
                for m in matches
                    .iter()
                    .skip(skip)
                    .take(available_rows.saturating_sub(lines.len()))
                {
                    lines.push(Line::from(Span::styled(m, Style::default().fg(theme.text))));
                }
                frame.render_widget(&mut Paragraph::new(lines), inner_body);
            }
            ToolPayload::Generic { content } => {
                let lines: Vec<Line> = content
                    .lines()
                    .skip(state.scroll)
                    .take(inner_body.height as usize)
                    .map(|l| Line::from(Span::styled(l, Style::default().fg(theme.text))))
                    .collect();
                frame.render_widget(&mut Paragraph::new(lines), inner_body);
            }
        }
    }
}

impl Component for ToolCard {
    type Props = ToolCardProps;
    type State = ToolCardState;
    type Msg = ToolCardMsg;
    type Cmd = ToolCardCmd;

    fn render(
        props: &Self::Props,
        state: &Self::State,
        frame: &mut Frame<'_>,
        area: Rect,
        theme: &Theme,
    ) {
        Self::render_with_layout(props, state, frame, area, theme, None, None);
    }

    fn update(props: &Self::Props, state: &mut Self::State, msg: Self::Msg) -> Option<Self::Cmd> {
        // Invariant [INV-TUI-RUNNING-02]: Observational inspection (expand/scroll)
        // remains unconditionally functional even while props.status.is_running()
        match msg {
            ToolCardMsg::ToggleExpand => {
                state.expanded = !state.expanded;
                Some(ToolCardCmd::ExpansionToggled(state.expanded))
            }
            ToolCardMsg::ScrollUp(n) => {
                state.scroll = state.scroll.saturating_sub(n);
                None
            }
            ToolCardMsg::ScrollDown(n) => {
                let total = match &props.payload {
                    ToolPayload::Diff {
                        old_text,
                        new_text,
                        line_offset,
                    } => line_diff(old_text, new_text, *line_offset).len(),
                    ToolPayload::Command { output, .. } => output.lines().count(),
                    ToolPayload::Code { content, .. } => content.lines().count(),
                    ToolPayload::Listing { entries } => entries.len(),
                    ToolPayload::Matches { matches, .. } => matches.len() + 1,
                    ToolPayload::Generic { content } => content.lines().count(),
                };
                if state.scroll + n < total {
                    state.scroll += n;
                }
                None
            }
            ToolCardMsg::ScrollToTop => {
                state.scroll = 0;
                None
            }
            ToolCardMsg::ScrollToBottom => {
                let total = match &props.payload {
                    ToolPayload::Diff {
                        old_text,
                        new_text,
                        line_offset,
                    } => line_diff(old_text, new_text, *line_offset).len(),
                    ToolPayload::Command { output, .. } => output.lines().count(),
                    ToolPayload::Code { content, .. } => content.lines().count(),
                    ToolPayload::Listing { entries } => entries.len(),
                    ToolPayload::Matches { matches, .. } => matches.len() + 1,
                    ToolPayload::Generic { content } => content.lines().count(),
                };
                state.scroll = total.saturating_sub(5);
                None
            }
        }
    }
}

impl StorybookComponent for ToolCard {
    fn category() -> &'static str {
        "Disclosure"
    }

    fn name() -> &'static str {
        "ToolCard"
    }

    fn stories() -> Vec<Story<Self::Props, Self::State>> {
        let old_code = r#"pub fn calculate_total(items: &[Item]) -> u64 {
    let mut sum = 0;
    for item in items {
        sum += item.price;
    }
    sum
}"#;

        let new_code = r#"pub fn calculate_total(items: &[Item]) -> u64 {
    // Optimized with iterator folding
    items.iter().map(|item| item.price).sum()
}"#;

        let mut mass_old = String::new();
        let mut mass_new = String::new();
        for i in 0..100 {
            mass_old.push_str(&format!(
                "    let var_{i} = calculate_legacy_metric({i});\n"
            ));
            mass_new.push_str(&format!(
                "    let var_{i} = calculate_fast_simd_metric({i}, ACCEL_V2);\n"
            ));
        }

        vec![
            Story::new(
                "Streaming In-Flight",
                "Tool call actively executing with spinner, elapsed timer, and live diff preview",
                ToolCardProps {
                    tool_name: "edit_text".to_string(),
                    target_path: Some("crates/nuo/src/shell/render.rs".to_string()),
                    status: ToolStatus::Running {
                        spinner_frame: 3,
                        elapsed_secs: Some(2),
                    },
                    payload: ToolPayload::Generic {
                        content: "Waiting for stream chunks from cognitive bridge...".to_string(),
                    },
                },
                ToolCardState {
                    expanded: true,
                    scroll: 0,
                },
            ),
            Story::new(
                "Standard Edit Diff",
                "Inline code diff demonstrating added/removed line numbers and word-level modifications",
                ToolCardProps {
                    tool_name: "edit_text".to_string(),
                    target_path: Some("src/billing/calc.rs".to_string()),
                    status: ToolStatus::Success {
                        duration_ms: Some(140),
                    },
                    payload: ToolPayload::Diff {
                        old_text: old_code.to_string(),
                        new_text: new_code.to_string(),
                        line_offset: 42,
                    },
                },
                ToolCardState {
                    expanded: true,
                    scroll: 0,
                },
            ),
            Story::new(
                "Massive 100-Line Diff (Stress)",
                "Virtual scroll stress test over 100 modified lines across boundary columns",
                ToolCardProps {
                    tool_name: "edit_text".to_string(),
                    target_path: Some("crates/simd/src/matrix.rs".to_string()),
                    status: ToolStatus::Success {
                        duration_ms: Some(850),
                    },
                    payload: ToolPayload::Diff {
                        old_text: mass_old,
                        new_text: mass_new,
                        line_offset: 1,
                    },
                },
                ToolCardState {
                    expanded: true,
                    scroll: 0,
                },
            ),
            Story::new(
                "Command Failure (Error State)",
                "Shell execution failure displaying non-zero exit code and stderr diagnostics",
                ToolCardProps {
                    tool_name: "execute_command".to_string(),
                    target_path: None,
                    status: ToolStatus::Failed {
                        exit_code: Some(101),
                        error_msg: "cargo test failed with 2 assertions broken".to_string(),
                        duration_ms: Some(420),
                    },
                    payload: ToolPayload::Command {
                        command: "cargo test -p nuo-harness".to_string(),
                        output: "error[E0425]: cannot find value `foo` in this scope\n  --> crates/nuo-harness/src/cognition/agent/rounds.rs:42:9\n   |\n42 |     foo.bar();\n   |     ^^^ not found in this scope".to_string(),
                    },
                },
                ToolCardState {
                    expanded: true,
                    scroll: 0,
                },
            ),
            Story::new(
                "Collapsed Summary",
                "Compact single-row summary view with +N -M count indicators",
                ToolCardProps {
                    tool_name: "edit_text".to_string(),
                    target_path: Some("crates/nuo-protocol/src/lib.rs".to_string()),
                    status: ToolStatus::Success { duration_ms: None },
                    payload: ToolPayload::Diff {
                        old_text: old_code.to_string(),
                        new_text: new_code.to_string(),
                        line_offset: 10,
                    },
                },
                ToolCardState {
                    expanded: false,
                    scroll: 0,
                },
            ),
            Story::new(
                "Code Block With Line Offsets",
                "File inspection view demonstrating 4-digit line numbering starting at line 1420",
                ToolCardProps {
                    tool_name: "read".to_string(),
                    target_path: Some("crates/nuo-protocol/src/codec.rs".to_string()),
                    status: ToolStatus::Success {
                        duration_ms: Some(25),
                    },
                    payload: ToolPayload::Code {
                        content: "pub fn decode_packet<R: Read>(mut reader: R) -> Result<Packet, CodecError> {\n    let header = PacketHeader::read_from(&mut reader)?;\n    let mut payload = vec![0u8; header.length as usize];\n    reader.read_exact(&mut payload)?;\n    Ok(Packet::new(header, payload))\n}".to_string(),
                        start_line: 1420,
                        language: Some("rust".to_string()),
                    },
                },
                ToolCardState {
                    expanded: true,
                    scroll: 0,
                },
            ),
            Story::new(
                "Search Text Grouped Matches",
                "Ripgrep token search results showing matched lines and file references",
                ToolCardProps {
                    tool_name: "search_text".to_string(),
                    target_path: None,
                    status: ToolStatus::Success {
                        duration_ms: Some(68),
                    },
                    payload: ToolPayload::Matches {
                        query: "INV-TUI-COLOR-01".to_string(),
                        matches: vec![
                            "docs/adr/0147.md:22: - [INV-TUI-COLOR-01] Semantic Color Purity".to_string(),
                            "crates/nuo/src/dev/color_purity.rs:1: //! (ADR-0147 [INV-TUI-COLOR-01])".to_string(),
                            "crates/nuo/tests/architecture_firewall.rs:36: test_inv_color_purity_firewall".to_string(),
                        ],
                    },
                },
                ToolCardState {
                    expanded: true,
                    scroll: 0,
                },
            ),
            Story::new(
                "Directory Hierarchy Listing",
                "Filesystem discovery payload with bulleted file and directory entries",
                ToolCardProps {
                    tool_name: "list_dir".to_string(),
                    target_path: Some("crates/nuo-ui-primitives/src".to_string()),
                    status: ToolStatus::Success {
                        duration_ms: Some(12),
                    },
                    payload: ToolPayload::Listing {
                        entries: vec![
                            "components/ (Directory)".to_string(),
                            "color_scheme.rs (12.4 KB)".to_string(),
                            "design.rs (16.2 KB)".to_string(),
                            "elevation.rs (14.8 KB)".to_string(),
                            "theme.rs (98.6 KB)".to_string(),
                        ],
                    },
                },
                ToolCardState {
                    expanded: true,
                    scroll: 0,
                },
            ),
            Story::new(
                "1,000-Line Folded Output (ADR-0115 Closed Compaction)",
                "Stress test simulating 1,000 lines of compilation logs folded into head and tail",
                ToolCardProps {
                    tool_name: "execute_command".to_string(),
                    target_path: None,
                    status: ToolStatus::Success {
                        duration_ms: Some(2140),
                    },
                    payload: ToolPayload::Command {
                        command: "cargo build --workspace --all-targets".to_string(),
                        output: {
                            let mut s = String::new();
                            s.push_str("   Compiling nuotc v0.0.6\n   Compiling nuo-protocol v0.0.86\n   Compiling nuo-host v0.0.86\n   Compiling nuo-ui-primitives v0.0.86\n");
                            s.push_str("   [... 990 lines of intermediate compilation artifacts folded per ADR-0115 ...]\n");
                            for i in 995..=1000 {
                                s.push_str(&format!("   Compiling satellite-crate-{i} v0.0.86\n"));
                            }
                            s.push_str("    Finished dev [unoptimized + debuginfo] target(s) in 2.14s\n");
                            s
                        },
                    },
                },
                ToolCardState {
                    expanded: true,
                    scroll: 0,
                },
            ),
        ]
    }

    fn map_key_to_msg(key: &Key, _props: &Self::Props, _state: &Self::State) -> Option<Self::Msg> {
        match key {
            Key::Char(' ') | Key::Enter => Some(ToolCardMsg::ToggleExpand),
            Key::Up | Key::Char('k') => Some(ToolCardMsg::ScrollUp(1)),
            Key::Down | Key::Char('j') => Some(ToolCardMsg::ScrollDown(1)),
            Key::PageUp => Some(ToolCardMsg::ScrollUp(5)),
            Key::PageDown => Some(ToolCardMsg::ScrollDown(5)),
            Key::Home | Key::Char('g') => Some(ToolCardMsg::ScrollToTop),
            Key::End | Key::Char('G') => Some(ToolCardMsg::ScrollToBottom),
            _ => None,
        }
    }

    fn tick(props: &mut Self::Props, _state: &mut Self::State) -> StoryEventOutcome {
        if let ToolStatus::Running {
            ref mut spinner_frame,
            ref mut elapsed_secs,
        } = props.status
        {
            *spinner_frame = (*spinner_frame + 1) % 10;
            if *spinner_frame == 0 {
                *elapsed_secs = Some(elapsed_secs.unwrap_or(0).saturating_add(1));
            }
            StoryEventOutcome::Consumed
        } else {
            StoryEventOutcome::Ignored
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nuo_view::model::document::TranscriptMessage;
    use tempfile::tempdir;

    #[test]
    fn from_transcript_message_uses_file_line_number_for_diff_offset() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("code.rs");
        let mut content = String::new();
        for i in 1..=30 {
            content.push_str(&format!("line {i}\n"));
        }
        std::fs::write(&file_path, &content).unwrap();

        // An edit at line 20: "line 20\n" -> "line 20 modified\n"
        let msg = TranscriptMessage::tool_step(
            "call_test",
            "edit_text",
            r#"{"path":"code.rs","old_string":"line 20\n","new_string":"line 20 modified\n"}"#,
        );

        let props = ToolCardProps::from_transcript_message(&msg, Some(dir.path()))
            .expect("must build ToolCardProps");
        match &props.payload {
            ToolPayload::Diff { line_offset, .. } => {
                // start_line is 20, so line_offset must be 19 (first line displayed as 20)
                assert_eq!(*line_offset, 19);
            }
            _ => panic!("Expected ToolPayload::Diff, got {:?}", props.payload),
        }

        // Now render it and verify the gutter line number is 20, not 1
        let mut terminal = nuotc::TestTerminal::new(80, 10);
        let theme = Theme::default();
        let state = ToolCardState {
            expanded: true,
            scroll: 0,
        };
        terminal.draw(|f| {
            ToolCard::render_with_layout(
                &props,
                &state,
                f,
                Rect::new(0, 0, 80, 10),
                &theme,
                None,
                None,
            );
        });

        let rendered = terminal
            .buffer()
            .cells()
            .iter()
            .map(|cell| cell.symbol.as_str())
            .collect::<String>();
        assert!(
            rendered.contains(" 20      -") && rendered.contains(" 20 +"),
            "Gutter must display line 20 for removed and added rows:\n{rendered}"
        );
        assert!(
            !rendered.contains("  1      -") && !rendered.contains("  1 +"),
            "Diff gutter must not show line 1:\n{rendered}"
        );
    }
}
