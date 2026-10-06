//! Input history, recall, session resume backfill, and on-disk history isolation tests.

use super::*;

#[test]
fn restored_history_hides_harness_messages() {
    assert!(transcript_message_from_core(Message::hidden(Role::User, "internal")).is_none());
    assert!(transcript_message_from_core(Message::new(Role::System, "system")).is_none());
}

#[test]
fn restored_history_uses_command_display_content() {
    let message = Message::new(Role::User, "Expanded internal prompt")
        .with_display_content("/review working-tree");
    let restored = transcript_message_from_core(message).unwrap();
    assert_eq!(restored.raw, "/review working-tree");
}

#[test]
fn restored_user_message_uses_exact_or_legacy_timestamp() {
    let exact = Message::new(Role::User, "hi").with_sent_at_ms(1_700_000_000_123);
    let restored = transcript_message_from_core(exact).unwrap();
    assert_eq!(restored.sent_at_ms, Some(1_700_000_000_123));

    let mut legacy = Message::new(Role::User, "hi");
    legacy.sent_at_ms = None;
    legacy.timestamp = Some(1_700_000_001);
    let restored = transcript_message_from_core(legacy).unwrap();
    assert_eq!(restored.sent_at_ms, Some(1_700_000_001_000));
}

#[test]
fn restored_assistant_tool_step_uses_message_timestamp_for_turn_header() {
    let mut assistant = Message::new(Role::Assistant, "");
    assistant.timestamp = Some(1_700_000_002);
    assistant.tool_calls = Some(vec![ToolCall {
        id: "call".to_string(),
        name: "read_text".to_string(),
        arguments: r#"{"path":"README.md"}"#.to_string(),
    }]);

    let restored = transcript_messages_from_core(vec![assistant], &config::TuiConfig::default());
    assert_eq!(restored.len(), 1);
    assert_eq!(restored[0].sent_at_ms, Some(1_700_000_002_000));
    assert_eq!(restored[0].round, Some(1));
    assert_eq!(restored[0].turn, Some(1));
}

#[test]
fn restored_assistant_components_share_their_round_and_turn() {
    let mut assistant = Message::new(Role::Assistant, "continue");
    assistant.reasoning_content = Some("inspect first".to_string());
    assistant.tool_calls = Some(vec![ToolCall {
        id: "call".to_string(),
        name: "read_text".to_string(),
        arguments: r#"{"path":"README.md"}"#.to_string(),
    }]);

    let restored = transcript_messages_from_core(vec![assistant], &config::TuiConfig::default());
    assert_eq!(restored.len(), 3);
    assert!(restored[0].is_reasoning());
    assert!(restored[1].is_tool_step());
    assert_eq!(restored[2].role, Role::Assistant);
    assert!(restored.iter().all(|message| message.round == Some(1)));
    assert!(restored.iter().all(|message| message.turn == Some(1)));
}

#[test]
fn restored_user_message_origin_inferred_from_shape() {
    use crate::model::document::UserMessageOrigin;
    // A genuine chat prompt: no display_content, no leading `!`.
    let chat = transcript_message_from_core(Message::new(Role::User, "fix the bug")).unwrap();
    assert_eq!(chat.origin, UserMessageOrigin::Chat);

    // A slash command carries a `display_content` whose text is the literal
    // `/cmd` (its real content is the harness-expanded form) → Slash.
    let slash = Message::new(Role::User, "expanded pursue body")
        .with_display_content("/pursue ship the release");
    let slash = transcript_message_from_core(slash).unwrap();
    assert_eq!(slash.origin, UserMessageOrigin::Slash);

    // A prompt starting with `!` is a normal chat prompt
    let exclamation = transcript_message_from_core(Message::new(Role::User, "!ls -la")).unwrap();
    assert_eq!(exclamation.origin, UserMessageOrigin::Chat);

    // A genuine prompt that merely *starts* with `/` (no display_content) is
    // NOT misclassified as a slash command — e.g. "/etc is a path" stays Chat.
    let path_like =
        transcript_message_from_core(Message::new(Role::User, "/etc is a path")).unwrap();
    assert_eq!(path_like.origin, UserMessageOrigin::Chat);
}

#[test]
fn restored_user_insert_keeps_mid_round_origin_without_opening_a_turn() {
    use crate::model::document::UserMessageOrigin;
    let first = Message::new(Role::Assistant, "first answer");
    let inserted = Message::new(Role::User, "one more constraint").with_origin(
        nuo_wire::InjectionOrigin::new(nuo_wire::InjectionKind::UserSteer),
    );
    let second = Message::new(Role::Assistant, "revised answer");

    let restored =
        transcript_messages_from_core(vec![first, inserted, second], &config::TuiConfig::default());
    assert_eq!(restored[1].origin, UserMessageOrigin::Steer);
    assert_eq!(restored[1].round, Some(1));
    assert_eq!(restored[1].turn, None);
    assert_eq!(restored[2].round, Some(1));
    assert_eq!(restored[2].turn, Some(2));
}

#[test]
fn restored_command_echo_origin_from_durable_provenance() {
    // ADR-0050: durable slash/shell echoes are persisted as
    // `Message::command_echo` — a visible user message carrying the
    // `CommandEcho` provenance. On resume the stored origin is consulted
    // FIRST (ahead of the shape heuristic), so an echo whose text lacks the
    // `display_content` / `!` shape signals is still classified as a
    // non-driving command, never as the round's driving prompt.
    use crate::model::document::UserMessageOrigin;

    // A slash echo: content is the literal `/cmd`, no display_content. The
    // shape heuristic alone would misread this (no display_content → fall to
    // the `!` check → fail → Chat). The durable origin must win.
    let slash_echo = Message::command_echo("/pursue ship it");
    assert!(slash_echo.is_command_echo());
    let restored = transcript_message_from_core(slash_echo).unwrap();
    assert_eq!(
        restored.origin,
        UserMessageOrigin::Slash,
        "durable echo provenance must classify as Slash, not Chat"
    );

    // A shell echo: content is `!cmd`. Even though the `!` shape heuristic
    // would also catch it, the origin-first path must handle it too.
    let shell_echo = Message::command_echo("!ls -la");
    let restored_shell = transcript_message_from_core(shell_echo).unwrap();
    assert_eq!(restored_shell.origin, UserMessageOrigin::Slash);
}

/// ADR-0108: a restored slash/shell echo folds into the command ledger
/// projection — the invocation renders once, on the `⌘` command component,
/// never twice (a `▌ cmd` user bubble *and* a command row). The echo is
/// dropped from the dialogue before merge; the ledger row keeps the record.
#[test]
fn restored_slash_echoes_fold_into_command_components() {
    use crate::model::document::UserMessageOrigin;
    use nuo_wire::Role;

    let restored = transcript_messages_from_core(
        vec![
            Message::new(Role::User, "hello"), // a real prompt: survives
            Message::new(Role::Assistant, "hi"),
            // A durable command echo (the legacy live path persisted these).
            Message::command_echo("/pursue ship it"),
            // A display-content slash shape (legacy sessions pre-ADR-0050).
            {
                let mut m = Message::new(Role::User, "expanded prompt text");
                m.display_content = Some("/delegate on".to_string());
                m
            },
            Message::new(Role::User, "and me too"), // another real prompt
        ],
        &crate::config::TuiConfig::default(),
    );

    let raws: Vec<&str> = restored.iter().map(|m| m.raw.as_str()).collect();
    assert_eq!(
        raws,
        vec!["hello", "hi", "and me too"],
        "command echoes must not render as user bubbles (ADR-0108); the ledger row owns the invocation"
    );
    // The projection builder still classifies them correctly when asked
    // directly (used by Activity-modal prompt gating) — the fold happens at
    // the list level, not by breaking classification.
    let echo = transcript_message_from_core(Message::command_echo("/pursue ship it")).unwrap();
    assert_eq!(echo.origin, UserMessageOrigin::Slash);
}

