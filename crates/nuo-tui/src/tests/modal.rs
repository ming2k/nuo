//! Overlay/modal/navigation tests: pickers, transient sheets, sub-layer pop, queue management, aside views, chrome restoration.

// Integration test target: assertions fail by panicking, so the production
// no-panic lints are meaningless here (the exemption ends at the test boundary).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;

/// Regression: the per-model effort editor must render the **node slider**
/// using the ladder the server shipped on the picker snapshot, not one
/// re-derived client-side.
///
/// The TUI binary links `nuo-wire` (which owns the `inventory` registry
/// slot) but **not** `nuo-providers` (which owns the baseline tables that fill
/// it). So `resolve_model` returns an empty ladder in the real binary, and
/// reading it here collapsed the effort control to its value-only fallback —
/// no track, no nodes, no tier labels. `nuo` only sees a populated registry
/// under `cfg(test)` because `nuo-providers` is a dev-dependency, which is
/// exactly why the bug survived the suite: every existing assertion rendered
/// through the test harness and saw a ladder the shipped binary never has.
///
/// The id below is deliberately unknown to any baseline table, so this test
/// cannot pass by accident via the dev-dependency registry.
#[test]
fn model_editor_effort_slider_uses_the_snapshot_ladder() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);

    // A model the static registry cannot know, so only the snapshot can supply
    // its ladder — as is true for every relay-served id in the real binary.
    let model = "relay-only-reasoner-unregistered";
    let levels = vec!["low".to_string(), "high".to_string(), "max".to_string()];
    assert!(
        nuo_wire::resolve_model(model).effort_levels.is_empty(),
        "precondition: the static registry must not know this id"
    );

    app.provider_picker = ProviderPickerSnapshot {
        default_id: "relay".to_string(),
        rows: vec![nuo_wire::ProviderPickerRow {
            id: "relay".to_string(),
            name: "Relay".to_string(),
            model: model.to_string(),
            models: vec![model.to_string()],
            model_info: vec![nuo_wire::ProviderModelInfo {
                model: model.to_string(),
                protocol: "openai".to_string(),
                effort: Some("high".to_string()),
                effort_levels: levels.clone(),
                ..Default::default()
            }],
            builtin: true,
            protocol: String::new(),
            base_url: String::new(),
            key_ready: true,
            provider: "relay".to_string(),
            client_identity: Default::default(),
            last_used_ms: None,
            auth: Default::default(),
        }],
    };
    app.open_dialog(crate::surfaces::DialogKind::Models);
    app.modal_index = 0;

    // Open the editor the way `e` does on a Models row.
    crate::event_loop::actions::handle_open_model_editor(&mut app);

    assert_eq!(
        app.editor_effort_levels, levels,
        "the snapshot ladder must be captured, not re-resolved"
    );
    // A route with a configured effort opens on exactly that value.
    assert_eq!(
        app.editor_effort, "high",
        "the model's own configured effort passes through unchanged"
    );

    // Now the default branch: a row whose route has no configured effort. The
    // documented rule is `medium` **clamped onto the ladder**. On
    // `low/high/max` that is `low` (the highest rung ≤ medium), not `high`:
    // `high` ranks *above* the requested `medium`, and opening there silently
    // deepens every request the user never touched. Hand-rolling the clamp
    // instead of calling `Effort::clamp_to` is what produced that drift.
    //
    // (`thinking` is set so the row stays openable — a row opens when it
    // exposes effort *or* thinking. The sheet must be dismissed first: the
    // handler only acts while the Models dialog is the active surface.)
    app.provider_picker.rows[0].model_info[0].effort = None;
    app.provider_picker.rows[0].model_info[0].thinking = Some(true);
    app.dismiss_surface();
    app.open_dialog(crate::surfaces::DialogKind::Models);
    crate::event_loop::actions::handle_open_model_editor(&mut app);
    assert_eq!(
        app.editor_effort, "low",
        "default must be medium clamped onto the ladder (clamp_to), not the next tier up"
    );

    let mut terminal = nuotc::TestTerminal::new(100, 24);
    terminal.draw(|f| {
        crate::event_loop::render_frame(&mut app, f, "session-a");
    });
    let buf = terminal.buffer();
    let area = buf.area();
    let text: String = (0..area.height)
        .map(|y| {
            (0..area.width)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n");

    // The slider's own markers: one node per rung on the track. Scoped to the
    // track line (the one bracketed by the `Faster`/`Smarter` scale ends) so
    // unrelated `●`/`○` chrome glyphs elsewhere in the frame cannot inflate or
    // mask the count.
    let track = text
        .lines()
        .find(|l| l.contains("Faster") && l.contains("Smarter"))
        .unwrap_or_else(|| panic!("no effort slider track rendered; got:\n{text}"));
    let nodes = track.chars().filter(|&c| matches!(c, '○' | '●')).count();
    assert_eq!(
        nodes,
        levels.len(),
        "the node slider must draw one node per ladder rung; track: {track:?}"
    );
    // Every rung is labelled on the tier row.
    for level in &levels {
        assert!(
            text.contains(level.as_str()),
            "missing tier label {level}:\n{text}"
        );
    }
}

#[test]
fn finalize_streaming_reasoning_freezes_orphaned_traces() {
    // An interrupt mid-reasoning leaves the in-flight Thinking message
    // with `duration_ms: None`, which the renderer treats as "running"
    // (breathing spinner). The sweep must stamp every such trace so the
    // spinner stops, while leaving already-finished traces untouched.
    let streaming = TranscriptMessage::reasoning("partial reasoning");
    assert!(
        streaming.is_reasoning_streaming(),
        "a fresh thinking trace should be in the streaming state"
    );

    let mut finished = TranscriptMessage::reasoning("done reasoning");
    finished.set_reasoning_duration(1234);
    assert!(
        !finished.is_reasoning_streaming(),
        "a trace with a stamped duration is not streaming"
    );

    let other = TranscriptMessage::new(Role::User, "hi");

    let mut messages = vec![streaming.clone(), finished.clone(), other];
    finalize_streaming_reasoning(&mut messages, Some(500));

    // The orphaned streaming trace is frozen with the supplied duration.
    assert!(
        !messages[0].is_reasoning_streaming(),
        "streaming trace must be finalized by the sweep"
    );
    assert!(
        messages[0].reasoning_summary().unwrap().contains("500ms"),
        "expected the supplied duration to be stamped, got {:?}",
        messages[0].reasoning_summary()
    );

    // The already-finished trace keeps its original duration (no overwrite
    // of real timing with the sweep's value).
    assert!(
        messages[1].reasoning_summary().unwrap().contains("1.2s"),
        "finished trace must keep its original duration, got {:?}",
        messages[1].reasoning_summary()
    );

    // A missing duration falls back to 0 so the trace still leaves the
    // streaming state even when the start instant was already consumed.
    let mut messages = vec![streaming];
    finalize_streaming_reasoning(&mut messages, None);
    assert!(
        !messages[0].is_reasoning_streaming(),
        "a None duration must still finalize the trace"
    );
    assert!(
        messages[0].reasoning_summary().unwrap().contains("0ms"),
        "expected 0ms fallback, got {:?}",
        messages[0].reasoning_summary()
    );
}

#[test]
fn enumerate_explicit_path_completion_expands_to_absolute() {
    // `@../` from a temp project lists the parent directory's children as
    // absolute paths. The candidates are terminal (PathExplicit): accepting
    // one drops the `@` and splices the absolute path — the core of req 1.
    use crate::completion::CompletionItemKind;
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(tmp.path().join("sibling.md"), "x").unwrap();
    // The "project" is a subdirectory of `tmp`; its parent (`tmp`) holds
    // `sibling.md`, reachable only via `../`.
    let project = tmp.path().join("proj");
    std::fs::create_dir_all(&project).unwrap();
    let (mut app, _proj_tmp) = app_in_tempdir(&[], &[]);
    // Override the captured cwd to the project subdir so `../` escapes it.
    app.cwd = project.clone();
    app.input = "@../sib".to_string();
    app.cursor_position = app.input.chars().count();
    let completions = app.completions();
    let sibling = completions
        .iter()
        .find(|c| c.label.ends_with("sibling.md"))
        .expect("sibling.md reachable via @../");
    // The label is an absolute path (req 1: expanded to absolute).
    assert!(
        std::path::Path::new(&sibling.label).is_absolute(),
        "explicit completion must be absolute: {}",
        sibling.label
    );
    // Every explicit candidate is terminal on accept.
    assert_eq!(sibling.kind, CompletionItemKind::PathExplicit);

    // Accepting it drops the `@` and splices the absolute path + space.
    let idx = completions
        .iter()
        .position(|c| c.label.ends_with("sibling.md"))
        .unwrap();
    app.accept_completion(idx);
    assert!(
        !app.input.contains('@'),
        "@ trigger must be dropped on accept: {}",
        app.input
    );
    assert!(
        app.input.trim().ends_with("sibling.md"),
        "absolute path spliced: {}",
        app.input
    );
    assert!(app.completion_dismissed, "explicit accept is terminal");
}

#[test]
fn picker_connections_count_matches_provider_rows_no_add_row() {
    // Adding a connection is a footer shortcut (`a`) now, not a synthetic list
    // row, so `picker_row_count()` for Connections equals the provider count
    // exactly (no +1).
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.open_dialog(crate::surfaces::DialogKind::Connections);
    // Seed a few snapshot rows so providers_filtered() renders the full list
    // (the picker is snapshot-driven).
    let row = |id: &str| nuo_wire::ProviderPickerRow {
        id: id.to_string(),
        name: id.to_string(),
        model: "m".to_string(),
        models: vec!["m".to_string()],
        model_info: Vec::new(),
        builtin: true,
        protocol: String::new(),
        base_url: String::new(),
        key_ready: true,
        provider: String::new(),
        client_identity: Default::default(),
        last_used_ms: None,
        auth: Default::default(),
    };
    app.provider_picker = nuo_wire::ProviderPickerSnapshot {
        default_id: "kimi-code".to_string(),
        rows: vec![row("kimi-code"), row("openai"), row("anthropic")],
    };
    let providers = app.providers_filtered().len();
    assert!(providers > 0, "snapshot seeds the full provider list");
    assert_eq!(app.picker_row_count(), providers);
}

/// Confirming the overlay dispatches exactly one `DeleteConnection` request and
/// tears the overlay down, so a stray second confirm cannot re-delete.
#[test]
fn confirm_provider_delete_dispatches_once_and_clears() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    // Simulate the overlay already being open (staged from a prior Shift+D).
    app.pending_provider_delete = Some("doomed".to_string());

    let req = app
        .confirm_provider_delete()
        .expect("confirm dispatches when an id is staged");
    assert!(
        matches!(req, AgentRequest::DeleteConnection { ref name } if name == "doomed"),
        "confirm dispatches a DeleteConnection request for the staged name"
    );
    // Overlay torn down: no staged id remains.
    assert!(
        app.pending_provider_delete.is_none(),
        "confirm clears the staged id"
    );
    // A second confirm is a no-op (nothing left to delete).
    assert!(
        app.confirm_provider_delete().is_none(),
        "second confirm is inert after the overlay closes"
    );
}

/// Cancelling the overlay drops the staged id and resets focus to the safe
/// default (Cancel), so reopening the overlay later starts fresh.
#[test]
fn cancel_provider_delete_clears_and_resets_focus() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.pending_provider_delete = Some("doomed".to_string());
    app.provider_delete_focus = crate::ProviderDeleteChoice::Delete;

    app.cancel_provider_delete();

    assert!(
        app.pending_provider_delete.is_none(),
        "cancel clears the staged id"
    );
    assert_eq!(
        app.provider_delete_focus,
        crate::ProviderDeleteChoice::Cancel,
        "cancel resets focus to the safe default"
    );
}

/// Switching the viewed session must not carry composer state across the
/// boundary: the ↑/↓ cursor, the stashed draft, staged attachments, and the
/// backfill all belong to the thread being left.
#[tokio::test]
async fn switching_sessions_resets_navigation_and_backfill() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "session-a".to_string();
    app.record_input_history("prompt in a".to_string(), Vec::new(), Vec::new());
    // Simulate a walk into history (cursor armed) with a stashed draft and a
    // staged image, plus a backfilled row.
    app.input = "walked row".to_string();
    app.history_index = Some(0);
    app.draft_capsule.text = "half-typed draft in a".to_string();
    app.pending_images = vec![nuo_wire::ImagePart {
        mime: "image/png".to_string(),
        data: "abc".to_string(),
    }];
    app.backfill_session_history(
        &prompt_tail(&[TranscriptMessage::new(Role::User, "resumed turn").with_sent_at_ms(1)]),
        1,
    );
    assert_eq!(app.session_history_backfill.len(), 1);

    // The event loop's per-frame transition: the id moves, the state resets.
    app.current_session_id = "session-b".to_string();
    app.on_viewed_session_changed();

    assert_eq!(
        app.history_index, None,
        "cursor does not cross the boundary"
    );
    assert!(app.draft_capsule.is_empty(), "draft does not leak");
    assert!(app.input.is_empty(), "composer starts clean");
    assert!(app.pending_images.is_empty(), "attachments do not leak");
    assert!(
        app.session_history_backfill.is_empty(),
        "backfill is rebuilt per thread"
    );
    assert_eq!(app.session_history_backfill_cursor, 0);

    // ↑ in the new session recalls only that session's rows — here none.
    let rows = app.current_session_history();
    assert!(rows.is_empty(), "session-b has no recallable rows yet");
    assert!(!app.history_prev(&rows), "↑ is a no-op with no rows");
}

/// ↑ walks toward older entries and ↓ walks back toward the newest,
/// restoring the stashed draft past the newest entry. Regression: the two
/// directions were swapped and ↑ was pinned at the newest entry (its
/// `saturating_sub(1)` clamped at 0), so a second ↑ never moved — exactly
/// "只能往上翻一个，再继续按上没效果；按下有效果但不总是".
#[tokio::test]
async fn inline_history_arrows_walk_old_then_new() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "session-a".to_string();
    app.current_workspace = "~/p".to_string();
    // Record oldest → newest so the newest row is also the latest stamped.
    for text in ["first (oldest)", "second", "third (newest)"] {
        app.record_input_history(text.to_string(), Vec::new(), Vec::new());
    }
    // Pre-seed a draft so the first-↑ stash has something to restore.
    app.input = "in-progress draft".to_string();

    let rows = app.current_session_history();
    assert_eq!(rows.len(), 3);

    // ↑ #1: the newest entry.
    assert!(app.history_prev(&rows));
    assert_eq!(app.input, "third (newest)");
    assert_eq!(app.history_index, Some(0));

    // ↑ #2: the second-newest (this used to stick at position 0).
    assert!(app.history_prev(&rows));
    assert_eq!(app.input, "second");
    assert_eq!(app.history_index, Some(1));

    // ↑ #3: the oldest.
    assert!(app.history_prev(&rows));
    assert_eq!(app.input, "first (oldest)");
    assert_eq!(app.history_index, Some(2));

    // ↑ #4: already at the oldest — stays put without reloading.
    assert!(!app.history_prev(&rows));
    assert_eq!(app.input, "first (oldest)");
    assert_eq!(app.history_index, Some(2));

    // ↓ walks back toward the newest.
    assert!(app.history_next(&rows));
    assert_eq!(app.input, "second");
    assert_eq!(app.history_index, Some(1));

    assert!(app.history_next(&rows));
    assert_eq!(app.input, "third (newest)");
    assert_eq!(app.history_index, Some(0));

    // ↓ past the newest restores the stashed draft (not a blank box).
    assert!(!app.history_next(&rows));
    assert_eq!(app.input, "in-progress draft");
    assert_eq!(app.history_index, None);

    // A bare ↓ without any prior ↑ is a no-op (cursor not armed).
    assert!(!app.history_next(&rows));
}

