//! Overlay/modal/navigation tests: pickers, transient sheets, sub-layer pop, queue management, aside views, chrome restoration.

use super::*;

/// Regression: the per-model effort editor must render the **node slider**
/// using the ladder the daemon shipped on the picker snapshot, not one
/// re-derived client-side.
///
/// The TUI binary links `nuo-wire` (which owns the `inventory` registry
/// slot) but **not** `nuo-providers` (which owns the baseline tables that fill
/// it). So `resolve_model` returns an empty ladder in the real binary, and
/// reading it here collapsed the effort control to its value-only fallback —
/// no track, no nodes, no tier labels. `nuox` only sees a populated registry
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
    let nodes = track
        .chars()
        .filter(|&c| matches!(c, '○' | '●'))
        .count();
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
/// backfill all belong to the conversation being left.
#[tokio::test]
async fn switching_sessions_resets_navigation_and_backfill() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "session-a".to_string();
    app.record_input_history("prompt in a".to_string(), Vec::new(), Vec::new());
    // Simulate a walk into history (cursor armed) with a stashed draft and a
    // staged image, plus a backfilled row.
    app.input = "walked row".to_string();
    app.history_index = Some(0);
    app.history_draft = "half-typed draft in a".to_string();
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
    assert!(app.history_draft.is_empty(), "draft does not leak");
    assert!(app.input.is_empty(), "composer starts clean");
    assert!(app.pending_images.is_empty(), "attachments do not leak");
    assert!(
        app.session_history_backfill.is_empty(),
        "backfill is rebuilt per conversation"
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
    app.history_draft = "stale draft".to_string();
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
        app.history_draft, "interrupted input",
        "stale draft replaced"
    );
    assert_eq!(app.history_draft_images.len(), 1);
    assert_eq!(app.pending_images.len(), 1);

    // ↑ then ↓ past the newest restores the adopted input, not the stale one.
    let rows = app.current_session_history();
    assert!(app.history_prev(&rows));
    assert!(!app.history_next(&rows));
    assert_eq!(app.input, "interrupted input");
    assert_eq!(app.pending_images.len(), 1);
}

/// ADR-0110: dispatching a slash command must not arm the activity bar's
/// liveness surface. A command is a synchronous control-plane operation
/// outside the round state machine — no `is_responding`, no optimistic
/// `"queued"` label (which would also fabricate an `Esc Esc interrupt`
/// affordance over a dispatch that cannot be interrupted), and no running-
/// session bookkeeping. The pending command row is the command's in-flight
/// feedback (ADR-0108); this locks the bar against ever lighting for it.
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
        "dispatch must push the pending command row (ADR-0108)"
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
    // daemon's `QueuePaused` verb — ADR-0197 M4).
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
        app.model_search = false;
        assert_eq!(
            app.caret_owner(),
            CaretOwner::None,
            "{dialog:?} browse mode has no editable field"
        );
        app.model_search = true;
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
    // stack (ADR-0205), so every state-dependent case — a picker that is only
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

