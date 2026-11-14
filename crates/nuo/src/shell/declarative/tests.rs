use super::notice_card::{NoticeCard, NoticeCardMsg, NoticeCardProps, NoticeCardState, Severity};
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
        9,
        "ToolCard must declare all 9 canonical stories covering diff, command, code, matches, listing, and 1000-line fold"
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
        50,
        "Default catalog must aggregate all 50 canonical stories across 12 domains"
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
    assert_eq!(exported, 50, "Must export all 50 snapshots to disk");
}

#[test]
fn test_inv_tui_story_04_continuous_live_tick_dispatch() {
    use super::story::StoryEventOutcome;
    use super::tool_card::ToolCard;

    let stories = ToolCard::stories();
    let streaming_story = stories
        .into_iter()
        .find(|s| s.name.contains("Streaming"))
        .expect("ToolCard must have streaming in-flight scenario");

    let mut instance = instantiate_story::<ToolCard>(streaming_story);

    // Tick 1: Must consume and mutate spinner frame
    let outcome = instance.tick();
    assert_eq!(
        outcome,
        StoryEventOutcome::Consumed,
        "[INV-TUI-STORY-04] In-flight tool card must consume tick event"
    );

    // Reset: Restores initial props and state
    instance.reset();
}

#[test]
fn test_inv_tui_story_05_dev_loop_continuity_roundtrip() {
    use super::workbench::{StorybookContinuityState, ViewportMode, Workbench, default_catalog};

    let state = StorybookContinuityState {
        selected_component: "Toast".to_string(),
        selected_story: "Clipboard Success (CopyOk)".to_string(),
        viewport_mode: "80x24".to_string(),
        theme_index: 2,
        search_query: "toast".to_string(),
    };

    let serialized = serde_json::to_string(&state).unwrap();
    let deserialized: StorybookContinuityState = serde_json::from_str(&serialized).unwrap();
    assert_eq!(deserialized.selected_component, "Toast");
    assert_eq!(deserialized.selected_story, "Clipboard Success (CopyOk)");
    assert_eq!(deserialized.viewport_mode, "80x24");
    assert_eq!(deserialized.theme_index, 2);

    // Workbench state restoration check
    let mut wb = Workbench::new(default_catalog());
    wb.apply_continuity_state(deserialized);
    assert_eq!(wb.viewport_mode(), ViewportMode::Size80x24);
    assert_eq!(wb.theme_index(), 2);
    let curr_idx = wb.current_story_index().expect("Active story must resolve");
    let active = &wb.stories()[curr_idx];
    assert_eq!(active.component_name(), "Toast");
    assert_eq!(active.story_name(), "Clipboard Success (CopyOk)");
}

#[test]
fn test_inv_tui_decl_toast_pure_state_and_snapshot() {
    use super::toast_card::ToastCard;

    let stories = ToastCard::stories();
    assert_eq!(
        stories.len(),
        5,
        "ToastCard must declare all 5 canonical stories"
    );

    for story in stories {
        let mut instance = instantiate_story::<ToastCard>(story);
        let snapshot = render_story_styled_snapshot(instance.as_ref(), 100, 10);
        assert!(!snapshot.is_empty());

        let key = crate::shell::declarative::key::Key::Char(' ');
        let outcome = instance.handle_key(&key);
        assert_eq!(outcome, story::StoryEventOutcome::Consumed);
    }
}

#[test]
fn test_inv_tui_decl_which_key_pure_state_and_snapshot() {
    use super::which_key_card::WhichKeyCard;

    let stories = WhichKeyCard::stories();
    assert_eq!(
        stories.len(),
        3,
        "WhichKeyCard must declare all 3 canonical stories"
    );

    for story in stories {
        let mut instance = instantiate_story::<WhichKeyCard>(story);
        let snapshot = render_story_styled_snapshot(instance.as_ref(), 100, 16);
        assert!(!snapshot.is_empty());

        let key = crate::shell::declarative::key::Key::Down;
        let outcome = instance.handle_key(&key);
        assert_eq!(outcome, story::StoryEventOutcome::Consumed);
    }
}

#[test]
fn test_inv_tui_decl_modal_pure_state_and_snapshot() {
    use super::modal_card::ModalCard;

    let stories = ModalCard::stories();
    assert_eq!(
        stories.len(),
        3,
        "ModalCard must declare all 3 canonical stories"
    );

    for story in stories {
        let mut instance = instantiate_story::<ModalCard>(story);
        let snapshot = render_story_styled_snapshot(instance.as_ref(), 100, 20);
        assert!(!snapshot.is_empty());

        let key = crate::shell::declarative::key::Key::Down;
        let outcome = instance.handle_key(&key);
        assert_eq!(outcome, story::StoryEventOutcome::Consumed);
    }
}

