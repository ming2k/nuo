use super::*;

/// ADR-0024: the dashboard carries the same two-row head band as every
/// other scene — row 1 the uniform session identity, row 2 the scene name
/// (`dashboard`) with the live fleet summary as its context and the
/// namespace pair on the right. This pins the rendered rows end to end,
/// because the layout split is what makes the scene row reachable.
#[test]
fn dashboard_head_renders_the_shared_three_tier_band() {
    let theme = Theme::default();
    let rows = vec![row("a-1", 100, "/work/app", ThreadActivity::Running)];
    let mut terminal = nuotc::TestTerminal::new(100, 24);
    let mut list_scroll = 0usize;
    let mut detail_scroll = 0usize;
    terminal.draw(|f| {
        draw_dashboard(
            f,
            DashboardProps {
                rows: &rows,
                selected: 0,
                focus: DashboardFocus::List,
                list_scroll: &mut list_scroll,
                list_follow: true,
                detail_scroll: &mut detail_scroll,
                log: &[],
                prompting: false,
                prompt_create_new: false,
                prompt_text: "",
                cursor_position: 0,
                current_session_id: "a-1",
                show_caret: false,
                tab_bar: Some(TabBarProps::default()),
                filter: "",
                filtering: false,
                view_usage: false,
                usage_report: None,
                usage_loading: false,
                usage_scroll: 0,
            },
            &theme,
        );
    });

    let buffer = terminal.buffer();
    let row_text = |y: u16| -> String {
        buffer
            .cells()
            .iter()
            .skip(y as usize * 100)
            .take(100)
            .map(|c| c.symbol.as_str())
            .collect()
    };

    // Row 1: the client TabBar / brand and namespace pair (ADR-0060, ADR-0062).
    let tabbar = row_text(0);
    assert!(
        tabbar.contains("help") || tabbar.contains("C-x"),
        "row 1 advertises client affordance: {tabbar:?}"
    );

    // Row 2: the polymorphic SceneBar (ADR-0062).
    let scene = row_text(1);
    assert!(
        scene.contains("dashboard"),
        "scene name on row 2: {scene:?}"
    );
    assert!(
        scene.contains("1 session(s)"),
        "fleet summary context: {scene:?}"
    );
    assert!(scene.contains("1 running"), "running count: {scene:?}");
    assert!(
        !scene.contains("Esc"),
        "Esc is never advertised as a scene exit: {scene:?}"
    );
}

fn row(id: &str, created_at: u64, project_root: &str, status: ThreadActivity) -> MonitoredThread {
    MonitoredThread {
        id: id.into(),
        title: None,
        overview: format!("task for {id}"),
        created_at,
        updated_at: created_at,
        message_count: 3,
        hosting: ThreadHosting::Hosted,
        status,
        round: 1,
        turn: None,
        output_tokens: 120,
        elapsed_ms: 5_000,
        current_tool: None,
        activity: None,
        context_tokens: None,
        note: None,
        project_root: project_root.into(),
        role: "developer".into(),
        parent_id: None,
        origin: nuo_protocol::ThreadOrigin::Trunk,
        digest: None,
    }
}

#[test]
fn sequence_numbers_follow_creation_order_not_snapshot_order() {
    // Snapshot is newest-first by updated_at; the dock must re-sort by
    // creation so #1 is the oldest session no matter how rows arrive.
    let rows = vec![
        row("c-newest", 300, "/work/app", ThreadActivity::Running),
        row("a-oldest", 100, "/work/lib", ThreadActivity::Idle),
        row("b-middle", 200, "/work/web", ThreadActivity::Idle),
    ];
    let entries = dock_entries(&rows);
    // dock_entries preserves input order as its slots but stamps seq by
    // creation: a-oldest must be #1 even though it sits at index 1.
    let seq_of = |id: &str| entries.iter().find(|e| e.row.id == id).unwrap().seq;
    assert_eq!(seq_of("a-oldest"), 1);
    assert_eq!(seq_of("b-middle"), 2);
    assert_eq!(seq_of("c-newest"), 3);
    // And the raw creation_order mapping (used by event handlers) agrees.
    let order = creation_order(&rows);
    assert_eq!(order, vec![1, 2, 0]);
}

