//! Session dashboard (`/dashboard`, formerly `/host`; ADR-0096, layout per
//! ADR-0097 §3): a first-class, full-screen orchestration console over every
//! session the unified daemon hosts. The surface is split into two zones:
//!
//! - **Console** (upper, flexible): the command surface. Its transcript
//!   keeps a receipt of every dispatched directive (what was sent, to which
//!   `#N`, and how the daemon answered), so the cockpit log answers "what
//!   did I ask the fleet to do" at a glance. The composer accepts the
//!   ADR-0097 address grammar — `@3 refactor the retry loop` dispatches to
//!   session `#3`, `@2 @3 …` fans out to several — plus slash verbs
//!   (`/kill`, `/interrupt`, `/suspend`, `/new`, …) that target the
//!   selected session or an explicit `@N`. Bare text prompts the selected
//!   session. The selected session's live monitor read-out rides beneath.
//! - **Sessions dock** (bottom strip): every session as a compact card —
//!   sequence number, workspace name (disambiguated when two sessions share
//!   a directory basename), uptime since the session opened, and lifecycle
//!   status. Cards tile into as many columns as the width affords and are
//!   ordered by sequence number (creation order), so positions are stable
//!   while statuses flip around them.
//!
//! Data is the live monitor snapshot the TUI maintains client-side (folded
//! from the daemon's `MonitorEvent` stream), so the dashboard refreshes
//! itself without any extra round-trip. The keyboard defaults to the
//! console/input region (`Tab` drops to the dock). Enter on a dock
//! selection opens the read-only session preview modal; `a` attaches to
//! the selected session (detach + re-attach, leaving the departed session
//! running); `i` / `p` / `n` issue control-plane verbs (interrupt / prompt
//! / new session); `k` kills the selection (confirm step); `s` suspends
//! it; `Esc` backs out of the preview, then the dashboard.

use nuo_wire::{MonitoredSession, SessionHosting, SessionStatus};
use nuotc::{
    Constraint, Direction, Frame, Layout, Line, Modifier, Rect, Span, Style,
    {Block as RtBlock, Clear, Paragraph},
};
use unicode_width::UnicodeWidthStr;

use crate::primitives::{
    ElevationContainer, LayoutTier, SCROLL_EDGE_MARGIN, resolve_scroll, viewport_rect,
};
use crate::render::Theme;

/// Which zone of the dashboard currently owns the keyboard.
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub enum DashboardFocus {
    /// The sessions dock (bottom strip): ↑/↓ moves the selection; Enter
    /// opens its preview modal, `a` attaches.
    List,
    /// The console / input region (upper, the default): ↑/↓ scrolls the
    /// interaction surface. The dashboard opens here (ADR-0097 §3) so the
    /// orchestrator composer owns typing without a Tab first.
    Detail,
}

/// The result of laying out one frame: the sub-rects the event loop records
/// for hit-testing and scroll math. Returned by [`draw_dashboard`].
pub struct DashboardRects {
    /// The whole dashboard surface (the viewport).
    pub area: Rect,
    /// The console body (chrome excluded) — the scroll viewport the event
    /// loop sizes PageUp/PageDown against.
    pub list_body: Rect,
    /// The sessions-dock body when visible. Reserved for future hit-testing.
    #[allow(dead_code)]
    pub detail_body: Option<Rect>,
}

/// A session card's dock position: its 1-based sequence number (creation
/// order across the whole snapshot) and its disambiguated workspace name.
struct DockEntry<'a> {
    row: &'a MonitoredSession,
    seq: usize,
    workspace: String,
}

/// The dashboard's sequence axis: indices into `rows` ordered by creation
/// (`created_at` ascending, id tiebreak so the order is total and stable).
/// The renderer and every selection-driven action (attach / interrupt /
/// prompt) must agree on this mapping — a positional selection means the
/// `#seq`-th card, not the `selected`-th snapshot row.
pub fn creation_order(rows: &[MonitoredSession]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..rows.len()).collect();
    order.sort_by(|&a, &b| {
        (rows[a].created_at, &rows[a].id).cmp(&(rows[b].created_at, &rows[b].id))
    });
    order
}

/// Order the snapshot by creation (the sequence axis) and precompute each
/// session's display fields: `#seq` numbers and workspace names that are
/// unique-by-basename when possible, full-path-disambiguated when two
/// sessions share a directory name (ADR-0097 §3).
fn dock_entries(rows: &[MonitoredSession]) -> Vec<DockEntry<'_>> {
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
        });
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
fn workspace_basename(project_root: &str) -> String {
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
/// entries; `current_session_id` marks the session this TUI is attached to.
/// `focus` selects whether ↑/↓/PgUp/PgDn move the dock selection or scroll
/// the console. `prompting` shows the inline new-session prompt line in
/// place of the footer command strip. `log` is the console's receipt
/// transcript (every dispatched directive and the daemon's answer).
/// Properties for rendering the full-screen session dashboard.
pub struct DashboardProps<'a> {
    pub rows: &'a [MonitoredSession],
    pub selected: usize,
    pub focus: DashboardFocus,
    pub list_scroll: &'a mut usize,
    pub list_follow: bool,
    pub detail_scroll: &'a mut usize,
    pub log: &'a [ConsoleLine],
    pub prompting: bool,
    pub prompt_create_new: bool,
    pub prompt_text: &'a str,
    pub current_session_id: &'a str,
    // The frame-level caret verdict (ADR-0205): the inline prompt borrows the
    // composer buffer but owns the cursor through this scene's own footer band.
    pub show_caret: bool,
}

