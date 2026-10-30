//! Session dashboard (`/dashboard`, formerly `/host`): a first-class, full-screen
//! orchestration console over every
//! session the unified server hosts. The surface is split into two zones:
//!
//! - **Console** (upper, flexible): the command surface. Its transcript
//!   keeps a receipt of every dispatched directive (what was sent, to which
//!   `#N`, and how the server answered), so the cockpit log answers "what
//!   did I ask the fleet to do" at a glance. The composer accepts the
//!   Address grammar — `@3 refactor the retry loop` dispatches to
//!   session `#3`, `@2 @3 …` fans out to several — plus slash verbs
//!   (`/kill`, `/interrupt`, `/suspend`, `/new`, …) that target the
//!   selected session or an explicit `@N`. Bare text prompts the selected
//!   session. The selected session's live monitor read-out rides beneath.
//! - **Sessions dock** (bottom strip): every session as a compact card —
//!   sequence number, workspace name (disambiguated when two sessions share
//!   a directory basename), uptime since the session opened, and lifecycle
//!   status. Cards tile into as many columns as the width affords and are
//!   ordered by sequence number (creation order), so positions are stable
//!   while statuses flip around them. A forked branch also carries a
//!   back-reference to its origin card (`⇂r2.1#3` — forked at r2.1 from card
//!   #3): a grid has no second axis to nest along, so lineage is stated, not
//!   implied [ADR-0120].
//!
//!   This axis stays **creation-ascending** (`#1` is the oldest session) while
//!   the browsing surfaces (`/threads`, `nuo thread ls`) are
//!   creation-**descending** [ADR-0128]. The first field here is not a reading
//!   order but an address: the console accepts `@3 …` / `#3` control verbs, and
//!   every receipt already printed in the log names a `#N`. Reversing the paint
//!   while keeping the numbers would make the address axis read backwards;
//!   renumbering would invalidate addresses already spoken.
//!
//! Data is the live monitor snapshot the TUI maintains client-side (folded
//! from the server's `MonitorEvent` stream), so the dashboard refreshes
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
    {Block as RtBlock, Paragraph},
};
use unicode_width::UnicodeWidthStr;

use crate::shell::render::Theme;
use crate::shell::view_header::TabBarProps;
use nuo_ui_primitives::design::INPUT_PROMPT_PREFIX;
use nuo_ui_primitives::primitives::{
    ElevationContainer, LayoutTier, SCROLL_EDGE_MARGIN, resolve_scroll, viewport_rect,
};

/// Which zone of the dashboard currently owns the keyboard.
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub enum DashboardFocus {
    /// The sessions dock (bottom strip): ↑/↓ moves the selection; Enter
    /// opens its preview modal, `a` attaches.
    List,
    /// The console / input region (upper, the default): ↑/↓ scrolls the
    /// interaction surface. The dashboard opens here so the
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
    /// Bounding rects for each rendered tab in the top tab bar: `(tab_index, rect)`.
    pub tab_rects: Vec<(usize, Rect)>,
    /// Full bounding rect for the top tab bar row.
    pub tab_bar_rect: Option<Rect>,
    /// Bounding rect for left overflow indicator (`‹N`), if present.
    pub tab_prev_rect: Option<Rect>,
    /// Bounding rect for right overflow indicator (`N›`), if present.
    pub tab_next_rect: Option<Rect>,
    /// Bounding rect for overflow overview / switcher button (`[▾]`), if present.
    pub tab_overflow_rect: Option<Rect>,
    pub tab_notices_rect: Option<Rect>,
    pub tab_help_rect: Option<Rect>,
}