#[test]
fn creation_order_is_stable_for_equal_timestamps() {
    let rows = vec![
        row("b", 100, "/p/b", ThreadActivity::Idle),
        row("a", 100, "/p/a", ThreadActivity::Idle),
    ];
    assert_eq!(creation_order(&rows), vec![1, 0]);
    // Repeated calls give the same answer (total order via id tiebreak).
    assert_eq!(creation_order(&rows), creation_order(&rows));
}

#[test]
fn workspace_names_use_basename_until_they_collide() {
    let rows = vec![
        row("one", 1, "/home/ming/projects/nuo", ThreadActivity::Idle),
        row("two", 2, "/home/ming/projects/app", ThreadActivity::Idle),
    ];
    let entries = dock_entries(&rows);
    assert_eq!(entries[0].workspace, "nuo");
    assert_eq!(entries[1].workspace, "app");

    // Same basename in two different parents → both fall back to full
    // paths (a bare "src" would be ambiguous AND meaningless).
    let rows = vec![
        row("one", 1, "/home/ming/projects/nuo", ThreadActivity::Idle),
        row("two", 2, "/tmp/worktree/nuo", ThreadActivity::Idle),
    ];
    let entries = dock_entries(&rows);
    assert_eq!(entries[0].workspace, "/home/ming/projects/nuo");
    assert_eq!(entries[1].workspace, "/tmp/worktree/nuo");
}

#[test]
fn workspace_basename_never_shows_a_parent() {
    assert_eq!(workspace_basename("/home/ming/projects/nuo"), "nuo");
    assert_eq!(workspace_basename("/home/ming/projects/nuo/"), "nuo");
    assert_eq!(workspace_basename("/"), "/");
    assert_eq!(workspace_basename("/src"), "src");
}

#[test]
fn dock_columns_adapt_to_width() {
    assert_eq!(dock_columns(30), 1);
    assert_eq!(dock_columns(36), 1);
    assert_eq!(dock_columns(72), 2);
    assert_eq!(dock_columns(140), 3);
    assert_eq!(dock_columns(400), 4); // capped
}

#[test]
fn dock_card_shows_seq_workspace_uptime_and_status() {
    let theme = Theme::default();
    let now = 1_000_000u64;
    let r = row(
        "x",
        now - (2 * 3600 + 14 * 60),
        "/work/nuo",
        ThreadActivity::Running,
    );
    let entries = dock_entries(std::slice::from_ref(&r));
    let line = dock_card_line(&entries[0], 60, false, false, now, &theme);
    let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(text.contains("#1"), "{text}");
    assert!(text.contains("nuo"), "{text}");
    assert!(text.contains("2h14m"), "{text}");
    assert!(text.contains("running"), "{text}");
    assert!(
        !text.contains('▶'),
        "dock card must not contain '▶': {text}"
    );

    let cur_line = dock_card_line(&entries[0], 60, true, false, now, &theme);
    let cur_text: String = cur_line.spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(
        !cur_text.contains('▶'),
        "current dock card must not contain '▶': {cur_text}"
    );
}

