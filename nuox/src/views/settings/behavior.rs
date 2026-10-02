//! Behavior settings panel: click-outside dismiss and UI interaction policies.

use nuotc::{Frame, Line, Modifier, Rect, Span, Style};

use super::{SettingsProps, render_scrollable};

pub(super) fn draw_behavior_detail(
    frame: &mut Frame,
    body: Rect,
    props: &mut SettingsProps<'_>,
    focused: bool,
) -> Option<Rect> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut selected_line = None;

    // Item 0: Click Outside Dismiss
    {
        let i = 0;
        let is_sel = i == props.detail_index;
        if is_sel {
            selected_line = Some(lines.len());
        }
        let cursor = if is_sel { "›" } else { " " };
        let is_dismiss = props.click_outside_dismiss;
        let mark = if is_dismiss { "●" } else { "○" };
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
                Style::default().fg(if is_dismiss {
                    props.theme.ok()
                } else if is_sel {
                    props.theme.brand()
                } else {
                    props.theme.dim()
                }),
            ),
            Span::styled("Click-Outside Dismiss", row_style),
            Span::raw("  "),
            Span::styled(
                if is_dismiss {
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
                "Clicking outside a modal overlay automatically dismisses it",
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
