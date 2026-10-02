//! Tool-step and disclosure rendering tests: sticky steps, diffs, matches, code content, ack detail lines, subagent steps.

use super::*;

/// Render both the compact Subagent step (root view) and the zoomed-in
/// TaskInspection scene with its page header, ensuring no layout panics.
/// Visual verification (run with MUTA_VISUAL=1 --nocapture): a subagent
/// zoom view with two ReAct turns, each emitting a concurrent tool-call
/// batch, groups into turn bands with flush same-turn calls and a blank
/// line between turns — exactly like the main session.
#[test]
fn subagent_view_groups_children_into_turn_bands() {
    let theme = Theme::default();
    let mut terminal = nuotc::TestTerminal::new(80, 30);
    let mut task = TranscriptMessage::tool_step(
        "task_1",
        "spawn_agent",
        r#"{"description":"explore the codebase","prompt":"..."}"#,
    );
    let call =
        |id: &str, name: &str, round: u64, turn: usize| nuo_contracts::SubagentEvent::ToolCall {
            id: id.into(),
            name: name.into(),
            arguments: r#"{"p":"x"}"#.into(),
            round,
            turn,
        };
    let result = |id: &str, name: &str| nuo_contracts::SubagentEvent::ToolResult {
        id: id.into(),
        name: name.into(),
        output: "done".into(),
        duration_ms: 5,
    };
    // Turn 1: a 3-call concurrent batch.
    for (id, name) in [("a", "read_text"), ("b", "search_text"), ("c", "list_dir")] {
        task.push_subagent_event(&call(id, name, 1, 0));
        task.push_subagent_event(&result(id, name));
    }
    // Turn 2: a 2-call concurrent batch.
    for (id, name) in [("d", "websearch"), ("e", "webfetch")] {
        task.push_subagent_event(&call(id, name, 1, 1));
        task.push_subagent_event(&result(id, name));
    }
    let children = task.subagent_children().unwrap().to_vec();
    terminal.draw(|f| {
        let mut layout_map = LayoutMap::new();
        let _ = draw_transcript(
            f,
            &mut layout_map,
            TranscriptProps {
                messages: &children,
                scroll: 0,
                selection: &SelectionState::None,
                cell_selection: None,
                backoff_clause: None,
                activity: "",
                awaiting_permission: false,
                spinner_phase: 0,
                input: "",
                byte_cursor: 0,
                chrome_hidden: false,
                queue_bar: QueueBarProps {
                    items: &[],
                    paused: false,
                    blocked: false,
                    expand_key: Some(crate::keymap::Key::CTRL_Q),
                },
                tasks_bar: Default::default(),
                persistence_health: None,
                subagent_bar: Some(SubagentBarInfo {
                    role: Some("explore".to_string()),
                    label: "the codebase".to_string(),
                    index: 1,
                    total: 1,
                }),
                side_banner: None,
                page_hints: None,
                session_head: None,
                round_started_at: None,
                hovered_step: None,
                focused_target: None,
                logo: None,
                guidance: EmptyStateGuidance::Tour,
                carousel_index: 0,
                theme: &theme,
                layout: crate::layout::Strategy::default(),
                height_cache: None,
            },
        );
    });
    let width = terminal.buffer().area().width as usize;
    let rows: Vec<String> = (0..terminal.buffer().area().height as usize)
        .map(|row| {
            terminal.buffer().content[row * width..(row + 1) * width]
                .iter()
                .map(|cell| cell.symbol())
                .collect()
        })
        .collect();
    if std::env::var("MUTA_VISUAL").is_ok() {
        eprintln!("\n┌─ Subagent zoom (turn-banded) ─");
        for r in &rows {
            eprintln!("│{r}");
        }
        eprintln!("└────\n");
    }
    // Two turn headers appear (turn 1 and turn 2 of the subagent's round 1).
    let body = rows.join("\n");
    assert!(body.contains("turn 1"), "expected a `turn 1` band: {body}");
    assert!(body.contains("turn 2"), "expected a `turn 2` band: {body}");
    // Same-turn sibling calls are flush (no blank row between `read_text`
    // and `search_text` inside turn 1); the two turns are separated by a blank.
    let line_of = |needle: &str| {
        rows.iter()
            .position(|r| r.contains(needle))
            .unwrap_or_else(|| panic!("no row containing {needle}"))
    };
    let t1_first = line_of("Read");
    let t1_second = line_of("Search");
    assert_eq!(t1_second, t1_first + 1, "same-turn calls stay flush");
    // turn 2's header sits at least one blank row after turn 1's batch.
    let t2_header = line_of("turn 2");
    assert!(t2_header > t1_second + 1, "turns are separated");
}