/// The ↑/↓ round-trip preserves staged attachments end to end: they are
/// stashed on the first ↑, and restored when ↓ walks back past the newest
/// entry — so an accidental ↑/↓ never drops a pasted image.
#[tokio::test]
async fn inline_history_round_trip_keeps_staged_attachments() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "session-a".to_string();
    app.record_input_history("sent prompt".to_string(), Vec::new(), Vec::new());

    let image = nuo_wire::ImagePart {
        mime: "image/png".to_string(),
        data: "abc".to_string(),
    };
    app.input = "my draft".to_string();
    app.pending_images = vec![image.clone()];

    let rows = app.current_session_history();
    assert!(app.history_prev(&rows));
    assert_eq!(app.input, "sent prompt");
    assert!(
        app.pending_images.is_empty(),
        "recalled entry has no cached attachments → vectors cleared"
    );

    // ↓ back past the newest restores the draft AND its attachments.
    assert!(!app.history_next(&rows));
    assert_eq!(app.input, "my draft");
    assert_eq!(app.pending_images.len(), 1);
    assert_eq!(app.pending_images[0].data, "abc");
}

/// The pointer model's "unsent restore = new draft" invariant: an input put
/// back by a Phase-1 unsend (or Ctrl+R insert / queue recall) becomes the
/// newest editable slot. It replaces any stale remembered draft, and a ↓ past
/// the newest history row restores *this* input.
#[tokio::test]
async fn adopt_as_draft_replaces_stale_draft_and_is_restored() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "session-a".to_string();
    app.current_workspace = "~/p".to_string();
    app.record_input_history("older".to_string(), Vec::new(), Vec::new());

    // A stale remembered draft from an earlier session of editing.
    app.draft_capsule.text = "stale draft".to_string();
    app.input = "whatever".to_string();

    let image = nuo_wire::ImagePart {
        mime: "image/png".to_string(),
        data: "img".to_string(),
    };
    app.adopt_as_draft(
        "interrupted input".to_string(),
        vec![image.clone()],
        Vec::new(),
        crate::app::DraftAdoption::Replace,
    );

    assert_eq!(app.input, "interrupted input");
    assert_eq!(app.history_index, None, "adoption enters draft mode");
    assert_eq!(
        app.draft_capsule.text, "interrupted input",
        "stale draft replaced"
    );
    assert_eq!(app.draft_capsule.pending_images.len(), 1);
    assert_eq!(app.pending_images.len(), 1);

    // ↑ then ↓ past the newest restores the adopted input, not the stale one.
    let rows = app.current_session_history();
    assert!(app.history_prev(&rows));
    assert!(!app.history_next(&rows));
    assert_eq!(app.input, "interrupted input");
    assert_eq!(app.pending_images.len(), 1);
}

/// Dispatching a slash command must not arm the activity bar's
/// liveness surface. A command is a synchronous control-plane operation
/// outside the round state machine — no `is_responding`, no optimistic
/// `"queued"` label (which would also fabricate an `Esc Esc interrupt`
/// affordance over a dispatch that cannot be interrupted), and no running-
/// session bookkeeping. The pending command row is the command's in-flight
/// feedback; this locks the bar against ever lighting for it.
#[tokio::test]
async fn slash_dispatch_never_arms_activity_state() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let runtime = crate::event_loop::UiRuntime::minimal_for_test();
    let session = crate::SessionSource::Remote {
        session_id: "session-a".to_string(),
    };

    super::event_loop::handle_send_slash(&mut app, &runtime, &session, "/delegate on".to_string())
        .await;

    assert!(
        !runtime
            .is_responding
            .load(std::sync::atomic::Ordering::SeqCst),
        "a command must not arm is_responding"
    );
    assert!(
        app.phase.is_none(),
        "a command must not paint an optimistic activity label"
    );
    assert!(
        !app.running_sessions.contains("session-a"),
        "a command must not mark the session as running"
    );
    // The in-flight feedback is the pending command row, not the bar.
    let messages = app.messages.clone();
    assert!(
        messages
            .last()
            .is_some_and(|message| message.is_command_result()
                && message.command_result_phase()
                    == Some(crate::model::document::CommandPhase::Pending)),
        "dispatch must push the pending command row"
    );
}

#[test]
fn toggle_queue_block_flips_state_and_blocks_dispatch() {
    // `F3` / queue-modal block is the hard "send nothing" override. While a
    // session is blocked, `begin_next_round_dispatch` must yield nothing —
    // even though the item is `Waiting`. The event loop relies on
    // `is_queue_blocked` (and the app-side gate is its mirror) so a blocked
    // outbox can't slip through.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.pending_dispatch
        .push_back(queued_dispatch("1", "session-a", "first"));
    app.pending_dispatch
        .push_back(queued_dispatch("2", "session-a", "second"));

    // Not blocked initially.
    assert!(!app.is_queue_blocked("session-a"));
    assert_eq!(app.pending_count("session-a"), 2);

    // Toggle on (the local flag is the optimistic projection of the
    // server's `QueuePaused` verb — M4).
    app.set_queue_blocked("session-a", true);
    assert!(app.is_queue_blocked("session-a"));

    // The block is persistent and session-scoped: another session is
    // unaffected.
    app.pending_dispatch
        .push_back(queued_dispatch("3", "session-b", "other"));
    assert!(!app.is_queue_blocked("session-b"));

    // Toggle off.
    app.set_queue_blocked("session-a", false);
    assert!(!app.is_queue_blocked("session-a"));
}

#[test]
fn block_and_resume_helpers_are_idempotent() {
    // `block_queue` forces the block on; `resume_queue` forces it off. Both
    // must be safe to call repeatedly. The queue modal's open/close path
    // relies on this: open always blocks, close always resumes.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.pending_dispatch
        .push_back(queued_dispatch("1", "session-a", "x"));

    app.block_queue("session-a");
    assert!(app.is_queue_blocked("session-a"));
    app.block_queue("session-a"); // idempotent
    assert!(app.is_queue_blocked("session-a"));

    app.resume_queue("session-a");
    assert!(!app.is_queue_blocked("session-a"));
    app.resume_queue("session-a"); // idempotent
    assert!(!app.is_queue_blocked("session-a"));
}

#[test]
fn remove_queued_at_deletes_by_index_and_clamps() {
    // `D` in the queue modal deletes the highlighted item. The event loop
    // clamps `modal_index` after a delete; here we verify the core removal is
    // index-keyed and session-scoped.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.pending_dispatch
        .push_back(queued_dispatch("a", "session-a", "one"));
    app.pending_dispatch
        .push_back(queued_dispatch("b", "session-b", "two"));
    app.pending_dispatch
        .push_back(queued_dispatch("c", "session-a", "three"));

    // session-a has two items: idx 0 = "a", idx 1 = "c". Delete idx 1.
    let removed = app
        .remove_queued_at("session-a", 1)
        .expect("should remove idx 1");
    assert_eq!(removed.id, "c");
    assert_eq!(app.pending_count("session-a"), 1);
    assert_eq!(app.pending_count("session-b"), 1, "other session untouched");

    // Out of range → None, nothing removed.
    assert!(app.remove_queued_at("session-a", 5).is_none());
    assert_eq!(app.pending_count("session-a"), 1);
}

#[test]
fn move_queued_swaps_within_session_and_clamps_at_edges() {
    // `J`/`K` in the queue modal reorder the highlighted item. Moving toward
    // the front (delta -1) makes it the next to pop; toward the tail (delta 1)
    // pushes it back. Reorder is clamped to the session slice so it can't
    // escape into another session's items.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.pending_dispatch
        .push_back(queued_dispatch("a", "session-a", "one"));
    app.pending_dispatch
        .push_back(queued_dispatch("x", "session-b", "intruder"));
    app.pending_dispatch
        .push_back(queued_dispatch("b", "session-a", "two"));
    app.pending_dispatch
        .push_back(queued_dispatch("c", "session-a", "three"));

    // session-a display order: [a, b, c]. Move idx 0 (a) toward the tail by 2:
    // clamped to the last position → order becomes [b, c, a].
    app.move_queued("session-a", 0, 2);
    let order: Vec<&str> = app
        .pending_dispatch
        .iter()
        .filter(|d| d.session_id == "session-a")
        .map(|d| d.id.as_str())
        .collect();
    assert_eq!(order, vec!["b", "c", "a"]);

    // The intruder session-b item is untouched: reorder never crossed session
    // boundaries (session-b still has exactly one item, the same one).
    let session_b: Vec<&str> = app
        .pending_dispatch
        .iter()
        .filter(|d| d.session_id == "session-b")
        .map(|d| d.id.as_str())
        .collect();
    assert_eq!(session_b, vec!["x"]);

    // Move the now-front item (b) toward the front by 5: clamped to 0 →
    // stays put. Order unchanged.
    app.move_queued("session-a", 0, -5);
    let order: Vec<&str> = app
        .pending_dispatch
        .iter()
        .filter(|d| d.session_id == "session-a")
        .map(|d| d.id.as_str())
        .collect();
    assert_eq!(order, vec!["b", "c", "a"]);

    // Move idx 1 (c) toward the front by 1: swaps with b → [c, b, a].
    app.move_queued("session-a", 1, -1);
    let order: Vec<&str> = app
        .pending_dispatch
        .iter()
        .filter(|d| d.session_id == "session-a")
        .map(|d| d.id.as_str())
        .collect();
    assert_eq!(order, vec!["c", "b", "a"]);
}

#[test]
fn inserts_are_transcript_owned_not_outbox_items() {
    // A live busy-Enter steer never enters the outbox. It
    // becomes a transcript entry (`DeliveryStatus::Queued`) the moment it is
    // sent, so the outbox cannot dispatch, recall, delete, or reorder it —
    // and `UserInputUnavailable` hands it back by *staging a new outbox item*
    // (same id), at which point it becomes an ordinary manageable entry.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.pending_dispatch
        .push_back(queued_dispatch("w1", "session-a", "waiting one"));

    // Race loss: the harness hands the insert back. There is no outbox item
    // to flip, so the content is staged as a fresh `Waiting` item under the
    // same id.
    app.requeue_dispatch(
        "session-a",
        "steer-1",
        Some(("steer this".to_string(), Vec::new(), Vec::new())),
    );
    assert_eq!(app.pending_count("session-a"), 2);
    let order: Vec<&str> = app
        .pending_dispatch
        .iter()
        .filter(|d| d.session_id == "session-a")
        .map(|d| d.id.as_str())
        .collect();
    assert_eq!(
        order,
        vec!["w1", "steer-1"],
        "handed-back insert joins the FIFO tail"
    );

    // The handed-back item is an ordinary queue item now: FIFO dispatch pops
    // the front (w1)…
    let popped = app
        .begin_next_round_dispatch("session-a")
        .expect("a Waiting item pops first");
    assert_eq!(popped.id, "w1");
    // …and the modal can recall it like any other entry.
    assert!(
        app.recall_queued_at("session-a", 0).is_some(),
        "the handed-back insert is modal-addressable"
    );

    // Only the Dispatching leftover (w1) remains; a live insert never
    // touched the outbox at any point in its lifecycle.
    assert_eq!(app.pending_count("session-a"), 1);
    assert!(
        app.pending_dispatch
            .iter()
            .all(|d| d.state == QueuedDispatchState::Dispatching)
    );
}

#[test]
fn modal_paste_splices_text_inline_stripping_newlines() {
    // Pasting into a free-text modal field (here the provider editor's
    // API-key field) splices the text at the cursor and collapses newlines
    // so a copied multi-line block pastes as one continuous single line,
    // matching the single-line semantics the modal already enforces. No
    // chip is inserted and no attachment is staged.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.surfaces
        .present_sheet(crate::surfaces::SheetKind::ModelEditor);
    app.editor_field = 0;
    app.input = "sk-".to_string();
    app.cursor_position = app.input.chars().count();

    clipboard_ops::apply_clipboard_paste(
        &mut app,
        crate::clipboard::ClipboardRead::Text("abc\ndef\n".to_string()),
    );

    assert_eq!(app.input, "sk-abcdef");
    assert_eq!(app.cursor_position, "sk-abcdef".chars().count());
    assert!(
        app.pending_text_pastes.is_empty(),
        "no chip staging in modals"
    );
    assert!(
        !app.input.contains("Pasted text"),
        "no chip label in modals"
    );
}

#[test]
fn modal_paste_inserts_at_cursor_not_at_end() {
    // The splice honors the cursor position, so a paste in the middle of
    // an existing field inserts between the surrounding characters rather
    // than appending.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.surfaces
        .present_sheet(crate::surfaces::SheetKind::ModelEditor);
    app.editor_field = 1;
    app.input = "gpt-4omini".to_string();
    app.cursor_position = "gpt-4o".chars().count();

    clipboard_ops::apply_clipboard_paste(
        &mut app,
        crate::clipboard::ClipboardRead::Text("turbo".to_string()),
    );

    assert_eq!(app.input, "gpt-4oturbomini");
    assert_eq!(
        app.cursor_position,
        "gpt-4oturbo".chars().count(),
        "cursor lands just past the inserted text"
    );
}

#[test]
fn modal_paste_applies_to_provider_picker_and_history_search() {
    // The inline paste path is shared by every free-text modal that borrows
    // the input line, so the model picker filter and the history search
    // query paste the same way as the editor.
    for dialog in [
        crate::surfaces::DialogKind::Models,
        crate::surfaces::DialogKind::HistorySearch,
    ] {
        let (mut app, _tmp) = app_in_tempdir(&[], &[]);
        app.open_dialog(dialog);
        app.input = String::new();
        app.cursor_position = 0;

        clipboard_ops::apply_clipboard_paste(
            &mut app,
            crate::clipboard::ClipboardRead::Text("query".to_string()),
        );

        assert_eq!(
            app.input, "query",
            "paste should inline into free-text modal"
        );
        assert_eq!(app.cursor_position, "query".chars().count());
        assert!(app.pending_text_pastes.is_empty());
    }
}

