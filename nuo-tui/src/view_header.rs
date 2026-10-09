//! Two-row head band for every scene.
//!
//! The head band is the fixed, always-present chrome pinned to the terminal's
//! top edge. It is **two rows by default** (ADR-0024) and each row has one
//! job:
//!
//! - **Row 1 — session identity.** The ambient *session* facts that never
//!   change while the user navigates scenes: the `SESSION` identity, the
//!   session's persistent-id tail, the staffing `[ROLE]` badge, and the bound
//!   workspace. Every scene (Thread, Dashboard, Settings, Subagent zoom,
//!   aside) draws this same row, so the session the client is attached to is
//!   never hidden by the scene beneath it.
//! - **Row 2 — scene + context + run-mode status.** The *scene* the user is
//!   standing in, named plainly (`thread`, `dashboard`, `settings`,
//!   `subagent`, `aside`), then the scene's context on the left — for the
//!   thread that is the chat's title, for a subagent its task label, for
//!   an aside the primary's status. The right edge carries the session's
//!   persistent run-mode flags (`UNATTENDED`, `UNCONFINED`) followed by the
//!   standing `C-x menu` namespace pair — the single entry point for the
//!   Command Palette / surface switcher (ADR-0023 `[INV-HINT-01]`).
//!
//! Esc is never the advertised exit: it does not close Scenes, so the legend points at the namespace that does. Keeping this outside disclosure rendering also leaves one
//! clear extension point for future focused scenes.

use nuotc::{Frame, Line, Modifier, Paragraph, Rect, Span, Style};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::{STEP_MIN_WIDTH, TRANSCRIPT_H_INSET, Theme};

/// Row-2 (scene) context for every scene. One struct because the row's
/// *shape* is shared across scenes (ADR-0024): a leading scene label — the
/// scene the user is standing in, named plainly — then the scene's own
/// context on the left, and the session's persistent run-mode flags on the right.
/// For thread-scoped scenes (Thread, Subagent, Aside), the bound
/// workspace path is also displayed here rather than on the client-level TabBar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ViewHints<'a> {
    /// Which scene the row describes — decides the leading scene label.
    pub kind: ViewKind,
    /// Scene context shown after the label: the thread's chat title, an
    /// aside's parent status, a subagent's `[ROLE] label (i/n)`, the
    /// dashboard's fleet summary, or the settings breadcrumb. `None` renders
    /// the label alone.
    pub context: Option<&'a str>,
    /// `true` while the context is an attention state (e.g. the dashboard has
    /// a session needing approval), which escalates it to the warning tone.
    pub context_warn: bool,
    /// `true` while the session runs unattended (`--unattended` /
    /// `/unattended on`). Rendered as a warning-toned `UNATTENDED` flag on the
    /// right — the session's persistent run-mode flag.
    pub unattended: bool,
    /// `false` while the session's workspace filesystem confinement is off
    /// (`/confinement off`). Rendered as a warning-toned `UNCONFINED` flag.
    pub confined: bool,
    /// Tilde-shortened workspace path bound to the active thread. Displayed
    /// only on thread-scoped scenes.
    pub workspace: Option<&'a str>,
    /// Optional breadcrumbs from the active tab's history stack (ADR-0040, ADR-0042).
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
            unattended: false,
            confined: true,
            workspace: None,
            breadcrumbs: None,
            can_back: false,
            can_forward: false,
        }
    }

    /// Row 2 always has content (ADR-0024): the scene label and the `C-x menu`
    /// namespace pair stand up on every scene, so the head band is always two
    /// rows. Retained as a method so callers keep one place to ask the band's
    /// row inventory.
    pub(crate) fn has_content(&self) -> bool {
        true
    }
}

use crate::surfaces::SceneKind;

/// Canonical scene identifier alias for header presentation (ADR-0042).
pub(crate) type ViewKind = SceneKind;

/// Row-1 content: the ambient session identity. Uniform across **every**
/// scene (ADR-0024) — the head band's top row always describes the session the
/// client is attached to, never the scene beneath it. The scene the user
/// stands in is named by row 2 ([`ViewHints`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SessionHead<'a> {
    /// The session's persistent id (full string). Only its last four
    /// characters are shown, dimmed, as a disambiguating tag.
    pub session_id: &'a str,
    /// Tilde-shortened workspace path (e.g. `~/projects/xx`), or empty when unbound.
    pub workspace: &'a str,
    /// Active staffing role for this session (ADR-0244).
    pub role: Option<&'a str>,
    /// When switching to another session, holds the target session id.
    pub switching_target: Option<&'a str>,
    /// Client-owned tabs mounted in this viewport (ADR-0039).
    pub tabs: Option<&'a [crate::surfaces::ClientTab]>,
    /// Active tab index within `tabs`.
    pub active_tab: usize,
}

