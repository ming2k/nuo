//! Tier 3 thread-context bar tests: governance, path ladder, and width.

use super::*;

#[test]
fn row3_reads_who_how_where_left_to_right() {
    let context = ThreadContext {
        workspace: "~/projects/xx",
        role: Some("developer"),
        unattended: true,
        confined: false,
    };
    let row = render_row3(80, context);
    // Who (`[ROLE]`) → how (`auto`/`open`) → where (workspace, right-anchored).
    assert!(row.starts_with("  [DEVELOPER]  auto  open"), "{row}");
    assert!(row.trim_end().ends_with("~/projects/xx"), "{row}");
}

#[test]
fn row3_reports_only_deviations_from_the_safe_default() {
    // Interactive confirmation + workspace-confined filesystem is the
    // default posture: it costs zero columns.
    let context = ThreadContext {
        workspace: "~/projects/xx",
        role: Some("developer"),
        unattended: false,
        confined: true,
    };
    let row = render_row3(80, context);
    assert!(
        row.starts_with("  [DEVELOPER]  "),
        "no posture tokens: {row}"
    );
    assert!(!row.contains("auto"), "{row}");
    assert!(!row.contains("open"), "{row}");
}

#[test]
fn row3_posture_tokens_are_lowercase_and_unbracketed() {
    // Caps + brackets are already spoken for: `[ROLE]` is the badge slot and
    // `[N queued]` means a count in the footer stack. Posture stays a plain
    // lowercase word so it cannot be misread as either.
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
fn row3_posture_tokens_join_with_the_peer_enumeration_gap() {
    let context = ThreadContext {
        workspace: "",
        role: Some("developer"),
        unattended: true,
        confined: false,
    };
    let row = render_row3(80, context);
    assert!(
        row.contains("[DEVELOPER]  auto  open"),
        "peer tokens use a plain two-column gap, no glyph: {row}"
    );
}

#[test]
fn row3_workspace_free_hides_workspace_and_shows_role_badge() {
    let context = ThreadContext {
        workspace: "",
        role: Some("philosophist"),
        unattended: false,
        confined: true,
    };
    let row = render_row3(80, context);
    assert!(row.starts_with("  [PHILOSOPHIST]"), "{row}");
    assert!(!row.contains("~/"), "{row}");
}

#[test]
fn row3_shows_custom_user_role_badge() {
    let context = ThreadContext {
        workspace: "~/projects/xx",
        role: Some("security-auditor"),
        unattended: false,
        confined: true,
    };
    let row = render_row3(80, context);
    assert!(row.starts_with("  [SECURITY-AUDITOR]"), "{row}");
}

/// `[INV-HEAD-07]` anchor stability: flipping a posture token must not reflow
/// the *path*, because the path is right-anchored and the governance
/// cluster grows into the gap between them.
#[test]
fn row3_toggling_posture_never_reflow_the_workspace_path() {
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
    let quiet_row = render_row3(80, quiet);
    let loud_row = render_row3(80, loud);
    for row in [&quiet_row, &loud_row] {
        assert!(row.trim_end().ends_with("~/projects/nuo"), "{row}");
    }
    // The path sits at the same column in both rows.
    assert_eq!(
        quiet_row.find("~/projects/nuo"),
        loud_row.find("~/projects/nuo"),
        "quiet: {quiet_row}\nloud: {loud_row}"
    );
}

/// `[INV-HEAD-07]` safety-first degradation: as the row narrows, the path
/// shortens while `[ROLE]` and the posture tokens survive intact.
#[test]
fn row3_narrow_row_keeps_posture_and_shortens_the_path() {
    let context = ThreadContext {
        workspace: "~/projects/nuo/crates/nuo-tui",
        role: Some("developer"),
        unattended: true,
        confined: false,
    };
    // Wide: the full path survives.
    let wide = render_row3(80, context);
    assert!(wide.contains("~/projects/nuo/crates/nuo-tui"), "{wide}");

    // Narrower: still governed, path shortened from the ancestor end so the
    // leaf — the part that names the project — is what remains.
    let narrow = render_row3(46, context);
    assert!(narrow.contains("[DEVELOPER]  auto  open"), "{narrow}");
    assert!(narrow.contains("nuo-tui"), "leaf survives: {narrow}");
    assert!(
        !narrow.contains("~/projects/nuo/crates"),
        "ancestors elided: {narrow}"
    );
}

/// The row must never paint past its own width. Regression guard for the
/// inter-cluster gap: when the path is dropped the gap must be dropped with
/// it, or the row overruns by two columns and clips the trailing pad.
#[test]
fn row3_never_paints_beyond_the_row_width() {
    let context = ThreadContext {
        workspace: "~/projects/nuo",
        role: Some("developer"),
        unattended: true,
        confined: false,
    };
    for width in 8u16..=120 {
        let row = render_row3(width, context);
        assert_eq!(
            row.chars().count(),
            width as usize,
            "row must fill exactly {width} columns: {row:?}"
        );
    }
}

/// Below the honesty floor the path is dropped wholesale: a two-letter
/// sliver would still claim to be the workspace while naming nothing, so
/// posture keeps the row.
#[test]
fn row3_drops_the_path_rather_than_lying_about_it() {
    let context = ThreadContext {
        workspace: "~/projects/nuo/crates/nuo-tui",
        role: Some("developer"),
        unattended: true,
        confined: false,
    };
    // 30 columns → 26 text columns. The governance cluster is 23 wide, so
    // it survives whole while the path is left a 1-column budget — below
    // the honesty floor, so it is dropped rather than drawn as `…`.
    let row = render_row3(30, context);
    assert!(row.contains("[DEVELOPER]  auto  open"), "{row}");
    assert!(!row.contains("~"), "no fake path: {row}");
    assert!(!row.contains("…"), "no fake path: {row}");
}

/// Posture is safety state, so it outranks every other element on the row
/// (`[INV-HEAD-08]`): a role badge long enough to crowd it must yield, and
/// the token must never be clipped into a prefix that misreports the session.
#[test]
fn row3_posture_outranks_a_long_role_badge() {
    let context = ThreadContext {
        workspace: "~/projects/nuo",
        role: Some("a-very-long-custom-user-role-name"),
        unattended: true,
        confined: false,
    };
    let row = render_row3(44, context);
    assert!(row.contains("auto  open"), "posture intact: {row}");
    assert!(row.contains('…'), "role badge elided instead: {row}");
    assert!(
        !row.contains("A-VERY-LONG-CUSTOM-USER-ROLE-NAME"),
        "full role must yield to safety state: {row}"
    );
}

/// A posture token is never emitted as a prefix (`[INV-HEAD-08]`). `open`
/// clipped to `op` would still be rendered in the warning tone, claiming a
/// state the session does not have.
#[test]
fn row3_never_emits_a_truncated_posture_token() {
    for width in 8u16..=80 {
        let context = ThreadContext {
            workspace: "~/projects/nuo",
            role: Some("security-auditor"),
            unattended: true,
            confined: false,
        };
        let row = render_row3(width, context);
        for fragment in ["aut", "ope"] {
            let clipped = row.contains(fragment) && !row.contains("auto") && !row.contains("open");
            assert!(
                !clipped,
                "width {width}: posture token clipped to a prefix: {row:?}"
            );
        }
    }
}

/// A non-Unicode terminal must not receive `…`. Both the badge elision and
/// the path ladder take their marker from `theme.glyphs.ellipsis`, which is
/// `...` under `ASCII_GLYPHS`.
///
/// Pinned to a width where an elision actually happens: a wider row draws no
/// marker at all, so asserting its absence there would pass even with the
/// glyph override removed.
#[test]
fn row3_degrades_its_ellipsis_marker_under_ascii_glyphs() {
    let theme = Theme {
        glyphs: nuotc::ASCII_GLYPHS,
        ..Theme::default()
    };
    let context = ThreadContext {
        workspace: "~/projects/nuo/crates/nuo-tui",
        role: Some("security-auditor"),
        unattended: true,
        confined: false,
    };
    // Width 30 elides the role badge, so the marker must come from the theme.
    let row = render_row3_with_theme(30, context, &theme);
    assert!(
        !row.contains('…'),
        "ASCII profile must not draw the Unicode ellipsis: {row:?}"
    );
    assert!(row.contains("..."), "ASCII marker drawn instead: {row:?}");
    assert!(row.contains("auto  open"), "posture intact: {row:?}");
}

/// The path ladder's own marker degrades too, even though `PathView`
/// hardcodes `…` internally: a single deep segment forces
/// `middle_truncate_filename`, the only ladder stage that emits one.
#[test]
fn row3_degrades_the_path_ladder_marker_under_ascii_glyphs() {
    let theme = Theme {
        glyphs: nuotc::ASCII_GLYPHS,
        ..Theme::default()
    };
    let context = ThreadContext {
        workspace: "a-really-long-single-directory-name-with-no-nesting",
        role: None,
        unattended: false,
        confined: true,
    };
    let row = render_row3_with_theme(30, context, &theme);
    assert!(
        !row.contains('…'),
        "ASCII profile must not draw the Unicode ellipsis: {row:?}"
    );
}

/// `CONTEXT_BAR_WORKSPACE_MIN_COLS` must be the ladder's own last stop, not
/// one rung above it. Asserted directly against the budget seam: at exactly
/// the floor the path still names the project, one column under it there is
/// nothing faithful left. An earlier floor of 8 silently discarded widths
/// 4..=7 that the ladder serves correctly.
///
/// This tests the constant itself. A render-width sweep would be a
/// tautology — it can only rediscover whatever floor is in force.
#[test]
fn row3_workspace_floor_matches_the_ladder_capability() {
    let theme = Theme::default();
    let at_floor = workspace_spans(
        "~/projects/nuo",
        CONTEXT_BAR_WORKSPACE_MIN_COLS,
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
        CONTEXT_BAR_WORKSPACE_MIN_COLS - 1,
        Style::default(),
        &theme,
    );
    assert!(
        below.is_empty(),
        "below the floor nothing faithful can be drawn"
    );
}

#[test]
fn row3_band_paints_the_full_row_width() {
    let theme = Theme::default();
    let context = ThreadContext {
        workspace: "~/projects/xx",
        role: Some("developer"),
        unattended: false,
        confined: true,
    };
    let mut terminal = nuotc::TestTerminal::new(60, 1);
    terminal.draw(|frame| {
        draw_thread_context_bar(frame, frame.area(), &context, &theme);
    });
    for cell in terminal.buffer().cells() {
        assert_eq!(cell.style.bg, theme.contextbar_bg());
    }
    let row: String = terminal
        .buffer()
        .cells()
        .iter()
        .map(|cell| cell.symbol.as_str())
        .collect();
    assert!(row.starts_with("  "), "left pad: {row:?}");
    assert!(row.ends_with("  "), "right pad: {row:?}");
}
