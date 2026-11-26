//! Single-row Thread SceneBar tests: governance, title lifecycle, fair-share
//! slack redistribution, path ladder, and width invariants (ADR-0261).

use super::*;

#[test]
fn thread_bar_untitled_reads_role_posture_id_and_workspace() {
    let context = ThreadContext {
        workspace: "~/projects/xx",
        role: Some("developer"),
        unattended: true,
        confined: false,
    };
    let row = render_thread_row(80, Some("sess-01a2b3c4"), None, context);
    assert!(
        row.starts_with("  [DEVELOPER]  auto  open  thread b3c4"),
        "{row}"
    );
    assert!(row.trim_end().ends_with("~/projects/xx"), "{row}");
}

#[test]
fn thread_bar_title_replaces_thread_id_in_place_without_shifting_governance() {
    let context = ThreadContext {
        workspace: "~/projects/nuo",
        role: Some("developer"),
        unattended: true,
        confined: false,
    };
    let untitled = render_thread_row(80, Some("sess-01a2b3c4"), None, context);
    let titled = render_thread_row(80, Some("sess-01a2b3c4"), Some("Fix retry loop"), context);

    // Governance cluster stays at identical columns across the title lifecycle.
    assert_eq!(
        untitled.find("[DEVELOPER]"),
        titled.find("[DEVELOPER]"),
        "role position must not move:\nuntitled: {untitled}\ntitled:   {titled}"
    );
    assert_eq!(
        untitled.find("auto  open"),
        titled.find("auto  open"),
        "posture position must not move:\nuntitled: {untitled}\ntitled:   {titled}"
    );
    // Title takes the exact starting column of `thread b3c4`.
    assert_eq!(
        untitled.find("thread b3c4"),
        titled.find("Fix retry loop"),
        "title must replace `thread <id_tail>` strictly in place:\nuntitled: {untitled}\ntitled:   {titled}"
    );
    assert!(
        !titled.contains("thread b3c4") && !titled.contains("b3c4"),
        "titled bar omits redundant thread id: {titled}"
    );
    // Right-anchored workspace stays pinned at identical columns.
    assert_eq!(
        untitled.find("~/projects/nuo"),
        titled.find("~/projects/nuo"),
        "workspace must stay right-anchored:\nuntitled: {untitled}\ntitled:   {titled}"
    );
}

#[test]
fn thread_bar_reports_only_deviations_from_the_safe_default() {
    let context = ThreadContext {
        workspace: "~/projects/xx",
        role: Some("developer"),
        unattended: false,
        confined: true,
    };
    let row = render_thread_row(80, Some("sess-01a2b3c4"), None, context);
    assert!(
        row.starts_with("  [DEVELOPER]  thread b3c4"),
        "no posture tokens in safe default: {row}"
    );
    assert!(!row.contains("auto"), "{row}");
    assert!(!row.contains("open"), "{row}");
}

#[test]
fn thread_bar_posture_tokens_are_lowercase_and_unbracketed() {
    assert_eq!(PostureToken::Auto.label(), "auto");
    assert_eq!(PostureToken::Open.label(), "open");
    for label in [PostureToken::Auto.label(), PostureToken::Open.label()] {
        assert!(
            !label.chars().any(char::is_uppercase) && !label.contains(['[', ']']),
            "{label}"
        );
    }
}

#[test]
fn thread_bar_posture_tokens_join_with_pure_whitespace_no_punctuation_soup() {
    let context = ThreadContext {
        workspace: "",
        role: Some("developer"),
        unattended: true,
        confined: false,
    };
    let row = render_thread_row(80, Some("sess-01a2b3c4"), Some("Audit auth"), context);
    assert!(
        row.contains("[DEVELOPER]  auto  open  Audit auth"),
        "peer tokens use a plain two-column gap, no glyph: {row}"
    );
    assert!(!row.contains('·') && !row.contains('|'), "{row}");
}