#[test]
fn subagent_step_and_view_render_without_panicking() {
    let theme = Theme::default();
    let mut terminal = nuotc::TestTerminal::new(80, 30);

    // Root view: a completed subagent task renders as a compact step.
    let mut task = TranscriptMessage::tool_step(
        "task_1",
        "spawn_agent",
        r#"{"description":"explore the codebase","prompt":"..."}"#,
    );
    task.push_subagent_event(&nuo_contracts::SubagentEvent::ToolCall {
        id: "inner".into(),
        name: "search_text".into(),
        arguments: r#"{"pattern":"foo"}"#.into(),
        round: 1,
        turn: 0,
    });
    task.finish_tool_step(
        "task_1",
        "found 3 matches",
        nuo_contracts::ToolOutput::text("found 3 matches"),
        1200,
    );
    let root_messages = vec![
        TranscriptMessage::new(nuo_contracts::Role::User, "explore please"),
        task,
    ];

    terminal.draw(|f| {
        let mut layout_map = LayoutMap::new();
        let _ = draw_transcript(
            f,
            &mut layout_map,
            TranscriptProps {
                messages: &root_messages,
                scroll: 0,
                selection: &SelectionState::None,
                cell_selection: None,
                backoff_clause: None,
                activity: "running subagent",
                awaiting_permission: false,
                spinner_phase: 0,
                input: "",
                byte_cursor: 0,
                chrome_hidden: false,
                queue_bar: QueueBarProps {
                    items: &[],
                    paused: false,
                    blocked: false,
                    expand_key: Some(crate::keymap::Key::CTRL_Q),
                },
                tasks_bar: Default::default(),
                persistence_health: None,
                subagent_bar: None,
                side_banner: None,
                page_hints: None,
                session_head: None,
                round_started_at: None,
                hovered_step: None,
                focused_target: None,
                logo: None,
                guidance: EmptyStateGuidance::Tour,
                carousel_index: 0,
                theme: &theme,
                layout: crate::layout::Strategy::default(),
                height_cache: None,
            },
        );
    });

    // Zoomed-in TaskInspection scene: the task's children are the message stream
    // and the contextual header is shown on the first row.
    let children = root_messages[1].subagent_children().unwrap().to_vec();
    terminal.draw(|f| {
        let mut layout_map = LayoutMap::new();
        let _ = draw_transcript(
            f,
            &mut layout_map,
            TranscriptProps {
                messages: &children,
                scroll: 0,
                selection: &SelectionState::None,
                cell_selection: None,
                backoff_clause: None,
                activity: "",
                awaiting_permission: false,
                spinner_phase: 0,
                input: "",
                byte_cursor: 0,
                chrome_hidden: false,
                queue_bar: QueueBarProps {
                    items: &[],
                    paused: false,
                    blocked: false,
                    expand_key: Some(crate::keymap::Key::CTRL_Q),
                },
                tasks_bar: Default::default(),
                persistence_health: None,
                subagent_bar: Some(SubagentBarInfo {
                    role: Some("explore".to_string()),
                    label: "the codebase".to_string(),
                    index: 1,
                    total: 2,
                }),
                side_banner: None,
                page_hints: None,
                session_head: None,
                round_started_at: None,
                hovered_step: None,
                focused_target: None,
                logo: None,
                guidance: EmptyStateGuidance::Tour,
                carousel_index: 0,
                theme: &theme,
                layout: crate::layout::Strategy::default(),
                height_cache: None,
            },
        );
    });

    let width = terminal.buffer().area().width as usize;
    let row_text = |row: usize| -> String {
        terminal.buffer().content[row * width..(row + 1) * width]
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    };
    let head_row = row_text(0);
    // The symbol row is unchanged by the full-width band (the pads and the
    // old inset columns were all spaces); only the background differs —
    // the whole row now paints `body`, asserted in view_header's tests.
    assert_eq!(
        head_row,
        "   SUBAGENT [EXPLORE] the codebase                                      (1/2)   ",
        "Subagent identity, role tag, title and sibling index on the head row"
    );
    // The TaskInspection scene carries no shortcut legend at all (ADR-0205):
    // its three chords are one `Esc` and a pair of remappable sibling walks,
    // which a fixed keycap row cannot advertise faithfully under a remap. The
    // scene is therefore exactly one head row plus the transcript — the last
    // terminal row is transcript, not chrome.
    let last_row = row_text(29);
    assert!(
        !last_row.contains("Esc back") && !last_row.contains("prev") && !last_row.contains("next"),
        "no shortcut legend is pinned to the TaskInspection scene: {last_row:?}"
    );
    assert!(
        !row_text(28).contains("Esc back"),
        "no shortcut legend on the row above either"
    );
}

