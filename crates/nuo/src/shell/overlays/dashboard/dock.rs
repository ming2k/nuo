use super::*;

/// A session card's dock position: its 1-based sequence number (creation
/// order across the whole snapshot) and its disambiguated workspace name.
pub(crate) struct DockEntry<'a> {
    pub(crate) row: &'a MonitoredThread,
    pub(crate) seq: usize,
    pub(crate) workspace: String,
    pub(crate) parent_seq: Option<usize>,
}

/// The dashboard's sequence axis: indices into `rows` ordered by creation
/// (`created_at` ascending, id tiebreak so the order is total and stable).
/// The renderer and every selection-driven action (attach / interrupt /
/// prompt) must agree on this mapping — a positional selection means the
/// `#seq`-th card, not the `selected`-th snapshot row.
///
/// Ascending is deliberate here and is *not* the picker's order: `#1` is the
/// oldest session because the number is an address the console consumes
/// (`@3 …`), not a browsing order. The browsing surfaces lead with the newest
/// thread instead (ADR-0128).
pub fn creation_order(rows: &[MonitoredThread]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..rows.len()).collect();
    order.sort_by(|&a, &b| {
        (rows[a].created_at, &rows[a].id).cmp(&(rows[b].created_at, &rows[b].id))
    });
    order
}

/// Order the snapshot by creation (the sequence axis) and precompute each
/// session's display fields: `#seq` numbers and workspace names that are
/// unique-by-basename when possible, full-path-disambiguated when two
/// sessions share a directory name.
pub(crate) fn dock_entries(rows: &[MonitoredThread]) -> Vec<DockEntry<'_>> {
    let order = creation_order(rows);

    // Basename collisions → the colliding entries fall back to the full path.
    let base_names: Vec<String> = rows
        .iter()
        .map(|row| workspace_basename(&row.project_root))
        .collect();
    let mut counts = std::collections::HashMap::new();
    for name in &base_names {
        *counts.entry(name.clone()).or_insert(0usize) += 1;
    }

    let mut entries: Vec<Option<DockEntry>> = (0..rows.len()).map(|_| None).collect();
    for (seq, idx) in order.into_iter().enumerate() {
        let row = &rows[idx];
        let workspace = if row.project_root.is_empty() {
            "—".to_string()
        } else if counts.get(&base_names[idx]).copied().unwrap_or(0) > 1 {
            row.project_root.clone()
        } else {
            base_names[idx].clone()
        };
        entries[idx] = Some(DockEntry {
            row,
            seq: seq + 1,
            workspace,
            parent_seq: None,
        });
    }

    // Resolve each branch's parent to its dock sequence (the `#N` the console
    // grammar already uses for addressing). Done after numbering so a branch
    // can point at a parent created later in the tuple order.
    let seq_of: std::collections::HashMap<&str, usize> = entries
        .iter()
        .flatten()
        .map(|entry| (entry.row.id.as_str(), entry.seq))
        .collect();
    for entry in entries.iter_mut().flatten() {
        entry.parent_seq = entry
            .row
            .parent_id
            .as_deref()
            .and_then(|parent| seq_of.get(parent).copied());
    }

    entries
        .into_iter()
        .enumerate()
        .map(|(idx, e)| e.unwrap_or_else(|| unreachable!("index {idx} filled above")))
        .collect()
}

/// The workspace directory's own name — never a parent path segment (a bare
/// basename like `src` carries no project signal when the workspace *is* a
/// src directory; the full path takes over on collision instead). Roots and
/// otherwise nameless paths stay verbatim.
pub(crate) fn workspace_basename(project_root: &str) -> String {
    let trimmed = project_root.trim_end_matches(['/', '\\']);
    if trimmed.is_empty() {
        // The path was nothing but separators (e.g. "/"): show it as-is.
        return project_root.to_string();
    }
    trimmed
        .rsplit(['/', '\\'])
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or(trimmed)
        .to_string()
}

/// Draw the full-screen dashboard and return its sub-rects.
///
/// `rows` is the live monitor snapshot (re-sorted internally by creation
/// order for the dock); `selected` indexes the *creation-ordered* dock