#[test]
fn restored_assistant_carries_provider_and_model_attribution() {
    // A persisted assistant message stamped by the harness keeps its
    // provider/model so a resumed session that mixed models stays
    // traceable in the transcript.
    let message = Message::new(Role::Assistant, "Hello from kimi")
        .with_attribution("kimi-code", "kimi-k2.7-code");
    let restored = transcript_message_from_core(message).unwrap();
    assert_eq!(restored.provider.as_deref(), Some("kimi-code"));
    assert_eq!(restored.model.as_deref(), Some("kimi-k2.7-code"));
    assert_eq!(
        restored.attribution_label(),
        Some(("kimi-code".to_string(), "kimi-k2.7-code".to_string()))
    );
    // No persisted effort → no depth chip on restore.
    assert_eq!(restored.effort, None);

    // The persisted reasoning depth round-trips with the attribution.
    let mut message = Message::new(Role::Assistant, "deep thought");
    message.effort = Some("high".to_string());
    let restored = transcript_message_from_core(message).unwrap();
    assert_eq!(restored.effort.as_deref(), Some("high"));
    assert_eq!(restored.attribution_label().map(|(_, m)| m), None::<String>);

    // A plain user message carries no attribution.
    let user = transcript_message_from_core(Message::new(Role::User, "hi")).unwrap();
    assert!(user.attribution_label().is_none());

    // A provider without an id still surfaces the model alone.
    let model_only = Message::new(Role::Assistant, "x").with_attribution("", "gpt-4o");
    let restored = transcript_message_from_core(model_only).unwrap();
    assert_eq!(
        restored.attribution_label(),
        Some((String::new(), "gpt-4o".to_string()))
    );
}

#[test]
fn restored_reasoning_is_not_shown_as_running() {
    let message = Message {
        role: Role::Assistant,
        content: String::new(),
        content_blob: None,
        display_content: None,
        reasoning_content: Some("step-by-step reasoning".to_string()),
        provider_meta: None,
        tool_calls: None,
        tool_call_id: None,
        images: None,
        provider: None,
        model: None,
        effort: None,
        hidden: false,
        children: None,
        subagent_meta: None,
        origin: None,
        timestamp: None,
        sent_at_ms: None,
        cache_frozen: false,
    };

    let restored = transcript_messages_from_core(vec![message], &config::TuiConfig::default());
    assert_eq!(restored.len(), 1);
    let thinking = &restored[0];
    assert!(thinking.is_reasoning());
    assert_eq!(thinking.turn, Some(1));
    // A finished reasoning block must not be rendered with a live spinner.
    assert!(
        thinking.reasoning_summary().unwrap().contains("0ms"),
        "restored thinking should have a finished duration, got {:?}",
        thinking.reasoning_summary()
    );
}

#[test]
fn restored_native_tool_calls_are_visible() {
    let message = Message {
        role: Role::Assistant,
        content: String::new(),
        content_blob: None,
        display_content: None,
        reasoning_content: None,
        provider_meta: None,
        tool_calls: Some(vec![ToolCall {
            id: "call".to_string(),
            name: "read_text".to_string(),
            arguments: "{\"path\":\"README.md\"}".to_string(),
        }]),
        tool_call_id: None,
        images: None,
        provider: None,
        model: None,
        effort: None,
        hidden: false,
        children: None,
        subagent_meta: None,
        origin: None,
        timestamp: None,
        sent_at_ms: None,
        cache_frozen: false,
    };

    let restored = transcript_message_from_core(message).unwrap();
    assert!(restored.raw.contains("read_text"));
}

#[test]
fn restored_tool_results_merge_into_steps_in_fifo_order() {
    let messages = vec![
        Message {
            role: Role::Assistant,
            content: String::new(),
            content_blob: None,
            display_content: None,
            reasoning_content: None,
            provider_meta: None,
            tool_calls: Some(vec![
                ToolCall {
                    id: "one".to_string(),
                    name: "read_text".to_string(),
                    arguments: r#"{"path":"one"}"#.to_string(),
                },
                ToolCall {
                    id: "two".to_string(),
                    name: "read_text".to_string(),
                    arguments: r#"{"path":"two"}"#.to_string(),
                },
            ]),
            tool_call_id: None,
            images: None,
            provider: None,
            model: None,
            effort: None,
            hidden: false,
            children: None,
            subagent_meta: None,
            origin: None,
            timestamp: None,
            sent_at_ms: None,
            cache_frozen: false,
        },
        Message::tool_result(
            &ToolCall {
                id: "one".to_string(),
                name: "read_text".to_string(),
                arguments: String::new(),
            },
            "[read_text result]:\nfirst",
        ),
        Message::tool_result(
            &ToolCall {
                id: "two".to_string(),
                name: "read_text".to_string(),
                arguments: String::new(),
            },
            "[read_text result]:\nsecond",
        ),
    ];

    let mut restored = transcript_messages_from_core(messages, &config::TuiConfig::default());
    assert_eq!(restored.len(), 2);
    restored[0].set_tool_step_expanded(true);
    restored[1].set_tool_step_expanded(true);
    assert!(restored[0].raw.contains("first"));
    assert!(!restored[0].raw.contains("second"));
    assert!(restored[1].raw.contains("second"));
}

#[test]
fn history_rows_lists_newest_first_then_ranks_search() {
    // The App-level view of the Ctrl+R panel. With no query the whole
    // cross-session history is listed newest-first (by created_at_ms),
    // unhighlighted; once the user types, only the fuzzy subsequence matches
    // surface, ordered by score with newest-first order as the stable
    // tiebreaker.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let mk = |text: &str, sid: &str, ts: u64| {
        nuo_wire::HistoryEntry::new(
            text.to_string(),
            Some(sid.to_string()),
            Some("~/p".to_string()),
            ts,
        )
    };
    app.input_history = vec![
        mk("scatter", "s1", 10),     // idx 0 — 'cat' mid-word, lowest score
        mk("catalog", "s1", 20),     // idx 1 — 'cat' at boundary, high score
        mk("cargo build", "s1", 30), // idx 2 — 'cat' is not a subsequence
        mk("the cat sat", "s1", 40), // idx 3 — 'cat' at boundary, high score
    ];

    // Empty query → newest-first by timestamp, score 0, no highlights.
    app.input.clear();
    let rows = app.history_rows();
    let indices: Vec<usize> = rows.iter().map(|(i, _)| *i).collect();
    assert_eq!(indices, vec![3, 2, 1, 0], "newest first by timestamp");
    for (_, m) in &rows {
        assert_eq!(m.score, 0);
        assert!(m.positions.is_empty());
    }

    // Search "cat" → matches catalog, "the cat sat", and scatter; not
    // "cargo build" (no 't' after the 'ca'). Boundary matches outrank
    // scatter; among the tied boundary matches the newest-first order wins
    // (idx 3 "the cat sat" ts=40 before idx 1 "catalog" ts=20).
    app.input = "cat".to_string();
    let rows = app.history_rows();
    let indices: Vec<usize> = rows.iter().map(|(i, _)| *i).collect();
    assert_eq!(
        indices,
        vec![3, 1, 0],
        "boundary matches first (newest-first on ties), then scatter"
    );
    assert!(rows[0].1.score > rows[2].1.score);
    for (_, m) in &rows {
        assert_eq!(m.positions.len(), 3);
    }

    // Query with no subsequence match → empty list (the renderer turns this
    // into the "no matches" placeholder).
    app.input = "xyz".to_string();
    assert!(app.history_rows().is_empty());
}