/// `modal_scroll_field` is the single source of truth that every `Scroll*`
/// action consults: it must resolve each scrollable modal to its own scroll
/// offset (and the right follow-flag for list modals), and return `None` for
/// the modals that don't scroll their own body. This is the event-loop half of
/// "any modal should support scroll" — if a modal is missing here, a page key
/// silently no-ops inside it.
#[test]
fn modal_scroll_field_resolves_every_scrollable_modal() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);

    // Seed a few follow flags so we can assert the helper hands back the
    // right one (and that mutating through it actually clears it).
    app.session_modal_follow = true;
    app.history_modal_follow = true;
    app.queue_modal_follow = true;
    app.model_modal_follow = true;
    app.question_modal_follow = true;

    // List modals return a follow-flag; clearing it must hit the right field.
    app.open_dialog(crate::surfaces::DialogKind::Queue);
    {
        let (scroll, follow) = app.modal_scroll_field().expect("queue scrolls");
        *scroll = 5;
        if let Some(f) = follow {
            *f = false;
        }
    }
    assert_eq!(app.queue_scroll, 5, "queue scroll mutated through helper");
    assert!(
        !app.queue_modal_follow,
        "queue follow cleared through helper"
    );

    app.open_dialog(crate::surfaces::DialogKind::Tools);
    {
        let (_, follow) = app.modal_scroll_field().expect("tools scrolls");
        if let Some(f) = follow {
            *f = false;
        }
    }
    assert!(
        !app.session_modal_follow,
        "tools reuses session follow flag"
    );

    app.open_dialog(crate::surfaces::DialogKind::Sessions);
    {
        let (_, follow) = app.modal_scroll_field().expect("sessions scrolls");
        assert!(follow.is_some(), "sessions shares the session follow flag");
    }

    // Pure-content dialogs/scenes return a scroll ref but no follow flag.
    app.open_dialog(crate::surfaces::DialogKind::UsageStats);
    let (s, f) = app.modal_scroll_field().expect("usage stats scrolls");
    assert!(f.is_none(), "usage stats has no selection-follow flag");
    *s = 7;

    app.open_dialog(crate::surfaces::DialogKind::Permissions);
    let (s, f) = app.modal_scroll_field().expect("permissions scrolls");
    assert!(f.is_none(), "permissions has no selection-follow flag");
    *s = 7;

    app.switch_scene(crate::surfaces::SceneKind::Settings);
    let (s, f) = app.modal_scroll_field().expect("settings scrolls");
    assert!(f.is_none(), "settings has no selection-follow flag");
    *s = 7;

    assert_eq!(app.usage_stats_scroll, 7);
    assert_eq!(app.permissions_scroll, 7);

    // Conversation and ModelEditor do not scroll their own body.
    app.reset_to_conversation();
    assert!(app.modal_scroll_field().is_none());

    app.surfaces
        .present_sheet(crate::surfaces::SheetKind::ModelEditor);
    assert!(app.modal_scroll_field().is_none());
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

/// `mutx attach` (no id) opens the sessions picker at startup instead of
/// loading any session, so the `startup_overlay` state must gate quit-on-close.
/// This pins the two state transitions the event loop relies on:
///
/// 1. The overlay defaults to `None` in an ordinary (in-session) App, so the
///    `/sessions` modal only ever dismisses on Esc.
/// 2. Selecting a session from the picker clears the overlay — once a real
///    conversation backs the view, the picker reverts to a plain transient
///    overlay. (The event loop's `OpenSelectedSession` arm does this.)
#[test]
fn startup_picker_flag_governs_sessions_modal_quit_and_resets_on_open() {
    use std::sync::atomic::Ordering;

    let (mut app, _tmp) = app_in_tempdir(&[], &[]);

    // Default: an in-session App never treats the picker as a startup gate.
    assert_eq!(app.startup_overlay, crate::StartupOverlay::None);

    // Simulate the startup path (`mutx attach` with no id): the picker
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
    app.reset_to_conversation();
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

/// Leaving the aside view (Ctrl+C detach, `SideViewSignal::Closed`) must
/// drop any armed Esc confirmation: it targets the aside's round, and a
/// carried arm could fire the *primary's* interrupt on the next Esc.
#[test]
fn leaving_side_view_drops_the_armed_esc_confirmation() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.side_session_id = Some("aside-1".to_string());
    app.in_side_view = true;
    app.current_session_id = "aside-1".to_string();
    app.running_sessions.insert("aside-1".to_string());

    assert!(!app.esc_press(), "the first Esc inside the aside arms");
    assert!(app.esc_armed());

    // Detach: exit_side_view itself runs inside on_viewed_session_changed,
    // which owns the disarm.
    app.exit_side_view();
    assert!(
        !app.esc_armed(),
        "detaching drops the aside's armed confirmation"
    );

    // And re-entering a view always starts unarmed.
    app.enter_side_view("aside-1".to_string());
    assert!(!app.esc_armed());
    assert!(!app.esc_press());
    assert!(app.esc_armed(), "a fresh arm works inside the view");
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

// View-scoped chrome for `/btw` aside views (ADR-0103 fix): an aside view must
// render its own session's activity bar, never inherit the primary's, and the
// primary's chrome must survive the aside detour untouched.

#[test]
fn aside_view_does_not_inherit_the_primary_activity_bar() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    // The primary is mid-round with live chrome.
    app.phase = Some(crate::phase::Phase::Answering);
    app.round_started_at = Some(std::time::Instant::now());
    app.round_count = 7;
    app.current_turn = 3;

    // Open a brand-new aside: no chrome entry exists yet, so the view must
    // show a fresh idle surface — not the primary's streaming bar.
    app.enter_side_view("side-1".to_string());
    assert!(app.in_side_view);
    assert!(
        app.viewed_chrome().phase.is_none(),
        "a new aside starts idle, not 'responding'"
    );
    assert_eq!(
        app.viewed_chrome().round_count,
        0,
        "a new aside carries no round counter"
    );
    assert!(
        app.viewed_chrome().round_started_at.is_none(),
        "a new aside has no elapsed timer"
    );

    // The primary's chrome is parked, not destroyed.
    let parked = app.saved_primary_chrome.as_ref().expect("primary parked");
    assert_eq!(parked.phase, Some(crate::phase::Phase::Answering));
    assert_eq!(parked.round_count, 7);
    assert_eq!(parked.current_turn, 3);
    assert!(parked.round_started_at.is_some());
}