#[test]
fn dock_card_states_lineage_as_a_back_reference() {
    // A card grid has no second axis to nest along (ADR-0120 Alternative E),
    // so lineage is stated positionally: `⇂r2.1#1` reads "forked at r2.1
    // from card #1". A trunk carries no marker — there is exactly one main
    // line and it needs none.
    let theme = Theme::default();
    let now = 1_000u64;

    let mut trunk = row("t", 1, "/work/main", ThreadActivity::Running);
    trunk.origin = nuo_protocol::ThreadOrigin::Trunk;
    let entries = dock_entries(std::slice::from_ref(&trunk));
    let line = dock_card_line(&entries[0], 60, false, false, now, &theme);
    let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(
        !text.contains('\u{21b2}'),
        "a trunk carries no lineage badge: {text}"
    );

    // A subagent thread with its parent present in the dock.
    let trunk_row = row("t", 1, "/work/main", ThreadActivity::Running);
    let mut fork = row("a", 2, "/work/main", ThreadActivity::Running);
    fork.parent_id = Some("t".into());
    fork.origin = nuo_protocol::ThreadOrigin::Subagent;
    let rows = vec![trunk_row, fork];
    let entries = dock_entries(&rows);
    let fork_entry = entries.iter().find(|e| e.row.id == "a").expect("fork card");
    let line = dock_card_line(fork_entry, 60, false, false, now, &theme);
    let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(
        text.contains("\u{21b2}#1"),
        "a child thread names its origin card: {text}"
    );

    // A child whose parent is not in the dock still states the lineage.
    let mut orphan = row("f", 3, "/work/main", ThreadActivity::Idle);
    orphan.parent_id = Some("absent".into());
    orphan.origin = nuo_protocol::ThreadOrigin::Subagent;
    let entries = dock_entries(std::slice::from_ref(&orphan));
    let line = dock_card_line(&entries[0], 60, false, false, now, &theme);
    let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(
        text.contains("\u{21b2}"),
        "an anchorless child still states its lineage: {text}"
    );
    assert!(
        !text.contains("#0"),
        "no card is invented for an absent parent: {text}"
    );
}

#[test]
fn dock_card_truncates_long_workspace_names() {
    let theme = Theme::default();
    let now = 1_000u64;
    // Colliding basenames force the full path onto the card; a long full
    // path must then be ellipsized into the name field.
    let rows = vec![
        row(
            "a",
            1,
            "/very/long/path/to/a/deeply/nested/project",
            ThreadActivity::Idle,
        ),
        row("b", 2, "/other/worktree/project", ThreadActivity::Idle),
    ];
    let entries = dock_entries(&rows);
    let line = dock_card_line(&entries[0], 60, false, false, now, &theme);
    let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(text.contains('…'), "{text}");
    assert!(
        unicode_width::UnicodeWidthStr::width(text.as_str()) <= 60,
        "{text:?}"
    );
    // Idle reads as "done" in the dock vocabulary.
    assert!(text.contains("done"), "{text}");
    // The unique-basename sibling is untouched.
    assert_eq!(entries[1].workspace, "/other/worktree/project");
}

#[test]
fn dock_card_at_minimum_width_keeps_fixed_fields() {
    let theme = Theme::default();
    let now = 1_000u64;
    let r = row("x", now, "/work/nuo", ThreadActivity::Running);
    let entries = dock_entries(std::slice::from_ref(&r));
    // A degenerate 30-cell cell (below the 36 target): seq, uptime and
    // status all survive; the name field just gets cramped.
    let line = dock_card_line(&entries[0], 30, false, false, now, &theme);
    let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(text.contains("#1"), "{text}");
    assert!(text.contains("running"), "{text}");
}

#[test]
fn uptime_format_scales() {
    assert_eq!(format_uptime(43), "43s");
    assert_eq!(format_uptime(12 * 60), "12m");
    assert_eq!(format_uptime(2 * 3600 + 14 * 60), "2h14m");
    assert_eq!(format_uptime(3 * 86_400 + 5 * 3600), "3d5h");
}

#[test]
fn dock_status_vocabulary() {
    assert_eq!(dock_status_label(ThreadActivity::Running), "running");
    assert_eq!(dock_status_label(ThreadActivity::Idle), "done");
    assert_eq!(
        dock_status_label(ThreadActivity::NeedsApproval),
        "needs approval"
    );
    assert_eq!(dock_status_label(ThreadActivity::NeedsInput), "needs input");
    assert_eq!(dock_status_label(ThreadActivity::Failed), "failed");
}

// console grammar

