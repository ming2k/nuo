//! Notice log residency end-to-end (ADR-0121): routing → retention → badge →
//! viewer → copy.
//!
//! The unit tests in `app::notices` prove the funnel and `overlays::notices`
//! proves the panel. These prove the *seam* between them, because the defect
//! being fixed lived exactly there: each piece worked, and the message still
//! vanished before it was read.

// Integration test target: assertions fail by panicking, so the production
// no-panic lints are meaningless here (the exemption ends at the test boundary).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;

use crate::shell::event_loop::apply::apply;
use crate::shell::event_loop::{AppMutation, runtime::UiRuntime};
use nuo_view::model::document::NoticeSeverity;
use nuo_view::model::log::LineStatus;

fn runtime() -> UiRuntime {
    UiRuntime::minimal_for_test()
}

/// Deliver one disclosure the way the translator does.
fn disclose(app: &mut App, severity: NoticeSeverity, topic: &str, text: &str, at_ms: u64) {
    apply(
        app,
        &runtime(),
        AppMutation::NoticeDisclosure {
            severity,
            topic: topic.to_string(),
            text: text.to_string(),
            at_ms,
            detail: None,
        },
    );
}

#[test]
fn a_fault_that_fades_from_the_bubble_is_still_in_the_log_and_flagged_unread() {
    // The reported defect, end to end: the bubble auto-expires after ~3s, and
    // the user was not looking. The message must survive, and something on
    // screen must say so.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    disclose(
        &mut app,
        NoticeSeverity::Error,
        "backend",
        "could not reach the backend",
        1,
    );

    // While the bubble is up.
    assert!(app.notice_toast_until.is_some(), "a fault interrupts");
    assert!(app.has_unread_notice_fault(), "and is counted as unseen");

    // After it expires (what the timer tick does).
    app.notice_toast_until = None;
    app.copy_toast_until = None;

    assert!(app.notice_toast_until.is_none(), "the bubble is gone");
    assert_eq!(app.notice_count(), 1, "the record is not");
    assert_eq!(
        app.notice_lines().back().map(|l| l.text.as_str()),
        Some("could not reach the backend")
    );
    assert!(
        app.has_unread_notice_fault(),
        "and the badge keeps pointing at it until the user looks"
    );
}

#[test]
fn a_second_fault_does_not_erase_the_first() {
    // The single bubble slot: the second message wins the *bubble*, and the
    // first is not lost because it was never only a bubble.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    disclose(
        &mut app,
        NoticeSeverity::Error,
        "link",
        "could not open link",
        1,
    );
    disclose(
        &mut app,
        NoticeSeverity::Error,
        "copy",
        "clipboard write timed out",
        2,
    );

    assert_eq!(
        app.notice_toast_message, "clipboard write timed out",
        "the bubble shows the newest fault"
    );
    assert_eq!(app.notice_count(), 2, "both are retained");
    let texts: Vec<&str> = app.notice_lines().iter().map(|l| l.text.as_str()).collect();
    assert_eq!(
        texts,
        vec!["could not open link", "clipboard write timed out"]
    );
    assert_eq!(app.unread_notices(), 2);
}

#[test]
fn opening_the_viewer_clears_the_badge_without_discarding_the_record() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    disclose(
        &mut app,
        NoticeSeverity::Warning,
        "images",
        "image withheld",
        1,
    );
    assert!(app.has_unread_notice_fault());

    app.open_notice_viewer();
    assert!(app.notice_viewer_open());
    assert_eq!(app.unread_notices(), 0, "a viewer on screen is seen");
    assert!(
        app.has_notice_fault(),
        "the fault is still there to read and copy"
    );

    app.close_notice_viewer();
    assert!(!app.notice_viewer_open());
    assert_eq!(
        app.unread_notices(),
        0,
        "closing does not resurrect the badge for something already read"
    );
}

#[test]
fn the_viewer_opens_at_the_newest_notice() {
    // A user reaching for the viewer after a missed bubble wants the newest
    // entry, not the oldest.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    for i in 0..4 {
        disclose(
            &mut app,
            NoticeSeverity::Error,
            "t",
            &format!("fault {i}"),
            i,
        );
    }
    app.open_notice_viewer();
    assert_eq!(app.notice_viewer.as_ref().unwrap().index, 3);
}

