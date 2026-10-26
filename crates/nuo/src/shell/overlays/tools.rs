//! Tools inspector and management modal.
//!
//! Opened via the `/tools` slash command, this overlay provides a clean, two-tier
//! interface to inspect and manage tools available to the live session:
//! - Level 1: A unified, selectable tool palette with instant per-row toggle (`Space`),
//!   alphabetical order, and zero artificial category noise.
//! - Level 2: A comprehensive tool inspector (`Enter`) presenting capability flags,
//!   safety risk profile, human approval requirements, multi-line descriptions,
//!   and structured Draft-07 JSON Schema parameter definitions.

use nuotc::{
    Frame, Style, {Line, Span},
};
use unicode_width::UnicodeWidthStr;

use super::common::{placeholder, truncate_ellipsis};
use crate::shell::render::Theme;
use nuo_ui_primitives::components::list::{
    SelectableListPage, draw_selectable_list_page, row_style,
};
use nuo_ui_primitives::components::modal::{ModalHeader, modal_body_width};
use nuo_ui_primitives::primitives::{ContentModalSpec, FooterHint, breadcrumb_parts, keyvocab};

/// Draw the tools manager modal: a centered, dismissable, selectable list of
/// the session's tools (Level 1), or the tool detail sub-view (Level 2).
#[allow(clippy::too_many_arguments)]
pub fn draw_tools_modal(
    frame: &mut Frame,
    session_context: Option<&nuo_wire::SessionContextSnapshot>,
    modal_index: usize,
    scroll: &mut usize,
    follow_selection: bool,
    info_detail: bool,
    info_scroll: &mut usize,
    theme: &Theme,
) -> nuotc::Rect {
    let body_width = modal_body_width(frame, ContentModalSpec::TOOLS);
    let tools = session_context.map(|s| s.tools.as_slice()).unwrap_or(&[]);

    if info_detail && !tools.is_empty() {
        let tool = &tools[modal_index.min(tools.len().saturating_sub(1))];
        let breadcrumbs = breadcrumb_parts("Tools", &tool.name);
        let lines = render_tool_detail(tool, body_width, theme);
        return draw_selectable_list_page(
            frame,
            SelectableListPage {
                geometry: ContentModalSpec::TOOLS,
                header: ModalHeader::parts(&breadcrumbs),
                lines,
                scroll: info_scroll,
                selected_line: None,
                follow_selection: false,
                has_items: true,
                item_footer_hints: &[
                    FooterHint::navigation(keyvocab::ARROWS_UD, "scroll"),
                    FooterHint::primary(keyvocab::SPACE, "toggle"),
                    FooterHint::always(crate::shell::keymap::Key::ESC.display(), "list"),
                ],
                empty_footer_hints: &[FooterHint::always(
                    crate::shell::keymap::Key::ESC.display(),
                    "list",
                )],
                extra_footer_hints: &[],
            },
            theme,
        );
    }

    let mut body: Vec<Line> = Vec::new();
    let mut selected_line: Option<usize> = None;

    if session_context.is_none() {
        body.push(placeholder("No tools available.", false, theme.muted()));
    } else if tools.is_empty() {
        body.push(placeholder("No tools available.", true, theme.muted()));
    } else {
        const GUTTER_W: usize = 2;
        const PREFIX_W: usize = GUTTER_W + 2; // gutter + "glyph "
        let name_col = tools
            .iter()
            .map(|t| t.name.width())
            .max()
            .unwrap_or(0)
            .clamp(8, 28);
        let badge_w = "[off]".width();
        let desc_budget = body_width
            .saturating_sub(PREFIX_W + name_col + badge_w + 4)
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
            let desc = truncate_ellipsis(&tool.description, desc_budget);
            let badge = format!("[{state}]");

            let left_w = GUTTER_W
                + 2 // glyph + space
                + name_col
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
                Span::styled(desc, Style::default().bg(style.bg).fg(style.dim)),
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
                FooterHint::primary(crate::shell::keymap::Key::ENTER.display(), "details"),
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

fn render_tool_detail(
    tool: &nuo_wire::ToolInfo,
    body_width: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();

    let status_glyph = if tool.enabled { "●" } else { "○" };
    let status_color = if tool.enabled {
        theme.ok()
    } else {
        theme.muted()
    };
    let status_text = if tool.enabled { "Enabled" } else { "Disabled" };

    lines.push(Line::from(vec![
        Span::styled("Status:      ", Style::default().fg(theme.muted())),
        Span::styled(
            format!("{status_glyph} {status_text}"),
            Style::default().fg(status_color),
        ),
        Span::styled("  (Space to toggle)", Style::default().fg(theme.muted())),
    ]));

    if let Some(risk) = &tool.risk {
        let risk_color = match risk.as_str() {
            "read_only" => theme.ok(),
            "destructive" | "arbitrary_execution" => theme.err(),
            _ => theme.warn(),
        };
        lines.push(Line::from(vec![
            Span::styled("Risk:        ", Style::default().fg(theme.muted())),
            Span::styled(risk.clone(), Style::default().fg(risk_color)),
        ]));
    }

    if tool.requires_user {
        lines.push(Line::from(vec![
            Span::styled("Approval:    ", Style::default().fg(theme.muted())),
            Span::styled(
                "Required (prompts before execution)",
                Style::default().fg(theme.warn()),
            ),
        ]));
    }

    if tool.requires_vision {
        lines.push(Line::from(vec![
            Span::styled("Vision:      ", Style::default().fg(theme.muted())),
            Span::styled(
                "Required (multimodal image capability)",
                Style::default().fg(theme.info()),
            ),
        ]));
    }

    if tool.source != "builtin" && !tool.source.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("Provider:    ", Style::default().fg(theme.muted())),
            Span::styled(tool.source.clone(), Style::default().fg(theme.fg())),
        ]));
    }

    lines.push(Line::from(""));

    lines.push(Line::from(Span::styled(
        "Description",
        Style::default()
            .fg(theme.brand())
            .add_modifier(nuotc::Modifier::BOLD),
    )));

    let desc_wrap_w = body_width.saturating_sub(2).max(1);
    for wl in nuo_view::text_layout::wrap_text(&tool.description, desc_wrap_w) {
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(wl.text, Style::default().fg(theme.fg())),
        ]));
    }

    lines.push(Line::from(""));

    lines.push(Line::from(Span::styled(
        "Parameters",
        Style::default()
            .fg(theme.brand())
            .add_modifier(nuotc::Modifier::BOLD),
    )));

    if let Some(params_str) = &tool.parameters {
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(params_str) {
            if let Some(props) = val.get("properties").and_then(|p| p.as_object()) {
                if props.is_empty() {
                    lines.push(Line::from(Span::styled(
                        "  (No parameters required)",
                        Style::default().fg(theme.muted()),
                    )));
                } else {
                    let required_set: std::collections::HashSet<&str> = val
                        .get("required")
                        .and_then(|r| r.as_array())
                        .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
                        .unwrap_or_default();

                    for (name, spec) in props {
                        let ty = spec.get("type").and_then(|t| t.as_str()).unwrap_or("any");
                        let ty_display = if ty == "array" {
                            if let Some(item_ty) = spec
                                .get("items")
                                .and_then(|it| it.get("type"))
                                .and_then(|t| t.as_str())
                            {
                                format!("array<{item_ty}>")
                            } else {
                                "array".to_string()
                            }
                        } else {
                            ty.to_string()
                        };
                        let is_required = required_set.contains(name.as_str());
                        let req_str = if is_required { "required" } else { "optional" };
                        let req_color = if is_required {
                            theme.warn()
                        } else {
                            theme.muted()
                        };

                        let default_info = spec.get("default").map(|d| match d {
                            serde_json::Value::String(s) => format!(" [default: \"{s}\"]"),
                            other => format!(" [default: {other}]"),
                        });

                        let enum_info = spec.get("enum").and_then(|e| e.as_array()).map(|arr| {
                            let choices = arr
                                .iter()
                                .filter_map(|v| v.as_str())
                                .collect::<Vec<_>>()
                                .join(", ");
                            format!(" [options: {choices}]")
                        });

                        let mut param_spans = vec![
                            Span::raw("  • "),
                            Span::styled(
                                name.clone(),
                                Style::default()
                                    .fg(theme.fg())
                                    .add_modifier(nuotc::Modifier::BOLD),
                            ),
                            Span::raw(" "),
                            Span::styled(
                                format!("({ty_display}, "),
                                Style::default().fg(theme.muted()),
                            ),
                            Span::styled(req_str, Style::default().fg(req_color)),
                            Span::styled(")", Style::default().fg(theme.muted())),
                        ];

                        if let Some(def) = default_info {
                            param_spans.push(Span::styled(def, Style::default().fg(theme.dim())));
                        }
                        if let Some(choices) = enum_info {
                            param_spans
                                .push(Span::styled(choices, Style::default().fg(theme.dim())));
                        }

                        lines.push(Line::from(param_spans));

                        if let Some(desc) = spec.get("description").and_then(|d| d.as_str()) {
                            let param_desc_wrap = body_width.saturating_sub(6).max(1);
                            for wl in nuo_view::text_layout::wrap_text(desc, param_desc_wrap) {
                                lines.push(Line::from(vec![
                                    Span::raw("      "),
                                    Span::styled(wl.text, Style::default().fg(theme.dim())),
                                ]));
                            }
                        }
                    }
                }
            } else {
                for l in params_str.lines() {
                    lines.push(Line::from(vec![
                        Span::raw("  "),
                        Span::styled(l.to_string(), Style::default().fg(theme.dim())),
                    ]));
                }
            }
        } else {
            lines.push(Line::from(Span::styled(
                "  (Not available)",
                Style::default().fg(theme.muted()),
            )));
        }
    } else {
        lines.push(Line::from(Span::styled(
            "  (Not available)",
            Style::default().fg(theme.muted()),
        )));
    }

    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use nuotc::TestTerminal;

    fn sample_snapshot() -> nuo_wire::SessionContextSnapshot {
        nuo_wire::SessionContextSnapshot {
            model: nuo_wire::ModelInfo::default(),
            tools: vec![
                nuo_wire::ToolInfo {
                    name: "read_text".to_string(),
                    description: "Reads a text file with line numbers.".to_string(),
                    enabled: true,
                    source: "builtin".to_string(),
                    parameters: Some(
                        r#"{
                        "type": "object",
                        "properties": {
                            "path": {
                                "type": "string",
                                "description": "Path to file"
                            },
                            "limit": {
                                "type": "integer",
                                "description": "Max lines",
                                "default": 200
                            }
                        },
                        "required": ["path"]
                    }"#
                        .to_string(),
                    ),
                    risk: Some("read_only".to_string()),
                    requires_user: false,
                    requires_vision: false,
                },
                nuo_wire::ToolInfo {
                    name: "execute_command".to_string(),
                    description: "Executes a shell command.".to_string(),
                    enabled: false,
                    source: "builtin".to_string(),
                    parameters: None,
                    risk: Some("arbitrary_execution".to_string()),
                    requires_user: true,
                    requires_vision: false,
                },
                nuo_wire::ToolInfo {
                    name: "github_issues".to_string(),
                    description: "List issues".to_string(),
                    enabled: true,
                    source: "mcp:github".to_string(),
                    parameters: None,
                    risk: None,
                    requires_user: false,
                    requires_vision: false,
                },
            ],
            permissions: vec![],
            skills: vec![],
            mcp: vec![],
        }
    }

    #[test]
    fn tools_modal_level1_renders_without_category_column() {
        let theme = Theme::default();
        let snapshot = sample_snapshot();
        let mut scroll = 0;
        let mut info_scroll = 0;

        let mut term = TestTerminal::new(100, 24);
        term.draw(|frame| {
            draw_tools_modal(
                frame,
                Some(&snapshot),
                0,
                &mut scroll,
                true,
                false,
                &mut info_scroll,
                &theme,
            );
        });

        let text: String = term
            .buffer()
            .cells()
            .iter()
            .map(|c| c.symbol.as_str())
            .collect();
        assert!(text.contains("Tools"), "Header should be Tools");
        assert!(text.contains("read_text"), "Should contain read_text");
        assert!(
            text.contains("execute_command"),
            "Should contain execute_command"
        );
        assert!(text.contains("[on]"), "read_text is on");
        assert!(text.contains("[off]"), "execute_command is off");
        // Ensure "builtin" category column is not in Level 1 row
        assert!(
            !text.contains("builtin"),
            "Level 1 should not render category column"
        );
        assert!(text.contains("details"), "Footer hint should show details");
    }

    #[test]
    fn tools_modal_level2_renders_detail_view_with_breadcrumbs_and_parameters() {
        let theme = Theme::default();
        let snapshot = sample_snapshot();
        let mut scroll = 0;
        let mut info_scroll = 0;

        let mut term = TestTerminal::new(100, 24);
        term.draw(|frame| {
            draw_tools_modal(
                frame,
                Some(&snapshot),
                0,
                &mut scroll,
                true,
                true,
                &mut info_scroll,
                &theme,
            );
        });

        let text: String = term
            .buffer()
            .cells()
            .iter()
            .map(|c| c.symbol.as_str())
            .collect();
        assert!(text.contains("Tools"), "Breadcrumb parent");
        assert!(text.contains("read_text"), "Breadcrumb child tool name");
        assert!(text.contains("Status:"), "Should show status");
        assert!(text.contains("Enabled"), "read_text is enabled");
        // Native tool should not have category/source line
        assert!(
            !text.contains("Provider:"),
            "Native tool has no category line"
        );
        assert!(text.contains("Risk:"), "Should show risk");
        assert!(text.contains("read_only"), "Risk is read_only");
        assert!(text.contains("Description"), "Description section");
        assert!(
            text.contains("Reads a text file with line numbers."),
            "Full description"
        );
        assert!(text.contains("Parameters"), "Parameters section");
        assert!(text.contains("path"), "Parameter name");
        assert!(text.contains("required"), "path is required");
        assert!(text.contains("Path to file"), "path description");
        assert!(text.contains("limit"), "limit parameter");
        assert!(text.contains("optional"), "limit is optional");
        assert!(text.contains("default: 200"), "limit default value");
        assert!(
            text.contains("list"),
            "Footer should show list affordance to go back"
        );

        // Test external MCP tool shows its source
        let mut term_mcp = TestTerminal::new(100, 24);
        term_mcp.draw(|frame| {
            draw_tools_modal(
                frame,
                Some(&snapshot),
                2,
                &mut scroll,
                true,
                true,
                &mut info_scroll,
                &theme,
            );
        });
        let mcp_text: String = term_mcp
            .buffer()
            .cells()
            .iter()
            .map(|c| c.symbol.as_str())
            .collect();
        assert!(mcp_text.contains("Provider:"), "MCP tool shows Provider");
        assert!(
            mcp_text.contains("mcp:github"),
            "MCP tool shows server name"
        );
    }
}