#[test]
fn address_grammar_routes_leading_targets() {
    match parse_console_command("@3 refactor the retry loop") {
        ConsoleCommand::Prompt { targets, text } => {
            assert_eq!(targets, vec![3]);
            assert_eq!(text, "refactor the retry loop");
        }
        other => panic!("expected Prompt, got {other:?}"),
    }
    // Fan-out: every leading @n token is collected, then the payload.
    match parse_console_command("@2 @3 summarize your findings") {
        ConsoleCommand::Prompt { targets, text } => {
            assert_eq!(targets, vec![2, 3]);
            assert_eq!(text, "summarize your findings");
        }
        other => panic!("expected Prompt, got {other:?}"),
    }
}

#[test]
fn bare_text_is_a_prompt_with_no_targets() {
    match parse_console_command("fix the flaky test") {
        ConsoleCommand::Prompt { targets, text } => {
            assert!(targets.is_empty());
            assert_eq!(text, "fix the flaky test");
        }
        other => panic!("expected Prompt, got {other:?}"),
    }
}

#[test]
fn archivist_address_parses_question_mark_and_ask_verb() {
    // `? text` asks the nuo-level retrieval agent.
    match parse_console_command("? where did we debug the retry loop") {
        ConsoleCommand::Archivist { text } => {
            assert_eq!(text, "where did we debug the retry loop");
        }
        other => panic!("expected Archivist, got {other:?}"),
    }
    // `/ask` is the verb alias.
    match parse_console_command("/ask which project was that") {
        ConsoleCommand::Archivist { text } => {
            assert_eq!(text, "which project was that");
        }
        other => panic!("expected Archivist, got {other:?}"),
    }
    // An address with no payload is a usage notice, not an ask.
    assert!(matches!(
        parse_console_command("?"),
        ConsoleCommand::Unrecognized(_)
    ));
    assert!(matches!(
        parse_console_command("/ask"),
        ConsoleCommand::Unrecognized(_)
    ));
    // `?` no longer aliases /help — /help keeps the verb table.
    assert!(matches!(
        parse_console_command("/help"),
        ConsoleCommand::Help
    ));
}

#[test]
fn mid_line_at_is_prose_not_an_address() {
    // The address applies to the whole line: an @ that is not the first
    // token stays in the payload (keeps the grammar
    // regular — no per-sentence retargeting).
    match parse_console_command("ping @alice about the review") {
        ConsoleCommand::Prompt { targets, text } => {
            assert!(targets.is_empty());
            assert_eq!(text, "ping @alice about the review");
        }
        other => panic!("expected Prompt, got {other:?}"),
    }
}

#[test]
fn verbs_parse_with_optional_address() {
    assert_eq!(
        parse_console_command("/kill"),
        ConsoleCommand::Verb {
            verb: ConsoleVerb::Kill,
            target: None
        }
    );
    assert_eq!(
        parse_console_command("/interrupt @3"),
        ConsoleCommand::Verb {
            verb: ConsoleVerb::Interrupt,
            target: Some(3)
        }
    );
    // Aliases and case-insensitivity.
    assert_eq!(
        parse_console_command("/STOP @1"),
        ConsoleCommand::Verb {
            verb: ConsoleVerb::Interrupt,
            target: Some(1)
        }
    );
    assert_eq!(
        parse_console_command("/park @2"),
        ConsoleCommand::Verb {
            verb: ConsoleVerb::Suspend,
            target: Some(2)
        }
    );
}

#[test]
fn verb_with_trailing_text_is_a_usage_error() {
    // A verb takes an optional address and nothing else; silently
    // dropping a tail would hide what the user typed.
    match parse_console_command("/kill @3 now") {
        ConsoleCommand::Unrecognized(msg) => {
            assert!(msg.contains("usage"), "message should explain: {msg}");
        }
        other => panic!("expected Unrecognized, got {other:?}"),
    }
}

#[test]
fn new_and_help_parse() {
    assert_eq!(
        parse_console_command("/new"),
        ConsoleCommand::New { text: None }
    );
    assert_eq!(
        parse_console_command("/new refactor the retry loop"),
        ConsoleCommand::New {
            text: Some("refactor the retry loop".to_string())
        }
    );
    assert_eq!(parse_console_command("/help"), ConsoleCommand::Help);
    assert_eq!(parse_console_command("/?"), ConsoleCommand::Help);
}