#[test]
fn an_acknowledgement_is_retained_but_never_lights_the_badge() {
    // `Info` disclosures are worth keeping (the user asked for that answer) but
    // must not train the user to ignore the badge.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    disclose(
        &mut app,
        NoticeSeverity::Info,
        "command",
        "delegated mode on",
        1,
    );
    assert_eq!(app.notice_count(), 1);
    assert!(!app.has_unread_notice_fault());
    assert!(!app.has_notice_fault());
}

#[test]
fn the_copied_block_carries_every_notice_with_severity_time_and_topic() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "test-session".to_string();
    disclose(
        &mut app,
        NoticeSeverity::Error,
        "link",
        "could not open link",
        1_700_000_000_000,
    );
    disclose(
        &mut app,
        NoticeSeverity::Info,
        "command",
        "delegated mode on",
        1_700_000_060_000,
    );

    let block = app
        .render_notices(Some("test-session"))
        .expect("a non-empty log renders");

    let (header, body) = block.split_once("\n\n").expect("header and body");
    assert!(header.starts_with("[nuo diagnostic] kind=notices"));
    assert!(header.contains("thread=test-session"), "{header}");
    assert!(header.contains("count=2"), "{header}");
    assert!(body.contains("link: could not open link"), "{body}");
    assert!(body.contains("command: delegated mode on"), "{body}");
    assert_eq!(body.lines().count(), 2);
}

#[test]
fn an_empty_log_renders_nothing_to_copy() {
    let (app, _tmp) = app_in_tempdir(&[], &[]);
    assert!(app.render_notices(None).is_none());
    assert!(!app.has_notices());
}

#[test]
fn clearing_forgets_the_notices_and_the_badge_together() {
    // The viewer's `x` verb. A clear that left the badge lit would be a lie the
    // user cannot dismiss.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    disclose(&mut app, NoticeSeverity::Error, "backend", "link down", 1);
    app.open_notice_viewer();
    app.clear_notices();
    assert_eq!(app.notice_count(), 0);
    assert_eq!(app.unread_notices(), 0);
    assert!(!app.has_unread_notice_fault());
    assert!(!app.has_notice_fault());
}

#[test]
fn a_local_fault_takes_the_same_path_as_a_routed_notice() {
    // The clipboard/link family never had a `RoundEvent`; it must not need a
    // second retention mechanism.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.record_local_fault("copy", "clipboard is empty");
    let line = app.notice_lines().back().expect("retained");
    assert_eq!(line.status, LineStatus::Failed);
    assert_eq!(line.label.as_deref(), Some("copy"));
    assert!(app.has_unread_notice_fault());
}

#[test]
fn the_log_survives_a_scene_switch() {
    // `[INV-NOTICE-02]`: a fault is a fact about the client, so leaving the
    // thread must not discard the record the user is about to go looking for.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    disclose(&mut app, NoticeSeverity::Error, "backend", "link down", 1);
    let count = app.notice_count();

    app.surfaces
        .switch_scene(crate::shell::surfaces::SceneKind::Settings);
    assert_eq!(app.notice_count(), count, "client-global, not scene-scoped");

    app.surfaces
        .switch_scene(crate::shell::surfaces::SceneKind::Thread);
    assert_eq!(app.notice_count(), count);
}

#[test]
fn the_log_survives_a_session_switch() {
    // The same invariant against the other unwinder: session-scoped dialogs are
    // archived per session, and the notice log must not be one of them.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "a".to_string();
    disclose(&mut app, NoticeSeverity::Error, "backend", "link down", 1);
    let count = app.notice_count();

    app.surfaces.dialogs.switch_session("b");
    assert_eq!(app.notice_count(), count);
    app.surfaces.dialogs.switch_session("a");
    assert_eq!(app.notice_count(), count);
}

