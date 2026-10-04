//! Transcript rendering tests: streamed deltas, hidden reasoning, command results and ledger rows, coalescing, drafts and outbox.

use super::*;

#[test]
fn only_high_frequency_stream_updates_are_coalesced() {
    let stream_delta = AgentResponse::Round {
        session_id: "session".to_string(),
        event: RoundEvent::StreamDelta("hello".to_string()),
    };
    let tool_stream = AgentResponse::Round {
        session_id: "session".to_string(),
        event: RoundEvent::ToolStream {
            id: "call".to_string(),
            stream: nuo_wire::ToolStream::Stdout("line\n".to_string()),
        },
    };
    let stream_start = AgentResponse::Round {
        session_id: "session".to_string(),
        event: RoundEvent::StreamStart,
    };
    let stream_end = AgentResponse::Round {
        session_id: "session".to_string(),
        event: RoundEvent::StreamEnd("done".to_string()),
    };

    assert!(is_coalescible_stream_update(&stream_delta));
    assert!(is_coalescible_stream_update(&tool_stream));
    assert!(!is_coalescible_stream_update(&stream_start));
    assert!(!is_coalescible_stream_update(&stream_end));
}

#[test]
fn streamed_text_is_appended_only_to_the_current_turn() {
    let mut messages = vec![TranscriptMessage::new(Role::Assistant, "older").with_turn(1)];
    let older_id = messages[0].id;

    assert_eq!(
        append_stream_text_delta(&mut messages, None, Some(2), "new"),
        None
    );
    assert_eq!(
        messages.len(),
        1,
        "a new round requires structural insertion"
    );
    assert_eq!(messages[0].raw, "older");

    messages.push(TranscriptMessage::new(Role::Assistant, "first").with_turn(2));
    let current_id = messages[1].id;
    assert_eq!(
        append_stream_text_delta(&mut messages, None, Some(2), " second"),
        Some(current_id)
    );
    assert_eq!(messages[0].id, older_id);
    assert_eq!(messages[0].raw, "older");
    assert_eq!(messages[1].raw, "first second");
}

#[test]
fn hidden_reasoning_needs_no_assistant_placeholder() {
    // StreamStart is lifecycle-only. If a hidden-chain reasoning delta is
    // ignored, the transcript remains exactly as it was: no zero-height message
    // can introduce a second semantic separator before the next visible item.
    let mut messages = vec![TranscriptMessage::new(Role::User, "question").with_round(1)];
    let before = messages[0].id;

    begin_stream(&mut messages);
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].id, before);
}

#[test]
fn command_ledger_restores_as_non_conversational_command_rows() {
    // ADR-0091: the command ledger is the durable source of truth for
    // commands on resume — the message stream is pure dialogue. Each record
    // restores as a distinct, non-conversational command row carrying the
    // typed result as its expandable body.
    let commands = vec![
        nuo_wire::CommandRecord::new("search", "foo").with_result(
            nuo_wire::CommandResult::Search {
                query: "foo".to_string(),
                hits: vec![],
            },
        ),
        // A result-less record (legacy fold / command without result): the
        // invocation still restores, with an empty expandable body.
        nuo_wire::CommandRecord::new("compact", ""),
    ];
    let restored = transcript_commands_from_ledger(commands);
    assert_eq!(restored.len(), 2);
    let search = &restored[0];
    assert!(
        search.is_command_result(),
        "command rows carry the CommandResult kind"
    );
    assert_eq!(
        search.command_result_summary().as_deref(),
        Some("/search foo")
    );
    assert_eq!(
        search.command_result_text().as_deref(),
        Some("No relevant history found.")
    );
    assert_eq!(search.round, None, "a command is not a conversation turn");
    assert_ne!(
        search.role,
        nuo_wire::Role::Assistant,
        "never assistant prose"
    );

    let compact = &restored[1];
    assert!(compact.is_command_result());
    assert_eq!(
        compact.command_result_summary().as_deref(),
        Some("/compact")
    );
    assert_eq!(compact.command_result_text(), None);
}

#[test]
fn command_error_settles_pending_command_in_place() {
    use crate::model::document::{CommandPhase, TranscriptMessage};

    let mut message = TranscriptMessage::pending_command("trust", "workspace");
    assert_eq!(message.command_result_phase(), Some(CommandPhase::Pending));
    assert_eq!(message.command_result_text(), None);

    let settled = message.settle_command_result(nuo_wire::CommandResult::Error {
        message: "Unknown /trust subcommand 'workspace'.".to_string(),
        detail: None,
    });
    assert!(settled);
    assert_eq!(
        message.command_result_phase(),
        Some(CommandPhase::Completed)
    );
    assert_eq!(
        message.command_result_text().as_deref(),
        Some("Error: Unknown /trust subcommand 'workspace'.")
    );
}

