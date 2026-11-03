//! Pure Declarative Toast Component (ADR-0072 / ADR-0073 / ADR-0116 / ADR-0146).
//!
//! Provides isolated visual inspection and interactive testing for layer 4 ephemeral
//! overlay toast bubbles across modern borderless chromatic and hybrid styling.

use super::{
    Component,
    story::{Story, StoryEventOutcome, StorybookComponent},
};
use crate::shell::declarative::key::Key;
use nuo_ui_primitives::components::toast::{ToastKind, draw_toast_bubble};
use nuo_ui_primitives::theme::Theme;
use nuotc::{Frame, Rect};

/// Presentation properties for a declarative toast bubble.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToastCardProps {
    pub message: String,
    pub kind: ToastKind,
    pub hint: Option<String>,
    pub max_width: Option<u16>,
}

/// Volatile interactive state for toast inspection.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ToastCardState {
    pub active_kind: Option<ToastKind>,
    pub elapsed_ticks: usize,
}

/// Interactive messages handled by toast storybook inspection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastCardMsg {
    CycleKind,
    Reset,
}

pub struct ToastCard;

impl Component for ToastCard {
    type Props = ToastCardProps;
    type State = ToastCardState;
    type Msg = ToastCardMsg;
    type Cmd = ();

    fn render(
        props: &Self::Props,
        state: &Self::State,
        frame: &mut Frame<'_>,
        area: Rect,
        theme: &Theme,
    ) {
        if area.width < 10 || area.height < 3 {
            return;
        }

        let kind = state.active_kind.unwrap_or(props.kind);
        let render_w = props.max_width.unwrap_or(area.width);

        draw_toast_bubble(
            frame,
            theme,
            &props.message,
            kind,
            props.hint.as_deref(),
            render_w,
        );
    }

    fn update(props: &Self::Props, state: &mut Self::State, msg: Self::Msg) -> Option<Self::Cmd> {
        match msg {
            ToastCardMsg::CycleKind => {
                let current = state.active_kind.unwrap_or(props.kind);
                let next = match current {
                    ToastKind::CopyOk => ToastKind::Info,
                    ToastKind::Info => ToastKind::Warning,
                    ToastKind::Warning => ToastKind::Armed,
                    ToastKind::Armed => ToastKind::Error,
                    ToastKind::Error => ToastKind::CopyFailed,
                    ToastKind::CopyFailed => ToastKind::CopyOk,
                };
                state.active_kind = Some(next);
            }
            ToastCardMsg::Reset => {
                state.active_kind = None;
                state.elapsed_ticks = 0;
            }
        }
        None
    }
}

impl StorybookComponent for ToastCard {
    fn category() -> &'static str {
        "Overlays"
    }

    fn name() -> &'static str {
        "Toast"
    }

    fn stories() -> Vec<Story<Self::Props, Self::State>> {
        vec![
            Story::new(
                "Clipboard Success (CopyOk)",
                "Transient acknowledgement toast for clipboard operations with green glyph",
                ToastCardProps {
                    message: "Prompt copied to system clipboard".to_string(),
                    kind: ToastKind::CopyOk,
                    hint: None,
                    max_width: None,
                },
                ToastCardState::default(),
            ),
            Story::new(
                "Dangerous Interruption (Armed/Warning)",
                "Two-step confirmation warning overlay with countdown hint",
                ToastCardProps {
                    message: "Press Esc again to interrupt running tool execution".to_string(),
                    kind: ToastKind::Armed,
                    hint: Some("2.4s remaining".to_string()),
                    max_width: None,
                },
                ToastCardState::default(),
            ),
            Story::new(
                "Command Rejection (Error)",
                "High-contrast failure badge when host execution is refused by policy",
                ToastCardProps {
                    message: "Execution blocked: command 'rm -rf /' denied by sandbox policy".to_string(),
                    kind: ToastKind::Error,
                    hint: Some("Code 126".to_string()),
                    max_width: None,
                },
                ToastCardState::default(),
            ),
            Story::new(
                "Slash Command Acknowledgment (Info)",
                "Subtle info pill confirming slash-command routing state",
                ToastCardProps {
                    message: "Delegation mode enabled: routing subtasks to worker pool".to_string(),
                    kind: ToastKind::Info,
                    hint: None,
                    max_width: None,
                },
                ToastCardState::default(),
            ),
            Story::new(
                "Multi-line Text Wrapping (Overflow)",
                "Tests horizontal bounds clamping and multi-row indentation gutter alignment",
                ToastCardProps {
                    message: "A long descriptive toast message that spans across multiple rows to test horizontal wrap margins, shrink-to-fit calculation, and proper vertical layout alignment".to_string(),
                    kind: ToastKind::Warning,
                    hint: Some("Notice".to_string()),
                    max_width: Some(48),
                },
                ToastCardState::default(),
            ),
        ]
    }

    fn map_key_to_msg(key: &Key, _props: &Self::Props, _state: &Self::State) -> Option<Self::Msg> {
        match key {
            Key::Char(' ') | Key::Enter => Some(ToastCardMsg::CycleKind),
            Key::Char('r') | Key::Char('R') => Some(ToastCardMsg::Reset),
            _ => None,
        }
    }

    fn tick(_props: &mut Self::Props, state: &mut Self::State) -> StoryEventOutcome {
        state.elapsed_ticks = state.elapsed_ticks.saturating_add(1);
        StoryEventOutcome::Consumed
    }
}
