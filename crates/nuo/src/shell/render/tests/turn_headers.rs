//! Turn-band and user-round header metadata: the `HH:MM` send time.
//!
//! Both header kinds are a two-tone [`MetaStrip`](nuo_ui_primitives::components::meta_strip::MetaStrip):
//! an info-tone anchor (`round N` / `turn N`) plus a right-aligned muted
//! timestamp. The timestamp is *not* chrome the renderer may invent — it reads
//! [`TranscriptMessage::sent_at_ms`], which is the entry's own record of when it
//! was sent. An entry that never got stamped therefore silently loses the only
//! temporal fact its header carries, and a reader cannot tell when the message
//! was produced.
//!
//! These tests pin the stamp → header contract at the render boundary: given a
//! stamped entry the time is painted, and every component of one turn agrees on
//! one value (a turn emits thinking, text and tools, but its band shows exactly
//! one clock time).

// Integration test target: assertions fail by panicking, so the production
// no-panic lints are meaningless here (the exemption ends at the test boundary).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;
use nuo_ui_primitives::time::sent_time_label;

/// The same renderer the transcript uses, exposing rows as strings.
fn grid_of(messages: &[TranscriptMessage], width: u16, height: u16) -> String {
    let terminal = render_full_view(width, height, messages, None);
    let buffer = terminal.buffer();
    let bw = buffer.area().width as usize;
    (0..buffer.area().height as usize)
        .map(|row| {
            let line: String = buffer.cells()[row * bw..(row + 1) * bw]
                .iter()
                .map(|cell| cell.symbol.as_str())
                .collect();
            line.trim_end().to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn header_row<'a>(grid: &'a str, anchor: &str) -> &'a str {
    grid.lines()
        .find(|row| row.contains(anchor))
        .unwrap_or_else(|| panic!("no header row for {anchor} in:\n{grid}"))
}

/// A finished tool step carrying a structured payload, i.e. what
/// `finish_tool_step` produces in production (so the step renders its result
/// rather than a bare invocation).
fn finished_step(name: &str, arguments: &str, text: &str) -> TranscriptMessage {
    let structured = nuo_wire::ToolOutput::Code {
        lang: None,
        text: text.to_string(),
        start_line: 1,
        prefix: None,
        suffix: None,
    };
    let mut step = TranscriptMessage::tool_step("call_test", name, arguments);
    step.finish_tool_step("call_test", text, structured, 0);
    step
}

const EPOCH_MS: u64 = 1_700_000_000_000;

#[test]
fn user_round_header_paints_its_send_time() {
    let message = TranscriptMessage::new(nuo_wire::Role::User, "hello there")
        .with_round(1)
        .with_sent_at_ms(EPOCH_MS);
    let grid = grid_of(&[message], 72, 12);
    let row = header_row(&grid, "› round 1");
    assert!(
        row.contains(&sent_time_label(EPOCH_MS)),
        "the user round header must carry the entry's send time:\n{grid}"
    );
}

/// The defect this module exists for: a turn whose **first** component is a
/// reasoning trace (every reasoning channel, every tool turn that thinks first)
/// showed `› turn N  model effort` with no clock at all, because only the
/// assistant-text entry was stamped. The header must show the turn's time
/// regardless of which component happens to open the band.
#[test]
fn turn_header_paints_its_send_time_when_thinking_opens_the_band() {
    let mut thinking = TranscriptMessage::reasoning("inspect the module first");
    thinking.round = Some(1);
    thinking.turn = Some(1);
    thinking.set_reasoning_duration(10);
    thinking.sent_at_ms = Some(EPOCH_MS);

    let mut step = finished_step("read_text", r#"{"path":"a.rs"}"#, "x")
        .with_round(1)
        .with_turn(1)
        .with_attribution("anthropic", "claude-sonnet");
    step.sent_at_ms = Some(EPOCH_MS);

    let grid = grid_of(&[thinking, step], 72, 16);
    let row = header_row(&grid, "› turn 1");
    assert!(
        row.contains(&sent_time_label(EPOCH_MS)),
        "a turn band opened by a reasoning trace must still show its send time:\n{grid}"
    );
}

/// A tool-only turn: no text, no thinking, so the announced/tool step is the
/// only header owner. It carries the turn's time like every other component.
#[test]
fn turn_header_paints_its_send_time_for_a_tool_only_turn() {
    let mut step = finished_step("read_text", r#"{"path":"a.rs"}"#, "x")
        .with_round(1)
        .with_turn(1)
        .with_attribution("anthropic", "claude-sonnet");
    step.sent_at_ms = Some(EPOCH_MS);

    let grid = grid_of(&[step], 72, 12);
    let row = header_row(&grid, "› turn 1");
    assert!(
        row.contains(&sent_time_label(EPOCH_MS)),
        "a tool-only turn band must show its send time:\n{grid}"
    );
}

/// One turn, one clock: thinking, the tool batch and the closing assistant text
/// are three components but a single band, so they must all agree on the time.
#[test]
fn every_component_of_one_turn_shares_the_same_time() {
    let stamp = |mut message: TranscriptMessage| {
        message.sent_at_ms = Some(EPOCH_MS);
        message.round = Some(1);
        message.turn = Some(1);
        message
    };

    let mut thinking = TranscriptMessage::reasoning("think");
    thinking.set_reasoning_duration(5);
    let thinking = stamp(thinking);

    let step = finished_step("read_text", r#"{"path":"a.rs"}"#, "x")
        .with_attribution("anthropic", "claude-sonnet");
    let step = stamp(step);

    let text = stamp(
        TranscriptMessage::new(nuo_wire::Role::Assistant, "done")
            .with_attribution("anthropic", "claude-sonnet"),
    );

    let grid = grid_of(&[thinking, step, text], 72, 20);
    let label = sent_time_label(EPOCH_MS);
    let headers: Vec<&str> = grid
        .lines()
        .filter(|row| row.contains("› turn 1"))
        .collect();
    assert_eq!(
        headers.len(),
        1,
        "one turn paints exactly one band header:\n{grid}"
    );
    assert!(
        headers[0].contains(&label),
        "the band header must carry the turn's send time:\n{grid}"
    );
}

/// An unstamped legacy entry still renders a clean header — the time is absent,
/// not fabricated. This is the negative control for the assertions above.
#[test]
fn an_unstamped_entry_renders_no_time_at_all() {
    let mut step = finished_step("read_text", r#"{"path":"a.rs"}"#, "x")
        .with_round(1)
        .with_turn(1)
        .with_attribution("anthropic", "claude-sonnet");
    step.sent_at_ms = None;

    let grid = grid_of(&[step], 72, 12);
    let row = header_row(&grid, "› turn 1");
    assert!(
        !row.contains(':'),
        "an unstamped header must not invent a clock:\n{grid}"
    );
    assert_eq!(row.trim(), "› turn 1  claude-sonnet");
}