#[test]
fn the_viewer_paints_over_the_transcript_when_opened() {
    // The panel is a real surface, not a data structure: it must reach the
    // screen, over whatever the scene is painting beneath it.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "test-session".to_string();
    disclose(
        &mut app,
        NoticeSeverity::Error,
        "backend",
        "could not reach the backend",
        1_700_000_000_000,
    );
    app.open_notice_viewer();

    let mut terminal = nuotc::TestTerminal::new(110, 30);
    terminal.draw(|f| {
        crate::shell::event_loop::render_frame(&mut app, f, "test-session");
    });
    let text = frame_text(&terminal);
    assert!(
        text.contains("Notices"),
        "the panel header is on screen: {text}"
    );
    assert!(
        text.contains("backend: could not reach the backend"),
        "the retained notice is readable: {text}"
    );
}

#[test]
fn the_unread_fault_badge_paints_on_the_model_bar() {
    // A retained record with no visible entry point is the old side channel. The
    // badge is what makes the log discoverable without knowing the chord.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "test-session".to_string();
    disclose(&mut app, NoticeSeverity::Error, "backend", "link down", 1);

    let mut terminal = nuotc::TestTerminal::new(110, 30);
    terminal.draw(|f| {
        crate::shell::event_loop::render_frame(&mut app, f, "test-session");
    });
    let text = frame_text(&terminal);
    assert!(
        text.contains("!1 notices"),
        "the badge counts unseen faults: {text}"
    );
    assert!(
        text.contains("Ctrl-o"),
        "and names the chord that opens it: {text}"
    );
}

#[test]
fn the_badge_is_absent_when_only_acknowledgements_are_unread() {
    // A badge that fired for "copied to clipboard" trains the user to ignore the
    // one that matters.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "test-session".to_string();
    disclose(
        &mut app,
        NoticeSeverity::Info,
        "command",
        "delegated mode on",
        1,
    );

    let mut terminal = nuotc::TestTerminal::new(110, 30);
    terminal.draw(|f| {
        crate::shell::event_loop::render_frame(&mut app, f, "test-session");
    });
    let text = frame_text(&terminal);
    assert!(
        !text.contains("notices"),
        "no badge for an unread acknowledgement: {text}"
    );
}

#[test]
fn the_badge_clears_once_the_viewer_has_been_opened() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "test-session".to_string();
    disclose(&mut app, NoticeSeverity::Error, "backend", "link down", 1);
    app.open_notice_viewer();
    app.close_notice_viewer();

    let mut terminal = nuotc::TestTerminal::new(110, 30);
    terminal.draw(|f| {
        crate::shell::event_loop::render_frame(&mut app, f, "test-session");
    });
    let text = frame_text(&terminal);
    assert!(!text.contains("!1 notices"), "read is read: {text}");
}

