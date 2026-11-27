//! Draft residency across view transitions and restarts (ADR-0247).
//!
//! The pre-ADR-0247 model kept the unsent draft on the `ClientTab`, which made
//! the draft a property of a *view*. Every test here is a reported loss under
//! that model and a guarantee under this one: a draft belongs to the thread it
//! was typed into, survives the view that held it, and survives the process.

use super::*;

fn thread_app() -> (App, tempfile::TempDir) {
    let (mut app, tmp) = app_in_tempdir(&[], &[]);
    app.session.current_session_id = "t1".to_string();
    app.surfaces
        .surfaces
        .open_tab(crate::shell::surfaces::ClientTab::thread(
            "t1",
            Some("Thread 1"),
        ));
    app.surfaces.surfaces.select_tab(0);
    app.hand_composer_to_current_thread();
    (app, tmp)
}

/// The core promise: switching to another thread tab and back returns the exact
/// text, caret, staged payloads, and view offset.
#[test]
fn a_tab_switch_parks_and_restores_the_draft_per_thread() {
    let (mut app, _tmp) = thread_app();
    app.composer.input = "draft for thread 1".to_string();
    app.composer.cursor_position = 7;
    app.composer.input_scroll = 3;
    app.composer.pending_text_pastes = vec!["a large paste".to_string()];

    app.surfaces
        .surfaces
        .open_tab(crate::shell::surfaces::ClientTab::thread(
            "t2",
            Some("Thread 2"),
        ));
    app.hand_composer_to_current_thread();
    assert_eq!(app.composer.input, "", "the arriving thread starts clean");

    app.composer.input = "draft for thread 2".to_string();
    app.composer.cursor_position = 4;

    app.surfaces.surfaces.select_tab(0);
    app.hand_composer_to_current_thread();
    assert_eq!(app.composer.input, "draft for thread 1");
    assert_eq!(app.composer.cursor_position, 7, "the caret is preserved");
    assert_eq!(
        app.composer.input_scroll, 3,
        "the view offset is preserved, not re-derived"
    );
    assert_eq!(
        app.composer.pending_text_pastes,
        vec!["a large paste".to_string()],
        "staged payloads are not dropped by a tab switch"
    );

    app.surfaces.surfaces.select_tab(1);
    app.hand_composer_to_current_thread();
    assert_eq!(app.composer.input, "draft for thread 2");
}

/// Re-focusing the tab you are already on (`Alt-1` on the active tab, clicking
/// an active tab) must be a non-event: it may not reset the draft.
#[test]
fn refocusing_the_active_thread_is_a_no_op_for_the_draft() {
    let (mut app, _tmp) = thread_app();
    app.composer.input = "still typing".to_string();
    app.composer.cursor_position = 5;

    app.hand_composer_to_current_thread();
    app.hand_composer_to_current_thread();

    assert_eq!(app.composer.input, "still typing");
    assert_eq!(app.composer.cursor_position, 5);
}

/// The regression that started this work: a cross-thread tab switch re-attaches,
/// so a **new** `App` boots and its first per-frame sync calls
/// `on_viewed_session_changed`. That hook used to blank the composer, erasing the
/// draft the tab restore had just installed. It must load the arriving thread's
/// draft instead.
#[test]
fn the_first_frame_session_sync_installs_the_arriving_draft_instead_of_blanking_it() {
    let (mut app, _tmp) = thread_app();
    app.composer.input = "work in progress".to_string();
    app.park_live_draft();

    // A runner hand-off: a fresh App with the parked draft as its only memory.
    let (mut booted, _tmp2) = app_in_tempdir(&[], &[]);
    booted.surfaces.drafts.park(
        "t1",
        nuo_view::composer_draft::ComposerDraft::capture(
            "work in progress".to_string(),
            6,
            Vec::new(),
            Vec::new(),
            1,
        ),
    );
    booted.session.current_session_id = "t1".to_string();
    booted
        .surfaces
        .surfaces
        .open_tab(crate::shell::surfaces::ClientTab::thread(
            "t1",
            None::<String>,
        ));

    // The frame where the live id lands on t1.
    booted.on_viewed_session_changed();

    assert_eq!(
        booted.composer.input, "work in progress",
        "the viewed-session hook must hand the thread's draft to the composer, not clear it"
    );
    assert_eq!(booted.composer.cursor_position, 6);
    assert_eq!(booted.composer.input_scroll, 1);
}

