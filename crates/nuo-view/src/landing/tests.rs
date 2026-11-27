use super::*;

#[test]
fn landing_renders_builtin_without_panicking() {
    let mut terminal = graticule_canvas::TestTerminal::new(80, 24);
    let theme = Theme::default();
    let keys = CarouselKeys::default();
    terminal.draw(|f| {
        draw_landing_hero(f, f.area(), None, LandingGuidance::Tour, 0, &keys, &theme);
    });
}

#[test]
fn landing_renders_user_logo_without_panicking() {
    let mut terminal = graticule_canvas::TestTerminal::new(80, 24);
    let theme = Theme::default();
    let keys = CarouselKeys::default();
    let logo = vec![
        " X X X X ".to_string(),
        "  X   X  ".to_string(),
        " X X X X ".to_string(),
    ];
    terminal.draw(|f| {
        draw_landing_hero(
            f,
            f.area(),
            Some(&logo),
            LandingGuidance::Tour,
            3,
            &keys,
            &theme,
        );
    });
}

#[test]
fn landing_renders_needs_provider_guidance_without_panicking() {
    let mut terminal = graticule_canvas::TestTerminal::new(80, 24);
    let theme = Theme::default();
    let keys = CarouselKeys::default();
    terminal.draw(|f| {
        draw_landing_hero(
            f,
            f.area(),
            None,
            LandingGuidance::NeedsProvider,
            0,
            &keys,
            &theme,
        );
    });
}

fn render_landing_to_string_sized(guidance: LandingGuidance, width: u16, height: u16) -> String {
    let mut terminal = graticule_canvas::TestTerminal::new(width, height);
    let theme = Theme::default();
    let keys = CarouselKeys::default();
    terminal.draw(|f| {
        draw_landing_hero(f, f.area(), None, guidance, 0, &keys, &theme);
    });
    let buffer = terminal.buffer();
    let (w, h) = buffer.size();
    let mut output = String::new();
    for y in 0..h {
        for x in 0..w {
            output.push_str(buffer.get(x, y).unwrap().symbol.as_str());
        }
        output.push('\n');
    }
    output
}

fn render_landing_to_string(guidance: LandingGuidance) -> String {
    render_landing_to_string_sized(guidance, 80, 24)
}

#[test]
fn landing_renders_resuming_thread_composed_with_phase() {
    let theme = Theme::default();
    let keys = CarouselKeys::default();
    let cases = [
        (
            nuo_protocol::ThreadPhase::Ready,
            "Resuming thread 7c405d7e…",
        ),
        (
            nuo_protocol::ThreadPhase::Preparing {
                stage: nuo_protocol::PrepareStage::Thread,
            },
            "Resuming 7c405d7e · preparing thread store…",
        ),
        (
            nuo_protocol::ThreadPhase::Preparing {
                stage: nuo_protocol::PrepareStage::Hooks,
            },
            "Resuming 7c405d7e · preparing hooks…",
        ),
    ];
    for (phase, expected) in cases {
        let guidance = LandingGuidance::ResumingThread {
            short_id: "7c405d7e".into(),
            phase,
        };
        assert_eq!(
            guidance_line_count(&guidance),
            guidance_section(guidance.clone(), 0, &keys, 78, &theme).len(),
        );
        let output = render_landing_to_string(guidance);
        assert!(
            output.contains(expected),
            "must render '{expected}':\n{output}"
        );
    }

    let failed = LandingGuidance::ResumingThread {
        short_id: "7c405d7e".into(),
        phase: nuo_protocol::ThreadPhase::Failed {
            code: None,
            message: "snapshot missing".into(),
        },
    };
    assert_eq!(
        guidance_line_count(&failed),
        guidance_section(failed.clone(), 0, &keys, 78, &theme).len(),
    );
    let output = render_landing_to_string(failed);
    assert!(output.contains("Thread setup failed."), "{output}");
    assert!(output.contains("snapshot missing"), "{output}");
}