/// ADR-0106 §2: on resume, command rows merge at their turn seams — a
/// command issued between two prompts renders between those rounds, exactly
/// where it appeared live — instead of all appending to the dialogue's tail.
#[test]
fn command_rows_merge_at_their_turn_seams_on_restore() {
    use crate::model::document::TranscriptMessage;
    use crate::transcript::merge_command_rows;
    use nuo_wire::Role;

    // Dialogue: two rounds, timestamps 1000 and 3000.
    let dialogue = vec![
        TranscriptMessage::new(Role::User, "first prompt")
            .with_sent_at_ms(1000)
            .with_origin(crate::model::document::UserMessageOrigin::Chat),
        TranscriptMessage::new(Role::Assistant, "first reply"),
        TranscriptMessage::new(Role::User, "second prompt")
            .with_sent_at_ms(3000)
            .with_origin(crate::model::document::UserMessageOrigin::Chat),
        TranscriptMessage::new(Role::Assistant, "second reply"),
    ];
    // Ledger: one command run between the rounds (2000), one after the last
    // (4000). Rebuild stamps each row with the record's timestamp.
    let commands = crate::transcript::transcript_commands_from_ledger(vec![
        nuo_wire::CommandRecord::new("compact", "")
            .with_result(nuo_wire::CommandResult::Ack {
                title: "Compacted".to_string(),
                detail: None,
            })
            .with_timestamp(2000),
        nuo_wire::CommandRecord::new("new", "")
            .with_result(nuo_wire::CommandResult::Text(
                "Started new session: c3".to_string(),
            ))
            .with_timestamp(4000),
    ]);

    let merged = merge_command_rows(dialogue, commands);
    let texts: Vec<&str> = merged.iter().map(|m| m.raw.as_str()).collect();
    assert_eq!(
        texts,
        vec![
            "first prompt",
            "first reply",
            "/compact",
            "second prompt",
            "second reply",
            "/new",
        ],
        "the mid-round command lands at its seam; the post-dialogue one at the tail"
    );
}

/// ADR-0106 §2: dialogue with no user timestamps is never reordered — the
/// command rows keep ledger order at the tail rather than guessing.
#[test]
fn command_rows_tail_when_dialogue_has_no_timestamps() {
    use crate::model::document::TranscriptMessage;
    use crate::transcript::merge_command_rows;
    use nuo_wire::Role;

    let dialogue = vec![
        TranscriptMessage::new(Role::User, "prompt"),
        TranscriptMessage::new(Role::Assistant, "reply"),
    ];
    let commands = crate::transcript::transcript_commands_from_ledger(vec![
        nuo_wire::CommandRecord::new("compact", "").with_timestamp(123),
    ]);

    let merged = merge_command_rows(dialogue, commands);
    let texts: Vec<&str> = merged.iter().map(|m| m.raw.as_str()).collect();
    assert_eq!(texts, vec!["prompt", "reply", "/compact"]);
}

#[test]
fn command_result_message_expands_and_round_trips_display() {
    // The command block's collapsed header is the invocation; expansion
    // reveals the typed result body. Pinning is respected (user toggle wins).
    use crate::model::document::TranscriptMessage;
    let mut message = TranscriptMessage::command_result(
        "permissions",
        "",
        Some(nuo_wire::CommandResult::PermissionList {
            allowed: vec!["run_command".to_string()],
        }),
    );
    assert_eq!(message.command_result_expanded(), Some(false));
    assert_eq!(
        message.command_result_summary().as_deref(),
        Some("/permissions")
    );
    assert_eq!(
        message.command_result_text().as_deref(),
        Some("Always-allowed tools:\n- run_command")
    );
    // The result body is the message's parsed blocks (non-empty here).
    assert!(!message.blocks.is_empty());

    message.pin_command_result_expanded(true);
    assert_eq!(message.command_result_expanded(), Some(true));
}