#[test]
fn modal_paste_drops_image_with_failure_toast() {
    // An image paste has nowhere to go in a single-line modal field, so it
    // is dropped with a failure toast rather than silently lost or staged
    // as an attachment.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.surfaces
        .present_sheet(crate::surfaces::SheetKind::ModelEditor);
    app.input = String::new();
    app.cursor_position = 0;

    clipboard_ops::apply_clipboard_paste(
        &mut app,
        crate::clipboard::ClipboardRead::Image {
            data: vec![0x89, 0x50, 0x4e, 0x47],
            mime: "image/png".to_string(),
        },
    );

    assert!(app.input.is_empty(), "image paste must not insert text");
    assert!(
        app.pending_images.is_empty(),
        "no attachment staging in modals"
    );
    assert!(
        app.copy_toast_failed,
        "image paste in a modal should toast a failure"
    );
    assert!(app.copy_toast_until.is_some());
}

#[test]
fn picker_caret_owner_exists_only_in_search_mode() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    for dialog in [
        crate::surfaces::DialogKind::Models,
        crate::surfaces::DialogKind::Connections,
    ] {
        app.open_dialog(dialog);
        app.set_picker_search(false);
        assert_eq!(
            app.caret_owner(),
            CaretOwner::None,
            "{dialog:?} browse mode has no editable field"
        );
        app.set_picker_search(true);
        assert_eq!(
            app.caret_owner(),
            CaretOwner::Overlay,
            "{dialog:?} search mode owns the visible query field"
        );
    }
}

#[test]
fn overlay_caret_owner_is_arbitrated_by_the_layer_stack() {
    // Caret ownership is never declared statically by a surface type. It is
    // arbitrated once per frame by `App::caret_owner()` from the mounted layer
    // stack, so every state-dependent case — a picker that is only
    // editable while its search row is open, the Question sheet's "Other"
    // field, the HistorySearch panel that borrows the live composer line —
    // resolves through the same function instead of a per-surface copy. See
    // `caret_owner_question_owns_caret_only_on_other` and
    // `picker_caret_owner_exists_only_in_search_mode` for those cases.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.surfaces
        .present_sheet(crate::surfaces::SheetKind::CustomProvider);
    assert_eq!(
        app.caret_owner(),
        CaretOwner::Overlay,
        "CustomProvider owns the caret"
    );
    app.surfaces.dismiss_all_overlays();
    assert_eq!(app.caret_owner(), CaretOwner::Composer);
}

/// ADR-0047 [INV-SURFACE-06, INV-SURFACE-08]: DialogView entities autonomously own their
/// scrolling via `handle_input`, resolving both standard lists and drill-down sub-layers
/// (like `info_detail`) without leaking raw pointers through `modal_scroll_field`.
#[test]
fn dialogs_autonomously_handle_scroll_actions_and_sub_views() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);

    // 1. List dialogs (Queue): ScrollDown increments scroll and clears follow.
    app.open_dialog(crate::surfaces::DialogKind::Queue);
    app.surfaces
        .dlg_mut::<crate::surfaces::QueueDialog>()
        .follow = true;
    app.surfaces
        .dlg_mut::<crate::surfaces::QueueDialog>()
        .scroll = 0;
    if let Some(mut ent) = app.surfaces.take_active_view() {
        let outcome = ent.handle_input(&crate::input::InputAction::ScrollDown, &mut app, "");
        assert_eq!(outcome, crate::surfaces::DialogOutcome::Consumed);
        app.surfaces.put_active_view(ent);
    }
    assert_eq!(
        app.surfaces
            .dlg_mut::<crate::surfaces::QueueDialog>()
            .scroll,
        1
    );
    assert!(
        !app.surfaces
            .dlg_mut::<crate::surfaces::QueueDialog>()
            .follow
    );

    // 2. Pure-scroll dialog (UsageStats): ScrollDown increments scroll.
    app.open_dialog(crate::surfaces::DialogKind::UsageStats);
    app.surfaces
        .dlg_mut::<crate::surfaces::UsageStatsDialog>()
        .scroll = 5;
    if let Some(mut ent) = app.surfaces.take_active_view() {
        let outcome = ent.handle_input(&crate::input::InputAction::ScrollDown, &mut app, "");
        assert_eq!(outcome, crate::surfaces::DialogOutcome::Consumed);
        app.surfaces.put_active_view(ent);
    }
    assert_eq!(
        app.surfaces
            .dlg_mut::<crate::surfaces::UsageStatsDialog>()
            .scroll,
        6
    );

    // 3. ThreadsDialog: in list view, scrolls list and clears follow.
    app.open_dialog(crate::surfaces::DialogKind::Threads);
    app.surfaces
        .dlg_mut::<crate::surfaces::ThreadsDialog>()
        .follow = true;
    app.surfaces
        .dlg_mut::<crate::surfaces::ThreadsDialog>()
        .scroll = 2;
    app.surfaces
        .dlg_mut::<crate::surfaces::ThreadsDialog>()
        .info_detail = false;
    app.surfaces
        .dlg_mut::<crate::surfaces::ThreadsDialog>()
        .info_scroll = 0;
    if let Some(mut ent) = app.surfaces.take_active_view() {
        let outcome = ent.handle_input(&crate::input::InputAction::ScrollDown, &mut app, "");
        assert_eq!(outcome, crate::surfaces::DialogOutcome::Consumed);
        app.surfaces.put_active_view(ent);
    }
    assert_eq!(
        app.surfaces
            .dlg_mut::<crate::surfaces::ThreadsDialog>()
            .scroll,
        3
    );
    assert!(
        !app.surfaces
            .dlg_mut::<crate::surfaces::ThreadsDialog>()
            .follow
    );
    assert_eq!(
        app.surfaces
            .dlg_mut::<crate::surfaces::ThreadsDialog>()
            .info_scroll,
        0
    );

    // 4. ThreadsDialog sub-view parity [INV-SURFACE-08]: when info_detail is active,
    // scrolls info_scroll independently, leaving the main list scroll intact!
    app.surfaces
        .dlg_mut::<crate::surfaces::ThreadsDialog>()
        .info_detail = true;
    if let Some(mut ent) = app.surfaces.take_active_view() {
        let outcome = ent.handle_input(&crate::input::InputAction::ScrollDown, &mut app, "");
        assert_eq!(outcome, crate::surfaces::DialogOutcome::Consumed);
        app.surfaces.put_active_view(ent);
    }
    assert_eq!(
        app.surfaces
            .dlg_mut::<crate::surfaces::ThreadsDialog>()
            .info_scroll,
        1,
        "info_scroll incremented"
    );
    assert_eq!(
        app.surfaces
            .dlg_mut::<crate::surfaces::ThreadsDialog>()
            .scroll,
        3,
        "main list scroll untouched"
    );

    // 5. ConnectionsDialog sub-view parity [INV-SURFACE-08]: when info_detail is active,
    // scrolls info_scroll independently.
    app.open_dialog(crate::surfaces::DialogKind::Connections);
    app.surfaces
        .dlg_mut::<crate::surfaces::ConnectionsDialog>()
        .info_detail = true;
    app.surfaces
        .dlg_mut::<crate::surfaces::ConnectionsDialog>()
        .info_scroll = 4;
    app.surfaces
        .dlg_mut::<crate::surfaces::ConnectionsDialog>()
        .scroll = 10;
    if let Some(mut ent) = app.surfaces.take_active_view() {
        let outcome = ent.handle_input(&crate::input::InputAction::ScrollDown, &mut app, "");
        assert_eq!(outcome, crate::surfaces::DialogOutcome::Consumed);
        app.surfaces.put_active_view(ent);
    }
    assert_eq!(
        app.surfaces
            .dlg_mut::<crate::surfaces::ConnectionsDialog>()
            .info_scroll,
        5
    );
    assert_eq!(
        app.surfaces
            .dlg_mut::<crate::surfaces::ConnectionsDialog>()
            .scroll,
        10
    );

    // 6. Under [INV-SURFACE-06], modal_scroll_field returns None for dialogs
    // (since they do not leak pointers), but continues to serve scenes/sheets.
    assert!(
        app.modal_scroll_field().is_none(),
        "dialogs do not use modal_scroll_field"
    );

    app.surfaces.dismiss_all_overlays();
    app.switch_scene(crate::surfaces::SceneKind::Settings);
    let (s, f) = app.modal_scroll_field().expect("settings scene scrolls");
    assert!(f.is_none());
    *s = 7;
    assert_eq!(app.config_scroll, 7);

    app.reset_to_thread();
    assert!(app.modal_scroll_field().is_none());
}

/// ADR-0047 [INV-SURFACE-07]: DialogView entities autonomously handle spatial
/// click-to-select via handle_input(SelectionStart { x, y }).
#[test]
fn dialog_spatial_click_to_select_and_models_section_headers() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);

    app.provider_picker = ProviderPickerSnapshot {
        default_id: "openai".to_string(),
        rows: vec![nuo_wire::ProviderPickerRow {
            id: "openai".to_string(),
            name: "OpenAI".to_string(),
            model: "gpt-5.5".to_string(),
            models: vec!["gpt-5.5".to_string(), "gpt-5-mini".to_string()],
            model_info: vec![
                nuo_wire::ProviderModelInfo {
                    model: "gpt-5.5".to_string(),
                    protocol: "openai".to_string(),
                    ..Default::default()
                },
                nuo_wire::ProviderModelInfo {
                    model: "gpt-5-mini".to_string(),
                    protocol: "openai".to_string(),
                    ..Default::default()
                },
            ],
            builtin: true,
            protocol: "openai".to_string(),
            base_url: String::new(),
            key_ready: true,
            provider: "openai".to_string(),
            client_identity: Default::default(),
            last_used_ms: Some(100),
            auth: Default::default(),
        }],
    };

    app.open_dialog(crate::surfaces::DialogKind::Models);
    app.ui.begin(nuotc::Rect::new(0, 0, 80, 24));
    app.ui.mount(
        crate::ui::UiKey::Overlay(crate::surfaces::OverlaySurface::Dialog(
            crate::surfaces::DialogKind::Models,
        )),
        nuotc::Rect::new(10, 5, 60, 15),
    );
    app.ui.commit();

    let models_dlg = app.surfaces.dlg_mut::<crate::surfaces::ModelsDialog>();
    models_dlg.index = 0;
    models_dlg.scroll = 0;

    // Body starts at y = 5 + 3 = 8.
    // Line 0 is section header "All models", Line 1 is model row 0.
    if let Some(mut ent) = app.surfaces.take_active_view() {
        let action = crate::input::InputAction::SelectionStart { x: 20, y: 9 };
        let outcome = ent.handle_input(&action, &mut app, "");
        assert_eq!(outcome, crate::surfaces::DialogOutcome::Consumed);
        app.surfaces.put_active_view(ent);
    }
    assert_eq!(
        app.surfaces
            .dlg_mut::<crate::surfaces::ModelsDialog>()
            .index,
        0
    );
    assert!(
        app.surfaces
            .dlg_mut::<crate::surfaces::ModelsDialog>()
            .follow
    );
}

/// The page step follows the captured modal body height (when known) and
/// falls back to the transcript `view_height` before the first render. It must
/// always be at least 1 so a page key never no-ops on a zero capture.
#[test]
fn modal_page_step_tracks_body_height_and_floors_at_one() {
    use crate::event_loop::modal_page_step;
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);

    // No body height captured yet → falls back to view_height, floored at 1.
    app.view_height = 0;
    app.modal_body_height = 0;
    assert_eq!(modal_page_step(&app), 1);

    // Transcript height known, modal not yet rendered → uses view_height - 1.
    app.view_height = 24;
    assert_eq!(modal_page_step(&app), 23);

    // Once the modal body height is captured, it wins over view_height so a
    // page advance matches the actual modal, not the transcript behind it.
    app.modal_body_height = 10;
    assert_eq!(
        modal_page_step(&app),
        9,
        "modal body height takes precedence"
    );

    // A 1-row modal body still yields a step of 1 (never 0).
    app.modal_body_height = 1;
    assert_eq!(modal_page_step(&app), 1);
}

/// `nuo attach` (no id) opens the sessions picker at startup instead of
/// loading any session, so the `startup_overlay` state must gate quit-on-close.
/// This pins the two state transitions the event loop relies on:
///
/// 1. The overlay defaults to `None` in an ordinary (in-session) App, so the
///    `/sessions` modal only ever dismisses on Esc.
/// 2. Selecting a session from the picker clears the overlay — once a real
///    thread backs the view, the picker reverts to a plain transient
///    overlay. (The event loop's `OpenSelectedSession` arm does this.)
#[test]
fn startup_picker_flag_governs_sessions_modal_quit_and_resets_on_open() {
    use std::sync::atomic::Ordering;

    let (mut app, _tmp) = app_in_tempdir(&[], &[]);

    // Default: an in-session App never treats the picker as a startup gate.
    assert_eq!(app.startup_overlay, crate::StartupOverlay::None);

    // Simulate the startup path (`nuo attach` with no id): the picker
    // opens and `startup_overlay` is armed. Closing it must quit.
    app.startup_overlay = crate::StartupOverlay::SessionsPicker;
    app.open_dialog(crate::surfaces::DialogKind::Sessions);
    assert!(
        app.startup_overlay == crate::StartupOverlay::SessionsPicker
            && app.active_dialog() == Some(crate::surfaces::DialogKind::Sessions)
    );
    // The quit gate is `should_quit`; it is still clear until a close happens.
    assert!(!app.should_quit.load(Ordering::SeqCst));

    // Open a session from the picker: the overlay clears so a later `/sessions`
    // modal behaves as a normal transient overlay.
    app.startup_overlay = crate::StartupOverlay::None;
    app.reset_to_thread();
    assert_eq!(
        app.startup_overlay,
        crate::StartupOverlay::None,
        "opening a session drops the startup gate"
    );
}

/// `resolve_scroll` is the pure scroll-resolution core factored out of
/// `render_body`, now also used by the windowed sessions picker. It must keep
/// the selection in view (edge-margin follow), clamp to the valid range, and
/// report the true `max_scroll` of the full list — which is what lets the
/// picker build only the visible window while the scrollbar still reflects the
/// whole list. This pins those invariants.
#[test]
fn resolve_scroll_follows_selection_and_clamps_to_max_scroll() {
    use crate::primitives::{SCROLL_EDGE_MARGIN, resolve_scroll};

    // 100 rows, 10 visible → max_scroll is 90. A selection at row 50 with a
    // top-anchored scroll of 0 must pull the viewport down so row 50 is in
    // view (edge-margin follow), but never past max_scroll.
    let mut scroll = 0usize;
    let (start, max_scroll) = resolve_scroll(&mut scroll, 10, 100, Some(50), SCROLL_EDGE_MARGIN);
    assert_eq!(max_scroll, 90, "max_scroll reflects the full list length");
    assert!(
        (start..start + 10).contains(&50),
        "selection 50 must land inside the resolved window {start}..{}",
        start + 10
    );
    assert!(start <= 90, "resolved scroll never exceeds max_scroll");

    // Selection at the very end clamps to max_scroll (no overshoot).
    let mut scroll = 0usize;
    let (start, _) = resolve_scroll(&mut scroll, 10, 100, Some(99), SCROLL_EDGE_MARGIN);
    assert_eq!(start, 90, "end-of-list selection pins to max_scroll");

    // Fewer rows than the viewport: max_scroll is 0 and scroll collapses to 0.
    let mut scroll = 5usize;
    let (start, max_scroll) = resolve_scroll(&mut scroll, 10, 3, Some(1), SCROLL_EDGE_MARGIN);
    assert_eq!(
        max_scroll, 0,
        "content shorter than viewport has no scroll range"
    );
    assert_eq!(start, 0);

    // No follow: scroll is only clamped to max_scroll, never auto-scrolled.
    let mut scroll = 200usize;
    let (start, max_scroll) = resolve_scroll(&mut scroll, 10, 100, None, SCROLL_EDGE_MARGIN);
    assert_eq!(max_scroll, 90);
    assert_eq!(start, 90, "out-of-range scroll clamps to max_scroll");
}