#[test]
fn height_cache_skip_path_matches_full_layout() {
    // Stage 2 invariant: a warm height cache (which lets the transcript
    // pass *skip* re-wrapping off-screen messages) must produce byte-for-
    // byte the same frame — and the same total `content_lines` — as a cold
    // render that lays every message out in full. If the skip arithmetic
    // (`skip_rows` / `current_y` / `content_lines`) drifted, this fails.
    use crate::model::layout::LayoutMap;
    let theme = Theme::default();

    // A tall transcript: enough wrapped plain-text messages to overflow an
    // 80x24 viewport several times, so both skip branches are exercised —
    // messages scrolled above the viewport (fully_above) and messages below
    // its bottom (fully_below).
    let messages: Vec<TranscriptMessage> = (0..40)
        .map(|i| {
            TranscriptMessage::new(
                nuo_contracts::Role::Assistant,
                format!(
                    "Message number {i} with enough words to wrap across a \
                         couple of lines in an eighty column terminal so the \
                         per-message heights are non-trivial and varied."
                ),
            )
        })
        .collect();
    let (width, height, scroll) = (80u16, 24u16, 30u16);

    let dump = |cache: &mut HeightCache| -> (String, usize) {
        let mut terminal = nuotc::TestTerminal::new(width, height);
        let mut layout_map = LayoutMap::new();
        let mut content_lines = 0usize;
        terminal.draw(|f| {
            let r = draw_transcript(
                f,
                &mut layout_map,
                TranscriptProps {
                    messages: &messages,
                    scroll,
                    selection: &SelectionState::None,
                    cell_selection: None,
                    backoff_clause: None,
                    activity: "",
                    awaiting_permission: false,
                    spinner_phase: 0,
                    input: "",
                    byte_cursor: 0,
                    chrome_hidden: false,
                    queue_bar: QueueBarProps {
                        items: &[],
                        paused: false,
                        blocked: false,
                        expand_key: Some(crate::keymap::Key::CTRL_Q),
                    },
                    tasks_bar: Default::default(),
                    persistence_health: None,
                    subagent_bar: None,
                    side_banner: None,
                    page_hints: None,
                    session_head: None,
                    round_started_at: None,
                    hovered_step: None,
                    focused_target: None,
                    logo: None,
                    guidance: EmptyStateGuidance::Tour,
                    carousel_index: 0,
                    theme: &theme,
                    layout: crate::layout::Strategy::default(),
                    height_cache: Some(cache),
                },
            );
            content_lines = r.content_lines;
        });
        let buf = terminal.buffer();
        let bw = buf.area().width as usize;
        let mut s = String::new();
        for y in 0..height as usize {
            for x in 0..width as usize {
                s.push_str(buf.content[y * bw + x].symbol());
            }
            s.push('\n');
        }
        (s, content_lines)
    };

    let mut cache = HeightCache::default();
    // Cold: cache empty, every message laid out in full (and measured).
    let (cold_grid, cold_lines) = dump(&mut cache);
    // Warm: off-screen messages now take the skip path.
    let (warm_grid, warm_lines) = dump(&mut cache);

    assert_eq!(
        cold_lines, warm_lines,
        "content_lines must match between full and skip layout"
    );
    assert_eq!(
        cold_grid, warm_grid,
        "rendered frame must be identical between full and skip layout"
    );
    // The skip path must actually have been reachable (cache populated).
    assert!(cache.get(messages[0].id).is_some());
}