/// Draw the full-screen dashboard view.
pub fn draw_dashboard(
    frame: &mut Frame,
    props: DashboardProps<'_>,
    theme: &Theme,
) -> DashboardRects {
    let DashboardProps {
        rows,
        selected,
        focus,
        list_scroll,
        list_follow,
        detail_scroll,
        log,
        prompting,
        prompt_create_new,
        prompt_text,
        current_session_id,
        show_caret,
    } = props;
    // A true full-screen surface: clear the whole frame and paint our own
    // backdrop.
    frame.render_widget(Clear, frame.area());
    frame.render_widget(
        RtBlock::default().style(Style::default().bg(theme.app_bg)),
        frame.area(),
    );
    // The head band spans the terminal's full width at the top edge; the body
    // below it keeps the shared viewport's horizontal insets and bottom margin,
    // starting one row lower than the viewport so the band's two rows are not
    // overlapped.
    let full = frame.area();
    let area = viewport_rect(frame);
    let tier = LayoutTier::from_rect(area, theme.elevation);
    /// The shared head band's height: identity row + namespace row (ADR-0298 §4).
    const BAND_ROWS: u16 = 2;
    let body = Rect {
        y: area.y.saturating_add(BAND_ROWS),
        height: area.height.saturating_sub(BAND_ROWS),
        ..area
    };

    let entries = dock_entries(rows);
    let selected = selected.min(entries.len().saturating_sub(1));

    // Two-tier responsive layout (ADR-0097 §3 evolution):
    // - Wide (width >= 90): Dual-pane side-by-side (Sessions list left, Cognitive Dossier + Console right).
    // - Compact (width < 90): Vertical stack (head band, Sessions dock top, Cognitive Dossier + Console bottom, footer).
    //
    // The head band is the shared two-row band every other scene carries
    // (ADR-0298 §4): row 1 is identity + fleet summary, row 2 is the `C-x`
    // scene namespace. It replaces this surface's former homegrown one-row
    // header plus gap row, so the dashboard's chrome can no longer drift from
    // the rest of the app.
    //
    // The band spans the full terminal width (its background owns every cell of
    // the row), pinned flush to the top edge exactly as every other scene draws
    // it (ADR-0298 §4: one head band). The body below sits inside the viewport
    // margins, offset past the band's two rows.
    let (header_rect, hints_rect, dock_rect, console_rect, footer_rect, card_columns) =
        if tier.is_wide() {
            let v_chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Min(0),    // main body
                    Constraint::Length(1), // gap
                    Constraint::Length(3), // footer
                ])
                .split(body);
            let main_body = v_chunks[0];
            let footer = v_chunks[2];

            let h_cols = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Percentage(38), // Sessions pane
                    Constraint::Length(1),      // gap
                    Constraint::Percentage(62), // Console & Mission Dossier pane
                ])
                .split(main_body);
            let dock = h_cols[0];
            let console = h_cols[2];
            let cols = dock_columns(dock.width);
            (
                Rect::new(full.x, full.y, full.width, 1),
                Rect::new(full.x, full.y.saturating_add(1), full.width, 1),
                dock,
                console,
                footer,
                cols,
            )
        } else {
            let cols = dock_columns(body.width);
            let dock_rows = entries.len().div_ceil(cols);
            let dock_height = (dock_rows.max(1) as u16 + 1).min(body.height / 2);

            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Min(0),              // console (Mission Dossier + Log)
                    Constraint::Length(1),           // gap
                    Constraint::Length(dock_height), // sessions dock
                    Constraint::Length(1),           // gap
                    Constraint::Length(3),           // footer
                ])
                .split(body);
            let console = chunks[0];
            let dock = chunks[2];
            let footer = chunks[4];
            (
                Rect::new(full.x, full.y, full.width, 1),
                Rect::new(full.x, full.y.saturating_add(1), full.width, 1),
                dock,
                console,
                footer,
                cols,
            )
        };

    // The head band, drawn by the shared renderer so the dashboard is
    // chrome-identical to every other scene.
    let (summary, needs_attention) = header_content(rows);
    let head = crate::render::ViewHeader::Dashboard(crate::render::DashboardHead {
        summary,
        needs_attention,
    });
    crate::render::draw_view_header(frame, header_rect, &head, theme);
    crate::render::draw_view_header_hints(
        frame,
        hints_rect,
        &crate::render::ViewHints {
            kind: crate::render::ViewKind::Dashboard,
            asides: None,
            breadcrumbs: None,
        },
        theme,
    );

    let console_body = draw_console(
        frame,
        console_rect,
        log,
        entries.get(selected).map(|e| e.row),
        focus == DashboardFocus::Detail,
        detail_scroll,
        theme,
    );

    let dock_body = draw_dock(
        frame,
        dock_rect,
        &entries,
        card_columns,
        selected,
        focus == DashboardFocus::List,
        list_scroll,
        list_follow,
        theme,
        current_session_id,
    );

    render_footer(
        frame,
        footer_rect,
        focus,
        prompting,
        prompt_create_new,
        prompt_text,
        theme,
        show_caret,
    );

    DashboardRects {
        area,
        list_body: console_body,
        detail_body: Some(dock_body),
    }
}

