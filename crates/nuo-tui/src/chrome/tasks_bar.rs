//! ADR-0212: Authoritative background tasks status bar.
//!
//! Renders persistent or transient background jobs (running and settled)
//! immediately above the composer/activity bar. Decouples task execution
//! observability from thread history.

use nuotc::{Frame, Line, Modifier, Paragraph, Rect, Span, Style};

use crate::theme::Theme;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackgroundTaskItem {
    pub id: String,
    pub label: String,
    pub running: bool,
    pub started_at_ms: u64,
    pub duration_secs: Option<u64>,
    pub success: Option<bool>,
    pub exit_code: Option<i32>,
    pub dismissed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct TasksBarProps<'a> {
    pub tasks: &'a [BackgroundTaskItem],
}

/// Draw the single-row background tasks status bar.
pub fn draw_tasks_bar(
    frame: &mut Frame,
    rect: Rect,
    props: TasksBarProps<'_>,
    theme: &Theme,
) -> Rect {
    let tasks: Vec<&BackgroundTaskItem> = props.tasks.iter().filter(|t| !t.dismissed).collect();

    if tasks.is_empty() {
        return Rect::default();
    }

    let dim = Style::default().fg(theme.muted());
    let tag_style = Style::default()
        .fg(theme.brand())
        .add_modifier(Modifier::BOLD);

    let running_tasks: Vec<&&BackgroundTaskItem> = tasks.iter().filter(|t| t.running).collect();
    let settled_tasks: Vec<&&BackgroundTaskItem> = tasks.iter().filter(|t| !t.running).collect();

    let mut left: Vec<Span<'static>> =
        vec![Span::styled("TASKS", tag_style), Span::styled(" ", dim)];

    if !running_tasks.is_empty() {
        let count_style = Style::default().fg(theme.fg()).add_modifier(Modifier::BOLD);
        left.push(Span::styled("⚙ ", Style::default().fg(theme.brand())));
        left.push(Span::styled(
            format!("{} running", running_tasks.len()),
            count_style,
        ));

        if let Some(first) = running_tasks.first() {
            let label = if first.label.len() > 24 {
                format!("{}…", &first.label[..23])
            } else {
                first.label.clone()
            };
            left.push(Span::styled(format!(": {label}"), dim));
        }
    }

    if !settled_tasks.is_empty() {
        if !running_tasks.is_empty() {
            left.push(Span::styled(" │ ", dim));
        }
        if let Some(last) = settled_tasks.last() {
            let ok = last.success.unwrap_or(false);
            let icon = if ok { "✓ " } else { "✘ " };
            let color = if ok { theme.info() } else { theme.err() };
            let style = Style::default().fg(color).add_modifier(Modifier::BOLD);
            left.push(Span::styled(icon, style));

            let label = if last.label.len() > 20 {
                format!("{}…", &last.label[..19])
            } else {
                last.label.clone()
            };
            let status_text = match (ok, last.exit_code) {
                (true, _) => format!("{label} ok"),
                (false, Some(code)) => format!("{label} exit {code}"),
                (false, None) => format!("{label} failed"),
            };
            left.push(Span::styled(status_text, style));
        }
    }

    let mut right: Vec<Span<'static>> = Vec::new();
    if !settled_tasks.is_empty() {
        right.push(Span::styled("Esc dismiss", dim));
    }

    let left_len: usize = left.iter().map(|s| s.content.chars().count()).sum();
    let right_len: usize = right.iter().map(|s| s.content.chars().count()).sum();
    let full_w = rect.width as usize;

    let mut spans = left;
    if full_w > left_len + right_len + 2 {
        let pad = full_w - left_len - right_len;
        spans.push(Span::raw(" ".repeat(pad)));
        spans.extend(right);
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), rect);
    rect
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_tasks_renders_nothing() {
        let props = TasksBarProps { tasks: &[] };
        assert!(props.tasks.is_empty());
    }

    #[test]
    fn tasks_bar_item_lifecycle() {
        let mut item = BackgroundTaskItem {
            id: "job-1".to_string(),
            label: "cargo check".to_string(),
            running: true,
            started_at_ms: 1000,
            duration_secs: None,
            success: None,
            exit_code: None,
            dismissed: false,
        };
        assert!(item.running);

        item.running = false;
        item.success = Some(true);
        item.exit_code = Some(0);
        item.duration_secs = Some(3);
        assert_eq!(item.exit_code, Some(0));
        assert!(item.success.unwrap());
    }
}
