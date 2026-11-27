//! The PreAttach gate's painted surface.
//!
//! These assert the *rendered* text, not the state object: the gate is the whole
//! screen while it is up, so the pixels are the contract. Every claim below was
//! wrong at some point in the surface's life — "Esc quit", a re-review described
//! as a first contact, a prompt that hid which domains had changed, and a status
//! spliced into the asset name with `  |  [` — and none of them was catchable
//! from `PreAttachState` alone.

use super::helpers::render_pre_attach_grid;
use crate::shell::PreAttachState;
use nuo_protocol::{WorkspaceSecuritySnapshot, WorkspaceTrustState};

fn snapshot(instructions: WorkspaceTrustState) -> WorkspaceSecuritySnapshot {
    WorkspaceSecuritySnapshot {
        root: "/tmp/proj".to_string(),
        mcp: WorkspaceTrustState::Absent,
        skills: WorkspaceTrustState::Absent,
        hooks: WorkspaceTrustState::Absent,
        instructions,
        ex_workspace: WorkspaceTrustState::Absent,
        user_assets: WorkspaceTrustState::Absent,
    }
}

fn mixed_snapshot() -> WorkspaceSecuritySnapshot {
    WorkspaceSecuritySnapshot {
        root: "/tmp/proj".to_string(),
        mcp: WorkspaceTrustState::Quarantined,
        skills: WorkspaceTrustState::Absent,
        hooks: WorkspaceTrustState::Absent,
        instructions: WorkspaceTrustState::Changed,
        ex_workspace: WorkspaceTrustState::Absent,
        user_assets: WorkspaceTrustState::Expired,
    }
}

#[test]
fn the_painted_gate_says_esc_skips_rather_than_quitting() {
    let state = PreAttachState::from_snapshot(&snapshot(WorkspaceTrustState::Quarantined))
        .expect("never-trusted instructions must gate");
    let grid = render_pre_attach_grid(&state, 80, 30);
    assert!(
        grid.contains("Esc skip"),
        "the footer must describe what Esc actually does:\n{grid}"
    );
    assert!(
        !grid.contains("Esc quit"),
        "Esc no longer quits — the copy must not promise it:\n{grid}"
    );
    // And what skipping *means* is on screen above it, because the key legend
    // has no room to be a sentence at a 56-column panel.
    assert!(
        grid.contains("stays off"),
        "the consequence of skipping an asset must be stated:\n{grid}"
    );
}

#[test]
fn the_painted_gate_describes_a_re_review_as_a_change() {
    // The regression: a `Changed` workspace is not a first contact. The painted
    // prompt must say the bytes moved, because that is the fact that makes the
    // decision informed.
    let state = PreAttachState::from_snapshot(&snapshot(WorkspaceTrustState::Changed))
        .expect("changed instructions must gate");
    let grid = render_pre_attach_grid(&state, 80, 30);
    assert!(
        grid.contains("changed on disk"),
        "a re-review must say so on screen:\n{grid}"
    );
    assert!(
        grid.contains("Project instructions"),
        "the changed asset must be named on its row:\n{grid}"
    );
    assert!(
        grid.contains("changed"),
        "the row must carry its status:\n{grid}"
    );
    assert!(
        !grid.contains("has not been reviewed"),
        "a re-review must not be painted as a first contact:\n{grid}"
    );
}

#[test]
fn the_painted_gate_badges_a_first_contact_as_new() {
    let state = PreAttachState::from_snapshot(&snapshot(WorkspaceTrustState::Quarantined))
        .expect("never-trusted instructions must gate");
    let grid = render_pre_attach_grid(&state, 80, 30);
    assert!(grid.contains("new"), "first contact status:\n{grid}");
    assert!(
        grid.contains("has not been reviewed"),
        "first contact wording:\n{grid}"
    );
    assert!(
        !grid.contains("changed on disk"),
        "not a re-review:\n{grid}"
    );
}

#[test]
fn the_painted_gate_carries_no_status_syntax_in_the_asset_name() {
    // The defect this pins, in the pixels: the row label used to read
    // `Project instructions  |  [Changed]`, so the status was punctuation the
    // reader had to parse, in a bracket form this product reserves for `[ROLE]`
    // badges and `[N queued]` counts. The status is now its own right-aligned
    // column and the asset name is the asset name.
    let state = PreAttachState::from_snapshot(&snapshot(WorkspaceTrustState::Changed))
        .expect("changed instructions must gate");
    let grid = render_pre_attach_grid(&state, 80, 30);
    assert!(
        !grid.contains("  |  ") && !grid.contains('·') && !grid.contains('─'),
        "no separator glyphs: fields are separated by columns and whitespace:\n{grid}"
    );
    let row = grid
        .lines()
        .find(|line| line.contains("Project instructions"))
        .unwrap_or_else(|| panic!("the asset row must be painted:\n{grid}"));
    assert!(
        !row.contains('[') || row.trim_end().matches('[').count() == 1,
        "the only bracket on a row is the selection marker, not a status:\n{row:?}"
    );
    assert!(
        row.trim_end().ends_with("changed"),
        "the status sits at the row's trailing edge, not inside the name: {row:?}"
    );
}