/// ADR-0106: the command row's layout follows the shape of the reply — a
/// a short single line joins inline with whitespace, anything longer discloses, and a
/// missing result renders plain. The classifier is width-aware so an inline
/// reply is never a truncated fragment.
#[test]
fn command_row_layout_classifies_by_result_shape() {
    use crate::model::document::{CommandRowLayout, TranscriptMessage};

    // No result (legacy fold / command without result) → Plain.
    let plain_cmd = TranscriptMessage::command_result("compact", "", None);
    assert_eq!(
        plain_cmd.command_row_layout(80),
        Some(CommandRowLayout::Plain),
        "a result-less record has nothing to disclose"
    );

    // Short single-line reply fits beside the invocation → Inline.
    let fresh = TranscriptMessage::command_result(
        "new",
        "",
        Some(nuo_wire::CommandResult::Text(
            "Started new session: a1b2c3".to_string(),
        )),
    );
    assert_eq!(fresh.command_row_layout(80), Some(CommandRowLayout::Inline));

    // The same reply in a narrow band must NOT inline — it would truncate.
    assert_eq!(
        fresh.command_row_layout(12),
        Some(CommandRowLayout::Disclose),
        "an inline reply that would truncate discloses instead"
    );

    // Multi-line replies always disclose, at any width.
    let permissions = TranscriptMessage::command_result(
        "permissions",
        "",
        Some(nuo_wire::CommandResult::PermissionList {
            allowed: vec!["run_command".to_string(), "edit_text".to_string()],
        }),
    );
    assert_eq!(
        permissions.command_row_layout(200),
        Some(CommandRowLayout::Disclose),
        "a multi-line result always earns the disclosure affordance"
    );
}

#[test]
fn tool_activity_is_semantic_and_loop_progress_is_preserved() {
    use crate::phase::{Phase, ToolVerb};
    assert_eq!(
        event_loop::tool_verb_for("search_text"),
        ToolVerb::Searching
    );
    assert_eq!(
        Phase::Tool(event_loop::tool_verb_for("edit_text")).label(),
        "making edits"
    );
    assert_eq!(
        event_loop::tool_verb_for("mcp__github__search"),
        ToolVerb::Mcp
    );
    let running = Phase::Tool(ToolVerb::Running);
    assert_eq!(
        display_status(LoopStatus::Running, Some(&running)),
        "running command"
    );
}

#[test]
fn queued_dispatch_carries_text_and_images() {
    // Smoke-check the struct's fields are wired as expected by the
    // SendChat and recall paths. Locks the field names + types so a
    // refactor can't quietly drop the images payload.
    let d = QueuedDispatch {
        id: "message-1".to_string(),
        session_id: "session-a".to_string(),
        state: QueuedDispatchState::Waiting,
        text: "hello".to_string(),
        queued_at_ms: 0,
        images: vec![nuo_wire::ImagePart {
            mime: "image/png".to_string(),
            data: "base64".to_string(),
        }],
        text_pastes: Vec::new(),
    };
    assert_eq!(d.text, "hello");
    assert_eq!(d.images.len(), 1);
    assert_eq!(d.images[0].mime, "image/png");
}

#[test]
fn outbox_count_and_fifo_dispatch_are_session_scoped() {
    // Every staged message is a next-round item (the insert/next-round
    // distinction was removed), so the outbox count is a single per-session
    // tally and FIFO dispatch is driven purely by `Waiting` state + session.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.pending_dispatch
        .push_back(queued_dispatch("b-next", "session-b", "other"));
    app.pending_dispatch
        .push_back(queued_dispatch("a-next", "session-a", "follow up"));

    assert_eq!(app.pending_count("session-a"), 1);
    assert_eq!(app.pending_count("session-b"), 1);
    let dispatch = app
        .begin_next_round_dispatch("session-a")
        .expect("session-a follow-up");
    assert_eq!(dispatch.id, "a-next");
    assert_eq!(app.pending_dispatch[0].id, "b-next");
}

/// The pointer model's "newest slot = unsent input" invariant: once a draft
/// is successfully sent it is historicised, so the remembered draft slot is
/// cleared and a later ↑→↓ round-trip must NOT bring the old draft back —
/// it returns to an empty composer (the fresh unsent slot).
#[tokio::test]
async fn sending_clears_the_remembered_draft() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "session-a".to_string();
    app.current_workspace = "~/p".to_string();
    app.record_input_history("older prompt".to_string(), Vec::new(), Vec::new());

    // Compose a draft, walk ↑ into history and back ↓ — the draft is stashed
    // in the remembered slot and restored into the composer.
    app.input = "my draft".to_string();
    let rows = app.current_session_history();
    assert!(app.history_prev(&rows));
    assert!(!app.history_next(&rows));
    assert_eq!(
        app.input, "my draft",
        "draft content restored into the composer on ↓ past the newest row"
    );
    // The remembered slot is a *lazy* stash (shell-style): after a restore the
    // content lives in the composer and the slot is re-stashed on the next ↑,
    // so the slot may be empty here without losing anything.

    // Send: the input is taken out of the composer, historicised (recorded),
    // and the draft slot cleared — exactly the SendChat handler's sequence.
    let sent = std::mem::take(&mut app.input);
    app.record_input_history(sent, Vec::new(), Vec::new());
    app.clear_history_draft();
    assert!(app.history_draft.is_empty());
    assert!(app.input.is_empty(), "composer is blank after a send");

    // A later ↑→↓ round-trip restores an empty composer, not the sent text.
    let rows = app.current_session_history();
    assert!(app.history_prev(&rows));
    assert!(!app.history_next(&rows));
    assert_eq!(app.input, "", "no stale draft may return after a send");
}

