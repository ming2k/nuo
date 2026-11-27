use super::*;

#[test]
fn empty_state_renders_builtin_without_panicking() {
    let mut terminal = graticule_canvas::TestTerminal::new(80, 24);
    let theme = Theme::default();
    terminal.draw(|f| {
        draw_empty_state(f, f.area(), None, EmptyStateGuidance::Tour, 0, &theme);
    });
}

#[test]
fn empty_state_renders_user_logo_without_panicking() {
    let mut terminal = graticule_canvas::TestTerminal::new(80, 24);
    let theme = Theme::default();
    // A logo at or above the floor so this exercises the logo path, not
    // the drop-to-guidance fallback.
    let logo = vec![
        " X X X X ".to_string(),
        "  X   X  ".to_string(),
        " X X X X ".to_string(),
    ];
    terminal.draw(|f| {
        draw_empty_state(
            f,
            f.area(),
            Some(&logo),
            EmptyStateGuidance::Tour,
            3,
            &theme,
        );
    });
}

#[test]
fn empty_state_renders_needs_provider_guidance_without_panicking() {
    let mut terminal = graticule_canvas::TestTerminal::new(80, 24);
    let theme = Theme::default();
    terminal.draw(|f| {
        draw_empty_state(
            f,
            f.area(),
            None,
            EmptyStateGuidance::NeedsProvider,
            0,
            &theme,
        );
    });
}

fn render_empty_state_to_string_sized(
    guidance: EmptyStateGuidance,
    width: u16,
    height: u16,
) -> String {
    let mut terminal = graticule_canvas::TestTerminal::new(width, height);
    let theme = Theme::default();
    terminal.draw(|f| {
        draw_empty_state(f, f.area(), None, guidance, 0, &theme);
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

fn render_empty_state_to_string(guidance: EmptyStateGuidance) -> String {
    render_empty_state_to_string_sized(guidance, 80, 24)
}

#[test]
fn empty_state_renders_resuming_thread_composed_with_phase() {
    let theme = Theme::default();
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
        let guidance = EmptyStateGuidance::ResumingThread {
            short_id: "7c405d7e".into(),
            phase,
        };
        assert_eq!(
            guidance_line_count(&guidance),
            guidance_section(guidance.clone(), 0, 78, &theme).len(),
        );
        let output = render_empty_state_to_string(guidance);
        assert!(
            output.contains(expected),
            "must render '{expected}':\n{output}"
        );
    }

    let failed = EmptyStateGuidance::ResumingThread {
        short_id: "7c405d7e".into(),
        phase: nuo_protocol::ThreadPhase::Failed {
            code: None,
            message: "snapshot missing".into(),
        },
    };
    assert_eq!(
        guidance_line_count(&failed),
        guidance_section(failed.clone(), 0, 78, &theme).len(),
    );
    let output = render_empty_state_to_string(failed);
    assert!(output.contains("Thread setup failed."), "{output}");
    assert!(output.contains("snapshot missing"), "{output}");
}

#[test]
fn empty_state_renders_honest_server_and_thread_phases_and_matches_line_count() {
    let theme = Theme::default();

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
        let guidance = EmptyStateGuidance::ServerPhase(phase);
        assert_eq!(
            guidance_line_count(&guidance),
            guidance_section(guidance.clone(), 0, 78, &theme).len(),
            "guidance_line_count must stay exact for {guidance:?}"
        );
        let output = render_empty_state_to_string(guidance);
        assert!(
            output.contains(expected),
            "expected '{expected}' in empty state:\n{output}"
        );
    }

    let server_failed = EmptyStateGuidance::ServerPhase(nuo_protocol::ServerPhase::Failed {
        code: Some("storage_init_failed".into()),
        message: "database locked".into(),
    });
    assert_eq!(
        guidance_line_count(&server_failed),
        guidance_section(server_failed.clone(), 0, 78, &theme).len(),
    );
    let output = render_empty_state_to_string(server_failed);
    assert!(output.contains("Nuo server startup failed."), "{output}");
    assert!(output.contains("database locked"), "{output}");

    let connecting = EmptyStateGuidance::Connecting {
        label: "Starting Nuo server…".into(),
    };
    assert_eq!(
        guidance_line_count(&connecting),
        guidance_section(connecting.clone(), 0, 78, &theme).len(),
    );
    assert!(render_empty_state_to_string(connecting).contains("Starting Nuo server…"));

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
        let guidance = EmptyStateGuidance::ThreadPhase(phase);
        assert_eq!(
            guidance_line_count(&guidance),
            guidance_section(guidance.clone(), 0, 78, &theme).len(),
            "guidance_line_count must stay exact for {guidance:?}"
        );
        let output = render_empty_state_to_string(guidance);
        assert!(
            output.contains(expected),
            "expected '{expected}' in empty state:\n{output}"
        );
    }

    let failed = EmptyStateGuidance::ThreadPhase(nuo_protocol::ThreadPhase::Failed {
        code: Some(nuo_protocol::ERR_SESSION_UNRESUMABLE.to_string()),
        message: "corrupt transcript\nsecond line".to_string(),
    });
    assert_eq!(
        guidance_line_count(&failed),
        guidance_section(failed.clone(), 0, 78, &theme).len(),
    );
    let output = render_empty_state_to_string(failed);
    assert!(output.contains("Thread setup failed."), "{output}");
    assert!(output.contains("corrupt transcript"), "{output}");

    let failed_empty = EmptyStateGuidance::ThreadPhase(nuo_protocol::ThreadPhase::Failed {
        code: None,
        message: "   ".to_string(),
    });
    assert_eq!(
        guidance_line_count(&failed_empty),
        guidance_section(failed_empty, 0, 78, &theme).len(),
    );
}

