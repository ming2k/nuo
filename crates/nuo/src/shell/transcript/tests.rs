use super::{
    merge_round_interrupt_rows, merge_timeline_rows, rebase_transcript_rounds,
    transcript_commands_from_ledger, transcript_interrupts_from_records,
    transcript_messages_from_core,
};
use tool::Role;
use view::model::document::TranscriptMessage;

#[test]
fn rebases_a_compacted_relative_tail_to_the_session_round_counter() {
    let mut messages = vec![
        TranscriptMessage::new(Role::User, "older").with_round(1),
        TranscriptMessage::new(Role::Assistant, "reply").with_round(2),
    ];

    rebase_transcript_rounds(&mut messages, 42);

    assert_eq!(messages[0].round, Some(41));
    assert_eq!(messages[1].round, Some(42));
}

/// An interrupted subagent survives a resume with its true classification and
/// its partial children intact: the persisted `subagent_meta.interrupted` flag
/// drives the `Interrupted` status (not `Ok` — the bare summary text cannot
/// say it), and the nested transcript rebuilds the drill-in view.
#[test]
fn restores_interrupted_subagent_with_children_and_status() {
    use tool::SubagentMeta;
    use tool::{Message, ToolCall};
    use view::tui_config::TuiConfig;

    let call = ToolCall {
        id: "call_9".to_string(),
        name: "spawn_agent".to_string(),
        arguments: r#"{"description":"d","prompt":"p"}"#.to_string(),
    };
    let inner_call = ToolCall {
        id: "inner_1".to_string(),
        name: "read".to_string(),
        arguments: "{}".to_string(),
    };
    // The subagent's partial internal transcript: one completed read round.
    let children = vec![
        Message {
            role: Role::Assistant,
            content: String::new(),
            tool_calls: Some(vec![inner_call.clone()]),
            ..Message::new(Role::Assistant, "")
        },
        Message::tool_result(&inner_call, "[read result]:\nfound 1 of 3"),
    ];
    let messages = vec![
        Message::new(Role::User, "research the handlers"),
        Message {
            role: Role::Assistant,
            content: String::new(),
            tool_calls: Some(vec![call.clone()]),
            ..Message::new(Role::Assistant, "")
        },
        Message::tool_result(
            &call,
            "[spawn_agent result]:\nInterrupted: stopped by the user",
        )
        .with_children(children)
        .with_subagent_meta(SubagentMeta {
            duration_ms: Some(42),
            failed: false,
            interrupted: true,
            ..Default::default()
        }),
    ];

    let restored = transcript_messages_from_core(messages, &TuiConfig::default());
    let step = restored
        .iter()
        .find(|m| m.is_subagent_task())
        .expect("the subagent step must be restored");
    assert_eq!(
        step.tool_step_status(),
        Some(view::model::document::ToolStepStatus::Interrupted),
        "restored interrupted subagent must classify as Interrupted, not Ok/Failed"
    );
    let kids = step.subagent_children().expect("children must be restored");
    assert_eq!(kids.len(), 1, "one completed child tool step expected");
    assert!(kids[0].is_tool_step());
    assert_eq!(
        kids[0].tool_step_status(),
        Some(view::model::document::ToolStepStatus::Ok),
        "the child read step completed normally"
    );
}

