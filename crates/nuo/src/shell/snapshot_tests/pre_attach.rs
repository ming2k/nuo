//! The PreAttach gate's painted surface.
//!
//! These assert the *rendered* text, not the state object: the gate is the whole
//! screen while it is up, so the pixels are the contract. Every claim below was
//! wrong at some point in the surface's life — "Esc quit", a re-review described
//! as a first contact, and a prompt that hid which domains had changed — and
//! none of them was catchable from `PreAttachState` alone.

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

#[test]
fn the_painted_gate_says_esc_declines_rather_than_quitting() {
    let state = PreAttachState::from_snapshot(&snapshot(WorkspaceTrustState::Quarantined))
        .expect("never-trusted instructions must gate");
    let grid = render_pre_attach_grid(&state, 80, 30);
    assert!(
        grid.contains("Esc decline"),
        "the footer must describe what Esc actually does:\n{grid}"
    );
    assert!(
        !grid.contains("Esc quit"),
        "Esc no longer quits — the copy must not promise it:\n{grid}"
    );
}

#[test]
fn the_painted_gate_describes_a_re_review_as_a_change() {
    // The regression: a `Changed` workspace is not a first contact. The
    // painted prompt must name the file and say the earlier approval does not
    // carry over, because that is the fact that makes the decision informed.
    let state = PreAttachState::from_snapshot(&snapshot(WorkspaceTrustState::Changed))
        .expect("changed instructions must gate");
    let grid = render_pre_attach_grid(&state, 80, 30);
    assert!(
        grid.contains("changed on disk"),
        "a re-review must say so on screen:\n{grid}"
    );
    assert!(
        grid.contains("Project instructions"),
        "the changed domain must be named:\n{grid}"
    );
    assert!(
        grid.contains("[Changed]"),
        "the option row must be badged Changed, not New:\n{grid}"
    );
    assert!(
        !grid.contains("never reviewed"),
        "a re-review must not be painted as a first contact:\n{grid}"
    );
}

#[test]
fn the_painted_gate_badges_a_first_contact_as_new() {
    let state = PreAttachState::from_snapshot(&snapshot(WorkspaceTrustState::Quarantined))
        .expect("never-trusted instructions must gate");
    let grid = render_pre_attach_grid(&state, 80, 30);
    assert!(grid.contains("[New]"), "first contact badge:\n{grid}");
    assert!(
        grid.contains("never reviewed"),
        "first contact wording:\n{grid}"
    );
    assert!(
        !grid.contains("changed on disk"),
        "not a re-review:\n{grid}"
    );
}

#[test]
fn the_painted_gate_never_breaks_a_word_in_the_prompt() {
    // The panel geometry used to give the question body 46 usable columns
    // while the re-review sentence contains a 47-column clause
    // ("The earlier approval was for different content,"), so the wrapper
    // split it as "…different conten" / "t, so…". A mid-word break in the
    // sentence explaining *what changed* is the worst place in the product
    // for a typographic bug: it is the text the decision depends on.
    let state = PreAttachState::from_snapshot(&snapshot(WorkspaceTrustState::Changed))
        .expect("changed instructions must gate");
    let grid = render_pre_attach_grid(&state, 80, 30);
    assert!(
        grid.contains("different content,"),
        "the key clause must not be broken mid-word:\n{grid}"
    );
    // A split word leaves a row ending in a letter with no punctuation or
    // space — the concrete shape of the old bug.
    for line in grid.lines() {
        let t = line.trim();
        assert!(
            !t.ends_with("conten") && !t.ends_with("conte"),
            "row ends mid-word: {t:?}\n{grid}"
        );
    }
}

#[test]
fn the_painted_gate_keeps_its_exit_hint_at_every_supported_width() {
    // The gate is the entire screen, so a footer clipped off the bottom is a
    // surface with no stated exit. At narrow widths the hint wraps rather
    // than truncating, so the assertion is that both halves are *present* —
    // not that they sit on one line (they cannot, and shouldn't be forced
    // to: the old code truncated instead of wrapping).
    for width in [48u16, 60, 80, 120] {
        let state = PreAttachState::from_snapshot(&snapshot(WorkspaceTrustState::Changed))
            .expect("changed instructions must gate");
        let grid = render_pre_attach_grid(&state, width, 30);
        let footer = grid
            .lines()
            .skip_while(|l| !l.contains("↑/↓"))
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            footer.contains("Esc") && footer.contains("decline"),
            "the exit hint must be fully present at {width} columns, got {footer:?}\n{grid}"
        );
    }
}