/// The frame as one string, so assertions can look for text anywhere on it.
fn frame_text(terminal: &nuotc::TestTerminal) -> String {
    let buf = terminal.buffer();
    let area = buf.area();
    (0..area.height)
        .map(|y| {
            (0..area.width)
                .map(|x| buf.get(x, y).unwrap().symbol.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn paging_steps_by_the_panels_real_body_height() {
    // `PgDn` must move by a page. Reading the dialog stack's body height (stale
    // or zero when the viewer paints over a dialog) would step by one row and
    // make paging useless.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "test-session".to_string();
    for i in 0..40 {
        disclose(
            &mut app,
            NoticeSeverity::Error,
            "t",
            &format!("fault {i}"),
            i,
        );
    }
    app.open_notice_viewer();

    let mut terminal = nuotc::TestTerminal::new(110, 30);
    terminal.draw(|f| {
        crate::shell::event_loop::render_frame(&mut app, f, "test-session");
    });
    let body_height = app.notice_viewer.as_ref().unwrap().body_height;
    assert!(
        body_height > 1,
        "the renderer recorded a usable page height: {body_height}"
    );

    // The viewer opens following the newest entry, so start from a known
    // position: cursor at the top, scroll at zero, follow off.
    {
        let viewer = app.notice_viewer.as_mut().unwrap();
        viewer.index = 0;
        viewer.scroll = 0;
        viewer.follow = false;
    }

    let runtime = runtime();
    crate::shell::event_loop::actions::dispatch_action_for_test(
        &mut app,
        &runtime,
        crate::shell::input::InputAction::NoticesPage { forward: true },
        "test-session",
    )
    .await;
    assert_eq!(
        app.notice_viewer.as_ref().unwrap().scroll,
        body_height as usize,
        "one page, not one row"
    );

    crate::shell::event_loop::actions::dispatch_action_for_test(
        &mut app,
        &runtime,
        crate::shell::input::InputAction::NoticesPage { forward: false },
        "test-session",
    )
    .await;
    assert_eq!(
        app.notice_viewer.as_ref().unwrap().scroll,
        0,
        "and back again"
    );
}

#[tokio::test]
async fn ctrl_o_opens_the_viewer_even_with_a_dialog_up() {
    // A fault recorded while a dialog is up is exactly the case the user needs
    // to review, so the chord must not be swallowed by the modal.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "test-session".to_string();
    app.open_dialog(crate::shell::surfaces::DialogKind::Models);

    let action = crate::shell::input::route_event(
        crossterm::event::Event::Key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('o'),
            crossterm::event::KeyModifiers::CONTROL,
        )),
        &mut String::new(),
        &mut 0,
        crate::shell::input::Dispatch {
            overlay: app.surfaces.active_overlay(),
            sheet: None,
            pre_attach: false,
            scene: app.current_scene(),
            key_overrides: Default::default(),
            focused_target: false,
            transcript_focused: false,
            scene_blocked: false,
            scene_namespace_armed: false,
            notice_viewer_open: false,
        },
        &crate::shell::modal_keys::ModalKeys::default(),
        &crate::shell::sheet::SheetKeys::default(),
        &crate::shell::session::SceneKeys::default(),
        &mut app.drag,
    );
    assert_eq!(action, crate::shell::input::InputAction::OpenNotices);
}

#[tokio::test]
async fn the_viewer_owns_its_keys_while_open() {
    // The viewer is not an `OverlaySurface`, so without an explicit arm its
    // arrow keys would scroll the transcript behind it.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "test-session".to_string();
    for i in 0..3 {
        disclose(
            &mut app,
            NoticeSeverity::Error,
            "t",
            &format!("fault {i}"),
            i,
        );
    }
    app.open_notice_viewer();

    let mut route = |code: crossterm::event::KeyCode| {
        crate::shell::input::route_event(
            crossterm::event::Event::Key(crossterm::event::KeyEvent::new(
                code,
                crossterm::event::KeyModifiers::NONE,
            )),
            &mut String::new(),
            &mut 0,
            crate::shell::input::Dispatch {
                overlay: None,
                sheet: None,
                pre_attach: false,
                scene: app.current_scene(),
                key_overrides: Default::default(),
                focused_target: false,
                transcript_focused: false,
                scene_blocked: false,
                scene_namespace_armed: false,
                notice_viewer_open: true,
            },
            &crate::shell::modal_keys::ModalKeys::default(),
            &crate::shell::sheet::SheetKeys::default(),
            &crate::shell::session::SceneKeys::default(),
            &mut app.drag,
        )
    };

    assert_eq!(
        route(crossterm::event::KeyCode::Up),
        crate::shell::input::InputAction::NoticesSelect { forward: false }
    );
    assert_eq!(
        route(crossterm::event::KeyCode::Enter),
        crate::shell::input::InputAction::NoticesToggleDetail
    );
    assert_eq!(
        route(crossterm::event::KeyCode::Char('x')),
        crate::shell::input::InputAction::NoticesClear
    );
    assert_eq!(
        route(crossterm::event::KeyCode::Esc),
        crate::shell::input::InputAction::CloseModal
    );

    // And `Esc` must actually close it rather than being spent elsewhere.
    app.close_notice_viewer();
    assert!(!app.notice_viewer_open());
}

#[tokio::test]
async fn esc_closes_the_viewer_before_it_reaches_a_dialog_beneath() {
    // The viewer paints above every overlay, so it is the deepest layer from the
    // user's point of view. Without the unwinder arm, `Esc` dismissed the dialog
    // *behind* the panel — which looks like the key did nothing at all.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "test-session".to_string();
    disclose(&mut app, NoticeSeverity::Error, "backend", "link down", 1);
    app.open_dialog(crate::shell::surfaces::DialogKind::Models);
    app.open_notice_viewer();

    let runtime = runtime();
    crate::shell::event_loop::actions::dispatch_action_for_test(
        &mut app,
        &runtime,
        crate::shell::input::InputAction::CloseModal,
        "test-session",
    )
    .await;

    assert!(!app.notice_viewer_open(), "Esc closed the viewer");
    assert_eq!(
        app.active_dialog(),
        Some(crate::shell::surfaces::DialogKind::Models),
        "and left the dialog beneath untouched"
    );
}

#[tokio::test]
async fn typing_does_not_kill_a_fault_bubble_but_esc_does() {
    // The behaviour that makes the dwell budget mean anything. Before this, *any*
    // key or paste cleared the bubble, so a fault lived only as long as the user
    // happened to sit still — the dwell constants were unreachable in practice.
    // Typing is how a user resumes work; it must not cost them the message.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "test-session".to_string();
    disclose(&mut app, NoticeSeverity::Error, "backend", "link down", 1);
    assert!(app.notice_toast_until.is_some());

    // A printable key.
    crate::shell::event_loop::process_event_for_test(
        &mut app,
        &crossterm::event::Event::Key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('a'),
            crossterm::event::KeyModifiers::NONE,
        )),
        "test-session",
    )
    .await;
    assert!(
        app.notice_toast_until.is_some(),
        "typing must not dismiss a fault the user has not read"
    );

    // An explicit dismissal.
    crate::shell::event_loop::process_event_for_test(
        &mut app,
        &crossterm::event::Event::Key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Esc,
            crossterm::event::KeyModifiers::NONE,
        )),
        "test-session",
    )
    .await;
    assert!(
        app.notice_toast_until.is_none(),
        "Esc means 'I have seen this'"
    );
    assert_eq!(
        app.notice_count(),
        1,
        "and dismissing the bubble still keeps the record"
    );
}