#[test]
fn landing_renders_honest_server_and_thread_phases_and_matches_line_count() {
    let theme = Theme::default();
    let keys = CarouselKeys::default();

    let server_cases = [
        (
            nuo_protocol::ServerPhase::initializing(nuo_protocol::ServerInitStage::Storage),
            "Initializing Nuo server · storage…",
        ),
        (
            nuo_protocol::ServerPhase::initializing_with(
                nuo_protocol::ServerInitStage::Storage,
                Some(nuo_protocol::StorageInitDetail::BackingUp { target_version: 9 }),
                None,
            ),
            "Initializing Nuo server · backing up database before v9…",
        ),
        (
            nuo_protocol::ServerPhase::initializing_with(
                nuo_protocol::ServerInitStage::Storage,
                Some(nuo_protocol::StorageInitDetail::Migrating {
                    step: 2,
                    total: 3,
                    name: "0009_pure_catalog".into(),
                }),
                None,
            ),
            "Initializing Nuo server · migrating database (2/3 · 0009_pure_catalog)…",
        ),
        (
            nuo_protocol::ServerPhase::initializing_with(
                nuo_protocol::ServerInitStage::Storage,
                Some(nuo_protocol::StorageInitDetail::Verifying),
                None,
            ),
            "Initializing Nuo server · verifying database integrity…",
        ),
        (
            nuo_protocol::ServerPhase::initializing(nuo_protocol::ServerInitStage::Runtime),
            "Initializing Nuo server · runtime…",
        ),
        (
            nuo_protocol::ServerPhase::Reloading,
            "Reloading Nuo server…",
        ),
        (
            nuo_protocol::ServerPhase::Draining {
                reason: Some("SIGTERM".into()),
            },
            "Nuo server is shutting down (SIGTERM)…",
        ),
    ];
    for (phase, expected) in server_cases {
        let guidance = LandingGuidance::ServerPhase(phase);
        assert_eq!(
            guidance_line_count(&guidance),
            guidance_section(guidance.clone(), 0, &keys, 78, &theme).len(),
            "guidance_line_count must stay exact for {guidance:?}"
        );
        let output = render_landing_to_string(guidance);
        assert!(
            output.contains(expected),
            "expected '{expected}' in landing hero:\n{output}"
        );
    }

    let server_failed = LandingGuidance::ServerPhase(nuo_protocol::ServerPhase::Failed {
        code: Some("storage_init_failed".into()),
        message: "database locked".into(),
    });
    assert_eq!(
        guidance_line_count(&server_failed),
        guidance_section(server_failed.clone(), 0, &keys, 78, &theme).len(),
    );
    let output = render_landing_to_string(server_failed);
    assert!(output.contains("Nuo server startup failed."), "{output}");
    assert!(output.contains("database locked"), "{output}");

    let connecting = LandingGuidance::Connecting {
        label: "Starting Nuo server…".into(),
    };
    assert_eq!(
        guidance_line_count(&connecting),
        guidance_section(connecting.clone(), 0, &keys, 78, &theme).len(),
    );
    assert!(render_landing_to_string(connecting).contains("Starting Nuo server…"));

    let cases = [
        (
            nuo_protocol::ThreadPhase::Preparing {
                stage: nuo_protocol::PrepareStage::Thread,
            },
            "Preparing thread store…",
        ),
        (
            nuo_protocol::ThreadPhase::Preparing {
                stage: nuo_protocol::PrepareStage::Provider,
            },
            "Preparing provider…",
        ),
        (
            nuo_protocol::ThreadPhase::Preparing {
                stage: nuo_protocol::PrepareStage::Skills,
            },
            "Preparing skills…",
        ),
        (
            nuo_protocol::ThreadPhase::Preparing {
                stage: nuo_protocol::PrepareStage::Hooks,
            },
            "Preparing hooks…",
        ),
    ];

    for (phase, expected) in cases {
        let guidance = LandingGuidance::ThreadPhase(phase);
        assert_eq!(
            guidance_line_count(&guidance),
            guidance_section(guidance.clone(), 0, &keys, 78, &theme).len(),
            "guidance_line_count must stay exact for {guidance:?}"
        );
        let output = render_landing_to_string(guidance);
        assert!(
            output.contains(expected),
            "expected '{expected}' in landing hero:\n{output}"
        );
    }

    let failed = LandingGuidance::ThreadPhase(nuo_protocol::ThreadPhase::Failed {
        code: Some(nuo_protocol::ERR_SESSION_UNRESUMABLE.to_string()),
        message: "corrupt transcript\nsecond line".to_string(),
    });
    assert_eq!(
        guidance_line_count(&failed),
        guidance_section(failed.clone(), 0, &keys, 78, &theme).len(),
    );
    let output = render_landing_to_string(failed);
    assert!(output.contains("Thread setup failed."), "{output}");
    assert!(output.contains("corrupt transcript"), "{output}");

    let failed_empty = LandingGuidance::ThreadPhase(nuo_protocol::ThreadPhase::Failed {
        code: None,
        message: "   ".to_string(),
    });
    assert_eq!(
        guidance_line_count(&failed_empty),
        guidance_section(failed_empty, 0, &keys, 78, &theme).len(),
    );
}

