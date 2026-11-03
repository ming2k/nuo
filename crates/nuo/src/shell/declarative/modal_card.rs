//! Pure Declarative Modal Dialog Component (ADR-0072 / ADR-0073 / ADR-0146).
//!
//! Provides isolated inspection for centered modal pages and dialog overlays,
//! validating responsive content sizing, title chrome, scrolling body, and footer hint rendering.

use super::{
    Component,
    story::{Story, StorybookComponent},
};
use crate::shell::declarative::key::Key;
use nuo_ui_primitives::components::footer::FooterHint;
use nuo_ui_primitives::components::modal::{
    ModalHeader, ModalPage, ModalPageSize, draw_modal_page,
};
use nuo_ui_primitives::components::scroll::ScrollBody;
use nuo_ui_primitives::primitives::ContentModalSpec;
use nuo_ui_primitives::theme::Theme;
use nuotc::{Frame, Line, Rect};

/// Presentation properties for a centered modal page.
#[derive(Clone)]
pub struct ModalCardProps {
    pub title: String,
    pub body_lines: Vec<String>,
    pub hints: Vec<(&'static str, &'static str)>,
    pub spec: ContentModalSpec,
}

impl std::fmt::Debug for ModalCardProps {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModalCardProps")
            .field("title", &self.title)
            .field("body_lines", &self.body_lines)
            .field("hints", &self.hints)
            .finish()
    }
}

/// Volatile interactive state for modal inspection (scroll position).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ModalCardState {
    pub scroll: usize,
}

/// Interactive messages handled by modal story inspection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModalCardMsg {
    ScrollUp(usize),
    ScrollDown(usize),
    ResetScroll,
}

pub struct ModalCard;

impl Component for ModalCard {
    type Props = ModalCardProps;
    type State = ModalCardState;
    type Msg = ModalCardMsg;
    type Cmd = ();

    fn render(
        props: &Self::Props,
        state: &Self::State,
        frame: &mut Frame<'_>,
        area: Rect,
        theme: &Theme,
    ) {
        if area.width < 20 || area.height < 8 {
            return;
        }

        let mut scroll = state.scroll;
        let rendered_lines: Vec<Line<'static>> = props
            .body_lines
            .iter()
            .map(|s| Line::raw(s.clone()))
            .collect();

        let footer_hints: Vec<FooterHint> = props
            .hints
            .iter()
            .map(|(k, l)| FooterHint::primary(k, l))
            .collect();

        let page = ModalPage {
            size: ModalPageSize::Content(props.spec),
            header: ModalHeader::title(&props.title),
            body: ScrollBody {
                lines: rendered_lines,
                scroll: &mut scroll,
                follow: None,
                edge_margin: 0,
                wrap: true,
            },
            footer_hints: &footer_hints,
            extra_footer_hints: &[],
        };

        draw_modal_page(frame, page, theme);
    }

    fn update(props: &Self::Props, state: &mut Self::State, msg: Self::Msg) -> Option<Self::Cmd> {
        let max_scroll = props.body_lines.len().saturating_sub(4);
        match msg {
            ModalCardMsg::ScrollUp(delta) => {
                state.scroll = state.scroll.saturating_sub(delta);
            }
            ModalCardMsg::ScrollDown(delta) => {
                state.scroll = (state.scroll + delta).min(max_scroll);
            }
            ModalCardMsg::ResetScroll => {
                state.scroll = 0;
            }
        }
        None
    }
}

impl StorybookComponent for ModalCard {
    fn category() -> &'static str {
        "Overlays"
    }

    fn name() -> &'static str {
        "ModalDialog"
    }

    fn stories() -> Vec<Story<Self::Props, Self::State>> {
        vec![
            Story::new(
                "Confirmation Dialog",
                "Standard centered modal prompt requesting confirmation for action execution",
                ModalCardProps {
                    title: "Apply File Modifications".to_string(),
                    body_lines: vec![
                        "The agent proposed 3 file modifications:".to_string(),
                        "  - crates/nuo/src/main.rs (+12 -2)".to_string(),
                        "  - crates/nuo-protocol/src/lib.rs (+4 -0)".to_string(),
                        "".to_string(),
                        "Proceeding will write changes directly to disk.".to_string(),
                    ],
                    hints: vec![("Enter", "Confirm"), ("Esc", "Cancel")],
                    spec: ContentModalSpec::TOOLS,
                },
                ModalCardState::default(),
            ),
            Story::new(
                "Dangerous Sandbox Escape Warning",
                "High-contrast security warning modal with explicit choice buttons",
                ModalCardProps {
                    title: "Out-of-Boundary Command Execution".to_string(),
                    body_lines: vec![
                        "WARNING: High Risk Command Detected".to_string(),
                        "".to_string(),
                        "Command: `rm -rf /tmp/target_cache`".to_string(),
                        "Reason: Agent requested destructive directory purge outside repository root.".to_string(),
                        "".to_string(),
                        "Authorizing this command permits unconstrained host system mutation.".to_string(),
                    ],
                    hints: vec![("1", "Deny"), ("2", "Allow Once"), ("Esc", "Cancel")],
                    spec: ContentModalSpec::TOOLS,
                },
                ModalCardState::default(),
            ),
            Story::new(
                "Help Keybindings Reference",
                "Tall scrollable modal demonstrating body scroll offsets and footer hints",
                ModalCardProps {
                    title: "Global Keyboard Shortcuts".to_string(),
                    body_lines: vec![
                        "Navigation:".to_string(),
                        "  Tab / Shift+Tab   Toggle focus between Composer and Transcript".to_string(),
                        "  Ctrl+x            Arm scene command namespace".to_string(),
                        "  Ctrl+t            Open thread browser".to_string(),
                        "".to_string(),
                        "Composer Editing:".to_string(),
                        "  Enter             Submit prompt".to_string(),
                        "  Ctrl+j            Insert literal newline".to_string(),
                        "  Ctrl+c            Clear input / dismiss selection".to_string(),
                        "".to_string(),
                        "Transcript Inspection:".to_string(),
                        "  ↑ / ↓             Select prior / next message or tool step".to_string(),
                        "  Space             Expand / collapse tool step diff".to_string(),
                        "  y                 Copy selected step output to clipboard".to_string(),
                        "  /                 Search transcript content".to_string(),
                    ],
                    hints: vec![("↑/↓", "Scroll"), ("Esc", "Close")],
                    spec: ContentModalSpec::SESSION_TRACE,
                },
                ModalCardState::default(),
            ),
        ]
    }

    fn map_key_to_msg(key: &Key, _props: &Self::Props, _state: &Self::State) -> Option<Self::Msg> {
        match key {
            Key::Up | Key::Char('k') => Some(ModalCardMsg::ScrollUp(1)),
            Key::Down | Key::Char('j') => Some(ModalCardMsg::ScrollDown(1)),
            Key::PageUp => Some(ModalCardMsg::ScrollUp(5)),
            Key::PageDown => Some(ModalCardMsg::ScrollDown(5)),
            Key::Home | Key::Char('g') => Some(ModalCardMsg::ResetScroll),
            _ => None,
        }
    }
}