/// The dashboard's head-row context: the fleet summary the shared head band
/// renders on its right edge.
///
/// The row itself is drawn by [`crate::render::draw_view_header`] — the same
/// band every other scene uses (ADR-0298 §3: one head, one row-2 legend). This
/// function only computes the scene's own content for it.
fn header_content(rows: &[MonitoredSession]) -> (String, bool) {
    let needing = rows
        .iter()
        .filter(|r| {
            matches!(
                r.status,
                SessionStatus::NeedsApproval | SessionStatus::NeedsInput | SessionStatus::Failed
            )
        })
        .count();
    let running = rows
        .iter()
        .filter(|r| r.status == SessionStatus::Running)
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

/// Render the session list. Returns the body rect used for rows (the scroll
/// viewport — but for card *rows*, not individual sessions: scrolling the
/// dock pages whole card rows at once.
#[allow(clippy::too_many_arguments)]
fn draw_dock(
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
    // Panel chrome: a titled band so the dock reads as a pane.
    let (panel, body) = inset_panel(frame, area, "Sessions", focused, theme);
    let _ = panel;

    if entries.is_empty() {
        render_scrollable(
            frame,
            body,
            vec![Line::from(vec![
                Span::styled(
                    "No sessions on the daemon yet.".to_string(),
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

    // Card width is an even share of the body; the last column can be wider
    // (remainder), which only gives its name field more room.
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
            if is_selected {
                frame.render_widget(Clear, cell);
                let style = theme.selection_style(true);
                frame.render_widget(RtBlock::default().style(style), cell);
            }
            let line = dock_card_line(
                entry,
                cell.width as usize,
                entry.row.id == current_session_id,
                is_selected,
                now,
                theme,
            );
            frame.render_widget(Paragraph::new(line), cell);
        }
    }
    crate::primitives::draw_scrollbar(
        frame,
        body,
        start_row,
        total_rows.saturating_sub(visible_rows),
        theme,
    );
    body
}

/// How many card columns the dock's width affords. One column per
/// [`DOCK_COLUMN_W`] cells, so the strip degrades to a single column on
/// narrow terminals and spreads out on wide ones.
fn dock_columns(body_width: u16) -> usize {
    (body_width as usize / DOCK_COLUMN_W).clamp(1, 4)
}

/// Target width of one session card. Sized so a card always fits the
/// sequence tag, a useful slice of workspace name, uptime, and status.
const DOCK_COLUMN_W: usize = 36;

/// One session card as a single line of spans:
/// `#3  muta  2h14m  running` — sequence · workspace · uptime · status.
/// The workspace name is the flexible field, truncated to fit; everything
/// else has a fixed reservation.
fn dock_card_line(
    entry: &DockEntry<'_>,
    cell_width: usize,
    is_current: bool,
    is_selected: bool,
    now: u64,
    theme: &Theme,
) -> Line<'static> {
    let row = entry.row;
    let seq = format!("#{}", entry.seq);
    let marker = if is_current { "▶" } else { " " };
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

    // Fixed reservations: marker+space (2) · seq (5) · uptime (7) ·
    // status (15) · three 2-cell gutters (6). The name takes the rest.
    const FIXED: usize = 2 + 5 + 7 + 15 + 6;
    let name_w = cell_width.saturating_sub(FIXED).max(4);
    // Lineage badge (fork surfacing): a branch of a live conversation is
    // labeled with its kind — `⑂ aside` for a `/btw` fork, `⑂ fork` for an
    // explicit branch — so the dock reads as trunk cards with their derived
    // branches marked, not as N independent sessions. A trunk keeps the
    // plain name (the main line needs no badge: there is exactly one).
    let lineage = match row.fork_kind {
        nuo_wire::SessionForkKind::Trunk => String::new(),
        nuo_wire::SessionForkKind::Aside => "⑂aside ".to_string(),
        nuo_wire::SessionForkKind::Fork => "⑂fork ".to_string(),
        // Subagent sessions never surface in the dashboard (filtered at the
        // listing layer), but the pattern must stay exhaustive.
        nuo_wire::SessionForkKind::Subagent => String::new(),
    };
    let lineage_w = lineage.chars().count();
    let name_budget = name_w.saturating_sub(lineage_w);
    let name = truncate_display(&entry.workspace, name_budget);

    let spans = vec![
        Span::styled(format!("{marker} "), Style::default().fg(theme.brand())),
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
fn dock_status_label(status: SessionStatus) -> &'static str {
    match status {
        SessionStatus::Running => "running",
        SessionStatus::NeedsApproval => "needs approval",
        SessionStatus::NeedsInput => "needs input",
        SessionStatus::Interrupted => "interrupted",
        SessionStatus::Failed => "failed",
        SessionStatus::Idle => "done",
    }
}

/// Seconds-since-open as a compact uptime: `43s`, `12m`, `2h14m`, `3d5h`.
fn format_uptime(secs: u64) -> String {
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
fn now_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Width-aware truncation with an ellipsis tail (mirrors `truncate_str` in
/// the token report's overlay, kept local so the dock is self-contained).
fn truncate_display(text: &str, max_width: usize) -> String {
    if text.width() <= max_width {
        text.to_string()
    } else if max_width <= 1 {
        "…".to_string()
    } else {
        let mut out: String = text.chars().take(max_width - 1).collect();
        out.push('…');
        out
    }
}

/// Left-align `text` into a fixed display-width field with trailing spaces.
fn format_padded(text: &str, width: usize) -> String {
    let w = text.width();
    if w >= width {
        text.to_string()
    } else {
        let mut out = text.to_string();
        out.push_str(&" ".repeat(width - w));
        out
    }
}

/// The console: the dashboard's upper, flexible command region. The
/// transcript is the cockpit log — every dispatched directive and the
/// daemon's receipt — with the selected session's live monitor read-out
/// beneath. Scrolling this pane is what `Detail` focus drives.
fn draw_console(
    frame: &mut Frame,
    area: Rect,
    log: &[ConsoleLine],
    row: Option<&MonitoredSession>,
    focused: bool,
    scroll: &mut usize,
    theme: &Theme,
) -> Rect {
    let (_, body) = inset_panel(frame, area, "Console", focused, theme);
    let lines = console_lines(log, row, body.width as usize, theme);
    render_scrollable(frame, body, lines, scroll, None, theme);
    body
}

// shared chrome

/// Paint a panel band with a small title and return (outer, inner-body).
/// `focused` brightens the title so the active pane reads as such.
fn inset_panel(
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
        Paragraph::new(Line::from(Span::styled(title.to_string(), title_style))),
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
fn render_scrollable(
    frame: &mut Frame,
    body: Rect,
    lines: Vec<Line<'static>>,
    scroll: &mut usize,
    follow: Option<usize>,
    theme: &Theme,
) {
    let visible = body.height as usize;
    let (_, max_scroll) = resolve_scroll(scroll, visible, lines.len(), follow, SCROLL_EDGE_MARGIN);
    let para = Paragraph::new(lines).scroll(*scroll as u16, 0);
    frame.render_widget(para, body);
    crate::primitives::draw_scrollbar(frame, body, *scroll, max_scroll, theme);
}

#[allow(clippy::too_many_arguments)]
fn render_footer(
    frame: &mut Frame,
    rect: Rect,
    focus: DashboardFocus,
    prompting: bool,
    prompt_create_new: bool,
    prompt_text: &str,
    theme: &Theme,
    // The frame-level caret verdict (ADR-0205): the Dashboard's inline prompt
    // borrows the composer buffer but owns the cursor through its own footer
    // band, so this flag comes from `App::caret_owner() == CaretOwner::Scene`
    // rather than from `prompting` alone.
    show_caret: bool,
) {
    if rect.height == 0 || rect.width < 10 {
        return;
    }

    let bg = theme.body();
    let fill = Style::default().bg(bg);
    let key_style = crate::components::keycap::keycap_style(theme).bg(bg);
    let hint_style = fill.fg(theme.keycap_label());
    let width = rect.width as usize;

    if prompting {
        // Inline prompt inside the 3-row Subagent-style bar:
        let (label, hint) = if prompt_create_new {
            ("New session task: ", " (Enter create  Esc cancel)")
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
            frame.render_widget(Paragraph::new(line), Rect::new(rect.x, y, rect.width, 1));
        }

        // The field is append-only (there is no caret motion inside a scene
        // prompt), so the caret rests flush after the typed text — measured
        // through the same `unicode_width` pass the spans were painted with,
        // and clamped to the band so a long task line never spills past the
        // right edge.
        if show_caret {
            let lead = "   ".width() + label.width() + prompt_text.width();
            let cursor_x = rect
                .x
                .saturating_add(lead.min(u16::MAX as usize) as u16)
                .min(rect.right().saturating_sub(1));
            frame.set_cursor_position((cursor_x, mid));
        }
        return;
    }

    // One verb list, no branches: the console's footer advertises the scene's
    // own verbs plus the namespace's leave entry. (`keymap_page` used to switch
    // this to a dead `Esc close / ? close` pair — a leftover from the retired
    // in-dialog keymap sub-page, ADR-0172 — whose only caller passed `false`.)
    let pairs: Vec<(&'static str, &'static str)> = match focus {
        DashboardFocus::List => vec![
            ("↑/↓", "navigate"),
            ("Tab", "switch pane"),
            ("Enter", "preview"),
            ("a", "attach"),
            ("p", "prompt"),
            ("n", "new session"),
            ("i", "interrupt"),
            ("k", "kill"),
            ("s", "suspend"),
            ("C-x", "scene"),
        ],
        DashboardFocus::Detail => vec![
            ("↑/↓", "scroll"),
            ("Tab", "switch pane"),
            ("n", "new session"),
            ("p", "prompt"),
            ("a", "attach"),
            ("C-x", "scene"),
        ],
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
        frame.render_widget(Paragraph::new(line), Rect::new(rect.x, y, rect.width, 1));
    }
}

fn status_style(status: SessionStatus, theme: &Theme) -> Style {
    match status {
        SessionStatus::Running => Style::default().fg(theme.brand()),
        SessionStatus::NeedsApproval | SessionStatus::NeedsInput => {
            Style::default().fg(theme.warn())
        }
        SessionStatus::Failed => Style::default().fg(theme.err()),
        SessionStatus::Idle | SessionStatus::Interrupted => Style::default().fg(theme.muted()),
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
fn session_detail_lines(row: &MonitoredSession, width: usize, theme: &Theme) -> Vec<Line<'static>> {
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
            SessionHosting::Hosted => "hosted".to_string(),
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

/// The session preview modal (ADR-0097 §3): a centered, read-only drill-in
/// on one session, opened by Enter on a dock selection. Selection alone
/// never opens it; Esc closes. Read-only — attaching is `a` on the dock.
pub fn draw_session_preview(
    frame: &mut Frame,
    row: Option<&MonitoredSession>,
    scroll: &mut usize,
    theme: &Theme,
) {
    let area = crate::primitives::centered_rect(70, 75, frame.area());
    // Occlude the dashboard beneath and paint the panel.
    frame.render_widget(Clear, area);
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
    frame.render_widget(Paragraph::new(Line::from(title_spans)), title);

    let lines = match row {
        Some(r) => session_detail_lines(r, body.width as usize, theme),
        None => vec![Line::from(Span::styled(
            "Session is no longer on the daemon.".to_string(),
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
    frame.render_widget(Paragraph::new(hint), footer);
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

// the console's command grammar

/// One line of the console transcript: what was dispatched and how the
/// daemon answered. Kept as typed data (not preformatted strings) so the
/// renderer owns all styling and the tests can assert on structure.
#[derive(Debug, Clone, PartialEq)]
pub enum ConsoleLine {
    /// A directive the user issued: the raw text plus the resolved targets
    /// (`#3`, `#2 #3`, or `new`) and the action taken.
    Dispatch {
        raw: String,
        targets: Vec<usize>,
        action: &'static str,
    },
    /// The daemon's answer to a dispatch: `ok` receipts in `theme.ok()`,
    /// failures in `theme.err()` with the daemon's error text.
    Receipt {
        ok: bool,
        target: Option<usize>,
        text: String,
    },
    /// A local notice (parse error, unknown session, confirmation hint).
    Notice(String),
}

impl ConsoleLine {
    /// Render one console line. `theme` owns the palette; widths are
    /// unconstrained (the console wraps nothing — receipts are one line).
    fn to_line(&self, theme: &Theme) -> Line<'static> {
        match self {
            Self::Dispatch {
                raw,
                targets,
                action,
            } => {
                let who = if targets.is_empty() {
                    "new".to_string()
                } else {
                    targets
                        .iter()
                        .map(|n| format!("#{n}"))
                        .collect::<Vec<_>>()
                        .join(" ")
                };
                Line::from(vec![
                    Span::styled("› ".to_string(), Style::default().fg(theme.brand())),
                    Span::styled(format!("[{who}] "), Style::default().fg(theme.brand())),
                    Span::styled(action.to_string(), Style::default().fg(theme.muted())),
                    Span::styled(format!("  {raw}"), Style::default().fg(theme.fg())),
                ])
            }
            Self::Receipt { ok, target, text } => {
                let (glyph, color) = if *ok {
                    ("✓", theme.ok())
                } else {
                    ("✗", theme.err())
                };
                let target_span = target
                    .map(|n| Span::styled(format!("#{n} "), Style::default().fg(theme.brand())))
                    .unwrap_or_else(|| Span::raw(String::new()));
                Line::from(vec![
                    Span::styled("  ".to_string(), Style::default()),
                    Span::styled(format!("{glyph} "), Style::default().fg(color)),
                    target_span,
                    Span::styled(text.clone(), Style::default().fg(color)),
                ])
            }
            Self::Notice(text) => Line::from(vec![
                Span::styled("  ".to_string(), Style::default()),
                Span::styled(text.clone(), Style::default().fg(theme.dim())),
            ]),
        }
    }
}

/// A parsed console directive (ADR-0097 §2's grammar plus slash verbs).
#[derive(Debug, Clone, PartialEq)]
pub enum ConsoleCommand {
    /// `@3 text` / `@2 @3 text` — send `text` as a new round to each `#N`.
    Prompt { targets: Vec<usize>, text: String },
    /// `/kill [@N]`, `/interrupt [@N]`, `/suspend [@N]` — a control verb on
    /// one session. No `@N` means the dock selection.
    Verb {
        verb: ConsoleVerb,
        target: Option<usize>,
    },
    /// `/new text` — create a session for the dashboard's project with
    /// `text` as the opening prompt.
    New { text: Option<String> },
    /// `? text` / `/ask text` — ask the Archivist (ADR-0208), the
    /// muta-level retrieval agent, one question. The answer arrives as a
    /// console receipt (synchronous control round).
    Archivist { text: String },
    /// `/help` — the verb table as a notice block.
    Help,
    /// Text that matched no rule (empty input, a bare `/` with no verb, …).
    Unrecognized(String),
}

/// The verbs the console dispatches to the control plane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsoleVerb {
    Kill,
    Interrupt,
    Suspend,
}

impl ConsoleVerb {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Kill => "kill",
            Self::Interrupt => "interrupt",
            Self::Suspend => "suspend",
        }
    }

    /// One-line summary for the `/help` notice block.
    pub fn help_line(self) -> &'static str {
        match self {
            Self::Kill => "/kill [@N]     tear the session down (history kept on disk)",
            Self::Interrupt => "/interrupt [@N] stop the session's current round",
            Self::Suspend => "/suspend [@N]  park it in memory; next attach resumes it",
        }
    }
}

/// Parse one console line. The grammar (ADR-0097 §2, plus verbs):
///
/// - `@3 refactor the retry loop` → [`ConsoleCommand::Prompt`] targeting `#3`
/// - `@2 @3 summarize` → fan-out to `#2` and `#3`
/// - `/kill`, `/kill @3` → [`ConsoleCommand::Verb`]
/// - `/new <text>`, `/new` → [`ConsoleCommand::New`]
/// - `? <text>` / `/ask <text>` → [`ConsoleCommand::Archivist`]
/// - `/help` → [`ConsoleCommand::Help`]
/// - anything else that is non-empty → [`ConsoleCommand::Prompt`] with no
///   targets (the caller resolves "no targets" to the dock selection)
pub fn parse_console_command(line: &str) -> ConsoleCommand {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return ConsoleCommand::Unrecognized(String::new());
    }

    // The Archivist address: `? text` — a single leading `?` distinguishes it
    // from `@N` (session addresses) and `/` (verbs). No address follows it.
    if let Some(rest) = trimmed.strip_prefix('?') {
        let text = rest.trim();
        if text.is_empty() {
            return ConsoleCommand::Unrecognized(
                "nothing to ask — the Archivist needs a question after '?'".to_string(),
            );
        }
        return ConsoleCommand::Archivist {
            text: text.to_string(),
        };
    }

    // Slash verbs first: `/verb [rest…]`.
    if let Some(rest) = trimmed.strip_prefix('/') {
        let mut parts = rest.splitn(2, char::is_whitespace);
        let word = parts.next().unwrap_or("").to_ascii_lowercase();
        let remainder = parts.next().unwrap_or("").trim().to_string();
        return match word.as_str() {
            "help" | "?" => ConsoleCommand::Help,
            "ask" => {
                if remainder.is_empty() {
                    ConsoleCommand::Unrecognized(
                        "nothing to ask — the Archivist needs a question after /ask".to_string(),
                    )
                } else {
                    ConsoleCommand::Archivist { text: remainder }
                }
            }
            "new" => {
                if remainder.is_empty() {
                    ConsoleCommand::New { text: None }
                } else {
                    ConsoleCommand::New {
                        text: Some(remainder),
                    }
                }
            }
            "kill" | "x" => parse_verb(ConsoleVerb::Kill, &remainder),
            "interrupt" | "stop" => parse_verb(ConsoleVerb::Interrupt, &remainder),
            "suspend" | "park" => parse_verb(ConsoleVerb::Suspend, &remainder),
            _ => ConsoleCommand::Unrecognized(format!(
                "unknown command '{word}' — /help lists the verbs"
            )),
        };
    }

    // Then the address grammar: a leading run of `@n` tokens.
    let (targets, text) = split_leading_targets(trimmed);
    ConsoleCommand::Prompt {
        targets,
        text: text.to_string(),
    }
}

/// `/verb [@N]` — an optional single address token after the verb word.
fn parse_verb(verb: ConsoleVerb, rest: &str) -> ConsoleCommand {
    let rest = rest.trim();
    if rest.is_empty() {
        return ConsoleCommand::Verb { verb, target: None };
    }
    let (targets, leftover) = split_leading_targets(rest);
    match (targets.first().copied(), leftover.trim().is_empty()) {
        (Some(n), true) => ConsoleCommand::Verb {
            verb,
            target: Some(n),
        },
        (None, true) => ConsoleCommand::Verb { verb, target: None },
        // Anything beyond the verb (and an optional address) is a usage
        // error; report it instead of silently discarding the tail.
        _ => ConsoleCommand::Unrecognized(format!(
            "usage: /{} [@N] — nothing else follows the verb",
            verb.as_str()
        )),
    }
}

/// Split a leading run of `@n` address tokens from the payload: returns
/// `(targets, rest)`. An `@` token that fails to parse as a number is left
/// in the payload (it is prose, not an address).
fn split_leading_targets(text: &str) -> (Vec<usize>, &str) {
    let mut targets = Vec::new();
    let mut rest = text;
    while let Some(after) = rest.strip_prefix('@') {
        // Digits immediately after the `@` form the number.
        let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
        if digits.is_empty() {
            break;
        }
        match digits.parse::<usize>() {
            Ok(n) => {
                targets.push(n);
                // Skip exactly one whitespace run between tokens.
                rest = after[digits.len()..].trim_start();
            }
            Err(_) => break,
        }
    }
    (targets, rest)
}

/// The console transcript rendering: receipts above the fold, the hint
/// header, then the selected session's monitor read-out.
fn console_lines(
    log: &[ConsoleLine],
    row: Option<&MonitoredSession>,
    width: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled(
            "Direct the fleet: ".to_string(),
            Style::default().fg(theme.muted()),
        ),
        Span::styled("@3 text".to_string(), Style::default().fg(theme.brand())),
        Span::styled(" sends  ".to_string(), Style::default().fg(theme.dim())),
        Span::styled(
            "/kill /interrupt /suspend /new /help".to_string(),
            Style::default().fg(theme.brand()),
        ),
        Span::styled(
            " manage  bare text prompts the selection".to_string(),
            Style::default().fg(theme.dim()),
        ),
    ]));
    if log.is_empty() {
        lines.push(Line::from(Span::styled(
            "No directives yet.".to_string(),
            Style::default().fg(theme.dim()),
        )));
    } else {
        lines.extend(log.iter().map(|l| l.to_line(theme)));
    }
    if let Some(row) = row {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "selected session".to_string(),
            Style::default().fg(theme.dim()),
        )));
        lines.extend(session_detail_lines(row, width, theme));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ADR-0298 §4: the dashboard is chrome-identical to every other scene. Its
    /// head is the shared two-row band — row 1 identity + fleet summary, row 2
    /// the `Ctrl-x scene` namespace — not the homegrown one-row header plus gap
    /// row it used to carry. This pins the rendered rows end to end, because the
    /// layout split is what makes the namespace row reachable at all.
    #[test]
    fn dashboard_head_renders_the_shared_two_row_band() {
        let theme = Theme::default();
        let rows = vec![row("a-1", 100, "/work/app", SessionStatus::Running)];
        let mut terminal = nuotc::TestTerminal::new(100, 24);
        let mut list_scroll = 0usize;
        let mut detail_scroll = 0usize;
        terminal.draw(|f| {
            draw_dashboard(
                f,
                DashboardProps {
                    rows: &rows,
                    selected: 0,
                    focus: DashboardFocus::List,
                    list_scroll: &mut list_scroll,
                    list_follow: true,
                    detail_scroll: &mut detail_scroll,
                    log: &[],
                    prompting: false,
                    prompt_create_new: false,
                    prompt_text: "",
                    current_session_id: "a-1",
                    show_caret: false,
                },
                &theme,
            );
        });

        let buffer = terminal.buffer();
        let row_text = |y: u16| -> String {
            buffer
                .content
                .iter()
                .skip(y as usize * 100)
                .take(100)
                .map(|c| c.symbol())
                .collect()
        };

        // Row 1: identity, scope, and the live fleet summary.
        let head = row_text(0);
        assert!(head.contains("DASHBOARD"), "identity on row 1: {head:?}");
        assert!(head.contains("all projects"), "scope on row 1: {head:?}");
        assert!(head.contains("1 session(s)"), "fleet summary: {head:?}");
        assert!(head.contains("1 running"), "running count: {head:?}");

        // Row 2: the namespace legend — the row the old layout had no slot for.
        let legend = row_text(1);
        assert!(
            legend.contains("Ctrl-x") && legend.contains("scene"),
            "row 2 advertises the scene namespace: {legend:?}"
        );
        assert!(
            !legend.contains("Esc"),
            "Esc is never advertised as a scene exit: {legend:?}"
        );
    }

    fn row(
        id: &str,
        created_at: u64,
        project_root: &str,
        status: SessionStatus,
    ) -> MonitoredSession {
        MonitoredSession {
            id: id.into(),
            overview: format!("task for {id}"),
            created_at,
            updated_at: created_at,
            message_count: 3,
            hosting: SessionHosting::Hosted,
            status,
            round: 1,
            turn: None,
            output_tokens: 120,
            elapsed_ms: 5_000,
            current_tool: None,
            activity: None,
            context_tokens: None,
            note: None,
            project_root: project_root.into(),
            parent_id: None,
            fork_kind: nuo_wire::SessionForkKind::Trunk,
            digest: None,
        }
    }

    #[test]
    fn sequence_numbers_follow_creation_order_not_snapshot_order() {
        // Snapshot is newest-first by updated_at; the dock must re-sort by
        // creation so #1 is the oldest session no matter how rows arrive.
        let rows = vec![
            row("c-newest", 300, "/work/app", SessionStatus::Running),
            row("a-oldest", 100, "/work/lib", SessionStatus::Idle),
            row("b-middle", 200, "/work/web", SessionStatus::Idle),
        ];
        let entries = dock_entries(&rows);
        // dock_entries preserves input order as its slots but stamps seq by
        // creation: a-oldest must be #1 even though it sits at index 1.
        let seq_of = |id: &str| entries.iter().find(|e| e.row.id == id).unwrap().seq;
        assert_eq!(seq_of("a-oldest"), 1);
        assert_eq!(seq_of("b-middle"), 2);
        assert_eq!(seq_of("c-newest"), 3);
        // And the raw creation_order mapping (used by event handlers) agrees.
        let order = creation_order(&rows);
        assert_eq!(order, vec![1, 2, 0]);
    }

    #[test]
    fn creation_order_is_stable_for_equal_timestamps() {
        let rows = vec![
            row("b", 100, "/p/b", SessionStatus::Idle),
            row("a", 100, "/p/a", SessionStatus::Idle),
        ];
        assert_eq!(creation_order(&rows), vec![1, 0]);
        // Repeated calls give the same answer (total order via id tiebreak).
        assert_eq!(creation_order(&rows), creation_order(&rows));
    }

    #[test]
    fn workspace_names_use_basename_until_they_collide() {
        let rows = vec![
            row("one", 1, "/home/ming/projects/muta", SessionStatus::Idle),
            row("two", 2, "/home/ming/projects/app", SessionStatus::Idle),
        ];
        let entries = dock_entries(&rows);
        assert_eq!(entries[0].workspace, "muta");
        assert_eq!(entries[1].workspace, "app");

        // Same basename in two different parents → both fall back to full
        // paths (a bare "src" would be ambiguous AND meaningless).
        let rows = vec![
            row("one", 1, "/home/ming/projects/muta", SessionStatus::Idle),
            row("two", 2, "/tmp/worktree/muta", SessionStatus::Idle),
        ];
        let entries = dock_entries(&rows);
        assert_eq!(entries[0].workspace, "/home/ming/projects/muta");
        assert_eq!(entries[1].workspace, "/tmp/worktree/muta");
    }

    #[test]
    fn workspace_basename_never_shows_a_parent() {
        assert_eq!(workspace_basename("/home/ming/projects/muta"), "muta");
        assert_eq!(workspace_basename("/home/ming/projects/muta/"), "muta");
        assert_eq!(workspace_basename("/"), "/");
        assert_eq!(workspace_basename("/src"), "src");
    }

    #[test]
    fn dock_columns_adapt_to_width() {
        assert_eq!(dock_columns(30), 1);
        assert_eq!(dock_columns(36), 1);
        assert_eq!(dock_columns(72), 2);
        assert_eq!(dock_columns(140), 3);
        assert_eq!(dock_columns(400), 4); // capped
    }

    #[test]
    fn dock_card_shows_seq_workspace_uptime_and_status() {
        let theme = Theme::default();
        let now = 1_000_000u64;
        let r = row(
            "x",
            now - (2 * 3600 + 14 * 60),
            "/work/muta",
            SessionStatus::Running,
        );
        let entries = dock_entries(std::slice::from_ref(&r));
        let line = dock_card_line(&entries[0], 60, false, false, now, &theme);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("#1"), "{text}");
        assert!(text.contains("muta"), "{text}");
        assert!(text.contains("2h14m"), "{text}");
        assert!(text.contains("running"), "{text}");
    }

    #[test]
    fn dock_card_badges_a_forked_session_by_kind() {
        // Lineage surfacing: a trunk card carries no badge (exactly one main
        // line per conversation); an aside/fork branch is badged `⑂aside` /
        // `⑂fork` so the dock reads as trunk + derived branches rather than
        // N independent sessions.
        let theme = Theme::default();
        let now = 1_000u64;

        let mut trunk = row("t", 1, "/work/main", SessionStatus::Running);
        trunk.fork_kind = nuo_wire::SessionForkKind::Trunk;
        let entries = dock_entries(std::slice::from_ref(&trunk));
        let line = dock_card_line(&entries[0], 60, false, false, now, &theme);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            !text.contains('⑂'),
            "a trunk carries no lineage badge: {text}"
        );

        let mut aside = row("a", 2, "/work/main", SessionStatus::Running);
        aside.parent_id = Some("t".into());
        aside.fork_kind = nuo_wire::SessionForkKind::Aside;
        let entries = dock_entries(std::slice::from_ref(&aside));
        let line = dock_card_line(&entries[0], 60, false, false, now, &theme);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("⑂aside"), "an aside branch is badged: {text}");

        let mut fork = row("f", 3, "/work/main", SessionStatus::Idle);
        fork.parent_id = Some("t".into());
        fork.fork_kind = nuo_wire::SessionForkKind::Fork;
        let entries = dock_entries(std::slice::from_ref(&fork));
        let line = dock_card_line(&entries[0], 60, false, false, now, &theme);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("⑂fork"), "an explicit fork is badged: {text}");
    }

    #[test]
    fn dock_card_truncates_long_workspace_names() {
        let theme = Theme::default();
        let now = 1_000u64;
        // Colliding basenames force the full path onto the card; a long full
        // path must then be ellipsized into the name field.
        let rows = vec![
            row(
                "a",
                1,
                "/very/long/path/to/a/deeply/nested/project",
                SessionStatus::Idle,
            ),
            row("b", 2, "/other/worktree/project", SessionStatus::Idle),
        ];
        let entries = dock_entries(&rows);
        let line = dock_card_line(&entries[0], 60, false, false, now, &theme);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains('…'), "{text}");
        assert!(
            unicode_width::UnicodeWidthStr::width(text.as_str()) <= 60,
            "{text:?}"
        );
        // Idle reads as "done" in the dock vocabulary.
        assert!(text.contains("done"), "{text}");
        // The unique-basename sibling is untouched.
        assert_eq!(entries[1].workspace, "/other/worktree/project");
    }

    #[test]
    fn dock_card_at_minimum_width_keeps_fixed_fields() {
        let theme = Theme::default();
        let now = 1_000u64;
        let r = row("x", now, "/work/muta", SessionStatus::Running);
        let entries = dock_entries(std::slice::from_ref(&r));
        // A degenerate 30-cell cell (below the 36 target): seq, uptime and
        // status all survive; the name field just gets cramped.
        let line = dock_card_line(&entries[0], 30, false, false, now, &theme);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("#1"), "{text}");
        assert!(text.contains("running"), "{text}");
    }

    #[test]
    fn uptime_format_scales() {
        assert_eq!(format_uptime(43), "43s");
        assert_eq!(format_uptime(12 * 60), "12m");
        assert_eq!(format_uptime(2 * 3600 + 14 * 60), "2h14m");
        assert_eq!(format_uptime(3 * 86_400 + 5 * 3600), "3d5h");
    }

    #[test]
    fn dock_status_vocabulary() {
        assert_eq!(dock_status_label(SessionStatus::Running), "running");
        assert_eq!(dock_status_label(SessionStatus::Idle), "done");
        assert_eq!(
            dock_status_label(SessionStatus::NeedsApproval),
            "needs approval"
        );
        assert_eq!(dock_status_label(SessionStatus::NeedsInput), "needs input");
        assert_eq!(dock_status_label(SessionStatus::Failed), "failed");
    }

    // console grammar

    #[test]
    fn address_grammar_routes_leading_targets() {
        match parse_console_command("@3 refactor the retry loop") {
            ConsoleCommand::Prompt { targets, text } => {
                assert_eq!(targets, vec![3]);
                assert_eq!(text, "refactor the retry loop");
            }
            other => panic!("expected Prompt, got {other:?}"),
        }
        // Fan-out: every leading @n token is collected, then the payload.
        match parse_console_command("@2 @3 summarize your findings") {
            ConsoleCommand::Prompt { targets, text } => {
                assert_eq!(targets, vec![2, 3]);
                assert_eq!(text, "summarize your findings");
            }
            other => panic!("expected Prompt, got {other:?}"),
        }
    }

    #[test]
    fn bare_text_is_a_prompt_with_no_targets() {
        match parse_console_command("fix the flaky test") {
            ConsoleCommand::Prompt { targets, text } => {
                assert!(targets.is_empty());
                assert_eq!(text, "fix the flaky test");
            }
            other => panic!("expected Prompt, got {other:?}"),
        }
    }

    #[test]
    fn archivist_address_parses_question_mark_and_ask_verb() {
        // ADR-0208: `? text` asks the muta-level retrieval agent.
        match parse_console_command("? where did we debug the retry loop") {
            ConsoleCommand::Archivist { text } => {
                assert_eq!(text, "where did we debug the retry loop");
            }
            other => panic!("expected Archivist, got {other:?}"),
        }
        // `/ask` is the verb alias.
        match parse_console_command("/ask which project was that") {
            ConsoleCommand::Archivist { text } => {
                assert_eq!(text, "which project was that");
            }
            other => panic!("expected Archivist, got {other:?}"),
        }
        // An address with no payload is a usage notice, not an ask.
        assert!(matches!(
            parse_console_command("?"),
            ConsoleCommand::Unrecognized(_)
        ));
        assert!(matches!(
            parse_console_command("/ask"),
            ConsoleCommand::Unrecognized(_)
        ));
        // `?` no longer aliases /help — /help keeps the verb table.
        assert!(matches!(
            parse_console_command("/help"),
            ConsoleCommand::Help
        ));
    }

    #[test]
    fn mid_line_at_is_prose_not_an_address() {
        // The address applies to the whole line: an @ that is not the first
        // token stays in the payload (ADR-0097 §2 keeps the grammar
        // regular — no per-sentence retargeting).
        match parse_console_command("ping @alice about the review") {
            ConsoleCommand::Prompt { targets, text } => {
                assert!(targets.is_empty());
                assert_eq!(text, "ping @alice about the review");
            }
            other => panic!("expected Prompt, got {other:?}"),
        }
    }

    #[test]
    fn verbs_parse_with_optional_address() {
        assert_eq!(
            parse_console_command("/kill"),
            ConsoleCommand::Verb {
                verb: ConsoleVerb::Kill,
                target: None
            }
        );
        assert_eq!(
            parse_console_command("/interrupt @3"),
            ConsoleCommand::Verb {
                verb: ConsoleVerb::Interrupt,
                target: Some(3)
            }
        );
        // Aliases and case-insensitivity.
        assert_eq!(
            parse_console_command("/STOP @1"),
            ConsoleCommand::Verb {
                verb: ConsoleVerb::Interrupt,
                target: Some(1)
            }
        );
        assert_eq!(
            parse_console_command("/park @2"),
            ConsoleCommand::Verb {
                verb: ConsoleVerb::Suspend,
                target: Some(2)
            }
        );
    }

    #[test]
    fn verb_with_trailing_text_is_a_usage_error() {
        // A verb takes an optional address and nothing else; silently
        // dropping a tail would hide what the user typed.
        match parse_console_command("/kill @3 now") {
            ConsoleCommand::Unrecognized(msg) => {
                assert!(msg.contains("usage"), "message should explain: {msg}");
            }
            other => panic!("expected Unrecognized, got {other:?}"),
        }
    }

    #[test]
    fn new_and_help_parse() {
        assert_eq!(
            parse_console_command("/new"),
            ConsoleCommand::New { text: None }
        );
        assert_eq!(
            parse_console_command("/new refactor the retry loop"),
            ConsoleCommand::New {
                text: Some("refactor the retry loop".to_string())
            }
        );
        assert_eq!(parse_console_command("/help"), ConsoleCommand::Help);
        assert_eq!(parse_console_command("/?"), ConsoleCommand::Help);
    }

    #[test]
    fn unknown_verb_names_the_command() {
        match parse_console_command("/frobnicate") {
            ConsoleCommand::Unrecognized(msg) => {
                assert!(msg.contains("frobnicate"), "message should name it: {msg}");
                assert!(msg.contains("/help"), "message should point at help: {msg}");
            }
            other => panic!("expected Unrecognized, got {other:?}"),
        }
    }

    #[test]
    fn empty_input_is_never_a_dispatch() {
        assert_eq!(
            parse_console_command("   "),
            ConsoleCommand::Unrecognized(String::new())
        );
    }

    #[test]
    fn session_detail_lines_renders_cognitive_digest_intent_and_milestones() {
        let theme = Theme::default();
        let mut r = row("s-123", 1000, "/work/muta", SessionStatus::Running);
        r.digest = Some(nuo_wire::SessionDigest {
            title: "Refactor Auth Middleware".to_string(),
            intent: "Rewrite token validator to support ECDSA signatures.".to_string(),
            history: vec![
                "Created test suite for ECDSA".to_string(),
                "Updated Cargo.toml dependencies".to_string(),
            ],
        });
        r.current_tool = Some("run_command".to_string());

        let lines = session_detail_lines(&r, 80, &theme);
        let rendered: String = lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            rendered.contains("Mission:   Refactor Auth Middleware"),
            "{rendered}"
        );
        assert!(
            rendered.contains("Intent:    Rewrite token validator to support ECDSA signatures."),
            "{rendered}"
        );
        assert!(rendered.contains("Milestones & Progress:"), "{rendered}");
        assert!(
            rendered.contains("• Created test suite for ECDSA"),
            "{rendered}"
        );
        assert!(
            rendered.contains("• Updated Cargo.toml dependencies"),
            "{rendered}"
        );
        assert!(rendered.contains("Telemetry:"), "{rendered}");
        assert!(rendered.contains("tool"), "{rendered}");
        assert!(rendered.contains("run_command"), "{rendered}");
    }

    #[test]
    fn session_detail_lines_falls_back_to_overview_when_no_digest() {
        let theme = Theme::default();
        let mut r = row("s-456", 1000, "/work/muta", SessionStatus::Idle);
        r.overview = "Plain user prompt without digest".to_string();
        r.digest = None;

        let lines = session_detail_lines(&r, 80, &theme);
        let rendered: String = lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");

        assert!(rendered.contains("Overview:"), "{rendered}");
        assert!(
            rendered.contains("Plain user prompt without digest"),
            "{rendered}"
        );
        assert!(!rendered.contains("Milestones & Progress:"), "{rendered}");
    }

    #[test]
    fn responsive_layout_tier_splits_wide_and_compact() {
        assert_eq!(LayoutTier::from_width(120), LayoutTier::Wide);
        assert_eq!(LayoutTier::from_width(90), LayoutTier::Wide);
        assert_eq!(LayoutTier::from_width(89), LayoutTier::Compact);
        assert_eq!(LayoutTier::from_width(60), LayoutTier::Compact);
    }

    #[test]
    fn responsive_layout_tier_adapts_to_elevation_archetype() {
        use nuotc::{ElevationArchetype, Rect};
        let rect_90 = Rect::new(0, 0, 90, 24);
        // Under Chromatic (TrueColor), 90 columns has 0 cost -> 90 effective -> Wide
        assert_eq!(
            LayoutTier::from_rect(rect_90, ElevationArchetype::Chromatic),
            LayoutTier::Wide
        );
        // Under Structured (Mono/Linux VT), 2 cols border cost -> 88 effective -> Compact
        assert_eq!(
            LayoutTier::from_rect(rect_90, ElevationArchetype::Structured),
            LayoutTier::Compact
        );
        // Under Structured with 92 columns -> 90 effective -> Wide
        let rect_92 = Rect::new(0, 0, 92, 24);
        assert_eq!(
            LayoutTier::from_rect(rect_92, ElevationArchetype::Structured),
            LayoutTier::Wide
        );
    }
}