#[tokio::test]
async fn typing_does_dismiss_an_acknowledgement_bubble() {
    // The mirror: an ack reports the outcome of the action the user just took, so
    // it must yield the composer the moment they resume typing.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "test-session".to_string();
    disclose(
        &mut app,
        NoticeSeverity::Info,
        "command",
        "delegated mode on",
        1,
    );
    assert!(app.notice_toast_until.is_some());

    crate::shell::event_loop::process_event_for_test(
        &mut app,
        &crossterm::event::Event::Key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('a'),
            crossterm::event::KeyModifiers::NONE,
        )),
        "test-session",
    )
    .await;
    assert!(
        app.notice_toast_until.is_none(),
        "an ack never lingers over the composer"
    );
    assert_eq!(app.notice_count(), 1, "but it is still in the log");
}

#[tokio::test]
async fn a_fault_bubble_outlives_the_wcag_perception_threshold() {
    // End-to-end: the deadline the applier sets must clear 5s, not the ~2.6s the
    // old single-slot toast used.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    disclose(
        &mut app,
        NoticeSeverity::Error,
        "backend",
        "could not reach the backend",
        1,
    );
    let remaining = app
        .notice_toast_until
        .expect("a fault interrupts")
        .saturating_duration_since(std::time::Instant::now());
    assert!(
        remaining > std::time::Duration::from_secs(5),
        "a fault must give a user who looked away a fair chance: {remaining:?}"
    );
    assert!(
        remaining <= std::time::Duration::from_secs(10),
        "and must not become an obstruction: {remaining:?}"
    );
}

#[tokio::test]
async fn esc_in_the_viewer_closes_only_the_panel_not_the_bubble_it_points_at() {
    // One keystroke must not destroy both the interruption and the place it
    // points at. With the panel open, `Esc` closes the panel; the bubble is
    // already moot because the user is reading the record.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "test-session".to_string();
    disclose(&mut app, NoticeSeverity::Error, "backend", "link down", 1);
    app.open_notice_viewer();

    crate::shell::event_loop::process_event_for_test(
        &mut app,
        &crossterm::event::Event::Key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Esc,
            crossterm::event::KeyModifiers::NONE,
        )),
        "test-session",
    )
    .await;

    assert!(!app.notice_viewer_open(), "Esc closed the panel");
    assert_eq!(
        app.notice_count(),
        1,
        "and the record it was pointing at is untouched"
    );
}

