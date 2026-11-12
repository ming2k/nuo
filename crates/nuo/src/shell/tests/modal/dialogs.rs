use super::*;

/// ADR-0047 [INV-SURFACE-06, INV-SURFACE-08]: DialogView entities autonomously own their
/// scrolling via `handle_input`, resolving both standard lists and drill-down sub-layers
/// (like `info_detail`) without leaking raw pointers through `modal_scroll_field`.
#[test]
fn dialogs_autonomously_handle_scroll_actions_and_sub_views() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);

    // 1. List dialogs (Queue): ScrollDown increments scroll and clears follow.
    app.open_dialog(crate::shell::surfaces::DialogKind::Queue);
    app.surfaces
        .dlg_mut::<crate::shell::surfaces::QueueDialog>()
        .follow = true;
    app.surfaces
        .dlg_mut::<crate::shell::surfaces::QueueDialog>()
        .scroll = 0;
    if let Some(mut ent) = app.surfaces.take_active_view() {
        let outcome = ent.handle_input(&crate::shell::input::InputAction::ScrollDown, &mut app, "");
        assert_eq!(outcome, crate::shell::surfaces::DialogOutcome::Consumed);
        app.surfaces.put_active_view(ent);
    }
    assert_eq!(
        app.surfaces
            .dlg_mut::<crate::shell::surfaces::QueueDialog>()
            .scroll,
        1
    );
    assert!(
        !app.surfaces
            .dlg_mut::<crate::shell::surfaces::QueueDialog>()
            .follow
    );

    // 2. Pure-scroll dialog (UsageStats): ScrollDown increments scroll.
    app.open_dialog(crate::shell::surfaces::DialogKind::UsageStats);
    app.surfaces
        .dlg_mut::<crate::shell::surfaces::UsageStatsDialog>()
        .scroll = 5;
    if let Some(mut ent) = app.surfaces.take_active_view() {
        let outcome = ent.handle_input(&crate::shell::input::InputAction::ScrollDown, &mut app, "");
        assert_eq!(outcome, crate::shell::surfaces::DialogOutcome::Consumed);
        app.surfaces.put_active_view(ent);
    }
    assert_eq!(
        app.surfaces
            .dlg_mut::<crate::shell::surfaces::UsageStatsDialog>()
            .scroll,
        6
    );

    // 3. ThreadsDialog: in list view, scrolls list and clears follow.
    app.open_dialog(crate::shell::surfaces::DialogKind::Threads);
    app.surfaces
        .dlg_mut::<crate::shell::surfaces::ThreadsDialog>()
        .follow = true;
    app.surfaces
        .dlg_mut::<crate::shell::surfaces::ThreadsDialog>()
        .scroll = 2;
    app.surfaces
        .dlg_mut::<crate::shell::surfaces::ThreadsDialog>()
        .info_detail = false;
    app.surfaces
        .dlg_mut::<crate::shell::surfaces::ThreadsDialog>()
        .info_scroll = 0;
    if let Some(mut ent) = app.surfaces.take_active_view() {
        let outcome = ent.handle_input(&crate::shell::input::InputAction::ScrollDown, &mut app, "");
        assert_eq!(outcome, crate::shell::surfaces::DialogOutcome::Consumed);
        app.surfaces.put_active_view(ent);
    }
    assert_eq!(
        app.surfaces
            .dlg_mut::<crate::shell::surfaces::ThreadsDialog>()
            .scroll,
        3
    );
    assert!(
        !app.surfaces
            .dlg_mut::<crate::shell::surfaces::ThreadsDialog>()
            .follow
    );
    assert_eq!(
        app.surfaces
            .dlg_mut::<crate::shell::surfaces::ThreadsDialog>()
            .info_scroll,
        0
    );

    // 4. ThreadsDialog sub-view parity [INV-SURFACE-08]: when info_detail is active,
    // scrolls info_scroll independently, leaving the main list scroll intact!
    app.surfaces
        .dlg_mut::<crate::shell::surfaces::ThreadsDialog>()
        .info_detail = true;
    if let Some(mut ent) = app.surfaces.take_active_view() {
        let outcome = ent.handle_input(&crate::shell::input::InputAction::ScrollDown, &mut app, "");
        assert_eq!(outcome, crate::shell::surfaces::DialogOutcome::Consumed);
        app.surfaces.put_active_view(ent);
    }
    assert_eq!(
        app.surfaces
            .dlg_mut::<crate::shell::surfaces::ThreadsDialog>()
            .info_scroll,
        1,
        "info_scroll incremented"
    );
    assert_eq!(
        app.surfaces
            .dlg_mut::<crate::shell::surfaces::ThreadsDialog>()
            .scroll,
        3,
        "main list scroll untouched"
    );

    // 5. ConnectionsDialog sub-view parity [INV-SURFACE-08]: when info_detail is active,
    // scrolls info_scroll independently.
    app.open_dialog(crate::shell::surfaces::DialogKind::Connections);
    app.surfaces
        .dlg_mut::<crate::shell::surfaces::ConnectionsDialog>()
        .info_detail = true;
    app.surfaces
        .dlg_mut::<crate::shell::surfaces::ConnectionsDialog>()
        .info_scroll = 4;
    app.surfaces
        .dlg_mut::<crate::shell::surfaces::ConnectionsDialog>()
        .scroll = 10;
    if let Some(mut ent) = app.surfaces.take_active_view() {
        let outcome = ent.handle_input(&crate::shell::input::InputAction::ScrollDown, &mut app, "");
        assert_eq!(outcome, crate::shell::surfaces::DialogOutcome::Consumed);
        app.surfaces.put_active_view(ent);
    }
    assert_eq!(
        app.surfaces
            .dlg_mut::<crate::shell::surfaces::ConnectionsDialog>()
            .info_scroll,
        5
    );
    assert_eq!(
        app.surfaces
            .dlg_mut::<crate::shell::surfaces::ConnectionsDialog>()
            .scroll,
        10
    );

    // 6. HelpDialog: pure-scroll dialog, ScrollDown increments and ScrollUp decrements.
    app.open_dialog(crate::shell::surfaces::DialogKind::Help);
    app.surfaces
        .dlg_mut::<crate::shell::surfaces::HelpDialog>()
        .scroll = 3;
    if let Some(mut ent) = app.surfaces.take_active_view() {
        let outcome = ent.handle_input(&crate::shell::input::InputAction::ScrollDown, &mut app, "");
        assert_eq!(outcome, crate::shell::surfaces::DialogOutcome::Consumed);
        app.surfaces.put_active_view(ent);
    }
    assert_eq!(
        app.surfaces
            .dlg_mut::<crate::shell::surfaces::HelpDialog>()
            .scroll,
        4
    );
    if let Some(mut ent) = app.surfaces.take_active_view() {
        let outcome = ent.handle_input(&crate::shell::input::InputAction::ScrollUp, &mut app, "");
        assert_eq!(outcome, crate::shell::surfaces::DialogOutcome::Consumed);
        app.surfaces.put_active_view(ent);
    }
    assert_eq!(
        app.surfaces
            .dlg_mut::<crate::shell::surfaces::HelpDialog>()
            .scroll,
        3
    );

    // 7. Under [INV-SURFACE-06], modal_scroll_field returns None for dialogs
    // (since they do not leak pointers), but continues to serve scenes/sheets.
    assert!(
        app.modal_scroll_field().is_none(),
        "dialogs do not use modal_scroll_field"
    );

    app.surfaces.dismiss_all_overlays();
    app.switch_scene(crate::shell::surfaces::SceneKind::Settings);
    let (s, f) = app.modal_scroll_field().expect("settings scene scrolls");
    assert!(f.is_none());
    *s = 7;
    assert_eq!(app.config_scroll, 7);

    app.reset_to_thread();
    assert!(app.modal_scroll_field().is_none());
}