#[test]
fn thread_bar_workspace_free_hides_workspace_and_keeps_role_and_title() {
    let context = ThreadContext {
        workspace: "",
        role: Some("philosophist"),
        unattended: false,
        confined: true,
    };
    let row = render_thread_row(80, Some("sess-01a2b3c4"), Some("Discuss ethics"), context);
    assert!(row.starts_with("  [PHILOSOPHIST]  Discuss ethics"), "{row}");
    assert!(!row.contains("~/"), "{row}");
}

#[test]
fn thread_bar_shows_custom_user_role_badge() {
    let context = ThreadContext {
        workspace: "~/projects/xx",
        role: Some("security-auditor"),
        unattended: false,
        confined: true,
    };
    let row = render_thread_row(80, Some("sess-01a2b3c4"), None, context);
    assert!(
        row.starts_with("  [SECURITY-AUDITOR]  thread b3c4"),
        "{row}"
    );
}

/// `[INV-HEAD-09]` anchor stability: flipping a posture token in relaxed widths
/// must not reflow the right-anchored workspace path.
#[test]
fn thread_bar_toggling_posture_never_reflows_the_workspace_path() {
    let quiet = ThreadContext {
        workspace: "~/projects/nuo",
        role: Some("developer"),
        unattended: false,
        confined: true,
    };
    let loud = ThreadContext {
        unattended: true,
        confined: false,
        ..quiet
    };
    let quiet_row = render_thread_row(80, Some("sess-01a2b3c4"), Some("Fix auth"), quiet);
    let loud_row = render_thread_row(80, Some("sess-01a2b3c4"), Some("Fix auth"), loud);
    for row in [&quiet_row, &loud_row] {
        assert!(row.trim_end().ends_with("~/projects/nuo"), "{row}");
    }
    assert_eq!(
        quiet_row.find("~/projects/nuo"),
        loud_row.find("~/projects/nuo"),
        "quiet: {quiet_row}\nloud:  {loud_row}"
    );
}

/// `[INV-HEAD-10]` fair-share with slack redistribution: a long title and a
/// deep path coexist smoothly on typical widths without either starving the
/// other, and ancestors elide before the project leaf.
#[test]
fn thread_bar_fair_share_preserves_both_title_and_workspace_leaf() {
    let context = ThreadContext {
        workspace: "~/projects/company/team/services/nuo-tui",
        role: Some("developer"),
        unattended: true,
        confined: false,
    };
    let row = render_thread_row(
        80,
        Some("sess-01a2b3c4"),
        Some("Refactor database migration layer for multi-tenant isolation"),
        context,
    );
    assert!(
        row.starts_with("  [DEVELOPER]  auto  open  Refactor"),
        "{row}"
    );
    assert!(
        row.contains("nuo-tui"),
        "workspace leaf must survive: {row}"
    );
    assert!(
        !row.contains("~/projects/company/team"),
        "workspace ancestors elide under pressure: {row}"
    );
    assert!(row.contains('…'), "long title elides cleanly: {row}");
}

/// `[INV-HEAD-10]` slack redistribution: a short identity yields its unused
/// share back to a deep workspace path.
#[test]
fn thread_bar_short_identity_flows_slack_to_workspace() {
    let context = ThreadContext {
        workspace: "~/projects/nuo/crates/nuo-tui",
        role: Some("developer"),
        unattended: true,
        confined: false,
    };
    // At 50 columns (46 text): gov=23, gap=2, shared=21, usable=19.
    // `thread b3c4` uses 11 < id_share(13), so 8 cols go to workspace -> `nuo-tui`.
    let row = render_thread_row(50, Some("sess-01a2b3c4"), None, context);
    assert!(
        row.contains("[DEVELOPER]  auto  open  thread b3c4"),
        "{row}"
    );
    assert!(row.contains("nuo-tui"), "leaf survives from slack: {row}");
    assert!(
        !row.contains("~/projects/nuo/crates"),
        "ancestors elided: {row}"
    );
}

