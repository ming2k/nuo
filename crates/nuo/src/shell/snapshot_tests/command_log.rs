//! The command log **scene**: a numbered page carrying the whole log, including
//! the middle the transcript folded away.
//!
//! Locked as a snapshot because the scene is the destination the fold divider
//! promises, and its shape (one numbered row per log row, the rail, the outcome
//! band) is the thing a user navigated into.

use super::helpers::painted_rows;
use crate::shell::render::Theme;
use nuo_view::model::layout::LayoutMap;

#[test]
fn command_log_scene_renders_a_numbered_page_of_the_whole_log() {
    use nuo_view::model::command_log::CommandLog;

    let stdout = (1..=20)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let structured = nuo_tool::ToolOutput::Shell {
        command: "cargo build".into(),
        stdout,
        stderr: String::new(),
        lines: Vec::new(),
        exit: Some(1),
        truncated: false,
        truncated_note: None,
        termination: nuo_tool::ShellTermination::Exited,
        detached_job_id: None,
    };
    let text = structured.to_text();
    let fold = nuo_view::tools::shell_output::ShellOutput::derive(&text, Some(&structured)).fold();
    let log = CommandLog::derive(
        0,
        r#"{"command":"cargo build"}"#,
        &text,
        Some(&structured),
        fold,
    );

    let tabs = [crate::shell::surfaces::ClientTab::thread(
        "s1",
        Some("build"),
    )];
    let mut terminal = nuotc::TestTerminal::new(80, 20);
    let mut scroll = 0usize;
    terminal.draw(|f| {
        let mut layout_map = LayoutMap::default();
        let _ = crate::shell::overlays::draw_command_log_scene(
            f,
            crate::shell::overlays::LogSceneProps {
                log: Some(&log),
                scroll: &mut scroll,
                thread_id: Some("s1"),
                tab_bar: Some(crate::shell::render::TabBarProps {
                    tabs: Some(&tabs),
                    active_tab: 0,
                    unread_faults: 0,
                }),
            },
            &mut layout_map,
            &Theme::default(),
        );
    });
    let grid = painted_rows(terminal.buffer(), 80, 20).0;
    insta::assert_snapshot!(grid);
}

/// The log scene's footer advertises what pressing `h` will *do*, so it flips with
/// the scope. A band that kept saying `hidden only` while the reader was already
/// reading the hidden rows would point at the place they are standing — the same
/// class of lie as a keycap that does not serve the key it prints.
#[test]
fn the_log_scene_footer_names_the_scope_the_toggle_would_reach() {
    use nuo_view::model::command_log::{CommandLog, LogScope};

    let stdout = (1..=20)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let structured = nuo_tool::ToolOutput::Shell {
        command: "cargo build".into(),
        stdout,
        stderr: String::new(),
        lines: Vec::new(),
        exit: Some(0),
        truncated: false,
        truncated_note: None,
        termination: nuo_tool::ShellTermination::Exited,
        detached_job_id: None,
    };
    let text = structured.to_text();
    let fold = nuo_view::tools::shell_output::ShellOutput::derive(&text, Some(&structured)).fold();
    let mut log = CommandLog::derive(
        0,
        r#"{"command":"cargo build"}"#,
        &text,
        Some(&structured),
        fold,
    );
    let tabs = [crate::shell::surfaces::ClientTab::thread(
        "s1",
        Some("build"),
    )];

    let render = |log: &CommandLog| {
        let mut terminal = nuotc::TestTerminal::new(80, 20);
        let mut scroll = 0usize;
        terminal.draw(|f| {
            let mut layout_map = LayoutMap::default();
            let _ = crate::shell::overlays::draw_command_log_scene(
                f,
                crate::shell::overlays::LogSceneProps {
                    log: Some(log),
                    scroll: &mut scroll,
                    thread_id: Some("s1"),
                    tab_bar: Some(crate::shell::render::TabBarProps {
                        tabs: Some(&tabs),
                        active_tab: 0,
                        unread_faults: 0,
                    }),
                },
                &mut layout_map,
                &Theme::default(),
            );
        });
        painted_rows(terminal.buffer(), 80, 20).0
    };

    log.scope = LogScope::Full;
    let full = render(&log);
    assert!(
        full.contains("h hidden only"),
        "reading the full log, `h` offers the hidden rows:\n{full}"
    );

    log.scope = LogScope::HiddenOnly;
    let hidden = render(&log);
    assert!(
        hidden.contains("h full log"),
        "reading the hidden rows, `h` offers the full log back:\n{hidden}"
    );
    assert!(
        !hidden.contains("hidden only"),
        "the band must not offer the scope the reader is already in:\n{hidden}"
    );
}
