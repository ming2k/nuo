//! Authoritative Unified Command Palette (Ctrl+L).
//!
//! Merges Quick Switcher, Which-Key, Actions menu, surface navigation, settings,
//! and rare administrative commands into one searchable, keyboard-first modal.

use nuotc::{
    Color, Frame, Modifier, Rect, Style, {Line, Paragraph, Span},
};
use unicode_width::UnicodeWidthStr;

use crate::components::selectable_body::{SelectableRow, render_selectable_body};
use crate::fuzzy::fuzzy_match;
use crate::keymap::{AppContext, Availability, COMMAND_REGISTRY, CommandId, DangerLevel};
use crate::model::layout::LayoutMap;
use crate::model::selection::SelectionState;
use crate::primitives::{
    FixedModalSpec, FooterHint, modal_area, modal_frame, modal_header, render_modal_footer,
};
use crate::render::Theme;

/// Target action for a command palette entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PaletteAction {
    Client(CommandId),
    Harness { slash: String, requires_args: bool },
}

/// One selectable entry in the Command Palette list.
#[derive(Debug, Clone)]
pub(crate) struct PaletteEntry {
    pub label: String,
    pub hint: String,
    pub slash: Option<String>,
    pub description: String,
    pub danger: DangerLevel,
    pub availability: Availability,
    #[allow(dead_code)]
    pub is_recent: bool,
    pub score: i64,
    pub action: PaletteAction,
}

fn humanize_command_name(name: &str) -> String {
    match name.trim() {
        "/compact" => "Compact Conversation".to_string(),
        "/new" => "New Session".to_string(),
        "/unattended" => "Unattended Mode".to_string(),
        "/auto" => "Unattended Mode (alias)".to_string(),
        "/autopilot" => "Unattended Mode (alias)".to_string(),
        "/delegate" => "Unattended Mode (legacy alias)".to_string(),
        "/yolo" => "Unattended Mode (legacy alias)".to_string(),
        "/unconfine" => "Workspace Confinement".to_string(),
        "/unconfined" => "Workspace Confinement".to_string(),
        "/jail" => "Workspace Confinement (legacy alias)".to_string(),
        "/role" => "Agent Role".to_string(),
        "/search" => "Search Session History".to_string(),
        "/fork" => "Fork Session".to_string(),
        "/diff" => "Workspace Diff".to_string(),
        "/undo" => "Undo Turn".to_string(),
        "/jobs" => "Background Jobs".to_string(),
        "/init" => "Initialize Project Config".to_string(),
        "/trust" => "Trust Project Assets".to_string(),
        "/untrust" => "Revoke Asset Trust".to_string(),
        "/export" => "Export Conversation".to_string(),
        "/debug" => "Debug Tracing".to_string(),
        "/retry" => "Retry Request".to_string(),
        other => {
            let bare = other.trim_start_matches('/');
            let mut words = Vec::new();
            for word in bare.split(['-', '_']) {
                if let Some(first) = word.chars().next() {
                    let mut s = String::new();
                    s.extend(first.to_uppercase());
                    s.push_str(&word[first.len_utf8()..]);
                    words.push(s);
                }
            }
            if words.is_empty() {
                other.to_string()
            } else {
                words.join(" ")
            }
        }
    }
}

