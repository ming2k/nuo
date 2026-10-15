//! Three-tier spatial head band for every scene (ADR-0060).
//!
//! The head band is the fixed, always-present chrome pinned to the terminal's
//! top edge. Under ADR-0060 it is structured across three sovereign tiers
//! (`PAGE_HEADER_ROWS = 3`), eliminating horizontal budget collisions and
//! enforcing strict domain stratification:
//!
//! - **Row 1 — Client Global TabBar (`draw_view_header`).**
//!   Fixed height: 1 row. Owned by the Client container. Projects open tabs
//!   with clean semantic titles (e.g. `Refactor DB`, `dashboard`, `settings`)
//!   or compact untitled fallbacks (`thread#b3c4`), strictly omitting ordinal
//!   prefixes (`1:`) [INV-TAB-09]. Carries the standing `C-x menu` client affordance
//!   on the right edge.
//!
//! - **Row 2 — Thread Identity & Governance Band (`draw_thread_governance_header`).**
//!   Fixed height: 1 row. Owned by the active thread. Displays the ambient
//!   thread identity (`THREAD <id_tail>`, `[ROLE]` badge, tilde-shortened
//!   workspace path) on the left, and persistent safety execution posture
//!   flags (`UNATTENDED`, `UNCONFINED`) on the right edge [INV-HEAD-04, INV-THREAD-02].
//!
//! - **Row 3 — Scene Head & Navigation Stack (`draw_view_header_hints`).**
//!   Fixed height: 1 row. Owned by the active scene view. Displays primary
//!   authentic task title or domain scene context on the left (with the vestigial
//!   `thread` placeholder eliminated for untitled threads [INV-SCENE-01]), and
//!   right-aligned drill-in navigation stack / breadcrumbs (`< thread > subagent #1 > diff`).

use nuotc::{Frame, Line, Modifier, Paragraph, Rect, Span, Style};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::surfaces::SceneKind;
use super::{STEP_MIN_WIDTH, TRANSCRIPT_H_INSET, Theme};

/// Canonical scene identifier alias for header presentation (ADR-0042).
pub(crate) type ViewKind = crate::surfaces::SceneKind;

/// Maximum display width allocated to individual tab titles before truncation.
pub(crate) const MAX_TAB_TITLE_WIDTH: usize = 20;

/// Row-3 (scene) context for every scene (ADR-0060).
///
/// Focuses purely on viewport context: authentic task title or domain summary
/// on the left, and navigation stack / drill-in breadcrumbs on the right.
/// Thread governance (workspace, role, safety flags) is hosted on Row 2.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ViewHints<'a> {
    /// Which scene the row describes — decides the leading scene label.
    pub kind: ViewKind,
    /// Scene context shown: the thread's authentic chat title, a subagent's
    /// task label, the dashboard's fleet summary, or the settings breadcrumb.
    pub context: Option<&'a str>,
    /// `true` while the context is an attention state (e.g. the dashboard has
    /// a session needing approval), escalating it to the warning tone.
    pub context_warn: bool,
    /// Optional breadcrumbs from the active tab's history stack (ADR-0040, ADR-0042, ADR-0060).
    pub breadcrumbs: Option<&'a [crate::surfaces::SceneKind]>,
    /// Whether back navigation is available in the active tab.
    pub can_back: bool,
    /// Whether forward navigation is available in the active tab.
    pub can_forward: bool,
}

impl<'a> ViewHints<'a> {
    #[allow(dead_code)]
    pub fn simple(kind: ViewKind) -> Self {
        Self {
            kind,
            context: None,
            context_warn: false,
            breadcrumbs: None,
            can_back: false,
            can_forward: false,
        }
    }
}

/// Canonical thread identity & governance state for header presentation (ADR-0043, ADR-0060 [INV-THREAD-02]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ThreadHead<'a> {
    /// The thread's persistent id (full string). Only its last four
    /// characters are shown, dimmed, as a disambiguating tag.
    pub thread_id: &'a str,
    /// Tilde-shortened workspace path (e.g. `~/projects/xx`), or empty when unbound.
    pub workspace: &'a str,
    /// Active staffing role for this thread (ADR-0244).
    pub role: Option<&'a str>,
    /// Client-owned tabs mounted in this viewport (ADR-0039, ADR-0060).
    pub tabs: Option<&'a [crate::surfaces::ClientTab]>,
    /// Active tab index within `tabs`.
    pub active_tab: usize,
    /// `true` while the thread runs unattended (`--unattended` / `/unattended on`).
    pub unattended: bool,
    /// `false` while workspace filesystem confinement is off (`/confinement off`).
    pub confined: bool,
}

