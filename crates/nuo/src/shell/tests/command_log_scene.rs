//! The command-log scene: opening it, reading it, scoping it, copying it,
//! and leaving it.
//!
//! The scene replaced a dialog, so these tests pin the properties that made the
//! move worth doing: it is a numbered *tab scene* (so `Alt+[` / breadcrumbs /
//! `Esc` work through the paths every scene uses), it holds the **whole** log
//! including what the fold hid, and every copy path describes the slice it
//! actually wrote.

// Integration test target: assertions fail by panicking, so the production
// no-panic lints are meaningless here (the exemption ends at the test boundary).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;
use nuo_tool::ToolOutput;
use nuo_tool::ShellTermination;
use nuo_view::model::command_log::LogScope;
use nuo_view::model::document::MessageKind;

/// Build a finished `execute_command` step whose body folds (well past
/// HEAD + TAIL + 1) with the given exit code.
fn folded_command_step(rows: usize, exit: i32) -> nuo_view::model::document::TranscriptMessage {
    let stdout = (1..=rows)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let structured = ToolOutput::Shell {
        command: "cargo build".into(),
        stdout,
        stderr: String::new(),
        lines: Vec::new(),
        exit: Some(exit),
        truncated: false,
        truncated_note: None,
        termination: ShellTermination::Exited,
        detached_job_id: None,
    };
    let text = structured.to_text();
    let mut message = nuo_view::model::document::TranscriptMessage::tool_step(
        "call_build",
        "execute_command",
        r#"{"command":"cargo build"}"#,
    );
    message.finish_tool_step("call_build", text, structured, 1200);
    if let MessageKind::ToolStep { expanded, .. } = &mut message.kind {
        *expanded = true;
    }
    message
}

fn app_with_command_log(rows: usize, exit: i32) -> App {
    let (mut app, _tmp) = app_in_tempdir(&["Cargo.toml"], &[]);
    // A log is pushed onto a *tab's* stack, so the fixture opens the thread tab
    // the step lives in — without one there is no stack to push onto.
    app.open_tab(crate::shell::surfaces::ClientTab::thread(
        "s1",
        Some("build"),
    ));
    app.messages = vec![folded_command_step(rows, exit)];
    app
}

#[test]
fn opening_a_folded_command_step_pushes_the_log_scene_onto_the_tab_stack() {
    let mut app = app_with_command_log(40, 1);
    app.open_command_log_scene(0);

    // The scene is the foreground, and it is the tab's own scene — the same
    // stack `Alt+[` walks — rather than a floating dialog.
    assert_eq!(
        app.current_scene(),
        crate::shell::surfaces::SceneKind::CommandLog
    );
    assert_eq!(app.surfaces.active_overlay(), None);
    let crumbs = app.surfaces.active_tab().unwrap().breadcrumbs().to_vec();
    assert_eq!(
        crumbs,
        vec![
            crate::shell::surfaces::SceneKind::Thread,
            crate::shell::surfaces::SceneKind::CommandLog
        ],
        "the log is one push above the thread it was read from"
    );
}

#[test]
fn the_log_scene_holds_every_row_the_fold_hid() {
    let mut app = app_with_command_log(40, 0);
    app.open_command_log_scene(0);
    let log = app.command_log.as_ref().expect("the scene derived a log");

    assert_eq!(log.scope, LogScope::Full);
    assert_eq!(log.len(), 40, "the scene is the whole log, not the fold");
    assert!(log.hidden_len() > 0, "precondition: the body folded");
    assert_eq!(log.command, "cargo build");
}

#[test]
fn leaving_the_scene_drops_the_snapshot_and_returns_to_the_thread() {
    let mut app = app_with_command_log(40, 0);
    app.open_command_log_scene(0);
    app.command_log_scroll = 7;

    assert!(app.close_command_log_scene());
    assert_eq!(
        app.current_scene(),
        crate::shell::surfaces::SceneKind::Thread
    );
    assert!(
        app.command_log.is_none(),
        "a kept snapshot would be a reading of a step the transcript may have replaced"
    );
    assert_eq!(app.command_log_scroll, 0);
}