#[allow(dead_code)]
impl<'a> SessionHead<'a> {
    pub fn simple(session_id: &'a str, workspace: &'a str, role: Option<&'a str>) -> Self {
        Self {
            session_id,
            workspace,
            role,
            switching_target: None,
            tabs: None,
            active_tab: 0,
        }
    }
}

/// Draw the head band's first row: the **session identity**, uniform on every
/// scene (ADR-0024). `SESSION` + the persistent-id tail (dimmed) + the `[ROLE]`
/// badge (brand) + the tilde-shortened workspace, or, while a session switch is
/// loading, the target id. The scene the user stands in is named on row 2
/// ([`draw_view_header_hints`]), so this row never changes as the user moves
/// between scenes.
///
/// The band's background spans the rect's full width — the head is top-level
/// chrome pinned to the terminal's top edge, so its `raised` surface reaches
/// both edges. The *text* keeps the shared [`TRANSCRIPT_H_INSET`] horizontal
/// inset (rendered as pad spans) so it stays aligned with the transcript band
/// below.
pub(crate) fn draw_view_header(
    frame: &mut Frame,
    rect: Rect,
    head: &SessionHead<'_>,
    theme: &Theme,
) -> Vec<(usize, Rect)> {
    let full_width = rect.width as usize;
    if full_width < STEP_MIN_WIDTH {
        return Vec::new();
    }

    let tag = if let Some(target) = head.switching_target {
        format!("{} (loading…)", target)
    } else {
        id_tail(head.session_id)
    };
    let badge = head
        .role
        .map(|r| format!("[{}]", r.to_uppercase()))
        .unwrap_or_default();

    let bg = theme.raised();
    let fill = Style::default().bg(bg);
    let pad = TRANSCRIPT_H_INSET as usize;
    let text_width = full_width.saturating_sub(2 * pad);

    // C-x affordance belongs to the client-level TabBar / Header (ADR-0039, ADR-0040)
    let affordance = crate::components::keycap::KeyAffordance::from_key(
        crate::keymap::Key::CTRL_X,
        SCENE_NAMESPACE_LABEL,
    );
    let right_width = affordance.width();

    // Tab workspace rendering (ADR-0039 [INV-TAB-01], ADR-0040 [INV-UI-01])
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
                    format!("{}:thread-{}", i + 1, id_tail(id))
                }
                crate::surfaces::TabKind::Dashboard => format!("{}:dashboard", i + 1),
                crate::surfaces::TabKind::Settings => format!("{}:settings", i + 1),
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
        let [key_span, label_span] = affordance.render_spans(theme, bg);
        spans.push(key_span);
        spans.push(label_span);
        spans.push(Span::styled(" ".repeat(pad), fill));
        frame.render_widget(Paragraph::new(Line::from(spans)), rect);
        return tab_rects;
    }

    let title_style = fill.fg(theme.heading()).add_modifier(Modifier::BOLD);
    let tag_style = fill.fg(theme.dim());
    let badge_style = fill.fg(theme.brand()).add_modifier(Modifier::BOLD);

    const TITLE: &str = "SESSION ";
    let title_width = TITLE.width();
    let tag_width = if tag.is_empty() { 0 } else { tag.width() + 1 };
    let badge_width = if badge.is_empty() {
        0
    } else {
        badge.width() + 1
    };
    let left_width = title_width + tag_width + badge_width;
    let gap = text_width.saturating_sub(left_width + right_width);

    let mut spans = vec![Span::styled(" ".repeat(pad), fill)];
    spans.push(Span::styled(TITLE, title_style));
    if !tag.is_empty() {
        spans.push(Span::styled(format!("{tag} "), tag_style));
    }
    if !badge.is_empty() {
        spans.push(Span::styled(format!("{badge} "), badge_style));
    }
    spans.push(Span::styled(" ".repeat(gap), fill));
    let [key_span, label_span] = affordance.render_spans(theme, bg);
    spans.push(key_span);
    spans.push(label_span);
    spans.push(Span::styled(" ".repeat(pad), fill));

    frame.render_widget(Paragraph::new(Line::from(spans)), rect);
    Vec::new()
}

