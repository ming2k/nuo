//! Composer input state tests: caret ownership, IME, selection, key handling, paste, focus.

use super::*;

#[test]
fn focused_tool_steps_mut_only_touches_focused_subagent_children() {
    let mut messages = conversation_with_subagents();
    // Focused on task_a: its single child is an assistant message (not a
    // tool step), so the focused stream has 1 message and 0 tool steps.
    let focus = vec![crate::app::ZoomFrame {
        call_id: "task_a".to_string(),
        saved_scroll: crate::app::ScrollSnapshot::default(),
    }];
    let total = focused_messages_mut(&mut messages, &focus).count();
    assert_eq!(total, 1);
    let tool_steps = focused_messages_mut(&mut messages, &focus)
        .filter(|m| m.is_tool_step())
        .count();
    assert_eq!(tool_steps, 0);

    // Root view: 4 messages total, 2 of which are tool steps.
    let focus: Vec<crate::app::ZoomFrame> = Vec::new();
    assert_eq!(focused_messages_mut(&mut messages, &focus).count(), 4);
    let tool_steps = focused_messages_mut(&mut messages, &focus)
        .filter(|m| m.is_tool_step())
        .count();
    assert_eq!(tool_steps, 2);
}

#[test]
fn paste_in_readonly_modal_is_dropped_silently() {
    // Read-only / non-text modals (Sessions, Usage stats, Permission, ...)
    // drop a paste silently — no insertion, no toast, no attachment.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.open_dialog(crate::surfaces::DialogKind::UsageStats);
    app.input = String::new();
    app.cursor_position = 0;

    clipboard_ops::apply_clipboard_paste(
        &mut app,
        crate::clipboard::ClipboardRead::Text("ignored".to_string()),
    );

    assert!(app.input.is_empty());
    assert!(
        app.copy_toast_until.is_none(),
        "readonly modal paste should not toast"
    );
    assert!(app.pending_text_pastes.is_empty());
}

// Caret ownership / visibility (IME anchor)
// `App::caret_owner` / `App::caret_visible` are the single source of truth for
// which surface holds the terminal cursor. The IME anchors its composition
// window to that cursor, so any state that owns no caret must hide it —
// otherwise the IME binds to a stale coordinate (the "drift" when a disclosure
// is clicked mid-composition). These lock the contract for every state.

#[test]
fn caret_owner_composer_by_default() {
    let (app, _tmp) = app_in_tempdir(&[], &[]);
    assert_eq!(app.caret_owner(), CaretOwner::Composer);
    assert!(
        app.caret_visible(),
        "no modal, no focus, no selection → visible"
    );
}

#[test]
fn caret_owner_none_when_step_focused() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.focused_target = Some(InteractiveTarget::tool_step(0));
    assert_eq!(app.caret_owner(), CaretOwner::None);
    assert!(
        !app.caret_visible(),
        "a focused transcript step owns no caret → hidden, IME unanchored"
    );
}

#[test]
fn caret_owner_none_in_subagent_view() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.enter_subagent("call-1".to_string());
    assert_eq!(app.caret_owner(), CaretOwner::None);
    assert!(
        !app.caret_visible(),
        "subagent zoom has no input line → cursor hidden, IME unanchored"
    );
}

#[test]
fn caret_owner_scene_for_the_dashboard_inline_prompt() {
    // ADR-0205 layering: a scene's own inline prompt is scene chrome, not the
    // chat composer and not an overlay. The `/dashboard` task line borrows the
    // composer *buffer* but renders in the scene's footer band, so it must
    // report `Scene` — otherwise the arbiter hides the cursor and the user
    // types into a field with no visible caret.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.switch_scene(crate::surfaces::SceneKind::Dashboard);
    assert_eq!(
        app.caret_owner(),
        CaretOwner::None,
        "the dashboard is a read-only console until its prompt opens"
    );
    app.host_prompting = true;
    assert_eq!(
        app.caret_owner(),
        CaretOwner::Scene,
        "the open inline prompt owns the cursor through its scene chrome"
    );
    assert!(
        app.caret_visible(),
        "a live scene prompt keeps the cursor visible so the IME anchors to it"
    );
    // An overlay above the scene still wins: the scene prompt is suspended.
    app.surfaces
        .present_dialog(crate::surfaces::DialogKind::UsageStats);
    assert_eq!(
        app.caret_owner(),
        CaretOwner::None,
        "a read-only overlay above the scene takes the caret away from it"
    );
}