#[test]
fn the_painted_gate_aligns_statuses_into_one_column() {
    // Two assets, two statuses: the tokens share a trailing column so the eye can
    // compare them vertically. A separator-glyph form has no column at all — the
    // status starts wherever the preceding label happened to end.
    //
    // The comparison is on the *untrimmed* grid: right alignment is about where a
    // token ends, and `painted_rows` trims exactly that information away.
    let state = PreAttachState::from_snapshot(&mixed_snapshot()).expect("mixed review must gate");
    let mut terminal = graticule::TestTerminal::new(80, 30);
    terminal.draw(|f| {
        crate::shell::pre_attach::draw_pre_attach(f, &state, &crate::shell::Theme::default());
    });
    let buf = terminal.buffer();
    let width = buf.area().width as usize;
    let rows: Vec<String> = (0..buf.area().height as usize)
        .map(|y| {
            (0..width)
                .map(|x| buf.cells()[y * width + x].symbol.as_str())
                .collect::<String>()
        })
        .filter(|row| row.contains("[x]") || row.contains("[ ]"))
        .collect();
    assert_eq!(rows.len(), 3, "every row must be painted:\n{rows:#?}");
    // The status column's right edge is the band's, and the band spans the
    // card's inner body width (inset by `card_h_padding` from the card edge).
    let panel = crate::shell::pre_attach::panel_width(buf.area()) as usize;
    let h_pad = crate::shell::pre_attach::card_h_padding(panel as u16) as usize;
    let last_col = (width - panel) / 2 + panel - h_pad;
    for token in ["new", "changed", "expired"] {
        let row = rows
            .iter()
            .find(|row| row.contains(token))
            .unwrap_or_else(|| panic!("no row carries {token}: {rows:#?}"));
        // `find` returns a 0-based start, so `start + len` is the exclusive end —
        // one past the last painted column.
        let end = row.find(token).map(|at| at + token.len()).unwrap_or(0);
        assert_eq!(
            end, last_col,
            "{token} must end at the band's trailing edge, got column {end}: {row:?}"
        );
    }
}

#[test]
fn the_painted_gate_separates_sections_by_area_not_by_a_rule() {
    // A horizontal rule spends a row to say what a background step already says,
    // and reads as a table where there is no table. The decision block is a band.
    let state = PreAttachState::from_snapshot(&mixed_snapshot()).expect("mixed review must gate");
    let (grid, (bg_map, legend)) = {
        let mut terminal = graticule::TestTerminal::new(80, 30);
        terminal.draw(|f| {
            crate::shell::pre_attach::draw_pre_attach(f, &state, &crate::shell::Theme::default());
        });
        super::helpers::painted_rows(terminal.buffer(), 80, 30)
    };
    assert!(
        !grid.contains('─') && !grid.contains("──"),
        "no horizontal rules:\n{grid}"
    );
    // The band is a distinct background: at least one non-backdrop, non-default
    // surface is painted behind the rows.
    assert!(
        !bg_map.is_empty(),
        "the decision band must paint a background:\nlegend: {legend}\n{grid}"
    );
}

#[test]
fn the_painted_gate_names_the_workspace_only_when_the_decision_crosses_projects() {
    // A project-only decision happens in the directory the user just opened, so
    // the path is zero information there; a user-global asset makes the boundary
    // the actual question.
    let project_only = PreAttachState::from_snapshot(&snapshot(WorkspaceTrustState::Changed))
        .expect("changed instructions must gate");
    let grid = render_pre_attach_grid(&project_only, 80, 30);
    assert!(
        !grid.contains("/tmp/proj"),
        "a project-only decision must not spend a line restating the project:\n{grid}"
    );

    let global = PreAttachState::from_snapshot(&mixed_snapshot()).expect("mixed review must gate");
    let grid = render_pre_attach_grid(&global, 80, 30);
    assert!(
        grid.contains("/tmp/proj"),
        "a global asset must be distinguishable from the project's own:\n{grid}"
    );
}