#[test]
fn unknown_verb_names_the_command() {
    match parse_console_command("/frobnicate") {
        ConsoleCommand::Unrecognized(msg) => {
            assert!(msg.contains("frobnicate"), "message should name it: {msg}");
            assert!(msg.contains("/help"), "message should point at help: {msg}");
        }
        other => panic!("expected Unrecognized, got {other:?}"),
    }
}

#[test]
fn empty_input_is_never_a_dispatch() {
    assert_eq!(
        parse_console_command("   "),
        ConsoleCommand::Unrecognized(String::new())
    );
}

#[test]
fn session_detail_lines_renders_cognitive_digest_intent_and_milestones() {
    let theme = Theme::default();
    let mut r = row("s-123", 1000, "/work/nuo", ThreadActivity::Running);
    r.digest = Some(nuo_protocol::ThreadDigest {
        title: "Refactor Auth Middleware".to_string(),
        intent: "Rewrite token validator to support ECDSA signatures.".to_string(),
        history: vec![
            "Created test suite for ECDSA".to_string(),
            "Updated Cargo.toml dependencies".to_string(),
        ],
    });
    r.current_tool = Some("execute_command".to_string());

    let lines = session_detail_lines(&r, 80, &theme);
    let rendered: String = lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        rendered.contains("Mission:   Refactor Auth Middleware"),
        "{rendered}"
    );
    assert!(
        rendered.contains("Intent:    Rewrite token validator to support ECDSA signatures."),
        "{rendered}"
    );
    assert!(rendered.contains("Milestones & Progress:"), "{rendered}");
    assert!(
        rendered.contains("• Created test suite for ECDSA"),
        "{rendered}"
    );
    assert!(
        rendered.contains("• Updated Cargo.toml dependencies"),
        "{rendered}"
    );
    assert!(rendered.contains("Telemetry:"), "{rendered}");
    assert!(rendered.contains("tool"), "{rendered}");
    assert!(rendered.contains("execute_command"), "{rendered}");
}

#[test]
fn session_detail_lines_falls_back_to_overview_when_no_digest() {
    let theme = Theme::default();
    let mut r = row("s-456", 1000, "/work/nuo", ThreadActivity::Idle);
    r.overview = "Plain user prompt without digest".to_string();
    r.digest = None;

    let lines = session_detail_lines(&r, 80, &theme);
    let rendered: String = lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");

    assert!(rendered.contains("Overview:"), "{rendered}");
    assert!(
        rendered.contains("Plain user prompt without digest"),
        "{rendered}"
    );
    assert!(!rendered.contains("Milestones & Progress:"), "{rendered}");
}

#[test]
fn responsive_layout_tier_splits_wide_and_compact() {
    assert_eq!(LayoutTier::from_width(120), LayoutTier::Wide);
    assert_eq!(LayoutTier::from_width(90), LayoutTier::Wide);
    assert_eq!(LayoutTier::from_width(89), LayoutTier::Compact);
    assert_eq!(LayoutTier::from_width(60), LayoutTier::Compact);
}

#[test]
fn responsive_layout_tier_adapts_to_elevation_archetype() {
    use nuo_ui_primitives::elevation::ElevationArchetype;
    use nuotc::Rect;
    let rect_90 = Rect::new(0, 0, 90, 24);
    // Under Chromatic (TrueColor), 90 columns has 0 cost -> 90 effective -> Wide
    assert_eq!(
        LayoutTier::from_rect(rect_90, ElevationArchetype::Chromatic),
        LayoutTier::Wide
    );
    // Under Structured (Mono/Linux VT), 2 cols border cost -> 88 effective -> Compact
    assert_eq!(
        LayoutTier::from_rect(rect_90, ElevationArchetype::Structured),
        LayoutTier::Compact
    );
    // Under Structured with 92 columns -> 90 effective -> Wide
    let rect_92 = Rect::new(0, 0, 92, 24);
    assert_eq!(
        LayoutTier::from_rect(rect_92, ElevationArchetype::Structured),
        LayoutTier::Wide
    );
}

