//! Golden baselines for the single-row Thread `SceneBar` (ADR-0261
//! `[INV-HEAD-09]`, `[INV-HEAD-10]`; revised by ADR-0268 `[INV-HEAD-11]`,
//! `[INV-HEAD-12]`).
//!
//! These lock the *painted* row — which columns each element occupies — rather
//! than the span list that produced it, so a refactor that keeps the spans but
//! shifts the layout is still caught. Widths straddle the full degradation
//! ladder: untitled silent default posture, titled in-place arrival, fair-share
//! pressure, untitled full-slack workspace, short-title slack redistribution,
//! elided role, framed bracket fallback, and dropped optional elements.

use crate::shell::render::Theme;
use nuo_view::view_header::{SceneKind, ThreadContext, ViewHints, draw_scene_bar};

fn render_bar_with_theme(
    width: u16,
    title: Option<&str>,
    context: &ThreadContext<'_>,
    theme: &Theme,
) -> String {
    let mut terminal = graticule::TestTerminal::new(width, 1);
    let hints = ViewHints {
        kind: SceneKind::Thread,
        context: title,
        ..ViewHints::simple(SceneKind::Thread)
    };
    terminal.draw(|f| {
        draw_scene_bar(f, f.area(), &hints, Some(context), theme);
    });
    let row: String = terminal
        .buffer()
        .cells()
        .iter()
        .map(|cell| cell.symbol.as_str())
        .collect();
    row.trim_end().to_string()
}

fn render_bar(width: u16, title: Option<&str>, context: &ThreadContext<'_>) -> String {
    render_bar_with_theme(width, title, context, &Theme::default())
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
fn untitled_default_posture_is_silent() {
    let h = head("~/projects/nuo", Some("developer"), false, true);
    insta::assert_snapshot!(render_bar(72, None, &h));
}

#[test]
fn titled_full_deviation_places_title_in_place() {
    let h = head("~/projects/nuo", Some("developer"), true, false);
    insta::assert_snapshot!(render_bar(72, Some("Fix retry loop"), &h));
}

#[test]
fn fair_share_elides_ancestors_and_long_title_before_posture_yields() {
    let h = head(
        "~/projects/nuo/crates/nuo-tui",
        Some("developer"),
        true,
        false,
    );
    insta::assert_snapshot!(render_bar(
        64,
        Some("Refactor database migration layer for multi-tenant isolation"),
        &h
    ));
}

#[test]
fn untitled_flows_all_slack_to_workspace() {
    let h = head(
        "~/projects/nuo/crates/nuo-tui",
        Some("developer"),
        true,
        false,
    );
    insta::assert_snapshot!(render_bar(50, None, &h));
}

#[test]
fn short_title_flows_slack_to_workspace_leaf() {
    let h = head(
        "~/projects/nuo/crates/nuo-tui",
        Some("developer"),
        true,
        false,
    );
    insta::assert_snapshot!(render_bar(50, Some("Fix auth"), &h));
}

/// Width 30 is the first width at which the role must yield: `SECURITY-AUDITOR`
/// is 16 columns and the posture block `auto  open` is 10 columns plus a 2-col
/// gap (28 > 26 text columns), so the role elides while posture stays intact.
#[test]
fn long_role_elides_before_posture_yields() {
    let h = head("~/projects/nuo", Some("security-auditor"), true, false);
    let row = render_bar(30, Some("Fix auth"), &h);
    assert!(row.contains("auto  open"), "posture intact: {row:?}");
    assert!(row.contains('…'), "role must actually elide: {row:?}");
    assert!(!row.contains("~/"), "path yielded entirely: {row:?}");
    insta::assert_snapshot!(row);
}

#[test]
fn narrow_row_drops_optional_elements_but_keeps_posture() {
    let h = head(
        "~/projects/nuo/crates/nuo-tui",
        Some("developer"),
        true,
        false,
    );
    insta::assert_snapshot!(render_bar(28, Some("Fix auth"), &h));
}

#[test]
fn framed_surface_preserves_role_brackets() {
    let h = head("~/projects/nuo", Some("developer"), true, false);
    let framed = Theme {
        elevation: nuo_ui_primitives::elevation::ElevationArchetype::Structured,
        ..Theme::default()
    };
    insta::assert_snapshot!(render_bar_with_theme(
        72,
        Some("Fix retry loop"),
        &h,
        &framed
    ));
}