#[test]
fn history_modal_is_click_dismissable_and_restores_draft() {
    // Phase 3 (ADR-0133): the per-view draft contract. Parking the draft on
    // the HistorySearch view's own slot, then dismissing the view, hands it
    // back to the composer — the same Esc/outside-click teardown.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.open_dialog(crate::surfaces::DialogKind::HistorySearch);
    // Simulate the parked draft (open_dialog parked the live composer, which
    // started empty) and the live filter state.
    if let Some(st) = app
        .surface_store
        .state_mut(&crate::surfaces::DialogKind::HistorySearch)
    {
        st.draft = Some("my draft".to_string());
    }
    app.input = "git".to_string(); // the live fuzzy query
    app.cursor_position = 3;
    app.history_search = true;
    app.modal_index = 4;

    assert!(app.dismiss_surface());

    assert_eq!(app.input, "my draft", "draft restored from the view's slot");
    assert_eq!(app.cursor_position, "my draft".chars().count());
    assert!(
        app.surface_store
            .state(&crate::surfaces::DialogKind::HistorySearch)
            .is_none_or(|st| st.draft.is_none()),
        "slot emptied"
    );
    assert!(!app.history_search);
    assert!(app.surfaces.active_overlay().is_none());
}

#[test]
fn history_insert_clears_search_query_buffer_and_places_entry() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.input_history.push(nuo_wire::HistoryEntry::new(
        "history row 1".to_string(),
        Some("s1".to_string()),
        None,
        100,
    ));
    app.input_history.push(nuo_wire::HistoryEntry::new(
        "history row 2".to_string(),
        Some("s1".to_string()),
        None,
        200,
    ));

    app.input = "draft before search".to_string();
    app.open_dialog(crate::surfaces::DialogKind::HistorySearch);
    app.input = "row".to_string();
    app.save_dialog_state(crate::surfaces::DialogKind::HistorySearch);

    // Simulate HistoryInsert action (Tab / Enter accept)
    let ranked = app.history_rows();
    let pick = ranked.first().or_else(|| ranked.first());
    assert!(pick.is_some());
    let (orig_idx, _) = *pick.unwrap();
    let text = app.input_history[orig_idx].text.clone();
    app.adopt_as_draft(text, vec![], vec![], crate::app::DraftAdoption::Replace);
    if let Some(state) = app
        .surface_store
        .state_mut(&crate::surfaces::DialogKind::HistorySearch)
    {
        state.draft = None;
        state.query.clear();
        state.index = 0;
    }
    app.surfaces.dismiss_all_overlays();
    app.history_search = false;

    // Composer now holds the selected history entry, not the search query or old draft
    assert_eq!(app.input, "history row 2");
    // Search query in state is cleared
    let search_state = app
        .surface_store
        .state(&crate::surfaces::DialogKind::HistorySearch);
    assert_eq!(search_state.map(|s| s.query.as_str()), Some(""));
}

#[test]
fn recall_queued_is_lifo_and_restores_input() {
    // Every queued dispatch is a next-round item, so recall pops the newest
    // staged message in LIFO order and restores it locally without an agent
    // roundtrip (no insert to cancel).
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.pending_dispatch
        .push_back(queued_dispatch("1", "session-a", "first"));
    app.pending_dispatch
        .push_back(queued_dispatch("2", "session-a", "second"));

    // First recall: most-recently-queued = "second".
    let Some(RecallQueued::Restored(dispatch)) = app.recall_queued("session-a") else {
        panic!("expected local restore");
    };
    app.restore_dispatch(dispatch);
    assert_eq!(app.input, "second");
    assert_eq!(app.cursor_position, "second".chars().count());
    assert_eq!(
        app.history_index, None,
        "history cursor must be cleared so ↓ returns to empty input"
    );
    // Second recall: now "first".
    let Some(RecallQueued::Restored(dispatch)) = app.recall_queued("session-a") else {
        panic!("expected local restore");
    };
    app.restore_dispatch(dispatch);
    assert_eq!(app.input, "first");

    // Third recall: queue empty → no-op.
    assert!(app.recall_queued("session-a").is_none());
    assert_eq!(
        app.input, "first",
        "input must be untouched when the queue is empty"
    );
}

#[test]
fn recall_queued_restores_staged_images() {
    // Images staged with the queued message (Ctrl+V before pressing
    // Enter) come back alongside the text so the user can re-edit and
    // resend without losing the attachment.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let image = nuo_wire::ImagePart {
        mime: "image/png".to_string(),
        data: "abc".to_string(),
    };
    let mut dispatch = queued_dispatch("1", "session-a", "look at this");
    dispatch.images = vec![image.clone()];
    app.pending_dispatch.push_back(dispatch);

    let Some(RecallQueued::Restored(dispatch)) = app.recall_queued("session-a") else {
        panic!("expected local restore");
    };
    app.restore_dispatch(dispatch);
    assert_eq!(app.input, "look at this");
    assert_eq!(
        app.pending_images.len(),
        1,
        "recalled images must land back in pending_images for resend"
    );
    assert_eq!(app.pending_images[0].data, image.data);
}

/// The interrupt → ↑/↓ → resend bug: a message sent with pasted images is
/// recorded to input history as text-only, so recalling it via ↑/↓ (or
/// Ctrl+R) and pressing Enter used to ship the bare `[Image #N]` chip label
/// with no payload — the model never received the pixels. Recording must
/// cache the staged attachments keyed by the entry's identity, and recall
/// must restore them into `pending_images` / `pending_text_pastes`.
#[tokio::test]
async fn history_recall_restores_staged_images_and_pastes() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "session-a".to_string();
    app.current_workspace = "~/p".to_string();

    let image = nuo_wire::ImagePart {
        mime: "image/png".to_string(),
        data: "abc".to_string(),
    };
    let chip = crate::composer_attachments::image_chip(1, 3);
    let paste = crate::composer_attachments::paste_chip(1, 2, 11);
    let text = format!("describe this {chip} then {paste}");
    app.record_input_history(text.clone(), vec![image.clone()], vec!["big paste".into()]);

    // ↑ recall: the entry is the newest row of the current session; the
    // event loop loads its text and calls restore_history_attachments.
    let session_rows = app.current_session_history();
    assert_eq!(session_rows.len(), 1);
    let orig_idx = session_rows[0];
    app.input = app.history_entry(orig_idx).expect("row").text.clone();
    app.restore_history_attachments(orig_idx);

    assert_eq!(app.input, text, "recalled text keeps its chip labels");
    assert_eq!(
        app.pending_images.len(),
        1,
        "image payload restored for resend"
    );
    assert_eq!(app.pending_images[0].data, "abc");
    assert_eq!(app.pending_text_pastes, vec!["big paste".to_string()]);

    // The chips pair back up with the payloads after an edit reconcile, so
    // a Backspace or typing never orphans them.
    app.reconcile_attachments();
    assert_eq!(app.pending_images.len(), 1);
    assert_eq!(app.pending_text_pastes.len(), 1);
}