#[test]
fn thread_display_title_prioritizes_authentic_title_over_digest_and_overview() {
    let mut r = row("t-1", 100, "/work/test", ThreadActivity::Running);
    r.overview.clear();
    assert_eq!(thread_display_title(&r), "untitled thread");

    r.overview = "first prompt text".to_string();
    assert_eq!(thread_display_title(&r), "first prompt text");

    r.digest = Some(nuo_protocol::ThreadDigest {
        title: "Digest Mission Title".to_string(),
        intent: "fix bug".to_string(),
        history: Vec::new(),
    });
    assert_eq!(thread_display_title(&r), "Digest Mission Title");

    r.title = Some("Authentic AI Thread Title".to_string());
    assert_eq!(thread_display_title(&r), "Authentic AI Thread Title");
}

#[test]
fn fmt_tokens_compact_formats_correctly() {
    assert_eq!(fmt_tokens_compact(0), "0");
    assert_eq!(fmt_tokens_compact(842), "842");
    assert_eq!(fmt_tokens_compact(1_500), "1.5k");
    assert_eq!(fmt_tokens_compact(14_200), "14.2k");
    assert_eq!(fmt_tokens_compact(1_250_000), "1.25M");
}

#[test]
fn status_chip_provides_distinct_visual_vocabulary() {
    let theme = Theme::default();
    let (label_run, _) = status_chip(ThreadActivity::Running, &theme);
    assert!(label_run.contains("RUNNING"));
    let (label_app, _) = status_chip(ThreadActivity::NeedsApproval, &theme);
    assert!(label_app.contains("APPROVAL"));
    let (label_inp, _) = status_chip(ThreadActivity::NeedsInput, &theme);
    assert!(label_inp.contains("INPUT"));
    let (label_fail, _) = status_chip(ThreadActivity::Failed, &theme);
    assert!(label_fail.contains("FAILED"));
    let (label_stop, _) = status_chip(ThreadActivity::Interrupted, &theme);
    assert!(label_stop.contains("STOPPED"));
    let (label_idle, _) = status_chip(ThreadActivity::Idle, &theme);
    assert!(label_idle.contains("IDLE"));
}

#[test]
fn draw_fleet_table_renders_structured_headers_and_thread_titles() {
    let theme = Theme::default();
    let mut r1 = row("a-1", 100, "/work/repo", ThreadActivity::Running);
    r1.title = Some("Refactor Database Pool".to_string());
    r1.current_tool = Some("bash".to_string());
    r1.output_tokens = 14_200;

    let rows = [r1];
    let entries = dock_entries(&rows);
    let mut terminal = nuotc::TestTerminal::new(100, 20);
    let mut scroll = 0usize;
    terminal.draw(|f| {
        draw_fleet_table(
            f,
            Rect::new(0, 0, 100, 20),
            &entries,
            0,
            true,
            &mut scroll,
            true,
            &theme,
            "a-1",
            200,
        );
    });

    let text = terminal
        .buffer()
        .cells()
        .iter()
        .map(|c| c.symbol.as_str())
        .collect::<String>();
    assert!(text.contains("Threads Fleet"), "{text}");
    assert!(text.contains("THREAD / MISSION"), "{text}");
    assert!(text.contains("Refactor Database Pool"), "{text}");
    assert!(text.contains("RUNNING"), "{text}");
    assert!(text.contains("tool: bash"), "{text}");
    assert!(text.contains("14.2k"), "{text}");
}

#[test]
fn draw_model_usage_view_renders_usage_analytics_independently() {
    let theme = Theme::default();
    let mut terminal = nuotc::TestTerminal::new(100, 20);
    terminal.draw(|f| {
        draw_model_usage_view(f, Rect::new(0, 0, 100, 20), None, true, 0, &theme);
    });
    let text = terminal
        .buffer()
        .cells()
        .iter()
        .map(|c| c.symbol.as_str())
        .collect::<String>();
    assert!(text.contains("Model Usage & System Analytics"), "{text}");
    assert!(
        text.contains("Loading cross-session model usage statistics"),
        "{text}"
    );
}
