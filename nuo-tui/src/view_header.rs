//! Two-row head band for every scene.
//!
//! The head band is the fixed, always-present chrome pinned to the terminal's
//! top edge. It is **two rows by default** (ADR-0024) and each row has one
//! job:
//!
//! - **Row 1 — session identity.** The ambient *session* facts that never
//!   change while the user navigates scenes: the `SESSION` identity, the
//!   session's persistent-id tail, the staffing `[ROLE]` badge, and the bound
//!   workspace. Every scene (Conversation, Dashboard, Settings, Subagent zoom,
//!   aside) draws this same row, so the session the client is attached to is
//!   never hidden by the scene beneath it.
//! - **Row 2 — scene + context + run-mode status.** The *scene* the user is
//!   standing in, named plainly (`conversation`, `dashboard`, `settings`,
//!   `subagent`, `aside`), then the scene's context on the left — for the
//!   conversation that is the chat's title, for a subagent its task label, for
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
/// context on the left, and the session's persistent run-mode flags plus the
/// standing `C-x menu` namespace pair on the right.
///
/// Row 2 **stands up on every scene** (ADR-0023 `[INV-HINT-01]`): it always
/// carries the `C-x menu` namespace pair — the single entry point for the
/// Command Palette / surface switcher — so that shortcut is discoverable
/// everywhere. Scene-specific context (the conversation's chat title, an
/// aside's parent status, a subagent's task label, the dashboard's fleet
/// summary) leads it.
///
/// The row never spells a scene exit as `Esc`: Esc does not close Scenes
/// (ADR-0205 `[INV-TUI-CLEAN-02]`). Leaving a Scene is the `C-x` scene
/// namespace's job, so the affordance is that namespace's opening key
/// (ADR-0298 §3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ViewHints<'a> {
    /// Which scene the row describes — decides the leading scene label.
    pub kind: ViewKind,
    /// Scene context shown after the label: the conversation's chat title, an
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
}

impl ViewHints<'_> {
    /// Row 2 always has content (ADR-0024): the scene label and the `C-x menu`
    /// namespace pair stand up on every scene, so the head band is always two
    /// rows. Retained as a method so callers keep one place to ask the band's
    /// row inventory.
    pub(crate) fn has_content(&self) -> bool {
        true
    }
}

/// Which scene the header band is describing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ViewKind {
    Session,
    Btw,
    Subagent,
    Settings,
    Dashboard,
}

