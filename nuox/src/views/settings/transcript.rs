//! Transcript settings panel: Turn Band layout, auto-scroll, and disclosure settings.

use nuotc::{Frame, Line, Modifier, Rect, Span, Style};

use super::{SettingsProps, render_scrollable};

pub(super) fn draw_transcript_detail(
    frame: &mut Frame,
    body: Rect,
    props: &mut SettingsProps<'_>,
    focused: bool,
) -> Option<Rect> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut selected_line = None;

    // Item 0: Strategy
    {
        let i = 0;
        let is_sel = i == props.detail_index;
        if is_sel {
            selected_line = Some(lines.len());
        }
        let cursor = if is_sel { "›" } else { " " };
        let is_band = props.transcript_layout == crate::render::layout::Strategy::TurnBand;
        let mark = if is_band { "●" } else { "○" };
        let row_style = if is_sel && focused {
            Style::default()
                .fg(props.theme.brand())
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(props.theme.fg())
                .add_modifier(Modifier::BOLD)
        };

        lines.push(Line::from(vec![
            Span::styled(
                format!(" {cursor} {mark} "),
                Style::default().fg(if is_band {
                    props.theme.ok()
                } else if is_sel {
                    props.theme.brand()
                } else {
                    props.theme.dim()
                }),
            ),
            Span::styled("Turn Band Layout  ", row_style),
            Span::styled(
                if is_band {
                    "[ Enabled ]"
                } else {
                    "[ Disabled ]"
                },
                Style::default()
                    .fg(props.theme.brand())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(
                "Visually distinct rounded message bands with gutters (ADR-0038)",
                Style::default().fg(props.theme.muted()),
            ),
        ]));
        lines.push(Line::from(""));
    }

    // Item 1: Auto Scroll
    {
        let i = 1;
        let is_sel = i == props.detail_index;
        if is_sel {
            selected_line = Some(lines.len());
        }
        let cursor = if is_sel { "›" } else { " " };
        let is_expand = props.expand_auto_scroll;
        let mark = if is_expand { "●" } else { "○" };
        let row_style = if is_sel && focused {
            Style::default()
                .fg(props.theme.brand())
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(props.theme.fg())
                .add_modifier(Modifier::BOLD)
        };

        lines.push(Line::from(vec![
            Span::styled(
                format!(" {cursor} {mark} "),
                Style::default().fg(if is_expand {
                    props.theme.ok()
                } else if is_sel {
                    props.theme.brand()
                } else {
                    props.theme.dim()
                }),
            ),
            Span::styled("Expand Auto-Scroll", row_style),
            Span::styled(
                if is_expand {
                    "[ Enabled ]"
                } else {
                    "[ Disabled ]"
                },
                Style::default()
                    .fg(props.theme.brand())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(
                "Automatically follow new turns when expanding collapsible disclosure steps",
                Style::default().fg(props.theme.muted()),
            ),
        ]));
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