#[test]
fn caret_owner_modal_for_caret_modals() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.surfaces
        .present_sheet(crate::surfaces::SheetKind::CustomProvider);
    assert_eq!(
        app.caret_owner(),
        CaretOwner::Overlay,
        "the provider editor borrows the input line and renders its own caret",
    );
    assert!(
        app.caret_visible(),
        "the provider editor must keep the cursor visible so the IME anchors to its field",
    );
    // The input-injection sheet borrows the composer line the same way
    // (ADR-0173 §3) — but it is a sheet, not a modal.
    app.set_active_sheet_for_test(crate::sheet::SheetKind::InputInjection);
    assert_eq!(
        app.caret_owner(),
        CaretOwner::Overlay,
        "the injection sheet renders its own caret",
    );
    assert!(
        app.caret_visible(),
        "the injection sheet must keep the cursor visible so the IME anchors",
    );
}

#[test]
fn caret_owner_none_for_read_only_and_decision_modals() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    for dialog in [
        crate::surfaces::DialogKind::UsageStats,
        crate::surfaces::DialogKind::Sessions,
        crate::surfaces::DialogKind::Tools,
        crate::surfaces::DialogKind::Mcp,
        crate::surfaces::DialogKind::Permissions,
    ] {
        app.open_dialog(dialog);
        assert_eq!(
            app.caret_owner(),
            CaretOwner::None,
            "{dialog:?} renders no caret → cursor must hide so the IME has no stale anchor",
        );
        assert!(
            !app.caret_visible(),
            "{dialog:?} must hide the terminal cursor",
        );
    }
    app.switch_scene(crate::surfaces::SceneKind::Settings);
    assert_eq!(app.caret_owner(), CaretOwner::None);
    assert!(!app.caret_visible());
    // The permission and question sheets cover the same default state: no
    // caret unless the question's "Other" row is highlighted.
    for kind in [
        crate::sheet::SheetKind::Permission,
        crate::sheet::SheetKind::Question,
    ] {
        app.set_active_sheet_for_test(kind);
        assert_eq!(
            app.caret_owner(),
            CaretOwner::None,
            "{kind:?} renders no caret by default",
        );
    }
}

#[test]
fn caret_owner_question_owns_caret_only_on_other() {
    // The Question modal is a decision sheet (no caret) EXCEPT while the
    // synthetic "Other" free-text row is highlighted — then it is a real
    // text-input surface and must own the terminal cursor so the host IME can
    // anchor its composition window. Navigating to/from "Other" flips
    // ownership, so the IME anchor appears exactly when there is a field to
    // type into and never when there is not.
    use crate::question_model::{QuestionAction, QuestionModel};
    use nuo_contracts::{UserQuestion, UserQuestionOption, UserQuestionRequest};

    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let req = UserQuestionRequest {
        id: "q".into(),
        questions: vec![UserQuestion {
            header: None,
            question: "pick".into(),
            options: vec![
                UserQuestionOption {
                    label: "a".into(),
                    description: None,
                },
                UserQuestionOption {
                    label: "b".into(),
                    description: None,
                },
            ],
            multi_select: false,
        }],
        origin: None,
    };
    // Open: highlight on row 0 (a real option) → no caret, cursor hidden.
    let model = QuestionModel::open(req);
    app.set_active_sheet_for_test(crate::sheet::SheetKind::Question);
    app.question = Some(model.clone());
    assert_eq!(
        app.caret_owner(),
        CaretOwner::None,
        "real option → no caret"
    );
    assert!(
        !app.caret_visible(),
        "a non-Other option must hide the cursor so the IME has no stale anchor",
    );

    // Navigate down to "Other" (index 2) → caret owned, cursor visible.
    let model = model.update(QuestionAction::Down).0; // -> b (1)
    let model = model.update(QuestionAction::Down).0; // -> Other (2)
    app.question = Some(model);
    assert_eq!(
        app.caret_owner(),
        CaretOwner::Overlay,
        "Other highlighted → overlay owns the caret for the IME",
    );
    assert!(
        app.caret_visible(),
        "the Other field must keep the cursor visible so the IME anchors to it",
    );

    // Navigate back to a real option → ownership reverts to None.
    let model = app.question.take().unwrap().update(QuestionAction::Up).0;
    app.question = Some(model);
    assert_eq!(
        app.caret_owner(),
        CaretOwner::None,
        "leaving Other must drop caret ownership again",
    );
}