#[test]
fn esc_back_from_the_log_scene_returns_to_the_transcript() {
    // `Esc` at this depth is "back", never "quit" — the scene has no sub-layer
    // to spend the key on, so the universal escape hatch must pop the scene.
    let mut app = app_with_command_log(40, 0);
    app.open_command_log_scene(0);
    app.scene_back();
    assert_eq!(
        app.current_scene(),
        crate::shell::surfaces::SceneKind::Thread
    );
}

#[tokio::test]
async fn scoping_to_the_hidden_rows_shows_exactly_the_middle() {
    let mut app = app_with_command_log(40, 0);
    app.open_command_log_scene(0);
    let hidden = app.command_log.as_ref().unwrap().hidden_len();
    app.command_log_scroll = 5;

    let runtime = crate::shell::event_loop::UiRuntime::minimal_for_test();
    crate::shell::event_loop::actions::dispatch_action_for_test(
        &mut app,
        &runtime,
        crate::shell::input::InputAction::CommandLogToggleScope,
        "s1",
    )
    .await;

    let log = app.command_log.as_ref().unwrap();
    assert_eq!(log.scope, LogScope::HiddenOnly);
    assert_eq!(log.len(), hidden);
    assert_eq!(
        app.command_log_scroll, 0,
        "the scope changed length, so the offset re-anchors at the top \
         instead of clamping into the middle of a different slice"
    );
}

#[tokio::test]
async fn the_scope_toggle_refuses_an_unfolded_step() {
    let mut app = app_with_command_log(2, 0);
    app.open_command_log_scene(0);
    assert!(!app.command_log.as_ref().unwrap().can_scope_toggle());

    let runtime = crate::shell::event_loop::UiRuntime::minimal_for_test();
    crate::shell::event_loop::actions::dispatch_action_for_test(
        &mut app,
        &runtime,
        crate::shell::input::InputAction::CommandLogToggleScope,
        "s1",
    )
    .await;
    assert_eq!(
        app.command_log.as_ref().unwrap().scope,
        LogScope::Full,
        "no key press is spent revealing an identical page"
    );
}

#[test]
fn a_non_command_step_opens_no_log_scene() {
    let (mut app, _tmp) = app_in_tempdir(&["Cargo.toml"], &[]);
    let mut message = nuo_view::model::document::TranscriptMessage::tool_step(
        "call_read",
        "read",
        r#"{"path":"Cargo.toml"}"#,
    );
    message.finish_tool_step("call_read", "contents", ToolOutput::text("contents"), 10);
    app.messages = vec![message];

    app.open_command_log_scene(0);
    assert_eq!(
        app.current_scene(),
        crate::shell::surfaces::SceneKind::Thread,
        "a read is not a command body, so there is no log to push"
    );
    assert!(app.command_log.is_none());
}

#[test]
fn the_hidden_rows_copy_carries_the_invocation_and_only_the_hidden_rows() {
    let app = app_with_command_log(40, 0);
    let (text, feedback) = crate::shell::copy::fold_hidden_rows_copy(&app, 0)
        .expect("a folded step has hidden rows to copy");

    let log = nuo_view::tools::shell_output::ShellOutput::derive(
        match &app.messages[0].kind {
            MessageKind::ToolStep { output, .. } => output.as_deref().unwrap_or(""),
            _ => unreachable!(),
        },
        match &app.messages[0].kind {
            MessageKind::ToolStep { structured, .. } => structured.as_deref(),
            _ => unreachable!(),
        },
    );
    let fold = log.fold().expect("precondition: the body folds");

    assert!(
        text.starts_with("$ cargo build\n"),
        "a pasted log without its invocation is output with no question attached"
    );
    let copied_rows: Vec<&str> = text.lines().skip(1).collect();
    assert_eq!(
        copied_rows.len(),
        fold.hidden,
        "the verb copies what its label promises: the rows the fold hides"
    );
    assert_eq!(copied_rows[0], log.rows[fold.head_end]);
    assert!(
        feedback.starts_with("hidden rows copied"),
        "the receipt names the slice that was written: {feedback}"
    );
}

#[test]
fn the_hidden_rows_copy_declines_when_nothing_is_hidden() {
    let app = app_with_command_log(2, 0);
    assert!(
        crate::shell::copy::fold_hidden_rows_copy(&app, 0).is_none(),
        "a verb that cannot honour its label reports that instead of copying the whole payload"
    );
}
