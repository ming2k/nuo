//! Polymorphic spatial header topology for every scene (ADR-0060, ADR-0062).
//!
//! The head band is the sovereign chrome pinned to the terminal's top edge.
//! Under ADR-0062 it is structured across a polymorphic hierarchy:
//!
//! - **Tier 1 — Client Global TabBar (`draw_tab_bar`).**
//!   Fixed height: 1 row. Owned by the Client container. Projects open tabs
//!   with clean semantic titles (e.g. `Refactor DB`, `dashboard`, `settings`)
//!   or compact untitled fallbacks (`thread#b3c4`), strictly omitting ordinal
//!   prefixes (`1:`) [INV-TAB-09]. Carries the standing `C-x menu` client affordance
//!   on the right edge.
//!
//! - **Tier 2 — Polymorphic SceneBar (`draw_scene_bar`).**
//!   Fixed height: 1 row. Owned by the active scene view [INV-SCENE-02]. Displays
//!   the primary task identity on the left:
//!   - For `Thread`: `THREAD <id_tail>` + authentic task title (with vestigial
//!     dummy `thread` placeholders eliminated when untitled [INV-SCENE-01]).
//!   - For Non-Thread (`Dashboard`, `Settings`): scene label + domain summary context.
//!   - On the right: right-aligned drill-in navigation stack / breadcrumbs
//!     (`‹ thread › subagent #1 › diff`) with back affordance (`‹ `).
//!
//! - **Tier 3 — Sovereign ThreadContextBar (`draw_thread_context_bar`).**
//!   Height: 1 row on Thread viewports; **0 rows on non-thread scenes** (`Dashboard`, `Settings`)
//!   [INV-HEAD-05]. Displays thread execution boundaries on the left:
//!   `[ROLE]` badge and tilde-shortened workspace path; persistent safety posture
//!   flags (`UNATTENDED`, `UNCONFINED`) on the right edge.
use nuotc::{Frame, Line, Modifier, Paragraph, Rect, Span, Style};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::{STEP_MIN_WIDTH, TRANSCRIPT_H_INSET, Theme};
use crate::surfaces::SceneKind;

/// Canonical scene identifier alias for header presentation (ADR-0042).
pub(crate) type ViewKind = crate::surfaces::SceneKind;

/// Maximum display width allocated to individual tab titles before truncation.
pub(crate) const MAX_TAB_TITLE_WIDTH: usize = 20;

/// Scene context for header presentation (ADR-0060, ADR-0062).
///
/// Focuses purely on viewport context: authentic task title or domain summary
/// on the left, and navigation stack / drill-in breadcrumbs on the right.
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

/// Canonical thread identity & governance state for header presentation (ADR-0043, ADR-0060, ADR-0062 [INV-THREAD-02]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ThreadHead<'a> {
    /// The thread's persistent id (full string). Only its last four
    /// characters are shown, dimmed, as a disambiguating tag.
    pub thread_id: &'a str,
    /// Tilde-shortened workspace path (e.g. `~/projects/xx`), or empty when unbound.
    pub workspace: &'a str,
    /// Active staffing role for this thread.
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