/// The Phase-1 unsend restore is asynchronous, not a user gesture: it must
/// never clobber a draft the user is composing while the round ran. The
/// interrupted prompt stays recoverable via the recorded history instead.
#[tokio::test]
async fn unsent_restore_preserves_in_progress_draft() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "session-a".to_string();
    app.current_workspace = "~/p".to_string();

    // The user was mid-composition when the interrupt landed.
    app.input = "half-typed next message".to_string();
    app.adopt_as_draft(
        "interrupted input".to_string(),
        Vec::new(),
        Vec::new(),
        crate::app::DraftAdoption::OnlyIfIdle,
    );

    assert_eq!(
        app.input, "half-typed next message",
        "the in-progress draft wins over the async restore"
    );
    assert_eq!(app.history_index, None);

    // An idle composer still adopts as before.
    app.input.clear();
    app.adopt_as_draft(
        "interrupted input".to_string(),
        Vec::new(),
        Vec::new(),
        crate::app::DraftAdoption::OnlyIfIdle,
    );
    assert_eq!(app.input, "interrupted input");

    // Explicit gestures (queue recall) keep replacing even a busy composer.
    app.input = "another half-typed".to_string();
    app.adopt_as_draft(
        "recalled from queue".to_string(),
        Vec::new(),
        Vec::new(),
        crate::app::DraftAdoption::Replace,
    );
    assert_eq!(app.input, "recalled from queue");
}

#[test]
fn render_frame_computes_max_scroll_for_overflowing_transcript() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "test-session".to_string();

    // Create enough messages to overflow the viewport
    let mut messages = Vec::new();
    for i in 0..50 {
        messages.push(TranscriptMessage::new(
            Role::User,
            format!("Message line {i}: some reasonably long content that takes up vertical space in the transcript area"),
        ));
    }
    app.messages = messages;

    let mut terminal = nuotc::TestTerminal::new(80, 24);
    terminal.draw(|f| {
        crate::event_loop::render_frame(&mut app, f, "test-session");
    });

    assert!(
        app.content_lines > app.view_height as usize,
        "content should exceed view height"
    );
    assert!(
        app.max_scroll > 0,
        "max_scroll should be non-zero when content overflows viewport"
    );
    assert_eq!(
        app.max_scroll,
        app.content_lines
            .saturating_sub(app.view_height as usize)
            .min(u16::MAX as usize) as u16,
        "max_scroll must equal content_lines - view_height"
    );
}

#[tokio::test]
async fn interrupt_marks_in_flight_prompt_cancelled_without_retracting() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    app.tx = tx;
    app.current_session_id = "session-a".to_string();
    app.running_sessions.insert("session-a".to_string());
    app.phase = Some(crate::phase::Phase::Queued);
    app.round_started_at = Some(std::time::Instant::now());

    // User prompt is sending in transcript
    let prompt = crate::model::document::TranscriptMessage::new(
        nuo_wire::Role::User,
        "write a fibonacci function",
    )
    .sending();
    app.messages.push(prompt);

    // Press Esc Esc to interrupt
    app.esc_armed_until = Some(std::time::Instant::now() + std::time::Duration::from_secs(1));
    crate::event_loop::handle_esc_interrupt(&mut app, false);

    // Verify interrupt request was sent to agent
    let req = rx.try_recv().expect("must send interrupt request");
    assert!(matches!(req, nuo_wire::AgentRequest::Interrupt));

    // UI state is immediately cleared (not blocked)
    assert!(!app.running_sessions.contains("session-a"));
    assert!(!app.viewed_chrome().responding);

    // The prompt is NOT popped/retracted — it stays in the transcript!
    assert_eq!(app.messages.len(), 1);
    assert_eq!(
        app.messages[0].delivery,
        crate::model::document::DeliveryStatus::Cancelled,
        "prompt status must transition to Cancelled"
    );

    // Composer input is NOT clobbered with the cancelled prompt
    assert!(app.input.is_empty());
}