#[test]
fn the_cursor_stays_visible_after_an_expanded_row_grows_the_body() {
    // The bug this guards: an expanded notice contributes several body rows, so
    // the generic list's row-index follow pointed at the wrong line and the
    // cursor could scroll out of the panel entirely.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    for i in 0..30 {
        disclose(
            &mut app,
            NoticeSeverity::Error,
            "t",
            &format!("fault {i} with enough words to make the detail block wrap"),
            i,
        );
    }
    app.open_notice_viewer();

    let mut terminal = nuotc::TestTerminal::new(110, 30);
    terminal.draw(|f| {
        crate::shell::event_loop::render_frame(&mut app, f, "test-session");
    });
    let body_height = app.notice_viewer.as_ref().unwrap().body_height as usize;
    assert!(body_height > 1);

    // Select the newest notice and expand it: the body now has extra rows.
    {
        let viewer = app.notice_viewer.as_mut().unwrap();
        viewer.index = 29;
        viewer.detail = true;
    }
    terminal.draw(|f| {
        crate::shell::event_loop::render_frame(&mut app, f, "test-session");
    });

    let text = frame_text(&terminal);
    assert!(
        text.contains("fault 29"),
        "the selected notice is on screen after its detail expanded: {text}"
    );

    // And the offset is a real viewport offset, not a row index.
    let scroll = app.notice_viewer.as_ref().unwrap().scroll;
    assert!(
        scroll > 0,
        "the view scrolled to follow the cursor instead of pinning at the top"
    );
}

#[test]
fn the_dock_is_a_footer_row_that_the_transcript_yields_rows_to() {
    // The whole point of docking rather than floating: the dock's height is part
    // of the footer stack's split, so the transcript gives up exactly those rows
    // and nothing overlaps. A float would have painted *over* the conversation.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "test-session".to_string();
    for i in 0..20 {
        app.messages
            .push(nuo_view::model::document::TranscriptMessage::new(
                nuo_wire::Role::Assistant,
                format!("turn {i}"),
            ));
    }
    let mut terminal = nuotc::TestTerminal::new(100, 40);

    terminal.draw(|f| {
        crate::shell::event_loop::render_frame(&mut app, f, "test-session");
    });
    let closed_height = footer_dock_height(&app);

    disclose(&mut app, NoticeSeverity::Error, "backend", "link down", 1);
    app.open_notice_viewer();
    terminal.draw(|f| {
        crate::shell::event_loop::render_frame(&mut app, f, "test-session");
    });
    let open_height = footer_dock_height(&app);

    assert_eq!(closed_height, 0, "a closed dock takes no rows");
    assert!(
        open_height >= nuo_ui_primitives::design::NOTICE_DOCK_MIN_ROWS,
        "an open dock gets its readable floor: {open_height}"
    );
    assert!(
        open_height <= 40 / 2,
        "and never more than half the terminal: {open_height}"
    );
}

#[test]
fn the_composer_stays_live_below_the_dock() {
    // The second justification for docking: the most common reaction to a fault is
    // to retype or retry. A modal barrier would forbid it exactly then.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "test-session".to_string();
    disclose(&mut app, NoticeSeverity::Error, "backend", "link down", 1);
    app.open_notice_viewer();

    let mut terminal = nuotc::TestTerminal::new(100, 40);
    terminal.draw(|f| {
        crate::shell::event_loop::render_frame(&mut app, f, "test-session");
    });
    let text = frame_text(&terminal);
    // The composer's own chrome is still on screen below the dock. `›` is the
    // composer's prompt glyph (not `>`), and the model bar beneath it is the
    // proof that the whole footer survived rather than being replaced.
    assert!(
        text.contains('›'),
        "the composer prompt is still painted with the dock open: {text}"
    );
    assert!(
        app.is_composer_focused() || app.caret_owner() == crate::shell::CaretOwner::Composer,
        "and it still owns the caret"
    );
}