#[test]
fn test_inv_tui_decl_turn_band_pure_state_and_snapshot() {
    use super::turn_band_card::TurnBandCard;

    let stories = TurnBandCard::stories();
    assert_eq!(
        stories.len(),
        3,
        "TurnBandCard must declare all 3 canonical stories"
    );

    for story in stories {
        let mut instance = instantiate_story::<TurnBandCard>(story);
        let snapshot = render_story_styled_snapshot(instance.as_ref(), 100, 10);
        assert!(!snapshot.is_empty());

        let outcome = instance.tick();
        assert_eq!(outcome, story::StoryEventOutcome::Consumed);
    }
}

#[test]
fn test_inv_tui_decl_empty_hero_pure_state_and_snapshot() {
    use super::empty_hero_card::EmptyHeroCard;

    let stories = EmptyHeroCard::stories();
    assert_eq!(
        stories.len(),
        3,
        "EmptyHeroCard must declare all 3 canonical stories"
    );

    for story in stories {
        let mut instance = instantiate_story::<EmptyHeroCard>(story);
        let snapshot = render_story_styled_snapshot(instance.as_ref(), 100, 24);
        assert!(!snapshot.is_empty());

        let key = crate::shell::declarative::key::Key::Right;
        let outcome = instance.handle_key(&key);
        assert_eq!(outcome, story::StoryEventOutcome::Consumed);
    }
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

#[test]
fn test_inv_tui_decl_palette_pure_state_and_snapshot() {
    use super::palette_card::PaletteCard;

    let stories = PaletteCard::stories();
    assert_eq!(
        stories.len(),
        4,
        "PaletteCard must declare 4 canonical preset stories"
    );

    for story in stories {
        let instance = instantiate_story::<PaletteCard>(story);
        let snapshot = render_story_styled_snapshot(instance.as_ref(), 100, 24);
        assert!(!snapshot.is_empty());
        assert!(snapshot.contains("Color Tokens"));
    }
}

#[test]
fn test_inv_tui_decl_reasoning_pure_state_and_snapshot() {
    use super::reasoning_card::ReasoningCard;

    let stories = ReasoningCard::stories();
    assert_eq!(
        stories.len(),
        3,
        "ReasoningCard must declare 3 canonical stories"
    );

    for story in stories {
        let mut instance = instantiate_story::<ReasoningCard>(story);
        let snapshot = render_story_styled_snapshot(instance.as_ref(), 100, 16);
        assert!(!snapshot.is_empty());

        let key = crate::shell::declarative::key::Key::Char(' ');
        let outcome = instance.handle_key(&key);
        assert_eq!(outcome, story::StoryEventOutcome::Consumed);
    }
}

#[test]
fn test_inv_tui_decl_notice_dialog_pure_state_and_snapshot() {
    use super::notice_dialog_card::NoticeDialogCard;

    let stories = NoticeDialogCard::stories();
    assert_eq!(
        stories.len(),
        5,
        "NoticeDialogCard must declare 5 canonical stories (Empty, MultiSeverity, Detail, Overflow, Extreme)"
    );

    for story in stories {
        let mut instance = instantiate_story::<NoticeDialogCard>(story);
        let snapshot = render_story_styled_snapshot(instance.as_ref(), 100, 24);
        assert!(!snapshot.is_empty());
        assert!(
            snapshot.contains("Notices & Disclosures"),
            "Modal header must render: {snapshot}"
        );

        // Verify interactive message routing (down arrow)
        let key = crate::shell::declarative::key::Key::Down;
        let outcome = instance.handle_key(&key);
        assert_eq!(outcome, story::StoryEventOutcome::Consumed);

        // Verify detail toggle (Enter)
        let enter_key = crate::shell::declarative::key::Key::Enter;
        let outcome_enter = instance.handle_key(&enter_key);
        assert_eq!(outcome_enter, story::StoryEventOutcome::Consumed);

        // Verify copy command (c)
        let copy_key = crate::shell::declarative::key::Key::Char('c');
        let outcome_copy = instance.handle_key(&copy_key);
        assert_eq!(outcome_copy, story::StoryEventOutcome::Consumed);

        // Verify clear action (x) renders empty guidance
        let clear_key = crate::shell::declarative::key::Key::Char('x');
        let outcome_clear = instance.handle_key(&clear_key);
        assert_eq!(outcome_clear, story::StoryEventOutcome::Consumed);
        let snap_cleared = render_story_styled_snapshot(instance.as_ref(), 100, 24);
        assert!(
            snap_cleared.contains("No operational notices"),
            "Cleared notice state must project empty guidance: {snap_cleared}"
        );

        // Verify reset (r) restores populated state
        let reset_key = crate::shell::declarative::key::Key::Char('r');
        let outcome_reset = instance.handle_key(&reset_key);
        assert_eq!(outcome_reset, story::StoryEventOutcome::Consumed);
    }

    // Verify ADR-0112 [INV-TUI-TIER-02] canonical presenter bridge
    let lines = vec![
        nuo_view::model::log::SurfaceLine::disclosure(
            nuo_view::model::log::LineStatus::Info,
            "Bridge test",
            1_700_000_000_000,
        )
        .with_label("test"),
    ];
    let bridged_props = super::notice_dialog_card::NoticeDialogCardProps::from_lines(&lines);
    assert_eq!(bridged_props.lines.len(), 1);
}