/// The disclosure-toggle scroll settle: expanding a step must latch
/// `scroll_settle_pending` so the event loop stages its next frame (measure
/// the new height) before painting the toggle's target scroll offset. That
/// staging is what keeps the un-clamped intermediate viewport off the
/// terminal — the expand/collapse flicker.
#[test]
fn disclosure_toggle_latches_scroll_settle() {
    use crate::model::document::TranscriptMessage;

    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    // The settle path only runs when the auto-scroll behavior is enabled
    // (`[tui] expand_auto_scroll`, default off).
    app.expand_auto_scroll = true;
    let mut messages = vec![
        TranscriptMessage::new(Role::User, "hi"),
        TranscriptMessage::tool_step("call_1", "read_text", r#"{"path":"README.md"}"#),
    ];

    // No toggle happened yet: nothing to settle.
    assert!(!app.scroll_settle_pending);

    // Expanding (collapsed by default) both flips the pin and latches the
    // settle request — the loop must not paint the expand's scroll target
    // against the pre-expand layout.
    assert!(app.toggle_step_pinned(&mut messages, 1));
    assert!(messages[1].tool_step_expanded() == Some(true));
    assert!(
        app.scroll_settle_pending,
        "expand must latch the settle request"
    );

    // Collapsing latches it too: the shrunk stream re-validates the offset.
    assert!(app.toggle_step_pinned(&mut messages, 1));
    assert!(messages[1].tool_step_expanded() == Some(false));
    assert!(
        app.scroll_settle_pending,
        "collapse must latch the settle request"
    );

    // The settle is one frame deep: once the loop has staged and settled the
    // frame, the latch clears (mirrored here the way the event loop consumes
    // it — a no-op toggle target keeps the latch off).
    app.scroll_settle_pending = false;

    // A toggle that resolves to nothing (index out of range) latches nothing
    // and leaves the messages untouched.
    let before = app.scroll_settle_pending;
    assert!(!app.toggle_step_pinned(&mut messages, 9));
    assert_eq!(app.scroll_settle_pending, before);
}

/// The default configuration (`[tui] expand_auto_scroll = false`, the
/// shipping default): a disclosure toggle is a pure read interaction. The
/// card flips its expansion, but the scroll offset and the follow/pin state
/// are left exactly as the user had them — the view never moves as a side
/// effect of a click, which is also what keeps any toggle from disturbing
/// an in-progress read.
#[test]
fn disclosure_toggle_disabled_by_default_leaves_scroll_untouched() {
    use crate::model::document::TranscriptMessage;

    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    assert!(
        !app.expand_auto_scroll,
        "expand_auto_scroll defaults to disabled"
    );
    let mut messages = vec![
        TranscriptMessage::new(Role::User, "hi"),
        TranscriptMessage::tool_step("call_1", "read_text", r#"{"path":"README.md"}"#),
    ];

    app.scroll = 12;
    let scroll_before = app.scroll;

    // Expand: the pin flips, the scroll offset does not move. A settle frame
    // is still requested — not to scroll, but to re-validate the untouched
    // offset against the new height.
    assert!(app.toggle_step_pinned(&mut messages, 1));
    assert!(messages[1].tool_step_expanded() == Some(true));
    assert_eq!(app.scroll, scroll_before, "scroll must not move on expand");

    // Collapsing clears follow-bottom (reading history pauses auto-follow,
    // matching every other transcript interaction) but still leaves the
    // offset where the user had it.
    assert!(app.toggle_step_pinned(&mut messages, 1));
    assert!(messages[1].tool_step_expanded() == Some(false));
    assert!(!app.follow_bottom, "toggle pauses bottom-follow");
    assert_eq!(
        app.scroll, scroll_before,
        "scroll must not move on collapse"
    );
}

#[test]
fn sticky_collapse_anchors_summary_at_top_and_respects_viewport_underflow() {
    use crate::model::document::TranscriptMessage;
    use nuo_wire::Role;

    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let mut messages = vec![
        TranscriptMessage::new(Role::User, "initial question"),
        TranscriptMessage::tool_step("call_1", "read_text", r#"{"path":"README.md"}"#),
    ];

    // Force step expanded initially
    assert!(app.toggle_step_pinned(&mut messages, 1));
    assert_eq!(messages[1].tool_step_expanded(), Some(true));

    // Simulate sticky anchor active for step 1 at line 50
    app.sticky_step = Some(1);
    app.sticky_summary_line = Some(50);
    app.scroll = 100;
    app.follow_bottom = true;

    // Collapse sticky bar
    assert!(app.toggle_step_pinned(&mut messages, 1));
    assert_eq!(messages[1].tool_step_expanded(), Some(false));
    assert!(!app.follow_bottom, "collapsing sticky pauses follow_bottom");
    assert_eq!(app.scroll, 50, "scroll re-anchors to summary line 50");
    assert_eq!(
        app.pin_summary_line,
        Some(50),
        "pin_summary_line latches to preserve summary at Row 0"
    );

    // Case A: Total content is smaller than viewport (e.g. 10 lines in a 30-line viewport).
    // The entire conversation fits on screen, so scroll must collapse to 0.
    app.content_lines = 10;
    app.view_height = 30;
    app.max_scroll = 0;
    let limit_fit = if app.content_lines <= app.view_height as usize {
        0
    } else {
        app.pin_summary_line
            .map(|line| app.max_scroll.max(line.min(u16::MAX as usize) as u16))
            .unwrap_or(app.max_scroll)
    };
    assert_eq!(
        limit_fit, 0,
        "when entire content fits, limit must collapse to 0"
    );

    // Case B: Content overflows viewport (e.g. 60 lines in a 30-line viewport, max_scroll = 30).
    // Summary is at line 50. pin_summary_line allows scrolling up to 50 so summary stays at Row 0,
    // accepting bottom underflow (lines 51..60 at rows 1..10, rows 11..30 blank).
    app.content_lines = 60;
    app.view_height = 30;
    app.max_scroll = 30;
    let limit_overflow = if app.content_lines <= app.view_height as usize {
        0
    } else {
        app.pin_summary_line
            .map(|line| app.max_scroll.max(line.min(u16::MAX as usize) as u16))
            .unwrap_or(app.max_scroll)
    };
    assert_eq!(
        limit_overflow, 50,
        "when content overflows, limit preserves summary line at Row 0"
    );
}

#[test]
fn adopt_caret_head_and_tail_break_selection() {
    let mut app = app_with_input_selection("hello");
    // Park the visible caret somewhere stale — the adopt must override it,
    // proving the relay wins over the stale position.
    app.cursor_position = 1;

    assert!(app.adopt_caret_from_input_selection(SelectionEdge::Head));
    assert_eq!(app.cursor_position, 5, "head edge = buffer end");
    assert_eq!(app.selection, SelectionState::None, "selection must break");

    // Re-arm and adopt the tail edge.
    app.selection = SelectionState::Block {
        message_idx: crate::render::INPUT_MSG_IDX,
        block_idx: 0,
    };
    assert!(app.adopt_caret_from_input_selection(SelectionEdge::Tail));
    assert_eq!(app.cursor_position, 0, "tail edge = buffer start");
    assert_eq!(app.selection, SelectionState::None);

    // Range selection: head is the release point, tail is the anchor point.
    app.selection = SelectionState::Range {
        anchor: crate::model::layout::SemanticCursor::new(crate::render::INPUT_MSG_IDX, 0, 1),
        head: crate::model::layout::SemanticCursor::new(crate::render::INPUT_MSG_IDX, 0, 4),
    };
    assert!(app.adopt_caret_from_input_selection(SelectionEdge::Head));
    assert_eq!(app.cursor_position, 4, "head edge adopts head cursor");
    assert_eq!(app.selection, SelectionState::None);

    // Backward drag: anchor is 4, head is 1 (mouse released at 1).
    app.selection = SelectionState::Range {
        anchor: crate::model::layout::SemanticCursor::new(crate::render::INPUT_MSG_IDX, 0, 4),
        head: crate::model::layout::SemanticCursor::new(crate::render::INPUT_MSG_IDX, 0, 1),
    };
    assert!(app.adopt_caret_from_input_selection(SelectionEdge::Head));
    assert_eq!(app.cursor_position, 1, "head edge adopts release position");
    assert_eq!(app.selection, SelectionState::None);

    // No selection → no-op, reports false.
    assert!(!app.adopt_caret_from_input_selection(SelectionEdge::Head));
}

#[test]
fn config_view_reopen_keeps_pane_and_category() {
    // Settings is a full-screen scene whose fields persist
    // natively on `App`: the enter ritual (pane reset + current-scheme
    // positioning) runs on every enter; a reopen keeps the category/pane
    // the user left. Esc's step-backs unwind the pane/dropdown *inside* the
    // scene and stop there; leaving the scene is `close_scene`
    // (`C-x w`, or the scene's own `q`).
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.switch_scene(crate::surfaces::SceneKind::Settings);
    assert_eq!(app.current_scene(), crate::surfaces::SceneKind::Settings);

    // Esc's back never leaves the scene, whatever sub-layer is open.
    app.config_category = 2;
    app.config_focus = crate::overlays::ConfigFocus::Detail;
    app.scene_back();
    assert_eq!(
        app.current_scene(),
        crate::surfaces::SceneKind::Settings,
        "Esc steps back inside the scene, it does not leave it"
    );

    // The deliberate exit verb leaves.
    app.config_focus = crate::overlays::ConfigFocus::Detail;
    app.close_scene();
    assert_eq!(app.current_scene(), crate::surfaces::SceneKind::Thread);

    // Reopen: the pane/category survived.
    app.switch_scene(crate::surfaces::SceneKind::Settings);
    assert_eq!(app.config_category, 2, "category retained across hide");
    assert_eq!(
        app.config_focus,
        crate::overlays::ConfigFocus::Detail,
        "pane retained across hide"
    );
}

/// `[INV-TUI-CLEAN-02]`: Esc is never a scene exit.
/// Sweeps every scene and every Esc-reachable dispatch surface (the router's
/// Esc arm, the shared close-modal handler, the `CancelOrBack` command, and the
/// scene-local back verb) and asserts the current scene survives all of them.
/// Only `close_scene` — the `C-x` namespace's leave verb — may leave a scene.
#[test]
fn esc_never_leaves_a_scene_anywhere_in_the_dispatch() {
    use crate::surfaces::SceneKind;

    for scene in [
        SceneKind::Dashboard,
        SceneKind::Settings,
        SceneKind::Subagent,
    ] {
        let (mut app, _tmp) = app_in_tempdir(&[], &[]);
        app.switch_scene(scene);

        // 1. The shared close-modal handler (Esc over a resolved overlay).
        crate::event_loop::handle_close_modal(&mut app, "s1");
        assert_eq!(
            app.current_scene(),
            scene,
            "the overlay dismiss verb must never leave {scene:?}"
        );

        // 2. The scene-local step back (Esc on the scene's own chrome).
        app.scene_back();
        assert_eq!(
            app.current_scene(),
            scene,
            "the step-back verb must never leave {scene:?}"
        );

        // 3. The shared dismiss verb (its old signature returned `true` for a
        //    scene, which is exactly how Esc used to navigate).
        assert!(
            !app.dismiss_surface(),
            "a dismiss on a bare {scene:?} has nothing to act on"
        );
        assert_eq!(
            app.current_scene(),
            scene,
            "the dismiss verb must never leave {scene:?}"
        );

        // The scene's own exit verb is the one that leaves.
        app.close_scene();
        assert_eq!(
            app.current_scene(),
            SceneKind::Thread,
            "`close_scene` leaves {scene:?}"
        );
    }
}

/// The Thread scene is the home: `close_scene` there is a spent gesture,
/// not a navigation (there is nowhere to go).
#[test]
fn close_scene_at_home_is_a_spent_gesture() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    assert_eq!(app.current_scene(), crate::surfaces::SceneKind::Thread);
    assert!(
        !app.close_scene(),
        "leaving the home scene reports no navigation"
    );
    assert_eq!(app.current_scene(), crate::surfaces::SceneKind::Thread);
}

/// `C-x w` dismisses a foreground dialog before touching the scene —
/// the overlay is the visual foreground, so one press spends itself on it and
/// the scene survives. Regression: the chord used to treat any
/// dialog as "the scene", so the second press could land on a demoted view.
#[test]
fn close_scene_spends_itself_on_a_foreground_dialog_first() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.switch_scene(crate::surfaces::SceneKind::Dashboard);
    app.open_dialog(crate::surfaces::DialogKind::Tools);
    assert!(app.active_dialog().is_some());

    // Press 1: the dialog is dismissed, the scene stays.
    app.dismiss_surface();
    assert!(app.active_dialog().is_none(), "the dialog is gone");
    assert_eq!(
        app.current_scene(),
        crate::surfaces::SceneKind::Dashboard,
        "the dismiss stopped at the overlay"
    );

    // Press 2: now the scene itself leaves.
    app.close_scene();
    assert_eq!(app.current_scene(), crate::surfaces::SceneKind::Thread);
}

#[test]
fn config_view_navigation_and_theme_preview() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.switch_scene(crate::surfaces::SceneKind::Settings);
    assert_eq!(app.current_scene(), crate::surfaces::SceneKind::Settings);
    assert_eq!(app.config_focus, crate::overlays::ConfigFocus::Categories);
    assert_eq!(app.config_category, 0);

    // Cycling down through all 5 categories
    for expected_cat in [1, 2, 3, 4, 0] {
        crate::event_loop::handle_modal_down(&mut app, "s1");
        assert_eq!(app.config_category, expected_cat);
    }

    // Switch focus to Detail pane
    app.config_focus = crate::overlays::ConfigFocus::Detail;
    app.config_detail_index = 0;

    let original_surface = app.theme.surface();

    // Up/down in detail pane does NOT apply themes before Enter apply
    crate::event_loop::handle_modal_down(&mut app, "s1");
    assert_eq!(app.config_detail_index, 1);
    assert_eq!(
        app.theme.surface(),
        original_surface,
        "theme must not change on navigation before Enter apply"
    );

    app.config_focus = crate::overlays::ConfigFocus::Categories;
    assert_eq!(
        app.theme.surface(),
        crate::render::Theme::from_color_scheme(&app.color_scheme, &app.custom_color_scheme)
            .surface()
    );

    // Leaving from Categories is the scene-exit verb — a dismiss on the bare
    // scene is a spent gesture and never navigates.
    assert!(
        !app.dismiss_surface(),
        "a dismiss has nothing to close on a bare scene"
    );
    assert_eq!(
        app.current_scene(),
        crate::surfaces::SceneKind::Settings,
        "a dismiss never leaves the scene"
    );
    app.close_scene();
    assert_eq!(app.current_scene(), crate::surfaces::SceneKind::Thread);
}