/// C11: interrupt markers re-project at their timestamp seam — before the
/// first user message sent *after* the stop — and a marker newer than
/// every seam lands at the tail (the process died mid-round).
#[test]
fn round_interrupt_markers_land_at_their_seams() {
    use protocol::{RoundInterrupt, RoundInterruptReason};

    let dialogue = vec![
        TranscriptMessage::new(Role::User, "first").with_sent_at_ms(1_000),
        TranscriptMessage::new(Role::Assistant, "reply 1"),
        TranscriptMessage::new(Role::User, "second").with_sent_at_ms(5_000),
        TranscriptMessage::new(Role::Assistant, "reply 2"),
    ];
    let markers = transcript_interrupts_from_records(vec![
        RoundInterrupt {
            reason: RoundInterruptReason::User,
            at_ms: 3_000,
            round: Some(1),
            detail: None,
        },
        RoundInterrupt {
            reason: RoundInterruptReason::Terminated,
            at_ms: 9_000,
            round: Some(2),
            detail: None,
        },
    ]);

    let merged = merge_round_interrupt_rows(dialogue, markers);

    // 6 rows: u1, a1, marker1, u2, a2, marker2(tail).
    assert_eq!(merged.len(), 6);
    assert!(merged[2].is_round_interrupt());
    assert_eq!(
        merged[2].sent_at_ms,
        Some(3_000),
        "mid-transcript marker keeps its own timestamp"
    );
    assert!(
        merged[5].is_round_interrupt(),
        "post-tail marker at the end"
    );
    assert!(!merged[1].is_round_interrupt(), "seam order preserved");
}

/// The marker row's raw text carries the round + reason vocabulary shared
/// with the live path.
#[test]
fn round_interrupt_marker_text_uses_shared_vocabulary() {
    use protocol::{RoundInterrupt, RoundInterruptReason};

    let marker = TranscriptMessage::round_interrupted(RoundInterrupt {
        reason: RoundInterruptReason::User,
        at_ms: 42,
        round: Some(3),
        detail: None,
    });
    assert_eq!(marker.raw, "Round 3 — cancelled via [Esc Esc]");

    let unnumbered = TranscriptMessage::round_interrupted(RoundInterrupt {
        reason: RoundInterruptReason::Terminated,
        at_ms: 42,
        round: None,
        detail: None,
    });
    assert_eq!(unnumbered.raw, "Process exited");

    let err_marker = TranscriptMessage::round_interrupted(RoundInterrupt {
        reason: RoundInterruptReason::Error,
        at_ms: 42,
        round: Some(4),
        detail: Some("Exhausted 30 retry attempts — Google HTTP 429 Too Many Requests".to_string()),
    });
    assert_eq!(
        err_marker.raw,
        "Exhausted 30 retry attempts — Google HTTP 429 Too Many Requests"
    );
}

/// Regression: in a multi-turn round where an error happened at 15:00,
/// `/retry` resumed the round at 16:36 without a new user message, and
/// subsequent turns ran through 16:59, switching tabs / rehydrating must
/// place `error` (15:00) then `/retry` (16:36) at the turn seam before the
/// resumed turns — never at the tail after 16:59 and never inverted.
#[test]
fn commands_and_errors_interleave_chronologically_between_assistant_turns() {
    use protocol::{CommandRecord, RoundInterrupt, RoundInterruptReason};

    let dialogue = vec![
        TranscriptMessage::new(Role::User, "investigate transcript")
            .with_round(1)
            .with_sent_at_ms(1_450_000),
        // Turn 1 (two components: reasoning + text) before the error.
        TranscriptMessage::reasoning("thinking before error")
            .with_round(1)
            .with_turn(1)
            .with_sent_at_ms(1_455_000),
        TranscriptMessage::new(Role::Assistant, "turn 1 reply")
            .with_round(1)
            .with_turn(1)
            .with_sent_at_ms(1_455_000),
        // Turn 2 resumed after /retry (reasoning + tool + text).
        TranscriptMessage::reasoning("thinking after retry")
            .with_round(1)
            .with_turn(2)
            .with_sent_at_ms(1_636_500),
        TranscriptMessage::tool_step("call_1", "search_text", "{}")
            .with_round(1)
            .with_turn(2)
            .with_sent_at_ms(1_636_500),
        TranscriptMessage::new(Role::Assistant, "turn 2 reply")
            .with_round(1)
            .with_turn(2)
            .with_sent_at_ms(1_636_500),
        // Turn 3 later in the same round.
        TranscriptMessage::new(Role::Assistant, "turn 3 reply")
            .with_round(1)
            .with_turn(3)
            .with_sent_at_ms(1_659_000),
    ];

    let commands = transcript_commands_from_ledger(vec![
        CommandRecord::new("retry", "").with_timestamp(1_636_000),
    ]);
    let interrupts = transcript_interrupts_from_records(vec![RoundInterrupt {
        reason: RoundInterruptReason::Error,
        at_ms: 1_500_000,
        round: Some(1),
        detail: Some("Provider stream failed".to_string()),
    }]);

    let merged = merge_timeline_rows(dialogue, commands, interrupts);
    let summary: Vec<(&str, Option<u64>)> = merged
        .iter()
        .map(|m| (m.raw.as_str(), m.sent_at_ms))
        .collect();

    assert_eq!(
        summary,
        vec![
            ("investigate transcript", Some(1_450_000)),
            ("thinking before error", Some(1_455_000)),
            ("turn 1 reply", Some(1_455_000)),
            ("Provider stream failed", Some(1_500_000)),
            ("/retry", Some(1_636_000)),
            ("thinking after retry", Some(1_636_500)),
            ("Search \"...\" in . (executing)", Some(1_636_500)),
            ("turn 2 reply", Some(1_636_500)),
            ("turn 3 reply", Some(1_659_000)),
        ]
    );
}