#[test]
fn caret_hidden_while_selection_active_even_for_composer() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    // Composer owns the caret, but an active selection hides the block cursor
    // so it does not clash with the selection background. Ownership is
    // unaffected; only visibility folds in the selection.
    assert_eq!(app.caret_owner(), CaretOwner::Composer);
    app.selection = SelectionState::Range {
        anchor: crate::model::layout::SemanticCursor::new(0, 0, 0),
        head: crate::model::layout::SemanticCursor::new(0, 0, 3),
    };
    assert_eq!(app.caret_owner(), CaretOwner::Composer);
    assert!(
        !app.caret_visible(),
        "an active selection hides the cursor regardless of ownership",
    );
}

#[test]
fn has_input_selection_detects_both_block_and_range() {
    let mut app = app_with_input_selection("hello");
    assert!(app.has_input_selection());

    // A transcript selection never binds the composer.
    app.selection = SelectionState::Block {
        message_idx: 0,
        block_idx: 0,
    };
    assert!(
        !app.has_input_selection(),
        "transcript selections must not trigger the input caret relay"
    );

    // An active Range on INPUT_MSG_IDX is an input selection.
    app.selection = SelectionState::Range {
        anchor: crate::model::layout::SemanticCursor::new(crate::render::INPUT_MSG_IDX, 0, 0),
        head: crate::model::layout::SemanticCursor::new(crate::render::INPUT_MSG_IDX, 0, 2),
    };
    assert!(app.has_input_selection());

    // A collapsed Range (anchor == head) is not active and does not count.
    app.selection = SelectionState::Range {
        anchor: crate::model::layout::SemanticCursor::new(crate::render::INPUT_MSG_IDX, 0, 0),
        head: crate::model::layout::SemanticCursor::new(crate::render::INPUT_MSG_IDX, 0, 0),
    };
    assert!(!app.has_input_selection());
}

#[test]
fn input_selection_relays_arrows_only_when_composer_owns_caret() {
    let mut app = app_with_input_selection("hello");
    assert_eq!(app.caret_owner(), CaretOwner::Composer);
    assert!(app.input_selection_relays_arrows());

    // A transcript step holding focus means the composer no longer owns the
    // caret: arrows mean step navigation, so the relay must stand down even
    // though a selection is technically active.
    app.focused_target = Some(crate::model::layout::InteractiveTarget::tool_step(0));
    assert!(
        !app.input_selection_relays_arrows(),
        "arrows belong to step navigation while a step holds focus"
    );
}

// Input viewport: wheel scrolling and selection edge-autoscroll

/// A composer panel fixture: a 60-col box at the screen bottom whose height
/// leaves `visible` text rows, and a draft of one-char-per-line rows so the
/// wrapped-row count is exact. The trailing newline is absent, so `n` rows of
/// text map to `n` wrapped rows.
fn app_with_input_viewport(rows: usize, visible: usize) -> (App, tempfile::TempDir) {
    let (mut app, tmp) = app_in_tempdir(&[], &[]);
    let height = visible as u16 + crate::design::COMPOSER_VERTICAL_CHROME_ROWS;
    app.ui.begin(nuotc::Rect::new(0, 0, 80, 60));
    app.ui.mount(
        crate::ui::UiKey::Composer,
        nuotc::Rect::new(0, 40, 60, height),
    );
    app.ui.commit();
    app.input = (0..rows)
        .map(|i| char::from(b'a' + (i % 26) as u8).to_string())
        .collect::<Vec<_>>()
        .join("\n");
    (app, tmp)
}