#[test]
fn help_dialog_scroll_and_footer_click_lifecycle() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.open_dialog(crate::shell::surfaces::DialogKind::Help);

    // Initial scroll is 0
    assert_eq!(
        app.surfaces
            .dlg_mut::<crate::shell::surfaces::HelpDialog>()
            .scroll,
        0
    );

    // ModalDown (arrow down) scrolls down
    if let Some(mut ent) = app.surfaces.take_active_view() {
        let outcome = ent.handle_input(&crate::shell::input::InputAction::ModalDown, &mut app, "");
        assert_eq!(outcome, crate::shell::surfaces::DialogOutcome::Consumed);
        app.surfaces.put_active_view(ent);
    }
    assert_eq!(
        app.surfaces
            .dlg_mut::<crate::shell::surfaces::HelpDialog>()
            .scroll,
        1
    );

    // ModalUp (arrow up) scrolls up
    if let Some(mut ent) = app.surfaces.take_active_view() {
        let outcome = ent.handle_input(&crate::shell::input::InputAction::ModalUp, &mut app, "");
        assert_eq!(outcome, crate::shell::surfaces::DialogOutcome::Consumed);
        app.surfaces.put_active_view(ent);
    }
    assert_eq!(
        app.surfaces
            .dlg_mut::<crate::shell::surfaces::HelpDialog>()
            .scroll,
        0
    );

    // Page scrolling
    if let Some(mut ent) = app.surfaces.take_active_view() {
        let outcome = ent.handle_input(
            &crate::shell::input::InputAction::ScrollPageDown,
            &mut app,
            "",
        );
        assert_eq!(outcome, crate::shell::surfaces::DialogOutcome::Consumed);
        app.surfaces.put_active_view(ent);
    }
    assert!(
        app.surfaces
            .dlg_mut::<crate::shell::surfaces::HelpDialog>()
            .scroll
            > 0
    );

    // ScrollTop
    if let Some(mut ent) = app.surfaces.take_active_view() {
        let outcome = ent.handle_input(&crate::shell::input::InputAction::ScrollTop, &mut app, "");
        assert_eq!(outcome, crate::shell::surfaces::DialogOutcome::Consumed);
        app.surfaces.put_active_view(ent);
    }
    assert_eq!(
        app.surfaces
            .dlg_mut::<crate::shell::surfaces::HelpDialog>()
            .scroll,
        0
    );

    // Mount Help overlay on UI and test footer dismissal click
    let modal_rect = nuotc::layout::Rect::new(10, 5, 60, 20);
    app.ui.begin(nuotc::Rect::new(0, 0, 80, 24));
    app.ui.mount(
        crate::shell::ui::UiKey::Overlay(crate::shell::surfaces::OverlaySurface::Dialog(
            crate::shell::surfaces::DialogKind::Help,
        )),
        modal_rect,
    );
    app.ui.commit();
    let footer_y = modal_rect.y + modal_rect.height - 1;
    if let Some(mut ent) = app.surfaces.take_active_view() {
        let outcome = ent.handle_input(
            &crate::shell::input::InputAction::SelectionStart {
                x: modal_rect.x + 5,
                y: footer_y,
            },
            &mut app,
            "",
        );
        assert_eq!(outcome, crate::shell::surfaces::DialogOutcome::Dismiss);
        app.surfaces.put_active_view(ent);
    }

    // Populate HelpDialog.actions and test row-click to execute
    let entries = crate::shell::overlays::help::build_help_entries(
        crate::shell::surfaces::SceneKind::Thread,
        Some("session-1"),
        0,
        &app.key_overrides,
        &app.surface_overrides,
        &app.theme,
    );
    app.surfaces
        .dlg_mut::<crate::shell::surfaces::HelpDialog>()
        .actions = entries.iter().map(|e| e.action).collect();

    // Line 1 is Notices in Thread scene (body_top is y = 5 + 3 = 8, so line 1 is y = 8 + 1 = 9)
    if let Some(mut ent) = app.surfaces.take_active_view() {
        let outcome = ent.handle_input(
            &crate::shell::input::InputAction::SelectionStart {
                x: modal_rect.x + 5,
                y: 9,
            },
            &mut app,
            "",
        );
        assert_eq!(outcome, crate::shell::surfaces::DialogOutcome::Dismiss);
        app.surfaces.put_active_view(ent);
    }
    assert_eq!(
        app.pending_help_action,
        Some(crate::shell::overlays::help::HelpActionKind::Notices)
    );
}

