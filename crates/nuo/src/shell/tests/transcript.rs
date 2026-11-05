//! Transcript rendering tests: streamed deltas, hidden reasoning, command results and ledger rows, coalescing, drafts and outbox.

// Integration test target: assertions fail by panicking, so the production
// no-panic lints are meaningless here (the exemption ends at the test boundary).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;

#[test]
fn only_high_frequency_stream_updates_are_coalesced() {
    let stream_delta = AgentResponse::Round {
        thread_id: "thread".to_string(),
        event: RoundEvent::StreamDelta("hello".to_string()),
    };
    let tool_stream = AgentResponse::Round {
        thread_id: "thread".to_string(),
        event: RoundEvent::ToolStream {
            id: "call".to_string(),
            stream: nuo_protocol::ToolStream::Stdout("line\n".to_string()),
        },
    };
    let stream_start = AgentResponse::Round {
        thread_id: "thread".to_string(),
        event: RoundEvent::StreamStart,
    };
    let stream_end = AgentResponse::Round {
        thread_id: "thread".to_string(),
        event: RoundEvent::StreamEnd("done".to_string()),
    };
    let tool_input_progress = AgentResponse::Round {
        thread_id: "thread".to_string(),
        event: RoundEvent::ToolInputProgress {
            index: 0,
            id: None,
            bytes: 4096,
            arguments: None,
        },
    };
    let tool_call_started = AgentResponse::Round {
        thread_id: "thread".to_string(),
        event: RoundEvent::ToolCallStarted {
            index: 0,
            id: None,
            name: "edit_text".to_string(),
        },
    };

    assert!(is_coalescible_stream_update(&stream_delta));
    assert!(is_coalescible_stream_update(&tool_stream));
    // ADR-0026: count-only progress rides the frame budget...
    assert!(is_coalescible_stream_update(&tool_input_progress));
    // ...but the one-shot announcement must wake the loop immediately.
    assert!(!is_coalescible_stream_update(&tool_call_started));
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
    // The command ledger is the durable source of truth for
    // commands on resume — the message stream is pure dialogue. Each record
    // restores as a distinct, non-conversational command row carrying the
    // typed result as its expandable body.
    let commands = vec![
        nuo_protocol::CommandRecord::new("search", "foo").with_result(
            nuo_protocol::CommandResult::Search {
                query: "foo".to_string(),
                hits: vec![],
            },
        ),
        // A result-less record (legacy fold / command without result): the
        // invocation still restores, with an empty expandable body.
        nuo_protocol::CommandRecord::new("compact", ""),
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
    assert_eq!(search.round, None, "a command is not a thread turn");
    assert_ne!(
        search.role,
        nuo_protocol::Role::Assistant,
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
    use nuo_view::model::document::{CommandPhase, TranscriptMessage};

    let mut message = TranscriptMessage::pending_command("trust", "workspace");
    assert_eq!(message.command_result_phase(), Some(CommandPhase::Pending));
    assert_eq!(message.command_result_text(), None);

    let settled = message.settle_command_result(nuo_protocol::CommandResult::Error {
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

/// On resume, command rows merge at their turn seams — a
/// command issued between two prompts renders between those rounds, exactly
/// where it appeared live — instead of all appending to the dialogue's tail.
#[test]
fn command_rows_merge_at_their_turn_seams_on_restore() {
    use crate::shell::transcript::merge_command_rows;
    use nuo_protocol::Role;
    use nuo_view::model::document::TranscriptMessage;

    // Dialogue: two rounds, timestamps 1000 and 3000.
    let dialogue = vec![
        TranscriptMessage::new(Role::User, "first prompt")
            .with_sent_at_ms(1000)
            .with_origin(nuo_view::model::document::UserMessageOrigin::Chat),
        TranscriptMessage::new(Role::Assistant, "first reply"),
        TranscriptMessage::new(Role::User, "second prompt")
            .with_sent_at_ms(3000)
            .with_origin(nuo_view::model::document::UserMessageOrigin::Chat),
        TranscriptMessage::new(Role::Assistant, "second reply"),
    ];
    // Ledger: one command run between the rounds (2000), one after the last
    // (4000). Rebuild stamps each row with the record's timestamp.
    let commands = crate::shell::transcript::transcript_commands_from_ledger(vec![
        nuo_protocol::CommandRecord::new("compact", "")
            .with_result(nuo_protocol::CommandResult::Ack {
                title: "Compacted".to_string(),
                detail: None,
            })
            .with_timestamp(2000),
        nuo_protocol::CommandRecord::new("new", "")
            .with_result(nuo_protocol::CommandResult::Text(
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

/// Dialogue with no user timestamps is never reordered — the
/// command rows keep ledger order at the tail rather than guessing.
#[test]
fn command_rows_tail_when_dialogue_has_no_timestamps() {
    use crate::shell::transcript::merge_command_rows;
    use nuo_protocol::Role;
    use nuo_view::model::document::TranscriptMessage;

    let dialogue = vec![
        TranscriptMessage::new(Role::User, "prompt"),
        TranscriptMessage::new(Role::Assistant, "reply"),
    ];
    let commands = crate::shell::transcript::transcript_commands_from_ledger(vec![
        nuo_protocol::CommandRecord::new("compact", "").with_timestamp(123),
    ]);

    let merged = merge_command_rows(dialogue, commands);
    let texts: Vec<&str> = merged.iter().map(|m| m.raw.as_str()).collect();
    assert_eq!(texts, vec!["prompt", "reply", "/compact"]);
}

#[test]
fn command_result_message_expands_and_round_trips_display() {
    // The command block's collapsed header is the invocation; expansion
    // reveals the typed result body. Pinning is respected (user toggle wins).
    use nuo_view::model::document::TranscriptMessage;
    let mut message = TranscriptMessage::command_result(
        "permissions",
        "",
        Some(nuo_protocol::CommandResult::PermissionList {
            allowed: vec!["execute_command".to_string()],
        }),
    );
    assert_eq!(message.command_result_expanded(), Some(false));
    assert_eq!(
        message.command_result_summary().as_deref(),
        Some("/permissions")
    );
    assert_eq!(
        message.command_result_text().as_deref(),
        Some("Always-allowed tools:\n- execute_command")
    );
    // The result body is the message's parsed blocks (non-empty here).
    assert!(!message.blocks.is_empty());

    message.pin_command_result_expanded(true);
    assert_eq!(message.command_result_expanded(), Some(true));
}

/// The command row's layout follows the shape of the reply — a
/// a short single line joins inline with whitespace, anything longer discloses, and a
/// missing result renders plain. The classifier is width-aware so an inline
/// reply is never a truncated fragment.
#[test]
fn command_row_layout_classifies_by_result_shape() {
    use nuo_view::model::document::{CommandRowLayout, TranscriptMessage};

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
        Some(nuo_protocol::CommandResult::Text(
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
        Some(nuo_protocol::CommandResult::PermissionList {
            allowed: vec!["execute_command".to_string(), "edit_text".to_string()],
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
    use nuo_view::phase::{Phase, ToolVerb};
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
        queued_at_ms: Some(0),
        images: vec![nuo_protocol::ImagePart {
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
fn outbox_count_and_addressing_are_session_scoped() {
    // Every staged message is a next-round item (the insert/next-round
    // distinction was removed), so the outbox count is a single per-session
    // tally and addressing is scoped by session — the client does not dispatch
    // this queue itself (the driver owns shipping, M4), so what must be
    // session-scoped is the *addressable* set, not a local FIFO drain.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.pending_dispatch
        .push_back(queued_dispatch("b-next", "session-b", "other"));
    app.pending_dispatch
        .push_back(queued_dispatch("a-next", "session-a", "follow up"));

    assert_eq!(app.pending_count("session-a"), 1);
    assert_eq!(app.pending_count("session-b"), 1);
    assert_eq!(
        app.queued_at("session-a", 0).map(|d| d.id.as_str()),
        Some("a-next"),
        "the session's own row is row 0"
    );
    assert!(
        app.queued_at("session-a", 1).is_none(),
        "another session's row is not addressable here"
    );
    assert_eq!(
        app.queued_at("session-b", 0).map(|d| d.id.as_str()),
        Some("b-next"),
        "and each session addresses only its own slice"
    );
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
    assert!(app.draft_capsule.is_empty());
    assert!(app.input.is_empty(), "composer is blank after a send");

    // A later ↑→↓ round-trip restores an empty composer, not the sent text.
    let rows = app.current_session_history();
    assert!(app.history_prev(&rows));
    assert!(!app.history_next(&rows));
    assert_eq!(app.input, "", "no stale draft may return after a send");
}

/// The async restore must not *lose* input in either direction.
///
/// The old policy (`OnlyIfIdle`) returned early when the composer was busy, so
/// the interrupt's prompt was dropped while the caller believed it had been
/// restored — the same silent-loss shape as the steer arms. `StashCurrent`
/// instead adopts the restored text and captures the in-progress draft into the
/// capsule, so ↓ brings the user's own composition back. Neither side is lost.
#[tokio::test]
async fn unsent_restore_stashes_the_in_progress_draft_instead_of_dropping_either() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "session-a".to_string();
    app.current_workspace = "~/p".to_string();

    // The user was mid-composition when the interrupt landed.
    app.input = "half-typed next message".to_string();
    app.adopt_as_draft(
        "interrupted input".to_string(),
        Vec::new(),
        Vec::new(),
        crate::shell::app::DraftAdoption::StashCurrent,
    );

    assert_eq!(
        app.input, "interrupted input",
        "the restore is never silently dropped: that was the OnlyIfIdle defect"
    );
    assert!(
        app.has_stashed_draft(),
        "the in-progress draft is captured, not discarded"
    );
    assert_eq!(app.history_index, None);

    // The stashed draft is the user's own composition, intact — that is the
    // property the old policy destroyed. (Restoring it is ↓'s job once history
    // holds a row; this fixture records none, so the capsule content is what is
    // asserted here.)
    assert_eq!(
        app.draft_capsule.text.clone(),
        "half-typed next message",
        "the stashed draft returns on ↓"
    );

    // Explicit gestures (queue recall) keep replacing even a busy composer.
    app.input = "another half-typed".to_string();
    app.adopt_as_draft(
        "recalled from queue".to_string(),
        Vec::new(),
        Vec::new(),
        crate::shell::app::DraftAdoption::Replace,
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
        crate::shell::event_loop::render_frame(&mut app, f, "test-session");
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
    app.phase = Some(nuo_view::phase::Phase::Queued);
    app.round_started_at = Some(std::time::Instant::now());

    // User prompt is sending in transcript
    let prompt = nuo_view::model::document::TranscriptMessage::new(
        nuo_protocol::Role::User,
        "write a fibonacci function",
    )
    .sending();
    app.messages.push(prompt);

    // Press Esc Esc to interrupt
    app.esc_armed_until = Some(std::time::Instant::now() + std::time::Duration::from_secs(1));
    crate::shell::event_loop::handle_esc_interrupt(
        &mut app,
        crate::shell::event_loop::InterruptTarget::Primary,
    );

    // Verify interrupt request was sent to agent
    let req = rx.try_recv().expect("must send interrupt request");
    assert!(matches!(req, nuo_protocol::AgentRequest::Interrupt));

    // UI state is immediately cleared (not blocked)
    assert!(!app.running_sessions.contains("session-a"));
    assert!(!app.viewed_chrome().responding);

    // The prompt is NOT popped/retracted — it stays in the transcript!
    assert_eq!(app.messages.len(), 1);
    assert_eq!(
        app.messages[0].delivery,
        nuo_view::model::document::DeliveryStatus::Cancelled,
        "prompt status must transition to Cancelled"
    );

    // Composer input is NOT clobbered with the cancelled prompt
    assert!(app.input.is_empty());
}

#[tokio::test]
async fn an_interrupted_round_records_one_anchor_and_no_transport_payload() {
    // The retry owns no transcript row while it runs, so an interrupt has
    // nothing to rewrite. Before this, `TranscriptEdit::Interrupted` reached
    // into the trailing retry entry and *settled* it into a permanent notice
    // whose detail was the pretty-printed HTTP body — a transport payload
    // dumped into the conversation, produced by a variant named `Interrupted`
    // that bypassed the admission gate.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let runtime = crate::shell::event_loop::UiRuntime::minimal_for_test();

    let prompt =
        nuo_view::model::document::TranscriptMessage::new(nuo_protocol::Role::User, "calculate pi")
            .sending();
    app.messages.push(prompt);

    // The producer parks the round in the transport-wait phase and publishes
    // the setback clause — chrome only, no transcript row.
    let state = crate::shell::app::ProviderRetryState::scheduled(
        2,
        5,
        std::time::Instant::now() + std::time::Duration::from_secs(5),
        "Google HTTP 503 Service Unavailable: No capacity available for model",
    );
    crate::shell::event_loop::apply::apply(
        &mut app,
        &runtime,
        crate::shell::event_loop::AppMutation::SetProviderRetry(state),
    );
    assert_eq!(
        app.messages.len(),
        1,
        "a scheduled retry must not append a transcript row"
    );

    // The interrupt's detail carries the last fault, one line — the producer
    // puts it there (see the `RoundInterrupted` arm of the translator), so the
    // anchor is self-explanatory without a second row.
    let record = nuo_protocol::RoundInterrupt {
        reason: nuo_protocol::RoundInterruptReason::User,
        round: Some(1),
        at_ms: 1_234_567,
        detail: Some("Google HTTP 503 Service Unavailable: No capacity available for model".into()),
    };
    crate::shell::event_loop::apply::apply(
        &mut app,
        &runtime,
        crate::shell::event_loop::AppMutation::Transcript {
            edit: crate::shell::event_loop::TranscriptEdit::Interrupted { record },
        },
    );

    // [0] the cancelled prompt, [1] the one interrupt anchor. No retry row.
    assert_eq!(app.messages.len(), 2, "one anchor per stopped round");
    assert_eq!(
        app.messages[0].delivery,
        nuo_view::model::document::DeliveryStatus::Cancelled
    );

    let anchor = &app.messages[1];
    assert!(anchor.is_round_interrupt());
    assert_eq!(
        anchor.raw,
        "Round 1 — cancelled via [Esc Esc]\nGoogle HTTP 503 Service Unavailable: No capacity available for model",
        "the reason is one line, not a pretty-printed JSON body: {:?}",
        anchor.raw
    );
    assert!(
        !anchor.raw.contains('{'),
        "no transport payload may reach the timeline: {:?}",
        anchor.raw
    );
}

#[tokio::test]
async fn tool_result_does_not_forge_reasoning_phase() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let runtime = crate::shell::event_loop::UiRuntime::minimal_for_test();

    // Start with a tool running phase
    crate::shell::event_loop::apply::apply(
        &mut app,
        &runtime,
        crate::shell::event_loop::AppMutation::SetPhase(Some(nuo_view::phase::Phase::Tool(
            nuo_view::phase::ToolVerb::Running,
        ))),
    );
    assert_eq!(
        app.phase,
        Some(nuo_view::phase::Phase::Tool(
            nuo_view::phase::ToolVerb::Running
        ))
    );

    // After tool finishes, phase transitions to Preparing, NEVER Reasoning ("thinking")
    crate::shell::event_loop::apply::apply(
        &mut app,
        &runtime,
        crate::shell::event_loop::AppMutation::SetPhase(Some(nuo_view::phase::Phase::Preparing)),
    );
    assert_ne!(
        app.phase,
        Some(nuo_view::phase::Phase::Reasoning),
        "tool completion must not set thinking/reasoning phase"
    );
    assert_eq!(app.phase, Some(nuo_view::phase::Phase::Preparing));
    assert_eq!(app.phase.as_ref().unwrap().label(), "preparing context");
}

#[test]
fn streaming_appends_with_queued_message_at_bottom_advances_max_scroll() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "test-session".to_string();
    app.follow_bottom = true;

    // Fill initial transcript with an assistant message followed by a queued user message
    let mut messages = Vec::new();
    for i in 0..20 {
        messages.push(TranscriptMessage::new(Role::User, format!("History {i}")));
    }
    let mut assistant =
        TranscriptMessage::new(Role::Assistant, "Assistant streaming response line 1\n");
    assistant.round = Some(1);
    assistant.turn = Some(1);
    messages.push(assistant);

    // Steer / queued prompt at the bottom
    let queued = TranscriptMessage::new(Role::User, "Queued follow-up / steer")
        .with_origin(nuo_view::model::document::UserMessageOrigin::Steer);
    messages.push(queued);
    app.messages = messages;

    // Frame 1: first layout pass calculates initial max_scroll under follow_bottom
    let mut terminal = nuotc::TestTerminal::new(80, 24);
    terminal.draw(|f| {
        crate::shell::event_loop::render_frame(&mut app, f, "test-session");
    });

    let initial_max_scroll = app.max_scroll;
    assert!(initial_max_scroll > 0);
    app.scroll = initial_max_scroll;

    // Render at max_scroll (bottom-follow steady state)
    terminal.draw(|f| {
        crate::shell::event_loop::render_frame(&mut app, f, "test-session");
    });
    let initial_content_lines = app.content_lines;

    // Frame 2 simulation: stream text delta arrives for assistant message above the queued message
    let id = crate::shell::append_stream_text_delta(
        &mut app.messages,
        Some(1),
        Some(1),
        "\n\nLine 2\n\nLine 3\n\nLine 4\n\nLine 5\n\n",
    )
    .expect("must append to streaming assistant message");
    app.layout_height_cache
        .invalidate_messages(std::iter::once(id));

    // Measure what stage pass produces:
    let painted_scroll = app.scroll;
    terminal.draw(|f| {
        crate::shell::event_loop::render_frame(&mut app, f, "test-session");
    });

    assert!(
        app.content_lines > initial_content_lines,
        "content lines must have grown from streaming delta"
    );
    assert!(
        app.max_scroll > initial_max_scroll,
        "max_scroll must have grown from streaming delta"
    );
    // Under bottom-follow, app.scroll is updated to app.max_scroll.
    // The event loop must detect that `app.scroll != painted_scroll` (bottom moved),
    // and skip committing the staged frame to avoid flickering the queued message.
    app.scroll = app.max_scroll;
    assert_ne!(
        app.scroll, painted_scroll,
        "stage_bottom_follow must detect that painted_scroll is stale"
    );
}

/// ADR-0137 [INV-STEER-01]: a busy-Enter steer never enters the transcript
/// while it is pending. It is staged in the outbox as a `Steering` item; the
/// transcript gains a row only when the harness admits it (`SteerAdmitted`).
#[test]
fn busy_enter_steer_is_staged_out_of_thread() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "test-session".to_string();
    app.running_sessions.insert("test-session".to_string());
    app.composer_send_mode = crate::shell::app::ComposerSendMode::Steer;
    app.input = "use rg instead".to_string();
    let before = app.messages.len();

    futures::executor::block_on(
        crate::shell::event_loop::actions::commands::handle_send_steer(
            &mut app,
            &crate::shell::event_loop::UiRuntime::minimal_for_test(),
            "test-session",
            "use rg instead".to_string(),
        ),
    );

    // The transcript is untouched.
    assert_eq!(
        app.messages.len(),
        before,
        "a pending steer must not enter the transcript (ADR-0137 [INV-STEER-01])"
    );
    // The outbox holds exactly one Steering item for the session.
    assert_eq!(app.pending_count("test-session"), 1);
    let item = app
        .pending_dispatch
        .iter()
        .find(|d| d.session_id == "test-session")
        .expect("steer staged in outbox");
    assert_eq!(
        item.state,
        crate::shell::app::QueuedDispatchState::Steering,
        "the staged steer must wear the Steering state"
    );
    assert_eq!(item.text, "use rg instead");
}

/// ADR-0140 `[INV-CMD-08]`: one reply, two homes, no overlap.
///
/// The server's declared class alone decides where a reply is filed. An **act**
/// settles the staged thread row (it holds a position in the conversation); a
/// **reading** appends a receipt and leaves the thread untouched. This is the
/// behaviour half of the invariant — the corridor lives in
/// `crates/nuo/src/shell/mod.rs`, and these tests drive its two branches the way
/// the event loop does.
mod command_reply_filing {
    use super::*;

    use crate::shell::app::notice_catalog::{ClientNotice, RenderedNotice};
    use nuo_protocol::CommandEffect;

    /// Render the receipt a reading produces, exactly as the event arm does.
    fn receipt(invocation: &str, body: &str, failed: bool) -> RenderedNotice {
        ClientNotice::CommandReceipt {
            invocation: invocation.to_string(),
            body: body.to_string(),
            failed,
        }
        .render()
    }

    /// A reading files a receipt and leaves the thread alone.
    #[test]
    fn reading_appends_a_receipt_and_no_thread_row() {
        let mut app = new_app_for_relay_tests();
        app.messages
            .push(TranscriptMessage::new(Role::User, "why is it slow?").with_round(1));
        let before = app.messages.len();
        let receipts_before = app.notice_count();

        // The filing decision the event arm makes for a reading.
        let rendered = receipt("/usage", "1.2M tokens across 14 threads", false);
        app.record_notice(rendered);

        assert_eq!(
            app.messages.len(),
            before,
            "a reading must never enter the thread — it is not a thing that was said"
        );
        assert_eq!(
            app.notice_count(),
            receipts_before + 1,
            "the reply is retained in the receipt log, not dropped"
        );
        let line = app.notice_lines().back().expect("the receipt line");
        assert_eq!(line.label.as_deref(), Some("command"));
        assert_eq!(line.text, "/usage", "the invocation names the receipt");
        assert_eq!(
            line.detail.as_deref(),
            Some("1.2M tokens across 14 threads"),
            "the full body rides in the detail, so nothing is truncated away"
        );
    }

    /// The mirror case: an act produces a thread row and no receipt.
    #[test]
    fn mutation_settles_a_thread_row_and_files_no_receipt() {
        let mut app = new_app_for_relay_tests();
        let receipts_before = app.notice_count();

        // Stage the optimistic row, as `handle_send_slash` does for an act.
        app.messages.push(
            TranscriptMessage::pending_command("unattended", "on")
                .with_sent_at_ms(1_700_000_000_000),
        );
        let before = app.messages.len();

        // The reply settles that row in place.
        let applied = crate::shell::event_loop::apply::apply_transcript(
            &mut app,
            crate::shell::event_loop::mutations::TranscriptEdit::SettleCommandResult {
                invocation: "/unattended on".to_string(),
                result: nuo_protocol::CommandResult::Ack {
                    title: "Unattended mode ON".to_string(),
                    detail: None,
                },
                fallback: None,
            },
        );

        assert!(applied);
        assert_eq!(
            app.messages.len(),
            before,
            "the row is settled, not appended"
        );
        assert_eq!(
            app.messages.last().and_then(|m| m.command_result_text()),
            Some("Unattended mode ON".to_string()),
            "the act's reply is the row's body"
        );
        assert_eq!(
            app.notice_count(),
            receipts_before,
            "an act produces no receipt: it already holds a thread row"
        );
    }

    /// A failed reading is a fault receipt (it raises a bubble); a successful one
    /// is information. Both stay in the same log, so a user reading back finds
    /// the failure where the answers are.
    #[test]
    fn a_failed_reading_is_a_fault_receipt() {
        let ok = receipt("/diff", "3 files changed", false);
        let failed = receipt("/diff", "could not read the workspace journal", true);
        assert_eq!(
            ok.severity(),
            nuo_view::model::document::NoticeSeverity::Info
        );
        assert_eq!(
            failed.severity(),
            nuo_view::model::document::NoticeSeverity::Error,
            "a reading that could not be produced is a fault, not information"
        );
    }

    /// The class is not consultable twice: `CommandEffect::is_durable` is the one
    /// predicate, and it agrees with the wire vocabulary's default.
    #[test]
    fn only_mutation_is_durable() {
        assert!(CommandEffect::Mutation.is_durable());
        assert!(!CommandEffect::Reading.is_durable());
        assert_eq!(
            CommandEffect::default(),
            CommandEffect::Reading,
            "a frame or vocabulary row that states no class makes no durability claim"
        );
    }

    /// `[INV-CMD-08]`, the staging half: what gets staged before dispatch.
    ///
    /// The catalog is an optimisation, and it is an *asynchronous* one — a client
    /// can dispatch before the catalog arrives, and `CommandCatalog::default()` is
    /// empty. An unknown name is `Mutation` there (the safe direction for the
    /// *residency* question, since a lost act is unrecoverable), which means a
    /// reading may be staged by mistake. That mistake must be reversible; the
    /// withdrawal is pinned below.
    #[test]
    fn an_unknown_name_is_staged_and_a_known_reading_is_not() {
        let catalog = nuo_server::startup::command_catalog(&[]);

        // A reading is not staged.
        assert_eq!(
            catalog.effect_for("/usage", None),
            CommandEffect::Reading,
            "a known reading stages no row"
        );
        // An act is staged.
        assert_eq!(
            catalog.effect_for("/unattended", Some("on")),
            CommandEffect::Mutation,
            "a known act stages its optimistic row"
        );
        // An unknown name stages a row: `Mutation` is the over-recording default,
        // and the event will withdraw it if the reply turns out to be a reading.
        assert_eq!(
            catalog.effect_for("/pursue", Some("ship it")),
            CommandEffect::Mutation,
            "an extension handler may mutate anything"
        );
        // An *empty* catalog — the pre-attach state — stages everything, which is
        // exactly why the withdrawal below has to exist.
        assert_eq!(
            nuo_protocol::CommandCatalog::default().effect_for("/usage", None),
            CommandEffect::Mutation,
            "an empty catalog cannot know, so it predicts an act"
        );
    }

    /// `[INV-CMD-08]`, the withdrawal: a row staged on a wrong prediction is
    /// removed when the authoritative event says "reading".
    ///
    /// Without this the row is left `Pending`, `CancelPendingCommands` later marks
    /// it `Cancelled`, and the thread keeps a permanent ghost telling the reader
    /// that a command never answered — for a command that answered in full.
    #[test]
    fn a_reading_withdraws_its_own_staged_row() {
        let mut app = new_app_for_relay_tests();
        app.messages
            .push(TranscriptMessage::new(Role::User, "why is it slow?").with_round(1));
        let dialogue_before = app.messages.len();

        // Staged on a wrong prediction (empty catalog at dispatch time).
        app.messages.push(
            TranscriptMessage::pending_command("usage", "").with_sent_at_ms(1_700_000_000_000),
        );
        assert_eq!(app.messages.len(), dialogue_before + 1);

        // The event is authority: withdraw, then file the receipt.
        let applied = crate::shell::event_loop::apply::apply_transcript(
            &mut app,
            crate::shell::event_loop::mutations::TranscriptEdit::WithdrawStagedCommand {
                invocation: "/usage".to_string(),
                restore_draft: false,
            },
        );

        assert!(applied);
        assert_eq!(
            app.messages.len(),
            dialogue_before,
            "the predicted row is gone — the thread holds only what was said"
        );
        assert!(
            app.messages.iter().all(|m| m.command_result_phase()
                != Some(nuo_view::model::document::CommandPhase::Pending)),
            "no pending row survives, so `CancelPendingCommands` has nothing to mark cancelled"
        );
    }

    /// The withdrawal is surgical: it removes only the matching pending row, so a
    /// concurrent act's staged row is untouched.
    #[test]
    fn withdrawal_leaves_other_pending_rows_alone() {
        let mut app = new_app_for_relay_tests();
        app.messages.push(
            TranscriptMessage::pending_command("unattended", "on")
                .with_sent_at_ms(1_700_000_000_000),
        );
        app.messages.push(
            TranscriptMessage::pending_command("usage", "").with_sent_at_ms(1_700_000_000_001),
        );

        crate::shell::event_loop::apply::apply_transcript(
            &mut app,
            crate::shell::event_loop::mutations::TranscriptEdit::WithdrawStagedCommand {
                invocation: "/usage".to_string(),
                restore_draft: false,
            },
        );

        assert_eq!(app.messages.len(), 1);
        assert_eq!(
            app.messages[0].command_result_summary().as_deref(),
            Some("/unattended on"),
            "the act's optimistic row must survive a reading's withdrawal"
        );
    }

    /// `[INV-CMD-09]`, `[INV-CMD-12]`: an unknown command or failed command
    /// withdraws its optimistic staged row from L0 transcript and restores its draft
    /// back into the empty composer.
    #[test]
    fn withdrawal_with_restore_draft_returns_invocation_to_empty_composer() {
        let mut app = new_app_for_relay_tests();
        assert!(app.input.is_empty());

        // Optimistically staged on unknown command dispatch
        app.messages.push(
            TranscriptMessage::pending_command("modles", "").with_sent_at_ms(1_700_000_000_000),
        );
        assert_eq!(app.messages.len(), 1);

        let applied = crate::shell::event_loop::apply::apply_transcript(
            &mut app,
            crate::shell::event_loop::mutations::TranscriptEdit::WithdrawStagedCommand {
                invocation: "/modles".to_string(),
                restore_draft: true,
            },
        );

        assert!(applied);
        assert_eq!(
            app.messages.len(),
            0,
            "staged row was withdrawn, keeping L0 transcript completely clean"
        );
        assert_eq!(
            app.input, "/modles",
            "the invocation draft was restored to the composer"
        );
        assert_eq!(
            app.cursor_position,
            app.input.len(),
            "cursor placed at end for immediate backspace editing"
        );
    }

    /// `[INV-CMD-09]`, `[INV-CMD-11]`: dispatching an unknown slash command
    /// does not stage any optimistic row in the transcript, and its failure
    /// receipt renders the error summary into the notice headline for the top-right toast.
    #[tokio::test]
    async fn unknown_slash_command_never_stages_and_surfaces_error_headline() {
        let mut app = new_app_for_relay_tests();
        app.command_catalog = nuo_server::startup::command_catalog(&[]);
        let runtime = crate::shell::event_loop::UiRuntime::minimal_for_test();
        let session = crate::shell::SessionSource::Remote {
            session_id: "test-session".to_string(),
        };

        // Dispatch a mistyped slash command
        crate::shell::event_loop::actions::commands::handle_send_slash(
            &mut app,
            &runtime,
            &session,
            "/modles".to_string(),
        )
        .await;

        assert!(
            app.messages.is_empty(),
            "an unknown slash command must NEVER stage an optimistic row in L0 transcript"
        );

        // When the server answers with UnknownCommand:
        let result = nuo_protocol::CommandResult::UnknownCommand {
            name: "/modles".to_string(),
            suggestion: Some("/models".to_string()),
        };
        let rendered = ClientNotice::CommandReceipt {
            invocation: "/modles".to_string(),
            body: result.to_text(),
            failed: true,
        }
        .render();

        assert_eq!(
            rendered.severity(),
            nuo_view::model::document::NoticeSeverity::Error
        );
        assert!(
            rendered
                .title()
                .starts_with("Unknown command: /modles. Did you mean /models?"),
            "notice title must carry the actual error and suggestion for the top-right toast"
        );

        // Recording the notice displays it in toast and records to surface log
        app.record_notice(rendered);
        assert_eq!(
            app.notice_toast_severity,
            nuo_view::model::document::NoticeSeverity::Error
        );
        assert!(
            app.notice_toast_message
                .starts_with("Unknown command: /modles. Did you mean /models?")
        );
        assert!(
            app.notice_toast_until.is_some(),
            "notice toast is armed for display"
        );
    }
}