#[test]
fn failed_detail_truncates_with_ellipsis_in_narrow_viewport() {
    let long_msg = "failed to assemble local harness because thread store sqlite file is locked by another process";
    let guidance = LandingGuidance::ThreadPhase(nuo_protocol::ThreadPhase::Failed {
        code: None,
        message: long_msg.to_string(),
    });
    let output = render_landing_to_string_sized(guidance, 40, 12);
    assert!(output.contains("Thread setup failed."), "{output}");
    assert!(
        output.contains('…'),
        "long failure detail must truncate with ellipsis rather than silently clipping:\n{output}"
    );
}

#[test]
fn narrow_viewport_clamps_every_guidance_line_within_budget() {
    let theme = Theme::default();
    let keys = CarouselKeys {
        help: vec!["Ctrl-Alt-Shift-F12".into(), "Ctrl-Alt-Shift-H".into()],
        history_search: Some("Ctrl-Alt-Shift-R".into()),
        command_palette: Some("Ctrl-Alt-Shift-P".into()),
    };
    let max_width = (nuo_ui_primitives::design::MIN_TERMINAL_COLS - 2) as usize;
    let samples = [
        LandingGuidance::ResumingThread {
            short_id: "7c405d7e-long-session-id".into(),
            phase: nuo_protocol::ThreadPhase::Preparing {
                stage: nuo_protocol::PrepareStage::Thread,
            },
        },
        LandingGuidance::ServerPhase(nuo_protocol::ServerPhase::initializing_with(
            nuo_protocol::ServerInitStage::Storage,
            None,
            Some(1),
        )),
        LandingGuidance::Connecting {
            label: "Reconnecting to 127.0.0.1:9800 (2/10, retrying in 5.0s · Esc to cancel)…"
                .into(),
        },
        LandingGuidance::Tour,
    ];
    for guidance in samples {
        for index in 0..carousel_pages_with_keys(&keys).len() {
            let lines = guidance_section(guidance.clone(), index, &keys, max_width, &theme);
            assert_eq!(lines.len(), guidance_line_count(&guidance));
            for line in &lines {
                let width: usize = line.spans.iter().map(|s| s.content.width()).sum();
                assert!(
                    width <= max_width,
                    "guidance line exceeded max_width ({width} > {max_width}): {line:?}"
                );
            }
        }
    }
}

