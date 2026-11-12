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
use nuo_protocol::{MonitoredThread, ThreadHosting, ThreadStatus};
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

/// entries; `current_session_id` marks the session this TUI is attached to.
/// `focus` selects whether ↑/↓/PgUp/PgDn move the dock selection or scroll
/// the console. `prompting` shows the inline new-session prompt line in
/// place of the footer command strip. `log` is the console's receipt
/// transcript (every dispatched directive and the server's answer).
/// Properties for rendering the full-screen session dashboard.
pub struct DashboardProps<'a> {
    pub rows: &'a [MonitoredThread],
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
    pub usage_report: Option<&'a nuo_protocol::usage_stats::UsageStatsReport>,
    /// Whether a model usage report fetch is in flight over the control plane.
    pub usage_loading: bool,
    /// Scroll offset for the model usage analytics report view.
    pub usage_scroll: usize,
}

/// Resolve the authentic semantic display title for a thread (ADR-0050, ADR-0129).
pub fn thread_display_title(row: &MonitoredThread) -> &str {
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
pub fn status_chip(status: ThreadStatus, theme: &Theme) -> (&'static str, Style) {
    match status {
        ThreadStatus::Running => (
            if !theme.elevation.is_chromatic() {
                "* RUNNING"
            } else {
                "● RUNNING"
            },
            Style::default()
                .fg(theme.brand())
                .add_modifier(Modifier::BOLD),
        ),
        ThreadStatus::NeedsApproval => (
            if !theme.elevation.is_chromatic() {
                "! APPROVAL"
            } else {
                "▲ APPROVAL"
            },
            Style::default()
                .fg(theme.warn())
                .add_modifier(Modifier::BOLD),
        ),
        ThreadStatus::NeedsInput => (
            "? INPUT",
            Style::default()
                .fg(theme.warn())
                .add_modifier(Modifier::BOLD),
        ),
        ThreadStatus::Failed => (
            "✕ FAILED",
            Style::default()
                .fg(theme.err())
                .add_modifier(Modifier::BOLD),
        ),
        ThreadStatus::Interrupted => ("■ STOPPED", Style::default().fg(theme.dim())),
        ThreadStatus::Idle => ("○ IDLE", Style::default().fg(theme.muted())),
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

pub mod console;
pub mod dock;
pub mod fleet;
pub mod preview;

#[cfg(test)]
mod tests;

pub use console::{ConsoleCommand, ConsoleLine, ConsoleVerb, parse_console_command};
pub use dock::{creation_order, format_uptime};
pub use preview::draw_session_preview;

pub(crate) use console::{draw_console, draw_console_log};
pub(crate) use dock::{
    DockEntry, dock_columns, dock_entries, dock_status_label, draw_dock, format_padded,
    now_unix_secs, truncate_display,
};
#[cfg(test)]
pub(crate) use dock::{dock_card_line, workspace_basename};
pub(crate) use fleet::{
    draw_fleet_table, draw_model_usage_view, draw_thread_dossier, header_content, inset_panel,
    render_footer, render_scrollable,
};
pub(crate) use preview::{session_detail_lines, status_style};