#[test]
fn the_painted_gate_never_breaks_a_word_in_the_prompt() {
    // The panel geometry once gave the question body 46 usable columns while the
    // re-review sentence contained a 47-column clause, so the wrapper split it as
    // "…different conten" / "t, so…". A mid-word break in the sentence
    // explaining *what changed* is the worst place in the product for a
    // typographic bug: it is the text the decision depends on.
    for width in [56u16, 60, 80, 120] {
        let state = PreAttachState::from_snapshot(&snapshot(WorkspaceTrustState::Changed))
            .expect("changed instructions must gate");
        let grid = render_pre_attach_grid(&state, width, 30);
        // The headline's own words must survive intact somewhere on screen. Each
        // word of it is short enough that a break can only land between them.
        for word in ["changed", "disk", "approved"] {
            assert!(
                grid.contains(word),
                "{word} missing at {width} columns:\n{grid}"
            );
        }
        // A split word leaves a row ending in a letter with no punctuation. The
        // sentence is prose, so every wrapped row must end on a word boundary.
        for line in grid.lines() {
            let t = line.trim();
            assert!(
                !t.ends_with("conten") && !t.ends_with("approve") && !t.ends_with("unreviewe"),
                "row ends mid-word at {width} columns: {t:?}\n{grid}"
            );
        }
    }
}

#[test]
fn the_painted_gate_keeps_its_exit_hint_at_every_supported_width() {
    // The gate is the entire screen, so a footer clipped off the bottom is a
    // surface with no stated exit. At narrow widths the hint degrades (labels
    // collapse, lower-priority hints overflow) rather than truncating mid-word,
    // so the assertion is that the exit is *present*.
    for width in [48u16, 60, 80, 120, 200] {
        let state = PreAttachState::from_snapshot(&snapshot(WorkspaceTrustState::Changed))
            .expect("changed instructions must gate");
        let grid = render_pre_attach_grid(&state, width, 30);
        let footer = grid
            .lines()
            .skip_while(|l| !l.contains("Enter"))
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            footer.contains("Esc"),
            "the exit key must be painted at {width} columns, got {footer:?}\n{grid}"
        );
    }
}

#[test]
fn the_painted_gate_advertises_only_keys_that_exist() {
    // The footer used to spend half its width on `↑/↓ move` and `Space toggle`
    // for a one-row list, which left the two keys that actually end the decision
    // as the quietest things on screen — and pushed them into `F1 more` on a
    // multi-row gate at the panel's real width.
    let single = PreAttachState::from_snapshot(&snapshot(WorkspaceTrustState::Changed))
        .expect("changed instructions must gate");
    let grid = render_pre_attach_grid(&single, 80, 30);
    assert!(
        !grid.contains("↑↓") && !grid.contains("toggle"),
        "a one-row gate must not advertise navigation:\n{grid}"
    );

    let multi = PreAttachState::from_snapshot(&mixed_snapshot()).expect("mixed review must gate");
    let grid = render_pre_attach_grid(&multi, 80, 30);
    assert!(
        grid.contains("↑↓") && grid.contains("toggle"),
        "a multi-row gate must advertise navigation:\n{grid}"
    );
    assert!(
        !grid.contains("F1 more"),
        "the gate has four keys; none of them may hide behind an overflow legend:\n{grid}"
    );
}

#[test]
fn the_submitting_state_is_one_line_not_three() {
    // Three sentences said this before: "Applying trust settings…", "Entering
    // session…", and "Waiting for the server to record the decision…". One fact,
    // one line.
    let mut state = PreAttachState::from_snapshot(&snapshot(WorkspaceTrustState::Changed))
        .expect("changed instructions must gate");
    let _ = state.apply(crate::shell::question_model::QuestionAction::Submit);
    assert!(state.submitting());
    let grid = render_pre_attach_grid(&state, 80, 30);
    assert!(
        grid.contains("Applying trust settings"),
        "the wait must be stated:\n{grid}"
    );
    assert!(
        !grid.contains("Entering session") && !grid.contains("Waiting for the server"),
        "one fact, one line:\n{grid}"
    );
    // And the rows are gone: a decision in flight is not answerable.
    assert!(
        !grid.contains("Project instructions"),
        "an in-flight decision must not offer rows to toggle:\n{grid}"
    );
}