/// Components belonging to the same `(round, turn)` must never be split by
/// a command or interrupt, even when their timestamps match.
#[test]
fn timeline_merge_never_splits_components_of_the_same_turn() {
    use protocol::CommandRecord;

    let dialogue = vec![
        TranscriptMessage::reasoning("thought")
            .with_round(1)
            .with_turn(1)
            .with_sent_at_ms(2_000),
        TranscriptMessage::tool_step("call_1", "read", "{}")
            .with_round(1)
            .with_turn(1)
            .with_sent_at_ms(2_000),
        TranscriptMessage::new(Role::Assistant, "done")
            .with_round(1)
            .with_turn(1)
            .with_sent_at_ms(2_000),
        TranscriptMessage::new(Role::Assistant, "next turn")
            .with_round(1)
            .with_turn(2)
            .with_sent_at_ms(3_000),
    ];
    // Command stamped during turn 1 (after turn 1 started, before turn 2).
    let commands = transcript_commands_from_ledger(vec![
        CommandRecord::new("unattended", "on").with_timestamp(2_500),
    ]);

    let merged = merge_timeline_rows(dialogue, commands, Vec::new());
    let labels: Vec<&str> = merged.iter().map(|m| m.raw.as_str()).collect();
    assert_eq!(
        labels,
        vec![
            "thought",
            "Read file (executing)",
            "done",
            "/unattended on",
            "next turn"
        ]
    );
}

/// When both commands and interrupts land at the tail (or share a timestamp),
/// they sort chronologically with each other, with interrupts before
/// commands on an exact tie.
#[test]
fn tail_commands_and_interrupts_sort_chronologically_together() {
    use protocol::{CommandRecord, RoundInterrupt, RoundInterruptReason};

    let dialogue = vec![
        TranscriptMessage::new(Role::User, "hello")
            .with_round(1)
            .with_sent_at_ms(1_000),
    ];
    let commands = transcript_commands_from_ledger(vec![
        CommandRecord::new("retry", "").with_timestamp(3_000),
        CommandRecord::new("compact", "").with_timestamp(2_000),
    ]);
    let interrupts = transcript_interrupts_from_records(vec![RoundInterrupt {
        reason: RoundInterruptReason::Error,
        at_ms: 2_000,
        round: Some(1),
        detail: Some("stream failed".to_string()),
    }]);

    let merged = merge_timeline_rows(dialogue, commands, interrupts);
    let labels: Vec<&str> = merged.iter().map(|m| m.raw.as_str()).collect();
    assert_eq!(labels, vec!["hello", "stream failed", "/compact", "/retry"]);
}
