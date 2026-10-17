//! Canonical Pilot Declarative Component: NoticeCard (ADR-0072 / ADR-0073).
//!
//! Demonstrates the pure TEA (Model-Msg-Update-View) paradigm with
//! zero dependencies on the monolithic `App` state.

use super::{
    Component,
    story::{Story, StorybookComponent},
};
use crate::theme::Theme;
use crossterm::event::{KeyCode, KeyEvent};
use nuotc::{
    Color, Frame, Line, Modifier, Paragraph, Rect, Span, Style,
    widgets::{Block, Borders},
};

/// Visual severity of the notice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Info,
    Warning,
    Critical,
}

/// Immutable input properties pushed from the parent.
#[derive(Debug, Clone)]
pub struct NoticeCardProps {
    pub title: String,
    pub message: String,
    pub severity: Severity,
    pub action_labels: Vec<String>,
}

/// Sovereign local volatile interaction state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoticeCardState {
    pub focused_button: usize,
    pub expanded: bool,
}

impl Default for NoticeCardState {
    fn default() -> Self {
        Self {
            focused_button: 0,
            expanded: false,
        }
    }
}

/// Discrete local event messages handled by this component.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoticeCardMsg {
    NextButton,
    PrevButton,
    ToggleExpand,
    ActivateCurrent,
}

/// Outward outcome emitted back to the host container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoticeCardCmd {
    ActionTriggered(usize),
}

/// The declarative NoticeCard component.
pub struct NoticeCard;

impl Component for NoticeCard {
    type Props = NoticeCardProps;
    type State = NoticeCardState;
    type Msg = NoticeCardMsg;
    type Cmd = NoticeCardCmd;

    fn render(
        props: &Self::Props,
        state: &Self::State,
        frame: &mut Frame<'_>,
        area: Rect,
        theme: &Theme,
    ) {
        if area.width < 10 || area.height < 4 {
            return;
        }

        let (border_color, severity_badge) = match props.severity {
            Severity::Info => (theme.system_fg, "[INFO]"),
            Severity::Warning => (theme.warning, "[WARN]"),
            Severity::Critical => (theme.error_fg, "[CRIT]"),
        };

        // Draw outer container block
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border_color))
            .style(Style::default().bg(theme.app_bg));
        frame.render_widget(block, area);

        // Title line
        let title_rect = Rect::new(
            area.x.saturating_add(2),
            area.y.saturating_add(1),
            area.width.saturating_sub(4),
            1,
        );
        let title_line = Line::from(vec![
            Span::styled(
                severity_badge,
                Style::default()
                    .fg(border_color)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
            Span::styled(
                &props.title,
                Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
            ),
        ]);
        frame.render_widget(Paragraph::new(title_line), title_rect);

        // Body message line
        let msg_rect = Rect::new(
            area.x.saturating_add(2),
            area.y.saturating_add(2),
            area.width.saturating_sub(4),
            1,
        );
        let msg_line = Line::from(Span::styled(
            &props.message,
            Style::default().fg(theme.text_muted),
        ));
        frame.render_widget(Paragraph::new(msg_line), msg_rect);

        // Action buttons row (anchored at the bottom)
        if !props.action_labels.is_empty() {
            let btn_y = area.y.saturating_add(area.height).saturating_sub(2);
            let mut cur_x = area.x.saturating_add(2);

            for (i, label) in props.action_labels.iter().enumerate() {
                let is_focused = state.focused_button == i;
                let btn_text = format!(" {label} ");
                let btn_width = btn_text.chars().count() as u16;
                let btn_rect = Rect::new(cur_x, btn_y, btn_width, 1);

                let btn_style = if is_focused {
                    Style::default()
                        .fg(theme.app_bg)
                        .bg(border_color)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme.text_muted).bg(Color::Reset)
                };

                frame.render_widget(
                    Paragraph::new(Line::from(Span::styled(btn_text, btn_style))),
                    btn_rect,
                );
                cur_x = cur_x.saturating_add(btn_width + 2);
            }
        }
    }

    fn update(props: &Self::Props, state: &mut Self::State, msg: Self::Msg) -> Option<Self::Cmd> {
        let count = props.action_labels.len();
        match msg {
            NoticeCardMsg::NextButton => {
                if count > 0 {
                    state.focused_button = (state.focused_button + 1) % count;
                }
                None
            }
            NoticeCardMsg::PrevButton => {
                if count > 0 {
                    state.focused_button = state.focused_button.checked_sub(1).unwrap_or(count - 1);
                }
                None
            }
            NoticeCardMsg::ToggleExpand => {
                state.expanded = !state.expanded;
                None
            }
            NoticeCardMsg::ActivateCurrent => {
                if count > 0 && state.focused_button < count {
                    Some(NoticeCardCmd::ActionTriggered(state.focused_button))
                } else {
                    None
                }
            }
        }
    }
}

impl StorybookComponent for NoticeCard {
    fn category() -> &'static str {
        "Components"
    }

    fn name() -> &'static str {
        "NoticeCard"
    }

    fn stories() -> Vec<Story<Self::Props, Self::State>> {
        vec![
            Story::new(
                "Standard Info",
                "Baseline informational banner with two actions",
                NoticeCardProps {
                    title: "Workspace Indexing Completed".to_string(),
                    message: "All 1,420 AST nodes have been successfully indexed.".to_string(),
                    severity: Severity::Info,
                    action_labels: vec!["View Details".to_string(), "Dismiss".to_string()],
                },
                NoticeCardState::default(),
            ),
            Story::new(
                "Dangerous Warning (Shell Operation)",
                "High-contrast security warning for out-of-boundary file changes",
                NoticeCardProps {
                    title: "Dangerous Sandbox Escape Attempted".to_string(),
                    message: "The agent attempted to execute `rm -rf /` outside workspace root.".to_string(),
                    severity: Severity::Critical,
                    action_labels: vec!["Deny".to_string(), "Terminate Session".to_string()],
                },
                NoticeCardState {
                    focused_button: 0,
                    expanded: false,
                },
            ),
            Story::new(
                "Long Text Truncation",
                "Verifies horizontal bound safety with an un-wrapped continuous token stream",
                NoticeCardProps {
                    title: "System Path Resolution Failure".to_string(),
                    message: "/very/long/deeply/nested/directory/structure/that/exceeds/standard/viewport/bounds/and/should/truncate/cleanly.log".to_string(),
                    severity: Severity::Warning,
                    action_labels: vec!["Retry".to_string()],
                },
                NoticeCardState::default(),
            ),
        ]
    }

    fn map_key_to_msg(
        key: &KeyEvent,
        _props: &Self::Props,
        _state: &Self::State,
    ) -> Option<Self::Msg> {
        match key.code {
            KeyCode::Right | KeyCode::Tab => Some(NoticeCardMsg::NextButton),
            KeyCode::Left | KeyCode::BackTab => Some(NoticeCardMsg::PrevButton),
            KeyCode::Enter => Some(NoticeCardMsg::ActivateCurrent),
            KeyCode::Char(' ') => Some(NoticeCardMsg::ToggleExpand),
            _ => None,
        }
    }
}
