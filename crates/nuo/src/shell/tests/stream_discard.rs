//! A discarded stream attempt must leave **no** provisional rows behind.
//!
//! A retried attempt reuses the same `(round, turn)`, and both
//! `append_stream_text_delta` and `append_reasoning_delta` resolve a target by
//! position alone. So any row that survives a discard is a row the *next*
//! attempt will append into — fusing two attempts into one block
//! (`"attempt one reasoningattempt two reasoning"`) with no separator to show
//! where one ended and the other began.
//!
//! An attempt can leave several rows: the thinking trace, the finalized copy of
//! that trace, and the answer text that was still streaming when the transport
//! died. Discarding only the last one orphans the rest.

use crate::shell::event_loop::apply::apply_transcript;
use crate::shell::event_loop::mutations::TranscriptEdit;
use crate::shell::tests::new_app_for_relay_tests;
use nuo_view::model::document::TranscriptMessage;

const ROUND: Option<u64> = Some(2);
const TURN: Option<u64> = Some(1);

/// The discard's position is a plain `(u64, u64)` — the state it destroys always
/// has one (ADR-0167).
const DISCARD_ROUND: u64 = 2;
const DISCARD_TURN: u64 = 1;

fn text(delta: &str) -> TranscriptEdit {
    TranscriptEdit::StreamTextDelta {
        round: ROUND,
        turn: TURN,
        delta: delta.to_string(),
        created: Some(
            TranscriptMessage::new(nuo_tool::Role::Assistant, delta)
                .with_round(2)
                .with_turn(1),
        ),
        clear_retry: false,
    }
}

fn reasoning(delta: &str) -> TranscriptEdit {
    TranscriptEdit::ReasoningDelta {
        round: ROUND,
        turn: TURN,
        delta: delta.to_string(),
        created: Some(
            TranscriptMessage::reasoning(delta)
                .with_round(2)
                .with_turn(1),
        ),
    }
}

fn reasoning_finalize(content: &str) -> TranscriptEdit {
    TranscriptEdit::ReasoningFinalize {
        round: ROUND,
        turn: TURN,
        content: content.to_string(),
        duration_ms: Some(1_200),
    }
}

fn discard() -> TranscriptEdit {
    TranscriptEdit::StreamDiscard {
        round: DISCARD_ROUND,
        turn: DISCARD_TURN,
    }
}

fn rows(app: &crate::shell::App) -> Vec<String> {
    app.transcript
        .messages
        .iter()
        .map(|m| m.raw.clone())
        .collect()
}

#[test]
fn discard_removes_a_lone_thinking_trace() {
    let mut app = new_app_for_relay_tests();
    apply_transcript(&mut app, reasoning("attempt one reasoning"));
    assert_eq!(app.transcript.messages.len(), 1);

    apply_transcript(&mut app, discard());

    assert!(
        app.transcript.messages.is_empty(),
        "the failed attempt's thinking trace must not survive: {:?}",
        rows(&app)
    );
}

#[test]
fn discard_removes_thinking_that_outlived_its_answer_text() {
    // Thinking streams, closes, then the answer text begins. The transport dies
    // mid-answer, so the transcript tail is the text row — discarding only that
    // one would orphan the thinking block.
    let mut app = new_app_for_relay_tests();
    apply_transcript(&mut app, reasoning("stale thinking"));
    apply_transcript(&mut app, text("partial answer"));
    assert_eq!(app.transcript.messages.len(), 2, "thinking + answer text");

    apply_transcript(&mut app, discard());

    assert!(
        app.transcript.messages.is_empty(),
        "the whole attempt must be discarded, not just its last row: {:?}",
        rows(&app)
    );
}

#[test]
fn retried_attempt_never_fuses_into_the_failed_attempt() {
    // The end-user-visible bug: two attempts' reasoning concatenated into a
    // single block, so the transcript claims the model thought both things in
    // one uninterrupted stretch.
    let mut app = new_app_for_relay_tests();

    // Attempt 1: thinking closes, text starts, transport dies.
    apply_transcript(&mut app, reasoning("attempt one reasoning"));
    apply_transcript(&mut app, reasoning_finalize("attempt one reasoning"));
    apply_transcript(&mut app, text("partial answer"));
    apply_transcript(&mut app, discard());

    // Attempt 2, same (round, turn), runs to completion.
    apply_transcript(&mut app, reasoning("attempt two reasoning"));
    apply_transcript(&mut app, reasoning_finalize("attempt two reasoning"));

    let traces: Vec<String> = app
        .transcript
        .messages
        .iter()
        .filter(|m| m.is_reasoning())
        .map(|m| m.raw.clone())
        .collect();

    assert_eq!(
        traces,
        vec!["attempt two reasoning".to_string()],
        "only the successful attempt's reasoning may remain"
    );
}