#[tokio::test]
async fn interrupted_round_preserves_retry_history_and_appends_interrupt_entry() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let runtime = crate::event_loop::UiRuntime::minimal_for_test();

    // 1. User prompt is delivered
    let prompt =
        crate::model::document::TranscriptMessage::new(nuo_wire::Role::User, "calculate pi")
            .sending();
    app.messages.push(prompt);

    // 2. Provider retry occurs
    let retry_at = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let retry_msg = crate::model::document::TranscriptMessage::provider_retry(
        2,
        5,
        retry_at,
        "Anthropic HTTP 529: Overloaded",
    );
    app.messages.push(retry_msg);
    assert_eq!(app.messages.len(), 2);
    assert!(app.messages[1].is_provider_retry());

    // 3. User interrupts via Esc Esc -> RoundInterrupted event applied
    let record = nuo_wire::RoundInterrupt {
        reason: nuo_wire::RoundInterruptReason::User,
        round: Some(1),
        at_ms: 1_234_567,
        detail: None,
    };
    crate::event_loop::apply::apply(
        &mut app,
        &runtime,
        crate::event_loop::AppMutation::Transcript {
            buffer: crate::event_loop::Buffer::Primary,
            edit: crate::event_loop::TranscriptEdit::Interrupted { record },
        },
    );

    // 4. Verify messages:
    //    [0] User prompt (cancelled)
    //    [1] Settled provider retry failure notice (NOT deleted!)
    //    [2] Interrupt entry ("Round 1 — cancelled via [Esc Esc]")
    assert_eq!(
        app.messages.len(),
        3,
        "retry entry must be preserved alongside interrupt entry"
    );

    assert_eq!(
        app.messages[0].delivery,
        crate::model::document::DeliveryStatus::Cancelled
    );

    // Settled retry entry checks
    let retry_entry = &app.messages[1];
    assert!(
        !retry_entry.is_provider_retry(),
        "settled retry is no longer an active ticking retry"
    );
    assert!(
        retry_entry.is_notice(),
        "settled retry becomes a static notice"
    );
    let crate::model::document::MessageKind::Notice {
        severity,
        ref parts,
        ..
    } = retry_entry.kind
    else {
        panic!("expected Notice kind");
    };
    assert_eq!(severity, crate::model::document::NoticeSeverity::Warning);
    let parts = parts.as_ref().expect("expected parts");
    assert_eq!(parts.topic.as_deref(), Some("retry"));
    assert_eq!(parts.title, "Provider request failed (attempt 2/5)");
    assert_eq!(
        parts.detail.as_deref(),
        Some("Anthropic HTTP 529: Overloaded")
    );

    // Interrupt entry checks
    let interrupt_entry = &app.messages[2];
    assert!(interrupt_entry.is_round_interrupt());
    assert_eq!(interrupt_entry.raw, "Round 1 — cancelled via [Esc Esc]");

    // 5. Subsequent RetainNotRetry (e.g. from a future round completion) must NOT drop the settled retry
    crate::event_loop::apply::apply(
        &mut app,
        &runtime,
        crate::event_loop::AppMutation::Transcript {
            buffer: crate::event_loop::Buffer::Primary,
            edit: crate::event_loop::TranscriptEdit::RetainNotRetry,
        },
    );
    assert_eq!(
        app.messages.len(),
        3,
        "settled retry must survive RetainNotRetry"
    );
}

#[tokio::test]
async fn tool_result_does_not_forge_reasoning_phase() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let runtime = crate::event_loop::UiRuntime::minimal_for_test();

    // Start with a tool running phase
    crate::event_loop::apply::apply(
        &mut app,
        &runtime,
        crate::event_loop::AppMutation::SetPhase(Some(crate::phase::Phase::Tool(
            crate::phase::ToolVerb::Running,
        ))),
    );
    assert_eq!(
        app.phase,
        Some(crate::phase::Phase::Tool(crate::phase::ToolVerb::Running))
    );

    // After tool finishes, phase transitions to Preparing, NEVER Reasoning ("thinking")
    crate::event_loop::apply::apply(
        &mut app,
        &runtime,
        crate::event_loop::AppMutation::SetPhase(Some(crate::phase::Phase::Preparing)),
    );
    assert_ne!(
        app.phase,
        Some(crate::phase::Phase::Reasoning),
        "tool completion must not set thinking/reasoning phase"
    );
    assert_eq!(app.phase, Some(crate::phase::Phase::Preparing));
    assert_eq!(app.phase.as_ref().unwrap().label(), "preparing context");
}