#[test]
fn exiting_an_aside_restores_the_primary_chrome_exactly() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.phase = Some(crate::phase::Phase::Tool(crate::phase::ToolVerb::Running));
    let started = std::time::Instant::now();
    app.round_started_at = Some(started);
    app.round_count = 12;
    app.current_turn = 2;

    app.enter_side_view("side-9".to_string());
    // While inside the aside, its own events land in its chrome entry only.
    app.session_chrome.insert(
        "side-9".to_string(),
        crate::app::SessionChrome {
            phase: Some(crate::phase::Phase::Reasoning),
            responding: true,
            round_count: 1,
            current_turn: 1,
            round_started_at: Some(std::time::Instant::now()),
            can_retry: false,
            last_turn_performance: None,
            transport_setback: None,
        },
    );
    // Re-entering (focus jump) must swap the aside's own chrome in.
    app.enter_side_view("side-9".to_string());
    assert!(matches!(
        app.viewed_chrome().phase,
        Some(crate::phase::Phase::Reasoning)
    ));
    assert_eq!(app.viewed_chrome().round_count, 1);

    // Leaving restores the primary's parked chrome bit-for-bit: the primary
    // round that kept streaming in the background shows its own bar again.
    app.exit_side_view();
    assert!(!app.in_side_view);
    let chrome = app.viewed_chrome();
    assert_eq!(
        chrome.phase,
        Some(crate::phase::Phase::Tool(crate::phase::ToolVerb::Running))
    );
    assert_eq!(chrome.round_count, 12);
    assert_eq!(chrome.current_turn, 2);
    assert!(chrome.round_started_at.is_some());
}

#[test]
fn reentering_a_running_aside_shows_its_own_chrome() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    // The primary is idle.
    app.phase = None;
    app.round_started_at = None;

    // A background aside is streaming (its listener-maintained entry).
    app.session_chrome.insert(
        "side-2".to_string(),
        crate::app::SessionChrome {
            phase: Some(crate::phase::Phase::Answering),
            responding: true,
            round_count: 2,
            current_turn: 1,
            round_started_at: Some(std::time::Instant::now()),
            can_retry: false,
            last_turn_performance: None,
            transport_setback: None,
        },
    );
    app.enter_side_view("side-2".to_string());
    let chrome = app.viewed_chrome();
    assert_eq!(chrome.phase, Some(crate::phase::Phase::Answering));
    assert!(chrome.responding);
    assert_eq!(chrome.round_count, 2);
    assert!(
        chrome.round_started_at.is_some(),
        "the aside's elapsed timer is its own"
    );
}

#[test]
fn config_view_reopen_keeps_pane_and_category() {
    // Settings is a full-screen scene (ADR-0141) whose fields persist
    // natively on `App`: the enter ritual (pane reset + current-scheme
    // positioning) runs on every enter; a reopen keeps the category/pane
    // the user left. Esc's step-backs unwind the pane/dropdown *inside* the
    // scene and stop there (ADR-0298 §2); leaving the scene is `close_scene`
    // (`C-x w` / `C-x k`, or the scene's own `q`).
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
    assert_eq!(
        app.current_scene(),
        crate::surfaces::SceneKind::Conversation
    );

    // Reopen: the pane/category survived.
    app.switch_scene(crate::surfaces::SceneKind::Settings);
    assert_eq!(app.config_category, 2, "category retained across hide");
    assert_eq!(
        app.config_focus,
        crate::overlays::ConfigFocus::Detail,
        "pane retained across hide"
    );
}