/// ADR-0047 [INV-SURFACE-07]: DialogView entities autonomously handle spatial
/// click-to-select via handle_input(SelectionStart { x, y }).
#[test]
fn dialog_spatial_click_to_select_and_models_section_headers() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);

    app.provider_picker = ProviderPickerSnapshot {
        default_id: "openai".to_string(),
        rows: vec![nuo_protocol::ProviderPickerRow {
            id: "openai".to_string(),
            name: "OpenAI".to_string(),
            model: "gpt-5.5".to_string(),
            models: vec!["gpt-5.5".to_string(), "gpt-5-mini".to_string()],
            model_info: vec![
                nuo_protocol::ProviderModelInfo {
                    model: "gpt-5.5".to_string(),
                    protocol: "openai".to_string(),
                    ..Default::default()
                },
                nuo_protocol::ProviderModelInfo {
                    model: "gpt-5-mini".to_string(),
                    protocol: "openai".to_string(),
                    ..Default::default()
                },
            ],
            builtin: true,
            protocol: "openai".to_string(),
            base_url: String::new(),
            key_ready: true,
            provider: "openai".to_string(),
            headers: Default::default(),
            last_used_ms: Some(100),
            auth: Default::default(),
        }],
    };

    app.open_dialog(crate::shell::surfaces::DialogKind::Models);
    app.ui.begin(nuotc::Rect::new(0, 0, 80, 24));
    app.ui.mount(
        crate::shell::ui::UiKey::Overlay(crate::shell::surfaces::OverlaySurface::Dialog(
            crate::shell::surfaces::DialogKind::Models,
        )),
        nuotc::Rect::new(10, 5, 60, 15),
    );
    app.ui.commit();

    let models_dlg = app
        .surfaces
        .dlg_mut::<crate::shell::surfaces::ModelsDialog>();
    models_dlg.index = 0;
    models_dlg.scroll = 0;

    // Body starts at y = 5 + 3 = 8.
    // Line 0 is section header "All models", Line 1 is model row 0.
    if let Some(mut ent) = app.surfaces.take_active_view() {
        let action = crate::shell::input::InputAction::SelectionStart { x: 20, y: 9 };
        let outcome = ent.handle_input(&action, &mut app, "");
        assert_eq!(outcome, crate::shell::surfaces::DialogOutcome::Consumed);
        app.surfaces.put_active_view(ent);
    }
    assert_eq!(
        app.surfaces
            .dlg_mut::<crate::shell::surfaces::ModelsDialog>()
            .index,
        0
    );
    assert!(
        app.surfaces
            .dlg_mut::<crate::shell::surfaces::ModelsDialog>()
            .follow
    );
}