#[test]
fn failed_detail_truncates_with_ellipsis_in_narrow_viewport() {
    let long_msg = "failed to assemble local harness because thread store sqlite file is locked by another process";
    let guidance = EmptyStateGuidance::ThreadPhase(nuo_protocol::ThreadPhase::Failed {
        code: None,
        message: long_msg.to_string(),
    });
    let output = render_empty_state_to_string_sized(guidance, 40, 12);
    assert!(output.contains("Thread setup failed."), "{output}");
    assert!(
        output.contains('…'),
        "long failure detail must truncate with ellipsis rather than silently clipping:\n{output}"
    );
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
    let pages = carousel_pages().len();
    assert!(pages >= 4, "the tour must teach several capabilities");
    // Slide boundaries: within one slide the page holds, then advances.
    assert_eq!(carousel_page_for(0), 0);
    assert_eq!(carousel_page_for(CAROUSEL_SLIDE_SECS as u128 * 1000 - 1), 0);
    assert_eq!(carousel_page_for(CAROUSEL_SLIDE_SECS as u128 * 1000), 1);
    // Full-cycle wrap.
    let cycle = CAROUSEL_SLIDE_SECS as u128 * 1000 * pages as u128;
    assert_eq!(carousel_page_for(cycle), 0);
    assert_eq!(carousel_page_for(cycle + 1), 0);
}

#[test]
fn carousel_pages_fit_the_minimum_terminal_width() {
    // Every page must stay on one centered line even at the narrowest
    // supported terminal, or the copy would wrap and break the
    // wrap-independent height accounting (`guidance_line_count`).
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
    let lines = guidance_section(EmptyStateGuidance::Tour, 1, 78, &theme);
    // Just the page line — no static tagline above it, no indicator row.
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
    // No dot indicator anywhere: the rotation is
    // self-explaining, so no chrome restates it.
    let joined = lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.clone().into_owned())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!joined.contains('●'), "no dot indicator: {joined}");
}

#[test]
fn tour_advertises_the_help_dialog_chords() {
    // ADR-0219: the TabBar's standing `F1 help` keycap is retired, so the
    // tour is the only surface that names the chords before a session has
    // content. Assert both activation chords are advertised and the page
    // stays within the minimum terminal width.
    let theme = Theme::default();
    let pages = carousel_pages();
    let help_page = (0..pages.len())
        .map(|index| {
            guidance_section(EmptyStateGuidance::Tour, index, 78, &theme)
                .iter()
                .flat_map(|line| line.spans.iter())
                .map(|span| span.content.clone().into_owned())
                .collect::<String>()
        })
        .find(|line| line.contains(nuo_ui_primitives::chord::Key::F1.display()))
        .expect("the tour must advertise the help dialog");
    assert!(
        help_page.contains(nuo_ui_primitives::chord::Key::ALT_H.display()),
        "the help page names both activation chords: {help_page}"
    );
    assert!(
        help_page.width() <= nuo_ui_primitives::design::MIN_TERMINAL_COLS as usize,
        "the help page fits the minimum terminal width: {help_page}"
    );
}