/// A session card's dock position: its 1-based sequence number (creation
/// order across the whole snapshot) and its disambiguated workspace name.
struct DockEntry<'a> {
    row: &'a MonitoredSession,
    seq: usize,
    workspace: String,
    /// Lineage (ADR-0120): the dock sequence of this card's parent, when the
    /// parent is itself in the dock. A card grid has no second axis to indent
    /// along, so a branch states its origin with a back-reference
    /// (`⇂r2.1#N`) instead of a nesting the geometry cannot express.
    parent_seq: Option<usize>,
    /// The persisted fork address, when the store recorded one (`⇂r2.1`).
    fork_point: Option<nuo_wire::ForkPoint>,
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
/// sessions share a directory name.
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
            parent_seq: None,
            fork_point: row.fork_point,
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
/// transcript (every dispatched directive and the server's answer).
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
    pub cursor_position: usize,
    pub current_session_id: &'a str,
    // The frame-level caret verdict: the inline prompt borrows the
    // composer buffer but owns the cursor through this scene's own footer band.
    pub show_caret: bool,
    /// Client-global TabBar projection (ADR-0039, ADR-0060, ADR-0062).
    /// Tier 1 of the head band, carrying open tabs and active tab selection.
    pub tab_bar: Option<TabBarProps<'a>>,
    /// Active search filter string for the threads fleet table.
    pub filter: &'a str,
    /// Whether the `/` inline filter search input is active.
    pub filtering: bool,
    /// Whether the dashboard is displaying the Model Usage view (ADR-0129).
    pub view_usage: bool,
    /// Cross-session model usage statistics report, if loaded.
    pub usage_report: Option<&'a nuo_wire::usage_stats::UsageStatsReport>,
    /// Whether a model usage report fetch is in flight over the control plane.
    pub usage_loading: bool,
    /// Scroll offset for the model usage analytics report view.
    pub usage_scroll: usize,
}

/// Resolve the authentic semantic display title for a thread (ADR-0050, ADR-0129).
pub fn thread_display_title(row: &MonitoredSession) -> &str {
    if let Some(t) = row.title.as_deref().filter(|s| !s.trim().is_empty()) {
        t.trim()
    } else if let Some(digest) = &row.digest
        && !digest.title.trim().is_empty()
    {
        digest.title.trim()
    } else if !row.overview.trim().is_empty() {
        row.overview.trim()
    } else {
        "untitled thread"
    }
}

/// Compact format for token counters (e.g. 842, 12.4k, 1.25M).
pub fn fmt_tokens_compact(tokens: u64) -> String {
    if tokens < 1_000 {
        format!("{tokens}")
    } else if tokens < 1_000_000 {
        format!("{:.1}k", tokens as f64 / 1_000.0)
    } else {
        format!("{:.2}M", tokens as f64 / 1_000_000.0)
    }
}