#[allow(dead_code)]
impl<'a> ThreadHead<'a> {
    pub fn new(
        thread_id: &'a str,
        workspace: &'a str,
        role: Option<&'a str>,
        unattended: bool,
        confined: bool,
    ) -> Self {
        Self {
            thread_id,
            workspace,
            role,
            tabs: None,
            active_tab: 0,
            unattended,
            confined,
        }
    }
}

/// Draw Tier 1: Client Global TabBar (ADR-0039, ADR-0040, ADR-0060 [INV-TAB-09]).
///
/// Projects open tabs with clean semantic titles without ordinal prefixes (`1:`),
/// with the client command namespace (`C-x menu`) anchored to the right edge.
/// Returns bounding rects for mouse click hit-testing.
pub(crate) fn draw_view_header(
    frame: &mut Frame,
    rect: Rect,
    head: &ThreadHead<'_>,
    theme: &Theme,
) -> Vec<(usize, Rect)> {
    let full_width = rect.width as usize;
    if full_width < STEP_MIN_WIDTH || rect.height == 0 {
        return Vec::new();
    }

    let bg = theme.raised();
    let fill = Style::default().bg(bg);
    let pad = TRANSCRIPT_H_INSET as usize;
    let text_width = full_width.saturating_sub(2 * pad);

    // C-x affordance belongs to the client-level TabBar / Header (ADR-0043 [INV-UI-02])
    let affordance = crate::components::keycap::KeyAffordance::tabbar_menu(SCENE_NAMESPACE_LABEL);
    let right_width = affordance.width();

    // Tab workspace rendering (ADR-0039 [INV-TAB-01], ADR-0040 [INV-UI-01], ADR-0060 [INV-TAB-09])
    if let Some(tabs) = head.tabs.filter(|t| !t.is_empty()) {
        let mut spans = vec![Span::styled(" ".repeat(pad), fill)];
        let mut tab_rects = Vec::with_capacity(tabs.len());
        let mut current_x = rect.x.saturating_add(pad as u16);
        let available_for_tabs = text_width.saturating_sub(right_width);
        let mut used_tab_w = 0usize;

        for (i, tab) in tabs.iter().enumerate() {
            let active = i == head.active_tab;
            let label = match &tab.kind {
                crate::surfaces::TabKind::Thread(id) => {
                    if let Some(title) = tab.display_title() {
                        let truncated = truncate_to_width(title, MAX_TAB_TITLE_WIDTH);
                        truncated.to_string()
                    } else {
                        format!("thread#{}", id_tail(id))
                    }
                }
                crate::surfaces::TabKind::Dashboard => "dashboard".to_string(),
                crate::surfaces::TabKind::Settings => "settings".to_string(),
            };

            let text = format!(" {label} ");
            let tab_w = text.width();
            let separator_w = if i + 1 < tabs.len() { 1 } else { 0 };

            if used_tab_w + tab_w > available_for_tabs && i > 0 {
                break;
            }

            let tab_style = if theme.elevation == nuotc::ElevationArchetype::Structured {
                if active {
                    Style::default().add_modifier(Modifier::REVERSE | Modifier::BOLD)
                } else {
                    Style::default().fg(theme.dim())
                }
            } else if active {
                Style::default()
                    .bg(theme.selected_bg)
                    .fg(theme.heading())
                    .add_modifier(Modifier::BOLD)
            } else {
                let inactive_bg = if i % 2 == 0 {
                    theme.panel()
                } else {
                    theme.body()
                };
                Style::default().bg(inactive_bg).fg(theme.text_muted)
            };

            if current_x < rect.right() {
                let w = (tab_w as u16).min(rect.right().saturating_sub(current_x));
                if w > 0 {
                    tab_rects.push((i, Rect::new(current_x, rect.y, w, 1)));
                }
            }

            spans.push(Span::styled(text, tab_style));
            current_x = current_x.saturating_add(tab_w as u16);
            used_tab_w += tab_w;

            if separator_w > 0 {
                spans.push(Span::styled(" ", fill));
                current_x = current_x.saturating_add(1);
                used_tab_w += 1;
            }
        }

        let gap = text_width.saturating_sub(used_tab_w + right_width);
        spans.push(Span::styled(" ".repeat(gap), fill));
        let [key_span, label_span] = affordance.render_tabbar_spans(theme, bg);
        spans.push(key_span);
        spans.push(label_span);
        spans.push(Span::styled(" ".repeat(pad), fill));
        frame.render_widget(Paragraph::new(Line::from(spans)), rect);
        return tab_rects;
    }

    // Fallback brand header when no tabs are mounted
    let brand_style = fill.fg(theme.brand()).add_modifier(Modifier::BOLD);
    const BRAND: &str = "nuo";
    let brand_width = BRAND.width();
    let gap = text_width.saturating_sub(brand_width + right_width);

    let mut spans = vec![Span::styled(" ".repeat(pad), fill)];
    spans.push(Span::styled(BRAND, brand_style));
    spans.push(Span::styled(" ".repeat(gap), fill));
    let [key_span, label_span] = affordance.render_tabbar_spans(theme, bg);
    spans.push(key_span);
    spans.push(label_span);
    spans.push(Span::styled(" ".repeat(pad), fill));

    frame.render_widget(Paragraph::new(Line::from(spans)), rect);
    Vec::new()
}