#[test]
fn expanded_edit_diff_height_is_scroll_independent() {
    // Regression: the expanded edit-diff renderer must account every
    // logical row in `content_lines` even when the viewport clips the
    // body mid-hunk. An early return once the viewport filled made the
    // measured height depend on the scroll offset; the app loop derives
    // `max_scroll` from it, so the scroll position oscillated and the
    // frame flickered during the animation heartbeat.
    let theme = Theme::default();

    // A completed edit whose diff body is several times taller than the
    // viewport, so mid-range scroll offsets clip inside the hunk rows.
    let old: String = (1..=60).map(|i| format!("let v{i} = {i};\n")).collect();
    let new: String = (1..=60)
        .map(|i| format!("let v{i} = {};\n", i * 10))
        .collect();
    let mut m = TranscriptMessage::tool_step(
        "call_test",
        "edit_text",
        r#"{"path":"a.rs","old_string":"…","new_string":"…"}"#,
    );
    let structured = nuo_contracts::ToolOutput::Patch {
        path: "a.rs".into(),
        op: nuo_contracts::PatchOp::Edit,
        old,
        new,
        start_line: 0,
        warnings: Vec::new(),
    };
    m.finish_tool_step("call_test", structured.to_text(), structured, 0);
    if let crate::model::document::MessageKind::ToolStep { expanded, .. } = &mut m.kind {
        *expanded = true;
    }
    let messages = vec![m];

    let (width, height) = (80u16, 24u16);
    let measure = |scroll: u16, cache: &mut HeightCache| -> usize {
        let mut terminal = nuotc::TestTerminal::new(width, height);
        let mut layout_map = LayoutMap::new();
        let mut lines = 0usize;
        terminal.draw(|f| {
            let r = draw_transcript(
                f,
                &mut layout_map,
                TranscriptProps {
                    messages: &messages,
                    scroll,
                    selection: &SelectionState::None,
                    cell_selection: None,
                    backoff_clause: None,
                    activity: "",
                    awaiting_permission: false,
                    spinner_phase: 0,
                    input: "",
                    byte_cursor: 0,
                    chrome_hidden: false,
                    queue_bar: QueueBarProps {
                        items: &[],
                        paused: false,
                        blocked: false,
                        expand_key: Some(crate::keymap::Key::CTRL_Q),
                    },
                    tasks_bar: Default::default(),
                    persistence_health: None,
                    subagent_bar: None,
                    side_banner: None,
                    page_hints: None,
                    session_head: None,
                    round_started_at: None,
                    hovered_step: None,
                    focused_target: None,
                    logo: None,
                    guidance: EmptyStateGuidance::Tour,
                    carousel_index: 0,
                    theme: &theme,
                    layout: crate::layout::Strategy::default(),
                    height_cache: Some(cache),
                },
            );
            lines = r.content_lines;
        });
        lines
    };

    let mut cache = HeightCache::default();
    let at_top = measure(0, &mut cache);
    assert!(
        at_top > height as usize,
        "the diff must overflow the viewport for this test to mean anything"
    );
    // Every offset that clips into the diff body must report the same
    // total height, through both cold and warm height-cache paths.
    for scroll in [1u16, 7, 20, 40, 60] {
        assert_eq!(
            measure(scroll, &mut cache),
            at_top,
            "content_lines must not depend on the scroll offset (scroll = {scroll})"
        );
    }
    let mut fresh_cache = HeightCache::default();
    assert_eq!(
        measure(20, &mut fresh_cache),
        at_top,
        "a cold height cache must measure the same height as a warm one"
    );
}

