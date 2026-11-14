use super::*;

pub(crate) fn header_content(rows: &[MonitoredThread]) -> (String, bool) {
    let needing = rows
        .iter()
        .filter(|r| {
            matches!(
                r.status,
                ThreadActivity::NeedsApproval | ThreadActivity::NeedsInput | ThreadActivity::Failed
            )
        })
        .count();
    let running = rows
        .iter()
        .filter(|r| r.status == ThreadActivity::Running)
        .count();
    let total_tokens: u64 = rows.iter().map(|r| r.output_tokens).sum();
    let summary = if rows.is_empty() {
        "no sessions ".to_string()
    } else {
        let mut parts = vec![format!("{} session(s)", rows.len())];
        if running > 0 {
            parts.push(format!("{running} running"));
        }
        if needing > 0 {
            parts.push(format!("{needing} need attention ⚠"));
        }
        if total_tokens > 0 {
            parts.push(format!("{total_tokens} tokens"));
        }
        format!("{} ", parts.join("  "))
    };
    (summary, needing > 0)
}

/// Render the Thread Fleet Table (ADR-0129 [INV-DASH-01]): a structured,
/// multi-column supervisory table that leads with authentic thread titles,
/// real-time status chips, workspace identity, active tools/turns, and token
/// counts.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_fleet_table(
    frame: &mut Frame,
    area: Rect,
    entries: &[DockEntry<'_>],
    selected: usize,
    focused: bool,
    scroll: &mut usize,
    follow: bool,
    theme: &Theme,
    current_session_id: &str,
    now: u64,
) -> Rect {
    let title = format!("Threads Fleet ({})", entries.len());
    let (panel, body) = inset_panel(frame, area, &title, focused, theme);
    let _ = panel;

    if entries.is_empty() {
        render_scrollable(
            frame,
            body,
            vec![Line::from(vec![
                Span::styled(
                    "No threads hosted on the server yet.".to_string(),
                    Style::default().fg(theme.muted()),
                ),
                Span::styled(
                    "  Press 'n' to launch one, or 'u' to inspect model usage.".to_string(),
                    Style::default().fg(theme.dim()),
                ),
            ])],
            scroll,
            None,
            theme,
        );
        return body;
    }

    if body.height <= 1 {
        return body;
    }

    // Row 0: Column header
    let header_rect = Rect { height: 1, ..body };
    let rows_rect = Rect {
        y: body.y.saturating_add(1),
        height: body.height.saturating_sub(1),
        ..body
    };

    let total_rows = entries.len();
    let visible_rows = rows_rect.height as usize;
    let follow_idx = if follow { Some(selected) } else { None };
    let (start_row, _) = resolve_scroll(
        scroll,
        visible_rows,
        total_rows,
        follow_idx,
        SCROLL_EDGE_MARGIN,
    );
    let end_row = (start_row + visible_rows).min(total_rows);

    let width = body.width as usize;
    let dim_style = Style::default()
        .fg(theme.dim())
        .add_modifier(Modifier::BOLD);

    let fixed_w = 2 + 4 + 12 + 8 + 6; // 32
    let has_tool_col = width >= 80;
    let has_ws_col = width >= 65;
    let ws_w = if has_ws_col { 12 } else { 0 };
    let tool_w = if has_tool_col { 16 } else { 0 };
    let title_w = width.saturating_sub(fixed_w + ws_w + tool_w + 4).max(12);

    let mut header_spans = vec![
        Span::styled("  ", dim_style),
        Span::styled(" #  ", dim_style),
        Span::styled(format!("{:<12}", "STATUS"), dim_style),
        Span::styled(
            format!("{:<width$}", "THREAD / MISSION", width = title_w),
            dim_style,
        ),
    ];
    if has_ws_col {
        header_spans.push(Span::styled(
            format!(" {:<width$}", "WORKSPACE", width = ws_w),
            dim_style,
        ));
    }
    if has_tool_col {
        header_spans.push(Span::styled(
            format!(" {:<width$}", "TOOL / PROGRESS", width = tool_w),
            dim_style,
        ));
    }
    header_spans.push(Span::styled(format!(" {:>7}", "TOKENS"), dim_style));
    header_spans.push(Span::styled(format!(" {:>6}", "AGE"), dim_style));

    frame.render_widget(&mut Paragraph::new(Line::from(header_spans)), header_rect);

    for (rel, idx) in (start_row..end_row).enumerate() {
        let Some(entry) = entries.get(idx) else { break };
        let row = entry.row;
        let is_selected = idx == selected;
        let is_current = row.id == current_session_id;

        let cell = Rect {
            x: rows_rect.x,
            y: rows_rect.y + rel as u16,
            width: rows_rect.width,
            height: 1,
        };

        if is_selected {
            frame.render_widget(&mut nuotc::Clear::default(), cell);
            let style = theme.selection_style(true);
            frame.render_widget(&mut RtBlock::default().style(style), cell);
        } else if is_current {
            frame.render_widget(&mut nuotc::Clear::default(), cell);
            let style = Style::default().bg(theme.panel());
            frame.render_widget(&mut RtBlock::default().style(style), cell);
        }

        let sel_marker = if is_selected {
            if !theme.elevation.is_chromatic() {
                ">"
            } else {
                "›"
            }
        } else {
            " "
        };
        let cur_marker = if is_current {
            if !theme.elevation.is_chromatic() {
                "*"
            } else {
                "●"
            }
        } else {
            " "
        };
        let (status_text, status_st) = status_chip(row.status, theme);

        let lineage = if row.parent_id.is_some() {
            match entry.parent_seq {
                Some(parent) => format!("\u{21b2}#{parent} "),
                None => "\u{21b2} ".to_string(),
            }
        } else {
            String::new()
        };

        let raw_title = thread_display_title(row);
        let title_budget = title_w.saturating_sub(lineage.chars().count());
        let truncated_title = truncate_display(raw_title, title_budget);

        let title_style = if is_selected {
            Style::default()
                .fg(theme.heading())
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.fg())
        };

        let mut row_spans = vec![
            Span::styled(
                format!("{sel_marker}{cur_marker}"),
                Style::default().fg(theme.brand()),
            ),
            Span::styled(
                format!("#{:<3}", entry.seq),
                Style::default().fg(theme.brand()),
            ),
            Span::styled(format!("{:<12}", status_text), status_st),
        ];

        if !lineage.is_empty() {
            row_spans.push(Span::styled(lineage, Style::default().fg(theme.brand())));
        }
        row_spans.push(Span::styled(
            format_padded(&truncated_title, title_budget),
            title_style,
        ));

        if has_ws_col {
            let ws_name = truncate_display(&entry.workspace, ws_w);
            row_spans.push(Span::styled(
                format!(" {:<width$}", ws_name, width = ws_w),
                Style::default().fg(theme.muted()),
            ));
        }

        if has_tool_col {
            let tool_str = if let Some(tool) = &row.current_tool {
                format!("tool: {tool}")
            } else if let Some(activity) = &row.activity {
                activity.clone()
            } else if row.round > 0 {
                format!("r{} › t{}", row.round, row.turn.unwrap_or(0))
            } else {
                "—".to_string()
            };
            let tool_disp = truncate_display(&tool_str, tool_w);
            row_spans.push(Span::styled(
                format!(" {:<width$}", tool_disp, width = tool_w),
                Style::default().fg(theme.dim()),
            ));
        }

        let tokens_str = if row.output_tokens > 0 {
            fmt_tokens_compact(row.output_tokens)
        } else {
            "—".to_string()
        };
        row_spans.push(Span::styled(
            format!(" {:>7}", tokens_str),
            Style::default().fg(theme.muted()),
        ));

        let age_str = format_uptime(now.saturating_sub(row.created_at));
        row_spans.push(Span::styled(
            format!(" {:>6}", age_str),
            Style::default().fg(theme.muted()),
        ));

        let mut line = Line::from(row_spans);
        if is_selected && theme.elevation.is_structured() {
            for span in &mut line.spans {
                span.style = span.style.add_modifier(Modifier::REVERSE);
            }
        }
        frame.render_widget(&mut Paragraph::new(line), cell);
    }

    nuo_ui_primitives::primitives::draw_scrollbar(
        frame,
        rows_rect,
        start_row,
        total_rows.saturating_sub(visible_rows),
        theme,
    );
    body
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_thread_dossier(
    frame: &mut Frame,
    area: Rect,
    row: Option<&MonitoredThread>,
    seq: usize,
    focused: bool,
    scroll: &mut usize,
    theme: &Theme,
) -> Rect {
    let title = if let Some(r) = row {
        let name = truncate_display(thread_display_title(r), 24);
        format!("Thread Dossier #{seq}: {name}")
    } else {
        "Thread Dossier".to_string()
    };
    let (_, body) = inset_panel(frame, area, &title, focused, theme);
    let lines = if let Some(r) = row {
        session_detail_lines(r, body.width as usize, theme)
    } else {
        vec![Line::from(Span::styled(
            "Select a thread from the fleet list to inspect its dossier.",
            Style::default().fg(theme.muted()),
        ))]
    };
    render_scrollable(frame, body, lines, scroll, None, theme);
    body
}

