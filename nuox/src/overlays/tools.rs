//! Tools manager modal — the interactive tool-list surface.
//!
//! Distinct from [`super::session`] (the read-only MODEL/MCP/SKILLS dashboard),
//! this is the centered, dismissable overlay opened via the `/tools` slash
//! command. It lists every tool available to the live session — builtin,
//! `mcp:<server>`, `pursuit`, or `plan` — with per-row toggle (`Space`).
//!
//! This used to be the `TOOLS` section embedded at the bottom of the Session
//! dashboard; it was pulled out into its own command/modal so the session
//! overview stays a glanceable summary and the tool list gets a focused,
//! selectable, scrollable surface of its own.

use nuotc::{
    Frame, Style, {Line, Span},
};
use unicode_width::UnicodeWidthStr;

use super::common::{placeholder, truncate_ellipsis};
use crate::components::list::{SelectableListPage, draw_selectable_list_page, row_style};
use crate::components::modal::{ModalHeader, modal_body_width};
use crate::primitives::{ContentModalSpec, FooterHint, keyvocab};
use crate::render::Theme;

/// Draw the tools manager modal: a centered, dismissable, selectable list of
/// the session's tools. Each row shows an enabled glyph, the tool name, its
/// source, a short description, and a `[on]`/`[off]` badge pinned to the right
/// edge. `Space` toggles the selected tool; the harness replies with a fresh
/// snapshot that re-renders the list. Data comes from the session-context
/// snapshot (the same one `/session` uses).
pub fn draw_tools_modal(
    frame: &mut Frame,
    session_context: Option<&nuo_wire::SessionContextSnapshot>,
    modal_index: usize,
    scroll: &mut usize,
    follow_selection: bool,
    theme: &Theme,
) -> nuotc::Rect {
    // Width is independent of height, so probe a full-height rect first to learn
    // the body's content width, then build the list, then size the panel to the
    // content (clamped) so there is no slab of dead space below it.
    let body_width = modal_body_width(frame, ContentModalSpec::TOOLS);

    let tools = session_context.map(|s| s.tools.as_slice()).unwrap_or(&[]);

    let mut body: Vec<Line> = Vec::new();
    let mut selected_line: Option<usize> = None;

    if session_context.is_none() {
        // Still loading: a single muted placeholder line.
        body.push(placeholder("No tools available.", false, theme.muted()));
    } else if tools.is_empty() {
        body.push(placeholder("No tools available.", true, theme.muted()));
    } else {
        // Geometry of every row: 2-col gutter + glyph(1) + space(1) +
        // name(name_col) + 2-col gap + source + 2-col gap + description, with a
        // `[on]`/`[off]` badge pinned to the row's right edge. `body_width`
        // already excludes the modal's horizontal border padding, so it is the
        // full per-row content budget.
        const GUTTER_W: usize = 2;
        const PREFIX_W: usize = GUTTER_W + 2; // gutter + "glyph "
        // Name column: cap so a long tool name can't crowd the description out,
        // but grow to align all rows' descriptions into one tidy column.
        let name_col = tools
            .iter()
            .map(|t| t.name.width())
            .max()
            .unwrap_or(0)
            .clamp(8, 24);
        let badge_w = "[off]".width();
        // The source tag (e.g. `mcp:github`) sits between the name column and
        // the description, dimmed; keep it narrow.
        const SOURCE_W: usize = 14;
        let desc_budget = body_width
            .saturating_sub(PREFIX_W + name_col + SOURCE_W + badge_w + 2)
            .max(1);

        for (i, tool) in tools.iter().enumerate() {
            let is_sel = i == modal_index;
            let style = row_style(is_sel, theme);
            let glyph_color = if is_sel {
                style.fg
            } else if tool.enabled {
                theme.ok()
            } else {
                theme.muted()
            };
            let glyph = if tool.enabled { "●" } else { "○" };
            let state = if tool.enabled { "on" } else { "off" };
            let name = truncate_ellipsis(&tool.name, name_col);
            let src = truncate_ellipsis(&tool.source, SOURCE_W);
            let desc = truncate_ellipsis(&tool.description, desc_budget);
            let badge = format!("[{state}]");
            // The description and source fill the gap between the name column
            // and the right-pinned badge; whatever is left is padding.
            let left_w = GUTTER_W
                + 2 // glyph + space
                + name_col
                + 2
                + src.width()
                + 2
                + desc.width();
            let pad = body_width.saturating_sub(left_w + badge_w);
            if is_sel {
                selected_line = Some(body.len());
            }
            body.push(Line::from(vec![
                Span::styled(" ".repeat(GUTTER_W), Style::default().bg(style.bg)),
                Span::styled(
                    format!("{glyph} "),
                    Style::default().bg(style.bg).fg(glyph_color),
                ),
                Span::styled(
                    format!("{:<w$}  ", name, w = name_col),
                    Style::default().bg(style.bg).fg(style.fg),
                ),
                Span::styled(
                    format!("{:<w$}", src, w = SOURCE_W),
                    Style::default().bg(style.bg).fg(style.dim),
                ),
                Span::styled(
                    format!("  {desc}"),
                    Style::default().bg(style.bg).fg(style.dim),
                ),
                Span::styled(" ".repeat(pad), Style::default().bg(style.bg)),
                Span::styled(badge, Style::default().bg(style.bg).fg(style.dim)),
            ]));
        }
    }

    let has_tools = session_context
        .map(|s| !s.tools.is_empty())
        .unwrap_or(false);
    draw_selectable_list_page(
        frame,
        SelectableListPage {
            geometry: ContentModalSpec::TOOLS,
            header: ModalHeader::title("Tools"),
            lines: body,
            scroll,
            selected_line,
            follow_selection,
            has_items: has_tools,
            item_footer_hints: &[
                FooterHint::navigation(keyvocab::ARROWS_UD, "select"),
                FooterHint::primary(keyvocab::SPACE, "toggle"),
                FooterHint::key_always(crate::keymap::Key::ESC, "close"),
            ],
            empty_footer_hints: &[FooterHint::key_always(crate::keymap::Key::ESC, "close")],
            extra_footer_hints: &[],
        },
        theme,
    )
}