/// Recalling a text-only entry (no cached payloads) must clear the staged
/// vectors so a resend never inherits an attachment that belonged to a
/// different entry — e.g. one restored by a Phase-1 unsend that the user then
/// navigated away from.
#[tokio::test]
async fn history_recall_clears_staged_attachments_for_plain_entries() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "session-a".to_string();
    app.current_workspace = "~/p".to_string();
    app.record_input_history("plain prompt".to_string(), Vec::new(), Vec::new());

    let image = nuo_wire::ImagePart {
        mime: "image/png".to_string(),
        data: "abc".to_string(),
    };
    app.pending_images.push(image);

    let orig_idx = app.current_session_history()[0];
    app.input = app.history_entry(orig_idx).expect("row").text.clone();
    app.restore_history_attachments(orig_idx);

    assert!(
        app.pending_images.is_empty(),
        "no orphaned payloads on recall"
    );
    assert!(app.pending_text_pastes.is_empty());
}

/// The ↓-past-newest branch restores the draft the user was composing before
/// the first ↑ — including any staged attachments — so an accidental ↑/↓
/// round-trip never drops a pasted image.
#[test]
fn history_draft_round_trip_keeps_attachments() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let draft = "my in-progress draft".to_string();
    let image = nuo_wire::ImagePart {
        mime: "image/png".to_string(),
        data: "draft-img".to_string(),
    };
    app.input = draft.clone();
    app.pending_images = vec![image.clone()];

    // First ↑: stash text + attachments (what the HistoryPrev handler does).
    app.history_draft = std::mem::take(&mut app.input);
    app.history_draft_images = std::mem::take(&mut app.pending_images);
    app.history_draft_text_pastes = std::mem::take(&mut app.pending_text_pastes);

    // ↓ past the newest entry: restore text + attachments together.
    app.input = std::mem::take(&mut app.history_draft);
    app.pending_images = std::mem::take(&mut app.history_draft_images);
    app.pending_text_pastes = std::mem::take(&mut app.history_draft_text_pastes);

    assert_eq!(app.input, draft);
    assert_eq!(app.pending_images.len(), 1);
    assert_eq!(app.pending_images[0].mime, "image/png");
    assert_eq!(app.pending_images[0].data, "draft-img");
    assert!(app.pending_text_pastes.is_empty());
}

/// The in-memory cache is bounded so a long session of image-heavy sends
/// cannot balloon the process's memory with base64 payloads.
#[tokio::test]
async fn history_attachment_cache_is_capped() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "session-a".to_string();
    for i in 0..40 {
        app.record_input_history(
            format!("prompt {i}"),
            vec![nuo_wire::ImagePart {
                mime: "image/png".to_string(),
                data: format!("img-{i}"),
            }],
            Vec::new(),
        );
    }
    assert!(
        app.history_attachments.len() <= crate::app::App::HISTORY_ATTACHMENTS_CAP,
        "cache must stay bounded"
    );
    // A recent entry's payload survives the FIFO eviction…
    let newest_idx = app
        .input_history
        .iter()
        .position(|e| e.text == "prompt 39")
        .expect("last prompt recorded");
    app.restore_history_attachments(newest_idx);
    assert_eq!(app.pending_images[0].data, "img-39");
    // …while the oldest entries were evicted (their recall clears the
    // staged vectors rather than restoring a stale payload).
    let oldest_idx = app
        .input_history
        .iter()
        .position(|e| e.text == "prompt 0")
        .expect("first prompt recorded");
    app.pending_images.clear();
    app.restore_history_attachments(oldest_idx);
    assert!(
        app.pending_images.is_empty(),
        "evicted entries must not restore attachments"
    );
}

/// `[input_history] dedup` (default on): the same prompt text sent twice —
/// even in a different session — stays a single entry, and re-sending bumps
/// its timestamp so it bubbles to the top of the newest-first picker.
#[tokio::test]
async fn record_input_history_dedups_globally_by_text() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "session-a".to_string();
    app.current_workspace = "~/p".to_string();

    app.record_input_history("build the thing".to_string(), Vec::new(), Vec::new());
    app.record_input_history("different prompt".to_string(), Vec::new(), Vec::new());
    assert_eq!(app.input_history.len(), 2);

    // The same text in a *different* session still collapses to one entry,
    // adopting the newest origin so ↑/↓ in the newer session finds it.
    app.current_session_id = "session-b".to_string();
    app.record_input_history("build the thing".to_string(), Vec::new(), Vec::new());

    assert_eq!(
        app.input_history.len(),
        2,
        "global dedup keeps one row per text"
    );
    let deduped = app
        .input_history
        .iter()
        .find(|e| e.text == "build the thing")
        .expect("entry survives dedup");
    assert_eq!(deduped.session_id.as_deref(), Some("session-b"));
    // The re-sent entry is newest → first in the history order.
    let order = app.history_order();
    assert_eq!(app.input_history[order[0]].text, "build the thing");
}

/// With dedup off (`[input_history] dedup = false`) the same words typed in
/// two sessions stay two entries, each with its own origin.
#[tokio::test]
async fn record_input_history_without_dedup_keeps_per_session_entries() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.input_history_dedup = false;
    app.current_session_id = "session-a".to_string();
    app.record_input_history("hello".to_string(), Vec::new(), Vec::new());
    app.current_session_id = "session-b".to_string();
    app.record_input_history("hello".to_string(), Vec::new(), Vec::new());
    assert_eq!(app.input_history.len(), 2);
}

#[tokio::test]
async fn resumed_session_backfills_prompt_rows_from_transcript() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "session-a".to_string();
    app.current_workspace = "~/p".to_string();
    // One prompt this client genuinely recorded (simulating the live tail of
    // the resumed session typed through this TUI), and a stale prompt that
    // belongs to a different session entirely.
    app.record_input_history("live prompt".to_string(), Vec::new(), Vec::new());
    app.record_input_history("other session's prompt".to_string(), Vec::new(), Vec::new());
    app.input_history.last_mut().unwrap().session_id = Some("session-b".to_string());

    // The resumed transcript: genuine chat prompts (oldest-first, as the
    // listener rebuilds it) plus a slash command and a shell passthrough —
    // neither of which may become a recall row.
    let transcript = vec![
        TranscriptMessage::new(Role::User, "first turn").with_sent_at_ms(100),
        TranscriptMessage::new(Role::Assistant, "ok"),
        TranscriptMessage::new(Role::User, "live prompt").with_sent_at_ms(200),
        TranscriptMessage::new(Role::User, "/model").with_origin(UserMessageOrigin::Slash),
        TranscriptMessage::new(Role::User, "steering").with_origin(UserMessageOrigin::Steer),
    ];
    app.backfill_session_history(&prompt_tail(&transcript), 1000);

    // Only the unseen prompt is backfilled; the already-recorded one is not
    // duplicated, and the derived rows never touch the persisted store.
    assert_eq!(
        app.session_history_backfill.len(),
        1,
        "only the unrecorded prompt is backfilled"
    );
    assert_eq!(app.session_history_backfill[0].text, "first turn");
    assert_eq!(app.input_history.len(), 2, "persisted history untouched");

    // ↑ walks the union newest-first: the live prompt (ts stamped by the
    // send), then the backfilled row.
    let rows = app.current_session_history();
    assert_eq!(rows.len(), 2, "other session's prompt is filtered out");
    assert_eq!(app.history_entry(rows[0]).unwrap().text, "live prompt");
    assert_eq!(app.history_entry(rows[1]).unwrap().text, "first turn");
    assert!(app.history_prev(&rows));
    assert_eq!(app.input, "live prompt");
    assert!(app.history_prev(&rows));
    assert_eq!(app.input, "first turn");

    // The backfill is incremental: re-running with the same transcript adds
    // nothing; appending a new turn adds exactly that row.
    app.backfill_session_history(&prompt_tail(&transcript), 1000);
    assert_eq!(app.session_history_backfill.len(), 1);
    let mut grown = transcript.clone();
    grown.push(TranscriptMessage::new(Role::User, "third turn").with_sent_at_ms(300));
    app.backfill_session_history(&prompt_tail(&grown), 1000);
    assert_eq!(app.session_history_backfill.len(), 2);
}

