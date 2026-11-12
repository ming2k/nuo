//! Golden baselines for the Tier 3 `ThreadContextBar` (ADR-0062
//! `[INV-HEAD-07]`, `[INV-HEAD-08]`).
//!
//! These lock the *painted* row — which columns each element occupies — rather
//! than the span list that produced it, so a refactor that keeps the spans but
//! shifts the layout is still caught. The widths are chosen to straddle the
//! degradation ladder: full path, elided ancestors, elided role, and dropped
//! path.

use crate::shell::render::Theme;
use crate::shell::view_header::{ThreadContext, draw_thread_context_bar};

fn render_bar(width: u16, context: &ThreadContext<'_>) -> String {
    let mut terminal = nuotc::TestTerminal::new(width, 1);
    terminal.draw(|f| {
        draw_thread_context_bar(f, f.area(), context, &Theme::default());
    });
    let row: String = terminal
        .buffer()
        .cells()
        .iter()
        .map(|cell| cell.symbol.as_str())
        .collect();
    row.trim_end().to_string()
}

fn head<'a>(
    workspace: &'a str,
    role: Option<&'a str>,
    unattended: bool,
    confined: bool,
) -> ThreadContext<'a> {
    ThreadContext {
        workspace,
        role,
        unattended,
        confined,
    }
}

#[test]
fn default_posture_is_silent() {
    let h = head("~/projects/nuo", Some("developer"), false, true);
    insta::assert_snapshot!(render_bar(72, &h));
}

#[test]
fn full_deviation_with_full_path() {
    let h = head("~/projects/nuo", Some("developer"), true, false);
    insta::assert_snapshot!(render_bar(72, &h));
}

#[test]
fn path_elides_ancestors_before_posture_yields() {
    let h = head(
        "~/projects/nuo/crates/nuo-tui",
        Some("developer"),
        true,
        false,
    );
    insta::assert_snapshot!(render_bar(50, &h));
}

/// Width 30 is the first width at which the role must yield: the badge
/// alone is 18 columns and the posture block 12, so 32 text columns cannot
/// hold both. Asserted structurally here too, because a snapshot happily
/// records a row whose name no longer matches its content.
#[test]
fn long_role_elides_before_posture_yields() {
    let h = head("~/projects/nuo", Some("security-auditor"), true, false);
    let row = render_bar(30, &h);
    assert!(row.contains("auto  open"), "posture intact: {row:?}");
    assert!(row.contains('…'), "role must actually elide: {row:?}");
    assert!(!row.contains("~/"), "path yielded entirely: {row:?}");
    insta::assert_snapshot!(row);
}

#[test]
fn narrow_row_drops_the_path_but_keeps_posture() {
    let h = head(
        "~/projects/nuo/crates/nuo-tui",
        Some("developer"),
        true,
        false,
    );
    insta::assert_snapshot!(render_bar(28, &h));
}