/// `[INV-HEAD-10]` extreme pressure: below the combined floor, only one
/// optional element fits honestly; because Tier 1 TabBar already projects the
/// tab title, the bar keeps the workspace leaf and omits the title.
#[test]
fn thread_bar_extreme_pressure_prefers_workspace_over_title() {
    let context = ThreadContext {
        workspace: "~/projects/nuo",
        role: Some("developer"),
        unattended: true,
        confined: false,
    };
    // Width 38 -> 34 text cols. gov=23, gap=2 -> shared=9, usable=7 < 4+6=10.
    // Only workspace can fit honestly -> `nuo` (or `~/p/nuo`) survives, title omitted.
    let row = render_thread_row(
        38,
        Some("sess-01a2b3c4"),
        Some("Long conversation title"),
        context,
    );
    assert!(row.starts_with("  [DEVELOPER]  auto  open"), "{row}");
    assert!(row.contains("nuo"), "workspace leaf survives: {row}");
    assert!(
        !row.contains("Long"),
        "title yields below combined floor: {row}"
    );
}

/// The row must never paint past its own width across the full width sweep.
#[test]
fn thread_bar_never_paints_beyond_the_row_width() {
    let context = ThreadContext {
        workspace: "~/projects/nuo",
        role: Some("developer"),
        unattended: true,
        confined: false,
    };
    for title in [None, Some("Fix retry logic in auth loop")] {
        for width in 8u16..=120 {
            let row = render_thread_row(width, Some("sess-01a2b3c4"), title, context);
            assert_eq!(
                row.chars().count(),
                width as usize,
                "row must fill exactly {width} columns (title={title:?}): {row:?}"
            );
        }
    }
}

/// Below the honesty floor both identity and workspace are dropped wholesale:
/// posture and role keep the row.
#[test]
fn thread_bar_drops_optional_elements_rather_than_lying() {
    let context = ThreadContext {
        workspace: "~/projects/nuo/crates/nuo-tui",
        role: Some("developer"),
        unattended: true,
        confined: false,
    };
    // 30 columns -> 26 text columns. Governance cluster is 23 wide, leaving
    // 1 column after the 2-col gap — below both floors, so both drop cleanly.
    let row = render_thread_row(30, Some("sess-01a2b3c4"), Some("Fix auth"), context);
    assert!(row.contains("[DEVELOPER]  auto  open"), "{row}");
    assert!(!row.contains('~'), "no fake path: {row}");
    assert!(!row.contains('…'), "no fake fragment: {row}");
}

/// Posture is safety state, so it outranks every other element on the row
/// (`[INV-HEAD-10]`): a role badge long enough to crowd it must yield.
#[test]
fn thread_bar_posture_outranks_a_long_role_badge() {
    let context = ThreadContext {
        workspace: "~/projects/nuo",
        role: Some("a-very-long-custom-user-role-name"),
        unattended: true,
        confined: false,
    };
    let row = render_thread_row(44, Some("sess-01a2b3c4"), Some("Fix auth"), context);
    assert!(row.contains("auto  open"), "posture intact: {row}");
    assert!(row.contains('…'), "role badge elided instead: {row}");
    assert!(
        !row.contains("A-VERY-LONG-CUSTOM-USER-ROLE-NAME"),
        "full role must yield to safety state: {row}"
    );
}

/// A posture token is NEVER emitted as a clipped prefix (`[INV-HEAD-10]`):
/// neither `auto` nor `open` may ever appear truncated (`aut`, `ope`, `op`).
#[test]
fn thread_bar_never_emits_a_truncated_posture_token() {
    for width in 8u16..=80 {
        let context = ThreadContext {
            workspace: "~/projects/nuo",
            role: Some("security-auditor"),
            unattended: true,
            confined: false,
        };
        let row = render_thread_row(width, Some("sess-01a2b3c4"), Some("Fix auth"), context);
        let words: Vec<&str> = row.split_whitespace().collect();
        for word in &words {
            assert!(
                !matches!(*word, "a" | "au" | "aut" | "o" | "op" | "ope"),
                "width {width}: posture token clipped to {word:?} in {row:?}"
            );
        }
    }
}

