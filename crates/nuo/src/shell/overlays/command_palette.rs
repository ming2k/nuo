//! Authoritative Unified Command Palette (`C-x p`).
//!
//! Merges Quick Switcher, Which-Key, Actions menu, surface navigation, settings,
//! and rare administrative commands into one searchable, keyboard-first modal.

use nuotc::{
    Color, Frame, Modifier, Rect, Style, {Line, Paragraph, Span},
};
use unicode_width::UnicodeWidthStr;

use crate::shell::keymap::{AppContext, Availability, CommandCategory, CommandId, DangerLevel};
use crate::shell::render::Theme;
use nuo_ui_primitives::components::list::row_style;
use nuo_ui_primitives::design::INPUT_PROMPT_PREFIX;
use nuo_ui_primitives::fuzzy::fuzzy_match;
use nuo_ui_primitives::primitives::{
    FixedModalSpec, FooterHint, modal_area, modal_frame, modal_header, render_modal_footer,
};
use nuo_view::components::selectable_body::{SelectableRow, render_selectable_body};
use nuo_view::model::layout::LayoutMap;
use nuo_view::model::selection::SelectionState;

/// One selectable entry in the Command Palette list.
#[derive(Debug, Clone)]
pub(crate) struct PaletteEntry {
    pub id: CommandId,
    pub label: String,
    pub category: CommandCategory,
    pub hint: String,
    pub slash: Option<String>,
    pub description: String,
    pub danger: DangerLevel,
    pub availability: Availability,
    #[allow(dead_code)]
    pub is_recent: bool,
    pub score: i64,
}

/// Filter and rank client-level commands for display in the Command Palette.
pub(crate) fn filter_palette_commands(
    query: &str,
    recent: &[String],
    ctx: &AppContext,
) -> Vec<PaletteEntry> {
    let clean_query = query.trim();

    let mut entries = Vec::new();

    // Client-side global actions and dialog navigation derived from commands_for_palette
    for (spec, avail) in crate::shell::keymap::commands_for_palette(ctx) {
        let is_recent = recent
            .iter()
            .any(|r| r == spec.label || spec.slash.is_some_and(|s| s == r));

        let entry = PaletteEntry {
            id: spec.id,
            label: spec.label.to_string(),
            category: spec.category,
            hint: spec.hint.to_string(),
            slash: spec.slash.map(|s| s.to_string()),
            description: spec.description.to_string(),
            danger: spec.danger,
            availability: avail,
            is_recent,
            score: if is_recent { 1000 } else { 0 },
        };

        if clean_query.is_empty() {
            entries.push(entry);
        } else if let Some(stripped) = clean_query.strip_prefix('@') {
            // ADR-0077: @ prefix scopes to Navigation / Settings
            if entry.category == CommandCategory::Navigate
                || entry.category == CommandCategory::Settings
            {
                let filter_str = stripped.trim();
                if filter_str.is_empty() {
                    entries.push(entry);
                } else {
                    let match_target = format!(
                        "{} {} {} {}",
                        entry.label,
                        entry.hint,
                        entry.slash.as_deref().unwrap_or(""),
                        entry.description
                    );
                    if let Some(m) = fuzzy_match(&match_target, filter_str) {
                        let mut scored_entry = entry;
                        scored_entry.score += m.score;
                        entries.push(scored_entry);
                    }
                }
            }
        } else if let Some(stripped) = clean_query.strip_prefix('>') {
            // ADR-0077: > prefix scopes to Actions / Commands
            if entry.category != CommandCategory::Navigate
                && entry.category != CommandCategory::Settings
            {
                let filter_str = stripped.trim();
                if filter_str.is_empty() {
                    entries.push(entry);
                } else {
                    let match_target = format!(
                        "{} {} {} {}",
                        entry.label,
                        entry.hint,
                        entry.slash.as_deref().unwrap_or(""),
                        entry.description
                    );
                    if let Some(m) = fuzzy_match(&match_target, filter_str) {
                        let mut scored_entry = entry;
                        scored_entry.score += m.score;
                        entries.push(scored_entry);
                    }
                }
            }
        } else if let Some(stripped) = clean_query.strip_prefix('/') {
            // ADR-0077: / prefix scopes to slash commands
            if entry.slash.is_some() {
                let filter_str = stripped.trim();
                if filter_str.is_empty() {
                    entries.push(entry);
                } else {
                    let match_target = format!(
                        "{} {} {} {}",
                        entry.label,
                        entry.hint,
                        entry.slash.as_deref().unwrap_or(""),
                        entry.description
                    );
                    if let Some(m) = fuzzy_match(&match_target, filter_str) {
                        let mut scored_entry = entry;
                        scored_entry.score += m.score;
                        entries.push(scored_entry);
                    }
                }
            }
        } else {
            let match_target = format!(
                "{} {} {} {}",
                entry.label,
                entry.hint,
                entry.slash.as_deref().unwrap_or(""),
                entry.description
            );
            if let Some(m) = fuzzy_match(&match_target, clean_query) {
                let mut scored_entry = entry;
                scored_entry.score += m.score;
                entries.push(scored_entry);
            }
        }
    }

    entries.sort_by(|a, b| {
        let a_avail = matches!(a.availability, Availability::Available);
        let b_avail = matches!(b.availability, Availability::Available);
        match (a_avail, b_avail) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => {
                if clean_query.is_empty() {
                    let cat_rank = |c: CommandCategory| match c {
                        CommandCategory::Navigate => 0,
                        CommandCategory::Settings => 1,
                        CommandCategory::Session => 2,
                        CommandCategory::Actions => 3,
                        CommandCategory::Global => 4,
                    };
                    cat_rank(a.category)
                        .cmp(&cat_rank(b.category))
                        .then_with(|| b.score.cmp(&a.score))
                        .then_with(|| a.label.cmp(&b.label))
                } else {
                    b.score.cmp(&a.score).then_with(|| a.label.cmp(&b.label))
                }
            }
        }
    });

    entries
}

