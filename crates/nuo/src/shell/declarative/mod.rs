//! Pure Declarative Component Paradigm (ADR-0072) and Storybook Protocol (ADR-0073).
//!
//! This module defines the architectural foundation for pure, isolated,
//! state-sovereign UI components in `nuo-ui`.

use nuo_ui_primitives::theme::Theme;
use nuotc::{Frame, Rect};

pub mod ask_user;
pub use nuo_ui_primitives::key;
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
        use super::notice_card::NoticeAction;

        let props = NoticeCardProps::new(
            "Test Notice",
            "Body content",
            Severity::Info,
            vec![NoticeAction::new("Btn 0"), NoticeAction::new("Btn 1")],
        );
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

        // Transition 3: Activate Idle button
        let cmd = NoticeCard::update(&props, &mut state, NoticeCardMsg::ActivateCurrent);
        assert_eq!(cmd, Some(notice_card::NoticeCardCmd::ActionTriggered(0)));

        // Transition 4 (ADR-0110 [INV-TUI-RUNNING-02]): Running and Disabled buttons are non-reentrant
        let running_props = NoticeCardProps::new(
            "Running Notice",
            "In-flight operation",
            Severity::Info,
            vec![
                NoticeAction::running("Cancel", "Cancelling", 0, Some(2)),
                NoticeAction::disabled("Apply", "Already running"),
            ],
        );
        let mut running_state = NoticeCardState::default();
        // Activate on running button -> must guard and return None
        let cmd_running = NoticeCard::update(
            &running_props,
            &mut running_state,
            NoticeCardMsg::ActivateCurrent,
        );
        assert_eq!(
            cmd_running, None,
            "[INV-TUI-RUNNING-02] Running action must guard re-entry"
        );

        running_state.focused_button = 1;
        // Activate on disabled button -> must return None
        let cmd_disabled = NoticeCard::update(
            &running_props,
            &mut running_state,
            NoticeCardMsg::ActivateCurrent,
        );
        assert_eq!(
            cmd_disabled, None,
            "[INV-TUI-RUNNING-02] Disabled action must be inert"
        );
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

            let key = crate::shell::declarative::key::Key::Down;
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

            let key = crate::shell::declarative::key::Key::Char(' ');
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
            13,
            "Default catalog must aggregate all 13 canonical stories"
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
        assert_eq!(exported, 13, "Must export all 13 snapshots to disk");
    }

    #[test]
    fn test_inv_tui_tier_02_canonical_presenter_bridges() {
        use super::notice_card::{NoticeCard, NoticeCardProps, NoticeCardState};
        use super::tool_card::{ToolCard, ToolCardProps, ToolCardState};
        use nuo_view::model::document::{NoticeSeverity, TranscriptMessage};
        use nuo_view::model::layout::{LayoutMap, NOTICE_BLOCK_IDX, TOOL_STEP_BLOCK_IDX};
        use nuotc::Rect;

        // 1. ToolCard Bridge: from TranscriptMessage to ToolCardProps
        let msg = TranscriptMessage::tool_step("call_123", "execute_command", "cargo test");

        let tool_props = ToolCardProps::from_transcript_message(&msg, None)
            .expect("Must extract ToolCardProps from transcript message");
        assert_eq!(tool_props.tool_name, "execute_command");
        assert!(matches!(
            tool_props.status,
            tool_card::ToolStatus::Running { .. }
        ));

        // 2. Connected rendering bridge: verifies layout map block registration
        let mut terminal = nuotc::TestTerminal::new(80, 24);
        let mut layout_map = LayoutMap::default();
        let theme = Theme::default();
        let state = ToolCardState::default();

        terminal.draw(|f| {
            ToolCard::render_with_layout(
                &tool_props,
                &state,
                f,
                Rect::new(0, 0, 80, 10),
                &theme,
                Some(&mut layout_map),
                Some(42),
            );
        });

        let region = layout_map.first_region_for_message(42);
        assert!(
            region.is_some(),
            "Connected render must record layout block regions"
        );
        assert_eq!(region.unwrap().block_idx, TOOL_STEP_BLOCK_IDX);
        assert_eq!(region.unwrap().message_idx, 42);

        // 3. NoticeCard Bridge: from TranscriptMessage to NoticeCardProps
        let notice_msg = TranscriptMessage::notice(
            NoticeSeverity::Warning,
            "OpenAI HTTP 429 Too Many Requests\nRate limit exceeded",
        );

        let notice_props = NoticeCardProps::from_transcript_message(&notice_msg)
            .expect("Must extract NoticeCardProps from transcript notice");
        assert!(notice_props.title.contains("429") || notice_props.title.contains("OpenAI"));

        let mut notice_layout = LayoutMap::default();
        let notice_state = NoticeCardState::default();
        terminal.draw(|f| {
            NoticeCard::render_with_layout(
                &notice_props,
                &notice_state,
                f,
                Rect::new(0, 10, 80, 6),
                &theme,
                Some(&mut notice_layout),
                Some(99),
            );
        });

        let notice_region = notice_layout.first_region_for_message(99);
        assert!(notice_region.is_some());
        assert_eq!(notice_region.unwrap().block_idx, NOTICE_BLOCK_IDX);
    }
}