/// Draw Tier 1: Client Global TabBar (ADR-0039, ADR-0040, ADR-0060 [INV-TAB-09], ADR-0062).
///
/// Projects open tabs with clean semantic titles without ordinal prefixes (`1:`),
/// with the client command namespace (`C-x menu`) anchored to the right edge.
/// Returns bounding rects for mouse click hit-testing.
pub(crate) fn draw_tab_bar(
    frame: &mut Frame,
    rect: Rect,
    head: &ThreadHead<'_>,
    theme: &Theme,
) -> Vec<(usize, Rect)> {
    let full_width = rect.width as usize;
    if full_width < STEP_MIN_WIDTH || rect.height == 0 {
        return Vec::new();
    }

    let bg = theme.tabbar_bg();
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

            // Contiguous hit target: tab 0 subsumes leading padding, and every tab
            // includes its trailing separator, eliminating dead gaps between tabs.
            let hit_start = if i == 0 { rect.x } else { current_x };
            let hit_end = current_x.saturating_add((tab_w + separator_w) as u16);
            if hit_start < rect.right() {
                let w = hit_end.min(rect.right()).saturating_sub(hit_start);
                if w > 0 {
                    tab_rects.push((i, Rect::new(hit_start, rect.y, w, 1)));
                }
            }

            spans.push(Span::styled(text, tab_style));
            used_tab_w += tab_w;
            current_x = current_x.saturating_add(tab_w as u16);

            if i + 1 < tabs.len() && used_tab_w + separator_w <= available_for_tabs {
                spans.push(Span::styled(" ", fill));
                used_tab_w += separator_w;
                current_x = current_x.saturating_add(separator_w as u16);
            }
        }

        let gap = text_width.saturating_sub(used_tab_w + right_width);
        spans.push(Span::styled(" ".repeat(gap), fill));

        // Right-aligned C-x affordance
        spans.extend(affordance.render_tabbar_spans(theme, bg));
        spans.push(Span::styled(" ".repeat(pad), fill));

        frame.render_widget(Paragraph::new(Line::from(spans)), rect);
        return tab_rects;
    }

    // Fallback: Brand mark + C-x affordance
    let brand = "nuo";
    let left_w = brand.width();
    let gap = text_width.saturating_sub(left_w + right_width);

    let mut spans = vec![
        Span::styled(" ".repeat(pad), fill),
        Span::styled(brand, fill.fg(theme.brand()).add_modifier(Modifier::BOLD)),
        Span::styled(" ".repeat(gap), fill),
    ];

    spans.extend(affordance.render_tabbar_spans(theme, bg));
    spans.push(Span::styled(" ".repeat(pad), fill));

    frame.render_widget(Paragraph::new(Line::from(spans)), rect);
    Vec::new()
}