/// A non-Unicode terminal must never receive `…` from badge, title, or path
/// truncation; all three take their marker from `theme.glyphs.ellipsis`.
#[test]
fn thread_bar_degrades_all_ellipsis_markers_under_ascii_glyphs() {
    let theme = Theme {
        glyphs: graticule_canvas::ASCII_GLYPHS,
        ..Theme::default()
    };

    // 1. Elided role badge
    let badge_ctx = ThreadContext {
        workspace: "~/projects/nuo/crates/nuo-tui",
        role: Some("security-auditor"),
        unattended: true,
        confined: false,
    };
    let badge_row =
        render_thread_row_with_theme(30, Some("sess-01a2b3c4"), None, badge_ctx, &theme);
    assert!(!badge_row.contains('…'), "{badge_row:?}");
    assert!(badge_row.contains("..."), "{badge_row:?}");
    assert!(badge_row.contains("auto  open"), "{badge_row:?}");

    // 2. Elided title
    let title_ctx = ThreadContext {
        workspace: "~/projects/nuo",
        role: Some("developer"),
        unattended: false,
        confined: true,
    };
    let title_row = render_thread_row_with_theme(
        52,
        Some("sess-01a2b3c4"),
        Some("A very long authentic conversation title that overflows"),
        title_ctx,
        &theme,
    );
    assert!(!title_row.contains('…'), "{title_row:?}");
    assert!(title_row.contains("..."), "{title_row:?}");

    // 3. Elided single-segment path ladder
    let path_ctx = ThreadContext {
        workspace: "a-really-long-single-directory-name-with-no-nesting",
        role: None,
        unattended: false,
        confined: true,
    };
    let path_row = render_thread_row_with_theme(30, Some("sess-01a2b3c4"), None, path_ctx, &theme);
    assert!(!path_row.contains('…'), "{path_row:?}");
}

/// `SCENE_BAR_WORKSPACE_MIN_COLS` must match the `PathView` ladder's own last
/// stop (bare leaf), not sit above it.
#[test]
fn thread_bar_workspace_floor_matches_the_ladder_capability() {
    let theme = Theme::default();
    let at_floor = workspace_spans(
        "~/projects/nuo",
        SCENE_BAR_WORKSPACE_MIN_COLS,
        Style::default(),
        &theme,
    );
    let text: String = at_floor.iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(
        text, "nuo",
        "at the floor the ladder must still name the project"
    );

    let below = workspace_spans(
        "~/projects/nuo",
        SCENE_BAR_WORKSPACE_MIN_COLS - 1,
        Style::default(),
        &theme,
    );
    assert!(
        below.is_empty(),
        "below the floor nothing faithful can be drawn"
    );
}

#[test]
fn thread_bar_paints_scenebar_bg_across_the_full_row_width() {
    let theme = Theme::default();
    let context = ThreadContext {
        workspace: "~/projects/xx",
        role: Some("developer"),
        unattended: false,
        confined: true,
    };
    let cells = render_row2_cells(
        60,
        Some("sess-01a2b3c4"),
        ViewHints {
            kind: SceneKind::Thread,
            context: Some("Refactor auth"),
            ..hints(SceneKind::Thread)
        },
        Some(context),
        &theme,
    );
    for cell in &cells {
        assert_eq!(cell.style.bg, theme.scenebar_bg());
    }
    let row: String = cells.iter().map(|cell| cell.symbol.as_str()).collect();
    assert!(row.starts_with("  "), "left pad: {row:?}");
    assert!(row.ends_with("  "), "right pad: {row:?}");
}

#[test]
fn thread_bar_preserves_short_root_workspace_without_dropping() {
    let context = ThreadContext {
        workspace: "/",
        role: Some("developer"),
        unattended: false,
        confined: true,
    };
    // Even when exact slack leaves < SCENE_BAR_WORKSPACE_MIN_COLS for "/",
    // the full root path fits verbatim without truncation and must survive.
    let row = render_thread_row(34, Some("sess-01a2b3c4"), Some("Fix auth"), context);
    assert!(
        row.trim_end().ends_with('/'),
        "root workspace must survive: {row:?}"
    );

    let short_tilde = ThreadContext {
        workspace: "~/a",
        ..context
    };
    let row_tilde = render_thread_row(34, Some("sess-01a2b3c4"), Some("Fix auth"), short_tilde);
    assert!(
        row_tilde.trim_end().ends_with("~/a"),
        "short full path fitting budget must survive: {row_tilde:?}"
    );
}

