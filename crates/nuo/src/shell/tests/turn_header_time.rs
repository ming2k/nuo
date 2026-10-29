//! Turn-header send-time stamping (`HH:MM` provenance).
//!
//! Every transcript entry owns its own record of *when* it was sent
//! ([`TranscriptMessage::sent_at_ms`]), and the band headers are the only place
//! that fact reaches the reader: a user round header reads the prompt's stamp, a
//! turn band header reads the stamp of whichever component opened the band.
//!
//! The two paths that build those entries — the live response translator and the
//! session resume — must therefore agree, and every component of one turn must
//! agree *inside* itself (one turn emits thinking, text and tool steps, but its
//! band shows exactly one clock time). These tests drive the real applier so the
//! guarantee is enforced by the code the TUI actually runs, not by a hand-built
//! fixture that could drift from it.

// Integration test target: assertions fail by panicking, so the production
// no-panic lints are meaningless here (the exemption ends at the test boundary).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;
use crate::shell::event_loop::TranscriptEdit;
use crate::shell::event_loop::UiRuntime;
use nuo_ui_primitives::time::sent_time_label;

const TURN_MS: u64 = 1_700_000_000_000;

fn apply_edit(app: &mut App, runtime: &UiRuntime, edit: TranscriptEdit) {
    crate::shell::event_loop::apply::apply(
        app,
        runtime,
        crate::shell::event_loop::AppMutation::Transcript { edit },
    );
}

/// A tool step placed at `(round, turn)` with no send time of its own — exactly
/// what an arm that forgot `.with_sent_at_ms(...)` produces.
fn unstamped_step(round: u64, turn: u64) -> TranscriptMessage {
    let mut step = TranscriptMessage::tool_step("call_1", "read_text", r#"{"path":"a.rs"}"#);
    step.round = Some(round);
    step.turn = Some(turn);
    step
}

/// The invariant that makes a header's clock trustworthy: a component joining a
/// turn that already has a send time adopts it. This is the repair mechanism —
/// a new builder call site cannot silently drop the `HH:MM`.
#[tokio::test]
async fn a_component_joining_a_stamped_turn_adopts_its_time() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let runtime = UiRuntime::minimal_for_test();

    let mut thinking = TranscriptMessage::reasoning("think first");
    thinking.round = Some(1);
    thinking.turn = Some(1);
    thinking.set_reasoning_duration(3);
    thinking.sent_at_ms = Some(TURN_MS);
    apply_edit(
        &mut app,
        &runtime,
        TranscriptEdit::Append {
            class: nuo_wire::EventClass::Dialogue,
            message: thinking,
        },
    );

    // The tool step arrives unstamped. It is a component of the same turn, so it
    // must inherit that turn's time rather than render a clockless header.
    apply_edit(
        &mut app,
        &runtime,
        TranscriptEdit::ToolStart {
            message: unstamped_step(1, 1),
        },
    );

    let step = app
        .messages
        .iter()
        .find(|message| message.is_tool_step())
        .expect("tool step admitted");
    assert_eq!(
        step.sent_at_ms,
        Some(TURN_MS),
        "an unstamped component must adopt the send time its turn already established"
    );
}

/// A different turn is a different moment: adoption must never leak one turn's
/// clock onto another's band.
#[tokio::test]
async fn adoption_is_scoped_to_the_component_own_turn() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let runtime = UiRuntime::minimal_for_test();

    let mut first = TranscriptMessage::reasoning("turn one");
    first.round = Some(1);
    first.turn = Some(1);
    first.set_reasoning_duration(1);
    first.sent_at_ms = Some(TURN_MS);
    apply_edit(
        &mut app,
        &runtime,
        TranscriptEdit::Append {
            class: nuo_wire::EventClass::Dialogue,
            message: first,
        },
    );

    // Turn 2 has no established time of its own in the document.
    apply_edit(
        &mut app,
        &runtime,
        TranscriptEdit::ToolStart {
            message: unstamped_step(1, 2),
        },
    );

    let step = app
        .messages
        .iter()
        .find(|message| message.is_tool_step())
        .expect("tool step admitted");
    assert_eq!(
        step.sent_at_ms, None,
        "a component of an unstamped turn must not borrow another turn's clock"
    );
}