/// Draw Tier 2: Thread Identity & Governance Band (ADR-0043 [INV-THREAD-02], ADR-0060 [INV-HEAD-04]).
///
/// Anchors thread execution boundaries: thread id tail, staffing `[ROLE]` badge,
/// and bound workspace path on the left; persistent safety posture flags
/// (`UNATTENDED`, `UNCONFINED`) on the right.
pub(crate) fn draw_thread_governance_header(
    frame: &mut Frame,
    rect: Rect,
    head: &ThreadHead<'_>,
    theme: &Theme,
) {
    let full_width = rect.width as usize;
    if full_width < STEP_MIN_WIDTH || rect.height == 0 {
        return;
    }

    let bg = theme.raised();
    let fill = Style::default().bg(bg);
    let pad = TRANSCRIPT_H_INSET as usize;
    let text_width = full_width.saturating_sub(2 * pad);

    let title_style = fill.fg(theme.heading()).add_modifier(Modifier::BOLD);
    let tag_style = fill.fg(theme.dim());
    let badge_style = fill.fg(theme.brand()).add_modifier(Modifier::BOLD);
    let ws_style = fill.fg(theme.dim());
    let flag_style = fill.fg(theme.warn()).add_modifier(Modifier::BOLD);

    let tag = id_tail(head.thread_id);
    let badge = head
        .role
        .map(|r| format!("[{}]", r.to_uppercase()))
        .unwrap_or_default();

    // Right-aligned persistent safety execution posture flags
    let mut flags = String::new();
    if head.unattended {
        flags.push_str("UNATTENDED ");
    }
    if !head.confined {
        flags.push_str("UNCONFINED ");
    }
    let flags_width = flags.width();

    const TITLE: &str = "THREAD ";
    let title_w = TITLE.width();
    let tag_w = if tag.is_empty() { 0 } else { tag.width() + 1 };
    let badge_w = if badge.is_empty() { 0 } else { badge.width() + 1 };
    let base_left_w = title_w + tag_w + badge_w;

    // Remaining budget for workspace path
    let ws_budget = text_width.saturating_sub(base_left_w + flags_width + 1);
    let truncated_ws = if !head.workspace.is_empty() {
        truncate_to_width(head.workspace, ws_budget)
    } else {
        String::new()
    };
    let ws_w = if truncated_ws.is_empty() {
        0
    } else {
        truncated_ws.width() + 1
    };

    let total_left_w = base_left_w + ws_w;
    let gap = text_width.saturating_sub(total_left_w + flags_width);

    let mut spans = vec![Span::styled(" ".repeat(pad), fill)];
    spans.push(Span::styled(TITLE, title_style));
    if !tag.is_empty() {
        spans.push(Span::styled(format!("{tag} "), tag_style));
    }
    if !badge.is_empty() {
        spans.push(Span::styled(format!("{badge} "), badge_style));
    }
    if !truncated_ws.is_empty() {
        spans.push(Span::styled(format!("{truncated_ws} "), ws_style));
    }
    spans.push(Span::styled(" ".repeat(gap), fill));
    if !flags.is_empty() {
        spans.push(Span::styled(flags, flag_style));
    }
    spans.push(Span::styled(" ".repeat(pad), fill));

    frame.render_widget(Paragraph::new(Line::from(spans)), rect);
}