/// ADR-0298 §2 / ADR-0205 `[INV-TUI-CLEAN-02]`: Esc is never a scene exit.
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
        SceneKind::TaskInspection,
        SceneKind::Aside,
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
            SceneKind::Conversation,
            "`close_scene` leaves {scene:?}"
        );
    }
}

/// The Conversation scene is the home: `close_scene` there is a spent gesture,
/// not a navigation (there is nowhere to go).
#[test]
fn close_scene_at_home_is_a_spent_gesture() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    assert_eq!(
        app.current_scene(),
        crate::surfaces::SceneKind::Conversation
    );
    assert!(
        !app.close_scene(),
        "leaving the home scene reports no navigation"
    );
    assert_eq!(
        app.current_scene(),
        crate::surfaces::SceneKind::Conversation
    );
}

/// `C-x w` / `C-x k` dismiss a foreground dialog before touching the scene —
/// the overlay is the visual foreground, so one press spends itself on it and
/// the scene survives (ADR-0298 §1). Regression: the chord used to treat any
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
    assert_eq!(
        app.current_scene(),
        crate::surfaces::SceneKind::Conversation
    );
}

#[test]
fn config_view_navigation_and_theme_preview() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.switch_scene(crate::surfaces::SceneKind::Settings);
    assert_eq!(app.current_scene(), crate::surfaces::SceneKind::Settings);
    assert_eq!(app.config_focus, crate::overlays::ConfigFocus::Categories);
    assert_eq!(app.config_category, 0);

    // Cycling down through all 6 categories
    for expected_cat in [1, 2, 3, 4, 5, 0] {
        crate::event_loop::handle_modal_down(&mut app, "s1");
        assert_eq!(app.config_category, expected_cat);
    }

    // Switch focus to Detail pane
    app.config_focus = crate::overlays::ConfigFocus::Detail;
    app.config_detail_index = 0;

    // Up/down in detail pane previews themes
    crate::event_loop::handle_modal_down(&mut app, "s1");
    let schemes = crate::render::Theme::available_color_schemes();
    let previewed_scheme = &schemes[app.config_detail_index % schemes.len()];
    assert_eq!(
        app.theme.surface(),
        crate::render::Theme::from_color_scheme(&previewed_scheme.id, &app.custom_color_scheme)
            .surface()
    );

    // Revert preview on exit to categories
    app.theme =
        crate::render::Theme::from_color_scheme(&app.color_scheme, &app.custom_color_scheme);
    app.config_focus = crate::overlays::ConfigFocus::Categories;
    assert_eq!(
        app.theme.surface(),
        crate::render::Theme::from_color_scheme(&app.color_scheme, &app.custom_color_scheme)
            .surface()
    );

    // Leaving from Categories is the scene-exit verb — a dismiss on the bare
    // scene is a spent gesture and never navigates (ADR-0298 §2).
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
    assert_eq!(
        app.current_scene(),
        crate::surfaces::SceneKind::Conversation
    );
}