#[test]
fn needs_provider_does_not_rotate() {
    assert!(!EmptyStateGuidance::NeedsProvider.is_carousel());
    let theme = Theme::default();
    let lines = guidance_section(EmptyStateGuidance::NeedsProvider, 5, 78, &theme);
    assert_eq!(lines.len(), 2, "blocker + action only: {lines:?}");
}

#[test]
fn parse_logo_truncates_wide_lines() {
    let wide = "a".repeat(200);
    let out = parse_logo(&wide).unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].chars().count(), MAX_LOGO_COLS);
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
fn plan_hero_shows_builtin_when_the_viewport_is_tall_enough() {
    let area = Rect::new(0, 0, 80, 24);
    let logo: Vec<&str> = BUILTIN_LOGO.to_vec();
    let plan = plan_hero(&logo, area, &EmptyStateGuidance::Tour);
    assert!(plan.show_logo, "80×24 fits the built-in wordmark");
    assert_eq!(plan.logo_rows, BUILTIN_LOGO.len());
    assert_eq!(
        plan.total_rows(),
        BUILTIN_LOGO.len() + HERO_LOGO_GAP_ROWS + CAROUSEL_LINES
    );
}

#[test]
fn plan_hero_drops_the_logo_when_the_viewport_is_too_short() {
    // The full hero needs logo + gap + carousel; one row short must drop
    // the art rather than clip it, leaving only the guidance line.
    let logo: Vec<&str> = BUILTIN_LOGO.to_vec();
    let needed = BUILTIN_LOGO.len() + HERO_LOGO_GAP_ROWS + CAROUSEL_LINES;
    let short = Rect::new(0, 0, 80, (needed - 1) as u16);
    let plan = plan_hero(&logo, short, &EmptyStateGuidance::Tour);
    assert!(!plan.show_logo, "too few rows → guidance only");
    assert_eq!(plan.logo_rows, 0);
    assert_eq!(plan.total_rows(), CAROUSEL_LINES);

    let exact = Rect::new(0, 0, 80, needed as u16);
    assert!(
        plan_hero(&logo, exact, &EmptyStateGuidance::Tour).show_logo,
        "exactly enough rows shows the logo"
    );
}

#[test]
fn plan_hero_drops_the_logo_when_the_viewport_is_too_narrow() {
    // One column narrower than the art would wrap (deform) the wordmark.
    let logo: Vec<&str> = BUILTIN_LOGO.to_vec();
    let area = Rect::new(0, 0, (BUILTIN_LOGO[0].len() - 1) as u16, 40);
    let plan = plan_hero(&logo, area, &EmptyStateGuidance::Tour);
    assert!(!plan.show_logo, "narrower than the art → guidance only");
    assert_eq!(plan.total_rows(), CAROUSEL_LINES);
}

#[test]
fn plan_hero_enforces_the_minimum_logo_floor() {
    // A mark below the floor (too narrow or too short) is dropped even on
    // a roomy viewport — a speck is worse than no logo.
    let area = Rect::new(0, 0, 80, 24);
    let too_narrow: Vec<&str> = vec!["x", "x", "x"];
    assert!(!plan_hero(&too_narrow, area, &EmptyStateGuidance::Tour).show_logo);

    let too_short: Vec<&str> = vec!["xxxxxxxx", "xxxxxxxx"];
    assert!(!plan_hero(&too_short, area, &EmptyStateGuidance::Tour).show_logo);

    let just_enough: Vec<&str> = vec!["xxxxxxxx", "xxxxxxxx", "xxxxxxxx"];
    assert!(plan_hero(&just_enough, area, &EmptyStateGuidance::Tour).show_logo);
}

#[test]
fn plan_hero_counts_the_blocker_as_two_rows() {
    let area = Rect::new(0, 0, 80, 24);
    let logo: Vec<&str> = BUILTIN_LOGO.to_vec();
    let tour = plan_hero(&logo, area, &EmptyStateGuidance::Tour);
    let needs = plan_hero(&logo, area, &EmptyStateGuidance::NeedsProvider);
    assert_eq!(
        guidance_line_count(&EmptyStateGuidance::Tour),
        CAROUSEL_LINES
    );
    assert_eq!(guidance_line_count(&EmptyStateGuidance::NeedsProvider), 2);
    assert_eq!(needs.total_rows(), tour.total_rows() + 1);
}