#[test]
fn discard_keeps_committed_dialogue_below_the_attempt() {
    // The sweep must stop at anything that is not this attempt's own
    // provisional output. A command result or a tool step is committed
    // dialogue; truncating beneath it would reorder history.
    let mut app = new_app_for_relay_tests();
    apply_transcript(&mut app, text("an earlier answer"));
    app.transcript
        .messages
        .push(TranscriptMessage::pending_command("delegate", "on"));
    apply_transcript(&mut app, reasoning("provisional thinking"));

    let before = rows(&app);
    assert_eq!(before.len(), 3, "answer + command result + thinking");

    apply_transcript(&mut app, discard());

    let after = rows(&app);
    assert_eq!(
        after.len(),
        2,
        "only the trailing provisional row goes, got {:?}",
        after
    );
    assert!(
        after.iter().any(|r| r.contains("delegate")),
        "the committed command result must survive, got {:?}",
        after
    );
}

#[test]
fn discard_is_a_no_op_when_the_attempt_streamed_nothing() {
    let mut app = new_app_for_relay_tests();
    apply_transcript(&mut app, text("an earlier turn's answer"));

    // A discard for a position that owns nothing must not eat unrelated rows.
    let mut unrelated = discard();
    if let TranscriptEdit::StreamDiscard { round, turn } = &mut unrelated {
        *round = 99;
        *turn = 99;
    }
    apply_transcript(&mut app, unrelated);

    assert_eq!(
        rows(&app),
        vec!["an earlier turn's answer".to_string()],
        "a discard must never truncate another turn's dialogue"
    );
}

#[test]
fn discard_reaches_text_beneath_a_still_pending_tool_step() {
    // Real event order on a mid-stream failure (orchestration sends the discard
    // *before* the retry): text streams, then a tool call is announced and its
    // arguments begin ingesting, then the transport dies.
    //
    //   [answer text (assistant, mine)] [tool step (Tool, Ingesting, mine)]
    //                                    ^ tail -> a tail-only sweep stops here
    //
    // The announced step is already *retracted* by the harness at this point
    // (`[INV-TOOL-ID-05]`, and again by the `RetryScheduled` sweep), so it must
    // not shield the answer text underneath it. Otherwise the retry appends into
    // the failed attempt's text row.
    let mut app = new_app_for_relay_tests();
    apply_transcript(&mut app, text("attempt one answer"));
    let mut step = TranscriptMessage::tool_step_announced("call_1", "bash");
    step.round = ROUND;
    step.turn = TURN;
    app.transcript.messages.push(step);
    assert_eq!(app.transcript.messages.len(), 2, "answer + announced step");

    apply_transcript(&mut app, discard());

    assert!(
        app.transcript.messages.is_empty(),
        "the retracted pending step must not shield the failed attempt's text: {:?}",
        rows(&app)
    );
}

#[test]
fn discard_never_truncates_beneath_a_dispatched_tool_step() {
    // The mirror of the test above: once the step has *dispatched*, it is real
    // work the user watched happen. The sweep must stop there, not truncate the
    // answer text out from under it.
    let mut app = new_app_for_relay_tests();
    apply_transcript(&mut app, text("an answer already shown"));
    let mut dispatched = TranscriptMessage::tool_step("call_1", "bash", "{}");
    dispatched.round = ROUND;
    dispatched.turn = TURN;
    app.transcript.messages.push(dispatched);
    let before = rows(&app);

    apply_transcript(&mut app, discard());

    assert_eq!(
        rows(&app),
        before,
        "a dispatched tool step is committed dialogue, not a provisional row"
    );
}

#[test]
fn wire_turn_zero_maps_to_the_first_transcript_turn() {
    // The wire position is 0-based (TurnStarted/StreamDiscard); transcript rows
    // are 1-based. A discard translated without this step owns no row and is a
    // silent no-op — the exact failure ADR-0167 removes.
    assert_eq!(crate::shell::transcript_turn(0), 1);
    assert_eq!(crate::shell::transcript_turn(7), 8);
    assert!(matches!(
        crate::shell::stream_discard_edit(2, 0),
        TranscriptEdit::StreamDiscard { round: 2, turn: 1 }
    ));
}