/// The page step follows the captured modal body height (when known) and
/// falls back to the transcript `view_height` before the first render. It must
/// always be at least 1 so a page key never no-ops on a zero capture.
#[test]
fn modal_page_step_tracks_body_height_and_floors_at_one() {
    use crate::shell::event_loop::modal_page_step;
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);

    // No body height captured yet → falls back to view_height, floored at 1.
    app.view_height = 0;
    app.modal_body_height = 0;
    assert_eq!(modal_page_step(&app), 1);

    // Transcript height known, modal not yet rendered → uses view_height - 1.
    app.view_height = 24;
    assert_eq!(modal_page_step(&app), 23);

    // Once the modal body height is captured, it wins over view_height so a
    // page advance matches the actual modal, not the transcript behind it.
    app.modal_body_height = 10;
    assert_eq!(
        modal_page_step(&app),
        9,
        "modal body height takes precedence"
    );

    // A 1-row modal body still yields a step of 1 (never 0).
    app.modal_body_height = 1;
    assert_eq!(modal_page_step(&app), 1);
}

/// `nuo attach` (no id) opens the sessions picker at startup instead of
/// loading any session, so the `startup_overlay` state must gate quit-on-close.
/// This pins the two state transitions the event loop relies on:
///
/// 1. The overlay defaults to `None` in an ordinary (in-session) App, so the
///    `/sessions` modal only ever dismisses on Esc.
/// 2. Selecting a session from the picker clears the overlay — once a real
///    thread backs the view, the picker reverts to a plain transient
///    overlay. (The event loop's `OpenSelectedSession` arm does this.)
#[test]
fn startup_picker_flag_governs_sessions_modal_quit_and_resets_on_open() {
    use std::sync::atomic::Ordering;

    let (mut app, _tmp) = app_in_tempdir(&[], &[]);

    // Default: an in-session App never treats the picker as a startup gate.
    assert_eq!(app.startup_overlay, crate::shell::StartupOverlay::None);

    // Simulate the startup path (`nuo attach` with no id): the picker
    // opens and `startup_overlay` is armed. Closing it must quit.
    app.startup_overlay = crate::shell::StartupOverlay::SessionsPicker;
    app.open_dialog(crate::shell::surfaces::DialogKind::Sessions);
    assert!(
        app.startup_overlay == crate::shell::StartupOverlay::SessionsPicker
            && app.active_dialog() == Some(crate::shell::surfaces::DialogKind::Sessions)
    );
    // The quit gate is `should_quit`; it is still clear until a close happens.
    assert!(!app.should_quit.load(Ordering::SeqCst));

    // Open a session from the picker: the overlay clears so a later `/sessions`
    // modal behaves as a normal transient overlay.
    app.startup_overlay = crate::shell::StartupOverlay::None;
    app.reset_to_thread();
    assert_eq!(
        app.startup_overlay,
        crate::shell::StartupOverlay::None,
        "opening a session drops the startup gate"
    );
}