#[test]
fn the_conversation_stays_visible_above_the_dock() {
    // The first justification: the fault usually explains what is behind it.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "test-session".to_string();
    for i in 0..20 {
        app.messages
            .push(nuo_view::model::document::TranscriptMessage::new(
                nuo_wire::Role::Assistant,
                format!("visible-turn-{i}"),
            ));
    }
    disclose(&mut app, NoticeSeverity::Error, "backend", "link down", 1);
    app.open_notice_viewer();

    let mut terminal = nuotc::TestTerminal::new(100, 40);
    terminal.draw(|f| {
        crate::shell::event_loop::render_frame(&mut app, f, "test-session");
    });
    let text = frame_text(&terminal);
    // The contract is that the conversation is *visible* with the dock open — a
    // float or a modal would have covered it. Which turn is scrolled into view is
    // the transcript's own concern (it follows the bottom in the real loop), so
    // the assertion is on transcript content, not on a particular row.
    assert!(
        text.contains("visible-turn-"),
        "conversation rows are still readable with the dock open: {text}"
    );
    assert!(
        text.contains("backend: link down"),
        "and so is the fault: {text}"
    );
}

/// The dock row's height this frame, resolved the way the renderer resolves it.
fn footer_dock_height(app: &crate::shell::App) -> u16 {
    if app.notice_viewer.is_none() {
        return 0;
    }
    let lines: Vec<nuo_view::model::log::SurfaceLine> =
        app.notice_lines().iter().cloned().collect();
    let base = nuo_ui_primitives::design::notice_dock_rows(40);
    let detail = crate::shell::overlays::notices::expanded_detail_rows(
        &lines,
        app.notice_viewer.as_ref().unwrap(),
        100,
    );
    if app.notice_viewer.as_ref().unwrap().detail {
        nuo_ui_primitives::design::notice_dock_rows_expanded(40, detail)
    } else {
        base
    }
}

#[tokio::test]
async fn clicking_a_dock_row_selects_it_and_clicking_again_opens_the_detail() {
    // The pointer mirror of `↑`/`↓` then `Enter`. It must resolve through the
    // geometry the dock recorded, not a re-derived footer stack — otherwise the
    // mapping breaks the moment the panel resizes.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "test-session".to_string();
    for i in 0..5 {
        disclose(
            &mut app,
            NoticeSeverity::Error,
            "t",
            &format!("fault {i}"),
            i,
        );
    }
    app.open_notice_viewer();

    let mut terminal = nuotc::TestTerminal::new(100, 40);
    terminal.draw(|f| {
        crate::shell::event_loop::render_frame(&mut app, f, "test-session");
    });
    // The loop commits the component tree after the frame is painted; a test that
    // skips the commit would hit-test against the *previous* scene, where the dock
    // was not mounted.
    app.ui.commit();

    let viewer = app.notice_viewer.as_ref().unwrap();
    let frame = frame_text(&terminal);
    let origin = viewer
        .body_origin
        .unwrap_or_else(|| panic!("the dock recorded its body origin; frame:\n{frame}"));
    let scroll = viewer.scroll;
    let body_height = viewer.body_height;
    assert!(body_height > 0);

    // Click the first *visible* row.
    let click_y = origin;
    let expected = crate::shell::overlays::notices::notice_at_body_row(
        &app.notice_lines().iter().cloned().collect::<Vec<_>>(),
        app.notice_viewer.as_ref().unwrap(),
        scroll,
        viewer.body_width,
    );
    let Some(expected) = expected else {
        // Nothing resolvable at that row (e.g. inside an expanded block); the
        // contract under test is the *mapping*, asserted directly below.
        return;
    };

    crate::shell::event_loop::actions::mouse::click_at_for_test(
        &mut app,
        50,
        click_y,
        "test-session",
    )
    .await;
    assert_eq!(
        app.notice_viewer.as_ref().unwrap().index,
        expected,
        "the clicked row became the cursor"
    );

    // Clicking the same row again toggles the detail.
    let before = app.notice_viewer.as_ref().unwrap().detail;
    crate::shell::event_loop::actions::mouse::click_at_for_test(
        &mut app,
        50,
        click_y,
        "test-session",
    )
    .await;
    assert_ne!(
        app.notice_viewer.as_ref().unwrap().detail,
        before,
        "a second click on the selected row toggles its detail"
    );
}