/// Draw the header band's scene row (ADR-0024, ADR-0040). It names the scene
/// the user is standing in, its context, and (for thread-type scenes)
/// the workspace path. The right side carries the session's persistent run-mode
/// flags (`UNATTENDED`, `UNCONFINED`).
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
    let workspace_style = fill.fg(theme.dim());
    let flag_style = fill.fg(theme.warn()).add_modifier(Modifier::BOLD);

    let width = rect.width as usize;
    let pad = TRANSCRIPT_H_INSET as usize;
    let text_width = width.saturating_sub(2 * pad);

    // The right side: the session's persistent run-mode flags.
    // Note: C-x menu is elevated to Row 1 (Client TabBar) as a client-scoped shortcut.
    let mut flags = String::new();
    if hints.unattended {
        flags.push_str("UNATTENDED ");
    }
    if !hints.confined {
        flags.push_str("UNCONFINED ");
    }
    let flags_width = flags.width();
    let right_width = flags_width;

    // The left side: the scene label or breadcrumbs, then the scene's context, and (for thread-scoped scenes) the workspace path.
    let (lead_spans, lead_width) = if let Some(crumbs) = hints.breadcrumbs.filter(|c| c.len() > 1) {
        let mut spans = Vec::new();
        let mut width = 0usize;
        if hints.can_back {
            let back_arrow = "< ";
            spans.push(Span::styled(back_arrow, fill.fg(theme.dim())));
            width += back_arrow.width();
        }
        for (idx, crumb) in crumbs.iter().enumerate() {
            let crumb_str = crumb.breadcrumb_label();
            let is_last = idx + 1 == crumbs.len();
            if is_last {
                spans.push(Span::styled(crumb_str, label_style));
                width += crumb_str.width();
            } else {
                spans.push(Span::styled(crumb_str, fill.fg(theme.dim())));
                spans.push(Span::styled(" > ", fill.fg(theme.dim())));
                width += crumb_str.width() + 3;
            }
        }
        (spans, width)
    } else {
        let label = hints.kind.scene_label();
        let width = label.width();
        (vec![Span::styled(label, label_style)], width)
    };

    // Only thread-scoped scenes display a bound workspace path.
    let show_workspace = matches!(
        hints.kind,
        SceneKind::Thread | SceneKind::Subagent | SceneKind::Aside
    );
    let ws = if show_workspace {
        hints.workspace.filter(|w| !w.is_empty())
    } else {
        None
    };

    let ws_len = ws.map(|w| w.width() + 2).unwrap_or(0);
    let context_budget = text_width.saturating_sub(lead_width + 2 + ws_len + right_width);
    let context = hints
        .context
        .filter(|c| !c.is_empty())
        .map(|c| truncate_to_width(c, context_budget));

    let mut spans = vec![Span::styled(" ".repeat(pad), fill)];
    spans.extend(lead_spans);
    let mut left_width = lead_width;

    if let Some(context) = context {
        spans.push(Span::styled("  ", fill));
        let context_width = context.width();
        spans.push(Span::styled(context, context_style));
        left_width += 2 + context_width;
    }

    if let Some(ws_path) = ws {
        let ws_budget = text_width.saturating_sub(left_width + 2 + right_width);
        let truncated_ws = truncate_to_width(ws_path, ws_budget);
        if !truncated_ws.is_empty() {
            spans.push(Span::styled("  ", fill));
            let ws_w = truncated_ws.width();
            spans.push(Span::styled(truncated_ws, workspace_style));
            left_width += 2 + ws_w;
        }
    }

    let gap = text_width.saturating_sub(left_width + right_width);
    spans.push(Span::styled(" ".repeat(gap), fill));
    if !flags.is_empty() {
        spans.push(Span::styled(flags, flag_style));
    }
    spans.push(Span::styled(" ".repeat(pad), fill));

    frame.render_widget(Paragraph::new(Line::from(spans)), rect);
}

/// The scene row's name for the `C-x` scene namespace (ADR-0023).
/// The keycap names the namespace — now **`menu`**, because the
/// namespace's headline verb is the palette / switcher (`C-x p`) — rather than
/// one of its lifecycle verbs: `w`/`k` close a scene, but the same row is
/// shared by pages already at their home scene, where there is nothing to
/// close.
const SCENE_NAMESPACE_LABEL: &str = "menu";