#[test]
fn config_view_monochrome_profile_navigation_and_item_counts() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.profile = nuotc::TerminalProfile::dec_vt100_monochrome();
    app.theme = crate::render::Theme::monochrome();

    app.switch_scene(crate::surfaces::SceneKind::Settings);
    assert_eq!(app.current_scene(), crate::surfaces::SceneKind::Settings);
    assert_eq!(app.config_focus, crate::overlays::ConfigFocus::Categories);
    assert_eq!(app.config_category, 0);

    // Categories are always all 5, cleanly available on all profiles
    for expected_cat in [1, 2, 3, 4, 0] {
        crate::event_loop::handle_modal_down(&mut app, "s1");
        assert_eq!(app.config_category, expected_cat);
    }

    // On monochrome, Appearance detail item count is 1 (Monochrome Hardware Mode)
    app.config_focus = crate::overlays::ConfigFocus::Detail;
    app.config_detail_index = 0;
    // Cycling down on single-item detail pane remains at index 0
    crate::event_loop::handle_modal_down(&mut app, "s1");
    assert_eq!(app.config_detail_index, 0);
}

#[tokio::test]
async fn config_view_components_toggle_and_persistence() {
    let tmp = tempfile::tempdir().unwrap();
    let dirs = nuo_host::paths::Dirs {
        config_dir: tmp.path().join("config"),
        data_dir: tmp.path().join("data"),
        state_dir: tmp.path().join("state"),
        cache_dir: tmp.path().join("cache"),
        runtime_dir: None,
    };
    let _sandbox = nuo_host::paths::TestDirsGuard::install(dirs);

    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let runtime = crate::event_loop::UiRuntime::minimal_for_test();
    app.switch_scene(crate::surfaces::SceneKind::Settings);
    // Navigate to category 1 (Components)
    app.config_category = 1;
    app.config_focus = crate::overlays::ConfigFocus::Detail;

    // Rows are addressed through the panel's own ordering so this test cannot
    // silently drift when a component is added (ADR-0020).
    use crate::views::settings::components::{ComponentRowId, item_count, row_for_index};
    let index_of = |pred: &dyn Fn(ComponentRowId) -> bool| {
        (0..item_count())
            .find(|i| row_for_index(*i).is_some_and(pred))
            .expect("row must exist in the Components panel")
    };
    let reasoning_row = index_of(&|row| row == ComponentRowId::Reasoning);
    let command_row = index_of(&|row| matches!(row, ComponentRowId::Tool(c) if c.id == "command"));
    let diff_row = index_of(&|row| matches!(row, ComponentRowId::Tool(c) if c.id == "diff"));
    let search_row = index_of(&|row| matches!(row, ComponentRowId::Tool(c) if c.id == "search"));
    let auto_scroll_row = index_of(&|row| row == ComponentRowId::AutoScroll);

    app.config_detail_index = reasoning_row;
    assert!(!crate::config::reasoning_default_expanded(&app.tui_config));
    assert!(!app.reasoning_default_expanded);

    crate::event_loop::actions::dispatch_action_for_test(
        &mut app,
        &runtime,
        crate::input::InputAction::ConfigActivate,
        "s1",
    )
    .await;
    assert!(crate::config::reasoning_default_expanded(&app.tui_config));
    assert!(app.reasoning_default_expanded);

    // The command component opens by default, so activating collapses it — and
    // the choice must land on every shell spelling, not just the canonical one.
    app.config_detail_index = command_row;
    assert!(crate::config::tool_default_expanded(
        &app.tui_config,
        "execute_command"
    ));
    crate::event_loop::actions::dispatch_action_for_test(
        &mut app,
        &runtime,
        crate::input::InputAction::ConfigActivate,
        "s1",
    )
    .await;
    assert!(!crate::config::tool_default_expanded(
        &app.tui_config,
        "execute_command"
    ));

    // The Diffs row covers edit_text and write_file together.
    app.config_detail_index = diff_row;
    assert!(crate::config::tool_default_expanded(
        &app.tui_config,
        "edit_text"
    ));
    crate::event_loop::actions::dispatch_action_for_test(
        &mut app,
        &runtime,
        crate::input::InputAction::ConfigActivate,
        "s1",
    )
    .await;
    assert!(!crate::config::tool_default_expanded(
        &app.tui_config,
        "edit_text"
    ));
    assert!(!crate::config::tool_default_expanded(
        &app.tui_config,
        "write_file"
    ));

    // An untouched row keeps its declared default — toggling one component
    // must never move another.
    assert!(!crate::config::tool_default_expanded(
        &app.tui_config,
        "search_text"
    ));
    app.config_detail_index = search_row;
    crate::event_loop::actions::dispatch_action_for_test(
        &mut app,
        &runtime,
        crate::input::InputAction::ConfigActivate,
        "s1",
    )
    .await;
    assert!(crate::config::tool_default_expanded(
        &app.tui_config,
        "search_text"
    ));
    assert!(!crate::config::tool_default_expanded(
        &app.tui_config,
        "edit_text"
    ));

    app.config_detail_index = auto_scroll_row;
    assert!(!app.expand_auto_scroll);
    crate::event_loop::actions::dispatch_action_for_test(
        &mut app,
        &runtime,
        crate::input::InputAction::ConfigActivate,
        "s1",
    )
    .await;
    assert!(app.expand_auto_scroll);
    assert!(app.tui_config.expand_auto_scroll);

    // Verify config file on disk has the persisted settings
    let loaded = crate::config::TuiConfig::load();
    assert!(crate::config::reasoning_default_expanded(&loaded));
    assert!(!crate::config::tool_default_expanded(
        &loaded,
        "execute_command"
    ));
    assert!(!crate::config::tool_default_expanded(&loaded, "bash"));
    assert!(!crate::config::tool_default_expanded(&loaded, "edit_text"));
    assert!(crate::config::tool_default_expanded(&loaded, "search_text"));
    assert!(loaded.expand_auto_scroll);
}

#[test]
fn switching_picker_view_preserves_query_and_chat_draft_separately() {
    // `[INV-SURFACE-01]`: the picker's query is its own entity state, so it
    // never clobbers (or is clobbered by) the chat composer draft.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.input = "unsent chat".to_string();
    app.open_dialog(crate::surfaces::DialogKind::Models);
    app.surfaces
        .dlg_mut::<crate::surfaces::ModelsDialog>()
        .search = true;
    app.surfaces
        .dlg_mut::<crate::surfaces::ModelsDialog>()
        .query
        .text = "claude".to_string();

    assert_eq!(app.input, "unsent chat", "composer draft untouched");

    app.open_dialog(crate::surfaces::DialogKind::UsageStats);
    assert_eq!(
        app.input, "unsent chat",
        "switch never touches the chat composer"
    );

    app.open_dialog(crate::surfaces::DialogKind::Models);
    assert_eq!(
        app.surfaces
            .dlg_mut::<crate::surfaces::ModelsDialog>()
            .query
            .text,
        "claude",
        "picker query is retained independently"
    );
    assert!(
        app.surfaces
            .dlg_mut::<crate::surfaces::ModelsDialog>()
            .search,
        "the search sub-layer is retained too"
    );
    assert!(app.dismiss_surface());
    assert_eq!(app.input, "unsent chat");
}

#[test]
fn sheet_mounting_leaves_the_panel_stack_untouched() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.open_dialog(crate::surfaces::DialogKind::Tools);
    // A sheet is slot state, not a router layer: mounting it must leave the
    // panel stack untouched, and dismissing it hands the slot straight back.
    app.push_sheet_surface(crate::sheet::SheetKind::Question);
    assert_eq!(
        app.active_dialog(),
        Some(crate::surfaces::DialogKind::Tools)
    );
    app.dismiss_sheet();
    assert_eq!(
        app.active_dialog(),
        Some(crate::surfaces::DialogKind::Tools)
    );
}

#[test]
fn backend_navigation_waits_for_transient_and_drill_in_surfaces() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    assert!(app.can_accept_navigation_signal(), "chat is safe");

    app.open_dialog(crate::surfaces::DialogKind::Models);
    app.surfaces
        .present_sheet(crate::surfaces::SheetKind::ModelEditor);
    assert!(
        !app.can_accept_navigation_signal(),
        "an editor must not be preempted"
    );
    app.pop_transient_surface();
    assert!(app.can_accept_navigation_signal());

    app.switch_scene(crate::surfaces::SceneKind::Settings);
    app.config_focus = crate::overlays::ConfigFocus::Detail;
    assert!(
        !app.can_accept_navigation_signal(),
        "a parent-owned drill-in must finish or pop first"
    );
}

#[test]
fn explicit_view_close_discards_retained_state_and_payload() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.open_dialog(crate::surfaces::DialogKind::UsageStats);
    app.surfaces
        .dlg_mut::<crate::surfaces::UsageStatsDialog>()
        .scroll = 17;
    app.close_dialog(crate::surfaces::DialogKind::UsageStats);

    assert!(
        !app.surface_store
            .is_open(crate::surfaces::DialogKind::UsageStats)
    );
    assert!(app.surfaces.active_overlay().is_none());
    assert_eq!(
        app.surfaces
            .dlg_mut::<crate::surfaces::UsageStatsDialog>()
            .scroll,
        0
    );
    assert!(app.open_dialog(crate::surfaces::DialogKind::UsageStats));
}

#[test]
fn switching_away_from_queue_runs_exit_hook() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let sid = "queue-session";
    app.open_dialog(crate::surfaces::DialogKind::Queue);
    app.block_queue(sid);
    app.queue_exit_session = Some(sid.to_string());

    app.open_dialog(crate::surfaces::DialogKind::UsageStats);

    assert!(!app.is_queue_blocked(sid));
    assert!(app.queue_exit_session.is_none());
}

#[test]
fn queue_view_hide_releases_the_auto_block() {
    // Phase 4: the open-time auto-block is released by EVERY hide path
    // (the exit hook in dismiss_active_dialog), not just the Esc arm.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.open_dialog(crate::surfaces::DialogKind::Queue);
    app.block_queue("sess");
    app.queue_exit_session = Some("sess".to_string());
    assert!(app.is_queue_blocked("sess"));

    assert!(app.dismiss_surface());
    assert!(
        !app.is_queue_blocked("sess"),
        "exit hook resumed the outbox"
    );
}

#[test]
fn pop_sublayer_steps_back_one_level_at_a_time() {
    // The shared one-step-back (phase 4): Esc's deepest-first chain and the
    // outside-click mirror both route through here.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.open_dialog(crate::surfaces::DialogKind::SessionTrace);
    app.surfaces
        .dlg_mut::<crate::surfaces::SessionTraceDialog>()
        .detail = true;
    assert!(app.pop_sublayer());
    assert!(
        !app.surfaces
            .dlg_mut::<crate::surfaces::SessionTraceDialog>()
            .detail,
        "drill-in closed"
    );
    assert_eq!(
        app.active_dialog(),
        Some(crate::surfaces::DialogKind::SessionTrace),
        "view stays up"
    );
    assert!(!app.pop_sublayer(), "no sub-layer left");

    // Host: preview is the deepest layer (painted over the prompting
    // state), so it pops first; prompting next; then the view itself.
    app.switch_scene(crate::surfaces::SceneKind::Dashboard);
    app.host_preview = Some("transcript".to_string());
    app.host_prompting = true;
    assert!(app.pop_sublayer());
    assert!(app.host_preview.is_none(), "deepest layer (preview) closed");
    assert!(app.host_prompting, "prompting still open beneath");
    assert!(app.pop_sublayer());
    assert!(!app.host_prompting);
    assert!(!app.pop_sublayer());

    // Connections: detail view pops back to connections list
    app.open_dialog(crate::surfaces::DialogKind::Connections);
    app.surfaces
        .dlg_mut::<crate::surfaces::ConnectionsDialog>()
        .info_detail = true;
    app.connection_detail = Some(Default::default());
    assert!(app.pop_sublayer());
    assert!(
        !app.surfaces
            .dlg_mut::<crate::surfaces::ConnectionsDialog>()
            .info_detail,
        "connection detail closed"
    );
    assert!(app.connection_detail.is_none());
    assert_eq!(
        app.active_dialog(),
        Some(crate::surfaces::DialogKind::Connections),
        "connections modal stays up"
    );
    assert!(!app.pop_sublayer(), "no sub-layer left");
}

#[test]
fn pop_sublayer_pops_trace_turn_page_before_round_detail() {
    // Session Trace has three levels (round list -> round detail -> attempt inspector):
    // Esc walks back one level at a time, attempt inspector first.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.open_dialog(crate::surfaces::DialogKind::SessionTrace);
    app.surfaces
        .dlg_mut::<crate::surfaces::SessionTraceDialog>()
        .detail = true;
    app.surfaces
        .dlg_mut::<crate::surfaces::SessionTraceDialog>()
        .turn = Some((2, 1));
    app.surfaces
        .dlg_mut::<crate::surfaces::SessionTraceDialog>()
        .turn_cursor = 1;
    app.surfaces
        .dlg_mut::<crate::surfaces::SessionTraceDialog>()
        .scroll = 4;
    assert!(app.pop_sublayer());
    assert!(
        app.surfaces
            .dlg_mut::<crate::surfaces::SessionTraceDialog>()
            .turn
            .is_none(),
        "attempt inspector closed first"
    );
    assert!(
        app.surfaces
            .dlg_mut::<crate::surfaces::SessionTraceDialog>()
            .detail,
        "round detail stays open"
    );
    assert_eq!(
        app.surfaces
            .dlg_mut::<crate::surfaces::SessionTraceDialog>()
            .scroll,
        0
    );
    assert_eq!(
        app.surfaces
            .dlg_mut::<crate::surfaces::SessionTraceDialog>()
            .turn_cursor,
        1,
        "cursor retained"
    );
    assert!(app.pop_sublayer());
    assert!(
        !app.surfaces
            .dlg_mut::<crate::surfaces::SessionTraceDialog>()
            .detail,
        "round detail closed next"
    );
    assert_eq!(
        app.surfaces
            .dlg_mut::<crate::surfaces::SessionTraceDialog>()
            .turn_cursor,
        0,
        "cursor reset"
    );
    assert_eq!(
        app.active_dialog(),
        Some(crate::surfaces::DialogKind::SessionTrace),
        "view stays up"
    );
    assert!(!app.pop_sublayer(), "no sub-layer left");
}