/// Draw Tier 2: Polymorphic SceneBar (ADR-0062 [INV-HEAD-05], [INV-SCENE-02]).
///
/// Universal to all scenes.
/// - Left side: Scene identity and authentic task title (`THREAD <id_tail>  <title>`
///   for thread; `<scene_label>  <context>` for dashboard / settings).
///   If untitled, the title slot remains empty without layout shift.
/// - Right side: Drill-in navigation stack / breadcrumbs (`< thread > subagent #1 > diff`)
///   with back affordance (`< `).
pub(crate) fn draw_scene_bar(
    frame: &mut Frame,
    rect: Rect,
    thread_id: Option<&str>,
    hints: &ViewHints<'_>,
    theme: &Theme,
) {
    if rect.height == 0 || (rect.width as usize) < STEP_MIN_WIDTH {
        return;
    }

    let bg = theme.scenebar_bg();
    let fill = Style::default().bg(bg);
    let title_style = fill.fg(theme.heading()).add_modifier(Modifier::BOLD);
    let tag_style = fill.fg(theme.dim());
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
    let (right_spans, right_width) = if let Some(crumbs) = hints.breadcrumbs.filter(|c| c.len() > 1)
    {
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
        if hints.can_forward {
            let fwd_arrow = " >";
            spans.push(Span::styled(fwd_arrow, fill.fg(theme.dim())));
            w += fwd_arrow.width();
        }
        (spans, w)
    } else {
        (Vec::new(), 0)
    };

    // Left side:
    // If Thread scene (or thread_id is provided):
    //   "THREAD " + "<id_tail> " + (optional title)
    // If Non-thread scene:
    //   scene_label + (optional context)
    let mut left_spans = Vec::new();
    let mut left_width = 0usize;

    if hints.kind == SceneKind::Thread {
        const TITLE: &str = "THREAD ";
        left_spans.push(Span::styled(TITLE, title_style));
        left_width += TITLE.width();

        if let Some(id) = thread_id {
            let tag = id_tail(id);
            if !tag.is_empty() {
                left_spans.push(Span::styled(tag.to_string(), tag_style));
                left_width += tag.width();
            }
        }

        if let Some(ctx) = hints.context.filter(|c| !c.is_empty()) {
            let sep_w = 2;
            let budget = text_width.saturating_sub(left_width + sep_w + right_width);
            let truncated = truncate_to_width(ctx, budget);
            if !truncated.is_empty() {
                let w = truncated.width();
                left_spans.push(Span::styled("  ", fill));
                left_spans.push(Span::styled(truncated, title_style));
                left_width += 2 + w;
            }
        }
    } else {
        let lead_label = if hints.breadcrumbs.is_none_or(|c| c.len() <= 1) {
            Some(hints.kind.scene_label())
        } else {
            None
        };

        if let Some(lbl) = lead_label {
            left_spans.push(Span::styled(lbl, label_style));
            left_width += lbl.width();
        }

        if let Some(ctx) = hints.context.filter(|c| !c.is_empty()) {
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
    }

    let gap = text_width.saturating_sub(left_width + right_width);
    let mut spans = vec![Span::styled(" ".repeat(pad), fill)];
    spans.extend(left_spans);
    spans.push(Span::styled(" ".repeat(gap), fill));
    spans.extend(right_spans);
    spans.push(Span::styled(" ".repeat(pad), fill));

    frame.render_widget(Paragraph::new(Line::from(spans)), rect);
}

/// Draw Tier 3: Sovereign ThreadContextBar (ADR-0062 [INV-HEAD-05]).
///
/// Only rendered on thread-backed viewports (0 rows on non-thread scenes).
/// - Left side: staffing `[ROLE]` badge and bound `workspace` path.
/// - Right side: persistent safety execution posture flags (`UNATTENDED`, `UNCONFINED`).
pub(crate) fn draw_thread_context_bar(
    frame: &mut Frame,
    rect: Rect,
    head: &ThreadHead<'_>,
    theme: &Theme,
) {
    let full_width = rect.width as usize;
    if full_width < STEP_MIN_WIDTH || rect.height == 0 {
        return;
    }

    let bg = theme.contextbar_bg();
    let fill = Style::default().bg(bg);
    let pad = TRANSCRIPT_H_INSET as usize;
    let text_width = full_width.saturating_sub(2 * pad);

    let badge_style = fill.fg(theme.brand()).add_modifier(Modifier::BOLD);
    let ws_style = fill.fg(theme.dim());
    let flag_style = fill.fg(theme.warn()).add_modifier(Modifier::BOLD);

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

    let badge_w = if badge.is_empty() {
        0
    } else {
        badge.width() + 1
    };

    // Remaining budget for workspace path
    let ws_budget = text_width.saturating_sub(badge_w + flags_width + 1);
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

    let total_left_w = badge_w + ws_w;
    let gap = text_width.saturating_sub(total_left_w + flags_width);

    let mut spans = vec![Span::styled(" ".repeat(pad), fill)];
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

/// The scene row's name for the `C-x` scene namespace (ADR-0023, ADR-0043).
const SCENE_NAMESPACE_LABEL: &str = "menu";

/// Disambiguate thread ids by taking their last four characters.
fn id_tail(id: &str) -> &str {
    let len = id.len();
    if len <= 4 { id } else { &id[len - 4..] }
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
    let budget = max_width.saturating_sub(1);
    let mut out = String::new();
    let mut current_width = 0;
    for ch in s.chars() {
        let ch_width = ch.width().unwrap_or(0);
        if current_width + ch_width > budget {
            break;
        }
        out.push(ch);
        current_width += ch_width;
    }
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render_row1(width: u16, head: ThreadHead<'_>) -> String {
        let cells = render_row1_cells(width, head, &Theme::default());
        cells.iter().map(|c| c.symbol()).collect()
    }

    fn render_row1_cells(width: u16, head: ThreadHead<'_>, theme: &Theme) -> Vec<nuotc::Cell> {
        let mut terminal = nuotc::TestTerminal::new(width, 1);
        terminal.draw(|frame| {
            draw_tab_bar(frame, frame.area(), &head, theme);
        });
        terminal.buffer().content.clone()
    }

    fn render_row2(width: u16, thread_id: Option<&str>, hints: ViewHints<'_>) -> String {
        let cells = render_row2_cells(width, thread_id, hints, &Theme::default());
        cells.iter().map(|c| c.symbol()).collect()
    }

    fn render_row2_cells(
        width: u16,
        thread_id: Option<&str>,
        hints: ViewHints<'_>,
        theme: &Theme,
    ) -> Vec<nuotc::Cell> {
        let mut terminal = nuotc::TestTerminal::new(width, 1);
        terminal.draw(|frame| {
            draw_scene_bar(frame, frame.area(), thread_id, &hints, theme);
        });
        terminal.buffer().content.clone()
    }

    fn render_row3(width: u16, head: ThreadHead<'_>) -> String {
        let cells = render_row3_cells(width, head, &Theme::default());
        cells.iter().map(|c| c.symbol()).collect()
    }

    fn render_row3_cells(width: u16, head: ThreadHead<'_>, theme: &Theme) -> Vec<nuotc::Cell> {
        let mut terminal = nuotc::TestTerminal::new(width, 1);
        terminal.draw(|frame| {
            draw_thread_context_bar(frame, frame.area(), &head, theme);
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
        assert!(
            !row.contains("1:thread#b3c4"),
            "tabs must not use ordinal prefix: {row}"
        );
        assert!(row.contains("dashboard"), "{row}");
        assert!(
            !row.contains("2:dashboard"),
            "tabs must not use ordinal prefix: {row}"
        );
        assert!(
            row.contains("C-x menu"),
            "compact client menu on TabBar: {row}"
        );
    }

    #[test]
    fn row1_renders_semantic_tab_titles_with_wider_budget() {
        let tabs = vec![
            crate::surfaces::ClientTab::thread(
                "sess-01a2b3c4",
                Some("Refactor Database Connection"),
            ),
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
        assert!(
            row.contains("Refactor Database C…"),
            "semantic title projected with wider budget: {row}"
        );
        assert!(!row.contains("1:Refactor"), "no ordinal prefix: {row}");
        assert!(
            row.contains("thread#e7f8"),
            "untitled tab falls back to placeholder thread#tail: {row}"
        );
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
            tab_rects = draw_tab_bar(frame, frame.area(), &head, &theme);
        });
        assert_eq!(tab_rects.len(), 3);
        assert_eq!(tab_rects[0].0, 0);
        assert_eq!(tab_rects[1].0, 1);
        assert_eq!(tab_rects[2].0, 2);
        assert_eq!(tab_rects[0].1.y, 0);
        assert_eq!(tab_rects[0].1.x, 0, "tab 0 subsumes leading pad");
        assert_eq!(
            tab_rects[1].1.x,
            tab_rects[0].1.right(),
            "tab 1 starts seamlessly where tab 0 ends"
        );
        assert_eq!(
            tab_rects[2].1.x,
            tab_rects[1].1.right(),
            "tab 2 starts seamlessly where tab 1 ends"
        );
        assert!(tab_rects[0].1.width > 0);
        assert!(tab_rects[1].1.x > tab_rects[0].1.x);
    }

    #[test]
    fn row1_renders_brand_when_no_tabs() {
        let head = ThreadHead::new(
            "sess-01a2b3c4",
            "~/projects/xx",
            Some("developer"),
            false,
            true,
        );
        let row = render_row1(80, head);
        assert!(row.contains("nuo"), "brand shown: {row}");
        assert!(row.contains("C-x menu"), "client menu shown: {row}");
    }

    #[test]
    fn row2_renders_thread_identity_and_task_title() {
        let row = render_row2(
            80,
            Some("sess-01a2b3c4"),
            ViewHints {
                kind: SceneKind::Thread,
                context: Some("Fix the retry loop"),
                ..hints(SceneKind::Thread)
            },
        );
        assert!(
            row.starts_with("  THREAD b3c4  Fix the retry loop"),
            "{row}"
        );
    }

    #[test]
    fn row2_renders_untitled_thread_without_placeholder() {
        let row = render_row2(80, Some("sess-01a2b3c4"), hints(SceneKind::Thread));
        assert!(row.starts_with("  THREAD b3c4"), "{row}");
        assert!(
            !row.contains("thread#"),
            "clean thread identifier without redundant label: {row}"
        );
    }

    #[test]
    fn row2_renders_non_thread_scene_labels() {
        for (kind, label) in [
            (SceneKind::Subagent, "subagent"),
            (SceneKind::Settings, "settings"),
            (SceneKind::Dashboard, "dashboard"),
        ] {
            assert_eq!(kind.scene_label(), label);
            let row = render_row2(80, None, hints(kind));
            assert!(row.starts_with(&format!("  {label}")), "{kind:?}: {row}");
        }
    }

    #[test]
    fn row2_context_tone_escalates_when_flagged() {
        let theme = Theme::default();
        let first_fg = |cells: &[nuotc::Cell], needle: &str| {
            let idx = cells
                .iter()
                .position(|c| c.symbol() == needle)
                .expect("needle present");
            cells[idx].fg
        };
        let quiet = render_row2_cells(
            80,
            None,
            ViewHints {
                context: Some("main running"),
                ..hints(SceneKind::Dashboard)
            },
            &theme,
        );
        let urgent = render_row2_cells(
            80,
            None,
            ViewHints {
                context: Some("needs approval"),
                context_warn: true,
                ..hints(SceneKind::Dashboard)
            },
            &theme,
        );
        assert_eq!(
            first_fg(&quiet, "m"),
            theme.fg(),
            "quiet context is foreground"
        );
        assert_eq!(
            first_fg(&urgent, "n"),
            theme.warn(),
            "flagged context takes the warning tone"
        );
    }

    #[test]
    fn row2_renders_right_aligned_drill_in_stack() {
        let crumbs = [SceneKind::Thread, SceneKind::Subagent];
        let row = render_row2(
            80,
            None,
            ViewHints {
                kind: SceneKind::Subagent,
                context: Some("[EXPLORE] inspect codebase"),
                context_warn: false,
                breadcrumbs: Some(&crumbs),
                can_back: true,
                can_forward: false,
            },
        );
        assert!(
            row.contains("[EXPLORE] inspect codebase"),
            "shows scene context: {row}"
        );
        assert!(row.contains("< "), "shows back affordance: {row}");
        assert!(
            row.contains("thread > subagent"),
            "shows breadcrumb trail: {row}"
        );
        let ctx_pos = row.find("[EXPLORE]").unwrap();
        let stack_pos = row.find("< thread").unwrap();
        assert!(ctx_pos < stack_pos, "context left, stack right: {row}");

        // With can_forward = true
        let row_fwd = render_row2(
            80,
            None,
            ViewHints {
                kind: SceneKind::Subagent,
                context: Some("[EXPLORE] inspect codebase"),
                context_warn: false,
                breadcrumbs: Some(&crumbs),
                can_back: true,
                can_forward: true,
            },
        );
        assert!(
            row_fwd.contains("subagent >"),
            "shows forward affordance: {row_fwd}"
        );
    }

    #[test]
    fn row2_band_paints_the_full_row_width() {
        let theme = Theme::default();
        let mut terminal = nuotc::TestTerminal::new(60, 1);
        terminal.draw(|frame| {
            draw_scene_bar(
                frame,
                frame.area(),
                None,
                &hints(SceneKind::Settings),
                &theme,
            );
        });
        for cell in &terminal.buffer().content {
            assert_eq!(cell.bg, theme.scenebar_bg());
        }
    }

    #[test]
    fn row3_renders_thread_context_role_and_workspace() {
        let head = ThreadHead {
            thread_id: "sess-01a2b3c4",
            workspace: "~/projects/xx",
            role: Some("developer"),
            tabs: None,
            active_tab: 0,
            unattended: true,
            confined: false,
        };
        let row = render_row3(80, head);
        assert!(row.starts_with("  [DEVELOPER] ~/projects/xx"), "{row}");
        assert!(row.contains("UNATTENDED"), "unattended flag: {row}");
        assert!(row.contains("UNCONFINED"), "unconfined flag: {row}");
    }

    #[test]
    fn row3_workspace_free_hides_workspace_and_shows_role_badge() {
        let head = ThreadHead {
            thread_id: "sess-01a2b3c4",
            workspace: "",
            role: Some("philosophist"),
            tabs: None,
            active_tab: 0,
            unattended: false,
            confined: true,
        };
        let row = render_row3(80, head);
        assert!(row.starts_with("  [PHILOSOPHIST]"), "{row}");
        assert!(!row.contains("~/"), "{row}");
    }

    #[test]
    fn row3_shows_custom_user_role_badge() {
        let head = ThreadHead {
            thread_id: "sess-01a2b3c4",
            workspace: "~/projects/xx",
            role: Some("security-auditor"),
            tabs: None,
            active_tab: 0,
            unattended: false,
            confined: true,
        };
        let row = render_row3(80, head);
        assert!(row.starts_with("  [SECURITY-AUDITOR]"), "{row}");
    }

    #[test]
    fn row3_band_paints_the_full_row_width() {
        let theme = Theme::default();
        let head = ThreadHead::new(
            "sess-01a2b3c4",
            "~/projects/xx",
            Some("developer"),
            false,
            true,
        );
        let mut terminal = nuotc::TestTerminal::new(60, 1);
        terminal.draw(|frame| {
            draw_thread_context_bar(frame, frame.area(), &head, &theme);
        });
        for cell in &terminal.buffer().content {
            assert_eq!(cell.bg, theme.contextbar_bg());
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
    fn header_tiers_exhibit_distinct_micro_elevation_ladder() {
        let theme = Theme::default();
        assert_ne!(
            theme.tabbar_bg(),
            theme.scenebar_bg(),
            "tier 1 tabbar background must differ from tier 2 scenebar"
        );
        assert_ne!(
            theme.scenebar_bg(),
            theme.contextbar_bg(),
            "tier 2 scenebar background must differ from tier 3 contextbar"
        );
        assert_ne!(
            theme.tabbar_bg(),
            theme.contextbar_bg(),
            "tier 1 tabbar background must differ from tier 3 contextbar"
        );
    }
}
