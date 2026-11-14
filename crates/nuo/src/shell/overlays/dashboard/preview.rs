use super::*;

pub(crate) fn status_style(status: ThreadActivity, theme: &Theme) -> Style {
    match status {
        // A running thread is *live*, not *identity*: it uses the in-flight hue
        // like every other running surface (ADR-0110 `[INV-TUI-RUNNING-01]`).
        ThreadActivity::Running => Style::default().fg(theme.running()),
        ThreadActivity::NeedsApproval | ThreadActivity::NeedsInput => {
            Style::default().fg(theme.warn())
        }
        ThreadActivity::Failed => Style::default().fg(theme.err()),
        ThreadActivity::Idle | ThreadActivity::Interrupted => Style::default().fg(theme.muted()),
    }
}

fn format_elapsed(ms: u64) -> String {
    let secs = ms / 1000;
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m{}s", secs / 60, secs % 60)
    } else {
        format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
    }
}

/// The cognitive mission dossier and full telemetry for one session, shared
/// by the console's detail view and the preview modal. `width` is the render
/// width for wrapping text.
pub(crate) fn session_detail_lines(
    row: &MonitoredThread,
    width: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let mut lines: Vec<Line> = Vec::new();
    let dim = Style::default().fg(theme.dim());
    let fg = Style::default().fg(theme.fg());
    let brand = Style::default().fg(theme.brand());
    let muted = Style::default().fg(theme.muted());

    // 1. Cognitive Mission & Intent
    if let Some(digest) = &row.digest {
        if !digest.title.is_empty() {
            lines.push(Line::from(vec![
                Span::styled("Mission:   ", dim.add_modifier(Modifier::BOLD)),
                Span::styled(digest.title.clone(), brand.add_modifier(Modifier::BOLD)),
            ]));
        }
        if !digest.intent.is_empty() {
            lines.push(Line::from(vec![
                Span::styled("Intent:    ", dim),
                Span::styled(digest.intent.clone(), fg),
            ]));
        }
        if !digest.history.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "Milestones & Progress:",
                dim.add_modifier(Modifier::BOLD),
            )));
            for entry in &digest.history {
                lines.push(Line::from(vec![
                    Span::styled("  • ", brand),
                    Span::styled(entry.clone(), fg),
                ]));
            }
        }
    } else if !row.overview.is_empty() {
        lines.push(Line::from(Span::styled(
            "Overview:",
            dim.add_modifier(Modifier::BOLD),
        )));
        let wrap_w = width.saturating_sub(1).max(8);
        for chunk in wrap_text(&row.overview, wrap_w) {
            lines.push(Line::from(Span::styled(format!("  {chunk}"), muted)));
        }
    }

    // 2. Telemetry & Runtime Status
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Telemetry:",
        dim.add_modifier(Modifier::BOLD),
    )));

    let mut field = |label: &str, value: String, style: Style| {
        lines.push(Line::from(vec![
            Span::styled(format!("  {label:<12}"), dim),
            Span::styled(value, style),
        ]));
    };

    field("session", row.id.clone(), fg);
    if !row.project_root.is_empty() {
        field("workspace", row.project_root.clone(), fg);
    }
    field(
        "status",
        row.status.as_str().to_string(),
        status_style(row.status, theme).add_modifier(Modifier::BOLD),
    );
    field(
        "hosting",
        match row.hosting {
            ThreadHosting::Hosted => "hosted".to_string(),
            ThreadHosting::Dormant => "dormant".to_string(),
        },
        fg,
    );
    let round = match row.turn {
        Some(t) => format!("round {} › turn {}", row.round, t),
        None => format!("round {}", row.round),
    };
    field("progress", round, fg);
    field("output", format!("{} tokens", row.output_tokens), fg);
    field(
        "uptime",
        format_uptime(now_unix_secs().saturating_sub(row.created_at)),
        fg,
    );
    field("elapsed", format_elapsed(row.elapsed_ms), fg);
    if let Some(ctx) = row.context_tokens {
        field("context", format!("{ctx} tokens"), fg);
    }
    if let Some(tool) = &row.current_tool {
        field("tool", tool.clone(), brand.add_modifier(Modifier::BOLD));
    }
    if let Some(activity) = &row.activity {
        field("activity", activity.clone(), fg);
    }
    if let Some(note) = &row.note {
        field(
            "note",
            note.clone(),
            Style::default()
                .fg(theme.warn())
                .add_modifier(Modifier::BOLD),
        );
    }
    field("messages", row.message_count.to_string(), fg);

    lines
}

/// The session preview modal: a centered, read-only drill-in
/// on one session, opened by Enter on a dock selection. Selection alone
/// never opens it; Esc closes. Read-only — attaching is `a` on the dock.
pub fn draw_session_preview(
    frame: &mut Frame,
    row: Option<&MonitoredThread>,
    scroll: &mut usize,
    theme: &Theme,
) {
    let area = nuo_ui_primitives::primitives::centered_rect(70, 75, frame.area());
    // Occlude the dashboard beneath and paint the panel.
    frame.render_widget(&mut nuotc::Clear::default(), area);
    let inner = ElevationContainer::overlay().render(frame, area, theme);

    // Vertical chrome: title / gap / body / gap / footer-hint.
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(inner);
    let title = chunks[0];
    let body = chunks[2];
    let footer = chunks[4];

    let mut title_spans = vec![Span::styled(
        " Mission Preview".to_string(),
        Style::default()
            .fg(theme.brand())
            .add_modifier(Modifier::BOLD),
    )];
    if let Some(r) = row {
        title_spans.push(Span::styled(
            format!(
                "  —  #{} ({})",
                r.id.chars().take(8).collect::<String>(),
                r.status.as_str()
            ),
            Style::default().fg(theme.muted()),
        ));
    }
    frame.render_widget(&mut Paragraph::new(Line::from(title_spans)), title);

    let lines = match row {
        Some(r) => session_detail_lines(r, body.width as usize, theme),
        None => vec![Line::from(Span::styled(
            "Session is no longer on the server.".to_string(),
            Style::default().fg(theme.muted()),
        ))],
    };
    render_scrollable(frame, body, lines, scroll, None, theme);

    let hint = Line::from(vec![
        Span::styled(" ↑↓".to_string(), Style::default().fg(theme.brand())),
        Span::styled(" scroll   ".to_string(), Style::default().fg(theme.muted())),
        Span::styled("Esc".to_string(), Style::default().fg(theme.brand())),
        Span::styled(" close".to_string(), Style::default().fg(theme.muted())),
    ]);
    frame.render_widget(&mut Paragraph::new(hint), footer);
}

/// Greedy word-wrap into `width` columns (character count; good enough for a
/// preview pane — wide glyphs are not split mid-codepoint).
fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let wlen = word.chars().count();
        let cur = line.chars().count();
        if cur > 0 && cur + 1 + wlen > width {
            out.push(std::mem::take(&mut line));
            line.push_str(word);
        } else {
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
        }
    }
    if !line.is_empty() {
        out.push(line);
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}
