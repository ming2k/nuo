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
    Frame, Modifier, Style, {Line, Span},
};
use unicode_width::UnicodeWidthStr;

use super::common::{placeholder, truncate_ellipsis};
use crate::shell::render::Theme;
use nuo_ui_primitives::components::list::{
    SelectableListPage, draw_selectable_list_page, row_style,
};
use nuo_ui_primitives::components::modal::{ModalHeader, modal_body_width};
use nuo_ui_primitives::primitives::{ContentModalSpec, FooterHint, keyvocab};

/// Draw the MCP manager modal: a centered, dismissable, selectable list of the
/// configured MCP servers. Each row shows a status glyph, the server name, a
/// status detail (`N tools` / `disabled` / `failed: …`), and an `[on]`/`[off]`
/// badge. `Space` toggles the selected server; `r` reconnects it. The harness
/// replies with a fresh snapshot that re-renders the list.
pub fn draw_mcp_modal(
    frame: &mut Frame,
    session_context: Option<&nuo_protocol::ThreadContextSnapshot>,
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
                (
                    if !theme.elevation.is_chromatic() {
                        " "
                    } else {
                        "○"
                    },
                    theme.muted(),
                    "off",
                    "disabled".to_string(),
                )
            } else if server.connected {
                (
                    if !theme.elevation.is_chromatic() {
                        "*"
                    } else {
                        "●"
                    },
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
                    if !theme.elevation.is_chromatic() {
                        "x"
                    } else {
                        "✕"
                    },
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

            let mut gutter_st = Style::default().bg(style.bg);
            let mut glyph_st = Style::default().bg(style.bg).fg(glyph_color);
            let mut name_st = Style::default().bg(style.bg).fg(style.fg);
            let mut dim_st = Style::default().bg(style.bg).fg(style.dim);
            if is_sel && !theme.elevation.is_chromatic() {
                gutter_st = gutter_st.add_modifier(Modifier::REVERSE);
                glyph_st = glyph_st.add_modifier(Modifier::REVERSE | Modifier::BOLD);
                name_st = name_st.add_modifier(Modifier::REVERSE | Modifier::BOLD);
                dim_st = dim_st.add_modifier(Modifier::REVERSE);
            }

            body.push(Line::from(vec![
                Span::styled(" ".repeat(GUTTER_W), gutter_st),
                Span::styled(format!("{glyph} "), glyph_st),
                Span::styled(format!("{:<w$}  ", name, w = name_col), name_st),
                Span::styled(detail, dim_st),
                Span::styled(" ".repeat(pad), gutter_st),
                Span::styled(badge, dim_st),
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
                FooterHint::always(crate::shell::keymap::Key::ESC.display(), "close"),
            ],
            empty_footer_hints: &[FooterHint::always(
                crate::shell::keymap::Key::ESC.display(),
                "close",
            )],
            extra_footer_hints: &[],
        },
        theme,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_modal_monochrome_renders_without_unicode_dots_and_has_reverse_video() {
        let theme_mono = Theme::monochrome();
        let context = nuo_protocol::ThreadContextSnapshot {
            mcp: vec![
                nuo_protocol::McpServerInfo {
                    name: "github".to_string(),
                    connected: true,
                    disabled: false,
                    failure: None,
                    tool_names: vec!["create_issue".to_string()],
                },
                nuo_protocol::McpServerInfo {
                    name: "slack".to_string(),
                    connected: false,
                    disabled: true,
                    failure: None,
                    tool_names: vec![],
                },
            ],
            model: Default::default(),
            tools: vec![],
            permissions: vec![],
            skills: vec![],
        };
        let mut terminal = nuotc::TestTerminal::new(80, 24);
        let mut scroll = 0;
        terminal.draw(|f| {
            draw_mcp_modal(f, Some(&context), 0, &mut scroll, true, &theme_mono);
        });
        let screen: String = terminal
            .buffer()
            .cells()
            .iter()
            .map(|c| c.symbol.as_str())
            .collect();
        assert!(screen.contains("github"), "shows github: {screen}");
        assert!(
            !screen.contains('●'),
            "no unicode ● in monochrome: {screen}"
        );
        assert!(
            !screen.contains('○'),
            "no unicode ○ in monochrome: {screen}"
        );
    }
}
