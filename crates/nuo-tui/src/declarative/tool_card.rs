//! Pure Declarative ToolStep / ToolCard Component (ADR-0072 / ADR-0073).
//!
//! Provides isolated, deterministic rendering for tool execution results,
//! including inline diffs, command outputs, streaming state, and error handling.

use nuotc::{
    Color, Frame, Line, Modifier, Paragraph, Rect, Span, Style,
    widgets::{Block, Borders},
};
use crossterm::event::{KeyCode, KeyEvent};

use crate::theme::Theme;
use crate::tools::diff::{DiffLine, DiffOp, line_diff, line_diff_counts};
use super::{Component, story::{Story, StorybookComponent}};

/// Execution lifecycle status of a tool invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolStatus {
    Running { spinner_frame: usize },
    Success,
    Failed { exit_code: Option<i32>, error_msg: String },
}

/// Rich structured payload produced by the tool.
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
        if area.width < 12 || area.height < 2 {
            return;
        }

        // 1. Build Header Line
        let (status_badge, status_color) = match &props.status {
            ToolStatus::Running { spinner_frame } => {
                let glyphs = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
                (glyphs[spinner_frame % glyphs.len()], theme.warning)
            }
            ToolStatus::Success => ("✓", theme.system_fg),
            ToolStatus::Failed { .. } => ("✗", theme.error_fg),
        };

        let expand_indicator = if state.expanded { "▼" } else { "▶" };
        let mut header_spans = vec![
            Span::styled(format!("{expand_indicator} {status_badge} "), Style::default().fg(status_color).add_modifier(Modifier::BOLD)),
            Span::styled(&props.tool_name, Style::default().fg(theme.text).add_modifier(Modifier::BOLD)),
        ];

        if let Some(target) = &props.target_path {
            header_spans.push(Span::raw(" "));
            header_spans.push(Span::styled(target, Style::default().fg(theme.system_fg)));
        }

        if let ToolPayload::Diff { old_text, new_text, .. } = &props.payload {
            let (added, removed) = line_diff_counts(old_text, new_text);
            header_spans.push(Span::raw(" ("));
            header_spans.push(Span::styled(format!("+{added}"), Style::default().fg(theme.system_fg)));
            header_spans.push(Span::raw(" "));
            header_spans.push(Span::styled(format!("-{removed}"), Style::default().fg(theme.error_fg)));
            header_spans.push(Span::raw(")"));
        }

        if let ToolStatus::Failed { exit_code: Some(code), .. } = &props.status {
            header_spans.push(Span::raw(" "));
            header_spans.push(Span::styled(format!("[exit {code}]"), Style::default().fg(theme.error_fg)));
        }

        let header_rect = Rect::new(area.x, area.y, area.width, 1);
        frame.render_widget(Paragraph::new(Line::from(header_spans)), header_rect);

        if !state.expanded || area.height <= 2 {
            return;
        }

        // 2. Render Expanded Body Area
        let body_area = Rect::new(
            area.x.saturating_add(2),
            area.y.saturating_add(1),
            area.width.saturating_sub(3),
            area.height.saturating_sub(1),
        );

        let body_block = Block::default()
            .borders(Borders::LEFT)
            .border_style(Style::default().fg(theme.dim_fg))
            .style(Style::default().bg(theme.app_bg));
        let inner_body = body_block.inner(body_area);
        frame.render_widget(body_block, body_area);

        match &props.payload {
            ToolPayload::Diff { old_text, new_text, line_offset } => {
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
                        DiffOp::Add => ("+", theme.system_fg, Color::Reset),
                        DiffOp::Remove => ("-", theme.error_fg, Color::Reset),
                        DiffOp::Context => (" ", theme.text_muted, Color::Reset),
                    };

                    let old_no_str = dl.old_no.map(|n| format!("{n:>4}")).unwrap_or_else(|| "    ".to_string());
                    let new_no_str = dl.new_no.map(|n| format!("{n:>4}")).unwrap_or_else(|| "    ".to_string());

                    let mut spans = vec![
                        Span::styled(format!("{old_no_str} {new_no_str} "), Style::default().fg(theme.dim_fg)),
                        Span::styled(format!("{op_char} "), Style::default().fg(op_color).add_modifier(Modifier::BOLD)),
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

                frame.render_widget(Paragraph::new(rendered_rows), inner_body);
            }
            ToolPayload::Command { output, .. } => {
                let lines: Vec<Line> = output
                    .lines()
                    .skip(state.scroll)
                    .take(inner_body.height as usize)
                    .map(|l| Line::from(Span::styled(l, Style::default().fg(theme.text_muted))))
                    .collect();
                frame.render_widget(Paragraph::new(lines), inner_body);
            }
            ToolPayload::Generic { content } => {
                let lines: Vec<Line> = content
                    .lines()
                    .skip(state.scroll)
                    .take(inner_body.height as usize)
                    .map(|l| Line::from(Span::styled(l, Style::default().fg(theme.text))))
                    .collect();
                frame.render_widget(Paragraph::new(lines), inner_body);
            }
        }
    }

    fn update(
        props: &Self::Props,
        state: &mut Self::State,
        msg: Self::Msg,
    ) -> Option<Self::Cmd> {
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
                    ToolPayload::Diff { old_text, new_text, line_offset } => {
                        line_diff(old_text, new_text, *line_offset).len()
                    }
                    ToolPayload::Command { output, .. } => output.lines().count(),
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
                    ToolPayload::Diff { old_text, new_text, line_offset } => {
                        line_diff(old_text, new_text, *line_offset).len()
                    }
                    ToolPayload::Command { output, .. } => output.lines().count(),
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
            mass_old.push_str(&format!("    let var_{i} = calculate_legacy_metric({i});\n"));
            mass_new.push_str(&format!("    let var_{i} = calculate_fast_simd_metric({i}, ACCEL_V2);\n"));
        }

        vec![
            Story::new(
                "Streaming In-Flight",
                "Tool call actively executing with spinner and partial target path",
                ToolCardProps {
                    tool_name: "edit_text".to_string(),
                    target_path: Some("crates/nuo-tui/src/render.rs".to_string()),
                    status: ToolStatus::Running { spinner_frame: 3 },
                    payload: ToolPayload::Generic {
                        content: "Waiting for stream chunks from cognitive bridge...".to_string(),
                    },
                },
                ToolCardState { expanded: true, scroll: 0 },
            ),
            Story::new(
                "Standard Edit Diff",
                "Inline code diff demonstrating added/removed line numbers and word-level modifications",
                ToolCardProps {
                    tool_name: "edit_text".to_string(),
                    target_path: Some("src/billing/calc.rs".to_string()),
                    status: ToolStatus::Success,
                    payload: ToolPayload::Diff {
                        old_text: old_code.to_string(),
                        new_text: new_code.to_string(),
                        line_offset: 42,
                    },
                },
                ToolCardState { expanded: true, scroll: 0 },
            ),
            Story::new(
                "Massive 100-Line Diff (Stress)",
                "Virtual scroll stress test over 100 modified lines across boundary columns",
                ToolCardProps {
                    tool_name: "edit_text".to_string(),
                    target_path: Some("crates/simd/src/matrix.rs".to_string()),
                    status: ToolStatus::Success,
                    payload: ToolPayload::Diff {
                        old_text: mass_old,
                        new_text: mass_new,
                        line_offset: 1,
                    },
                },
                ToolCardState { expanded: true, scroll: 0 },
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
                    },
                    payload: ToolPayload::Command {
                        command: "cargo test -p nuo-agent".to_string(),
                        output: "error[E0425]: cannot find value `foo` in this scope\n  --> crates/nuo-agent/src/rounds.rs:42:9\n   |\n42 |     foo.bar();\n   |     ^^^ not found in this scope".to_string(),
                    },
                },
                ToolCardState { expanded: true, scroll: 0 },
            ),
            Story::new(
                "Collapsed Summary",
                "Compact single-row summary view with +N -M count indicators",
                ToolCardProps {
                    tool_name: "edit_text".to_string(),
                    target_path: Some("crates/nuo-protocol/src/lib.rs".to_string()),
                    status: ToolStatus::Success,
                    payload: ToolPayload::Diff {
                        old_text: old_code.to_string(),
                        new_text: new_code.to_string(),
                        line_offset: 10,
                    },
                },
                ToolCardState { expanded: false, scroll: 0 },
            ),
        ]
    }

    fn map_key_to_msg(
        key: &KeyEvent,
        _props: &Self::Props,
        _state: &Self::State,
    ) -> Option<Self::Msg> {
        match key.code {
            KeyCode::Char(' ') | KeyCode::Enter => Some(ToolCardMsg::ToggleExpand),
            KeyCode::Up | KeyCode::Char('k') => Some(ToolCardMsg::ScrollUp(1)),
            KeyCode::Down | KeyCode::Char('j') => Some(ToolCardMsg::ScrollDown(1)),
            KeyCode::PageUp => Some(ToolCardMsg::ScrollUp(5)),
            KeyCode::PageDown => Some(ToolCardMsg::ScrollDown(5)),
            KeyCode::Home | KeyCode::Char('g') => Some(ToolCardMsg::ScrollToTop),
            KeyCode::End | KeyCode::Char('G') => Some(ToolCardMsg::ScrollToBottom),
            _ => None,
        }
    }
}