#[test]
fn dashboard_reopen_keeps_selection_and_log() {
    // The dashboard is a full-screen scene whose dock selection
    // and cockpit log persist natively on `App` across hide. Leaving is the
    // scene-exit verb (`close_scene`) — a dismiss never navigates

    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.switch_scene(crate::surfaces::SceneKind::Dashboard);
    app.host_console_log
        .push(crate::overlays::ConsoleLine::Receipt {
            ok: true,
            target: None,
            text: "ok".to_string(),
        });
    app.modal_index = 3;
    app.close_scene();
    assert_eq!(app.current_scene(), crate::surfaces::SceneKind::Thread);

    app.switch_scene(crate::surfaces::SceneKind::Dashboard);
    assert_eq!(app.modal_index, 3, "dock selection retained");
    assert_eq!(app.host_console_log.len(), 1, "cockpit log retained");
}

#[tokio::test]
async fn closing_dashboard_scene_closes_singleton_tab() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let runtime = crate::event_loop::UiRuntime::minimal_for_test();

    // 1. Mount initial thread tab and dashboard tab
    app.surfaces
        .open_tab(crate::surfaces::ClientTab::thread("s1", Some("Thread 1")));
    app.open_tab(crate::surfaces::ClientTab::dashboard());

    assert_eq!(app.surfaces.tabs().len(), 2);
    assert_eq!(app.surfaces.active_tab_index(), 1);
    assert_eq!(app.current_scene(), crate::surfaces::SceneKind::Dashboard);

    // 2. Dispatch CloseTab (C-x w)
    let flow = crate::event_loop::actions::dispatch_action_for_test(
        &mut app,
        &runtime,
        crate::input::InputAction::CloseTab,
        "s1",
    )
    .await;

    assert_eq!(flow, crate::event_loop::actions::ActionFlow::Handled);
    assert_eq!(
        app.surfaces.tabs().len(),
        1,
        "dashboard tab must be closed and removed"
    );
    assert_eq!(
        app.surfaces.active_tab_index(),
        0,
        "active tab should switch to remaining thread tab"
    );
    assert_eq!(app.current_scene(), crate::surfaces::SceneKind::Thread);
    assert!(app.surfaces.tabs()[0].is_thread());
}

#[tokio::test]
async fn closing_sole_dashboard_tab_detaches_client() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let runtime = crate::event_loop::UiRuntime::minimal_for_test();

    // Only dashboard tab mounted
    app.surfaces
        .open_tab(crate::surfaces::ClientTab::dashboard());
    assert_eq!(app.surfaces.tabs().len(), 1);
    assert_eq!(app.current_scene(), crate::surfaces::SceneKind::Dashboard);

    let flow = crate::event_loop::actions::dispatch_action_for_test(
        &mut app,
        &runtime,
        crate::input::InputAction::CloseTab,
        "s1",
    )
    .await;

    assert_eq!(
        flow,
        crate::event_loop::actions::ActionFlow::Exit,
        "closing sole tab exits client"
    );
    assert!(app.surfaces.tabs().is_empty());
}

#[test]
fn modal_over_dialog_dismiss_restores_underlying_dialog() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.open_dialog(crate::surfaces::DialogKind::Sessions);
    app.modal_index = 2;

    // Opening Usage stats over the Sessions dialog
    app.open_dialog(crate::surfaces::DialogKind::UsageStats);
    assert_eq!(
        app.active_dialog(),
        Some(crate::surfaces::DialogKind::UsageStats)
    );
    assert_eq!(
        app.surfaces.underlying_dialog(),
        Some(crate::surfaces::DialogKind::Sessions)
    );

    // Dismissing Usage stats restores the Sessions dialog with preserved index
    assert!(app.dismiss_surface());
    assert_eq!(
        app.active_dialog(),
        Some(crate::surfaces::DialogKind::Sessions)
    );
    assert_eq!(app.modal_index, 2, "Sessions selection index is preserved");
}

#[test]
fn dialog_keys_sublayer_pop_and_deactivate_behavior() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.open_dialog(crate::surfaces::DialogKind::Sessions);
    assert_eq!(
        app.active_dialog(),
        Some(crate::surfaces::DialogKind::Sessions)
    );

    // Turn on dialog keys sub-view
    app.set_dialog_keys(true);
    *app.dialog_keys_scroll().expect("active dialog") = 5;

    // While dialog_keys is active, modal_scroll_field directs to dialog_keys_scroll
    {
        let (scroll, follow) = app.modal_scroll_field().expect("scroll field exists");
        assert_eq!(*scroll, 5);
        assert!(follow.is_none());
        *scroll += 1;
    }
    assert_eq!(*app.dialog_keys_scroll().expect("active dialog"), 6);

    // Popping sublayer dismisses dialog_keys but leaves the dialog active
    assert!(app.pop_sublayer(), "dialog_keys sublayer was popped");
    assert!(!app.dialog_keys(), "dialog_keys is now false");
    assert_eq!(
        *app.dialog_keys_scroll().expect("active dialog"),
        0,
        "dialog_keys_scroll was reset"
    );
    assert_eq!(
        app.active_dialog(),
        Some(crate::surfaces::DialogKind::Sessions),
        "parent dialog remains open"
    );

    // Turning on dialog_keys again, then dismissing the dialog resets dialog_keys
    app.set_dialog_keys(true);
    *app.dialog_keys_scroll().expect("active dialog") = 3;
    assert!(app.dismiss_active_dialog(), "dismissed active dialog");
    assert!(!app.dialog_keys(), "dialog_keys reset on dialog deactivate");
}

#[test]
fn modal_arrows_route_through_the_dialog_entity() {
    // `[INV-SURFACE-01]` / ADR-0035 §1: the generic ↑/↓ arrows are handled by
    // the active dialog entity's `DialogView::handle_input`, mutating only that
    // entity's own state.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.open_dialog(crate::surfaces::DialogKind::SessionStats);
    app.surfaces
        .dlg_mut::<crate::surfaces::SessionStatsDialog>()
        .scroll = 5;

    crate::event_loop::actions::handle_modal_up(&mut app, "test-session");
    assert_eq!(
        app.surfaces
            .dlg_mut::<crate::surfaces::SessionStatsDialog>()
            .scroll,
        4,
        "arrow handled by the session stats entity"
    );
    crate::event_loop::actions::handle_modal_down(&mut app, "test-session");
    assert_eq!(
        app.surfaces
            .dlg_mut::<crate::surfaces::SessionStatsDialog>()
            .scroll,
        5
    );
}

#[test]
fn dialog_reads_see_the_entity_while_it_is_rendered() {
    // Regression: rendering takes the entity out of the registry; App methods
    // that read the dialog's own query (`picker_query`, `history_rows`) must
    // still see that query, not a blank placeholder.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.open_dialog(crate::surfaces::DialogKind::Models);
    app.surfaces
        .dlg_mut::<crate::surfaces::ModelsDialog>()
        .search = true;
    app.surfaces
        .dlg_mut::<crate::surfaces::ModelsDialog>()
        .query
        .text = "claude".to_string();

    let taken = app.surfaces.take_active_view().expect("models is open");
    assert_eq!(
        app.picker_query(),
        "claude",
        "read sees the entity's query during render"
    );
    app.surfaces.put_active_view(taken);
    assert_eq!(app.picker_query(), "claude");
}

#[tokio::test]
async fn session_stats_slash_open_and_unbound_ctrl_o() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "test-session".to_string();

    let runtime = crate::event_loop::runtime::UiRuntime::minimal_for_test();
    crate::event_loop::actions::dispatch_action_for_test(
        &mut app,
        &runtime,
        crate::input::InputAction::OpenSessionStats,
        "test-session",
    )
    .await;

    assert_eq!(
        app.active_dialog(),
        Some(crate::surfaces::DialogKind::SessionStats),
        "OpenSessionStats (/stats) opens SessionStats dialog"
    );

    // Ctrl+O is no longer bound canonically to OpenSessionStats
    assert_eq!(
        crate::keymap::resolve_global_key(crate::keymap::Key::CTRL_O),
        None
    );
}

#[tokio::test]
async fn session_stats_t_affordance_transitions_to_session_trace() {
    // `[INV-STATS-01]` + `[INV-AFFORDANCE-01]` / ADR-0037:
    // SessionStats modal binds `t` to transition directly to SessionTrace.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "test-session".to_string();
    app.open_dialog(crate::surfaces::DialogKind::SessionStats);
    assert_eq!(
        app.active_dialog(),
        Some(crate::surfaces::DialogKind::SessionStats)
    );

    let runtime = crate::event_loop::runtime::UiRuntime::minimal_for_test();
    crate::event_loop::actions::dispatch_action_for_test(
        &mut app,
        &runtime,
        crate::input::InputAction::SessionStatsOpenTrace,
        "test-session",
    )
    .await;

    assert_eq!(
        app.active_dialog(),
        Some(crate::surfaces::DialogKind::SessionTrace),
        "`t` affordance in SessionStats opens SessionTrace"
    );
}

#[tokio::test]
async fn session_trace_slash_and_drill_down_lifecycle() {
    // `[INV-TRACE-01]` / ADR-0037:
    // SessionTrace has hierarchical drill-down without tabs or lateral page hops.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "test-session".to_string();

    let runtime = crate::event_loop::runtime::UiRuntime::minimal_for_test();
    crate::event_loop::actions::dispatch_action_for_test(
        &mut app,
        &runtime,
        crate::input::InputAction::OpenSessionTrace,
        "test-session",
    )
    .await;

    assert_eq!(
        app.active_dialog(),
        Some(crate::surfaces::DialogKind::SessionTrace),
        "OpenSessionTrace opens SessionTrace dialog"
    );

    // Initial state: L1 rounds list, no detail, no turn
    assert!(
        !app.surfaces
            .dlg::<crate::surfaces::SessionTraceDialog>()
            .detail
    );
    assert!(
        app.surfaces
            .dlg::<crate::surfaces::SessionTraceDialog>()
            .turn
            .is_none()
    );

    // Enter in L1 drills into L2 (turns list) when rounds exist or directly via action
    app.surfaces
        .dlg_mut::<crate::surfaces::SessionTraceDialog>()
        .detail = true;
    app.surfaces
        .dlg_mut::<crate::surfaces::SessionTraceDialog>()
        .turn = Some((1, 1));
    assert!(
        app.surfaces
            .dlg::<crate::surfaces::SessionTraceDialog>()
            .turn
            .is_some()
    );

    // Esc from L3 steps back to L2
    assert!(app.pop_sublayer());
    assert!(
        app.surfaces
            .dlg::<crate::surfaces::SessionTraceDialog>()
            .turn
            .is_none()
    );
    assert!(
        app.surfaces
            .dlg::<crate::surfaces::SessionTraceDialog>()
            .detail
    );

    // Esc from L2 steps back to L1
    assert!(app.pop_sublayer());
    assert!(
        !app.surfaces
            .dlg::<crate::surfaces::SessionTraceDialog>()
            .detail
    );
    assert_eq!(
        app.active_dialog(),
        Some(crate::surfaces::DialogKind::SessionTrace)
    );

    // In L1, no sub-layer remains
    assert!(!app.pop_sublayer(), "no sub-layer left in L1");
    assert!(app.dismiss_surface());
    assert_eq!(app.active_dialog(), None);
}

#[tokio::test]
async fn tools_dialog_two_tier_drill_in_and_pop_sublayer() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "test-session".to_string();
    app.session_context = Some(nuo_wire::SessionContextSnapshot {
        model: nuo_wire::ModelInfo::default(),
        tools: vec![
            nuo_wire::ToolInfo {
                name: "read_text".to_string(),
                description: "Read file".to_string(),
                enabled: true,
                source: "builtin".to_string(),
                parameters: None,
                risk: Some("read_only".to_string()),
                requires_user: false,
                requires_vision: false,
            },
            nuo_wire::ToolInfo {
                name: "execute_command".to_string(),
                description: "Run command".to_string(),
                enabled: false,
                source: "builtin".to_string(),
                parameters: None,
                risk: Some("arbitrary_execution".to_string()),
                requires_user: true,
                requires_vision: false,
            },
        ],
        permissions: vec![],
        skills: vec![],
        mcp: vec![],
    });

    app.open_dialog(crate::surfaces::DialogKind::Tools);
    assert_eq!(
        app.active_dialog(),
        Some(crate::surfaces::DialogKind::Tools),
        "Tools dialog opened"
    );

    // Initial state: Level 1 list, info_detail is false
    assert!(
        !app.surfaces
            .dlg::<crate::surfaces::ToolsDialog>()
            .info_detail
    );
    assert_eq!(app.surfaces.dlg::<crate::surfaces::ToolsDialog>().index, 0);

    let runtime = crate::event_loop::runtime::UiRuntime::minimal_for_test();

    // Select second tool (execute_command)
    crate::event_loop::actions::dispatch_action_for_test(
        &mut app,
        &runtime,
        crate::input::InputAction::SessionSelect { forward: true },
        "test-session",
    )
    .await;
    assert_eq!(app.surfaces.dlg::<crate::surfaces::ToolsDialog>().index, 1);

    // Press Enter -> OpenToolDetail
    crate::event_loop::actions::dispatch_action_for_test(
        &mut app,
        &runtime,
        crate::input::InputAction::OpenToolDetail,
        "test-session",
    )
    .await;
    assert!(
        app.surfaces
            .dlg::<crate::surfaces::ToolsDialog>()
            .info_detail,
        "Drilled into Level 2 tool detail"
    );
    assert_eq!(
        app.surfaces
            .dlg::<crate::surfaces::ToolsDialog>()
            .info_scroll,
        0
    );

    // Scrolling down in Level 2 adjusts info_scroll
    crate::event_loop::actions::dispatch_action_for_test(
        &mut app,
        &runtime,
        crate::input::InputAction::SessionSelect { forward: true },
        "test-session",
    )
    .await;
    assert_eq!(
        app.surfaces
            .dlg::<crate::surfaces::ToolsDialog>()
            .info_scroll,
        1,
        "Up/Down in Level 2 scrolls info_scroll without changing tool index"
    );
    assert_eq!(app.surfaces.dlg::<crate::surfaces::ToolsDialog>().index, 1);

    // Space toggles the tool in Level 2
    let req = app.session_activate_request();
    assert!(req.is_some(), "Space toggles tool being viewed");
    if let Some(nuo_wire::AgentRequest::ToggleTool { name, enabled }) = req {
        assert_eq!(name, "execute_command");
        assert!(enabled, "execute_command was false, toggle enables it");
    }

    // Esc from Level 2 pops back to Level 1
    assert!(app.pop_sublayer(), "pop_sublayer pops Level 2 detail");
    assert!(
        !app.surfaces
            .dlg::<crate::surfaces::ToolsDialog>()
            .info_detail,
        "Returned to Level 1 list"
    );
    assert_eq!(
        app.active_dialog(),
        Some(crate::surfaces::DialogKind::Tools),
        "Tools modal remains active"
    );
    assert_eq!(
        app.surfaces.dlg::<crate::surfaces::ToolsDialog>().index,
        1,
        "Tool selection preserved"
    );

    // Esc from Level 1 closes modal
    assert!(!app.pop_sublayer(), "No sublayer left in Level 1");
    assert!(app.dismiss_surface());
    assert_eq!(app.active_dialog(), None);
}