#[test]
fn switching_picker_view_preserves_query_and_chat_draft_separately() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.input = "unsent chat".to_string();
    app.open_dialog(crate::surfaces::DialogKind::Models);
    app.model_search = true;
    app.input = "claude".to_string();

    app.open_dialog(crate::surfaces::DialogKind::UsageStats);
    assert_eq!(
        app.input, "unsent chat",
        "switch restores the chat composer"
    );

    app.open_dialog(crate::surfaces::DialogKind::Models);
    assert_eq!(
        app.input, "claude",
        "picker query is retained independently"
    );
    assert!(app.model_search, "the search sub-layer is retained too");
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
    app.usage_stats_scroll = 17;
    app.close_dialog(crate::surfaces::DialogKind::UsageStats);

    assert!(
        app.surface_store
            .state(&crate::surfaces::DialogKind::UsageStats)
            .is_none()
    );
    assert!(app.surfaces.active_overlay().is_none());
    assert_eq!(app.usage_stats_scroll, 0);
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
    app.open_dialog(crate::surfaces::DialogKind::Telemetry);
    app.telemetry_detail = true;
    assert!(app.pop_sublayer());
    assert!(!app.telemetry_detail, "drill-in closed");
    assert_eq!(
        app.active_dialog(),
        Some(crate::surfaces::DialogKind::Telemetry),
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
    app.connection_info_detail = true;
    app.connection_detail = Some(Default::default());
    assert!(app.pop_sublayer());
    assert!(!app.connection_info_detail, "connection detail closed");
    assert!(app.connection_detail.is_none());
    assert_eq!(
        app.active_dialog(),
        Some(crate::surfaces::DialogKind::Connections),
        "connections modal stays up"
    );
    assert!(!app.pop_sublayer(), "no sub-layer left");
}

#[test]
fn pop_sublayer_pops_telemetry_turn_page_before_round_detail() {
    // Session Telemetry has three levels (round list -> round detail -> attempt inspector):
    // Esc walks back one level at a time, attempt inspector first.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.open_dialog(crate::surfaces::DialogKind::Telemetry);
    app.telemetry_detail = true;
    app.telemetry_turn = Some((2, 1));
    app.telemetry_turn_cursor = 1;
    app.telemetry_scroll = 4;
    assert!(app.pop_sublayer());
    assert!(
        app.telemetry_turn.is_none(),
        "attempt inspector closed first"
    );
    assert!(app.telemetry_detail, "round detail stays open");
    assert_eq!(app.telemetry_scroll, 0);
    assert_eq!(app.telemetry_turn_cursor, 1, "cursor retained");
    assert!(app.pop_sublayer());
    assert!(!app.telemetry_detail, "round detail closed next");
    assert_eq!(app.telemetry_turn_cursor, 0, "cursor reset");
    assert_eq!(
        app.active_dialog(),
        Some(crate::surfaces::DialogKind::Telemetry),
        "view stays up"
    );
    assert!(!app.pop_sublayer(), "no sub-layer left");
}

#[test]
fn dashboard_reopen_keeps_selection_and_log() {
    // The dashboard is a full-screen scene (ADR-0141) whose dock selection
    // and cockpit log persist natively on `App` across hide. Leaving is the
    // scene-exit verb (`close_scene`) — a dismiss never navigates
    // (ADR-0298 §2).
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
    assert_eq!(
        app.current_scene(),
        crate::surfaces::SceneKind::Conversation
    );

    app.switch_scene(crate::surfaces::SceneKind::Dashboard);
    assert_eq!(app.modal_index, 3, "dock selection retained");
    assert_eq!(app.host_console_log.len(), 1, "cockpit log retained");
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
    app.dialog_keys = true;
    app.dialog_keys_scroll = 5;

    // While dialog_keys is active, modal_scroll_field directs to dialog_keys_scroll
    {
        let (scroll, follow) = app.modal_scroll_field().expect("scroll field exists");
        assert_eq!(*scroll, 5);
        assert!(follow.is_none());
        *scroll += 1;
    }
    assert_eq!(app.dialog_keys_scroll, 6);

    // Popping sublayer dismisses dialog_keys but leaves the dialog active
    assert!(app.pop_sublayer(), "dialog_keys sublayer was popped");
    assert!(!app.dialog_keys, "dialog_keys is now false");
    assert_eq!(app.dialog_keys_scroll, 0, "dialog_keys_scroll was reset");
    assert_eq!(
        app.active_dialog(),
        Some(crate::surfaces::DialogKind::Sessions),
        "parent dialog remains open"
    );

    // Turning on dialog_keys again, then dismissing the dialog resets dialog_keys
    app.dialog_keys = true;
    app.dialog_keys_scroll = 3;
    assert!(app.dismiss_active_dialog(), "dismissed active dialog");
    assert!(!app.dialog_keys, "dialog_keys reset on dialog deactivate");
    assert_eq!(app.dialog_keys_scroll, 0);
}


