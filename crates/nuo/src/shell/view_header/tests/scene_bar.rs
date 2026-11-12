//! Tier 2 scene bar tests: identity, labels, and drill-in rects.

use super::*;

use crate::shell::surfaces::SceneKind;

#[test]
fn row2_renders_thread_identity_and_task_title() {
    let row = render_row2(
        80,
        Some("sess-01a2b3c4"),
        ViewHints {
            kind: SceneKind::Thread,
            context: Some("Fix the retry loop"),
            ..hints(SceneKind::Thread)
        },
    );
    assert!(
        row.starts_with("  THREAD b3c4  Fix the retry loop"),
        "{row}"
    );
}

#[test]
fn row2_renders_untitled_thread_without_placeholder() {
    let row = render_row2(80, Some("sess-01a2b3c4"), hints(SceneKind::Thread));
    assert!(row.starts_with("  THREAD b3c4"), "{row}");
    assert!(
        !row.contains("thread#"),
        "clean thread identifier without redundant label: {row}"
    );
}

#[test]
fn row2_renders_non_thread_scene_labels() {
    for (kind, label) in [
        (SceneKind::Subagent, "subagent"),
        (SceneKind::Settings, "settings"),
        (SceneKind::Dashboard, "dashboard"),
    ] {
        assert_eq!(kind.scene_label(), label);
        let row = render_row2(80, None, hints(kind));
        assert!(row.starts_with(&format!("  {label}")), "{kind:?}: {row}");
    }
}

#[test]
fn row2_context_tone_escalates_when_flagged() {
    let theme = Theme::default();
    let first_fg = |cells: &[nuotc::Cell], needle: &str| {
        let idx = cells
            .iter()
            .position(|c| c.symbol.as_str() == needle)
            .expect("needle present");
        cells[idx].style.fg
    };
    let quiet = render_row2_cells(
        80,
        None,
        ViewHints {
            context: Some("main running"),
            ..hints(SceneKind::Dashboard)
        },
        &theme,
    );
    let urgent = render_row2_cells(
        80,
        None,
        ViewHints {
            context: Some("needs approval"),
            context_warn: true,
            ..hints(SceneKind::Dashboard)
        },
        &theme,
    );
    assert_eq!(
        first_fg(&quiet, "m"),
        theme.fg(),
        "quiet context is foreground"
    );
    assert_eq!(
        first_fg(&urgent, "n"),
        theme.warn(),
        "flagged context takes the warning tone"
    );
}

#[test]
fn row2_renders_right_aligned_drill_in_stack() {
    let crumbs = [SceneKind::Thread, SceneKind::Subagent];
    let row = render_row2(
        80,
        None,
        ViewHints {
            kind: SceneKind::Subagent,
            context: Some("[EXPLORE] inspect codebase"),
            context_warn: false,
            breadcrumbs: Some(&crumbs),
            can_back: true,
            can_forward: false,
        },
    );
    assert!(
        row.contains("[EXPLORE] inspect codebase"),
        "shows scene context: {row}"
    );
    // The affordances are navigation controls, not keycaps: the chord text
    // belongs to the Help dialog, which resolves it from the live keymap
    // (ADR-0131 §3). SceneBar must never hardcode a binding the user can
    // remap.
    assert!(row.contains('‹'), "shows a back chevron affordance: {row}");
    assert!(
        !row.contains("Alt+["),
        "the back affordance must not print a hardcoded chord: {row}"
    );
    assert!(
        row.contains("thread › subagent"),
        "shows canonical breadcrumb trail: {row}"
    );
    assert!(
        !row.contains("< thread >"),
        "no pseudo-tag delimiter artifact: {row}"
    );
    let ctx_pos = row.find("[EXPLORE]").unwrap();
    let stack_pos = row.find('‹').unwrap();
    assert!(ctx_pos < stack_pos, "context left, stack right: {row}");

    // With can_forward = true
    let row_fwd = render_row2(
        80,
        None,
        ViewHints {
            kind: SceneKind::Subagent,
            context: Some("[EXPLORE] inspect codebase"),
            context_warn: false,
            breadcrumbs: Some(&crumbs),
            can_back: true,
            can_forward: true,
        },
    );
    assert!(row_fwd.contains('›'), "shows forward affordance: {row_fwd}");
    assert!(
        !row_fwd.contains("Alt+]"),
        "the forward affordance must not print a hardcoded chord: {row_fwd}"
    );
}

#[test]
fn row2_computes_nav_hit_rects_accurately() {
    let crumbs = [SceneKind::Thread, SceneKind::Subagent];
    let theme = Theme::default();
    let mut terminal = nuotc::TestTerminal::new(80, 1);
    let mut nav_rects = SceneNavRects::default();
    terminal.draw(|frame| {
        nav_rects = draw_scene_bar(
            frame,
            frame.area(),
            None,
            &ViewHints {
                kind: SceneKind::Subagent,
                context: Some("task 1"),
                context_warn: false,
                breadcrumbs: Some(&crumbs),
                can_back: true,
                can_forward: true,
            },
            &theme,
        );
    });

    assert!(nav_rects.back.is_some());
    assert_eq!(nav_rects.crumbs.len(), 2);
    assert!(nav_rects.forward.is_some());
    let back_rect = nav_rects.back.unwrap();
    assert!(back_rect.width > 0);
    let crumb0 = nav_rects.crumbs[0].1;
    assert!(crumb0.x > back_rect.x);
    let crumb1 = nav_rects.crumbs[1].1;
    assert!(crumb1.x > crumb0.x);
    let fwd_rect = nav_rects.forward.unwrap();
    assert!(fwd_rect.x > crumb1.x);
}

#[test]
fn row2_band_paints_the_full_row_width() {
    let theme = Theme::default();
    let mut terminal = nuotc::TestTerminal::new(60, 1);
    terminal.draw(|frame| {
        draw_scene_bar(
            frame,
            frame.area(),
            None,
            &hints(SceneKind::Settings),
            &theme,
        );
    });
    for cell in terminal.buffer().cells() {
        assert_eq!(cell.style.bg, theme.scenebar_bg());
    }
}