#[test]
fn input_viewport_wheel_steps_clamp_to_hidden_rows() {
    // 8 wrapped rows, 3 visible → 5 rows of scroll range.
    let (mut app, _tmp) = app_with_input_viewport(8, 3);
    assert_eq!(app.input_scroll_max(), Some(5), "8 rows − 3 visible");

    assert_eq!(app.step_input_scroll(false, 4), Some(4));
    assert!(
        !app.input_scroll_follow_cursor,
        "manual scrolling suspends caret-follow until the next edit"
    );
    assert_eq!(
        app.step_input_scroll(false, 4),
        Some(5),
        "scroll clamps at the last hidden row"
    );
    assert_eq!(app.step_input_scroll(true, 2), Some(3));
    assert_eq!(
        app.step_input_scroll(true, 99),
        Some(0),
        "scrolling up clamps at the top"
    );
    app.set_cursor(0);
    assert!(
        app.input_scroll_follow_cursor,
        "caret movement re-arms viewport following"
    );

    // No composer on screen (overlay modal, first frame): not scrollable.
    app.ui.begin(nuotc::Rect::new(0, 0, 80, 60));
    app.ui.commit();
    assert_eq!(app.input_scroll_max(), None);
    assert_eq!(app.step_input_scroll(false, 1), None);
}

#[test]
fn input_viewport_does_not_consume_wheel_when_every_row_is_visible() {
    let (mut app, _tmp) = app_with_input_viewport(2, 3);
    assert_eq!(app.input_scroll_max(), Some(0));
    assert_eq!(
        app.step_input_scroll(false, 4),
        None,
        "a non-scrollable composer must let the wheel fall through to the transcript"
    );
    assert!(app.input_scroll_follow_cursor);
}

#[test]
fn input_edge_autoscroll_arms_beyond_text_rows_and_extends_selection() {
    // 8 rows, 2 visible; viewport scrolled to the end (rows 6–7 visible,
    // text rows at screen y 41–42, chrome at 43–44 on a height-5 panel).
    let (mut app, _tmp) = app_with_input_viewport(8, 2);
    app.input_scroll = 6;

    // A selection drag anchored in the composer's second row.
    app.drag.begin_range(
        &mut app.selection,
        crate::model::layout::SemanticCursor::new(crate::render::INPUT_MSG_IDX, 0, 2),
    );

    // Pointer above the text rows (the panel's own breathing row counts as
    // past the edge): arms upward, and the first step both scrolls and pins
    // the selection head to the newly exposed row's start byte. (The event
    // loop's SelectionUpdate arm stores the direction before stepping; the
    // test mirrors that hand-off.)
    assert_eq!(app.input_drag_scroll_edge(40), Some(true));
    app.input_drag_scroll = Some(true);
    assert!(app.step_input_drag_scroll());
    assert_eq!(app.input_scroll, 5);
    assert!(
        !app.input_scroll_follow_cursor,
        "edge-autoscroll must survive the next composer render"
    );
    let SelectionState::Range { head, .. } = &app.selection else {
        panic!("selection survives the edge step");
    };
    let wrapped = crate::composer::composer_wrapped(
        &app.input,
        crate::composer::composer_text_width(60),
        app.input.len(),
    );
    assert_eq!(
        head.byte_offset, wrapped[5].start_byte,
        "head pins to the exposed viewport edge"
    );

    // Pointer inside the text rows: no arm — the drag follows the pointer
    // normally again.
    assert_eq!(app.input_drag_scroll_edge(41), None);

    // Pointer at/below the last text row (the gap/hint chrome): arms
    // downward while rows hide below, stepping to the bottom clamp.
    assert_eq!(app.input_drag_scroll_edge(43), Some(false));
    app.input_drag_scroll = Some(false);
    while app.step_input_drag_scroll() {}
    assert_eq!(app.input_scroll, 6, "down-arm marches to the max scroll");
    assert_eq!(
        app.input_drag_scroll, None,
        "reaching the viewport boundary must stop the animation heartbeat"
    );
    assert_eq!(
        app.input_drag_scroll_edge(43),
        None,
        "clamped: no more rows"
    );

    // Ending the drag disarms the autoscroll; the heartbeat step no-ops.
    app.drag.finish(&mut app.selection);
    app.input_drag_scroll = None;
    assert!(!app.step_input_drag_scroll());
}