#[test]
fn server_and_thread_phase_status_map_every_phase_honestly() {
    assert_eq!(
        server_phase_status(&nuo_protocol::ServerPhase::initializing(
            nuo_protocol::ServerInitStage::Storage
        ))
        .as_deref(),
        Some("initializing server · storage…")
    );
    assert_eq!(
        server_phase_status_at(
            &nuo_protocol::ServerPhase::initializing_with(
                nuo_protocol::ServerInitStage::Storage,
                Some(nuo_protocol::StorageInitDetail::BackingUp { target_version: 9 }),
                Some(1_000),
            ),
            5_500,
        )
        .as_deref(),
        Some("backing up database before v9… (4s)")
    );
    assert_eq!(
        server_phase_status_at(
            &nuo_protocol::ServerPhase::initializing_with(
                nuo_protocol::ServerInitStage::Storage,
                Some(nuo_protocol::StorageInitDetail::Migrating {
                    step: 1,
                    total: 2,
                    name: "0009_pure_catalog".into(),
                }),
                Some(1_000),
            ),
            2_500,
        )
        .as_deref(),
        Some("migrating database (1/2 · 0009_pure_catalog)…")
    );
    assert_eq!(
        server_phase_status(&nuo_protocol::ServerPhase::initializing_with(
            nuo_protocol::ServerInitStage::Storage,
            Some(nuo_protocol::StorageInitDetail::Verifying),
            None,
        ))
        .as_deref(),
        Some("verifying database integrity…")
    );
    assert_eq!(
        server_phase_status(&nuo_protocol::ServerPhase::initializing(
            nuo_protocol::ServerInitStage::Runtime
        ))
        .as_deref(),
        Some("initializing server · runtime…")
    );
    assert_eq!(server_phase_status(&nuo_protocol::ServerPhase::Ready), None);
    assert_eq!(
        server_phase_status(&nuo_protocol::ServerPhase::Reloading).as_deref(),
        Some("reloading server…")
    );
    assert_eq!(
        server_phase_status(&nuo_protocol::ServerPhase::Draining { reason: None }).as_deref(),
        Some("server shutting down…")
    );
    assert_eq!(
        server_phase_status(&nuo_protocol::ServerPhase::Failed {
            code: None,
            message: "boom".into(),
        })
        .as_deref(),
        Some("server startup failed")
    );

    assert_eq!(
        thread_phase_status(&nuo_protocol::ThreadPhase::Preparing {
            stage: nuo_protocol::PrepareStage::Thread
        }),
        Some("preparing · thread store…")
    );
    assert_eq!(
        thread_phase_status(&nuo_protocol::ThreadPhase::Preparing {
            stage: nuo_protocol::PrepareStage::Provider
        }),
        Some("preparing · provider…")
    );
    assert_eq!(
        thread_phase_status(&nuo_protocol::ThreadPhase::Preparing {
            stage: nuo_protocol::PrepareStage::Skills
        }),
        Some("preparing · skills…")
    );
    assert_eq!(
        thread_phase_status(&nuo_protocol::ThreadPhase::Preparing {
            stage: nuo_protocol::PrepareStage::Hooks
        }),
        Some("preparing · hooks…")
    );
    assert_eq!(
        thread_phase_status(&nuo_protocol::ThreadPhase::Failed {
            code: None,
            message: "boom".into(),
        }),
        Some("thread setup failed")
    );
    assert_eq!(thread_phase_status(&nuo_protocol::ThreadPhase::Ready), None);

    assert_eq!(
        resuming_thread_status("7c405d7e", &nuo_protocol::ThreadPhase::Ready),
        "resuming thread 7c405d7e…"
    );
    assert_eq!(
        resuming_thread_status(
            "7c405d7e",
            &nuo_protocol::ThreadPhase::Preparing {
                stage: nuo_protocol::PrepareStage::Skills
            }
        ),
        "resuming 7c405d7e · preparing skills…"
    );
    assert_eq!(
        resuming_thread_status(
            "7c405d7e",
            &nuo_protocol::ThreadPhase::Failed {
                code: None,
                message: "boom".into(),
            }
        ),
        "thread setup failed"
    );
}

