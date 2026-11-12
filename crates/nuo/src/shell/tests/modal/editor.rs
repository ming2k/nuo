use super::*;

/// Regression: the per-model effort editor must render the **node slider**
/// using the ladder the server shipped on the picker snapshot, not one
/// re-derived client-side.
///
/// The TUI binary links `nuo-protocol` (which owns the `inventory` registry
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
        nuo_protocol::resolve_model(model).effort_levels.is_empty(),
        "precondition: the static registry must not know this id"
    );

    app.provider_picker = ProviderPickerSnapshot {
        default_id: "relay".to_string(),
        rows: vec![nuo_protocol::ProviderPickerRow {
            id: "relay".to_string(),
            name: "Relay".to_string(),
            model: model.to_string(),
            models: vec![model.to_string()],
            model_info: vec![nuo_protocol::ProviderModelInfo {
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
            headers: Default::default(),
            last_used_ms: None,
            auth: Default::default(),
        }],
    };
    app.open_dialog(crate::shell::surfaces::DialogKind::Models);
    app.modal_index = 0;

    // Open the editor the way `e` does on a Models row.
    crate::shell::event_loop::actions::handle_open_model_editor(&mut app);

    assert_eq!(
        app.editor_effort_levels, levels,
        "the snapshot ladder must be captured, not re-resolved"
    );
    // A route with a configured effort opens on exactly that value.
    assert_eq!(
        app.editor_effort, "high",
        "the model's own configured effort passes through unchanged"
    );

    // Now the no-value branch: a row whose route reports no depth at all.
    // The editor opens **empty** and does not invent a tier. Choosing a
    // default is the server's job (it clamps the provider's declared
    // preference onto the provider's declared ladder); the client has neither
    // the baseline tables nor the declared preference, so any value it made up
    // here would be a second implementation of a rule it cannot see — which is
    // exactly how the old `unwrap_or("medium")` + local `clamp_to` opened the
    // editor on a tier the wire would not carry (ADR-0126).
    //
    // (`thinking` is set so the row stays openable — a row opens when it
    // exposes effort *or* thinking. The sheet must be dismissed first: the
    // handler only acts while the Models dialog is the active surface.)
    app.provider_picker.rows[0].model_info[0].effort = None;
    app.provider_picker.rows[0].model_info[0].thinking = Some(true);
    app.dismiss_surface();
    app.open_dialog(crate::shell::surfaces::DialogKind::Models);
    crate::shell::event_loop::actions::handle_open_model_editor(&mut app);
    assert_eq!(
        app.editor_effort, "",
        "no server-resolved depth means an empty editor, never a client-invented tier"
    );

    let mut terminal = nuotc::TestTerminal::new(100, 24);
    terminal.draw(|f| {
        crate::shell::event_loop::render_frame(&mut app, f, "session-a");
    });
    let buf = terminal.buffer();
    let area = buf.area();
    let text: String = (0..area.height)
        .map(|y| {
            (0..area.width)
                .map(|x| buf.get(x, y).unwrap().symbol.as_str().to_string())
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
    use crate::shell::completion::CompletionItemKind;
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