/// Properties for rendering the Command Palette modal.
pub(crate) struct CommandPaletteProps<'a> {
    pub query: &'a str,
    pub cursor_position: usize,
    pub entries: &'a [PaletteEntry],
    pub selected_index: usize,
    pub scroll: &'a mut usize,
    pub show_caret: bool,
}

/// Draw the unified Command Palette modal.
pub(crate) fn draw_command_palette(
    frame: &mut Frame,
    props: CommandPaletteProps<'_>,
    theme: &Theme,
    selection: &SelectionState,
    layout_map: &mut LayoutMap,
) -> Rect {
    let CommandPaletteProps {
        query,
        cursor_position,
        entries,
        selected_index,
        scroll,
        show_caret,
    } = props;
    let outer_rect = modal_area(frame, FixedModalSpec::PROVIDER);
    let f = modal_frame(frame, outer_rect, theme, true, true);

    let title = if entries.is_empty() {
        "Commands".to_string()
    } else {
        format!("Commands ({})", entries.len())
    };
    modal_header(frame, f.header, &title, theme);

    // Search query box line at the top of body
    let query_line_rect = Rect {
        x: f.body.x,
        y: f.body.y,
        width: f.body.width,
        height: 1,
    };

    let prefix_width = INPUT_PROMPT_PREFIX.width();
    let field_width = (query_line_rect.width as usize).saturating_sub(prefix_width);
    let (offset, visible_query) =
        super::common::field_viewport(query, cursor_position, field_width);
    let caret = super::common::caret_column(query, cursor_position);
    let local = (caret as usize).saturating_sub(offset);

    let query_spans = vec![
        Span::styled(
            INPUT_PROMPT_PREFIX,
            Style::default()
                .fg(theme.brand())
                .add_modifier(Modifier::BOLD),
        ),
        if query.is_empty() {
            Span::styled(
                "Type a command, or @views, /slash, >actions...",
                Style::default().fg(theme.muted()),
            )
        } else {
            Span::styled(
                visible_query,
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            )
        },
    ];
    frame.render_widget(
        &mut Paragraph::new(Line::from(query_spans)),
        query_line_rect,
    );

    if show_caret {
        let cursor_x = (query_line_rect.x + prefix_width as u16 + local.min(field_width) as u16)
            .min(query_line_rect.right().saturating_sub(1));
        let cursor_y = query_line_rect.y;
        frame.set_cursor_position((cursor_x, cursor_y));
    }

    // Separator line
    let sep_rect = Rect {
        x: f.body.x,
        y: f.body.y + 1,
        width: f.body.width,
        height: 1,
    };
    let sep_str = "─".repeat(sep_rect.width as usize);
    frame.render_widget(
        &mut Paragraph::new(Line::from(vec![Span::styled(
            sep_str,
            Style::default().fg(theme.muted()),
        )])),
        sep_rect,
    );

    let list_rect = Rect {
        x: f.body.x,
        y: f.body.y + 2,
        width: f.body.width,
        height: f.body.height.saturating_sub(2),
    };

    let mut rows: Vec<SelectableRow> = Vec::new();

    if entries.is_empty() {
        rows.push(SelectableRow::from_line(Line::from(vec![Span::styled(
            "  No matching commands found.",
            Style::default().fg(theme.muted()),
        )])));
    } else {
        let body_w = list_rect.width as usize;

        for (i, entry) in entries.iter().enumerate() {
            let is_sel = i == selected_index;
            let avail = matches!(entry.availability, Availability::Available);
            let style = row_style(is_sel, theme);

            let gutter = "  ";
            let gutter_style = if is_sel {
                Style::default().bg(style.bg).fg(style.dim)
            } else {
                Style::default().fg(theme.muted())
            };

            let label_style = if !avail {
                if is_sel {
                    Style::default().bg(style.bg).fg(style.dim)
                } else {
                    Style::default().fg(theme.muted())
                }
            } else if is_sel {
                Style::default()
                    .bg(style.bg)
                    .fg(style.fg)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.fg())
            };

            let cat_tag = match entry.category {
                CommandCategory::Navigate => "[nav] ",
                CommandCategory::Settings => "[set] ",
                CommandCategory::Session => "[ses] ",
                CommandCategory::Actions | CommandCategory::Global => "[act] ",
            };

            let cat_style = if is_sel {
                Style::default().bg(style.bg).fg(style.dim)
            } else {
                Style::default().fg(theme.dim())
            };

            let mut left_spans = vec![
                Span::styled(gutter, gutter_style),
                Span::styled(cat_tag, cat_style),
                Span::styled(entry.label.clone(), label_style),
            ];

            if entry.danger == DangerLevel::Dangerous {
                left_spans.push(Span::styled(
                    " ",
                    Style::default().bg(if is_sel { style.bg } else { Color::Reset }),
                ));
                left_spans.push(Span::styled(
                    "[DANGER]",
                    Style::default()
                        .bg(if is_sel { style.bg } else { Color::Reset })
                        .fg(if is_sel { style.fg } else { theme.err() })
                        .add_modifier(Modifier::BOLD),
                ));
            } else if entry.danger == DangerLevel::Cautious {
                left_spans.push(Span::styled(
                    " ",
                    Style::default().bg(if is_sel { style.bg } else { Color::Reset }),
                ));
                left_spans.push(Span::styled(
                    "[CAUTION]",
                    Style::default()
                        .bg(if is_sel { style.bg } else { Color::Reset })
                        .fg(if is_sel { style.fg } else { theme.warn() }),
                ));
            }

            let right_text = match entry.availability {
                Availability::Available => entry.hint.clone(),
                Availability::Unavailable(reason) => reason.to_string(),
            };

            let right_style = if !avail {
                if is_sel {
                    Style::default().bg(style.bg).fg(style.dim)
                } else {
                    Style::default().fg(theme.muted())
                }
            } else if is_sel {
                Style::default().bg(style.bg).fg(style.dim)
            } else {
                Style::default().fg(theme.muted())
            };

            let left_w: usize = left_spans.iter().map(|s| s.content.width()).sum();
            let right_w = right_text.width();

            let row_line = if body_w >= left_w + right_w + 2 {
                let gap = body_w - left_w - right_w;
                let mut spans = left_spans;
                spans.push(Span::styled(
                    " ".repeat(gap),
                    Style::default().bg(if is_sel { style.bg } else { Color::Reset }),
                ));
                spans.push(Span::styled(right_text, right_style));
                Line::from(spans)
            } else {
                let mut spans = left_spans;
                let pad = body_w.saturating_sub(left_w);
                if pad > 0 {
                    spans.push(Span::styled(
                        " ".repeat(pad),
                        Style::default().bg(if is_sel { style.bg } else { Color::Reset }),
                    ));
                }
                Line::from(spans)
            };

            let mut row_line = row_line;
            if is_sel && theme.elevation.is_structured() {
                for span in &mut row_line.spans {
                    span.style = span.style.add_modifier(Modifier::REVERSE);
                }
            }

            rows.push(SelectableRow::from_line(row_line));
        }
    }

    render_selectable_body(
        frame,
        list_rect,
        &rows,
        scroll,
        Some(selected_index),
        theme,
        selection,
        layout_map,
    );

    if let Some(fo) = f.footer {
        let footer_hints = [
            FooterHint::navigation("↑↓", "move"),
            FooterHint::primary("Enter", "execute"),
            FooterHint::always("Esc", "close"),
        ];
        render_modal_footer(frame, fo, &footer_hints, theme);
    }

    outer_rect
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_palette_includes_client_commands() {
        let ctx = AppContext::default();
        let entries = filter_palette_commands("", &[], &ctx);

        // Client commands present
        assert!(entries.iter().any(|e| e.id == CommandId::OpenModels));
        assert!(entries.iter().any(|e| e.id == CommandId::NavigateSettings));
        assert!(entries.iter().any(|e| e.id == CommandId::OpenConnections));
        assert!(entries.iter().any(|e| e.id == CommandId::OpenPortShare));
        assert!(entries.iter().any(|e| e.id == CommandId::NavigateDashboard));
        assert!(entries.iter().any(|e| e.id == CommandId::Quit));
    }

    #[test]
    fn command_palette_excludes_scene_controls_and_slash_commands() {
        let ctx = AppContext::default();
        let entries = filter_palette_commands("", &[], &ctx);

        // Slash commands are thread-scoped operations, not workspace
        // navigation, so the client palette must not list them (ADR-0041
        // [INV-CMD-01], ADR-0119).
        assert!(
            !entries
                .iter()
                .any(|e| e.slash.as_deref() == Some("/compact"))
        );
        assert!(!entries.iter().any(|e| e.slash.as_deref() == Some("/undo")));
        assert!(!entries.iter().any(|e| e.slash.as_deref() == Some("/fork")));
        assert!(!entries.iter().any(|e| e.slash.as_deref() == Some("/diff")));

        // Scene-internal composer controls must NOT be in palette
        assert!(!entries.iter().any(|e| e.label == "Send Prompt"));
        assert!(!entries.iter().any(|e| e.label == "Queue Follow-Up"));

        // Dialog-scoped internal shortcuts must NOT be in palette
        assert!(!entries.iter().any(|e| e.label.contains("Block Model")));
        assert!(!entries.iter().any(|e| e.label.contains("Edit Connection")));
    }

    #[test]
    fn fuzzy_search_filters_and_scores_client_commands() {
        let ctx = AppContext::default();

        let port_matches = filter_palette_commands("port", &[], &ctx);
        assert!(!port_matches.is_empty());
        assert!(
            port_matches
                .iter()
                .any(|e| e.id == CommandId::OpenPortShare)
        );

        let model_matches = filter_palette_commands("model", &[], &ctx);
        assert!(!model_matches.is_empty());
        assert!(model_matches.iter().any(|e| e.id == CommandId::OpenModels));

        let conn_matches = filter_palette_commands("connections", &[], &ctx);
        assert!(!conn_matches.is_empty());
        assert!(
            conn_matches
                .iter()
                .any(|e| e.id == CommandId::OpenConnections)
        );
    }

    #[test]
    fn dead_commands_are_purged_from_palette() {
        let ctx = AppContext::default();
        let entries = filter_palette_commands("", &[], &ctx);

        assert!(
            !entries
                .iter()
                .any(|e| e.label.contains("Scroll Transcript"))
        );
        assert!(!entries.iter().any(|e| e.label.contains("Insert Newline")));
        assert!(!entries.iter().any(|e| e.label.contains("Reconnect MCP")));
        assert!(!entries.iter().any(|e| e.label.contains("Toggle Tool")));
    }

    #[test]
    fn palette_selected_row_renders_without_pointer_symbol() {
        let ctx = AppContext::default();
        let entries = filter_palette_commands("", &[], &ctx);
        let theme = Theme::default();
        let mut terminal = nuotc::TestTerminal::new(80, 24);
        let mut scroll = 0;
        let selection = SelectionState::default();
        let mut layout_map = LayoutMap::default();

        terminal.draw(|f| {
            draw_command_palette(
                f,
                CommandPaletteProps {
                    query: "",
                    cursor_position: 0,
                    entries: &entries,
                    selected_index: 0,
                    scroll: &mut scroll,
                    show_caret: false,
                },
                &theme,
                &selection,
                &mut layout_map,
            );
        });

        let content: String = terminal
            .buffer()
            .cells()
            .iter()
            .map(|c| c.symbol.as_str())
            .collect();
        assert!(
            !content.contains('▶'),
            "palette rows must not contain '▶' pointer glyph: {content}"
        );
        assert!(
            content.contains("[nav]"),
            "palette rows must render category badges: {content}"
        );
    }

    #[test]
    fn prefix_filtering_scopes_by_domain() {
        let ctx = AppContext::default();

        // @ prefix scopes strictly to Navigate / Settings
        let nav_entries = filter_palette_commands("@", &[], &ctx);
        assert!(!nav_entries.is_empty());
        assert!(nav_entries.iter().all(|e| {
            e.category == CommandCategory::Navigate || e.category == CommandCategory::Settings
        }));
        assert!(
            nav_entries
                .iter()
                .any(|e| e.id == CommandId::NavigateDashboard)
        );
        assert!(
            nav_entries
                .iter()
                .any(|e| e.id == CommandId::NavigateSettings)
        );

        let nav_filtered = filter_palette_commands("@dash", &[], &ctx);
        assert!(!nav_filtered.is_empty());
        assert_eq!(nav_filtered[0].id, CommandId::NavigateDashboard);
        assert!(nav_filtered.iter().all(|e| {
            e.category == CommandCategory::Navigate || e.category == CommandCategory::Settings
        }));

        // > prefix scopes strictly to Actions
        let action_entries = filter_palette_commands(">", &[], &ctx);
        assert!(!action_entries.is_empty());
        assert!(action_entries.iter().all(|e| {
            e.category != CommandCategory::Navigate && e.category != CommandCategory::Settings
        }));
        assert!(action_entries.iter().any(|e| e.id == CommandId::Quit));

        // / prefix scopes strictly to slash triggers
        let slash_entries = filter_palette_commands("/", &[], &ctx);
        assert!(!slash_entries.is_empty());
        assert!(slash_entries.iter().all(|e| e.slash.is_some()));
    }

    #[test]
    fn command_palette_renders_safely_on_narrow_terminal() {
        let ctx = AppContext::default();
        let entries = filter_palette_commands("", &[], &ctx);
        let theme = Theme::default();
        let mut terminal = nuotc::TestTerminal::new(40, 15);
        let mut scroll = 0;
        let selection = SelectionState::default();
        let mut layout_map = LayoutMap::default();

        terminal.draw(|f| {
            draw_command_palette(
                f,
                CommandPaletteProps {
                    query: "@",
                    cursor_position: 1,
                    entries: &entries,
                    selected_index: 0,
                    scroll: &mut scroll,
                    show_caret: true,
                },
                &theme,
                &selection,
                &mut layout_map,
            );
        });
    }
}