/// An explicitly stamped component keeps its own moment. Adoption is a repair
/// for a missing fact, never an override of a present one.
#[tokio::test]
async fn an_explicit_stamp_is_never_overwritten() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let runtime = UiRuntime::minimal_for_test();

    let mut thinking = TranscriptMessage::reasoning("turn one");
    thinking.round = Some(1);
    thinking.turn = Some(1);
    thinking.set_reasoning_duration(1);
    thinking.sent_at_ms = Some(TURN_MS);
    apply_edit(
        &mut app,
        &runtime,
        TranscriptEdit::Append {
            class: nuo_wire::EventClass::Dialogue,
            message: thinking,
        },
    );

    let own = TURN_MS + 5_000;
    let mut step = unstamped_step(1, 1);
    step.sent_at_ms = Some(own);
    apply_edit(
        &mut app,
        &runtime,
        TranscriptEdit::ToolStart { message: step },
    );

    let step = app
        .messages
        .iter()
        .find(|message| message.is_tool_step())
        .expect("tool step admitted");
    assert_eq!(step.sent_at_ms, Some(own));
}

/// The render contract, end to end: an admitted live turn paints its `HH:MM`
/// even though the component that opened the band was thinking, and the time it
/// paints is the turn's.
#[tokio::test]
async fn an_admitted_turn_band_paints_the_turn_time() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let runtime = UiRuntime::minimal_for_test();

    let mut thinking = TranscriptMessage::reasoning("think first");
    thinking.round = Some(1);
    thinking.turn = Some(1);
    thinking.set_reasoning_duration(3);
    thinking.sent_at_ms = Some(TURN_MS);
    apply_edit(
        &mut app,
        &runtime,
        TranscriptEdit::Append {
            class: nuo_wire::EventClass::Dialogue,
            message: thinking,
        },
    );
    apply_edit(
        &mut app,
        &runtime,
        TranscriptEdit::ToolStart {
            message: unstamped_step(1, 1),
        },
    );

    let grid = render_grid(&app.messages, 72, 16);
    let header = grid
        .lines()
        .find(|row| row.contains("› turn 1"))
        .unwrap_or_else(|| panic!("turn band must render:\n{grid}"));
    assert!(
        header.contains(&sent_time_label(TURN_MS)),
        "an admitted turn band must paint the turn's send time:\n{grid}"
    );
}

/// Render a transcript grid with the production transcript renderer.
fn render_grid(messages: &[TranscriptMessage], width: u16, height: u16) -> String {
    use crate::shell::render::{
        EmptyStateGuidance, QueueBarProps, Theme, TranscriptProps, draw_transcript,
    };
    use nuo_view::model::layout::LayoutMap;

    let theme = Theme::default();
    let selection = SelectionState::default();
    let mut terminal = nuotc::TestTerminal::new(width, height);
    let mut layout_map = LayoutMap::new();
    terminal.draw(|f| {
        let _ = draw_transcript(
            f,
            &mut layout_map,
            TranscriptProps {
                messages,
                scroll: 0,
                selection: &selection,
                cell_selection: None,
                backoff: None,
                activity: "",
                awaiting_permission: false,
                spinner_phase: 0,
                input: "",
                chrome_hidden: false,
                queue_bar: QueueBarProps {
                    items: &[],
                    paused: false,
                    blocked: false,
                    expand_key: Some(crate::shell::keymap::Key::CTRL_Q),
                },
                tasks_bar: Default::default(),
                notice_dock: None,
                subagent_bar: None,
                page_hints: None,
                tab_bar: None,
                thread_id: None,
                thread_context: None,
                round_started_at: None,
                hovered_step: None,
                focused_target: None,
                logo: None,
                guidance: EmptyStateGuidance::Tour,
                carousel_index: 0,
                theme: &theme,
                layout: nuo_view::layout::Strategy::default(),
                height_cache: None,
            },
        );
    });
    let buffer = terminal.buffer();
    let bw = buffer.area().width as usize;
    (0..buffer.area().height as usize)
        .map(|row| {
            buffer.cells()[row * bw..(row + 1) * bw]
                .iter()
                .map(|cell| cell.symbol.as_str())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}