pub(crate) fn draw_model_usage_view(
    frame: &mut Frame,
    area: Rect,
    report: Option<&nuo_protocol::usage_stats::UsageStatsReport>,
    loading: bool,
    scroll: usize,
    theme: &Theme,
) -> Rect {
    let (panel, body) = inset_panel(frame, area, "Model Usage & System Analytics", true, theme);
    let _ = panel;

    let lines = if loading && report.is_none() {
        vec![
            Line::from(""),
            Line::from(vec![
                Span::styled("   ", Style::default()),
                Span::styled(
                    "Loading cross-session model usage statistics from control plane…",
                    Style::default().fg(theme.muted()),
                ),
            ]),
        ]
    } else if let Some(rep) = report {
        crate::shell::overlays::usage_stats::usage_body(rep, body.width as usize, theme)
    } else {
        vec![
            Line::from(""),
            Line::from(vec![
                Span::styled("   ", Style::default()),
                Span::styled(
                    "No model usage recorded yet — it appears after the first model request.",
                    Style::default().fg(theme.muted()),
                ),
            ]),
        ]
    };

    let mut scroll_slot = scroll;
    render_scrollable(frame, body, lines, &mut scroll_slot, None, theme);
    body
}

// shared chrome

/// Paint a panel band with a small title and return (outer, inner-body).
/// `focused` brightens the title so the active pane reads as such.
pub(crate) fn inset_panel(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    focused: bool,
    theme: &Theme,
) -> (Rect, Rect) {
    let inner = ElevationContainer::panel()
        .focused(focused)
        .render(frame, area, theme);
    // Title row at the top of the panel inner area.
    let title_rect = Rect {
        x: inner.x + 1,
        y: inner.y,
        width: inner.width.saturating_sub(1),
        height: 1,
    };
    let title_style = if focused {
        theme.focus_style(true)
    } else {
        Style::default().fg(theme.muted())
    };
    frame.render_widget(
        &mut Paragraph::new(Line::from(Span::styled(title.to_string(), title_style))),
        title_rect,
    );
    // Body: everything below the title, inset one cell on each side.
    let body = Rect {
        x: inner.x + 1,
        y: inner.y + 1,
        width: inner.width.saturating_sub(2),
        height: inner.height.saturating_sub(1),
    };
    (area, body)
}