/// The ↑/↓ rows follow the **live** session id, not the id the client started
/// with: `current_session_id` is what stamps new entries, so a prompt sent
/// after a mid-run `/session open` is tagged with the switched-to session.
/// (The wiring this guards — the listener updating `UiRuntime::live_session_id`
/// from `ConversationCleared`/`ConversationReplaced` — lives in the event
/// loop; here the contract is that stamping and recall agree on one id.)
#[tokio::test]
async fn history_rows_are_scoped_by_the_live_session_id() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    // The client started attached to session-old…
    app.current_session_id = "session-old".to_string();
    app.record_input_history(
        "typed before the switch".to_string(),
        Vec::new(),
        Vec::new(),
    );
    // …then `/session open` repointed the harness and the listener tracked it.
    app.current_session_id = "session-new".to_string();
    app.on_viewed_session_changed();
    app.record_input_history("typed after the switch".to_string(), Vec::new(), Vec::new());

    let texts: Vec<&str> = app
        .current_session_history()
        .into_iter()
        .filter_map(|i| app.history_entry(i).map(|e| e.text.as_str()))
        .collect();
    assert_eq!(texts, vec!["typed after the switch"]);
}

/// `/command` invocations are not prompt history: by default they are skipped
/// entirely (`[input_history] record_commands = false`).
#[tokio::test]
async fn record_input_history_skips_slash_commands_by_default() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "session-a".to_string();
    app.record_input_history("/model".to_string(), Vec::new(), Vec::new());
    app.record_input_history("/new".to_string(), Vec::new(), Vec::new());
    assert!(
        app.input_history.is_empty(),
        "commands must not pollute the prompt history"
    );

    // Opting in restores them.
    app.input_history_record_commands = true;
    app.record_input_history("/model".to_string(), Vec::new(), Vec::new());
    assert_eq!(app.input_history.len(), 1);
    assert_eq!(app.input_history[0].text, "/model");
}

/// `App`'s test constructor keeps disk persistence off, so exercising the
/// record path must never dispatch persistence intents to the daemon
/// (regression: `record_input_history` used to merge synthetic `prompt N`
/// rows straight into the user's database file). The frontend has no
/// database access at all now (ADR-0197) — the guarantee is that no
/// persistence intent leaves the App when persistence is disabled.
#[tokio::test]
async fn test_app_does_not_touch_disk_history() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    assert!(
        !app.input_history_persist,
        "test-constructed App must default to no disk persistence"
    );
    // Retain the request receiver so dispatched intents are observable.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    app.tx = tx;

    app.current_session_id = "session-a".to_string();
    for i in 0..5 {
        app.record_input_history(format!("prompt {i}"), Vec::new(), Vec::new());
    }

    // Give any (buggy) dispatch a moment, then assert no persistence intent
    // was sent to the daemon.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let persistence_intents: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok())
        .filter(|req| {
            matches!(
                req,
                nuo_wire::AgentRequest::RecordInputHistory { .. }
                    | nuo_wire::AgentRequest::DeleteInputHistoryEntry { .. }
            )
        })
        .collect();
    assert!(
        persistence_intents.is_empty(),
        "persistence intents dispatched while persistence is disabled: {persistence_intents:?}"
    );
}

/// History rows are read-only snapshots: editing one is temporary and is
/// discarded the moment the pointer moves — coming back reloads the original
/// text (the shell "other rows are readonly" behaviour).
#[tokio::test]
async fn history_rows_are_readonly_snapshots() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "session-a".to_string();
    app.current_workspace = "~/p".to_string();
    app.record_input_history("older row".to_string(), Vec::new(), Vec::new());
    app.record_input_history("newest row".to_string(), Vec::new(), Vec::new());

    let rows = app.current_session_history();
    assert_eq!(rows.len(), 2);
    // Newest-first: rows[0] = "newest row" (later stamp), rows[1] = "older row".
    assert!(app.history_prev(&rows));
    assert_eq!(app.input, "newest row");
    assert!(app.history_prev(&rows));
    assert_eq!(app.input, "older row");

    // Edit the history row — the edit is temporary.
    app.input = "EDITED".to_string();
    // At the oldest row, pressing Up is a no-op: no reload, edit stays in place until moving away.
    assert!(!app.history_prev(&rows));
    assert_eq!(app.input, "EDITED");
    // Move away toward newer rows and back: the original text is reloaded.
    assert!(app.history_next(&rows));
    assert_eq!(app.input, "newest row");
    assert!(app.history_prev(&rows));
    assert_eq!(
        app.input, "older row",
        "history row reloads its original text"
    );
    assert!(app.history_next(&rows));
    assert_eq!(app.input, "newest row");
    assert!(!app.history_next(&rows));
    // The adopted/empty draft comes back, never the temporary edit.
    assert_eq!(app.input, app.history_draft);
}

/// The composer's recall badge (ADR-0192): a 1-based pointer position over
/// the current session's newest-first slice, present exactly while
/// `history_index` is `Some`, with the `edited` clause when the live buffer
/// has forked from the loaded row. Draft mode renders no badge — the
/// zero-mode-indication-tax stance (ADR-0173) means the only announced state
/// is the one that silently swaps the buffer.
#[tokio::test]
async fn history_recall_badge_tracks_the_pointer() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "session-a".to_string();
    app.current_workspace = "~/p".to_string();
    app.record_input_history("older row".to_string(), Vec::new(), Vec::new());
    app.record_input_history("newest row".to_string(), Vec::new(), Vec::new());
    let rows = app.current_session_history();
    assert_eq!(rows.len(), 2);

    // Draft mode: no badge.
    assert_eq!(app.history_recall_badge(), None);

    // First ↑ lands on the newest row: 1-based position 1 of 2, unedited.
    assert!(app.history_prev(&rows));
    assert_eq!(app.input, "newest row");
    assert_eq!(app.history_recall_badge(), Some((1, 2, false)));

    // Second ↑ walks to the older row: position 2 of 2, still unedited.
    assert!(app.history_prev(&rows));
    assert_eq!(app.input, "older row");
    assert_eq!(app.history_recall_badge(), Some((2, 2, false)));

    // Editing the buffer forks it from the loaded row → the badge's `edited`
    // clause goes live while the pointer still addresses row 2.
    app.input = "EDITED".to_string();
    assert_eq!(app.history_recall_badge(), Some((2, 2, true)));

    // ↓ back past the newest row returns to the draft: badge gone.
    assert!(app.history_next(&rows));
    assert!(!app.history_next(&rows));
    assert_eq!(app.history_index, None);
    assert_eq!(app.history_recall_badge(), None);
}

