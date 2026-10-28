//! Canonical Pilot Declarative Component: NoticeCard (ADR-0072 / ADR-0073 / ADR-0110).
//!
//! Demonstrates the pure TEA (Model-Msg-Update-View) paradigm with
//! zero dependencies on the monolithic `App` state.
//!
//! Enforces:
//! - `[INV-TUI-RUNNING-01]` (Strict Lifecycle-Tone Separation): In-flight actions
//!   use active info hue (`theme.info()`), bold, never `theme.dim()`.
//! - `[INV-TUI-RUNNING-02]` (Action Permissibility Tri-Partition): Re-entrant activation
//!   of running and disabled actions is strictly guarded (no-op).

use super::{
    Component,
    story::{Story, StorybookComponent},
};
use crate::shell::declarative::key::Key;
use nuo_ui_primitives::{ActionStatus, theme::Theme};
use nuo_view::model::document::{MessageKind, NoticeSeverity, TranscriptMessage};
use nuo_view::model::layout::{BlockRegion, LayoutMap, NOTICE_BLOCK_IDX};
use nuotc::{
    Frame, Line, Modifier, Paragraph, Rect, Span, Style,
    widgets::{Block, Borders},
};

/// Visual severity of the notice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Info,
    Warning,
    Critical,
}

/// An individual action control inside NoticeCard (ADR-0110).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoticeAction {
    pub label: String,
    pub running_label: Option<String>,
    pub status: ActionStatus,
}

impl NoticeAction {
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            running_label: None,
            status: ActionStatus::Idle,
        }
    }

    pub fn running(
        label: impl Into<String>,
        running_label: impl Into<String>,
        spinner_frame: usize,
        elapsed_secs: Option<u64>,
    ) -> Self {
        Self {
            label: label.into(),
            running_label: Some(running_label.into()),
            status: ActionStatus::running(spinner_frame, elapsed_secs),
        }
    }

    pub fn disabled(label: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            running_label: None,
            status: ActionStatus::disabled(reason),
        }
    }
}

impl From<String> for NoticeAction {
    fn from(s: String) -> Self {
        Self::new(s)
    }
}

impl From<&str> for NoticeAction {
    fn from(s: &str) -> Self {
        Self::new(s)
    }
}

/// Immutable input properties pushed from the parent.
#[derive(Debug, Clone)]
pub struct NoticeCardProps {
    pub title: String,
    pub message: String,
    pub severity: Severity,
    pub actions: Vec<NoticeAction>,
}

impl NoticeCardProps {
    pub fn new(
        title: impl Into<String>,
        message: impl Into<String>,
        severity: Severity,
        actions: Vec<NoticeAction>,
    ) -> Self {
        Self {
            title: title.into(),
            message: message.into(),
            severity,
            actions,
        }
    }

    /// Canonical domain extraction bridge: projects a transcript notice into
    /// immutable [`NoticeCardProps`] (ADR-0112 [INV-TUI-TIER-02]).
    pub fn from_transcript_message(msg: &TranscriptMessage) -> Option<Self> {
        let (severity, content) = match &msg.kind {
            MessageKind::Notice { severity, .. } => {
                let s = match severity {
                    NoticeSeverity::Info => Severity::Info,
                    NoticeSeverity::Warning => Severity::Warning,
                    NoticeSeverity::Error => Severity::Critical,
                };
                (s, msg.raw.clone())
            }
            MessageKind::ProviderRetry { failure, .. } => (Severity::Warning, failure.clone()),
            _ => return None,
        };

        // The body is normalized by its single owner in the model tier
        // (`nuo_view::model::notice_text`); this bridge only splits the stored
        // text into the card's title/body slots. Re-parsing here would restore
        // the second projection that owner exists to prevent.
        let parsed = nuo_view::model::notice_text::normalize(&content);
        Some(Self {
            title: parsed.title,
            message: parsed.detail.unwrap_or(content),
            severity,
            actions: Vec::new(),
        })
    }
}

/// Sovereign local volatile interaction state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NoticeCardState {
    pub focused_button: usize,
    pub expanded: bool,
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

