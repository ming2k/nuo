//! Golden baselines for the single-row Thread `SceneBar` (ADR-0261
//! `[INV-HEAD-09]`, `[INV-HEAD-10]`).
//!
//! These lock the *painted* row — which columns each element occupies — rather
//! than the span list that produced it, so a refactor that keeps the spans but
//! shifts the layout is still caught. Widths straddle the full degradation
//! ladder: untitled default posture, titled in-place swap, fair-share pressure,
//! slack redistribution, elided role, and dropped optional elements.

use crate::shell::render::Theme;
use view::view_header::{SceneKind, ThreadContext, ViewHints, draw_scene_bar};

fn render_bar(
    width: u16,
    thread_id: Option<&str>,
    title: Option<&str>,
    context: &ThreadContext<'_>,
) -> String {
    let mut terminal = graticule::TestTerminal::new(width, 1);
    let hints = ViewHints {
        kind: SceneKind::Thread,
        context: title,
        ..ViewHints::simple(SceneKind::Thread)
    };
    terminal.draw(|f| {
        draw_scene_bar(
            f,
            f.area(),
            thread_id,
            &hints,
            Some(context),
            &Theme::default(),
        );
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
fn untitled_default_posture_is_silent() {
    let h = head("~/projects/nuo", Some("developer"), false, true);
    insta::assert_snapshot!(render_bar(72, Some("sess-01a2b3c4"), None, &h));
}

#[test]
fn titled_full_deviation_replaces_thread_id_in_place() {
    let h = head("~/projects/nuo", Some("developer"), true, false);
    insta::assert_snapshot!(render_bar(
        72,
        Some("sess-01a2b3c4"),
        Some("Fix retry loop"),
        &h
    ));
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
        Some("sess-01a2b3c4"),
        Some("Refactor database migration layer for multi-tenant isolation"),
        &h
    ));
}

#[test]
fn short_identity_flows_slack_to_workspace_leaf() {
    let h = head(
        "~/projects/nuo/crates/nuo-tui",
        Some("developer"),
        true,
        false,
    );
    insta::assert_snapshot!(render_bar(50, Some("sess-01a2b3c4"), None, &h));
}

/// Width 30 is the first width at which the role must yield: the badge
/// alone is 18 columns and the posture block 10, so 26 text columns cannot
/// hold both.
#[test]
fn long_role_elides_before_posture_yields() {
    let h = head("~/projects/nuo", Some("security-auditor"), true, false);
    let row = render_bar(30, Some("sess-01a2b3c4"), Some("Fix auth"), &h);
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
    insta::assert_snapshot!(render_bar(28, Some("sess-01a2b3c4"), Some("Fix auth"), &h));
}
