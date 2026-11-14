//! Queued-recall semantics and draft restoration.

use super::*;

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
    let Some(dispatch) = app.recall_queued("session-a") else {
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
    let Some(dispatch) = app.recall_queued("session-a") else {
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
    let image = nuo_tool::ImagePart {
        mime: "image/png".to_string(),
        data: "abc".to_string(),
    };
    let mut dispatch = queued_dispatch("1", "session-a", "look at this");
    dispatch.images = vec![image.clone()];
    app.pending_dispatch.push_back(dispatch);

    let Some(dispatch) = app.recall_queued("session-a") else {
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

    let image = nuo_tool::ImagePart {
        mime: "image/png".to_string(),
        data: "abc".to_string(),
    };
    let chip = nuo_view::composer_attachments::image_chip(1, 3);
    let paste = nuo_view::composer_attachments::paste_chip(1, 2, 11);
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

    let image = nuo_tool::ImagePart {
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
    let image = nuo_tool::ImagePart {
        mime: "image/png".to_string(),
        data: "draft-img".to_string(),
    };
    app.input = draft.clone();
    app.pending_images = vec![image.clone()];

    // First ↑: stash text + attachments (what the HistoryPrev handler does).
    app.stash_draft();

    // ↓ past the newest entry: restore text + attachments together.
    app.restore_stashed_draft();

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
            vec![nuo_tool::ImagePart {
                mime: "image/png".to_string(),
                data: format!("img-{i}"),
            }],
            Vec::new(),
        );
    }
    assert!(
        app.history_attachments.len() <= crate::shell::app::App::HISTORY_ATTACHMENTS_CAP,
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
/// record path must never dispatch persistence intents to the server
/// (regression: `record_input_history` used to merge synthetic `prompt N`
/// rows straight into the user's database file). The frontend has no
/// database access at all now — the guarantee is that no
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
    // was sent to the server.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let persistence_intents: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok())
        .filter(|req| {
            matches!(
                req,
                nuo_protocol::AgentRequest::RecordInputHistory { .. }
                    | nuo_protocol::AgentRequest::DeleteInputHistoryEntry { .. }
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
    assert_eq!(app.input, app.draft_capsule.text);
}

/// The composer's recall badge: a 1-based pointer position over
/// the current session's newest-first slice, present exactly while
/// `history_index` is `Some`, with the `edited` clause when the live buffer
/// has forked from the loaded row. Draft mode renders no badge — the
/// zero-mode-indication-tax stance means the only announced state
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
/// the universal "get me back" chord must exit the recall state.
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
    let image = nuo_tool::ImagePart {
        mime: "image/png".to_string(),
        data: "abc".to_string(),
    };
    let mut dispatch = queued_dispatch("1", "session-a", "queued msg");
    dispatch.images = vec![image];
    app.pending_dispatch.push_back(dispatch);

    let Some(dispatch) = app.recall_queued("session-a") else {
        panic!("expected local restore");
    };
    app.restore_dispatch(dispatch);
    assert_eq!(app.input, "queued msg");
    assert_eq!(app.history_index, None, "recall enters draft mode");
    assert_eq!(
        app.draft_capsule.text, "queued msg",
        "recalled text becomes the draft"
    );
    assert_eq!(app.draft_capsule.pending_images.len(), 1);
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

    let Some(dispatch) = app.recall_queued("session-a") else {
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

    let Some(dispatch) = app.recall_queued("session-a") else {
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
    let Some(dispatch) = app.recall_queued_at("session-a", 0) else {
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
    let Some(dispatch) = app.recall_queued_at("session-a", 0) else {
        panic!("expected restore");
    };
    assert_eq!(dispatch.id, "back");

    // Out of range is a no-op (returns None), leaving the (now empty) queue
    // untouched.
    assert!(app.recall_queued_at("session-a", 0).is_none());
}