/// Filter and rank commands for display in the Command Palette.
pub(crate) fn filter_palette_commands(
    query: &str,
    catalog: &nuo_wire::CommandCatalog,
    recent: &[String],
    ctx: &AppContext,
) -> Vec<PaletteEntry> {
    let clean_query = query.trim();

    let mut entries = Vec::new();

    // 1. Client-side navigation and app commands from COMMAND_REGISTRY
    for spec in COMMAND_REGISTRY {
        let avail = (spec.availability)(ctx);
        let is_recent = recent
            .iter()
            .any(|r| r == spec.label || spec.slash.is_some_and(|s| s == r));

        let entry = PaletteEntry {
            label: spec.label.to_string(),
            hint: spec.hint.to_string(),
            slash: spec.slash.map(|s| s.to_string()),
            description: spec.description.to_string(),
            danger: spec.danger,
            availability: avail,
            is_recent,
            score: if is_recent { 1000 } else { 0 },
            action: PaletteAction::Client(spec.id),
        };

        if clean_query.is_empty() {
            entries.push(entry);
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

    // 2. Harness commands from catalog
    for cmd in &catalog.commands {
        // Skip commands that already have a dedicated client UI entry
        if COMMAND_REGISTRY
            .iter()
            .any(|s| s.slash == Some(cmd.name.as_str()))
        {
            continue;
        }

        let is_recent = recent
            .iter()
            .any(|r| r == &cmd.name || r == cmd.name.trim_start_matches('/'));
        let can_run_bare =
            cmd.usage.is_empty() || cmd.usage.iter().any(|u| u.trim() == cmd.name.trim());
        let requires_args = !can_run_bare;

        let avail = if ctx.has_overlay {
            Availability::Unavailable("overlay active")
        } else {
            Availability::Available
        };

        let danger = match cmd.name.as_str() {
            "/new" | "/undo" | "/untrust" => DangerLevel::Cautious,
            _ => DangerLevel::Safe,
        };

        let human_label = humanize_command_name(&cmd.name);

        let entry = PaletteEntry {
            label: human_label,
            hint: cmd.name.clone(),
            slash: Some(cmd.name.clone()),
            description: cmd.summary.clone(),
            danger,
            availability: avail,
            is_recent,
            score: if is_recent { 1000 } else { 0 },
            action: PaletteAction::Harness {
                slash: cmd.name.clone(),
                requires_args,
            },
        };

        if clean_query.is_empty() {
            entries.push(entry);
        } else {
            let keywords = cmd.intent_keywords.join(" ");
            let match_target = format!(
                "{} {} {} {} {}",
                entry.label,
                entry.hint,
                entry.slash.as_deref().unwrap_or(""),
                entry.description,
                keywords
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
            _ => b.score.cmp(&a.score).then_with(|| a.label.cmp(&b.label)),
        }
    });

    entries
}

/// Properties for rendering the Command Palette modal.
pub(crate) struct CommandPaletteProps<'a> {
    pub query: &'a str,
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

    let query_spans = vec![
        Span::styled(
            "> ",
            Style::default()
                .fg(theme.brand())
                .add_modifier(Modifier::BOLD),
        ),
        if query.is_empty() {
            Span::styled(
                "Type a command, slash trigger, or surface name...",
                Style::default().fg(theme.muted()),
            )
        } else {
            Span::styled(
                query,
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            )
        },
    ];
    frame.render_widget(Paragraph::new(Line::from(query_spans)), query_line_rect);

    if show_caret {
        let cursor_x = (query_line_rect.x + 2 + query.width() as u16)
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
        Paragraph::new(Line::from(vec![Span::styled(
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

            let gutter = if is_sel { "▶ " } else { "  " };
            let gutter_style = if is_sel {
                Style::default()
                    .fg(theme.brand())
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.muted())
            };

            let label_style = if !avail {
                Style::default().fg(theme.muted())
            } else if is_sel {
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.fg())
            };

            let mut left_spans = vec![
                Span::styled(gutter, gutter_style),
                Span::styled(entry.label.clone(), label_style),
            ];

            if entry.danger == DangerLevel::Dangerous {
                left_spans.push(Span::raw(" "));
                left_spans.push(Span::styled(
                    "[DANGER]",
                    Style::default()
                        .fg(theme.err())
                        .add_modifier(Modifier::BOLD),
                ));
            } else if entry.danger == DangerLevel::Cautious {
                left_spans.push(Span::raw(" "));
                left_spans.push(Span::styled("[CAUTION]", Style::default().fg(theme.warn())));
            }

            let right_text = match entry.availability {
                Availability::Available => entry.hint.clone(),
                Availability::Unavailable(reason) => reason.to_string(),
            };

            let right_style = if !avail {
                Style::default().fg(theme.muted())
            } else if is_sel {
                Style::default().fg(theme.brand())
            } else {
                Style::default().fg(theme.muted())
            };

            let left_w: usize = left_spans.iter().map(|s| s.content.width()).sum();
            let right_w = right_text.width();

            let row_line = if body_w >= left_w + right_w + 3 {
                let gap = body_w - left_w - right_w - 2;
                let mut spans = left_spans;
                spans.push(Span::raw(" ".repeat(gap)));
                spans.push(Span::styled(right_text, right_style));
                Line::from(spans)
            } else {
                Line::from(left_spans)
            };

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

    fn sample_catalog() -> nuo_wire::CommandCatalog {
        nuo_client::command_catalog(&[("/custom-check".into(), "Custom health check".into())])
    }

    #[test]
    fn command_palette_includes_both_client_and_harness_commands() {
        let catalog = sample_catalog();
        let ctx = AppContext::default();
        let entries = filter_palette_commands("", &catalog, &[], &ctx);

        // Client command present
        assert!(
            entries
                .iter()
                .any(|e| matches!(e.action, PaletteAction::Client(CommandId::OpenModels)))
        );
        assert!(
            entries
                .iter()
                .any(|e| matches!(e.action, PaletteAction::Client(CommandId::NavigateSettings)))
        );

        // Harness commands present
        assert!(
            entries
                .iter()
                .any(|e| e.slash.as_deref() == Some("/compact"))
        );
        assert!(entries.iter().any(|e| e.slash.as_deref() == Some("/undo")));
        assert!(
            entries
                .iter()
                .any(|e| e.slash.as_deref() == Some("/custom-check"))
        );
    }

    #[test]
    fn no_duplicate_between_client_registry_and_catalog() {
        let catalog = sample_catalog();
        let ctx = AppContext::default();
        let entries = filter_palette_commands("", &catalog, &[], &ctx);

        // /models is in COMMAND_REGISTRY as Client(OpenModels)
        let model_entries: Vec<_> = entries
            .iter()
            .filter(|e| e.slash.as_deref() == Some("/models"))
            .collect();
        assert_eq!(
            model_entries.len(),
            1,
            "There must be exactly one /models entry in the palette"
        );
        assert!(matches!(
            model_entries[0].action,
            PaletteAction::Client(CommandId::OpenModels)
        ));
    }

    #[test]
    fn harness_command_args_requirement_is_detected() {
        let catalog = sample_catalog();
        let ctx = AppContext::default();
        let entries = filter_palette_commands("", &catalog, &[], &ctx);

        let compact = entries
            .iter()
            .find(|e| e.slash.as_deref() == Some("/compact"))
            .unwrap();
        assert!(matches!(
            compact.action,
            PaletteAction::Harness {
                requires_args: false,
                ..
            }
        ));
    }

    #[test]
    fn fuzzy_search_filters_and_scores_catalog_and_client_commands() {
        let catalog = sample_catalog();
        let ctx = AppContext::default();

        let compact_matches = filter_palette_commands("compact", &catalog, &[], &ctx);
        assert!(!compact_matches.is_empty());
        assert_eq!(compact_matches[0].slash.as_deref(), Some("/compact"));

        let custom_matches = filter_palette_commands("custom-check", &catalog, &[], &ctx);
        assert!(!custom_matches.is_empty());
        assert_eq!(custom_matches[0].slash.as_deref(), Some("/custom-check"));
    }

    #[test]
    fn dead_commands_are_purged_from_palette() {
        let catalog = sample_catalog();
        let ctx = AppContext::default();
        let entries = filter_palette_commands("", &catalog, &[], &ctx);

        assert!(
            !entries
                .iter()
                .any(|e| e.label.contains("Scroll Transcript"))
        );
        assert!(!entries.iter().any(|e| e.label.contains("Insert Newline")));
        assert!(!entries.iter().any(|e| e.label.contains("Reconnect MCP")));
        assert!(!entries.iter().any(|e| e.label.contains("Toggle Tool")));
    }
}
