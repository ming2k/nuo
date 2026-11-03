//! Pure Declarative WhichKey Card Component (ADR-0023 / ADR-0072 / ADR-0073 / ADR-0146).
//!
//! Provides isolated inspection for the floating bottom-right which-key guide card,
//! validating chromatic borderless pill layout and chord readability.

use super::{
    Component,
    story::{Story, StorybookComponent},
};
use crate::shell::declarative::key::Key;
use nuo_ui_primitives::components::which_key::{WhichKeyItem, draw_which_key_overlay};
use nuo_ui_primitives::theme::Theme;
use nuotc::{Frame, Rect};

/// Presentation properties for the which-key floating guide card.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WhichKeyCardProps {
    pub armed: bool,
    pub items: Vec<WhichKeyItem>,
}

/// Volatile interactive state for which-key card inspection.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WhichKeyCardState {
    pub armed_override: Option<bool>,
    pub active_index: usize,
}

/// Interactive messages handled by which-key story inspection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WhichKeyCardMsg {
    ToggleArmed,
    NextItem,
    PrevItem,
}

pub struct WhichKeyCard;

impl Component for WhichKeyCard {
    type Props = WhichKeyCardProps;
    type State = WhichKeyCardState;
    type Msg = WhichKeyCardMsg;
    type Cmd = ();

    fn render(
        props: &Self::Props,
        state: &Self::State,
        frame: &mut Frame<'_>,
        area: Rect,
        theme: &Theme,
    ) {
        let is_armed = state.armed_override.unwrap_or(props.armed);
        let mut items = props.items.clone();
        if !items.is_empty() {
            for (idx, item) in items.iter_mut().enumerate() {
                item.active = idx == state.active_index;
            }
        }
        draw_which_key_overlay(frame, theme, is_armed, &items, area);
    }

    fn update(props: &Self::Props, state: &mut Self::State, msg: Self::Msg) -> Option<Self::Cmd> {
        match msg {
            WhichKeyCardMsg::ToggleArmed => {
                let current = state.armed_override.unwrap_or(props.armed);
                state.armed_override = Some(!current);
            }
            WhichKeyCardMsg::NextItem => {
                if !props.items.is_empty() {
                    state.active_index = (state.active_index + 1) % props.items.len();
                }
            }
            WhichKeyCardMsg::PrevItem => {
                if !props.items.is_empty() {
                    state.active_index = state
                        .active_index
                        .checked_sub(1)
                        .unwrap_or(props.items.len().saturating_sub(1));
                }
            }
        }
        None
    }
}

impl StorybookComponent for WhichKeyCard {
    fn category() -> &'static str {
        "Navigation"
    }

    fn name() -> &'static str {
        "WhichKey"
    }

    fn stories() -> Vec<Story<Self::Props, Self::State>> {
        vec![
            Story::new(
                "Ctrl-X Armed Scene Namespace",
                "Floating key guide card when C-x leader is armed with tab and overlay actions",
                WhichKeyCardProps {
                    armed: true,
                    items: vec![
                        WhichKeyItem::new("x k", "close active tab", true),
                        WhichKeyItem::new("x 2", "split terminal vertically", false),
                        WhichKeyItem::new("x o", "switch to next window", false),
                        WhichKeyItem::new("x u", "undo layout change", false),
                    ],
                },
                WhichKeyCardState::default(),
            ),
            Story::new(
                "Dialog Leaving Active Context",
                "Dynamic guide row adapts to close overlay when a foreground dialog is open",
                WhichKeyCardProps {
                    armed: true,
                    items: vec![
                        WhichKeyItem::new("Esc", "close overlay", true),
                        WhichKeyItem::new("Enter", "confirm selection", false),
                        WhichKeyItem::new("Tab", "next form field", false),
                    ],
                },
                WhichKeyCardState::default(),
            ),
            Story::new(
                "High Density Multi-Action Guide",
                "Tests multi-row alignment and chord padding under 6 concurrent shortcuts",
                WhichKeyCardProps {
                    armed: true,
                    items: vec![
                        WhichKeyItem::new("g g", "jump to top of transcript", false),
                        WhichKeyItem::new("G", "jump to bottom", false),
                        WhichKeyItem::new("Ctrl+f", "search text forward", true),
                        WhichKeyItem::new("Ctrl+b", "search text backward", false),
                        WhichKeyItem::new("y", "yank current line", false),
                        WhichKeyItem::new("p", "paste from clipboard", false),
                    ],
                },
                WhichKeyCardState::default(),
            ),
        ]
    }

    fn map_key_to_msg(key: &Key, _props: &Self::Props, _state: &Self::State) -> Option<Self::Msg> {
        match key {
            Key::Char(' ') | Key::Enter => Some(WhichKeyCardMsg::ToggleArmed),
            Key::Down | Key::Char('j') => Some(WhichKeyCardMsg::NextItem),
            Key::Up | Key::Char('k') => Some(WhichKeyCardMsg::PrevItem),
            _ => None,
        }
    }
}