/// Draw Tier 3: Scene Head & Navigation Stack (ADR-0060 [INV-HEAD-04], [INV-SCENE-01]).
///
/// Left side projects the primary authentic task title or domain scene context.
/// On root conversation view, dummy `thread` placeholders are eliminated.
/// Right side projects the drill-in navigation stack / breadcrumb trail
/// (`< thread > subagent #1 > diff`) when stack depth exceeds 1.
pub(crate) fn draw_view_header_hints(
    frame: &mut Frame,
    rect: Rect,
    hints: &ViewHints<'_>,
    theme: &Theme,
) {
    if rect.height == 0 || (rect.width as usize) < STEP_MIN_WIDTH {
        return;
    }

    let bg = theme.raised();
    let fill = Style::default().bg(bg);
    let label_style = fill.fg(theme.brand()).add_modifier(Modifier::BOLD);
    let context_style = if hints.context_warn {
        fill.fg(theme.warn()).add_modifier(Modifier::BOLD)
    } else {
        fill.fg(theme.fg())
    };

    let width = rect.width as usize;
    let pad = TRANSCRIPT_H_INSET as usize;
    let text_width = width.saturating_sub(2 * pad);

    // Right side: Navigation stack / breadcrumbs (when drill-in depth > 1)
    let (right_spans, right_width) = if let Some(crumbs) = hints.breadcrumbs.filter(|c| c.len() > 1) {
        let mut spans = Vec::new();
        let mut w = 0usize;
        if hints.can_back {
            let back_arrow = "< ";
            spans.push(Span::styled(back_arrow, fill.fg(theme.dim())));
            w += back_arrow.width();
        }
        for (idx, crumb) in crumbs.iter().enumerate() {
            let crumb_str = crumb.breadcrumb_label();
            let is_last = idx + 1 == crumbs.len();
            if is_last {
                spans.push(Span::styled(crumb_str, label_style));
                w += crumb_str.width();
            } else {
                spans.push(Span::styled(crumb_str, fill.fg(theme.dim())));
                spans.push(Span::styled(" > ", fill.fg(theme.dim())));
                w += crumb_str.width() + 3;
            }
        }
        (spans, w)
    } else {
        (Vec::new(), 0)
    };

    // Left side: Scene Title or Domain Label + Context
    // Under ADR-0060 [INV-SCENE-01]:
    // - Thread scene: authentic title if present; if untitled, NO placeholder "thread".
    // - Non-thread scene (Dashboard, Settings, Subagent without breadcrumbs): scene_label + context.
    let (lead_label, context_str) = match hints.kind {
        SceneKind::Thread => (None, hints.context),
        other => {
            if hints.breadcrumbs.map_or(true, |c| c.len() <= 1) {
                (Some(other.scene_label()), hints.context)
            } else {
                (None, hints.context)
            }
        }
    };

    let mut left_spans = Vec::new();
    let mut left_width = 0usize;

    if let Some(lbl) = lead_label {
        left_spans.push(Span::styled(lbl, label_style));
        left_width += lbl.width();
    }

    if let Some(ctx) = context_str.filter(|c| !c.is_empty()) {
        let sep_w = if left_width > 0 { 2 } else { 0 };
        let budget = text_width.saturating_sub(left_width + sep_w + right_width);
        let truncated = truncate_to_width(ctx, budget);
        if !truncated.is_empty() {
            if left_width > 0 {
                left_spans.push(Span::styled("  ", fill));
                left_width += 2;
            }
            left_width += truncated.width();
            left_spans.push(Span::styled(truncated, context_style));
        }
    }

    let gap = text_width.saturating_sub(left_width + right_width);
    let mut spans = vec![Span::styled(" ".repeat(pad), fill)];
    spans.extend(left_spans);
    spans.push(Span::styled(" ".repeat(gap), fill));
    spans.extend(right_spans);
    spans.push(Span::styled(" ".repeat(pad), fill));

    frame.render_widget(Paragraph::new(Line::from(spans)), rect);
}