/// `resolve_scroll` is the pure scroll-resolution core factored out of
/// `render_body`, now also used by the windowed sessions picker. It must keep
/// the selection in view (edge-margin follow), clamp to the valid range, and
/// report the true `max_scroll` of the full list — which is what lets the
/// picker build only the visible window while the scrollbar still reflects the
/// whole list. This pins those invariants.
#[test]
fn resolve_scroll_follows_selection_and_clamps_to_max_scroll() {
    use nuo_ui_primitives::primitives::{SCROLL_EDGE_MARGIN, resolve_scroll};

    // 100 rows, 10 visible → max_scroll is 90. A selection at row 50 with a
    // top-anchored scroll of 0 must pull the viewport down so row 50 is in
    // view (edge-margin follow), but never past max_scroll.
    let mut scroll = 0usize;
    let (start, max_scroll) = resolve_scroll(&mut scroll, 10, 100, Some(50), SCROLL_EDGE_MARGIN);
    assert_eq!(max_scroll, 90, "max_scroll reflects the full list length");
    assert!(
        (start..start + 10).contains(&50),
        "selection 50 must land inside the resolved window {start}..{}",
        start + 10
    );
    assert!(start <= 90, "resolved scroll never exceeds max_scroll");

    // Selection at the very end clamps to max_scroll (no overshoot).
    let mut scroll = 0usize;
    let (start, _) = resolve_scroll(&mut scroll, 10, 100, Some(99), SCROLL_EDGE_MARGIN);
    assert_eq!(start, 90, "end-of-list selection pins to max_scroll");

    // Fewer rows than the viewport: max_scroll is 0 and scroll collapses to 0.
    let mut scroll = 5usize;
    let (start, max_scroll) = resolve_scroll(&mut scroll, 10, 3, Some(1), SCROLL_EDGE_MARGIN);
    assert_eq!(
        max_scroll, 0,
        "content shorter than viewport has no scroll range"
    );
    assert_eq!(start, 0);

    // No follow: scroll is only clamped to max_scroll, never auto-scrolled.
    let mut scroll = 200usize;
    let (start, max_scroll) = resolve_scroll(&mut scroll, 10, 100, None, SCROLL_EDGE_MARGIN);
    assert_eq!(max_scroll, 90);
    assert_eq!(start, 90, "out-of-range scroll clamps to max_scroll");
}