impl ViewKind {
    /// The plain, lowercase name of the scene shown as row 2's leading label
    /// (ADR-0024): the default home scene is `conversation`; the rest name
    /// themselves.
    pub(crate) fn scene_label(self) -> &'static str {
        match self {
            ViewKind::Session => "conversation",
            ViewKind::Btw => "aside",
            ViewKind::Subagent => "subagent",
            ViewKind::Settings => "settings",
            ViewKind::Dashboard => "dashboard",
        }
    }
}

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
) {
    let full_width = rect.width as usize;
    if full_width < STEP_MIN_WIDTH {
        return;
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
    let title_style = fill.fg(theme.heading()).add_modifier(Modifier::BOLD);
    let tag_style = fill.fg(theme.dim());
    let badge_style = fill.fg(theme.brand()).add_modifier(Modifier::BOLD);
    let primary_style = fill.fg(theme.brand()).add_modifier(Modifier::BOLD);

    // The text column is the full row minus the shared horizontal inset on
    // each side; the inset itself is painted as pad spans so the band's
    // background still owns every cell of the row.
    let pad = TRANSCRIPT_H_INSET as usize;
    let text_width = full_width.saturating_sub(2 * pad);

    const TITLE: &str = "SESSION ";
    let title_width = TITLE.width();
    // The tag renders as `<tag> ` right after the title (the title already ends
    // with a space); the badge (`[ROLE]`) follows the same rule; the workspace
    // trails both. The workspace truncates first when the row runs short.
    let tag_width = if tag.is_empty() { 0 } else { tag.width() + 1 };
    let badge_width = if badge.is_empty() {
        0
    } else {
        badge.width() + 1
    };
    let workspace_budget = text_width.saturating_sub(title_width + tag_width + badge_width + 1);
    let workspace = truncate_to_width(head.workspace, workspace_budget);
    let workspace_width = workspace.width();
    let gap =
        text_width.saturating_sub(title_width + tag_width + badge_width + workspace_width);

    let mut spans = vec![Span::styled(" ".repeat(pad), fill)];
    spans.push(Span::styled(TITLE, title_style));
    if !tag.is_empty() {
        spans.push(Span::styled(format!("{tag} "), tag_style));
    }
    if !badge.is_empty() {
        spans.push(Span::styled(format!("{badge} "), badge_style));
    }
    if !workspace.is_empty() {
        spans.push(Span::styled(workspace, primary_style));
    }
    // Trailing pad so the band's background owns the row out to the terminal's
    // right edge. The palette affordance is deliberately **not** on this row:
    // it lives on the row-2 namespace legend (`C-x menu`), the same entry point
    // on every scene (ADR-0023).
    spans.push(Span::styled(
        " ".repeat(gap + pad),
        fill,
    ));

    frame.render_widget(Paragraph::new(Line::from(spans)), rect);
}

/// Draw the header band's second row: the **scene row** (ADR-0024). It names
/// the scene the user is standing in (the plain lowercase label — `conversation`
/// for the default home scene, `dashboard`, `settings`, `subagent`, `aside`),
/// then the scene's own context (the chat title, the aside's parent status, a
/// subagent's task label, the dashboard's fleet summary), and finally — on the
/// right — the session's persistent run-mode flags (`UNATTENDED`, `UNCONFINED`)
/// ahead of the standing `C-x menu` namespace pair (ADR-0023 `[INV-HINT-01]`).
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
    let flag_style = fill.fg(theme.warn()).add_modifier(Modifier::BOLD);

    let width = rect.width as usize;
    let pad = TRANSCRIPT_H_INSET as usize;
    let text_width = width.saturating_sub(2 * pad);

    // The right side: the session's persistent run-mode flags, then the
    // standing `C-x menu` namespace pair (ADR-0023). The flags are the session
    // facts the user most needs never to lose sight of, so they sit here on the
    // scene row rather than competing with row 1's identity.
    let mut flags = String::new();
    if hints.unattended {
        flags.push_str("UNATTENDED ");
    }
    if !hints.confined {
        flags.push_str("UNCONFINED ");
    }
    let flags_width = flags.width();
    let affordance = crate::components::keycap::KeyAffordance::from_key(
        crate::keymap::Key::CTRL_X,
        SCENE_NAMESPACE_LABEL,
    );
    let right_width = flags_width + affordance.width();

    // The left side: the scene label, then the scene's context. The context
    // truncates first (the label is the row's anchor) when the row runs short;
    // the right side (flags + namespace) is always retained.
    let label = hints.kind.scene_label();
    let label_width = label.width();
    let context_budget = text_width.saturating_sub(label_width + 2 + right_width + 2);
    let context = hints
        .context
        .filter(|c| !c.is_empty())
        .map(|c| truncate_to_width(c, context_budget));

    let mut spans = vec![Span::styled(" ".repeat(pad), fill)];
    spans.push(Span::styled(label, label_style));
    let mut left_width = label_width;
    if let Some(context) = context {
        spans.push(Span::styled("  ", fill));
        let context_width = context.width();
        spans.push(Span::styled(context, context_style));
        left_width += 2 + context_width;
    }
    let gap = text_width.saturating_sub(left_width + right_width);
    spans.push(Span::styled(" ".repeat(gap), fill));
    if !flags.is_empty() {
        spans.push(Span::styled(flags, flag_style));
    }
    let [key_span, label_span] = affordance.render_spans(theme, bg);
    spans.push(key_span);
    spans.push(label_span);
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
        ViewHints {
            kind,
            context: None,
            context_warn: false,
            unattended: false,
            confined: true,
        }
    }

    /// Row 1 is the session identity, uniform across scenes: `SESSION`, the
    /// id tail, the `[ROLE]` badge, and the workspace. It no longer carries the
    /// run-mode flags (those moved to row 2 with the chat title, ADR-0024).
    #[test]
    fn row1_is_session_identity() {
        let head = SessionHead {
            session_id: "sess-01a2b3c4",
            workspace: "~/projects/xx",
            role: Some("developer"),
            switching_target: None,
        };
        let row = render_row1(80, head);
        assert!(row.starts_with("  SESSION b3c4 [DEVELOPER] ~/projects/xx"), "{row}");
        assert!(
            !row.contains("UNATTENDED") && !row.contains("UNCONFINED"),
            "the run-mode flags live on row 2 now: {row}"
        );
        assert!(!row.contains("palette"), "no palette keycap on row 1: {row}");
    }

    #[test]
    fn row1_workspace_free_hides_workspace_and_shows_role_badge() {
        let head = SessionHead {
            session_id: "sess-01a2b3c4",
            workspace: "",
            role: Some("philosophist"),
            switching_target: None,
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
        };
        let row = render_row1(80, head);
        assert!(row.starts_with("  SESSION b3c4 [SECURITY-AUDITOR] ~/projects/xx"), "{row}");
    }

    #[test]
    fn row1_shows_switching_target_loading() {
        let head = SessionHead {
            session_id: "sess-01a2b3c4",
            workspace: "~/projects/xx",
            role: None,
            switching_target: Some("7c405d7e"),
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
    /// keeps the `C-x menu` namespace pair on the right (ADR-0023/0024).
    #[test]
    fn row2_names_the_scene_with_context_and_the_namespace() {
        let row = render_row2(
            80,
            ViewHints {
                kind: ViewKind::Session,
                context: Some("Fix the retry loop"),
                ..hints(ViewKind::Session)
            },
        );
        assert!(row.starts_with("  conversation  Fix the retry loop"), "{row}");
        assert!(row.contains("Ctrl-x menu"), "namespace retained: {row}");
        assert!(!row.contains("Esc"), "Esc never advertises a scene exit: {row}");
    }

    #[test]
    fn row2_conversation_without_a_title_shows_the_label_alone() {
        let row = render_row2(80, hints(ViewKind::Session));
        assert!(row.starts_with("  conversation"), "{row}");
        assert!(row.contains("Ctrl-x menu"), "{row}");
        // The label is the only left-side content: nothing sits between it and
        // the right-aligned namespace pair beyond the gap fill.
        assert!(
            !row.contains("Fix") && !row.trim_end().starts_with("conversation  Fix"),
            "{row}"
        );
    }

    #[test]
    fn row2_scene_labels_are_per_scene() {
        for (kind, label) in [
            (ViewKind::Session, "conversation"),
            (ViewKind::Btw, "aside"),
            (ViewKind::Subagent, "subagent"),
            (ViewKind::Settings, "settings"),
            (ViewKind::Dashboard, "dashboard"),
        ] {
            assert_eq!(kind.scene_label(), label);
            let row = render_row2(80, hints(kind));
            assert!(row.starts_with(&format!("  {label}")), "{kind:?}: {row}");
        }
    }

    /// The run-mode flags sit on the right of row 2, ahead of the namespace.
    #[test]
    fn row2_carries_unattended_and_unconfined_flags_on_the_right() {
        let row = render_row2(
            80,
            ViewHints {
                unattended: true,
                confined: false,
                ..hints(ViewKind::Dashboard)
            },
        );
        let flags = row.find("UNATTENDED").expect("unattended flag");
        let ns = row.find("Ctrl-x").expect("namespace pair");
        assert!(row.contains("UNCONFINED"), "unconfined flag: {row}");
        assert!(flags < ns, "flags precede the namespace pair: {row}");
        assert!(row.trim_end().ends_with("menu"), "{row}");
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
                ..hints(ViewKind::Dashboard)
            },
            &theme,
        );
        let urgent = render_row2_cells(
            80,
            ViewHints {
                context: Some("needs approval"),
                context_warn: true,
                ..hints(ViewKind::Dashboard)
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

    /// Every scene stands up row 2 (ADR-0023 `[INV-HINT-01]`): the band is
    /// always two rows, so the namespace pair is discoverable everywhere.
    #[test]
    fn row2_stands_up_on_every_scene() {
        for kind in [
            ViewKind::Session,
            ViewKind::Btw,
            ViewKind::Subagent,
            ViewKind::Settings,
            ViewKind::Dashboard,
        ] {
            assert!(hints(kind).has_content(), "{kind:?} stands up row 2");
            let row = render_row2(80, hints(kind));
            assert!(
                row.contains("Ctrl-x") && row.contains("menu"),
                "{kind:?} offers the namespace pair: {row}"
            );
        }
    }

    #[test]
    fn row2_band_paints_the_full_row_width() {
        let theme = Theme::default();
        let mut terminal = nuotc::TestTerminal::new(60, 1);
        terminal.draw(|frame| {
            draw_view_header_hints(frame, frame.area(), &hints(ViewKind::Settings), &theme);
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
}