/// Visual status chip and theme-aligned style for a thread (ADR-0129).
pub fn status_chip(status: SessionStatus, theme: &Theme) -> (&'static str, Style) {
    match status {
        SessionStatus::Running => (
            "● RUNNING",
            Style::default()
                .fg(theme.brand())
                .add_modifier(Modifier::BOLD),
        ),
        SessionStatus::NeedsApproval => (
            "▲ APPROVAL",
            Style::default()
                .fg(theme.warn())
                .add_modifier(Modifier::BOLD),
        ),
        SessionStatus::NeedsInput => (
            "? INPUT",
            Style::default()
                .fg(theme.warn())
                .add_modifier(Modifier::BOLD),
        ),
        SessionStatus::Failed => (
            "✕ FAILED",
            Style::default()
                .fg(theme.err())
                .add_modifier(Modifier::BOLD),
        ),
        SessionStatus::Interrupted => ("■ STOPPED", Style::default().fg(theme.dim())),
        SessionStatus::Idle => ("○ IDLE", Style::default().fg(theme.muted())),
    }
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
        cursor_position,
        current_session_id,
        show_caret,
        tab_bar,
        filter,
        filtering,
        view_usage,
        usage_report,
        usage_loading,
        usage_scroll,
    } = props;
    // A true full-screen surface: clear the whole frame and paint our own
    // backdrop.
    frame.render_widget(&mut nuotc::Clear::default(), frame.area());
    frame.render_widget(
        &mut RtBlock::default().style(Style::default().bg(theme.app_bg)),
        frame.area(),
    );
    // The head band spans the terminal's full width at the top edge; the body
    // below it keeps the shared viewport's horizontal insets and bottom margin,
    // starting one row lower than the viewport so the band's two rows are not
    // overlapped.
    let full = frame.area();
    let area = viewport_rect(frame);
    let tier = LayoutTier::from_rect(area, theme.elevation);
    /// The shared head band's height: polymorphic scene header (ADR-0062).
    const BAND_ROWS: u16 = 2;
    let body = Rect {
        y: area.y.saturating_add(BAND_ROWS),
        height: area.height.saturating_sub(BAND_ROWS),
        ..area
    };

    let all_entries = dock_entries(rows);
    let entries: Vec<DockEntry<'_>> = if filter.trim().is_empty() {
        all_entries
    } else {
        let q = filter.to_lowercase();
        all_entries
            .into_iter()
            .filter(|e| {
                e.workspace.to_lowercase().contains(&q)
                    || thread_display_title(e.row).to_lowercase().contains(&q)
                    || e.row.id.to_lowercase().contains(&q)
                    || format!("#{}", e.seq).contains(&q)
                    || dock_status_label(e.row.status).contains(&q)
            })
            .collect()
    };
    let selected = selected.min(entries.len().saturating_sub(1));
    let selected_seq = entries.get(selected).map(|e| e.seq).unwrap_or(selected + 1);

    // Two-tier responsive layout (evolution):
    // - Wide (width >= 90): Dual-pane side-by-side (Threads Fleet explorer left, Cognitive Dossier + Console right).
    // - Compact (width < 90): Vertical stack (head band, Threads Fleet top, Cognitive Dossier + Console bottom, footer).
    let (header_rect, scene_rect, dock_rect, dossier_rect, console_rect, footer_rect, card_columns) =
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
                    Constraint::Percentage(56), // Threads Fleet Explorer
                    Constraint::Length(1),      // gap
                    Constraint::Percentage(44), // Right pane (Dossier + Console)
                ])
                .split(main_body);
            let dock = h_cols[0];
            let right_pane = h_cols[2];

            let r_rows = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Percentage(62), // Focused Thread Dossier
                    Constraint::Length(1),      // gap
                    Constraint::Percentage(38), // Directives & Receipts Console
                ])
                .split(right_pane);
            let dossier = r_rows[0];
            let console = r_rows[2];
            let cols = dock_columns(dock.width);
            (
                Rect::new(full.x, full.y, full.width, 1),
                Rect::new(full.x, full.y.saturating_add(1), full.width, 1),
                dock,
                Some(dossier),
                console,
                footer,
                cols,
            )
        } else {
            let cols = dock_columns(body.width);
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Percentage(55), // Threads Fleet Table
                    Constraint::Length(1),      // gap
                    Constraint::Percentage(45), // Dossier & Console
                    Constraint::Length(1),      // gap
                    Constraint::Length(3),      // footer
                ])
                .split(body);
            let dock = chunks[0];
            let console = chunks[2];
            let footer = chunks[4];
            (
                Rect::new(full.x, full.y, full.width, 1),
                Rect::new(full.x, full.y.saturating_add(1), full.width, 1),
                dock,
                None,
                console,
                footer,
                cols,
            )
        };

    // The head band, drawn by the shared renderer so the dashboard is
    // chrome-identical to every other scene (ADR-0062). Row 1 is
    // the client TabBar; row 2 is the Polymorphic SceneBar (`dashboard` + fleet summary).
    let tab_rects = if let Some(bar) = tab_bar.as_ref() {
        crate::shell::render::draw_tab_bar(frame, header_rect, bar, theme)
    } else {
        crate::shell::view_header::TabBarNavRects::default()
    };
    let (summary, needs_attention) = header_content(rows);
    crate::shell::render::draw_scene_bar(
        frame,
        scene_rect,
        None,
        &crate::shell::render::ViewHints {
            kind: crate::shell::surfaces::SceneKind::Dashboard,
            context: Some(summary.trim_end()),
            context_warn: needs_attention,
            breadcrumbs: None,
            can_back: false,
            can_forward: false,
        },
        theme,
    );

    if view_usage {
        let v_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(0),    // main body
                Constraint::Length(1), // gap
                Constraint::Length(3), // footer
            ])
            .split(body);
        let usage_rect = v_chunks[0];
        let usage_footer_rect = v_chunks[2];

        let usage_body = draw_model_usage_view(
            frame,
            usage_rect,
            usage_report,
            usage_loading,
            usage_scroll,
            theme,
        );

        render_footer(
            frame,
            usage_footer_rect,
            focus,
            prompting,
            prompt_create_new,
            prompt_text,
            cursor_position,
            theme,
            show_caret,
            filtering,
            true,
        );

        return DashboardRects {
            area,
            list_body: usage_body,
            detail_body: None,
            tab_rects: tab_rects.tab_rects,
            tab_bar_rect: Some(header_rect),
            tab_prev_rect: tab_rects.prev_rect,
            tab_next_rect: tab_rects.next_rect,
            tab_overflow_rect: tab_rects.overview_rect,
            tab_notices_rect: tab_rects.notices_rect,
            tab_help_rect: tab_rects.help_rect,
        };
    }

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

    let (list_body, detail_body) = if let Some(dossier) = dossier_rect {
        let dossier_body = draw_thread_dossier(
            frame,
            dossier,
            entries.get(selected).map(|e| e.row),
            selected_seq,
            focus == DashboardFocus::Detail,
            detail_scroll,
            theme,
        );
        let mut console_scroll = 0usize;
        let _ = draw_console_log(frame, console_rect, log, false, &mut console_scroll, theme);
        if focus == DashboardFocus::List {
            (dock_body, Some(dossier_body))
        } else {
            (dossier_body, Some(dock_body))
        }
    } else {
        let console_body = draw_console(
            frame,
            console_rect,
            log,
            entries.get(selected).map(|e| e.row),
            focus == DashboardFocus::Detail,
            detail_scroll,
            theme,
        );
        if focus == DashboardFocus::List {
            (dock_body, Some(console_body))
        } else {
            (console_body, Some(dock_body))
        }
    };

    render_footer(
        frame,
        footer_rect,
        focus,
        prompting,
        prompt_create_new,
        prompt_text,
        cursor_position,
        theme,
        show_caret,
        filtering,
        false,
    );

    DashboardRects {
        area,
        list_body,
        detail_body,
        tab_rects: tab_rects.tab_rects,
        tab_bar_rect: Some(header_rect),
        tab_prev_rect: tab_rects.prev_rect,
        tab_next_rect: tab_rects.next_rect,
        tab_overflow_rect: tab_rects.overview_rect,
        tab_notices_rect: tab_rects.notices_rect,
        tab_help_rect: tab_rects.help_rect,
    }
}