#[test]
fn thread_bar_handles_cjk_title_widths_without_overflow() {
    let context = ThreadContext {
        workspace: "~/projects/nuo/crates/view",
        role: Some("developer"),
        unattended: true,
        confined: false,
    };
    let cjk_title = "修复认证重试逻辑并优化连接池并发性能";
    for width in 20u16..=100 {
        let cells = render_row2_cells(
            width,
            Some("sess-01a2b3c4"),
            ViewHints {
                kind: SceneKind::Thread,
                context: Some(cjk_title),
                ..hints(SceneKind::Thread)
            },
            Some(context),
            &Theme::default(),
        );
        assert_eq!(cells.len(), width as usize);
        // Reconstruct logical text skipping each wide glyph's trailing
        // continuation cell in graticule's cell grid.
        let mut text = String::new();
        let mut idx = 0usize;
        while idx < cells.len() {
            let sym = cells[idx].symbol.as_str();
            text.push_str(sym);
            idx += sym.width().max(1);
        }
        assert_eq!(
            text.width(),
            width as usize,
            "width {width}: CJK logical display width must match terminal width exactly: {text:?}"
        );
        assert!(
            text.starts_with("  ") && text.ends_with("  "),
            "width {width}: left/right padding must remain intact: {text:?}"
        );
    }
}

#[test]
fn thread_bar_falls_back_to_bare_thread_when_id_absent_and_tolerates_none_context() {
    let no_id = render_thread_row(
        60,
        None,
        None,
        ThreadContext {
            workspace: "~/projects/nuo",
            role: Some("developer"),
            unattended: false,
            confined: true,
        },
    );
    assert!(
        no_id.starts_with("  [DEVELOPER]  thread  "),
        "missing id falls back to bare `thread`: {no_id:?}"
    );
    assert!(no_id.trim_end().ends_with("~/projects/nuo"), "{no_id:?}");

    let no_ctx = render_row2_with_context(
        60,
        Some("sess-01a2b3c4"),
        ViewHints {
            kind: SceneKind::Thread,
            context: Some("Standalone title"),
            ..hints(SceneKind::Thread)
        },
        None,
    );
    assert!(
        no_ctx.starts_with("  Standalone title"),
        "None thread_context still renders title cleanly: {no_ctx:?}"
    );
}

#[test]
fn thread_bar_id_tail_is_utf8_safe() {
    let cells = render_row2_cells(
        60,
        Some("会话-甲乙丙丁戊"),
        hints(SceneKind::Thread),
        Some(ThreadContext {
            workspace: "",
            role: None,
            unattended: false,
            confined: true,
        }),
        &Theme::default(),
    );
    let mut text = String::new();
    let mut idx = 0usize;
    while idx < cells.len() {
        let sym = cells[idx].symbol.as_str();
        text.push_str(sym);
        idx += sym.width().max(1);
    }
    assert!(
        text.starts_with("  thread 乙丙丁戊"),
        "id_tail must slice on UTF-8 char boundaries without panicking: {text:?}"
    );
}

#[test]
fn thread_bar_styles_untitled_dim_and_titled_bold_heading() {
    let theme = Theme::default();
    let context = ThreadContext {
        workspace: "~/projects/nuo",
        role: Some("developer"),
        unattended: false,
        confined: true,
    };
    let untitled_cells = render_row2_cells(
        72,
        Some("sess-01a2b3c4"),
        hints(SceneKind::Thread),
        Some(context),
        &theme,
    );
    let untitled_row: String = untitled_cells.iter().map(|c| c.symbol.as_str()).collect();
    let thread_col = untitled_row
        .find("thread b3c4")
        .expect("untitled fallback present");
    assert_eq!(
        untitled_cells[thread_col].style.fg,
        theme.dim(),
        "untitled fallback must use dim foreground"
    );
    assert!(
        !untitled_cells[thread_col]
            .style
            .add
            .contains(Modifier::BOLD),
        "untitled fallback must not be bold"
    );

    let titled_cells = render_row2_cells(
        72,
        Some("sess-01a2b3c4"),
        ViewHints {
            kind: SceneKind::Thread,
            context: Some("Fix retry loop"),
            ..hints(SceneKind::Thread)
        },
        Some(context),
        &theme,
    );
    let titled_row: String = titled_cells.iter().map(|c| c.symbol.as_str()).collect();
    let title_col = titled_row
        .find("Fix retry loop")
        .expect("authentic title present");
    assert_eq!(
        titled_cells[title_col].style.fg,
        theme.heading(),
        "authentic title must use heading foreground"
    );
    assert!(
        titled_cells[title_col].style.add.contains(Modifier::BOLD),
        "authentic title must be bold"
    );
}