#[test]
fn carousel_rotates_on_a_wall_clock_cadence() {
    let keys = CarouselKeys::default();
    let pages = carousel_pages_with_keys(&keys).len();
    assert!(pages >= 4, "the tour must teach several capabilities");
    assert_eq!(carousel_page_for(0, &keys), 0);
    assert_eq!(
        carousel_page_for(CAROUSEL_SLIDE_SECS as u128 * 1000 - 1, &keys),
        0
    );
    assert_eq!(
        carousel_page_for(CAROUSEL_SLIDE_SECS as u128 * 1000, &keys),
        1
    );
    let cycle = CAROUSEL_SLIDE_SECS as u128 * 1000 * pages as u128;
    assert_eq!(carousel_page_for(cycle, &keys), 0);
    assert_eq!(carousel_page_for(cycle + 1, &keys), 0);
}

#[test]
fn carousel_pages_fit_the_minimum_terminal_width() {
    for page in carousel_pages() {
        assert!(
            page.width() <= nuo_ui_primitives::design::MIN_TERMINAL_COLS as usize,
            "carousel page too wide ({} cols): {:?}",
            page.width(),
            page.lead
        );
    }
}

#[test]
fn tour_carousel_renders_only_the_current_page_line() {
    let theme = Theme::default();
    let keys = CarouselKeys::default();
    let lines = guidance_section(LandingGuidance::Tour, 1, &keys, 78, &theme);
    assert_eq!(lines.len(), CAROUSEL_LINES);
    let page_line: String = lines[0]
        .spans
        .iter()
        .map(|s| s.content.clone().into_owned())
        .collect();
    assert!(
        page_line.contains(carousel_pages()[1].lead),
        "page 1 shows its lead: {page_line}"
    );
    assert!(!page_line.contains('●'), "no dot indicator: {page_line}");
}

#[test]
fn tour_reflects_custom_and_unbound_keybindings() {
    let theme = Theme::default();

    // Default keymap advertises both F1 and Alt-h.
    let defaults = CarouselKeys::default();
    let default_lines: Vec<String> = (0..carousel_pages_with_keys(&defaults).len())
        .map(|i| {
            guidance_section(LandingGuidance::Tour, i, &defaults, 78, &theme)[0]
                .spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect()
        })
        .collect();
    let help_line = default_lines
        .iter()
        .find(|line| line.contains("See every shortcut"))
        .expect("help slide present");
    assert!(help_line.contains(nuo_ui_primitives::chord::Key::F1.display()));
    assert!(help_line.contains(nuo_ui_primitives::chord::Key::ALT_H.display()));

    // Remapped chords appear verbatim; unbound chords are omitted or fall back.
    let custom = CarouselKeys {
        help: vec!["Alt-?".into()],
        history_search: Some("Ctrl-g".into()),
        command_palette: None,
    };
    let custom_lines: Vec<String> = (0..carousel_pages_with_keys(&custom).len())
        .map(|i| {
            guidance_section(LandingGuidance::Tour, i, &custom, 78, &theme)[0]
                .spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect()
        })
        .collect();
    assert!(
        custom_lines
            .iter()
            .any(|l| l == "See every shortcut with Alt-?"),
        "remapped help chord must replace defaults: {custom_lines:?}"
    );
    assert!(
        custom_lines
            .iter()
            .any(|l| l == "Recall past prompts with Ctrl-g"),
        "remapped history chord must be advertised: {custom_lines:?}"
    );
    assert!(
        !custom_lines
            .iter()
            .any(|l| l.contains("Open the command palette")),
        "unbound command palette slide must be omitted: {custom_lines:?}"
    );

    // Unbound help falls back to `/help`.
    let no_help_key = CarouselKeys {
        help: Vec::new(),
        ..CarouselKeys::default()
    };
    let line: String = guidance_section(LandingGuidance::Tour, 1, &no_help_key, 78, &theme)[0]
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect();
    assert_eq!(line, "See every shortcut with /help");
}