#[test]
fn input_edge_autoscroll_ignores_transcript_anchored_drags() {
    let (mut app, _tmp) = app_with_input_viewport(8, 2);
    app.input_scroll = 0;
    // A drag anchored in transcript content never drives the input viewport,
    // however far above the input the pointer climbs.
    app.drag.begin_range(
        &mut app.selection,
        crate::model::layout::SemanticCursor::new(0, 0, 5),
    );
    assert_eq!(app.input_drag_scroll_edge(0), None);
    assert!(!app.step_input_drag_scroll());
    assert_eq!(app.input_scroll, 0);
}

#[test]
fn failed_intent_send_latches_the_daemon_link_down_state() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);

    assert!(!app.link_down);
    assert!(!app.send_intent(AgentRequest::Interrupt));
    assert!(app.link_down);
}

fn relay_key(
    app: &mut App,
    code: crossterm::event::KeyCode,
    modifiers: crossterm::event::KeyModifiers,
) -> Option<crate::input::InputAction> {
    crate::event_loop::component_input::route(
        app,
        &crossterm::event::Event::Key(crossterm::event::KeyEvent::new(code, modifiers)),
        &[crate::ui::UiKey::Composer],
    )
}

#[test]
fn shift_right_selects_characters_and_extracts() {
    use crate::event_loop::transcript::extract_selection_text;
    use crossterm::event::KeyModifiers;

    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.input = "hello world".to_string();
    app.set_cursor(0);

    // Shift+Right selects first char 'h'
    let action = relay_key(
        &mut app,
        crossterm::event::KeyCode::Right,
        KeyModifiers::SHIFT,
    );
    assert!(matches!(action, Some(crate::input::InputAction::None)));
    assert_eq!(app.cursor_position, 1);
    assert_eq!(
        app.selection,
        SelectionState::InputRange {
            anchor_byte: 0,
            head_byte: 1,
        }
    );
    assert_eq!(
        extract_selection_text(&app.selection, &[], &app.input, &app.ui.document, None),
        Some("h".to_string())
    );

    // Shift+Right again selects "he"
    let action = relay_key(
        &mut app,
        crossterm::event::KeyCode::Right,
        KeyModifiers::SHIFT,
    );
    assert!(matches!(action, Some(crate::input::InputAction::None)));
    assert_eq!(app.cursor_position, 2);
    assert_eq!(
        extract_selection_text(&app.selection, &[], &app.input, &app.ui.document, None),
        Some("he".to_string())
    );

    // Shift+Left shrinks to "h"
    let action = relay_key(
        &mut app,
        crossterm::event::KeyCode::Left,
        KeyModifiers::SHIFT,
    );
    assert!(matches!(action, Some(crate::input::InputAction::None)));
    assert_eq!(app.cursor_position, 1);
    assert_eq!(
        extract_selection_text(&app.selection, &[], &app.input, &app.ui.document, None),
        Some("h".to_string())
    );

    // Shift+Left again collapses back to None
    let action = relay_key(
        &mut app,
        crossterm::event::KeyCode::Left,
        KeyModifiers::SHIFT,
    );
    assert!(matches!(action, Some(crate::input::InputAction::None)));
    assert_eq!(app.cursor_position, 0);
    assert_eq!(app.selection, SelectionState::None);
}