fn truncate_to_width(text: &str, max_width: usize) -> String {
    if text.width() <= max_width && !text.contains(['\n', '\r']) {
        return text.to_string();
    }
    if max_width == 0 {
        return String::new();
    }
    if max_width == 1 {
        return "…".to_string();
    }

    let content_width = max_width - 1;
    let mut out = String::new();
    let mut used = 0;
    for ch in text.chars() {
        if ch == '\n' || ch == '\r' {
            break;
        }
        let width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + width > content_width {
            break;
        }
        out.push(ch);
        used += width;
    }
    out.push('…');
    out
}

fn id_tail(id: &str) -> String {
    let trimmed = id.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let chars: Vec<char> = trimmed.chars().collect();
    let take = chars.len().min(4);
    chars[chars.len() - take..].iter().collect()
}

/// The aside scene's row-2 context: the coarse primary-session status, phrased
/// as a short clause after the scene label (ADR-0024), e.g.
/// `aside  main running`. The attention states carry the `⚠` marker the caller
/// pairs with [`ViewHints::context_warn`].
pub(crate) fn parent_status_context(parent: nuo_wire::ParentStatus) -> &'static str {
    match parent {
        nuo_wire::ParentStatus::Idle => "main idle",
        nuo_wire::ParentStatus::Running => "main running",
        nuo_wire::ParentStatus::NeedsApproval => "⚠ main approval needed",
        nuo_wire::ParentStatus::NeedsInput => "⚠ main input needed",
        nuo_wire::ParentStatus::Failed => "⚠ main failed",
        nuo_wire::ParentStatus::Interrupted => "⚠ main interrupted",
    }
}

