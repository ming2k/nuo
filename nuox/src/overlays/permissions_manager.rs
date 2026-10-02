//! Permissions manager modal — the "always allow" rule management surface.
//!
//! Distinct from [`super::permission`] (the inline real-time approval sheet),
//! this is a centered, dismissable overlay opened via the `/permissions` slash
//! command. It lists every cached "always allow" rule for the session, with
//! per-row revoke (`Space`) and a clear-all action (`c`).

use nuotc::{Frame, Line};

use super::common::{placeholder, selectable_row};
use crate::primitives::{
    ContentModalSpec, FooterHint, SCROLL_EDGE_MARGIN, content_modal_area, keyvocab,
    modal_chrome_rows, modal_frame, modal_header, render_body, render_modal_footer,
};
use crate::render::Theme;

/// Draw the permissions manager modal: a centered, dismissable list of cached
/// "always allow" rules. Each row shows `<tool> <scope>`; `Space` revokes the
/// selected rule, `c` clears all. Data comes from the session-context snapshot
/// (the same one the `/session` modal used), refreshed after each mutation.
pub fn draw_permissions_manager(
    frame: &mut Frame,
    session_context: Option<&nuo_contracts::SessionContextSnapshot>,
    modal_index: usize,
    scroll: &mut usize,
    theme: &Theme,
) -> nuotc::Rect {
    let geometry = ContentModalSpec::PERMISSIONS;
    let rules = session_context
        .map(|s| s.permissions.as_slice())
        .unwrap_or(&[]);
    let content_lines = if rules.is_empty() {
        1
    } else {
        rules.len() as u16
    };
    let desired = content_lines + modal_chrome_rows(geometry.modal_spec());
    let area = content_modal_area(frame, geometry, desired);
    let f = modal_frame(frame, area, theme, true, true);

    // Header
    modal_header(frame, f.header, "Permissions", theme);

    // Body: the rule list
    let rules = session_context
        .map(|s| s.permissions.as_slice())
        .unwrap_or(&[]);
    let mut body: Vec<Line> = Vec::new();
    if rules.is_empty() {
        body.push(placeholder(
            "No always-allow rules cached this session.",
            session_context.is_some(),
            theme.muted(),
        ));
    } else {
        let body_w = f.body.width as usize;
        for (i, rule) in rules.iter().enumerate() {
            let summary = format!("{} {}", rule.tool, rule.scope);
            body.push(selectable_row(
                i,
                modal_index,
                &summary,
                "Space revokes",
                true,
                "allowed",
                "",
                body_w,
                theme,
            ));
        }
    }

    let follow = if rules.is_empty() {
        None
    } else {
        Some(modal_index)
    };
    render_body(
        frame,
        f.body,
        body,
        scroll,
        crate::primitives::BodyRenderOptions::new(follow, SCROLL_EDGE_MARGIN, false),
        theme,
    );

    // Footer
    if let Some(fo) = f.footer {
        let hints: &[FooterHint] = if rules.is_empty() {
            &[FooterHint::key_always(crate::keymap::Key::ESC, "close")]
        } else {
            &[
                FooterHint::navigation(keyvocab::ARROWS_UD, "select"),
                FooterHint::primary(keyvocab::SPACE, "revoke"),
                FooterHint::secondary("c", "clear all"),
                FooterHint::key_always(crate::keymap::Key::ESC, "close"),
            ]
        };
        render_modal_footer(frame, fo, hints, theme);
    }
    area
}