#[test]
fn shift_left_backwards_selection() {
    use crate::event_loop::transcript::extract_selection_text;
    use crossterm::event::KeyModifiers;

    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.input = "hello world".to_string();
    app.set_cursor(5); // parked after "hello"

    // Shift+Left selects 'o' backwards
    let action = relay_key(
        &mut app,
        crossterm::event::KeyCode::Left,
        KeyModifiers::SHIFT,
    );
    assert!(matches!(action, Some(crate::input::InputAction::None)));
    assert_eq!(app.cursor_position, 4);
    assert_eq!(
        app.selection,
        SelectionState::InputRange {
            anchor_byte: 5,
            head_byte: 4,
        }
    );
    assert_eq!(
        extract_selection_text(&app.selection, &[], &app.input, &app.ui.document, None),
        Some("o".to_string())
    );

    // Shift+Left again selects "lo"
    let action = relay_key(
        &mut app,
        crossterm::event::KeyCode::Left,
        KeyModifiers::SHIFT,
    );
    assert!(matches!(action, Some(crate::input::InputAction::None)));
    assert_eq!(app.cursor_position, 3);
    assert_eq!(
        extract_selection_text(&app.selection, &[], &app.input, &app.ui.document, None),
        Some("lo".to_string())
    );

    // Shift+Right shrinks back to "o"
    let action = relay_key(
        &mut app,
        crossterm::event::KeyCode::Right,
        KeyModifiers::SHIFT,
    );
    assert!(matches!(action, Some(crate::input::InputAction::None)));
    assert_eq!(app.cursor_position, 4);
    assert_eq!(
        extract_selection_text(&app.selection, &[], &app.input, &app.ui.document, None),
        Some("o".to_string())
    );

    // Shift+Right collapses to None
    let action = relay_key(
        &mut app,
        crossterm::event::KeyCode::Right,
        KeyModifiers::SHIFT,
    );
    assert!(matches!(action, Some(crate::input::InputAction::None)));
    assert_eq!(app.cursor_position, 5);
    assert_eq!(app.selection, SelectionState::None);

    // Shift+Right again selects forwards: ' ' (space)
    let action = relay_key(
        &mut app,
        crossterm::event::KeyCode::Right,
        KeyModifiers::SHIFT,
    );
    assert!(matches!(action, Some(crate::input::InputAction::None)));
    assert_eq!(app.cursor_position, 6);
    assert_eq!(
        extract_selection_text(&app.selection, &[], &app.input, &app.ui.document, None),
        Some(" ".to_string())
    );
}

#[test]
fn shift_ctrl_left_and_right_word_selection() {
    use crate::event_loop::transcript::extract_selection_text;
    use crossterm::event::KeyModifiers;

    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.input = "hello world".to_string();
    app.set_cursor(0);

    // Shift+Ctrl+Right selects "hello"
    let action = relay_key(
        &mut app,
        crossterm::event::KeyCode::Right,
        KeyModifiers::SHIFT | KeyModifiers::CONTROL,
    );
    assert!(matches!(action, Some(crate::input::InputAction::None)));
    assert_eq!(app.cursor_position, 5);
    assert_eq!(
        extract_selection_text(&app.selection, &[], &app.input, &app.ui.document, None),
        Some("hello".to_string())
    );

    // Shift+Ctrl+Right again selects "hello world"
    let action = relay_key(
        &mut app,
        crossterm::event::KeyCode::Right,
        KeyModifiers::SHIFT | KeyModifiers::CONTROL,
    );
    assert!(matches!(action, Some(crate::input::InputAction::None)));
    assert_eq!(app.cursor_position, 11);
    assert_eq!(
        extract_selection_text(&app.selection, &[], &app.input, &app.ui.document, None),
        Some("hello world".to_string())
    );
}

