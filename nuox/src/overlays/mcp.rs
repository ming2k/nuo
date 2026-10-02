//! MCP manager modal — the interactive MCP-server-list surface.
//!
//! Distinct from [`super::session`] (the read-only MODEL/MCP/SKILLS dashboard)
//! and [`super::tools`] (the per-tool toggle), this is the centered, dismissable
//! overlay opened via the `/mcp` slash command. It lists every configured MCP
//! server with its connection status (connected / disabled / failed) and tool
//! count, with per-row actions: `Space` connects/disconnects the server for the
//! session, and `r` reconnects it. Data comes from the session-context
//! snapshot's `mcp` pane (the same snapshot `/session` and `/tools` use).

use nuotc::{
    Frame, Style, {Line, Span},
};
use unicode_width::UnicodeWidthStr;

use super::common::{placeholder, truncate_ellipsis};
use crate::components::list::{SelectableListPage, draw_selectable_list_page, row_style};
use crate::components::modal::{ModalHeader, modal_body_width};
use crate::primitives::{ContentModalSpec, FooterHint, keyvocab};
use crate::render::Theme;

/// Draw the MCP manager modal: a centered, dismissable, selectable list of the
/// configured MCP servers. Each row shows a status glyph, the server name, a
/// status detail (`N tools` / `disabled` / `failed: …`), and an `[on]`/`[off]`
/// badge. `Space` toggles the selected server; `r` reconnects it. The harness
/// replies with a fresh snapshot that re-renders the list.
pub fn draw_mcp_modal(
    frame: &mut Frame,
    session_context: Option<&nuo_contracts::SessionContextSnapshot>,
    modal_index: usize,
    scroll: &mut usize,
    follow_selection: bool,
    theme: &Theme,
) -> nuotc::Rect {
    // Width is independent of height: probe a full-height rect for the content
    // width, build the list, then size the panel to the content (clamped).
    let body_width = modal_body_width(frame, ContentModalSpec::MCP);

    let servers = session_context.map(|s| s.mcp.as_slice()).unwrap_or(&[]);

    let mut body: Vec<Line> = Vec::new();
    let mut selected_line: Option<usize> = None;

    if session_context.is_none() {
        body.push(placeholder("Loading MCP servers…", false, theme.muted()));
    } else if servers.is_empty() {
        body.push(placeholder(
            "No MCP servers configured.",
            true,
            theme.muted(),
        ));
    } else {
        const GUTTER_W: usize = 2;
        const PREFIX_W: usize = GUTTER_W + 2; // gutter + "glyph "
        let name_col = servers
            .iter()
            .map(|s| s.name.width())
            .max()
            .unwrap_or(0)
            .clamp(8, 24);
        let badge_w = "[off]".width();
        let detail_budget = body_width
            .saturating_sub(PREFIX_W + name_col + badge_w + 4)
            .max(1);

        for (i, server) in servers.iter().enumerate() {
            let is_sel = i == modal_index;
            let style = row_style(is_sel, theme);

            // Glyph + status detail derive from the connection tri-state. A
            // disabled server reads "off"; connected and failed are both
            // enabled intents, distinguished by glyph and detail.
            let (glyph, glyph_color, state, detail) = if server.disabled {
                ("○", theme.muted(), "off", "disabled".to_string())
            } else if server.connected {
                (
                    "●",
                    theme.ok(),
                    "on",
                    format!(
                        "{} tool{}",
                        server.tool_names.len(),
                        if server.tool_names.len() == 1 {
                            ""
                        } else {
                            "s"
                        }
                    ),
                )
            } else {
                (
                    "✕",
                    theme.err(),
                    "on",
                    format!(
                        "failed: {}",
                        server.failure.as_deref().unwrap_or("not connected")
                    ),
                )
            };
            let glyph_color = if is_sel { style.fg } else { glyph_color };

            let name = truncate_ellipsis(&server.name, name_col);
            let detail = truncate_ellipsis(&detail, detail_budget);
            let badge = format!("[{state}]");
            let left_w = GUTTER_W + 2 + name_col + 2 + detail.width();
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
                Span::styled(detail, Style::default().bg(style.bg).fg(style.dim)),
                Span::styled(" ".repeat(pad), Style::default().bg(style.bg)),
                Span::styled(badge, Style::default().bg(style.bg).fg(style.dim)),
            ]));
        }
    }

    let has_servers = session_context.map(|s| !s.mcp.is_empty()).unwrap_or(false);
    draw_selectable_list_page(
        frame,
        SelectableListPage {
            geometry: ContentModalSpec::MCP,
            header: ModalHeader::title("MCP servers"),
            lines: body,
            scroll,
            selected_line,
            follow_selection,
            has_items: has_servers,
            item_footer_hints: &[
                FooterHint::navigation(keyvocab::ARROWS_UD, "select"),
                FooterHint::primary(keyvocab::SPACE, "toggle"),
                FooterHint::primary("r", "reconnect"),
                FooterHint::key_always(crate::keymap::Key::ESC, "close"),
            ],
            empty_footer_hints: &[FooterHint::key_always(crate::keymap::Key::ESC, "close")],
            extra_footer_hints: &[],
        },
        theme,
    )
}
