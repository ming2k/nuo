//! Appearance settings panel: themes and palette swatches.

use std::path::Path;

use nuotc::{Frame, Line, Modifier, Rect, Span, Style};

use super::{SettingsProps, render_scrollable};
use crate::render::Theme;
use crate::theme::mix;

pub(super) fn draw_appearance_detail(
    frame: &mut Frame,
    body: Rect,
    props: &mut SettingsProps<'_>,
    focused: bool,
) -> Option<Rect> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut selected_line = None;

    let ws_path = if props.workspace.is_empty() {
        None
    } else {
        Some(Path::new(props.workspace))
    };
    let schemes = Theme::available_color_schemes_with_workspace(ws_path);

    for (i, preset) in schemes.iter().enumerate() {
        let is_sel = i == props.detail_index;
        if is_sel {
            selected_line = Some(lines.len());
        }

        let is_active = props.color_scheme == preset.id;
        let cursor = if is_sel { "›" } else { " " };
        let mark = if is_active { "●" } else { "○" };

        let row_style = if is_sel && focused {
            Style::default()
                .fg(props.theme.brand())
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(props.theme.fg())
                .add_modifier(Modifier::BOLD)
        };

        let preview_theme =
            Theme::from_color_scheme_with_workspace(&preset.id, props.custom_color_scheme, ws_path);

        let c1 = preview_theme.body();
        let c2 = preview_theme.panel();
        let c3 = preview_theme.brand();
        let c4 = preview_theme.info();
        let c5 = preview_theme.ok();
        let c6 = preview_theme.warn();

        let swatch_spans = vec![
            Span::styled("█", Style::default().fg(c1)),
            Span::styled("█", Style::default().fg(c2)),
            Span::styled("█", Style::default().fg(c3)),
            Span::styled("█", Style::default().fg(c4)),
            Span::styled("█", Style::default().fg(c5)),
            Span::styled("█", Style::default().fg(c6)),
        ];

        let mut row = vec![
            Span::styled(
                format!(" {cursor} {mark} "),
                Style::default().fg(if is_active {
                    props.theme.ok()
                } else if is_sel {
                    props.theme.brand()
                } else {
                    props.theme.dim()
                }),
            ),
            Span::styled(format!("{:<22}", preset.label), row_style),
            Span::raw(" "),
        ];
        row.extend(swatch_spans);
        row.push(Span::raw("  "));
        row.push(Span::styled(
            preset.description.clone(),
            Style::default().fg(if is_sel {
                props.theme.muted()
            } else {
                mix(props.theme.muted(), props.theme.dim(), 0.5)
            }),
        ));

        lines.push(Line::from(row));
        lines.push(Line::from(""));
    }

    render_scrollable(
        frame,
        body,
        lines,
        props.detail_scroll,
        selected_line,
        props.theme,
    )
}