#[test]
fn completed_diff_cache_survives_height_invalidation_and_resize() {
    let mut cache = HeightCache::default();
    let first = cache.diff_cache.patch(42, "old", "new", 10);

    cache.clear();
    cache.prepare(120);

    let second = cache.diff_cache.patch(42, "old", "new", 10);
    assert!(
        std::sync::Arc::ptr_eq(&first, &second),
        "width-dependent height invalidation must retain semantic diff rows"
    );
}

/// The declarative footer stack must place every row exactly where the
/// old hand-rolled offset arithmetic did. This test keeps the legacy
/// formula as an oracle: with a full chrome (todo + queue + activity +
/// composer + hint all visible) each bar's rect must equal the
/// `status_y + Σ(prior heights)` it replaced, so the refactor is provably
/// behavior-preserving.
#[test]
fn footer_stack_places_rows_where_the_legacy_offsets_did() {
    let theme = Theme::default();
    let messages = vec![TranscriptMessage::new(nuo_contracts::Role::User, "hello")];
    let queue_items = [crate::chrome::QueueItemProps {
        queued_at_ms: 1_700_000_000_000,
        text: "next".into(),
    }];

    let mut terminal = nuotc::TestTerminal::new(80, 30);
    let mut render_opt: Option<TranscriptRender> = None;
    terminal.draw(|f| {
        render_opt = Some(draw_transcript(
            f,
            &mut LayoutMap::new(),
            TranscriptProps {
                messages: &messages,
                scroll: 0,
                selection: &SelectionState::None,
                cell_selection: None,
                backoff_clause: None,
                activity: "responding",
                awaiting_permission: false,
                spinner_phase: 0,
                input: "",
                byte_cursor: 0,
                chrome_hidden: false,
                queue_bar: crate::chrome::QueueBarProps {
                    items: &queue_items,
                    paused: false,
                    blocked: false,
                    expand_key: Some(crate::keymap::Key::CTRL_Q),
                },
                tasks_bar: Default::default(),
                persistence_health: None,
                subagent_bar: None,
                side_banner: None,
                page_hints: None,
                session_head: None,
                round_started_at: None,
                hovered_step: None,
                focused_target: None,
                logo: None,
                guidance: EmptyStateGuidance::Tour,
                carousel_index: 0,
                theme: &theme,
                layout: crate::layout::Strategy::default(),
                height_cache: None,
            },
        ));
    });
    let rendered = render_opt.expect("render result");

    // Legacy oracle, verbatim from the pre-stack code: footer_x/w from the
    // shared inset, status_y after the top gap, then each row's y is the
    // cumulative sum of the rows above it.
    let footer_h = crate::design::FOOTER_TOP_GAP_ROWS
            + crate::design::QUEUE_BAR_ROWS
            + crate::design::ACTIVITY_BAR_ROWS
            + rendered.input_rect.height // composer
            + crate::design::MODEL_BAR_ROWS;
    // The terminal is 30 rows; the head is absent here, so the footer
    // band starts at 30 - footer_h.
    let band_y = 30 - footer_h;
    let footer_x = crate::design::FOOTER_H_INSET;
    let footer_w = 80 - 2 * crate::design::FOOTER_H_INSET;
    let status_y = band_y + crate::design::FOOTER_TOP_GAP_ROWS;

    let expect = |y: u16, h: u16| nuotc::Rect::new(footer_x, y, footer_w, h);
    assert_eq!(
        footer_stack::rect_of(&rendered.footer, FooterRowId::Queue),
        Some(expect(status_y, QUEUE_BAR_ROWS)),
        "queue bar rect"
    );
    assert_eq!(
        footer_stack::rect_of(&rendered.footer, FooterRowId::Activity),
        Some(expect(status_y + QUEUE_BAR_ROWS, ACTIVITY_BAR_ROWS)),
        "activity bar rect"
    );
    assert_eq!(
        Some(rendered.input_rect),
        footer_stack::rect_of(&rendered.footer, FooterRowId::Composer),
        "composer rect appears in the registry exactly as returned"
    );
    assert_eq!(
        rendered.input_rect,
        expect(
            status_y + QUEUE_BAR_ROWS + ACTIVITY_BAR_ROWS,
            rendered.input_rect.height
        ),
        "composer rect matches the legacy offset"
    );
    assert_eq!(
        Some(rendered.hint_rect),
        footer_stack::rect_of(&rendered.footer, FooterRowId::ModelBar),
        "hint bar rect appears in the registry exactly as returned"
    );
    assert_eq!(
        rendered.hint_rect,
        expect(
            status_y + QUEUE_BAR_ROWS + ACTIVITY_BAR_ROWS + rendered.input_rect.height,
            MODEL_BAR_ROWS
        ),
        "hint bar rect matches the legacy offset"
    );
    // The registry contains the four interactive rows (TopGap is 0-height and omitted).
    assert_eq!(rendered.footer.rows.len(), 4, "registry completeness");
    assert_eq!(
        footer_stack::rect_of(&rendered.footer, FooterRowId::TopGap),
        None,
        "the 0-height top gap places no rect"
    );
}