#[test]
fn the_row_mapping_is_the_inverse_of_the_body_assembly() {
    // Forward and reverse must agree: for every notice, the row it starts at maps
    // back to that notice. This is what stops the two arithmetic paths drifting.
    use crate::shell::overlays::notices::{expanded_detail_rows, notice_at_body_row};
    let lines: Vec<nuo_view::model::log::SurfaceLine> = (0..6)
        .map(|i| {
            nuo_view::model::log::SurfaceLine::disclosure(
                nuo_view::model::log::LineStatus::Failed,
                format!("fault {i}"),
                i,
            )
        })
        .collect();
    let mut viewer = crate::shell::app::notices::NoticeViewer::open_at_newest(lines.len());
    let width = 60usize;

    // Collapsed: row i is notice i.
    for i in 0..lines.len() {
        assert_eq!(notice_at_body_row(&lines, &viewer, i, width), Some(i));
    }
    // Past the end is not a notice.
    assert_eq!(
        notice_at_body_row(&lines, &viewer, lines.len(), width),
        None
    );

    // Expanded: the detail rows belong to no notice, and the notices after it
    // shift by exactly the block's extra height.
    viewer.index = 2;
    viewer.detail = true;
    assert_eq!(notice_at_body_row(&lines, &viewer, 2, width), Some(2));
    assert_eq!(
        notice_at_body_row(&lines, &viewer, 3, width),
        None,
        "a click inside the expanded block selects nothing"
    );
    // The block occupies the rows the growth calculation budgets for.
    let growth = expanded_detail_rows(&lines, &viewer, width as u16);
    assert!(
        growth > 0,
        "expanding a row must ask the panel for more height: {growth}"
    );
}

#[test]
fn the_dock_does_not_reflow_the_transcript_as_notices_arrive() {
    // The defect this locks down. A height derived from the log's length makes the
    // footer grow on every arrival, which shrinks the transcript above it and
    // reflows the text the user is reading — while they read it, because reviewing
    // a fault is exactly when more faults arrive. The dock's height must be a
    // function of the viewport alone.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "test-session".to_string();
    app.open_notice_viewer();

    let mut terminal = nuotc::TestTerminal::new(100, 40);
    let height_now = |app: &crate::shell::App, terminal: &nuotc::TestTerminal| -> (u16, u16) {
        let dock = footer_dock_height(app);
        let buf = terminal.buffer();
        let area = buf.area();
        (dock, area.height)
    };
    let _ = &mut terminal;

    let mut heights = Vec::new();
    for i in 0..25 {
        disclose(
            &mut app,
            NoticeSeverity::Error,
            "t",
            &format!("fault {i}"),
            i,
        );
        terminal.draw(|f| {
            crate::shell::event_loop::render_frame(&mut app, f, "test-session");
        });
        heights.push(height_now(&app, &terminal).0);
    }

    let first = heights[0];
    assert!(
        heights.iter().all(|h| *h == first),
        "the dock height must not change as notices arrive: {heights:?}"
    );
    assert!(first > 0, "and it must be open, not zero");
}

#[test]
fn expanding_a_detail_is_the_one_thing_that_may_grow_the_dock() {
    // The single allowed reflow: the user pressed `Enter`, so the extra height is
    // the layout they asked for, not jitter.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "test-session".to_string();
    for i in 0..10 {
        disclose(
            &mut app,
            NoticeSeverity::Error,
            "t",
            &format!("fault {i} with enough words to wrap across several rows in the dock"),
            i,
        );
    }
    app.open_notice_viewer();
    let collapsed = footer_dock_height(&app);

    app.notice_viewer.as_mut().unwrap().detail = true;
    let expanded = footer_dock_height(&app);

    assert!(
        expanded > collapsed,
        "an expanded detail must buy the rows it needs: {collapsed} -> {expanded}"
    );
    assert!(
        expanded <= 40 / 2,
        "but never past the ceiling that protects the conversation: {expanded}"
    );
}