/// Render a scrollable body (no follow / selection), clamping scroll.
pub(crate) fn render_scrollable(
    frame: &mut Frame,
    body: Rect,
    lines: Vec<Line<'static>>,
    scroll: &mut usize,
    follow: Option<usize>,
    theme: &Theme,
) {
    let visible = body.height as usize;
    let (_, max_scroll) = resolve_scroll(scroll, visible, lines.len(), follow, SCROLL_EDGE_MARGIN);
    let mut para = Paragraph::new(lines).scroll(*scroll as u16, 0);
    frame.render_widget(&mut para, body);
    nuo_ui_primitives::primitives::draw_scrollbar(frame, body, *scroll, max_scroll, theme);
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn render_footer(
    frame: &mut Frame,
    rect: Rect,
    focus: DashboardFocus,
    prompting: bool,
    prompt_create_new: bool,
    prompt_text: &str,
    cursor_position: usize,
    theme: &Theme,
    // The frame-level caret verdict: the Dashboard's inline prompt
    // borrows the composer buffer but owns the cursor through its own footer
    // band, so this flag comes from `App::caret_owner() == CaretOwner::Scene`
    // rather than from `prompting` alone.
    show_caret: bool,
    filtering: bool,
    view_usage: bool,
) {
    if rect.height == 0 || rect.width < 10 {
        return;
    }

    let bg = theme.body();
    let fill = Style::default().bg(bg);
    let key_style = nuo_ui_primitives::components::keycap::keycap_style(theme).bg(bg);
    let hint_style = fill.fg(theme.keycap_label());
    let width = rect.width as usize;

    if prompting {
        // Inline prompt inside the 3-row Subagent-style bar:
        let (label, hint) = if prompt_create_new {
            ("New thread task: ", " (Enter create  Esc cancel)")
        } else {
            ("Send task: ", " (Enter send  Esc cancel)")
        };

        let prompt_line = Line::from(vec![
            Span::styled("   ", fill),
            Span::styled(
                label.to_string(),
                Style::default()
                    .bg(bg)
                    .fg(theme.brand())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                prompt_text.to_string(),
                Style::default().bg(bg).fg(theme.fg()),
            ),
            Span::styled(hint.to_string(), Style::default().bg(bg).fg(theme.dim())),
        ]);

        let mid = rect.y + rect.height / 2;
        let blank = Line::from(Span::styled(" ".repeat(width), fill));
        for y in rect.y..rect.y + rect.height {
            let line = if y == mid {
                prompt_line.clone()
            } else {
                blank.clone()
            };
            frame.render_widget(
                &mut Paragraph::new(line),
                Rect::new(rect.x, y, rect.width, 1),
            );
        }

        if show_caret {
            let prefix_cols = "   ".width() + label.width();
            let caret_col =
                crate::shell::overlays::common::caret_column(prompt_text, cursor_position) as usize;
            let cursor_x = rect
                .x
                .saturating_add((prefix_cols + caret_col).min(u16::MAX as usize) as u16)
                .min(rect.right().saturating_sub(1));
            frame.set_cursor_position((cursor_x, mid));
        }
        return;
    }

    if filtering {
        let prompt_line = Line::from(vec![
            Span::styled("   ", fill),
            Span::styled(
                "Filter threads: ".to_string(),
                Style::default()
                    .bg(bg)
                    .fg(theme.brand())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                prompt_text.to_string(),
                Style::default().bg(bg).fg(theme.fg()),
            ),
            Span::styled(
                " (Enter apply | Esc clear)".to_string(),
                Style::default().bg(bg).fg(theme.dim()),
            ),
        ]);

        let mid = rect.y + rect.height / 2;
        let blank = Line::from(Span::styled(" ".repeat(width), fill));
        for y in rect.y..rect.y + rect.height {
            let line = if y == mid {
                prompt_line.clone()
            } else {
                blank.clone()
            };
            frame.render_widget(
                &mut Paragraph::new(line),
                Rect::new(rect.x, y, rect.width, 1),
            );
        }

        if show_caret {
            let prefix_cols = "   ".width() + "Filter threads: ".width();
            let caret_col =
                crate::shell::overlays::common::caret_column(prompt_text, cursor_position) as usize;
            let cursor_x = rect
                .x
                .saturating_add((prefix_cols + caret_col).min(u16::MAX as usize) as u16)
                .min(rect.right().saturating_sub(1));
            frame.set_cursor_position((cursor_x, mid));
        }
        return;
    }

    let pairs: Vec<(&'static str, &'static str)> = if view_usage {
        vec![
            ("u", "threads fleet"),
            ("↑/↓", "scroll"),
            ("Esc", "close"),
            ("C-x", "menu"),
        ]
    } else {
        match focus {
            DashboardFocus::List => vec![
                ("↑/↓", "select"),
                ("Tab", "switch pane"),
                ("Enter", "preview"),
                ("a", "attach"),
                ("p", "prompt"),
                ("n", "new thread"),
                ("i", "interrupt"),
                ("k", "kill"),
                ("s", "suspend"),
                ("u", "model usage"),
                ("/", "filter"),
                ("C-x", "menu"),
            ],
            DashboardFocus::Detail => vec![
                ("↑/↓", "scroll"),
                ("Tab", "switch pane"),
                ("p", "prompt"),
                ("n", "new thread"),
                ("a", "attach"),
                ("u", "model usage"),
                ("C-x", "menu"),
            ],
        }
    };

    const PAIR_GAP: usize = 3;
    const MARGIN_MIN: usize = 2;

    // Filter pairs that fit into terminal width.
    let content: Vec<(&'static str, &'static str)> = {
        let mut chosen = pairs.clone();
        loop {
            let pairs_width: usize = chosen
                .iter()
                .map(|(key, label)| key.width() + 1 + label.width())
                .sum();
            let needed = pairs_width + PAIR_GAP * chosen.len().saturating_sub(1);
            if needed <= width.saturating_sub(2 * MARGIN_MIN) || chosen.len() <= 1 {
                break;
            }
            chosen.pop();
        }
        chosen
    };

    let mut spans: Vec<Span<'static>> = Vec::new();
    for (idx, (key, label)) in content.iter().enumerate() {
        if idx > 0 {
            spans.push(Span::styled(" ".repeat(PAIR_GAP), fill));
        }
        spans.push(Span::styled(key.to_string(), key_style));
        spans.push(Span::styled(format!(" {label}"), hint_style));
    }

    let content_len: usize = content
        .iter()
        .map(|(k, l)| k.width() + 1 + l.width())
        .sum::<usize>()
        + PAIR_GAP * content.len().saturating_sub(1);

    let pad_left = (width.saturating_sub(content_len)) / 2;
    let pad_right = width.saturating_sub(pad_left + content_len);

    let mut row_spans = vec![Span::styled(" ".repeat(pad_left), fill)];
    row_spans.extend(spans);
    row_spans.push(Span::styled(" ".repeat(pad_right), fill));

    let mid = rect.y + rect.height / 2;
    let blank = Line::from(Span::styled(" ".repeat(width), fill));
    for y in rect.y..rect.y + rect.height {
        let line = if y == mid {
            Line::from(row_spans.clone())
        } else {
            blank.clone()
        };
        frame.render_widget(
            &mut Paragraph::new(line),
            Rect::new(rect.x, y, rect.width, 1),
        );
    }
}