/// Whether a parent status is an attention state (approval / input / failure /
/// interruption) that should escalate the aside's context to the warning tone.
pub(crate) fn parent_status_needs_attention(parent: nuo_wire::ParentStatus) -> bool {
    matches!(
        parent,
        nuo_wire::ParentStatus::NeedsApproval
            | nuo_wire::ParentStatus::NeedsInput
            | nuo_wire::ParentStatus::Failed
            | nuo_wire::ParentStatus::Interrupted
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render_row1(width: u16, head: SessionHead<'_>) -> String {
        render_row1_cells(width, head, &Theme::default())
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    fn render_row1_cells(width: u16, head: SessionHead<'_>, theme: &Theme) -> Vec<nuotc::Cell> {
        let mut terminal = nuotc::TestTerminal::new(width, 1);
        terminal.draw(|frame| {
            draw_view_header(frame, frame.area(), &head, theme);
        });
        terminal.buffer().content.clone()
    }

    fn render_row2(width: u16, hints: ViewHints<'_>) -> String {
        render_row2_cells(width, hints, &Theme::default())
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    fn render_row2_cells(width: u16, hints: ViewHints<'_>, theme: &Theme) -> Vec<nuotc::Cell> {
        let mut terminal = nuotc::TestTerminal::new(width, 1);
        terminal.draw(|frame| {
            draw_view_header_hints(frame, frame.area(), &hints, theme);
        });
        terminal.buffer().content.clone()
    }

    fn hints<'a>(kind: ViewKind) -> ViewHints<'a> {
        ViewHints::simple(kind)
    }

    /// Row 1 is the client-level top bar: `SESSION` (or tabs), the id tail,
    /// the `[ROLE]` badge, and on the right the client-level `C-x menu` namespace.
    /// It does not carry the workspace path (which moved to the scene head, ADR-0040).
    #[test]
    fn row1_is_session_identity() {
        let head = SessionHead {
            session_id: "sess-01a2b3c4",
            workspace: "~/projects/xx",
            role: Some("developer"),
            switching_target: None,
            tabs: None,
            active_tab: 0,
        };
        let row = render_row1(80, head);
        assert!(row.starts_with("  SESSION b3c4 [DEVELOPER]"), "{row}");
        assert!(
            !row.contains("~/projects/xx"),
            "workspace belongs to scene head: {row}"
        );
        assert!(
            !row.contains("UNATTENDED") && !row.contains("UNCONFINED"),
            "the run-mode flags live on row 2 now: {row}"
        );
        assert!(row.contains("Ctrl-x") && row.contains("menu"), "client menu on row 1: {row}");
    }

    #[test]
    fn row1_renders_tabs_and_client_namespace() {
        let tabs = vec![
            crate::surfaces::ClientTab::thread("sess-01a2b3c4", "Thread"),
            crate::surfaces::ClientTab::dashboard(),
        ];
        let head = SessionHead {
            session_id: "sess-01a2b3c4",
            workspace: "~/projects/xx",
            role: Some("developer"),
            switching_target: None,
            tabs: Some(&tabs),
            active_tab: 0,
        };
        let row = render_row1(80, head);
        assert!(row.contains("1:thread-b3c4"), "{row}");
        assert!(!row.contains("[1:thread-b3c4]"), "tabs must not use brackets: {row}");
        assert!(row.contains("2:dashboard"), "{row}");
        assert!(!row.contains("~/projects/xx"), "workspace removed from client TabBar: {row}");
        assert!(row.contains("Ctrl-x") && row.contains("menu"), "client menu on TabBar: {row}");
    }

    #[test]
    fn row1_tab_rects_and_styling() {
        let tabs = vec![
            crate::surfaces::ClientTab::thread("sess-01a2b3c4", "Thread"),
            crate::surfaces::ClientTab::dashboard(),
            crate::surfaces::ClientTab::settings(),
        ];
        let head = SessionHead {
            session_id: "sess-01a2b3c4",
            workspace: "~/projects/xx",
            role: Some("developer"),
            switching_target: None,
            tabs: Some(&tabs),
            active_tab: 0,
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
    fn row1_workspace_free_hides_workspace_and_shows_role_badge() {
        let head = SessionHead {
            session_id: "sess-01a2b3c4",
            workspace: "",
            role: Some("philosophist"),
            switching_target: None,
            tabs: None,
            active_tab: 0,
        };
        let row = render_row1(80, head);
        assert!(row.starts_with("  SESSION b3c4 [PHILOSOPHIST]"), "{row}");
        assert!(!row.contains("~/"), "{row}");
    }

    #[test]
    fn row1_shows_custom_user_role_badge() {
        let head = SessionHead {
            session_id: "sess-01a2b3c4",
            workspace: "~/projects/xx",
            role: Some("security-auditor"),
            switching_target: None,
            tabs: None,
            active_tab: 0,
        };
        let row = render_row1(80, head);
        assert!(row.starts_with("  SESSION b3c4 [SECURITY-AUDITOR]"), "{row}");
    }

    #[test]
    fn row1_shows_switching_target_loading() {
        let head = SessionHead {
            session_id: "sess-01a2b3c4",
            workspace: "~/projects/xx",
            role: None,
            switching_target: Some("7c405d7e"),
            tabs: None,
            active_tab: 0,
        };
        let row = render_row1(80, head);
        assert!(row.contains("7c405d7e (loading…)"), "{row}");
    }

    #[test]
    fn row1_band_paints_the_full_row_width() {
        let theme = Theme::default();
        let head = SessionHead {
            session_id: "sess-01a2b3c4",
            workspace: "~/projects/xx",
            role: Some("developer"),
            switching_target: None,
            tabs: None,
            active_tab: 0,
        };
        let mut terminal = nuotc::TestTerminal::new(60, 1);
        terminal.draw(|frame| {
            draw_view_header(frame, frame.area(), &head, &theme);
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

    /// Row 2 names the scene the user stands in, then the scene's context, and
    /// (for thread) the workspace path.
    #[test]
    fn row2_names_the_scene_with_context_and_workspace() {
        let row = render_row2(
            80,
            ViewHints {
                kind: SceneKind::Thread,
                context: Some("Fix the retry loop"),
                workspace: Some("~/projects/xx"),
                ..hints(SceneKind::Thread)
            },
        );
        assert!(row.starts_with("  thread  Fix the retry loop"), "{row}");
        assert!(row.contains("~/projects/xx"), "workspace present on scene head: {row}");
        assert!(!row.contains("Ctrl-x"), "namespace moved up to TabBar: {row}");
        assert!(!row.contains("Esc"), "Esc never advertises a scene exit: {row}");
    }

    #[test]
    fn row2_thread_without_a_title_shows_the_label_alone() {
        let row = render_row2(80, hints(SceneKind::Thread));
        assert!(row.starts_with("  thread"), "{row}");
        assert!(!row.contains("Ctrl-x"), "no namespace on scene row: {row}");
        assert!(
            !row.contains("Fix") && !row.trim_end().starts_with("thread  Fix"),
            "{row}"
        );
    }

    #[test]
    fn row2_workspace_only_for_thread_scenes() {
        let session_row = render_row2(
            80,
            ViewHints {
                kind: SceneKind::Thread,
                workspace: Some("~/repo"),
                ..hints(SceneKind::Thread)
            },
        );
        assert!(session_row.contains("~/repo"), "session scene has workspace: {session_row}");

        let dashboard_row = render_row2(
            80,
            ViewHints {
                kind: SceneKind::Dashboard,
                workspace: Some("~/repo"),
                ..hints(SceneKind::Dashboard)
            },
        );
        assert!(!dashboard_row.contains("~/repo"), "dashboard scene ignores workspace: {dashboard_row}");

        let settings_row = render_row2(
            80,
            ViewHints {
                kind: SceneKind::Settings,
                workspace: Some("~/repo"),
                ..hints(SceneKind::Settings)
            },
        );
        assert!(!settings_row.contains("~/repo"), "settings scene ignores workspace: {settings_row}");
    }

    #[test]
    fn row2_scene_labels_are_per_scene() {
        for (kind, label) in [
            (SceneKind::Thread, "thread"),
            (SceneKind::Aside, "aside"),
            (SceneKind::Subagent, "subagent"),
            (SceneKind::Settings, "settings"),
            (SceneKind::Dashboard, "dashboard"),
        ] {
            assert_eq!(kind.scene_label(), label);
            let row = render_row2(80, hints(kind));
            assert!(row.starts_with(&format!("  {label}")), "{kind:?}: {row}");
        }
    }

    /// The run-mode flags sit on the right of row 2.
    #[test]
    fn row2_carries_unattended_and_unconfined_flags_on_the_right() {
        let row = render_row2(
            80,
            ViewHints {
                unattended: true,
                confined: false,
                ..hints(SceneKind::Dashboard)
            },
        );
        assert!(row.contains("UNATTENDED"), "unattended flag: {row}");
        assert!(row.contains("UNCONFINED"), "unconfined flag: {row}");
        assert!(!row.contains("Ctrl-x"), "namespace elevated to TabBar: {row}");
    }

    /// An attention-toned context escalates to the warning tone; a quiet one
    /// stays at the foreground tone.
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
            ViewHints {
                context: Some("main running"),
                ..hints(SceneKind::Dashboard)
            },
            &theme,
        );
        let urgent = render_row2_cells(
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

    /// Every scene stands up row 2 (ADR-0023 `[INV-HINT-01]`).
    #[test]
    fn row2_stands_up_on_every_scene() {
        for kind in [
            SceneKind::Thread,
            SceneKind::Aside,
            SceneKind::Subagent,
            SceneKind::Settings,
            SceneKind::Dashboard,
        ] {
            assert!(hints(kind).has_content(), "{kind:?} stands up row 2");
            let row = render_row2(80, hints(kind));
            assert!(row.starts_with(&format!("  {}", kind.scene_label())), "{row}");
        }
    }

    #[test]
    fn row2_band_paints_the_full_row_width() {
        let theme = Theme::default();
        let mut terminal = nuotc::TestTerminal::new(60, 1);
        terminal.draw(|frame| {
            draw_view_header_hints(frame, frame.area(), &hints(SceneKind::Settings), &theme);
        });
        for cell in &terminal.buffer().content {
            assert_eq!(cell.bg, theme.raised());
        }
    }

    #[test]
    fn parent_status_context_marks_attention_states() {
        assert_eq!(
            parent_status_context(nuo_wire::ParentStatus::Running),
            "main running"
        );
        assert_eq!(
            parent_status_context(nuo_wire::ParentStatus::NeedsApproval),
            "⚠ main approval needed"
        );
        assert!(!parent_status_needs_attention(nuo_wire::ParentStatus::Idle));
        assert!(!parent_status_needs_attention(nuo_wire::ParentStatus::Running));
        assert!(parent_status_needs_attention(nuo_wire::ParentStatus::NeedsApproval));
        assert!(parent_status_needs_attention(nuo_wire::ParentStatus::Failed));
    }

    #[test]
    fn row2_renders_tab_breadcrumbs_and_subagent_drill_in() {
        let crumbs = [SceneKind::Thread, SceneKind::Subagent];
        let row = render_row2(
            80,
            ViewHints {
                kind: SceneKind::Subagent,
                context: Some("[EXPLORE] inspect codebase"),
                breadcrumbs: Some(&crumbs),
                can_back: true,
                can_forward: false,
                ..hints(SceneKind::Subagent)
            },
        );
        assert!(row.contains("< "), "shows back affordance: {row}");
        assert!(row.contains("thread > subagent"), "shows breadcrumb trail: {row}");
        assert!(row.contains("[EXPLORE] inspect codebase"), "shows scene context: {row}");
    }
}