/// `replace_tab` (picking a thread over an empty placeholder) must not overwrite
/// the departing thread's draft with the arriving tab's empty state — the
/// departing thread keeps its text in the ledger under its own id.
#[test]
fn replacing_a_tab_in_place_keeps_the_departing_threads_draft() {
    let (mut app, _tmp) = thread_app();
    app.composer.input = "half-written".to_string();
    app.park_live_draft();

    let idx = app.surfaces.surfaces.active_tab_index();
    app.replace_tab(
        idx,
        crate::shell::surfaces::ClientTab::thread("t7", Some("Picked")),
    );
    assert_eq!(
        app.composer.input, "",
        "the arriving thread has no draft of its own"
    );

    // Going back to t1 restores what the user typed there.
    app.session.current_session_id = "t1".to_string();
    app.open_tab(crate::shell::surfaces::ClientTab::thread(
        "t1",
        Some("Thread 1"),
    ));
    assert_eq!(app.composer.input, "half-written");
}

/// A thread whose only content is an unsent draft is **not** empty: recycling it
/// (`/new`, the TabBar `+`) or `DeleteThread`-ing it would destroy text the user
/// can still see.
#[test]
fn a_thread_with_a_parked_draft_is_not_empty() {
    let (mut app, _tmp) = thread_app();
    assert!(app.is_current_thread_empty(), "a fresh thread is empty");

    app.composer.input = "not sent yet".to_string();
    assert!(
        !app.is_current_thread_empty(),
        "unsent text in the live composer makes the thread non-empty"
    );

    // A subagent zoom, a command log, or a diff is still *inside* the thread's
    // tab: reading a step must not make the composer stop owning that thread's
    // draft.
    app.surfaces
        .surfaces
        .switch_scene(crate::shell::surfaces::SceneKind::Subagent);
    app.park_live_draft();
    assert!(
        app.surfaces.drafts.has_stored("t1"),
        "an intra-tab scene does not detach the draft from its thread"
    );
    app.surfaces
        .surfaces
        .switch_scene(crate::shell::surfaces::SceneKind::Thread);

    // Parked (the tab-switch shape) and the live buffer cleared.
    app.park_live_draft();
    app.composer.input.clear();
    assert!(
        !app.is_current_thread_empty(),
        "a parked draft makes the thread non-empty even with a clean composer"
    );
}

/// Sending is the one event that makes a draft stop existing. The parked copy
/// must go with it, or the draft hint would offer to restore text the user
/// already sent.
#[test]
fn sending_clears_the_parked_draft() {
    let (mut app, _tmp) = thread_app();
    app.composer.input = "send me".to_string();
    app.park_live_draft();
    assert!(app.surfaces.drafts.has_stored("t1"));

    app.clear_parked_draft();
    // The send takes the text out of the composer as well; both halves are the
    // same event ("this input is no longer unsent").
    app.composer.input.clear();
    assert!(
        !app.surfaces.drafts.has_stored("t1"),
        "the sent text is no longer the unsent slot"
    );
    assert!(
        app.is_current_thread_empty(),
        "with the draft gone the thread is empty again"
    );
}

/// A workspace scene borrows the same buffer for its inline prompt. That borrow
/// is not the thread's draft: leaving for the Dashboard parks it, and coming back
/// installs it — the directive the user was typing into the dashboard prompt must
/// never be resurrected in the chat box and the chat draft must not be lost.
#[test]
fn a_workspace_scene_parks_the_draft_and_releases_the_buffer() {
    let (mut app, _tmp) = thread_app();
    app.composer.input = "chat draft".to_string();
    app.park_live_draft();

    app.open_tab(crate::shell::surfaces::ClientTab::dashboard());
    app.hand_composer_to_current_thread();
    assert_eq!(app.composer.input, "", "the dashboard owns the buffer now");

    app.composer.input = "new session prompt".to_string();

    app.surfaces.surfaces.select_tab(0);
    app.hand_composer_to_current_thread();
    assert_eq!(
        app.composer.input, "chat draft",
        "returning to the thread installs its draft, not the dashboard directive"
    );
}