/// Esc during inline recall cancels it and restores the stashed draft —
/// the universal "get me back" chord must exit the recall state (ADR-0192).
#[tokio::test]
async fn esc_cancels_history_recall_and_restores_draft() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "session-a".to_string();
    app.current_workspace = "~/p".to_string();
    app.record_input_history("older row".to_string(), Vec::new(), Vec::new());
    app.record_input_history("newest row".to_string(), Vec::new(), Vec::new());
    let rows = app.current_session_history();

    // A draft (with a staged attachment) exists before navigation.
    app.input = "half-typed draft".to_string();
    app.history_draft = app.input.clone();

    assert!(app.history_prev(&rows));
    assert_eq!(app.input, "newest row");
    assert!(app.history_index.is_some());

    app.cancel_history_recall();
    assert_eq!(app.history_index, None, "recall cancelled");
    assert_eq!(app.input, "half-typed draft", "draft restored");
    // Cancel is a no-op in draft mode: the live draft is untouched.
    app.cancel_history_recall();
    assert_eq!(app.input, "half-typed draft");
}

/// Queue recall adopts the recalled content as the draft (text + attachments
/// mirrored into both the pending slots and the remembered-draft stash).
#[test]
fn recall_queued_adopts_content_as_draft() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let image = nuo_wire::ImagePart {
        mime: "image/png".to_string(),
        data: "abc".to_string(),
    };
    let mut dispatch = queued_dispatch("1", "session-a", "queued msg");
    dispatch.images = vec![image];
    app.pending_dispatch.push_back(dispatch);

    let Some(RecallQueued::Restored(dispatch)) = app.recall_queued("session-a") else {
        panic!("expected local restore");
    };
    app.restore_dispatch(dispatch);
    assert_eq!(app.input, "queued msg");
    assert_eq!(app.history_index, None, "recall enters draft mode");
    assert_eq!(
        app.history_draft, "queued msg",
        "recalled text becomes the draft"
    );
    assert_eq!(app.history_draft_images.len(), 1);
    assert_eq!(app.pending_images.len(), 1);
}

#[test]
fn recall_queued_always_restores_locally() {
    // With the insert/next-round distinction gone there is no agent-side
    // cancel to wait for: recall always pops the newest staged message and
    // hands it back as a local `Restored` item (the event loop then feeds it
    // to `restore_dispatch`), leaving the queue one item shorter.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.pending_dispatch
        .push_back(queued_dispatch("queued-1", "session-a", "queued"));

    let Some(RecallQueued::Restored(dispatch)) = app.recall_queued("session-a") else {
        panic!("expected local restore, not an agent cancel");
    };
    assert_eq!(dispatch.id, "queued-1");
    app.restore_dispatch(dispatch);
    assert_eq!(app.input, "queued");
    assert!(
        app.pending_dispatch.is_empty(),
        "recalled item must be removed from the outbox"
    );
}

#[test]
fn recall_queued_latches_completion_dismissal() {
    // A recall replaces `input` programmatically (not via a keystroke), so it
    // must latch `completion_dismissed` the same way a slash-command accept
    // does. Otherwise recalling a queued `/help` would immediately re-open the
    // slash-completion popup — a spurious "complete" step the user never asked
    // for. Mirrors the latch in the history-navigation paths.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.pending_dispatch
        .push_back(queued_dispatch("1", "session-a", "/help"));

    let Some(RecallQueued::Restored(dispatch)) = app.recall_queued("session-a") else {
        panic!("expected local restore");
    };
    app.restore_dispatch(dispatch);
    assert_eq!(app.input, "/help");
    assert!(
        app.completion_dismissed,
        "recall must latch dismissal so the slash popup stays hidden"
    );
    assert!(
        app.suggestion_index.is_none(),
        "recall must clear the completion highlight"
    );
    // The completions for `/help` are non-empty, so the latch is the only thing
    // keeping the render gate (`!completion_dismissed`) from drawing the menu.
    assert!(
        !app.completions().is_empty(),
        "`/help` should have candidates"
    );
}

#[test]
fn recall_queued_at_targets_selected_index_not_newest() {
    // The queue modal's `Enter` re-edits the *selected* item (the ↑/↓
    // highlight), so a mid-queue item can be pulled back rather than always
    // the newest. `recall_queued_at(idx=0)` returns the front (next to pop),
    // distinct from `recall_queued` which is LIFO/newest.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.pending_dispatch
        .push_back(queued_dispatch("front", "session-a", "first"));
    app.pending_dispatch
        .push_back(queued_dispatch("back", "session-a", "second"));

    // idx 0 = front = "first".
    let Some(RecallQueued::Restored(dispatch)) = app.recall_queued_at("session-a", 0) else {
        panic!("expected restore of selected item");
    };
    assert_eq!(dispatch.id, "front");
    app.restore_dispatch(dispatch);
    assert_eq!(app.input, "first");
    assert_eq!(
        app.pending_count("session-a"),
        1,
        "recalled item must leave the outbox"
    );

    // Now the only remaining item is "second"; idx 0 still works.
    let Some(RecallQueued::Restored(dispatch)) = app.recall_queued_at("session-a", 0) else {
        panic!("expected restore");
    };
    assert_eq!(dispatch.id, "back");

    // Out of range is a no-op (returns None), leaving the (now empty) queue
    // untouched.
    assert!(app.recall_queued_at("session-a", 0).is_none());
}

#[test]
fn test_delete_selected_history_entry_and_cascade() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "session-test".to_string();

    // 1. Setup 3 history entries
    app.input_history = vec![
        nuo_wire::HistoryEntry::new(
            "first entry".into(),
            Some("session-test".into()),
            None,
            100,
        ),
        nuo_wire::HistoryEntry::new(
            "target entry".into(),
            Some("session-test".into()),
            None,
            200,
        ),
        nuo_wire::HistoryEntry::new(
            "third entry".into(),
            Some("session-test".into()),
            None,
            300,
        ),
    ];
    // Backfill has target entry
    app.session_history_backfill = vec![
        nuo_wire::HistoryEntry::new(
            "target entry".into(),
            Some("session-test".into()),
            None,
            200,
        ),
        nuo_wire::HistoryEntry::new(
            "other backfill".into(),
            Some("session-test".into()),
            None,
            250,
        ),
    ];
    // Attachments has target entry
    let identity = ("target entry".to_string(), Some("session-test".to_string()));
    app.history_attachments.insert(
        identity.clone(),
        crate::app::HistoryAttachments {
            images: vec![],
            text_pastes: vec!["some paste".into()],
        },
    );
    app.history_attachments_order.push_back(identity.clone());

    // History order newest first: "third entry" (idx 2), "target entry" (idx 1), "first entry" (idx 0).
    // Let's select row 1 ("target entry")
    app.modal_index = 1;
    let removed = app.delete_selected_history_entry();
    assert!(removed.is_some());
    let removed = removed.unwrap();
    assert_eq!(removed.text, "target entry");

    // Verify removed from input_history
    assert_eq!(app.input_history.len(), 2);
    assert!(!app.input_history.iter().any(|e| e.text == "target entry"));

    // Verify pruned from session_history_backfill
    assert_eq!(app.session_history_backfill.len(), 1);
    assert_eq!(app.session_history_backfill[0].text, "other backfill");

    // Verify pruned from history_attachments and order
    assert!(!app.history_attachments.contains_key(&identity));
    assert!(!app.history_attachments_order.iter().any(|k| k == &identity));

    // Verify modal_index clamped
    assert_eq!(app.modal_index, 1);

    // Delete remaining entries
    app.delete_selected_history_entry();
    assert_eq!(app.input_history.len(), 1);
    app.delete_selected_history_entry();
    assert_eq!(app.input_history.len(), 0);
    assert_eq!(app.modal_index, 0);

    // Deleting from empty history is a safe no-op
    assert!(app.delete_selected_history_entry().is_none());
}