/// The disclosure-toggle scroll settle: expanding a step must latch
/// `scroll_settle_pending` so the event loop stages its next frame (measure
/// the new height) before painting the toggle's target scroll offset. That
/// staging is what keeps the un-clamped intermediate viewport off the
/// terminal — the expand/collapse flicker.
#[test]
fn disclosure_toggle_latches_scroll_settle() {
    use nuo_view::model::document::TranscriptMessage;

    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    // The settle path only runs when the auto-scroll behavior is enabled
    // (`[tui] expand_auto_scroll`, default off).
    app.expand_auto_scroll = true;
    let mut messages = vec![
        TranscriptMessage::new(Role::User, "hi"),
        TranscriptMessage::tool_step("call_1", "read_text", r#"{"path":"README.md"}"#),
    ];

    // No toggle happened yet: nothing to settle.
    assert!(!app.scroll_settle_pending);

    // Expanding (collapsed by default) both flips the pin and latches the
    // settle request — the loop must not paint the expand's scroll target
    // against the pre-expand layout.
    assert!(app.toggle_step_pinned(&mut messages, 1));
    assert!(messages[1].tool_step_expanded() == Some(true));
    assert!(
        app.scroll_settle_pending,
        "expand must latch the settle request"
    );

    // Collapsing latches it too: the shrunk stream re-validates the offset.
    assert!(app.toggle_step_pinned(&mut messages, 1));
    assert!(messages[1].tool_step_expanded() == Some(false));
    assert!(
        app.scroll_settle_pending,
        "collapse must latch the settle request"
    );

    // The settle is one frame deep: once the loop has staged and settled the
    // frame, the latch clears (mirrored here the way the event loop consumes
    // it — a no-op toggle target keeps the latch off).
    app.scroll_settle_pending = false;

    // A toggle that resolves to nothing (index out of range) latches nothing
    // and leaves the messages untouched.
    let before = app.scroll_settle_pending;
    assert!(!app.toggle_step_pinned(&mut messages, 9));
    assert_eq!(app.scroll_settle_pending, before);
}

/// The default configuration (`[tui] expand_auto_scroll = false`, the
/// shipping default): a disclosure toggle is a pure read interaction. The
/// card flips its expansion, but the scroll offset and the follow/pin state
/// are left exactly as the user had them — the view never moves as a side
/// effect of a click, which is also what keeps any toggle from disturbing
/// an in-progress read.
#[test]
fn disclosure_toggle_disabled_by_default_leaves_scroll_untouched() {
    use nuo_view::model::document::TranscriptMessage;

    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    assert!(
        !app.expand_auto_scroll,
        "expand_auto_scroll defaults to disabled"
    );
    let mut messages = vec![
        TranscriptMessage::new(Role::User, "hi"),
        TranscriptMessage::tool_step("call_1", "read_text", r#"{"path":"README.md"}"#),
    ];

    app.scroll = 12;
    let scroll_before = app.scroll;

    // Expand: the pin flips, the scroll offset does not move. A settle frame
    // is still requested — not to scroll, but to re-validate the untouched
    // offset against the new height.
    assert!(app.toggle_step_pinned(&mut messages, 1));
    assert!(messages[1].tool_step_expanded() == Some(true));
    assert_eq!(app.scroll, scroll_before, "scroll must not move on expand");

    // Collapsing clears follow-bottom (reading history pauses auto-follow,
    // matching every other transcript interaction) but still leaves the
    // offset where the user had it.
    assert!(app.toggle_step_pinned(&mut messages, 1));
    assert!(messages[1].tool_step_expanded() == Some(false));
    assert!(!app.follow_bottom, "toggle pauses bottom-follow");
    assert_eq!(
        app.scroll, scroll_before,
        "scroll must not move on collapse"
    );
}

#[test]
fn sticky_collapse_anchors_summary_at_top_and_respects_viewport_underflow() {
    use nuo_protocol::Role;
    use nuo_view::model::document::TranscriptMessage;

    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let mut messages = vec![
        TranscriptMessage::new(Role::User, "initial question"),
        TranscriptMessage::tool_step("call_1", "read_text", r#"{"path":"README.md"}"#),
    ];

    // Force step expanded initially
    assert!(app.toggle_step_pinned(&mut messages, 1));
    assert_eq!(messages[1].tool_step_expanded(), Some(true));

    // Simulate sticky anchor active for step 1 at line 50
    app.sticky_step = Some(1);
    app.sticky_summary_line = Some(50);
    app.scroll = 100;
    app.follow_bottom = true;

    // Collapse sticky bar
    assert!(app.toggle_step_pinned(&mut messages, 1));
    assert_eq!(messages[1].tool_step_expanded(), Some(false));
    assert!(!app.follow_bottom, "collapsing sticky pauses follow_bottom");
    assert_eq!(app.scroll, 50, "scroll re-anchors to summary line 50");
    assert_eq!(
        app.pin_summary_line,
        Some(50),
        "pin_summary_line latches to preserve summary at Row 0"
    );

    // Case A: Total content is smaller than viewport (e.g. 10 lines in a 30-line viewport).
    // The entire conversation fits on screen, so scroll must collapse to 0.
    app.content_lines = 10;
    app.view_height = 30;
    app.max_scroll = 0;
    let limit_fit = if app.content_lines <= app.view_height as usize {
        0
    } else {
        app.pin_summary_line
            .map(|line| app.max_scroll.max(line.min(u16::MAX as usize) as u16))
            .unwrap_or(app.max_scroll)
    };
    assert_eq!(
        limit_fit, 0,
        "when entire content fits, limit must collapse to 0"
    );

    // Case B: Content overflows viewport (e.g. 60 lines in a 30-line viewport, max_scroll = 30).
    // Summary is at line 50. pin_summary_line allows scrolling up to 50 so summary stays at Row 0,
    // accepting bottom underflow (lines 51..60 at rows 1..10, rows 11..30 blank).
    app.content_lines = 60;
    app.view_height = 30;
    app.max_scroll = 30;
    let limit_overflow = if app.content_lines <= app.view_height as usize {
        0
    } else {
        app.pin_summary_line
            .map(|line| app.max_scroll.max(line.min(u16::MAX as usize) as u16))
            .unwrap_or(app.max_scroll)
    };
    assert_eq!(
        limit_overflow, 50,
        "when content overflows, limit preserves summary line at Row 0"
    );
}

#[test]
fn adopt_caret_head_and_tail_break_selection() {
    let mut app = app_with_input_selection("hello");
    // Park the visible caret somewhere stale — the adopt must override it,
    // proving the relay wins over the stale position.
    app.cursor_position = 1;

    assert!(app.adopt_caret_from_input_selection(SelectionEdge::Head));
    assert_eq!(app.cursor_position, 5, "head edge = buffer end");
    assert_eq!(app.selection, SelectionState::None, "selection must break");

    // Re-arm and adopt the tail edge.
    app.selection = SelectionState::Block {
        message_idx: crate::shell::render::INPUT_MSG_IDX,
        block_idx: 0,
    };
    assert!(app.adopt_caret_from_input_selection(SelectionEdge::Tail));
    assert_eq!(app.cursor_position, 0, "tail edge = buffer start");
    assert_eq!(app.selection, SelectionState::None);

    // Range selection: head is the release point, tail is the anchor point.
    app.selection = SelectionState::Range {
        anchor: nuo_view::model::layout::SemanticCursor::new(
            crate::shell::render::INPUT_MSG_IDX,
            0,
            1,
        ),
        head: nuo_view::model::layout::SemanticCursor::new(
            crate::shell::render::INPUT_MSG_IDX,
            0,
            4,
        ),
    };
    assert!(app.adopt_caret_from_input_selection(SelectionEdge::Head));
    assert_eq!(app.cursor_position, 4, "head edge adopts head cursor");
    assert_eq!(app.selection, SelectionState::None);

    // Backward drag: anchor is 4, head is 1 (mouse released at 1).
    app.selection = SelectionState::Range {
        anchor: nuo_view::model::layout::SemanticCursor::new(
            crate::shell::render::INPUT_MSG_IDX,
            0,
            4,
        ),
        head: nuo_view::model::layout::SemanticCursor::new(
            crate::shell::render::INPUT_MSG_IDX,
            0,
            1,
        ),
    };
    assert!(app.adopt_caret_from_input_selection(SelectionEdge::Head));
    assert_eq!(app.cursor_position, 1, "head edge adopts release position");
    assert_eq!(app.selection, SelectionState::None);

    // No selection → no-op, reports false.
    assert!(!app.adopt_caret_from_input_selection(SelectionEdge::Head));
}

#[test]
fn modal_over_dialog_dismiss_restores_underlying_dialog() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.open_dialog(crate::shell::surfaces::DialogKind::Sessions);
    app.modal_index = 2;

    // Opening Usage stats over the Sessions dialog
    app.open_dialog(crate::shell::surfaces::DialogKind::UsageStats);
    assert_eq!(
        app.active_dialog(),
        Some(crate::shell::surfaces::DialogKind::UsageStats)
    );
    assert_eq!(
        app.surfaces.underlying_dialog(),
        Some(crate::shell::surfaces::DialogKind::Sessions)
    );

    // Dismissing Usage stats restores the Sessions dialog with preserved index
    assert!(app.dismiss_surface());
    assert_eq!(
        app.active_dialog(),
        Some(crate::shell::surfaces::DialogKind::Sessions)
    );
    assert_eq!(app.modal_index, 2, "Sessions selection index is preserved");
}

#[test]
fn dialog_keys_sublayer_pop_and_deactivate_behavior() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.open_dialog(crate::shell::surfaces::DialogKind::Sessions);
    assert_eq!(
        app.active_dialog(),
        Some(crate::shell::surfaces::DialogKind::Sessions)
    );

    // Turn on dialog keys sub-view
    app.set_dialog_keys(true);
    *app.dialog_keys_scroll().expect("active dialog") = 5;

    // While dialog_keys is active, modal_scroll_field directs to dialog_keys_scroll
    {
        let (scroll, follow) = app.modal_scroll_field().expect("scroll field exists");
        assert_eq!(*scroll, 5);
        assert!(follow.is_none());
        *scroll += 1;
    }
    assert_eq!(*app.dialog_keys_scroll().expect("active dialog"), 6);

    // Popping sublayer dismisses dialog_keys but leaves the dialog active
    assert!(app.pop_sublayer(), "dialog_keys sublayer was popped");
    assert!(!app.dialog_keys(), "dialog_keys is now false");
    assert_eq!(
        *app.dialog_keys_scroll().expect("active dialog"),
        0,
        "dialog_keys_scroll was reset"
    );
    assert_eq!(
        app.active_dialog(),
        Some(crate::shell::surfaces::DialogKind::Sessions),
        "parent dialog remains open"
    );

    // Turning on dialog_keys again, then dismissing the dialog resets dialog_keys
    app.set_dialog_keys(true);
    *app.dialog_keys_scroll().expect("active dialog") = 3;
    assert!(app.dismiss_active_dialog(), "dismissed active dialog");
    assert!(!app.dialog_keys(), "dialog_keys reset on dialog deactivate");
}

#[test]
fn modal_arrows_route_through_the_dialog_entity() {
    // `[INV-SURFACE-01]` / ADR-0035 §1: the generic ↑/↓ arrows are handled by
    // the active dialog entity's `DialogView::handle_input`, mutating only that
    // entity's own state.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.open_dialog(crate::shell::surfaces::DialogKind::ThreadStats);
    app.surfaces
        .dlg_mut::<crate::shell::surfaces::ThreadStatsDialog>()
        .scroll = 5;

    crate::shell::event_loop::actions::handle_modal_up(&mut app, "test-session");
    assert_eq!(
        app.surfaces
            .dlg_mut::<crate::shell::surfaces::ThreadStatsDialog>()
            .scroll,
        4,
        "arrow handled by the thread stats entity"
    );
    crate::shell::event_loop::actions::handle_modal_down(&mut app, "test-session");
    assert_eq!(
        app.surfaces
            .dlg_mut::<crate::shell::surfaces::ThreadStatsDialog>()
            .scroll,
        5
    );
}

#[test]
fn dialog_reads_see_the_entity_while_it_is_rendered() {
    // Regression: rendering takes the entity out of the registry; App methods
    // that read the dialog's own query (`picker_query`, `history_rows`) must
    // still see that query, not a blank placeholder.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.open_dialog(crate::shell::surfaces::DialogKind::Models);
    app.surfaces
        .dlg_mut::<crate::shell::surfaces::ModelsDialog>()
        .search = true;
    app.surfaces
        .dlg_mut::<crate::shell::surfaces::ModelsDialog>()
        .query
        .text = "claude".to_string();

    let taken = app.surfaces.take_active_view().expect("models is open");
    assert_eq!(
        app.picker_query(),
        "claude",
        "read sees the entity's query during render"
    );
    app.surfaces.put_active_view(taken);
    assert_eq!(app.picker_query(), "claude");
}