#[test]
fn needs_provider_does_not_rotate() {
    assert!(!LandingGuidance::NeedsProvider.is_carousel());
    assert!(!LandingGuidance::QuietWait.is_carousel());
    assert!(
        !LandingGuidance::Connecting {
            label: "Connecting…".into()
        }
        .is_carousel()
    );
    assert!(LandingGuidance::Tour.is_carousel());
    let theme = Theme::default();
    let keys = CarouselKeys::default();
    let lines = guidance_section(LandingGuidance::NeedsProvider, 5, &keys, 78, &theme);
    assert_eq!(lines.len(), 2, "blocker + action only: {lines:?}");
}

#[test]
fn parse_logo_truncates_by_display_width_including_wide_chars() {
    let wide_ascii = "a".repeat(200);
    let out = parse_logo(&wide_ascii).unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].width(), MAX_LOGO_COLS);

    // Each CJK glyph occupies 2 terminal columns: 40 chars = 80 cols > MAX_LOGO_COLS (60).
    let wide_cjk = "诺".repeat(40);
    let out_cjk = parse_logo(&wide_cjk).unwrap();
    assert_eq!(out_cjk.len(), 1);
    assert!(
        out_cjk[0].width() <= MAX_LOGO_COLS,
        "wide CJK logo must clamp to display columns, got {}",
        out_cjk[0].width()
    );
    assert_eq!(out_cjk[0].chars().count(), MAX_LOGO_COLS / 2);
}

#[test]
fn parse_logo_truncates_tall_blocks() {
    let tall = (0..MAX_LOGO_ROWS + 50)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let out = parse_logo(&tall).unwrap();
    assert_eq!(out.len(), MAX_LOGO_ROWS);
}

#[test]
fn parse_logo_strips_crlf_and_trailing_blanks() {
    let raw = "\r\nhello\r\nworld\r\n\r\n";
    let out = parse_logo(raw).unwrap();
    assert_eq!(out, vec!["hello", "world"]);
}

#[test]
fn parse_logo_returns_none_for_empty_input() {
    assert!(parse_logo("").is_none());
    assert!(parse_logo("\n\n\n").is_none());
    assert!(parse_logo("   \n  \n").is_none());
}

#[test]
fn plan_hero_uses_display_width_for_wide_logos() {
    // 10 CJK glyphs = 20 terminal display columns (even though chars().count() == 10).
    let cjk_logo: Vec<&str> = vec!["诺诺诺诺诺诺诺诺诺诺"; 3];
    let too_narrow = Rect::new(0, 0, 19, 10);
    assert!(
        !plan_hero(&cjk_logo, too_narrow, &LandingGuidance::Tour).show_logo,
        "19 cols cannot fit a 20-col CJK logo without wrapping"
    );
    let exact = Rect::new(0, 0, 20, 10);
    assert!(
        plan_hero(&cjk_logo, exact, &LandingGuidance::Tour).show_logo,
        "20 cols fits a 20-col CJK logo"
    );
}

#[test]
fn plan_hero_quiet_wait_adds_no_phantom_gap_row() {
    let logo: Vec<&str> = BUILTIN_LOGO.to_vec();
    let exact = Rect::new(0, 0, 80, BUILTIN_LOGO.len() as u16);
    let plan = plan_hero(&logo, exact, &LandingGuidance::QuietWait);
    assert!(
        plan.show_logo,
        "QuietWait needs only logo_rows height, no phantom gap row"
    );
    assert_eq!(plan.total_rows(), BUILTIN_LOGO.len());

    // Actual rendered row count must match plan.total_rows() exactly.
    let mut terminal = graticule_canvas::TestTerminal::new(80, BUILTIN_LOGO.len() as u16);
    let theme = Theme::default();
    let keys = CarouselKeys::default();
    let mut painted = 0;
    terminal.draw(|f| {
        painted = draw_landing_hero(
            f,
            f.area(),
            None,
            LandingGuidance::QuietWait,
            0,
            &keys,
            &theme,
        );
    });
    assert_eq!(painted, BUILTIN_LOGO.len());
}