impl NoticeCard {
    /// Connected render bridge: paints NoticeCard into frame with optional layout registration
    /// (ADR-0112 [INV-TUI-TIER-03]).
    pub fn render_with_layout(
        props: &NoticeCardProps,
        state: &NoticeCardState,
        frame: &mut Frame<'_>,
        area: Rect,
        theme: &Theme,
        mut layout_map: Option<&mut LayoutMap>,
        message_idx: Option<usize>,
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
        let mut block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border_color))
            .style(Style::default().bg(theme.app_bg));
        frame.render_widget(&mut block, area);

        if let (Some(lm), Some(mi)) = (&mut layout_map, message_idx) {
            lm.push(BlockRegion {
                message_idx: mi,
                block_idx: NOTICE_BLOCK_IDX,
                start_byte: 0,
                end_byte: 0,
                text: props.title.clone(),
                prefix_cols: 0,
                rect: area,
                hidden_ranges: Vec::new(),
            });
        }

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
        frame.render_widget(&mut Paragraph::new(title_line), title_rect);

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
        frame.render_widget(&mut Paragraph::new(msg_line), msg_rect);

        // Action buttons row (anchored at the bottom)
        if !props.actions.is_empty() {
            let btn_y = area.y.saturating_add(area.height).saturating_sub(2);
            let mut cur_x = area.x.saturating_add(2);

            for (i, action) in props.actions.iter().enumerate() {
                let is_focused = state.focused_button == i;
                let btn_text = action
                    .status
                    .format_label(&action.label, action.running_label.as_deref());
                let btn_width = btn_text.chars().count() as u16;
                let btn_rect = Rect::new(cur_x, btn_y, btn_width, 1);

                let btn_style = action.status.style(theme, is_focused, border_color);

                frame.render_widget(
                    &mut Paragraph::new(Line::from(Span::styled(btn_text, btn_style))),
                    btn_rect,
                );
                cur_x = cur_x.saturating_add(btn_width + 2);
            }
        }
    }
}

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
        Self::render_with_layout(props, state, frame, area, theme, None, None);
    }

    fn update(props: &Self::Props, state: &mut Self::State, msg: Self::Msg) -> Option<Self::Cmd> {
        let count = props.actions.len();
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
                    let action = &props.actions[state.focused_button];
                    // Invariant [INV-TUI-RUNNING-02]: Guard against re-entrant activation of Running and Disabled actions
                    if action.status.is_actionable() {
                        Some(NoticeCardCmd::ActionTriggered(state.focused_button))
                    } else {
                        None
                    }
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
                    actions: vec![
                        NoticeAction::new("View Details"),
                        NoticeAction::new("Dismiss"),
                    ],
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
                    actions: vec![
                        NoticeAction::new("Deny"),
                        NoticeAction::new("Terminate Session"),
                    ],
                },
                NoticeCardState {
                    focused_button: 0,
                    expanded: false,
                },
            ),
            Story::new(
                "In-Flight Running Action",
                "Demonstrates in-flight asynchronous execution with active info hue and non-reentrant guard (ADR-0110)",
                NoticeCardProps {
                    title: "Workspace Database Migration".to_string(),
                    message: "Applying schema upgrade across 12 partitioned sqlite databases...".to_string(),
                    severity: Severity::Info,
                    actions: vec![
                        NoticeAction::running("Cancel", "Cancelling", 2, Some(4)),
                        NoticeAction::disabled("Apply Now", "Operation in-flight"),
                    ],
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
                    actions: vec![NoticeAction::new("Retry")],
                },
                NoticeCardState::default(),
            ),
        ]
    }

    fn map_key_to_msg(key: &Key, _props: &Self::Props, _state: &Self::State) -> Option<Self::Msg> {
        match key {
            Key::Right | Key::Tab => Some(NoticeCardMsg::NextButton),
            Key::Left | Key::BackTab => Some(NoticeCardMsg::PrevButton),
            Key::Enter => Some(NoticeCardMsg::ActivateCurrent),
            Key::Char(' ') => Some(NoticeCardMsg::ToggleExpand),
            _ => None,
        }
    }
}