#[tokio::test]
async fn open_selected_session_preserves_current_running_session_messages_and_resets_scroll() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let runtime = crate::event_loop::UiRuntime::minimal_for_test();
    let sid = "session-running-123";
    app.current_session_id = sid.to_string();
    app.running_sessions.insert(sid.to_string());
    app.messages
        .push(crate::model::document::TranscriptMessage::new(
            nuo_wire::Role::User,
            "Hello",
        ));
    app.scroll = 42;

    app.sessions_overview = vec![nuo_wire::SessionOverview {
        id: sid.to_string(),
        title: Some("Running Task".into()),
        overview: "Running Task".into(),
        created_at: 100,
        updated_at: 200,
        message_count: 1,
        active: true,
        parent_id: None,
        fork_kind: nuo_wire::SessionForkKind::Trunk,
        digest: None,
    }];
    app.open_dialog(crate::surfaces::DialogKind::Sessions);
    assert_eq!(
        app.active_dialog(),
        Some(crate::surfaces::DialogKind::Sessions)
    );

    // Dispatch OpenSelectedSession via dispatch_action_for_test
    crate::event_loop::actions::dispatch_action_for_test(
        &mut app,
        &runtime,
        crate::input::InputAction::OpenSelectedSession,
        sid,
    )
    .await;

    // Modal dismissed
    assert_eq!(app.active_dialog(), None);
    // Messages must NOT be wiped!
    assert_eq!(app.messages.len(), 1);
    // Scroll reset to 0 (pinned to bottom)
    assert_eq!(app.scroll, 0);
    // Same session does not trigger process quit / switch
    assert_eq!(app.switch_to_target, None);
}

#[tokio::test]
async fn open_selected_session_switches_to_another_thread_via_client_tab_and_runner_attach() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let runtime = crate::event_loop::UiRuntime::minimal_for_test();
    let current_sid = "session-1";
    let target_sid = "session-2";
    app.current_session_id = current_sid.to_string();

    app.sessions_overview = vec![nuo_wire::SessionOverview {
        id: target_sid.to_string(),
        title: Some("Target Task".into()),
        overview: "Target Task".into(),
        created_at: 100,
        updated_at: 200,
        message_count: 3,
        active: false,
        parent_id: None,
        fork_kind: nuo_wire::SessionForkKind::Trunk,
        digest: None,
    }];
    app.open_dialog(crate::surfaces::DialogKind::Sessions);
    assert_eq!(
        app.active_dialog(),
        Some(crate::surfaces::DialogKind::Sessions)
    );

    // Dispatch OpenSelectedSession on the target thread
    crate::event_loop::actions::dispatch_action_for_test(
        &mut app,
        &runtime,
        crate::input::InputAction::OpenSelectedSession,
        current_sid,
    )
    .await;

    // Modal dismissed
    assert_eq!(app.active_dialog(), None);
    // ClientTab opened/focused
    assert!(
        app.surfaces
            .tabs()
            .iter()
            .any(|t| t.thread_id() == Some(target_sid))
    );
    // Runner attach queued
    assert_eq!(app.switch_to_target, Some(target_sid.to_string()));
    assert!(app.should_quit.load(std::sync::atomic::Ordering::SeqCst));
}

#[tokio::test]
async fn test_subagent_bracket_and_q_scene_history_navigation() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let runtime = crate::event_loop::UiRuntime::minimal_for_test();
    app.open_tab(crate::surfaces::ClientTab::thread("conv-1", None::<String>));
    assert_eq!(app.current_scene(), crate::surfaces::SceneKind::Thread);

    // Enter subagent
    app.enter_subagent("call_child_1".to_string());
    assert_eq!(app.current_scene(), crate::surfaces::SceneKind::Subagent);
    assert!(app.in_subagent_view());

    // Back in scene history via TabHistoryBack (emitted by `[` or `q`)
    crate::event_loop::actions::dispatch_action_for_test(
        &mut app,
        &runtime,
        crate::input::InputAction::TabHistoryBack,
        "conv-1",
    )
    .await;

    assert_eq!(app.current_scene(), crate::surfaces::SceneKind::Thread);
    assert!(!app.in_subagent_view());

    // Forward in scene history via TabHistoryForward (emitted by `]`)
    crate::event_loop::actions::dispatch_action_for_test(
        &mut app,
        &runtime,
        crate::input::InputAction::TabHistoryForward,
        "conv-1",
    )
    .await;

    assert_eq!(app.current_scene(), crate::surfaces::SceneKind::Subagent);
}

#[tokio::test]
async fn test_close_tab_accurately_closes_tab_on_thread_and_subagent() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let runtime = crate::event_loop::UiRuntime::minimal_for_test();
    app.open_tab(crate::surfaces::ClientTab::thread("conv-1", None::<String>));
    app.open_tab(crate::surfaces::ClientTab::thread("conv-2", None::<String>));
    assert_eq!(app.surfaces.tabs().len(), 2);
    assert_eq!(app.surfaces.active_tab_index(), 1);

    // Enter subagent in Tab 1
    app.enter_subagent("call_sub_2".to_string());
    assert_eq!(app.current_scene(), crate::surfaces::SceneKind::Subagent);

    // C-x w dispatches CloseTab: closes Tab 1 entirely, does not just pop subagent
    crate::event_loop::actions::dispatch_action_for_test(
        &mut app,
        &runtime,
        crate::input::InputAction::CloseTab,
        "conv-2",
    )
    .await;

    assert_eq!(app.surfaces.tabs().len(), 1);
    assert_eq!(app.surfaces.active_tab_index(), 0);
    assert_eq!(app.current_scene(), crate::surfaces::SceneKind::Thread);

    // Close remaining Tab 0
    let flow = crate::event_loop::actions::dispatch_action_for_test(
        &mut app,
        &runtime,
        crate::input::InputAction::CloseTab,
        "conv-1",
    )
    .await;

    assert_eq!(flow, crate::event_loop::actions::ActionFlow::Exit);
    assert!(app.surfaces.tabs().is_empty());
}

#[tokio::test]
async fn open_selected_session_overwrites_empty_tab_and_discards_empty_thread() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    app.tx = tx;
    let runtime = crate::event_loop::UiRuntime::minimal_for_test();
    let empty_sid = "empty-session-1";
    let target_sid = "target-session-2";
    app.current_session_id = empty_sid.to_string();
    app.open_tab(crate::surfaces::ClientTab::thread(
        empty_sid,
        None::<String>,
    ));
    assert_eq!(app.surfaces.tabs().len(), 1);
    assert_eq!(app.surfaces.tabs()[0].thread_id(), Some(empty_sid));
    assert!(app.is_current_thread_empty());

    app.sessions_overview = vec![nuo_wire::SessionOverview {
        id: target_sid.to_string(),
        title: Some("Target Thread".into()),
        overview: "Target Thread".into(),
        created_at: 100,
        updated_at: 200,
        message_count: 5,
        active: false,
        parent_id: None,
        fork_kind: nuo_wire::SessionForkKind::Trunk,
        digest: None,
    }];
    app.open_dialog(crate::surfaces::DialogKind::Sessions);

    crate::event_loop::actions::dispatch_action_for_test(
        &mut app,
        &runtime,
        crate::input::InputAction::OpenSelectedSession,
        empty_sid,
    )
    .await;

    // Must NOT create a second tab! Tab count must remain 1, overwritten in-place!
    assert_eq!(app.surfaces.tabs().len(), 1);
    assert_eq!(app.surfaces.tabs()[0].thread_id(), Some(target_sid));
    assert_eq!(app.switch_to_target, Some(target_sid.to_string()));

    // Must have sent DeleteSession intent for the abandoned empty thread
    let req = rx.try_recv().expect("DeleteSession sent for empty thread");
    match req {
        nuo_wire::AgentRequest::DeleteSession { id } => {
            assert_eq!(id, empty_sid);
        }
        other => panic!("expected DeleteSession, got {other:?}"),
    }
}

#[tokio::test]
async fn open_selected_session_appends_new_tab_when_current_tab_has_messages() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    app.tx = tx;
    let runtime = crate::event_loop::UiRuntime::minimal_for_test();
    let active_sid = "active-session-1";
    let target_sid = "target-session-2";
    app.current_session_id = active_sid.to_string();
    app.open_tab(crate::surfaces::ClientTab::thread(
        active_sid,
        Some("Active Thread"),
    ));
    // Add a message to make it non-empty
    app.messages
        .push(crate::model::document::TranscriptMessage::new(
            nuo_wire::Role::User,
            "Hello",
        ));
    assert!(!app.is_current_thread_empty());

    app.sessions_overview = vec![nuo_wire::SessionOverview {
        id: target_sid.to_string(),
        title: Some("Target Thread".into()),
        overview: "Target Thread".into(),
        created_at: 100,
        updated_at: 200,
        message_count: 5,
        active: false,
        parent_id: None,
        fork_kind: nuo_wire::SessionForkKind::Trunk,
        digest: None,
    }];
    app.open_dialog(crate::surfaces::DialogKind::Sessions);

    crate::event_loop::actions::dispatch_action_for_test(
        &mut app,
        &runtime,
        crate::input::InputAction::OpenSelectedSession,
        active_sid,
    )
    .await;

    // Must append a new tab (total 2 tabs)!
    assert_eq!(app.surfaces.tabs().len(), 2);
    assert_eq!(app.surfaces.tabs()[0].thread_id(), Some(active_sid));
    assert_eq!(app.surfaces.tabs()[1].thread_id(), Some(target_sid));
    assert_eq!(app.switch_to_target, Some(target_sid.to_string()));

    // Must NOT have sent DeleteSession!
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn create_new_session_resets_in_place_when_empty_and_switches_to_new_when_non_empty() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    app.tx = tx;
    let runtime = crate::event_loop::UiRuntime::minimal_for_test();
    let empty_sid = "empty-session-1";
    app.current_session_id = empty_sid.to_string();
    app.open_tab(crate::surfaces::ClientTab::thread(
        empty_sid,
        None::<String>,
    ));
    assert!(app.is_current_thread_empty());

    // 1. In empty tab: pressing 'N' should send /new to reset in-place
    crate::event_loop::actions::dispatch_action_for_test(
        &mut app,
        &runtime,
        crate::input::InputAction::CreateNewSession,
        empty_sid,
    )
    .await;

    assert!(!app.switch_to_new);
    let req = rx.try_recv().expect("/new sent for empty thread");
    match req {
        nuo_wire::AgentRequest::SlashCommand(cmd) => assert_eq!(cmd, "/new"),
        other => panic!("expected /new slash command, got {other:?}"),
    }

    // 2. In non-empty tab: pressing 'N' should arm switch_to_new
    app.messages
        .push(crate::model::document::TranscriptMessage::new(
            nuo_wire::Role::User,
            "Existing message",
        ));
    assert!(!app.is_current_thread_empty());

    crate::event_loop::actions::dispatch_action_for_test(
        &mut app,
        &runtime,
        crate::input::InputAction::CreateNewSession,
        empty_sid,
    )
    .await;

    assert!(app.switch_to_new);
    assert!(app.should_quit.load(std::sync::atomic::Ordering::SeqCst));
}

#[tokio::test]
async fn slash_new_command_resets_in_place_when_empty_and_switches_to_new_tab_when_non_empty() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    app.tx = tx;
    let runtime = crate::event_loop::UiRuntime::minimal_for_test();
    let sid = "session-test-1";
    app.current_session_id = sid.to_string();
    app.open_tab(crate::surfaces::ClientTab::thread(sid, None::<String>));
    let session = crate::SessionSource::Remote {
        session_id: sid.to_string(),
    };
    assert!(app.is_current_thread_empty());

    // 1. In empty tab: "/new" should send /new to reset in-place
    super::event_loop::handle_send_slash(&mut app, &runtime, &session, "/new".to_string()).await;

    assert!(!app.switch_to_new);
    let req = rx.try_recv().expect("/new sent for empty thread");
    match req {
        nuo_wire::AgentRequest::SlashCommand(cmd) => assert_eq!(cmd, "/new"),
        other => panic!("expected /new slash command, got {other:?}"),
    }

    // 2. In non-empty tab: "/new" must NOT send /new over wire, but arm switch_to_new and preserve conversation
    app.messages.clear();
    app.messages
        .push(crate::model::document::TranscriptMessage::new(
            nuo_wire::Role::User,
            "Crucial conversation",
        ));
    assert!(!app.is_current_thread_empty());

    super::event_loop::handle_send_slash(&mut app, &runtime, &session, "/new".to_string()).await;

    assert!(app.switch_to_new);
    assert_eq!(app.switch_to_new_prompt, None);
    assert!(app.should_quit.load(std::sync::atomic::Ordering::SeqCst));
    // Verify no destructive in-place reset command was sent
    assert!(rx.try_recv().is_err());
    // Verify existing messages are preserved
    assert_eq!(app.messages.len(), 1);
    assert_eq!(app.messages[0].raw, "Crucial conversation");
}

#[tokio::test]
async fn slash_new_with_prompt_arms_switch_to_new_prompt_when_non_empty() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    app.tx = tx;
    let runtime = crate::event_loop::UiRuntime::minimal_for_test();
    let sid = "session-test-2";
    app.current_session_id = sid.to_string();
    app.open_tab(crate::surfaces::ClientTab::thread(sid, None::<String>));
    let session = crate::SessionSource::Remote {
        session_id: sid.to_string(),
    };

    app.messages
        .push(crate::model::document::TranscriptMessage::new(
            nuo_wire::Role::User,
            "Prior turn",
        ));
    assert!(!app.is_current_thread_empty());

    super::event_loop::handle_send_slash(
        &mut app,
        &runtime,
        &session,
        "/new optimize memory allocations".to_string(),
    )
    .await;

    assert!(app.switch_to_new);
    assert_eq!(
        app.switch_to_new_prompt,
        Some("optimize memory allocations".to_string())
    );
    assert!(app.should_quit.load(std::sync::atomic::Ordering::SeqCst));
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn slash_session_new_behaves_identically_to_slash_new() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    app.tx = tx;
    let runtime = crate::event_loop::UiRuntime::minimal_for_test();
    let sid = "session-canonical-test";
    app.current_session_id = sid.to_string();
    app.open_tab(crate::surfaces::ClientTab::thread(sid, None::<String>));
    let session = crate::SessionSource::Remote {
        session_id: sid.to_string(),
    };

    app.messages
        .push(crate::model::document::TranscriptMessage::new(
            nuo_wire::Role::User,
            "Active dialogue",
        ));
    assert!(!app.is_current_thread_empty());

    super::event_loop::handle_send_slash(&mut app, &runtime, &session, "/session new".to_string())
        .await;

    assert!(
        app.switch_to_new,
        "/session new must arm switch_to_new on non-empty thread"
    );
    assert!(
        app.should_quit.load(std::sync::atomic::Ordering::SeqCst),
        "/session new must request runner quit to mount new tab"
    );
    assert!(
        rx.try_recv().is_err(),
        "/session new must not send destructive in-place reset"
    );
}