/// The dashboard's scene-row context: the fleet summary the shared head band
/// renders after the `dashboard` scene name.
///
/// The rows themselves are drawn by [`crate::shell::render::draw_tab_bar`] and
/// [`crate::shell::render::draw_scene_bar`] — the same band every other scene
/// uses (ADR-0024: one head, one scene row). This function only
/// computes the dashboard's own context string and its attention flag.
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

/// Render the Thread Fleet Table (ADR-0129 [INV-DASH-01]): a structured,
/// multi-column supervisory table that leads with authentic thread titles,
/// real-time status chips, workspace identity, active tools/turns, and token
/// counts.
#[allow(clippy::too_many_arguments)]
fn draw_fleet_table(
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

        let sel_marker = if is_selected { "›" } else { " " };
        let cur_marker = if is_current { "●" } else { " " };
        let (status_text, status_st) = status_chip(row.status, theme);

        let lineage = match row.fork_kind {
            nuo_wire::SessionForkKind::Trunk | nuo_wire::SessionForkKind::Subagent => String::new(),
            nuo_wire::SessionForkKind::Fork => {
                let address = entry.fork_point.map(|p| p.label()).unwrap_or_default();
                match (address.is_empty(), entry.parent_seq) {
                    (false, Some(parent)) => format!("\u{21b2}{address}#{parent} "),
                    (false, None) => format!("\u{21b2}{address} "),
                    (true, Some(parent)) => format!("\u{21b2}#{parent} "),
                    (true, None) => "\u{21b2} ".to_string(),
                }
            }
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
fn draw_compact_cards(
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
fn dock_columns(body_width: u16) -> usize {
    (body_width as usize / DOCK_COLUMN_W).clamp(1, 4)
}

/// Target width of one session card. Sized so a card always fits the
/// sequence tag, a useful slice of workspace name, uptime, and status.
const DOCK_COLUMN_W: usize = 36;

/// One session card as a single line of spans:
/// `#3  nuo  2h14m  running` — sequence · workspace · uptime · status.
/// The workspace name is the flexible field, truncated to fit; everything
/// else has a fixed reservation.
fn dock_card_line(
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

    // Fixed reservations: marker+space (2) · seq (5) · uptime (7) ·
    // status (15) · three 2-cell gutters (6). The name takes the rest.
    const FIXED: usize = 2 + 5 + 7 + 15 + 6;
    let name_w = cell_width.saturating_sub(FIXED).max(4);
    // Lineage badge (fork surfacing, ADR-0120). A grid cannot nest, so a branch
    // states its origin positionally — `⇂r2.1#3` reads "forked at r2.1 from
    // card #3" — while a trunk keeps the plain name (there is exactly one main
    // line and it needs no marker). The fork address comes from the store; the
    // parent sequence is resolved from the dock itself.
    let lineage = match row.fork_kind {
        nuo_wire::SessionForkKind::Trunk => String::new(),
        nuo_wire::SessionForkKind::Subagent => String::new(),
        nuo_wire::SessionForkKind::Fork => {
            let address = entry
                .fork_point
                .map(|point| point.label())
                .unwrap_or_default();
            match (address.is_empty(), entry.parent_seq) {
                (false, Some(parent)) => format!("\u{21b2}{address}#{parent} "),
                (false, None) => format!("\u{21b2}{address} "),
                (true, Some(parent)) => format!("\u{21b2}#{parent} "),
                (true, None) => "\u{21b2} ".to_string(),
            }
        }
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
/// server's receipt — with the selected session's live monitor read-out
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

/// Render the focused thread's cognitive mission dossier and live telemetry (ADR-0129).
fn draw_thread_dossier(
    frame: &mut Frame,
    area: Rect,
    row: Option<&MonitoredSession>,
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

/// Render the directives and receipts cockpit console log (ADR-0116, ADR-0129).
fn draw_console_log(
    frame: &mut Frame,
    area: Rect,
    log: &[ConsoleLine],
    focused: bool,
    scroll: &mut usize,
    theme: &Theme,
) -> Rect {
    let (_, body) = inset_panel(frame, area, "Directives & Receipts", focused, theme);
    let mut lines = Vec::new();
    if log.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("Fleet Control: ", Style::default().fg(theme.brand())),
            Span::styled(
                "@3 text sends  /kill /interrupt /suspend /new  bare text prompts selection",
                Style::default().fg(theme.muted()),
            ),
        ]));
    } else {
        for entry in log {
            lines.push(entry.to_line(theme));
        }
    }
    render_scrollable(frame, body, lines, scroll, None, theme);
    body
}

/// Render the decoupled Model Usage & System Analytics view on the dashboard (ADR-0129 [INV-USAGE-01]).
fn draw_model_usage_view(
    frame: &mut Frame,
    area: Rect,
    report: Option<&nuo_wire::usage_stats::UsageStatsReport>,
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
    let mut para = Paragraph::new(lines).scroll(*scroll as u16, 0);
    frame.render_widget(&mut para, body);
    nuo_ui_primitives::primitives::draw_scrollbar(frame, body, *scroll, max_scroll, theme);
}

#[allow(clippy::too_many_arguments)]
fn render_footer(
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
            let caret_col = super::common::caret_column(prompt_text, cursor_position) as usize;
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
                " (Enter apply · Esc clear)".to_string(),
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
            let caret_col = super::common::caret_column(prompt_text, cursor_position) as usize;
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

/// The session preview modal: a centered, read-only drill-in
/// on one session, opened by Enter on a dock selection. Selection alone
/// never opens it; Esc closes. Read-only — attaching is `a` on the dock.
pub fn draw_session_preview(
    frame: &mut Frame,
    row: Option<&MonitoredSession>,
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

// the console's command grammar

/// One line of the console transcript: what was dispatched and how the
/// server answered. Kept as typed data (not preformatted strings) so the
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
    /// The server's answer to a dispatch: `ok` receipts in `theme.ok()`,
    /// failures in `theme.err()` with the server's error text.
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
                    Span::styled(INPUT_PROMPT_PREFIX, Style::default().fg(theme.brand())),
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

/// A parsed console directive (grammar plus slash verbs).
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
    /// `? text` / `/ask text` — ask the Archivist, the
    /// nuo-level retrieval agent, one question. The answer arrives as a
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

/// Parse one console line. The grammar (plus verbs):
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

    /// ADR-0024: the dashboard carries the same two-row head band as every
    /// other scene — row 1 the uniform session identity, row 2 the scene name
    /// (`dashboard`) with the live fleet summary as its context and the
    /// namespace pair on the right. This pins the rendered rows end to end,
    /// because the layout split is what makes the scene row reachable.
    #[test]
    fn dashboard_head_renders_the_shared_three_tier_band() {
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
                    cursor_position: 0,
                    current_session_id: "a-1",
                    show_caret: false,
                    tab_bar: Some(TabBarProps::default()),
                    filter: "",
                    filtering: false,
                    view_usage: false,
                    usage_report: None,
                    usage_loading: false,
                    usage_scroll: 0,
                },
                &theme,
            );
        });

        let buffer = terminal.buffer();
        let row_text = |y: u16| -> String {
            buffer
                .cells()
                .iter()
                .skip(y as usize * 100)
                .take(100)
                .map(|c| c.symbol.as_str())
                .collect()
        };

        // Row 1: the client TabBar / brand and namespace pair (ADR-0060, ADR-0062).
        let tabbar = row_text(0);
        assert!(
            tabbar.contains("help") || tabbar.contains("C-x"),
            "row 1 advertises client affordance: {tabbar:?}"
        );

        // Row 2: the polymorphic SceneBar (ADR-0062).
        let scene = row_text(1);
        assert!(
            scene.contains("dashboard"),
            "scene name on row 2: {scene:?}"
        );
        assert!(
            scene.contains("1 session(s)"),
            "fleet summary context: {scene:?}"
        );
        assert!(scene.contains("1 running"), "running count: {scene:?}");
        assert!(
            !scene.contains("Esc"),
            "Esc is never advertised as a scene exit: {scene:?}"
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
            title: None,
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
            fork_point: None,
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
            row("one", 1, "/home/ming/projects/nuo", SessionStatus::Idle),
            row("two", 2, "/home/ming/projects/app", SessionStatus::Idle),
        ];
        let entries = dock_entries(&rows);
        assert_eq!(entries[0].workspace, "nuo");
        assert_eq!(entries[1].workspace, "app");

        // Same basename in two different parents → both fall back to full
        // paths (a bare "src" would be ambiguous AND meaningless).
        let rows = vec![
            row("one", 1, "/home/ming/projects/nuo", SessionStatus::Idle),
            row("two", 2, "/tmp/worktree/nuo", SessionStatus::Idle),
        ];
        let entries = dock_entries(&rows);
        assert_eq!(entries[0].workspace, "/home/ming/projects/nuo");
        assert_eq!(entries[1].workspace, "/tmp/worktree/nuo");
    }

    #[test]
    fn workspace_basename_never_shows_a_parent() {
        assert_eq!(workspace_basename("/home/ming/projects/nuo"), "nuo");
        assert_eq!(workspace_basename("/home/ming/projects/nuo/"), "nuo");
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
            "/work/nuo",
            SessionStatus::Running,
        );
        let entries = dock_entries(std::slice::from_ref(&r));
        let line = dock_card_line(&entries[0], 60, false, false, now, &theme);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("#1"), "{text}");
        assert!(text.contains("nuo"), "{text}");
        assert!(text.contains("2h14m"), "{text}");
        assert!(text.contains("running"), "{text}");
        assert!(
            !text.contains('▶'),
            "dock card must not contain '▶': {text}"
        );

        let cur_line = dock_card_line(&entries[0], 60, true, false, now, &theme);
        let cur_text: String = cur_line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            !cur_text.contains('▶'),
            "current dock card must not contain '▶': {cur_text}"
        );
    }

    #[test]
    fn dock_card_states_lineage_as_a_back_reference() {
        // A card grid has no second axis to nest along (ADR-0120 Alternative E),
        // so lineage is stated positionally: `⇂r2.1#1` reads "forked at r2.1
        // from card #1". A trunk carries no marker — there is exactly one main
        // line and it needs none.
        let theme = Theme::default();
        let now = 1_000u64;

        let mut trunk = row("t", 1, "/work/main", SessionStatus::Running);
        trunk.fork_kind = nuo_wire::SessionForkKind::Trunk;
        let entries = dock_entries(std::slice::from_ref(&trunk));
        let line = dock_card_line(&entries[0], 60, false, false, now, &theme);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            !text.contains('\u{21b2}'),
            "a trunk carries no lineage badge: {text}"
        );

        // An explicit fork with a recorded address, parent present in the dock.
        let trunk_row = row("t", 1, "/work/main", SessionStatus::Running);
        let mut fork = row("a", 2, "/work/main", SessionStatus::Running);
        fork.parent_id = Some("t".into());
        fork.fork_kind = nuo_wire::SessionForkKind::Fork;
        fork.fork_point = Some(nuo_wire::ForkPoint::at_turn(2, 1));
        let rows = vec![trunk_row, fork];
        let entries = dock_entries(&rows);
        let fork_entry = entries.iter().find(|e| e.row.id == "a").expect("fork card");
        let line = dock_card_line(fork_entry, 60, false, false, now, &theme);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            text.contains("\u{21b2}r2.1#1"),
            "a branch names its fork address and origin card: {text}"
        );

        // A branch whose parent is not in the dock still states the address.
        let mut orphan = row("f", 3, "/work/main", SessionStatus::Idle);
        orphan.parent_id = Some("absent".into());
        orphan.fork_kind = nuo_wire::SessionForkKind::Fork;
        orphan.fork_point = Some(nuo_wire::ForkPoint::at_round(4));
        let entries = dock_entries(std::slice::from_ref(&orphan));
        let line = dock_card_line(&entries[0], 60, false, false, now, &theme);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            text.contains("\u{21b2}r4"),
            "an anchorless branch still states where it diverged: {text}"
        );
        assert!(
            !text.contains("#0"),
            "no card is invented for an absent parent: {text}"
        );
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
        let r = row("x", now, "/work/nuo", SessionStatus::Running);
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
        // `? text` asks the nuo-level retrieval agent.
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
        // token stays in the payload (keeps the grammar
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
        let mut r = row("s-123", 1000, "/work/nuo", SessionStatus::Running);
        r.digest = Some(nuo_wire::SessionDigest {
            title: "Refactor Auth Middleware".to_string(),
            intent: "Rewrite token validator to support ECDSA signatures.".to_string(),
            history: vec![
                "Created test suite for ECDSA".to_string(),
                "Updated Cargo.toml dependencies".to_string(),
            ],
        });
        r.current_tool = Some("execute_command".to_string());

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
        assert!(rendered.contains("execute_command"), "{rendered}");
    }

    #[test]
    fn session_detail_lines_falls_back_to_overview_when_no_digest() {
        let theme = Theme::default();
        let mut r = row("s-456", 1000, "/work/nuo", SessionStatus::Idle);
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
        use nuo_ui_primitives::elevation::ElevationArchetype;
        use nuotc::Rect;
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

    #[test]
    fn thread_display_title_prioritizes_authentic_title_over_digest_and_overview() {
        let mut r = row("t-1", 100, "/work/test", SessionStatus::Running);
        r.overview.clear();
        assert_eq!(thread_display_title(&r), "untitled thread");

        r.overview = "first prompt text".to_string();
        assert_eq!(thread_display_title(&r), "first prompt text");

        r.digest = Some(nuo_wire::cognitive::SessionDigest {
            title: "Digest Mission Title".to_string(),
            intent: "fix bug".to_string(),
            history: Vec::new(),
        });
        assert_eq!(thread_display_title(&r), "Digest Mission Title");

        r.title = Some("Authentic AI Thread Title".to_string());
        assert_eq!(thread_display_title(&r), "Authentic AI Thread Title");
    }

    #[test]
    fn fmt_tokens_compact_formats_correctly() {
        assert_eq!(fmt_tokens_compact(0), "0");
        assert_eq!(fmt_tokens_compact(842), "842");
        assert_eq!(fmt_tokens_compact(1_500), "1.5k");
        assert_eq!(fmt_tokens_compact(14_200), "14.2k");
        assert_eq!(fmt_tokens_compact(1_250_000), "1.25M");
    }

    #[test]
    fn status_chip_provides_distinct_visual_vocabulary() {
        let theme = Theme::default();
        let (label_run, _) = status_chip(SessionStatus::Running, &theme);
        assert!(label_run.contains("RUNNING"));
        let (label_app, _) = status_chip(SessionStatus::NeedsApproval, &theme);
        assert!(label_app.contains("APPROVAL"));
        let (label_inp, _) = status_chip(SessionStatus::NeedsInput, &theme);
        assert!(label_inp.contains("INPUT"));
        let (label_fail, _) = status_chip(SessionStatus::Failed, &theme);
        assert!(label_fail.contains("FAILED"));
        let (label_stop, _) = status_chip(SessionStatus::Interrupted, &theme);
        assert!(label_stop.contains("STOPPED"));
        let (label_idle, _) = status_chip(SessionStatus::Idle, &theme);
        assert!(label_idle.contains("IDLE"));
    }

    #[test]
    fn draw_fleet_table_renders_structured_headers_and_thread_titles() {
        let theme = Theme::default();
        let mut r1 = row("a-1", 100, "/work/repo", SessionStatus::Running);
        r1.title = Some("Refactor Database Pool".to_string());
        r1.current_tool = Some("bash".to_string());
        r1.output_tokens = 14_200;

        let rows = [r1];
        let entries = dock_entries(&rows);
        let mut terminal = nuotc::TestTerminal::new(100, 20);
        let mut scroll = 0usize;
        terminal.draw(|f| {
            draw_fleet_table(
                f,
                Rect::new(0, 0, 100, 20),
                &entries,
                0,
                true,
                &mut scroll,
                true,
                &theme,
                "a-1",
                200,
            );
        });

        let text = terminal
            .buffer()
            .cells()
            .iter()
            .map(|c| c.symbol.as_str())
            .collect::<String>();
        assert!(text.contains("Threads Fleet"), "{text}");
        assert!(text.contains("THREAD / MISSION"), "{text}");
        assert!(text.contains("Refactor Database Pool"), "{text}");
        assert!(text.contains("RUNNING"), "{text}");
        assert!(text.contains("tool: bash"), "{text}");
        assert!(text.contains("14.2k"), "{text}");
    }

    #[test]
    fn draw_model_usage_view_renders_usage_analytics_independently() {
        let theme = Theme::default();
        let mut terminal = nuotc::TestTerminal::new(100, 20);
        terminal.draw(|f| {
            draw_model_usage_view(f, Rect::new(0, 0, 100, 20), None, true, 0, &theme);
        });
        let text = terminal
            .buffer()
            .cells()
            .iter()
            .map(|c| c.symbol.as_str())
            .collect::<String>();
        assert!(text.contains("Model Usage & System Analytics"), "{text}");
        assert!(
            text.contains("Loading cross-session model usage statistics"),
            "{text}"
        );
    }
}