#[test]
fn test_shift_delete_and_bare_delete_dispatch() {
    use crate::keymap::Key;
    use crate::modal_keys::resolve_modal_key;
    use crossterm::event::{KeyCode, KeyModifiers};

    let c = crate::modal_keys::ModalKeys::default();

    // Shift+Delete in HistorySearch resolves to HistoryDeleteSelected
    let shift_del = Key::SHIFT_DELETE;
    let action = resolve_modal_key(
        Some(crate::surfaces::OverlaySurface::Dialog(
            crate::surfaces::DialogKind::HistorySearch,
        )),
        crate::surfaces::SceneKind::Conversation,
        shift_del,
        &c,
        &mut String::new(),
        &mut 0,
    );
    assert_eq!(
        action,
        Some(crate::input::InputAction::HistoryDeleteSelected)
    );

    // Bare Delete in HistorySearch falls through to None (text engine DeleteForward)
    let bare_del = Key {
        modifiers: KeyModifiers::NONE,
        code: KeyCode::Delete,
    };
    let action = resolve_modal_key(
        Some(crate::surfaces::OverlaySurface::Dialog(
            crate::surfaces::DialogKind::HistorySearch,
        )),
        crate::surfaces::SceneKind::Conversation,
        bare_del,
        &c,
        &mut String::new(),
        &mut 0,
    );
    assert_eq!(action, None);
}

#[test]
fn test_composer_hints_history_search_density() {
    use crate::components::composer_hints::{ActionDensity, ComposeTarget, hint_row_parts};
    use crate::render::Theme;

    let theme = Theme::default();
    let key = crate::keymap::Key::TAB;

    // Full density: includes close, navigate, delete, insert
    let (left, right) = hint_row_parts(
        false,
        ActionDensity::Full,
        ComposeTarget::HistorySearch,
        &theme,
        theme.panel(),
        key,
    );
    let left_str: String = left.iter().map(|s| s.content.as_ref()).collect();
    let right_str: String = right.iter().map(|s| s.content.as_ref()).collect();
    assert!(left_str.contains("close"));
    assert!(left_str.contains("navigate"));
    assert!(left_str.contains("delete"));
    assert!(left_str.contains("⇧Del"));
    assert!(right_str.contains("Tab"));
    assert!(right_str.contains("Enter"));
    assert!(right_str.contains("insert"));

    // Compact density: drops navigate and Tab
    let (left_c, right_c) = hint_row_parts(
        false,
        ActionDensity::Compact,
        ComposeTarget::HistorySearch,
        &theme,
        theme.panel(),
        key,
    );
    let left_c_str: String = left_c.iter().map(|s| s.content.as_ref()).collect();
    let right_c_str: String = right_c.iter().map(|s| s.content.as_ref()).collect();
    assert!(left_c_str.contains("close"));
    assert!(!left_c_str.contains("navigate"));
    assert!(left_c_str.contains("delete"));
    assert!(!right_c_str.contains("Tab"));
    assert!(right_c_str.contains("Enter"));

    // Tiny density: drops delete as well, only close and Enter insert
    let (left_t, right_t) = hint_row_parts(
        false,
        ActionDensity::Tiny,
        ComposeTarget::HistorySearch,
        &theme,
        theme.panel(),
        key,
    );
    let left_t_str: String = left_t.iter().map(|s| s.content.as_ref()).collect();
    let right_t_str: String = right_t.iter().map(|s| s.content.as_ref()).collect();
    assert!(left_t_str.contains("close"));
    assert!(!left_t_str.contains("delete"));
    assert!(!left_t_str.contains("navigate"));
    assert!(right_t_str.contains("Enter"));
}

#[tokio::test]
async fn test_ctrl_c_in_history_search() {
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;
    use tokio::sync::mpsc;

    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.open_dialog(crate::surfaces::DialogKind::HistorySearch);
    assert_eq!(
        app.active_dialog(),
        Some(crate::surfaces::DialogKind::HistorySearch)
    );

    // Case 1: Filter query is non-empty -> Ctrl+C clears the filter and resets cursor
    app.input = "my search query".to_string();
    app.set_cursor(5);
    app.modal_index = 2;

    let (copy_tx, _copy_rx) = mpsc::unbounded_channel();
    let copy_pending = Arc::new(AtomicUsize::new(0));

    crate::event_loop::handle_ctrl_c(&mut app, "test-session", &copy_tx, &copy_pending);

    // Dialog remains open, input cleared, modal_index reset
    assert_eq!(
        app.active_dialog(),
        Some(crate::surfaces::DialogKind::HistorySearch)
    );
    assert_eq!(app.input, "");
    assert_eq!(app.modal_index, 0);

    // Case 2: Filter query is empty -> Ctrl+C dismisses history dialog
    crate::event_loop::handle_ctrl_c(&mut app, "test-session", &copy_tx, &copy_pending);

    // Dialog dismissed
    assert_ne!(
        app.active_dialog(),
        Some(crate::surfaces::DialogKind::HistorySearch)
    );
}

#[test]
fn test_history_ranking_prefers_exact_word_over_scattered_and_applies_recency() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "live-session".to_string();

    let hour_ms = 3_600_000;
    let base_time = 1_000_000_000;

    app.input_history = vec![
        // Scattered acronym initials across sentence: "all dogs run in the park" (ts: newest!)
        nuo_wire::HistoryEntry::new(
            "all dogs run in the park".into(),
            Some("other-session".into()),
            None,
            base_time + 5 * hour_ms,
        ),
        // Exact whole word "adr", but typed 4 hours ago in other session
        nuo_wire::HistoryEntry::new(
            "let's write the adr".into(),
            Some("other-session".into()),
            None,
            base_time + hour_ms,
        ),
        // Word prefix "adroit approach"
        nuo_wire::HistoryEntry::new(
            "adroit approach to problems".into(),
            Some("other-session".into()),
            None,
            base_time + 2 * hour_ms,
        ),
        // Exact whole word "adr" typed recently in CURRENT session
        nuo_wire::HistoryEntry::new(
            "review the adr now".into(),
            Some("live-session".into()),
            None,
            base_time + 5 * hour_ms,
        ),
    ];

    app.input = "adr".to_string();
    let rows = app.history_rows();
    assert_eq!(rows.len(), 4, "all 4 match the subsequence 'adr'");

    // Row 0: "review the adr now" (exact word + current session + newest)
    assert_eq!(app.input_history[rows[0].0].text, "review the adr now");

    // Row 1: "let's write the adr" (exact word, older)
    assert_eq!(app.input_history[rows[1].0].text, "let's write the adr");

    // Row 2: "adroit approach to problems" (word prefix)
    assert_eq!(
        app.input_history[rows[2].0].text,
        "adroit approach to problems"
    );

    // Row 3: "all dogs run in the park" (scattered acronym, MUST be lowest rank despite newer timestamp!)
    assert_eq!(
        app.input_history[rows[3].0].text,
        "all dogs run in the park"
    );

    // Verify exact word scores strictly higher than scattered acronym
    assert!(rows[0].1.score > rows[3].1.score);
    assert!(rows[1].1.score > rows[3].1.score);
}