#[test]
fn shift_up_and_down_multiline_selection() {
    use crate::event_loop::transcript::extract_selection_text;
    use crossterm::event::KeyModifiers;

    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.input = "hello\nworld".to_string();
    // 'r' is index 8 (hello=5, \n=6, w=6, o=7, r=8)
    app.set_cursor(8);

    // Shift+Up moves to line 1 col 2 ('l' at index 2)
    let action = relay_key(&mut app, crossterm::event::KeyCode::Up, KeyModifiers::SHIFT);
    assert!(matches!(action, Some(crate::input::InputAction::None)));
    assert_eq!(app.cursor_position, 2);
    assert_eq!(
        extract_selection_text(&app.selection, &[], &app.input, &app.ui.document, None),
        Some("llo\nwo".to_string())
    );

    // Shift+Down returns to index 8, collapsing selection
    let action = relay_key(
        &mut app,
        crossterm::event::KeyCode::Down,
        KeyModifiers::SHIFT,
    );
    assert!(matches!(action, Some(crate::input::InputAction::None)));
    assert_eq!(app.cursor_position, 8);
    assert_eq!(app.selection, SelectionState::None);
}

#[test]
fn shift_selection_delete_and_backspace() {
    use crossterm::event::KeyModifiers;

    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.input = "hello world".to_string();
    app.set_cursor(6);

    // Select "world"
    relay_key(
        &mut app,
        crossterm::event::KeyCode::End,
        KeyModifiers::SHIFT,
    );
    assert_eq!(app.cursor_position, 11);

    // Backspace replaces the selected text
    let action = relay_key(
        &mut app,
        crossterm::event::KeyCode::Backspace,
        KeyModifiers::NONE,
    );
    assert!(matches!(action, Some(crate::input::InputAction::Backspace)));
    assert_eq!(app.input, "hello ");
    assert_eq!(app.cursor_position, 6);
    assert_eq!(app.selection, SelectionState::None);
}

#[test]
fn shift_selection_cjk() {
    use crate::event_loop::transcript::extract_selection_text;
    use crossterm::event::KeyModifiers;

    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.input = "你好世界".to_string();
    app.set_cursor(0);

    // Shift+Right selects "你"
    relay_key(
        &mut app,
        crossterm::event::KeyCode::Right,
        KeyModifiers::SHIFT,
    );
    assert_eq!(app.cursor_position, 1);
    assert_eq!(
        extract_selection_text(&app.selection, &[], &app.input, &app.ui.document, None),
        Some("你".to_string())
    );

    // Shift+Right selects "你好"
    relay_key(
        &mut app,
        crossterm::event::KeyCode::Right,
        KeyModifiers::SHIFT,
    );
    assert_eq!(app.cursor_position, 2);
    assert_eq!(
        extract_selection_text(&app.selection, &[], &app.input, &app.ui.document, None),
        Some("你好".to_string())
    );
}

#[test]
fn shift_home_and_end_selection() {
    use crate::event_loop::transcript::extract_selection_text;
    use crossterm::event::KeyModifiers;

    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.input = "hello world".to_string();
    app.set_cursor(5);

    // Shift+Home selects to start
    relay_key(
        &mut app,
        crossterm::event::KeyCode::Home,
        KeyModifiers::SHIFT,
    );
    assert_eq!(app.cursor_position, 0);
    assert_eq!(
        extract_selection_text(&app.selection, &[], &app.input, &app.ui.document, None),
        Some("hello".to_string())
    );

    // Shift+End selects from 5 to 11
    relay_key(
        &mut app,
        crossterm::event::KeyCode::End,
        KeyModifiers::SHIFT,
    );
    assert_eq!(app.cursor_position, 11);
    assert_eq!(
        extract_selection_text(&app.selection, &[], &app.input, &app.ui.document, None),
        Some(" world".to_string())
    );
}

#[test]
fn esc_cancels_shift_selection() {
    use crossterm::event::KeyModifiers;

    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.input = "hello world".to_string();
    app.set_cursor(0);

    relay_key(
        &mut app,
        crossterm::event::KeyCode::Right,
        KeyModifiers::SHIFT,
    );
    assert!(app.has_input_selection());

    let action = relay_key(&mut app, crossterm::event::KeyCode::Esc, KeyModifiers::NONE);
    assert!(matches!(action, Some(crate::input::InputAction::None)));
    assert_eq!(app.selection, SelectionState::None);
    assert_eq!(app.input, "hello world");
}