pub(crate) fn draw_compact_cards(
    frame: &mut Frame,
    area: Rect,
    entries: &[DockEntry<'_>],
    card_columns: usize,
    selected: usize,
    focused: bool,
    scroll: &mut usize,
    follow: bool,
    theme: &Theme,
    current_session_id: &str,
) -> Rect {
    let (panel, body) = inset_panel(frame, area, "Sessions", focused, theme);
    let _ = panel;

    if entries.is_empty() {
        render_scrollable(
            frame,
            body,
            vec![Line::from(vec![
                Span::styled(
                    "No sessions on the server yet.".to_string(),
                    Style::default().fg(theme.muted()),
                ),
                Span::styled(
                    "  Press n to create one.".to_string(),
                    Style::default().fg(theme.dim()),
                ),
            ])],
            scroll,
            None,
            theme,
        );
        return body;
    }

    let total_rows = entries.len().div_ceil(card_columns);
    let visible_rows = body.height as usize;
    let selected_card_row = selected / card_columns;
    let follow_idx = if follow {
        Some(selected_card_row)
    } else {
        None
    };
    let (start_row, _) = resolve_scroll(
        scroll,
        visible_rows,
        total_rows,
        follow_idx,
        SCROLL_EDGE_MARGIN,
    );
    let end_row = (start_row + visible_rows).min(total_rows);

    let col_w = (body.width as usize / card_columns).max(1);
    let now = now_unix_secs();
    for (rel, card_row) in (start_row..end_row).enumerate() {
        for col in 0..card_columns {
            let idx = card_row * card_columns + col;
            let Some(entry) = entries.get(idx) else { break };
            let cell = Rect {
                x: body.x + (col * col_w) as u16,
                y: body.y + rel as u16,
                width: if col + 1 == card_columns {
                    body.width - (col * col_w) as u16
                } else {
                    col_w as u16
                },
                height: 1,
            };
            let is_selected = idx == selected;
            let is_current = entry.row.id == current_session_id;
            if is_selected {
                frame.render_widget(&mut nuotc::Clear::default(), cell);
                let style = theme.selection_style(true);
                frame.render_widget(&mut RtBlock::default().style(style), cell);
            } else if is_current {
                frame.render_widget(&mut nuotc::Clear::default(), cell);
                let style = Style::default().bg(theme.panel());
                frame.render_widget(&mut RtBlock::default().style(style), cell);
            }
            let line = dock_card_line(
                entry,
                cell.width as usize,
                is_current,
                is_selected,
                now,
                theme,
            );
            frame.render_widget(&mut Paragraph::new(line), cell);
        }
    }
    nuo_ui_primitives::primitives::draw_scrollbar(
        frame,
        body,
        start_row,
        total_rows.saturating_sub(visible_rows),
        theme,
    );
    body
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_dock(
    frame: &mut Frame,
    area: Rect,
    entries: &[DockEntry<'_>],
    card_columns: usize,
    selected: usize,
    focused: bool,
    scroll: &mut usize,
    follow: bool,
    theme: &Theme,
    current_session_id: &str,
) -> Rect {
    if area.width >= 50 {
        draw_fleet_table(
            frame,
            area,
            entries,
            selected,
            focused,
            scroll,
            follow,
            theme,
            current_session_id,
            now_unix_secs(),
        )
    } else {
        draw_compact_cards(
            frame,
            area,
            entries,
            card_columns,
            selected,
            focused,
            scroll,
            follow,
            theme,
            current_session_id,
        )
    }
}

/// How many card columns the dock's width affords. One column per
/// [`DOCK_COLUMN_W`] cells, so the strip degrades to a single column on
/// narrow terminals and spreads out on wide ones.
pub(crate) fn dock_columns(body_width: u16) -> usize {
    (body_width as usize / DOCK_COLUMN_W).clamp(1, 4)
}

/// Target width of one session card. Sized so a card always fits the
/// sequence tag, a useful slice of workspace name, uptime, and status.
const DOCK_COLUMN_W: usize = 36;

/// One session card as a single line of spans:
/// `#3  nuo  2h14m  running` — sequence | workspace | uptime | status.
/// The workspace name is the flexible field, truncated to fit; everything
/// else has a fixed reservation.
pub(crate) fn dock_card_line(
    entry: &DockEntry<'_>,
    cell_width: usize,
    _is_current: bool,
    is_selected: bool,
    now: u64,
    theme: &Theme,
) -> Line<'static> {
    let row = entry.row;
    let seq = format!("#{}", entry.seq);
    let uptime = format_uptime(now.saturating_sub(row.created_at));
    let status = dock_status_label(row.status);
    // The name is the card's identity: bold it under selection, plain
    // otherwise — the band background carries the rest of the highlight.
    let base_style = Style::default().fg(theme.fg());
    let name_style = if is_selected {
        base_style.add_modifier(Modifier::BOLD)
    } else {
        base_style
    };

    // Fixed reservations: marker+space (2) | seq (5) | uptime (7) |
    // status (15) | three 2-cell gutters (6). The name takes the rest.
    const FIXED: usize = 2 + 5 + 7 + 15 + 6;
    let name_w = cell_width.saturating_sub(FIXED).max(4);
    // Lineage badge (fork surfacing, ADR-0120). A grid cannot nest, so a branch
    // states its origin positionally — `⇂r2.1#3` reads "forked at r2.1 from
    // card #3" — while a trunk keeps the plain name (there is exactly one main
    // line and it needs no marker). The fork address comes from the store; the
    // parent sequence is resolved from the dock itself.
    let lineage = if row.parent_id.is_some() {
        match entry.parent_seq {
            Some(parent) => format!("\u{21b2}#{parent} "),
            None => "\u{21b2} ".to_string(),
        }
    } else {
        String::new()
    };
    let lineage_w = lineage.chars().count();
    let name_budget = name_w.saturating_sub(lineage_w);
    let name = truncate_display(&entry.workspace, name_budget);

    let spans = vec![
        Span::styled("  ", Style::default().fg(theme.brand())),
        Span::styled(format!("{seq:<5}"), Style::default().fg(theme.brand())),
        Span::styled(
            format_padded(&name, name_budget),
            if lineage.is_empty() {
                name_style
            } else {
                // A branch reads as derived: muted next to its trunk's plain
                // name, still bold under selection.
                if is_selected {
                    name_style
                } else {
                    Style::default().fg(theme.muted())
                }
            },
        ),
        Span::styled(lineage, Style::default().fg(theme.brand())),
        Span::styled("  ".to_string(), Style::default()),
        Span::styled(format!("{uptime:>7}"), Style::default().fg(theme.muted())),
        Span::styled("  ".to_string(), Style::default()),
        Span::styled(format!("{status:<15}"), status_style(row.status, theme)),
    ];
    let mut line = Line::from(spans);
    if is_selected && theme.elevation.is_structured() {
        for span in &mut line.spans {
            span.style = span.style.add_modifier(Modifier::REVERSE);
        }
    }
    line
}

/// The dock's coarse status vocabulary: running vs. done, with the blocked
/// states kept distinct since they are the dashboard's whole reason to be.
pub(crate) fn dock_status_label(status: ThreadActivity) -> &'static str {
    match status {
        ThreadActivity::Running => "running",
        ThreadActivity::NeedsApproval => "needs approval",
        ThreadActivity::NeedsInput => "needs input",
        ThreadActivity::Interrupted => "interrupted",
        ThreadActivity::Failed => "failed",
        ThreadActivity::Idle => "done",
    }
}

/// Seconds-since-open as a compact uptime: `43s`, `12m`, `2h14m`, `3d5h`.
pub fn format_uptime(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
    } else {
        format!("{}d{}h", secs / 86_400, (secs % 86_400) / 3600)
    }
}

/// One `SystemTime::now()` per frame for every uptime in the dock (the
/// session picker's precedent: `tui/overlays/session.rs`).
pub(crate) fn now_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Clamp a dock label to `max_width` columns, keeping its head.
///
/// Dock labels are prose. The mechanics live in the substrate's one overflow
/// implementation (ADR-0152 `[INV-TEXT-OVERFLOW-01]`); a local copy here is
/// precisely how the dock and the token report would drift apart.
pub(crate) fn truncate_display(text: &str, max_width: usize) -> String {
    nuo_ui_primitives::components::overflow::fit(
        text,
        max_width,
        nuo_ui_primitives::components::overflow::Overflow::End,
    )
    .text
}

/// Left-align `text` into a fixed display-width field with trailing spaces.
pub(crate) fn format_padded(text: &str, width: usize) -> String {
    let w = text.width();
    if w >= width {
        text.to_string()
    } else {
        let mut out = text.to_string();
        out.push_str(&" ".repeat(width - w));
        out
    }
}