#[tokio::test]
async fn ctrl_c_clears_history_recall_and_resets_draft() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "session-a".to_string();
    app.current_workspace = "~/p".to_string();
    app.record_input_history("prior command".to_string(), Vec::new(), Vec::new());
    let rows = app.current_session_history();

    app.input = "draft text".to_string();
    assert!(app.history_prev(&rows));
    assert_eq!(app.input, "prior command");
    assert!(app.history_index.is_some());
    assert_eq!(app.history_draft, "draft text");

    // Edit the recalled text
    app.input.push_str(" --flag");

    let (copy_tx, _copy_rx) = tokio::sync::mpsc::unbounded_channel();
    let copy_pending = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));

    // Pressing Ctrl-C clears the recalled / edited text and exits recall mode
    crate::event_loop::handle_ctrl_c(&mut app, "session-a", &copy_tx, &copy_pending);

    assert_eq!(app.input, "");
    assert_eq!(app.history_index, None);
    assert_eq!(app.history_draft, "");
}

#[test]
fn esc_preserves_recalled_history_and_interrupts_when_running() {
    use crate::keymap::Key;
    use crate::session::{SceneKeys, resolve_chat_surface_key};

    let mut keys = SceneKeys {
        is_responding: false,
        composer_send_mode: Default::default(),
        completion_kind: crate::completion::CompletionKind::None,
        completion_dismissed: false,
        has_trigger_text: false,
        suggestion_count: 0,
        suggestion_index: None,
        has_exact_suggestion: false,
        in_history_recall: true,
        surface_overrides: Default::default(),
        focused_target: false,
        transcript_focused: false,
        focused_subagent_running: false,
    };

    let mut input = "recalled command with edits".to_string();
    let mut cursor = input.len();

    // Idle session: Esc does NOT clear or cancel recall
    let action = resolve_chat_surface_key(Key::ESC, &keys, &mut input, &mut cursor);
    assert_eq!(action, None, "Esc must not clear or cancel history recall");
    assert_eq!(input, "recalled command with edits");

    // Responding session: Esc resolves to Interrupt rather than clearing recall
    keys.is_responding = true;
    let action = resolve_chat_surface_key(Key::ESC, &keys, &mut input, &mut cursor);
    assert_eq!(
        action,
        Some(crate::input::InputAction::Interrupt),
        "Esc while running must interrupt rather than clear history recall"
    );
    assert_eq!(input, "recalled command with edits");
}

#[tokio::test]
async fn history_search_overlay_does_not_dim_composer() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "session-a".to_string();
    app.record_input_history("existing entry".to_string(), Vec::new(), Vec::new());

    // 1. Draw normal frame (no dialog)
    let mut terminal = nuotc::TestTerminal::new(80, 24);
    terminal.draw(|f| {
        crate::event_loop::render_frame(&mut app, f, "session-a");
    });
    // The composer bottom row is within the last rows (e.g. y = 22)
    let normal_cell = terminal.buffer().get(10, 22).cloned().expect("cell");

    // 2. Open HistorySearch
    app.open_dialog(crate::surfaces::DialogKind::HistorySearch);
    let mut terminal_hist = nuotc::TestTerminal::new(80, 24);
    terminal_hist.draw(|f| {
        crate::event_loop::render_frame(&mut app, f, "session-a");
    });
    let hist_cell = terminal_hist.buffer().get(10, 22).cloned().expect("cell");

    // Composer cells must not be dimmed when history search is open
    assert_eq!(
        hist_cell.bg, normal_cell.bg,
        "composer background should not be dimmed when history search is open"
    );
}

#[tokio::test]
async fn oldest_history_entry_up_arrow_does_not_cycle_cursor() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "session-a".to_string();
    app.current_workspace = "~/p".to_string();
    app.record_input_history("multiline\nentry\nbottom".to_string(), Vec::new(), Vec::new());
    let rows = app.current_session_history();
    assert_eq!(rows.len(), 1);

    // Initial draft
    app.input = "".to_string();
    assert!(app.history_prev(&rows));
    assert_eq!(app.input, "multiline\nentry\nbottom");
    assert_eq!(app.cursor_position, "multiline\nentry\nbottom".chars().count());

    // Move cursor up line-by-line
    assert!(crate::input::cursor_line_up(&app.input, &mut app.cursor_position));
    assert!(crate::input::cursor_line_up(&app.input, &mut app.cursor_position));
    // Cursor is now on the top line
    let top_line_cursor = app.cursor_position;
    assert!(!crate::input::cursor_line_up(&app.input, &mut app.cursor_position));
    assert_eq!(app.cursor_position, top_line_cursor);

    // Pressing Up at the top line tries history_prev: already at oldest entry, must be a no-op!
    assert!(!app.history_prev(&rows));
    // Cursor position and text must NOT change (must not reset cursor to end of message)
    assert_eq!(app.cursor_position, top_line_cursor);
    assert_eq!(app.input, "multiline\nentry\nbottom");
    assert_eq!(app.history_index, Some(0));
}

#[tokio::test]
async fn history_rows_scales_to_100k_entries_without_lag() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "session-scale".to_string();

    let count = 100_000;
    app.input_history.reserve(count);
    for i in 0..count {
        let text = if i % 1000 == 0 {
            format!("git commit -m 'release {i}'")
        } else if i % 200 == 0 {
            format!("cargo test package_{i}")
        } else {
            format!("random prompt message line number {i}")
        };
        app.input_history.push(nuo_wire::HistoryEntry::new(
            text,
            Some("session-scale".to_string()),
            Some("~/work".to_string()),
            i as u64,
        ));
    }

    assert_eq!(app.input_history.len(), count);

    // 1. Empty query returns all 100,000 items newest-first
    let t0 = std::time::Instant::now();
    app.input.clear();
    let rows_empty = app.history_rows();
    let d0 = t0.elapsed();
    assert_eq!(rows_empty.len(), count);
    assert!(
        d0.as_millis() < 500,
        "empty query on 100k items took {:?}, expected < 500ms in debug mode",
        d0
    );

    // 2. Filtered search with pre-filter pruning
    let t1 = std::time::Instant::now();
    app.input = "gcm".to_string();
    let rows_filtered = app.history_rows();
    let d1 = t1.elapsed();
    // 100 matches of "git commit -m 'release {i}'"
    assert_eq!(rows_filtered.len(), 100);
    assert!(
        d1.as_millis() < 250,
        "search query on 100k items took {:?}, expected < 250ms in debug mode",
        d1
    );

    // Verified results are ranked with highest scores first
    for w in rows_filtered.windows(2) {
        assert!(w[0].1.score >= w[1].1.score);
    }
}
