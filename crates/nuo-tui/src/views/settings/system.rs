//! System settings panel: config file paths, environment info, and runtime metrics.

use nuotc::{Frame, Line, Modifier, Rect, Span, Style};

use super::{ScrollableRects, SettingsProps, render_scrollable_indexed};

/// Count of items in the System settings panel.
pub fn item_count() -> usize {
    5
}

pub(super) fn draw_system_detail(
    frame: &mut Frame,
    body: Rect,
    props: &mut SettingsProps<'_>,
    _focused: bool,
) -> ScrollableRects {
    let mut lines: Vec<Line<'static>> = Vec::new();

    let items = [
        ("Config File", "~/.config/nuo/config.toml"),
        ("Credentials", "~/.config/nuo/credentials.toml"),
        (
            "Workspace",
            if props.workspace.is_empty() {
                "(none)"
            } else {
                props.workspace
            },
        ),
        ("TUI Engine", "In-House Grid-Diff Engine (ADR-0038)"),
        ("Version", env!("CARGO_PKG_VERSION")),
    ];

    for (label, val) in items {
        lines.push(Line::from(vec![
            Span::raw("   "),
            Span::styled(
                format!("{:<18}", label),
                Style::default().fg(props.theme.muted()),
            ),
            Span::styled(
                val.to_string(),
                Style::default()
                    .fg(props.theme.fg())
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
        lines.push(Line::from(""));
    }

    render_scrollable_indexed(
        frame,
        body,
        lines,
        props.detail_scroll,
        None,
        &[],
        props.theme,
    )
}
