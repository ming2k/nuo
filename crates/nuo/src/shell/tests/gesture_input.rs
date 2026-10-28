//! End-to-end gesture input tests: one click / one chord must be enough.
//!
//! These drive the **real** dispatch path (`render_frame` → `app.ui.commit()` →
//! [`process_one_event`](crate::shell::event_loop::process_one_event)) rather
//! than a hand-mounted component tree, so the presented scene — the sole
//! pointer hit-test authority — and the component/keyboard routing are exactly
//! the ones the running TUI uses. The defect class they pin down: a gesture
//! whose first attempt is silently consumed, so the operator has to repeat it
//! ("the tab click needs two clicks", "`C-x w` sometimes needs two presses").
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::app_in_tempdir;

use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};

use crate::shell::ui::UiKey;

/// Paint one frame and publish its geometry — the state the next input event
/// resolves against.
fn paint(app: &mut crate::shell::App, terminal: &mut nuotc::TestTerminal) {
    terminal.draw(|f| {
        crate::shell::event_loop::render_frame(app, f, "s1");
    });
    app.ui.commit();
}

async fn send(app: &mut crate::shell::App, terminal: &mut nuotc::TestTerminal, event: Event) {
    paint(app, terminal);
    crate::shell::event_loop::process_event_for_test(app, &event, "s1").await;
}

fn mouse(kind: MouseEventKind, column: u16, row: u16) -> Event {
    Event::Mouse(MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    })
}

fn key(code: KeyCode, modifiers: KeyModifiers) -> Event {
    Event::Key(KeyEvent::new(code, modifiers))
}

fn ctrl_x() -> Event {
    key(KeyCode::Char('x'), KeyModifiers::CONTROL)
}

/// Three tabs: thread (x 0..11), dashboard (x 11..23), settings (x 23..33).
fn seeded_app() -> (crate::shell::App, tempfile::TempDir) {
    let (mut app, tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "s1".to_string();
    app.open_tab(crate::shell::surfaces::ClientTab::thread(
        "s1",
        Some("Thread 1"),
    ));
    app.open_tab(crate::shell::surfaces::ClientTab::dashboard());
    app.open_tab(crate::shell::surfaces::ClientTab::settings());
    app.surfaces.select_tab(0);
    (app, tmp)
}

/// [INV-TAB-11] single-click switch fidelity, repeatedly: every click on an
/// inactive tab switches on *that* click, with no intervening repaint of the
/// caller's own making.
#[tokio::test]
async fn every_single_click_switches() {
    let (mut app, _tmp) = seeded_app();
    let mut terminal = nuotc::TestTerminal::new(80, 24);
    paint(&mut app, &mut terminal);

    for (x, expected) in [(15u16, 1usize), (5, 0), (28, 2), (5, 0), (15, 1)] {
        send(
            &mut app,
            &mut terminal,
            mouse(MouseEventKind::Down(MouseButton::Left), x, 0),
        )
        .await;
        assert_eq!(
            app.surfaces.active_tab_index(),
            expected,
            "click at x={x} must activate tab {expected} on the first click"
        );
    }
}

/// A pointer motion event is not a keystroke: it must not cancel a half-typed
/// `C-x` chord. Terminals that report motion (the WinAPI backend does so
/// natively regardless of the DEC tracking modes) used to interleave `Hover`
/// actions between the two strokes and disarm the namespace, which is why
/// `C-x w` sometimes needed two attempts.
#[tokio::test]
async fn pointer_motion_does_not_cancel_the_ctrl_x_chord() {
    let (mut app, _tmp) = seeded_app();
    let mut terminal = nuotc::TestTerminal::new(80, 24);
    paint(&mut app, &mut terminal);
    let tabs_before = app.surfaces.tabs().len();

    send(&mut app, &mut terminal, ctrl_x()).await;
    assert!(app.scene_namespace_armed, "C-x arms the scene namespace");

    send(&mut app, &mut terminal, mouse(MouseEventKind::Moved, 10, 0)).await;
    assert!(
        app.scene_namespace_armed,
        "a pointer motion between strokes must not disarm the chord"
    );

    send(
        &mut app,
        &mut terminal,
        key(KeyCode::Char('w'), KeyModifiers::NONE),
    )
    .await;
    assert_eq!(
        app.surfaces.tabs().len(),
        tabs_before - 1,
        "C-x w closes one tab on the first press"
    );
}

/// A keystroke that is *not* a namespace verb still disarms the chord, so a
/// half-typed leader cannot linger and reinterpret a later key.
#[tokio::test]
async fn a_stray_keystroke_still_disarms_the_chord() {
    let (mut app, _tmp) = seeded_app();
    let mut terminal = nuotc::TestTerminal::new(80, 24);
    paint(&mut app, &mut terminal);

    send(&mut app, &mut terminal, ctrl_x()).await;
    assert!(app.scene_namespace_armed);
    send(
        &mut app,
        &mut terminal,
        key(KeyCode::F(5), KeyModifiers::NONE),
    )
    .await;
    assert!(
        !app.scene_namespace_armed,
        "an unrecognized keystroke cancels the namespace"
    );
}

/// A mounted keyboard-modal component (`ConfigDropdown`) has no claim on the
/// pointer: the click that dismisses the popup must reach the mouse handler.
/// Routing mouse events through the component cascade made every click in the
/// frame inert while a settings dropdown was up — the drop-down could only be
/// dismissed with Esc, and clicks on the tab bar beneath it did nothing.
#[tokio::test]
async fn click_dismisses_an_open_dropdown_through_the_real_dispatch() {
    let (mut app, _tmp) = seeded_app();
    app.surfaces.select_tab(2);
    let dropdown = crate::shell::views::settings::build_websearch_provider_dropdown("exa", None);
    app.config_dropdown = Some((
        dropdown,
        nuo_ui_primitives::components::dropdown::DropdownAnchor::center_screen(),
    ));

    let mut terminal = nuotc::TestTerminal::new(80, 24);
    paint(&mut app, &mut terminal);
    assert!(
        app.ui.scene().id(&UiKey::ConfigDropdown).is_some(),
        "the open dropdown is a mounted modal component (inputs: keyboard path [ConfigDropdown])"
    );

    send(
        &mut app,
        &mut terminal,
        mouse(MouseEventKind::Down(MouseButton::Left), 60, 18),
    )
    .await;
    assert!(
        app.config_dropdown.is_none(),
        "a click outside the popup dismisses it"
    );
}