/// The TaskInspection scene renders the head row only — no second row and no
/// pinned legend band, because the scene's three chords (one `Esc` plus a pair
/// of remappable sibling walks) cannot be rendered faithfully by a fixed
/// keycap row (ADR-0205/ADR-0104). Discovery lives in the Command Palette and
/// Help instead.
#[test]
fn subagent_view_omits_row2_entirely() {
    let hints = ViewHints {
        kind: ViewKind::Subagent,
        asides: None,
        breadcrumbs: None,
    };
    assert!(!hints.has_content());
    let terminal = render_full_view(80, 24, &[], Some(hints));
    let row1 = grid_row(&terminal, 1);
    assert!(row1.trim().is_empty(), "row 2 blank on subagent: {row1:?}");
}

/// A checklist/todo tool step rendered while an active selection spans the block
/// must not panic from out-of-bounds byte slicing across the glyph prefix.
#[test]
fn checklist_tool_step_renders_with_active_selection_without_panic() {
    let mut m = TranscriptMessage::tool_step(
        "todo_call",
        "write_todos",
        r#"{"items":[{"content":"Implement feature and verify test suite","status":"completed"}]}"#,
    );
    let output = r#"[{"content":"Implement feature and verify test suite","status":"completed"}]"#;
    m.finish_tool_step(
        "todo_call",
        output.to_string(),
        nuo_contracts::ToolOutput::Text(output.to_string()),
        0,
    );
    if let crate::model::document::MessageKind::ToolStep { expanded, .. } = &mut m.kind {
        *expanded = true;
    }
    let messages = vec![m];
    let theme = Theme::default();

    let render_with_sel = |selection: &SelectionState| {
        let mut terminal = nuotc::TestTerminal::new(80, 24);
        let mut layout_map = LayoutMap::new();
        terminal.draw(|f| {
            let _ = draw_transcript(
                f,
                &mut layout_map,
                TranscriptProps {
                    messages: &messages,
                    scroll: 0,
                    selection,
                    cell_selection: None,
                    backoff_clause: None,
                    activity: "",
                    awaiting_permission: false,
                    spinner_phase: 0,
                    input: "",
                    byte_cursor: 0,
                    chrome_hidden: false,
                    queue_bar: QueueBarProps {
                        items: &[],
                        paused: false,
                        blocked: false,
                        expand_key: Some(crate::keymap::Key::CTRL_Q),
                    },
                    tasks_bar: Default::default(),
                    persistence_health: None,
                    subagent_bar: None,
                    side_banner: None,
                    page_hints: None,
                    session_head: None,
                    round_started_at: None,
                    hovered_step: None,
                    focused_target: None,
                    logo: None,
                    guidance: EmptyStateGuidance::Tour,
                    carousel_index: 0,
                    theme: &theme,
                    layout: crate::layout::Strategy::default(),
                    height_cache: None,
                },
            );
        });
    };

    // Test with Block selection (covers the whole payload block, block_idx 1).
    render_with_sel(&SelectionState::Block {
        message_idx: 0,
        block_idx: 1,
    });

    // Test with Range selection spanning across the block.
    render_with_sel(&SelectionState::Range {
        anchor: crate::model::layout::SemanticCursor::new(0, 1, 0),
        head: crate::model::layout::SemanticCursor::new(0, 1, 100),
    });
}
