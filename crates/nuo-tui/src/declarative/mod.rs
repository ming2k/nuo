//! Pure Declarative Component Paradigm (ADR-0072) and Storybook Protocol (ADR-0073).
//!
//! This module defines the architectural foundation for pure, isolated,
//! state-sovereign UI components in `nuo-tui`.

use crate::theme::Theme;
use nuotc::{Frame, Rect};

pub mod ask_user;
pub mod notice_card;
pub mod story;
pub mod testing;
pub mod tool_card;
pub mod workbench;

/// Canonical declarative component contract (ADR-0072).
///
/// Every component is a pure projection:
/// - Presentation is stateless with respect to the component instance.
/// - Local interaction state is owned entirely by `State`.
/// - Transitions occur strictly via `update(msg)`.
pub trait Component {
    /// Pure, immutable presentation data provided by the parent.
    type Props: Clone + Send + 'static;
    /// Self-contained local volatile state (selection index, scroll, cursor).
    type State: Default + Clone + Send + 'static;
    /// Discrete local event messages handled by this component.
    type Msg: Send + 'static;
    /// Outward commands or intent emitted back to the host/parent.
    type Cmd: Send + 'static;

    /// Pure rendering projection into the terminal frame.
    ///
    /// Invariant `[INV-TUI-DECL-01]`: Render functions must never borrow
    /// mutable application state or read external side effects.
    fn render(
        props: &Self::Props,
        state: &Self::State,
        frame: &mut Frame<'_>,
        area: Rect,
        theme: &Theme,
    );

    /// Sovereign, deterministic state transition.
    ///
    /// Invariant `[INV-TUI-DECL-02]`: State transitions must occur exclusively
    /// within this function in response to a typed `Msg`.
    fn update(props: &Self::Props, state: &mut Self::State, msg: Self::Msg) -> Option<Self::Cmd>;
}

#[cfg(test)]
mod tests {
    use super::notice_card::{
        NoticeCard, NoticeCardMsg, NoticeCardProps, NoticeCardState, Severity,
    };
    use super::story::{StorybookComponent, instantiate_story};
    use super::testing::render_story_styled_snapshot;
    use super::*;

    #[test]
    fn test_inv_tui_decl_02_sovereign_state_transition() {
        let props = NoticeCardProps {
            title: "Test Notice".to_string(),
            message: "Body content".to_string(),
            severity: Severity::Info,
            action_labels: vec!["Btn 0".to_string(), "Btn 1".to_string()],
        };
        let mut state = NoticeCardState::default();
        assert_eq!(state.focused_button, 0);

        // Transition 1: Next button
        let cmd = NoticeCard::update(&props, &mut state, NoticeCardMsg::NextButton);
        assert_eq!(cmd, None);
        assert_eq!(state.focused_button, 1);

        // Transition 2: Wrap around
        let cmd = NoticeCard::update(&props, &mut state, NoticeCardMsg::NextButton);
        assert_eq!(cmd, None);
        assert_eq!(state.focused_button, 0);

        // Transition 3: Activate
        let cmd = NoticeCard::update(&props, &mut state, NoticeCardMsg::ActivateCurrent);
        assert_eq!(cmd, Some(notice_card::NoticeCardCmd::ActionTriggered(0)));
    }

    #[test]
    fn test_inv_tui_story_01_hermetic_offline_styled_snapshot() {
        let stories = NoticeCard::stories();
        assert!(
            !stories.is_empty(),
            "NoticeCard must declare canonical stories"
        );

        for story in stories {
            let story_instance = instantiate_story::<NoticeCard>(story);
            let snapshot = render_story_styled_snapshot(story_instance.as_ref(), 80, 8);
            assert!(!snapshot.is_empty());
            assert!(
                snapshot.contains("styles"),
                "Snapshot must preserve style/color layer"
            );
        }
    }

    #[test]
    fn test_inv_tui_decl_ask_user_pure_state_and_snapshot() {
        use super::ask_user::AskUser;

        let stories = AskUser::stories();
        assert_eq!(
            stories.len(),
            4,
            "AskUser must declare all 4 canonical stories"
        );

        for story in stories {
            let mut story_instance = instantiate_story::<AskUser>(story);
            let snapshot = render_story_styled_snapshot(story_instance.as_ref(), 100, 20);
            assert!(!snapshot.is_empty());

            let key = crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Down,
                crossterm::event::KeyModifiers::NONE,
            );
            let outcome = story_instance.handle_key(&key);
            assert_eq!(outcome, story::StoryEventOutcome::Consumed);
        }
    }

    #[test]
    fn test_inv_tui_decl_tool_card_pure_state_and_snapshot() {
        use super::tool_card::ToolCard;

        let stories = ToolCard::stories();
        assert_eq!(
            stories.len(),
            5,
            "ToolCard must declare all 5 canonical stories"
        );

        for story in stories {
            let mut story_instance = instantiate_story::<ToolCard>(story);
            let snapshot = render_story_styled_snapshot(story_instance.as_ref(), 100, 20);
            assert!(!snapshot.is_empty());

            let key = crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char(' '),
                crossterm::event::KeyModifiers::NONE,
            );
            let outcome = story_instance.handle_key(&key);
            assert_eq!(outcome, story::StoryEventOutcome::Consumed);
        }
    }

    #[test]
    fn test_inv_tui_story_02_exhaustive_default_catalog() {
        use super::workbench::default_catalog;

        let catalog = default_catalog();
        assert_eq!(
            catalog.len(),
            12,
            "Default catalog must aggregate all 12 canonical stories"
        );

        for story in &catalog {
            assert!(!story.category().is_empty());
            assert!(!story.component_name().is_empty());
            assert!(!story.story_name().is_empty());

            let snap_80 = render_story_styled_snapshot(story.as_ref(), 80, 24);
            assert!(!snap_80.is_empty());
            let snap_120 = render_story_styled_snapshot(story.as_ref(), 120, 30);
            assert!(!snap_120.is_empty());
        }

        let temp_dir = tempfile::tempdir().unwrap();
        let exported = workbench::Workbench::export_all_snapshots(temp_dir.path()).unwrap();
        assert_eq!(exported, 12, "Must export all 12 snapshots to disk");
    }
}