#[test]
fn a_failed_attestation_is_stated_as_a_failure_with_its_cause() {
    let mut state = PreAttachState::from_snapshot(&snapshot(WorkspaceTrustState::Changed))
        .expect("changed instructions must gate");
    let _ = state.apply(crate::shell::question_model::QuestionAction::Submit);
    state.note_attestation_error("cannot open sqlite db".to_string());
    let grid = render_pre_attach_grid(&state, 80, 30);
    assert!(
        grid.contains("Trust was not applied"),
        "the failure must be named:\n{grid}"
    );
    assert!(
        grid.contains("cannot open sqlite db"),
        "the cause must be on screen, not only in the logs:\n{grid}"
    );
    // The decision reopens: the user retries or declines knowing why.
    assert!(
        grid.contains("Project instructions"),
        "a failed round-trip must return the decision to the user:\n{grid}"
    );
}

#[test]
fn the_painted_gate_renders_as_a_compact_centered_elevated_card() {
    let state = PreAttachState::from_snapshot(&snapshot(WorkspaceTrustState::Changed))
        .expect("changed instructions must gate");
    let theme = crate::shell::Theme::default();
    assert_ne!(
        theme.panel(),
        theme.backdrop,
        "card surface must be elevated above the backdrop"
    );

    let (width, height) = (90u16, 30u16);
    let mut terminal = graticule::TestTerminal::new(width, height);
    terminal.draw(|f| {
        crate::shell::pre_attach::draw_pre_attach(f, &state, &theme);
    });
    let buf = terminal.buffer();

    // Find bounding box of all cells that are NOT painted with `theme.backdrop`.
    let mut min_x = width;
    let mut max_x = 0u16;
    let mut min_y = height;
    let mut max_y = 0u16;
    for y in 0..height {
        for x in 0..width {
            let bg = buf.get(x, y).expect("in bounds").bg();
            if bg != theme.backdrop {
                min_x = min_x.min(x);
                max_x = max_x.max(x);
                min_y = min_y.min(y);
                max_y = max_y.max(y);
            }
        }
    }

    let card_w = max_x - min_x + 1;
    let card_h = max_y - min_y + 1;
    let expected_w = crate::shell::pre_attach::panel_width(buf.area());
    assert_eq!(card_w, expected_w, "card width matches compact measure");
    assert!(
        card_w < width - 20,
        "card ({card_w}) must not span across the {width}-col viewport"
    );

    // Horizontally and vertically centered within 1 cell of rounding.
    let left_margin = min_x;
    let right_margin = width - 1 - max_x;
    assert!(
        left_margin.abs_diff(right_margin) <= 1,
        "card must be horizontally centered: left={left_margin}, right={right_margin}"
    );
    let top_margin = min_y;
    let bottom_margin = height - 1 - max_y;
    assert!(
        top_margin.abs_diff(bottom_margin) <= 1,
        "card must be vertically centered: top={top_margin}, bottom={bottom_margin}"
    );

    // Every cell inside the card is painted with either `theme.panel()` or a
    // decision-row band (`theme.selected()` / `theme.raised()`), never the backdrop
    // or `Color::Reset`.
    for y in min_y..=max_y {
        for x in min_x..=max_x {
            let bg = buf.get(x, y).expect("in bounds").bg();
            assert!(
                bg == theme.panel() || bg == theme.selected() || bg == theme.raised(),
                "unexpected bg at ({x}, {y}) inside card: {bg:?}"
            );
        }
    }
    // Top/bottom padding rows inside the card carry `theme.panel()`.
    assert_eq!(
        buf.get(min_x, min_y).expect("in bounds").bg(),
        theme.panel()
    );
    assert_eq!(
        buf.get(max_x, max_y).expect("in bounds").bg(),
        theme.panel()
    );
    // Card stays compact enough for its height.
    assert!(card_h >= 10 && card_h < height);
}

#[test]
fn the_painted_gate_frames_the_card_in_monochrome_mode() {
    let state = PreAttachState::from_snapshot(&snapshot(WorkspaceTrustState::Changed))
        .expect("changed instructions must gate");
    let mono = crate::shell::Theme::monochrome();
    let mut terminal = graticule::TestTerminal::new(80, 24);
    terminal.draw(|f| {
        crate::shell::pre_attach::draw_pre_attach(f, &state, &mono);
    });
    let (grid, _) = super::helpers::painted_rows(terminal.buffer(), 80, 24);
    assert!(
        grid.contains('+') && grid.contains('-'),
        "monochrome card draws an explicit ASCII frame:\n{grid}"
    );
    assert!(
        grid.contains("Workspace trust") && grid.contains("Enter trust"),
        "monochrome card preserves content and exit affordance:\n{grid}"
    );
}
