//! Pure Declarative EmptyStateHero Component (ADR-0072 / ADR-0073 / ADR-0146).
//!
//! Provides isolated inspection for the welcome empty-state hero, validating
//! ASCII figlet branding, responsive vertical centering, and rotating capability hints.

use super::{
    Component,
    story::{Story, StoryEventOutcome, StorybookComponent},
};
use crate::shell::declarative::key::Key;
use nuo_ui_primitives::theme::Theme;
use nuo_view::empty_state::{EmptyStateGuidance, draw_empty_state};
use nuotc::{Frame, Rect};

/// Presentation properties for the transcript empty state hero.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmptyHeroCardProps {
    pub guidance: EmptyStateGuidance,
    pub user_logo: Option<Vec<String>>,
}

/// Volatile interactive state for empty hero inspection.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EmptyHeroCardState {
    pub carousel_index: usize,
    pub tick_accumulator: usize,
}

/// Interactive messages handled by empty hero inspection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmptyHeroCardMsg {
    NextCarousel,
    PrevCarousel,
}

pub struct EmptyHeroCard;

impl Component for EmptyHeroCard {
    type Props = EmptyHeroCardProps;
    type State = EmptyHeroCardState;
    type Msg = EmptyHeroCardMsg;
    type Cmd = ();

    fn render(
        props: &Self::Props,
        state: &Self::State,
        frame: &mut Frame<'_>,
        area: Rect,
        theme: &Theme,
    ) {
        if area.width < 10 || area.height < 5 {
            return;
        }

        draw_empty_state(
            frame,
            area,
            props.user_logo.as_deref(),
            props.guidance.clone(),
            state.carousel_index,
            theme,
        );
    }

    fn update(_props: &Self::Props, state: &mut Self::State, msg: Self::Msg) -> Option<Self::Cmd> {
        match msg {
            EmptyHeroCardMsg::NextCarousel => {
                state.carousel_index = (state.carousel_index + 1) % 6;
            }
            EmptyHeroCardMsg::PrevCarousel => {
                state.carousel_index = state.carousel_index.checked_sub(1).unwrap_or(5);
            }
        }
        None
    }
}

impl StorybookComponent for EmptyHeroCard {
    fn category() -> &'static str {
        "Transcript"
    }

    fn name() -> &'static str {
        "EmptyStateHero"
    }

    fn stories() -> Vec<Story<Self::Props, Self::State>> {
        vec![
            Story::new(
                "Standard Figlet Brand Hero",
                "Default brand wordmark with rotating capability tour carousel",
                EmptyHeroCardProps {
                    guidance: EmptyStateGuidance::Tour,
                    user_logo: None,
                },
                EmptyHeroCardState::default(),
            ),
            Story::new(
                "Missing Provider Blocker",
                "Pinned onboarding setup blocker steering user to configure credentials",
                EmptyHeroCardProps {
                    guidance: EmptyStateGuidance::NeedsProvider,
                    user_logo: None,
                },
                EmptyHeroCardState::default(),
            ),
            Story::new(
                "Loading Session State",
                "Restoring active transcript state during asynchronous hydration",
                EmptyHeroCardProps {
                    guidance: EmptyStateGuidance::LoadingSession("session_arch_review".to_string()),
                    user_logo: None,
                },
                EmptyHeroCardState::default(),
            ),
        ]
    }

    fn map_key_to_msg(key: &Key, _props: &Self::Props, _state: &Self::State) -> Option<Self::Msg> {
        match key {
            Key::Char(' ') | Key::Enter | Key::Right => Some(EmptyHeroCardMsg::NextCarousel),
            Key::Left => Some(EmptyHeroCardMsg::PrevCarousel),
            _ => None,
        }
    }

    fn tick(_props: &mut Self::Props, state: &mut Self::State) -> StoryEventOutcome {
        state.tick_accumulator = state.tick_accumulator.saturating_add(1);
        // Rotate capability carousel every ~14 ticks
        if state.tick_accumulator % 14 == 0 {
            state.carousel_index = (state.carousel_index + 1) % 6;
            StoryEventOutcome::Consumed
        } else {
            StoryEventOutcome::Ignored
        }
    }
}