/// The scene row's name for the `C-x` scene namespace (ADR-0023, ADR-0043).
const SCENE_NAMESPACE_LABEL: &str = "menu";

/// Disambiguate thread ids by taking their last four characters.
fn id_tail(id: &str) -> &str {
    let len = id.len();
    if len <= 4 {
        id
    } else {
        &id[len - 4..]
    }
}

/// Truncate `s` to at most `max_width` display columns using an ellipsis character.
fn truncate_to_width(s: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }
    let total_width = s.width();
    if total_width <= max_width {
        return s.to_string();
    }
    if max_width == 1 {
        return "…".to_string();
    }
    let target = max_width.saturating_sub(1);
    let mut current_width = 0;
    let mut end_byte = 0;
    for (idx, ch) in s.char_indices() {
        let ch_w = ch.width().unwrap_or(0);
        if current_width + ch_w > target {
            break;
        }
        current_width += ch_w;
        end_byte = idx + ch.len_utf8();
    }
    format!("{}…", &s[..end_byte])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render_row1(width: u16, head: ThreadHead<'_>) -> String {
        let cells = render_row1_cells(width, head, &Theme::default());
        cells.iter().map(|c| c.symbol()).collect()
    }

    fn render_row1_cells(
        width: u16,
        head: ThreadHead<'_>,
        theme: &Theme,
    ) -> Vec<nuotc::Cell> {
        let mut terminal = nuotc::TestTerminal::new(width, 1);
        terminal.draw(|frame| {
            draw_view_header(frame, frame.area(), &head, theme);
        });
        terminal.buffer().content.clone()
    }

    fn render_row2(width: u16, head: ThreadHead<'_>) -> String {
        let cells = render_row2_cells(width, head, &Theme::default());
        cells.iter().map(|c| c.symbol()).collect()
    }

    fn render_row2_cells(
        width: u16,
        head: ThreadHead<'_>,
        theme: &Theme,
    ) -> Vec<nuotc::Cell> {
        let mut terminal = nuotc::TestTerminal::new(width, 1);
        terminal.draw(|frame| {
            draw_thread_governance_header(frame, frame.area(), &head, theme);
        });
        terminal.buffer().content.clone()
    }

    fn render_row3(width: u16, hints: ViewHints<'_>) -> String {
        let cells = render_row3_cells(width, hints, &Theme::default());
        cells.iter().map(|c| c.symbol()).collect()
    }

    fn render_row3_cells(
        width: u16,
        hints: ViewHints<'_>,
        theme: &Theme,
    ) -> Vec<nuotc::Cell> {
        let mut terminal = nuotc::TestTerminal::new(width, 1);
        terminal.draw(|frame| {
            draw_view_header_hints(frame, frame.area(), &hints, theme);
        });
        terminal.buffer().content.clone()
    }

    fn hints<'a>(kind: ViewKind) -> ViewHints<'a> {
        ViewHints::simple(kind)
    }

    #[test]
    fn row1_renders_tabs_without_ordinal_prefixes() {
        let tabs = vec![
            crate::surfaces::ClientTab::thread("sess-01a2b3c4", None::<String>),
            crate::surfaces::ClientTab::dashboard(),
        ];
        let head = ThreadHead {
            thread_id: "sess-01a2b3c4",
            workspace: "~/projects/xx",
            role: Some("developer"),
            tabs: Some(&tabs),
            active_tab: 0,
            unattended: false,
            confined: true,
        };
        let row = render_row1(80, head);
        assert!(row.contains("thread#b3c4"), "{row}");
        assert!(!row.contains("1:thread#b3c4"), "tabs must not use ordinal prefix: {row}");
        assert!(row.contains("dashboard"), "{row}");
        assert!(!row.contains("2:dashboard"), "tabs must not use ordinal prefix: {row}");
        assert!(row.contains("C-x menu"), "compact client menu on TabBar: {row}");
    }

    #[test]
    fn row1_renders_semantic_tab_titles_with_wider_budget() {
        let tabs = vec![
            crate::surfaces::ClientTab::thread("sess-01a2b3c4", Some("Refactor Database Connection")),
            crate::surfaces::ClientTab::thread("sess-05d6e7f8", None::<String>),
            crate::surfaces::ClientTab::dashboard(),
        ];
        let head = ThreadHead {
            thread_id: "sess-01a2b3c4",
            workspace: "~/projects/xx",
            role: Some("developer"),
            tabs: Some(&tabs),
            active_tab: 0,
            unattended: false,
            confined: true,
        };
        let row = render_row1(80, head);
        assert!(row.contains("Refactor Database C…"), "semantic title projected with wider budget: {row}");
        assert!(!row.contains("1:Refactor"), "no ordinal prefix: {row}");
        assert!(row.contains("thread#e7f8"), "untitled tab falls back to placeholder thread#tail: {row}");
        assert!(row.contains("dashboard"), "{row}");
    }

    #[test]
    fn row1_tab_rects_and_styling() {
        let tabs = vec![
            crate::surfaces::ClientTab::thread("sess-01a2b3c4", None::<String>),
            crate::surfaces::ClientTab::dashboard(),
            crate::surfaces::ClientTab::settings(),
        ];
        let head = ThreadHead {
            thread_id: "sess-01a2b3c4",
            workspace: "~/projects/xx",
            role: Some("developer"),
            tabs: Some(&tabs),
            active_tab: 0,
            unattended: false,
            confined: true,
        };
        let mut terminal = nuotc::TestTerminal::new(80, 1);
        let theme = Theme::default();
        let mut tab_rects = Vec::new();
        terminal.draw(|frame| {
            tab_rects = draw_view_header(frame, frame.area(), &head, &theme);
        });
        assert_eq!(tab_rects.len(), 3);
        assert_eq!(tab_rects[0].0, 0);
        assert_eq!(tab_rects[1].0, 1);
        assert_eq!(tab_rects[2].0, 2);
        assert_eq!(tab_rects[0].1.y, 0);
        assert!(tab_rects[0].1.width > 0);
        assert!(tab_rects[1].1.x > tab_rects[0].1.x);
    }

    #[test]
    fn row1_renders_brand_when_no_tabs() {
        let head = ThreadHead::new("sess-01a2b3c4", "~/projects/xx", Some("developer"), false, true);
        let row = render_row1(80, head);
        assert!(row.contains("nuo"), "brand shown: {row}");
        assert!(row.contains("C-x menu"), "client menu shown: {row}");
    }

    #[test]
    fn row2_renders_thread_governance_and_workspace() {
        let head = ThreadHead {
            thread_id: "sess-01a2b3c4",
            workspace: "~/projects/xx",
            role: Some("developer"),
            tabs: None,
            active_tab: 0,
            unattended: true,
            confined: false,
        };
        let row = render_row2(80, head);
        assert!(row.starts_with("  THREAD b3c4 [DEVELOPER] ~/projects/xx"), "{row}");
        assert!(row.contains("UNATTENDED"), "unattended flag: {row}");
        assert!(row.contains("UNCONFINED"), "unconfined flag: {row}");
    }

    #[test]
    fn row2_workspace_free_hides_workspace_and_shows_role_badge() {
        let head = ThreadHead {
            thread_id: "sess-01a2b3c4",
            workspace: "",
            role: Some("philosophist"),
            tabs: None,
            active_tab: 0,
            unattended: false,
            confined: true,
        };
        let row = render_row2(80, head);
        assert!(row.starts_with("  THREAD b3c4 [PHILOSOPHIST]"), "{row}");
        assert!(!row.contains("~/"), "{row}");
    }

    #[test]
    fn row2_shows_custom_user_role_badge() {
        let head = ThreadHead {
            thread_id: "sess-01a2b3c4",
            workspace: "~/projects/xx",
            role: Some("security-auditor"),
            tabs: None,
            active_tab: 0,
            unattended: false,
            confined: true,
        };
        let row = render_row2(80, head);
        assert!(row.starts_with("  THREAD b3c4 [SECURITY-AUDITOR]"), "{row}");
    }

    #[test]
    fn row2_shows_thread_id_tail() {
        let head = ThreadHead {
            thread_id: "thread-7c405d7e",
            workspace: "~/projects/xx",
            role: None,
            tabs: None,
            active_tab: 0,
            unattended: false,
            confined: true,
        };
        let row = render_row2(80, head);
        assert!(row.contains("5d7e"), "{row}");
    }

    #[test]
    fn row2_band_paints_the_full_row_width() {
        let theme = Theme::default();
        let head = ThreadHead::new("sess-01a2b3c4", "~/projects/xx", Some("developer"), false, true);
        let mut terminal = nuotc::TestTerminal::new(60, 1);
        terminal.draw(|frame| {
            draw_thread_governance_header(frame, frame.area(), &head, &theme);
        });
        for cell in &terminal.buffer().content {
            assert_eq!(cell.bg, theme.raised());
        }
        let row: String = terminal
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(row.starts_with("  "), "left pad: {row:?}");
        assert!(row.ends_with("  "), "right pad: {row:?}");
    }

    #[test]
    fn row3_root_thread_with_title_shows_title_directly() {
        let row = render_row3(
            80,
            ViewHints {
                kind: SceneKind::Thread,
                context: Some("Fix the retry loop"),
                ..hints(SceneKind::Thread)
            },
        );
        assert!(row.starts_with("  Fix the retry loop"), "{row}");
        assert!(!row.contains("thread"), "redundant thread label eliminated: {row}");
    }

    #[test]
    fn row3_root_thread_untitled_has_no_thread_placeholder() {
        let row = render_row3(80, hints(SceneKind::Thread));
        assert!(!row.contains("thread"), "untitled thread has NO dummy placeholder: {row}");
        assert_eq!(row.trim(), "", "clean empty row when untitled");
    }

    #[test]
    fn row3_scene_labels_for_tool_scenes() {
        for (kind, label) in [
            (SceneKind::Subagent, "subagent"),
            (SceneKind::Settings, "settings"),
            (SceneKind::Dashboard, "dashboard"),
        ] {
            assert_eq!(kind.scene_label(), label);
            let row = render_row3(80, hints(kind));
            assert!(row.starts_with(&format!("  {label}")), "{kind:?}: {row}");
        }
    }

    #[test]
    fn row3_context_tone_escalates_when_flagged() {
        let theme = Theme::default();
        let first_fg = |cells: &[nuotc::Cell], needle: &str| {
            let idx = cells
                .iter()
                .position(|c| c.symbol() == needle)
                .expect("needle present");
            cells[idx].fg
        };
        let quiet = render_row3_cells(
            80,
            ViewHints {
                context: Some("main running"),
                ..hints(SceneKind::Dashboard)
            },
            &theme,
        );
        let urgent = render_row3_cells(
            80,
            ViewHints {
                context: Some("needs approval"),
                context_warn: true,
                ..hints(SceneKind::Dashboard)
            },
            &theme,
        );
        assert_eq!(first_fg(&quiet, "m"), theme.fg(), "quiet context is foreground");
        assert_eq!(
            first_fg(&urgent, "n"),
            theme.warn(),
            "flagged context takes the warning tone"
        );
    }

    #[test]
    fn row3_renders_right_aligned_drill_in_stack() {
        let crumbs = [SceneKind::Thread, SceneKind::Subagent];
        let row = render_row3(
            80,
            ViewHints {
                kind: SceneKind::Subagent,
                context: Some("[EXPLORE] inspect codebase"),
                context_warn: false,
                breadcrumbs: Some(&crumbs),
                can_back: true,
                can_forward: false,
            },
        );
        assert!(row.contains("[EXPLORE] inspect codebase"), "shows scene context: {row}");
        assert!(row.contains("< "), "shows back affordance: {row}");
        assert!(row.contains("thread > subagent"), "shows breadcrumb trail: {row}");
        // Context on the left, stack on the right
        let ctx_pos = row.find("[EXPLORE]").unwrap();
        let stack_pos = row.find("< thread").unwrap();
        assert!(ctx_pos < stack_pos, "context left, stack right: {row}");
    }

    #[test]
    fn row3_band_paints_the_full_row_width() {
        let theme = Theme::default();
        let mut terminal = nuotc::TestTerminal::new(60, 1);
        terminal.draw(|frame| {
            draw_view_header_hints(frame, frame.area(), &hints(SceneKind::Settings), &theme);
        });
        for cell in &terminal.buffer().content {
            assert_eq!(cell.bg, theme.raised());
        }
    }
}