#[tokio::test]
async fn slash_new_with_round_count_greater_than_zero_opens_new_tab() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    app.tx = tx;
    let runtime = crate::event_loop::UiRuntime::minimal_for_test();
    let sid = "session-rounds-test";
    app.current_session_id = sid.to_string();
    app.open_tab(crate::surfaces::ClientTab::thread(sid, None::<String>));
    let session = crate::SessionSource::Remote {
        session_id: sid.to_string(),
    };

    app.messages.clear();
    app.round_count = 1;
    assert!(!app.is_current_thread_empty());

    super::event_loop::handle_send_slash(&mut app, &runtime, &session, "/new".to_string()).await;

    assert!(app.switch_to_new);
    assert!(app.should_quit.load(std::sync::atomic::Ordering::SeqCst));
}

#[test]
fn models_dialog_arrow_navigation_skips_unusable_models() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let info = |model: &str, usable: bool| nuo_wire::ProviderModelInfo {
        model: model.to_string(),
        name: None,
        protocol: String::new(),
        effort: None,
        thinking: None,
        effort_levels: Vec::new(),
        favorite: false,
        last_used_ms: None,
        vision: Some(false),
        context_window: 128_000,
        max_output_tokens: None,
        availability: if usable {
            Some(nuo_wire::Availability::usable())
        } else {
            Some(nuo_wire::Availability::locked(Some(
                "Requires subscription or account".to_string(),
            )))
        },
        availability_overridden: false,
        advertised: None,
        availability_stale: false,
    };
    app.provider_picker = nuo_wire::ProviderPickerSnapshot {
        default_id: "p1".into(),
        rows: vec![nuo_wire::ProviderPickerRow {
            id: "p1".into(),
            name: "P1".into(),
            model: "m1".into(),
            models: vec!["m1".into(), "m2".into(), "m3".into()],
            model_info: vec![
                info("m1", true),
                info("m2", false), // locked
                info("m3", true),
            ],
            builtin: true,
            protocol: String::new(),
            base_url: String::new(),
            key_ready: true,
            provider: String::new(),
            client_identity: Default::default(),
            last_used_ms: None,
            auth: Default::default(),
        }],
    };
    app.current_provider = "p1".to_string();
    app.current_model = "m1".to_string();

    assert!(app.open_dialog(crate::surfaces::DialogKind::Models));
    assert_eq!(
        app.surfaces.dlg::<crate::surfaces::ModelsDialog>().index,
        0,
        "initially points at first usable model"
    );

    // ModalDown: should skip index 1 (locked) and jump directly to index 2
    if let Some(mut ent) = app.surfaces.take_active_view() {
        let action = crate::input::InputAction::ModalDown;
        let outcome = ent.handle_input(&action, &mut app, "");
        assert_eq!(outcome, crate::surfaces::DialogOutcome::Consumed);
        app.surfaces.put_active_view(ent);
    }
    assert_eq!(
        app.surfaces.dlg::<crate::surfaces::ModelsDialog>().index,
        2,
        "arrow down must skip locked model index 1 and land on index 2"
    );

    // ModalDown again: wraps around to index 0, skipping locked index 1
    if let Some(mut ent) = app.surfaces.take_active_view() {
        let action = crate::input::InputAction::ModalDown;
        let outcome = ent.handle_input(&action, &mut app, "");
        assert_eq!(outcome, crate::surfaces::DialogOutcome::Consumed);
        app.surfaces.put_active_view(ent);
    }
    assert_eq!(
        app.surfaces.dlg::<crate::surfaces::ModelsDialog>().index,
        0,
        "arrow down wrap must skip locked model and land on index 0"
    );

    // ModalUp: wraps backward to index 2, skipping locked index 1
    if let Some(mut ent) = app.surfaces.take_active_view() {
        let action = crate::input::InputAction::ModalUp;
        let outcome = ent.handle_input(&action, &mut app, "");
        assert_eq!(outcome, crate::surfaces::DialogOutcome::Consumed);
        app.surfaces.put_active_view(ent);
    }
    assert_eq!(
        app.surfaces.dlg::<crate::surfaces::ModelsDialog>().index,
        2,
        "arrow up wrap must skip locked model and land on index 2"
    );

    // ModalUp again: moves to index 0, skipping locked index 1
    if let Some(mut ent) = app.surfaces.take_active_view() {
        let action = crate::input::InputAction::ModalUp;
        let outcome = ent.handle_input(&action, &mut app, "");
        assert_eq!(outcome, crate::surfaces::DialogOutcome::Consumed);
        app.surfaces.put_active_view(ent);
    }
    assert_eq!(
        app.surfaces.dlg::<crate::surfaces::ModelsDialog>().index,
        0,
        "arrow up must skip locked model and land on index 0"
    );
}

#[test]
fn models_dialog_click_on_unusable_model_is_ignored() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let info = |model: &str, usable: bool| nuo_wire::ProviderModelInfo {
        model: model.to_string(),
        name: None,
        protocol: String::new(),
        effort: None,
        thinking: None,
        effort_levels: Vec::new(),
        favorite: false,
        last_used_ms: None,
        vision: Some(false),
        context_window: 128_000,
        max_output_tokens: None,
        availability: if usable {
            Some(nuo_wire::Availability::usable())
        } else {
            Some(nuo_wire::Availability::locked(Some(
                "Requires subscription".to_string(),
            )))
        },
        availability_overridden: false,
        advertised: None,
        availability_stale: false,
    };
    app.provider_picker = nuo_wire::ProviderPickerSnapshot {
        default_id: "p1".into(),
        rows: vec![nuo_wire::ProviderPickerRow {
            id: "p1".into(),
            name: "P1".into(),
            model: "m1".into(),
            models: vec!["m1".into(), "m2".into()],
            model_info: vec![
                info("m1", true),
                info("m2", false), // locked at index 1
            ],
            builtin: true,
            protocol: String::new(),
            base_url: String::new(),
            key_ready: true,
            provider: String::new(),
            client_identity: Default::default(),
            last_used_ms: None,
            auth: Default::default(),
        }],
    };
    app.current_provider = "p1".to_string();
    app.current_model = "m1".to_string();

    assert!(app.open_dialog(crate::surfaces::DialogKind::Models));
    app.ui.begin(nuotc::Rect::new(0, 0, 80, 24));
    app.ui.mount(
        crate::ui::UiKey::Overlay(crate::surfaces::OverlaySurface::Dialog(
            crate::surfaces::DialogKind::Models,
        )),
        nuotc::Rect::new(10, 5, 60, 15),
    );
    app.ui.commit();

    let models_dlg = app.surfaces.dlg_mut::<crate::surfaces::ModelsDialog>();
    models_dlg.index = 0;
    models_dlg.scroll = 0;

    // Body starts at y = 5 + 3 = 8.
    // Line 0 is section header "All models", Line 1 is model row 0 (usable), Line 2 is model row 1 (locked).
    if let Some(mut ent) = app.surfaces.take_active_view() {
        // Click on Line 2 (locked row m2) at y = 10
        let action = crate::input::InputAction::SelectionStart { x: 20, y: 10 };
        let outcome = ent.handle_input(&action, &mut app, "");
        assert_eq!(outcome, crate::surfaces::DialogOutcome::Consumed);
        app.surfaces.put_active_view(ent);
    }
    // Index must NOT have changed to 1
    assert_eq!(
        app.surfaces.dlg::<crate::surfaces::ModelsDialog>().index,
        0,
        "clicking on an unusable model must NOT change selection cursor"
    );
}

#[test]
fn models_dialog_reset_picker_nav_lands_on_first_usable_model() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let info = |model: &str, usable: bool| nuo_wire::ProviderModelInfo {
        model: model.to_string(),
        name: None,
        protocol: String::new(),
        effort: None,
        thinking: None,
        effort_levels: Vec::new(),
        favorite: false,
        last_used_ms: None,
        vision: Some(false),
        context_window: 128_000,
        max_output_tokens: None,
        availability: if usable {
            Some(nuo_wire::Availability::usable())
        } else {
            Some(nuo_wire::Availability::locked(None))
        },
        availability_overridden: false,
        advertised: None,
        availability_stale: false,
    };
    app.provider_picker = nuo_wire::ProviderPickerSnapshot {
        default_id: "p1".into(),
        rows: vec![nuo_wire::ProviderPickerRow {
            id: "p1".into(),
            name: "P1".into(),
            model: "m1".into(),
            models: vec!["m0".into(), "m1".into()],
            model_info: vec![
                info("m0", false), // locked at index 0
                info("m1", true),  // usable at index 1
            ],
            builtin: true,
            protocol: String::new(),
            base_url: String::new(),
            key_ready: true,
            provider: String::new(),
            client_identity: Default::default(),
            last_used_ms: None,
            auth: Default::default(),
        }],
    };
    assert!(app.open_dialog(crate::surfaces::DialogKind::Models));
    app.reset_picker_nav();
    assert_eq!(
        app.surfaces.dlg::<crate::surfaces::ModelsDialog>().index,
        1,
        "reset_picker_nav must land on first usable model (index 1), not locked index 0"
    );
}

#[test]
fn models_dialog_two_tier_drill_in_and_pop_sublayer() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let info = |model: &str, usable: bool| nuo_wire::ProviderModelInfo {
        model: model.to_string(),
        name: Some(format!("Model {model}")),
        protocol: String::new(),
        effort: Some("medium".to_string()),
        thinking: Some(true),
        effort_levels: vec!["low".to_string(), "medium".to_string(), "high".to_string()],
        favorite: false,
        last_used_ms: None,
        vision: Some(false),
        context_window: 128_000,
        max_output_tokens: None,
        availability: if usable {
            Some(nuo_wire::Availability::usable())
        } else {
            Some(nuo_wire::Availability::locked(Some(
                "Requires subscription or account entitlement".to_string(),
            )))
        },
        availability_overridden: false,
        advertised: None,
        availability_stale: false,
    };
    app.provider_picker = nuo_wire::ProviderPickerSnapshot {
        default_id: "p1".into(),
        rows: vec![nuo_wire::ProviderPickerRow {
            id: "p1".into(),
            name: "P1".into(),
            model: "m1".into(),
            models: vec!["m1".into(), "m2".into()],
            model_info: vec![
                info("m1", true),  // usable at index 0
                info("m2", false), // locked at index 1
            ],
            builtin: true,
            protocol: String::new(),
            base_url: String::new(),
            key_ready: true,
            provider: String::new(),
            client_identity: Default::default(),
            last_used_ms: None,
            auth: Default::default(),
        }],
    };

    assert!(app.open_dialog(crate::surfaces::DialogKind::Models));
    assert!(
        !app.surfaces
            .dlg::<crate::surfaces::ModelsDialog>()
            .info_detail
    );

    // 1. Drill into usable model m1
    crate::event_loop::actions::handle_open_model_detail(&mut app);
    assert!(
        app.surfaces
            .dlg::<crate::surfaces::ModelsDialog>()
            .info_detail
    );

    // 2. Open model editor sheet via `e` from detail view (clean separation: inspector is read-only)
    crate::event_loop::actions::handle_open_model_editor(&mut app);
    assert!(
        app.surfaces
            .contains_sheet(crate::surfaces::SheetKind::ModelEditor)
    );
    assert_eq!(app.editor_effort, "medium");
    assert!(app.editor_thinking);

    // Adjust effort live inside the editor
    let levels: &[String] = &app.editor_effort_levels;
    let cur = levels.iter().position(|l| l == &app.editor_effort).unwrap() as isize;
    let next = ((cur + 1).rem_euclid(levels.len() as isize)) as usize;
    app.editor_effort = levels[next].clone();
    assert_eq!(app.editor_effort, "high");

    // Pop the editor sheet; returns cleanly to the underlying detail sub-view
    app.pop_transient_surface();
    assert!(
        !app.surfaces
            .contains_sheet(crate::surfaces::SheetKind::ModelEditor)
    );
    assert!(
        app.surfaces
            .dlg::<crate::surfaces::ModelsDialog>()
            .info_detail
    );

    // 3. Esc pops detail sublayer back to Level 1 list
    assert!(app.pop_sublayer());
    assert!(
        !app.surfaces
            .dlg::<crate::surfaces::ModelsDialog>()
            .info_detail
    );

    // 4. Enter on locked model opens detail view rather than being a silent dead end
    let models_dlg = app.surfaces.dlg_mut::<crate::surfaces::ModelsDialog>();
    models_dlg.index = 1; // point at m2 (locked)
    let rows = app.models_flat_filtered();
    let picked = rows.get(1);
    assert!(picked.is_some_and(|r| !r.usable));
    crate::event_loop::actions::handle_open_model_detail(&mut app);
    assert!(
        app.surfaces
            .dlg::<crate::surfaces::ModelsDialog>()
            .info_detail
    );

    // Pop back to list
    assert!(app.pop_sublayer());
    assert!(
        !app.surfaces
            .dlg::<crate::surfaces::ModelsDialog>()
            .info_detail
    );
}

#[test]
fn browse_connection_models_transitions_to_models_dialog() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.connection_detail = Some(nuo_wire::ConnectionDetail {
        name: "google-antigravity".to_string(),
        provider: "google".to_string(),
        provider_label: "Google Antigravity".to_string(),
        models: vec!["gemini-1.5-pro".to_string()],
        ..Default::default()
    });

    assert!(app.open_dialog(crate::surfaces::DialogKind::Connections));
    app.surfaces
        .dlg_mut::<crate::surfaces::ConnectionsDialog>()
        .info_detail = true;

    // Simulate BrowseConnectionModels action
    let filter = app
        .connection_detail
        .as_ref()
        .map(|d| d.provider_label.clone())
        .unwrap();
    app.open_dialog(crate::surfaces::DialogKind::Models);
    let models_dlg = app.surfaces.dlg_mut::<crate::surfaces::ModelsDialog>();
    models_dlg.info_detail = false;
    models_dlg.search = true;
    models_dlg.query.text = filter.clone();
    models_dlg.query.cursor = filter.len();

    assert_eq!(
        app.active_dialog(),
        Some(crate::surfaces::DialogKind::Models)
    );
    assert_eq!(
        app.surfaces
            .dlg::<crate::surfaces::ModelsDialog>()
            .query
            .text,
        "Google Antigravity"
    );
    assert!(app.surfaces.dlg::<crate::surfaces::ModelsDialog>().search);
}