/// The durability guarantee, end to end: park on the exit seam, then a *new*
/// start-up installs what the old process wrote.
#[test]
fn sealing_on_exit_makes_the_draft_survive_the_process() {
    let dir = tempfile::tempdir().expect("tempdir");
    {
        let mut app = crate::shell::tests::new_app_for_relay_tests();
        app.surfaces.drafts =
            crate::shell::draft_store::DraftLedger::load(dir.path().to_path_buf());
        app.session.current_session_id = "t1".to_string();
        app.surfaces
            .surfaces
            .open_tab(crate::shell::surfaces::ClientTab::thread(
                "t1",
                Some("Thread 1"),
            ));
        app.composer.input = "survive me".to_string();
        app.composer.cursor_position = 4;
        app.seal_drafts();
    }

    // A new process: reload the ledger from the same state directory and hand
    // the composer to the thread the boot path selects.
    let (mut booted, _tmp) = app_in_tempdir(&[], &[]);
    booted.surfaces.drafts = crate::shell::draft_store::DraftLedger::load(dir.path().to_path_buf());
    booted.session.current_session_id = "t1".to_string();
    booted
        .surfaces
        .surfaces
        .open_tab(crate::shell::surfaces::ClientTab::thread(
            "t1",
            None::<String>,
        ));
    booted.hand_composer_to_current_thread();

    assert_eq!(booted.composer.input, "survive me");
    assert_eq!(booted.composer.cursor_position, 4);
    assert_eq!(booted.composer.draft_thread.as_deref(), Some("t1"));
}

/// Sealing while history recall is active must park the *stashed* draft, not the
/// history row currently occupying the buffer.
#[test]
fn sealing_parks_the_stashed_draft_not_a_recalled_history_row() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (mut app, _tmp) = thread_app();
    app.surfaces.drafts = crate::shell::draft_store::DraftLedger::load(dir.path().to_path_buf());
    app.composer.input = "my real draft".to_string();
    app.stash_draft();
    app.composer.input = "a recalled history row".to_string();
    app.composer.history_index = Some(0);

    app.seal_drafts();

    let mut reopened = crate::shell::draft_store::DraftLedger::load(dir.path().to_path_buf());
    let parked = reopened.parked("t1").expect("draft");
    assert_eq!(
        parked.text, "my real draft",
        "a recall row is a projection, not the user's draft"
    );
}

/// The input-injection sheet borrows the composer line through the same stash,
/// so the user's staged attachments survive the borrow.
#[test]
fn the_input_injection_sheet_borrows_the_line_without_dropping_attachments() {
    let (mut app, _tmp) = thread_app();
    app.composer.input = "before the sheet".to_string();
    app.composer.cursor_position = 6;
    app.composer.pending_text_pastes = vec!["a large paste".to_string()];

    app.park_input_draft();
    assert_eq!(app.composer.input, "");
    assert!(app.has_stashed_draft());

    // The sheet's own text, then dismissal.
    app.composer.input = "the sheet's answer".to_string();
    app.restore_input_draft();

    assert_eq!(app.composer.input, "before the sheet");
    assert_eq!(app.composer.cursor_position, 6);
    assert_eq!(
        app.composer.pending_text_pastes,
        vec!["a large paste".to_string()],
        "the borrow must not discard staged payloads"
    );
}

/// Switching tabs while `↑` has recalled a history row parks the user's stashed
/// draft — the row is a projection of history, not text the user wrote here.
#[test]
fn a_tab_switch_during_history_recall_parks_the_stash_not_the_row() {
    let (mut app, _tmp) = thread_app();
    app.composer.input = "the real draft".to_string();
    app.stash_draft();
    app.composer.input = "a recalled row".to_string();
    app.composer.history_index = Some(0);

    app.surfaces
        .surfaces
        .open_tab(crate::shell::surfaces::ClientTab::thread(
            "t2",
            Some("Thread 2"),
        ));
    app.hand_composer_to_current_thread();
    assert_eq!(app.composer.input, "", "the arriving thread starts clean");

    app.surfaces.surfaces.select_tab(0);
    app.hand_composer_to_current_thread();
    assert_eq!(
        app.composer.input, "the real draft",
        "a recalled history row must never be parked as the thread's draft"
    );
}

/// Dropping a thread drops its draft: a file left behind under a deleted id can
/// only ever be restored into a thread that no longer exists.
#[test]
fn a_parked_draft_can_be_dropped_with_its_thread() {
    let (mut app, _tmp) = thread_app();
    app.composer.input = "about to be deleted".to_string();
    app.park_live_draft();
    assert!(app.surfaces.drafts.has_stored("t1"));

    app.surfaces.drafts.forget("t1");
    assert!(!app.surfaces.drafts.has_stored("t1"));

    // A view that has never held the buffer — the shape after a re-attach.
    let (mut booted, _tmp2) = app_in_tempdir(&[], &[]);
    booted.session.current_session_id = "t1".to_string();
    booted
        .surfaces
        .surfaces
        .open_tab(crate::shell::surfaces::ClientTab::thread(
            "t1",
            None::<String>,
        ));
    booted.hand_composer_to_current_thread();
    assert!(
        booted.composer.input.is_empty(),
        "a forgotten draft cannot be restored"
    );
}