#[test]
fn plan_hero_shows_builtin_when_the_viewport_is_tall_enough() {
    let area = Rect::new(0, 0, 80, 24);
    let logo: Vec<&str> = BUILTIN_LOGO.to_vec();
    let plan = plan_hero(&logo, area, &LandingGuidance::Tour);
    assert!(plan.show_logo, "80×24 fits the built-in wordmark");
    assert_eq!(plan.logo_rows, BUILTIN_LOGO.len());
    assert_eq!(
        plan.total_rows(),
        BUILTIN_LOGO.len() + HERO_LOGO_GAP_ROWS + CAROUSEL_LINES
    );
}

#[test]
fn plan_hero_drops_the_logo_when_the_viewport_is_too_short() {
    let logo: Vec<&str> = BUILTIN_LOGO.to_vec();
    let needed = BUILTIN_LOGO.len() + HERO_LOGO_GAP_ROWS + CAROUSEL_LINES;
    let short = Rect::new(0, 0, 80, (needed - 1) as u16);
    let plan = plan_hero(&logo, short, &LandingGuidance::Tour);
    assert!(!plan.show_logo, "too few rows → guidance only");
    assert_eq!(plan.logo_rows, 0);
    assert_eq!(plan.total_rows(), CAROUSEL_LINES);

    let exact = Rect::new(0, 0, 80, needed as u16);
    assert!(
        plan_hero(&logo, exact, &LandingGuidance::Tour).show_logo,
        "exactly enough rows shows the logo"
    );
}

#[test]
fn plan_hero_drops_the_logo_when_the_viewport_is_too_narrow() {
    let logo: Vec<&str> = BUILTIN_LOGO.to_vec();
    let area = Rect::new(0, 0, (BUILTIN_LOGO[0].len() - 1) as u16, 40);
    let plan = plan_hero(&logo, area, &LandingGuidance::Tour);
    assert!(!plan.show_logo, "narrower than the art → guidance only");
    assert_eq!(plan.total_rows(), CAROUSEL_LINES);
}

#[test]
fn plan_hero_enforces_the_minimum_logo_floor() {
    let area = Rect::new(0, 0, 80, 24);
    let too_narrow: Vec<&str> = vec!["x", "x", "x"];
    assert!(!plan_hero(&too_narrow, area, &LandingGuidance::Tour).show_logo);

    let too_short: Vec<&str> = vec!["xxxxxxxx", "xxxxxxxx"];
    assert!(!plan_hero(&too_short, area, &LandingGuidance::Tour).show_logo);

    let just_enough: Vec<&str> = vec!["xxxxxxxx", "xxxxxxxx", "xxxxxxxx"];
    assert!(plan_hero(&just_enough, area, &LandingGuidance::Tour).show_logo);
}

#[test]
fn plan_hero_counts_the_blocker_as_two_rows() {
    let area = Rect::new(0, 0, 80, 24);
    let logo: Vec<&str> = BUILTIN_LOGO.to_vec();
    let tour = plan_hero(&logo, area, &LandingGuidance::Tour);
    let needs = plan_hero(&logo, area, &LandingGuidance::NeedsProvider);
    assert_eq!(guidance_line_count(&LandingGuidance::Tour), CAROUSEL_LINES);
    assert_eq!(guidance_line_count(&LandingGuidance::NeedsProvider), 2);
    assert_eq!(needs.total_rows(), tour.total_rows() + 1);
}