#[test]
fn thread_bar_golden_baselines_lock_exact_columns() {
    let cases = [
        (
            "untitled_default_posture_is_silent",
            render_thread_row(
                72,
                Some("sess-01a2b3c4"),
                None,
                ThreadContext {
                    workspace: "~/projects/nuo",
                    role: Some("developer"),
                    unattended: false,
                    confined: true,
                },
            ),
        ),
        (
            "titled_full_deviation_replaces_thread_id_in_place",
            render_thread_row(
                72,
                Some("sess-01a2b3c4"),
                Some("Fix retry loop"),
                ThreadContext {
                    workspace: "~/projects/nuo",
                    role: Some("developer"),
                    unattended: true,
                    confined: false,
                },
            ),
        ),
        (
            "fair_share_elides_ancestors_and_long_title_before_posture_yields",
            render_thread_row(
                64,
                Some("sess-01a2b3c4"),
                Some("Refactor database migration layer for multi-tenant isolation"),
                ThreadContext {
                    workspace: "~/projects/nuo/crates/nuo-tui",
                    role: Some("developer"),
                    unattended: true,
                    confined: false,
                },
            ),
        ),
        (
            "short_identity_flows_slack_to_workspace_leaf",
            render_thread_row(
                50,
                Some("sess-01a2b3c4"),
                None,
                ThreadContext {
                    workspace: "~/projects/nuo/crates/nuo-tui",
                    role: Some("developer"),
                    unattended: true,
                    confined: false,
                },
            ),
        ),
        (
            "long_role_elides_before_posture_yields",
            render_thread_row(
                30,
                Some("sess-01a2b3c4"),
                Some("Fix auth"),
                ThreadContext {
                    workspace: "~/projects/nuo",
                    role: Some("security-auditor"),
                    unattended: true,
                    confined: false,
                },
            ),
        ),
        (
            "narrow_row_drops_optional_elements_but_keeps_posture",
            render_thread_row(
                28,
                Some("sess-01a2b3c4"),
                Some("Fix auth"),
                ThreadContext {
                    workspace: "~/projects/nuo/crates/nuo-tui",
                    role: Some("developer"),
                    unattended: true,
                    confined: false,
                },
            ),
        ),
    ];
    let expected = [
        (
            "untitled_default_posture_is_silent",
            "  [DEVELOPER]  thread b3c4                              ~/projects/nuo",
        ),
        (
            "titled_full_deviation_replaces_thread_id_in_place",
            "  [DEVELOPER]  auto  open  Fix retry loop               ~/projects/nuo",
        ),
        (
            "fair_share_elides_ancestors_and_long_title_before_posture_yields",
            "  [DEVELOPER]  auto  open  Refactor database mig…  .../nuo-tui",
        ),
        (
            "short_identity_flows_slack_to_workspace_leaf",
            "  [DEVELOPER]  auto  open  thread b3c4   nuo-tui",
        ),
        (
            "long_role_elides_before_posture_yields",
            "  [SECURITY-AU…]  auto  open",
        ),
        (
            "narrow_row_drops_optional_elements_but_keeps_posture",
            "  [DEVELOPER]  auto  open",
        ),
    ];
    for ((name, actual), (exp_name, exp_row)) in cases.iter().zip(expected.iter()) {
        assert_eq!(name, exp_name);
        assert_eq!(
            actual.trim_end(),
            *exp_row,
            "golden layout drifted for {name}"
        );
    }
}
