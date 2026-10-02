//! Contextual first-row header for every scene.
//!
//! Every scene — Conversation, `/btw` Aside, TaskInspection, Settings — shares
//! one layout rule for the head row: identity and scene-specific context on the
//! left, mode / index metadata on the right. Navigation shortcuts do **not**
//! live on the head row; they live on row 2 (ADR-0103 §3). Row 2 is
//! demand-driven (ADR-0104): it exists only while the scene has something to
//! say that no other surface already says. The aside and TaskInspection scenes
//! are identified by a breadcrumb, so their row 2 is that crumb plus the `C-x`
//! scene namespace's opening key — and nothing else, because their remaining
//! chords are remappable and a fixed keycap row cannot advertise a remap
//! faithfully (ADR-0205: chrome never advertises what it cannot honour).
//! Esc is never the advertised exit: it does not close Scenes (ADR-0205
//! `[INV-TUI-CLEAN-02]`), so the legend points at the namespace that does
//! (ADR-0298 §3). Keeping this outside disclosure rendering also leaves one
//! clear extension point for future focused scenes.

use nuotc::{Frame, Line, Modifier, Paragraph, Rect, Span, Style};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::{STEP_MIN_WIDTH, SubagentBarInfo, TRANSCRIPT_H_INSET, Theme};

pub(crate) enum ViewHeader<'a> {
    /// The Main session view: `SESSION` identity, the session's persistent-id
    /// tail, and the workspace on the left; the session mode (e.g.
    /// `DELEGATED`) on the right.
    Session(&'a SessionHead<'a>),
    /// The `/btw` aside view (ADR-0103): identity + parent status on row 1;
    /// its shortcuts live on row 2 via [`draw_view_header_hints`].
    Btw(BtwHead),
    /// The TaskInspection scene (ADR-0205): `SUBAGENT` identity, the task's
    /// role tag, its label, and the `N of M` sibling index; row 2 is the
    /// `Main › Subagent[role]` breadcrumb plus the `Ctrl-x scene` namespace.
    Subagent(&'a SubagentBarInfo),
    /// Full-screen Settings scene (ADR-0141): `SETTINGS` identity.
    Settings,
    /// The session dashboard (`/dashboard`): `DASHBOARD` identity, its scope,
    /// and a live fleet summary on the right. It carries no breadcrumb (it is
    /// a peer scene, not a drill-in), so row 2 is optional.
    Dashboard(DashboardHead),
}

/// Row-1 context for the dashboard scene's head.
///
/// Owned (not borrowed) because the dashboard aggregates its own fleet summary
/// from the live snapshot; there is no long-lived string to point at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DashboardHead {
    /// Fleet count summary, pre-joined by the caller (the dashboard owns the
    /// aggregation): e.g. `"3 session(s)  1 running  12k tokens"`. Empty
    /// renders no right-side summary.
    pub summary: String,
    /// `true` while at least one monitored session needs attention, which
    /// escalates the summary to the warning tone.
    pub needs_attention: bool,
}

/// Row-1 content for the `/btw` aside view's head.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BtwHead {
    /// Coarse primary-session status, rendered as the left context's meta
    /// segment ("main running", …).
    pub parent: nuo_contracts::ParentStatus,
}

/// Row-2 (view affordance) context for every view kind. One struct because
/// the legend's *shape* is shared: a leading descriptive segment (the main
/// view's live aside count, the aside view's parent state) followed by
/// keycap pairs for the view's own shortcuts.
///
/// The band is **demand-driven** (ADR-0104): row 2 renders only when
/// [`ViewHints::has_content`] is `true` — i.e. when this view genuinely has
/// view-specific affordances to announce. Nothing renders a row for pairs
/// that are either global or already carried by a *more specific* surface: the
/// main view's interrupt lives on the activity bar (which spells the real
/// double-Esc arming, `Esc Esc interrupt`).
///
/// The legend never spells a scene exit as `Esc`: Esc does not close Scenes
/// (ADR-0205 `[INV-TUI-CLEAN-02]`). Leaving a Scene is the `C-x` scene
/// namespace's job, so the breadcrumb line's affordance is that namespace's
/// opening key (ADR-0298 §3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ViewHints<'a> {
    /// Which view the legend belongs to — decides the keycap set.
    pub kind: ViewKind,
    /// Live aside count + how many have a round in flight (main view only,
    /// ADR-0103 §3). `None` renders no aside segment.
    pub asides: Option<AsidesChip>,
    /// Optional view stack breadcrumbs.
    pub breadcrumbs: Option<&'a str>,
}

impl ViewHints<'_> {
    /// Whether row 2 has anything view-specific to say (ADR-0104). `false`
    /// means the caller must not reserve the row at all — the head collapses
    /// to a single row and the transcript reclaims the line.
    ///
    /// - **Breadcrumb-identified pages** (aside, subagent task): always — the
    ///   crumb line plus the `Ctrl-x scene` namespace *is* the row. See the
    ///   notes below for why those pages advertise nothing else.
    /// - **Session**: while asides are live — the chip plus the asides chord.
    /// - **Settings**: always — the `Ctrl-x` namespace is the center's exit.
    /// - **Dashboard**: always — same reason; it is a peer scene, not a
    ///   drill-in, so it has no crumb but still needs its namespace row.
    /// - **Btw** / **Subagent** *without* a crumb:
    ///   unreachable (`event_loop::render` sets the crumb whenever it sets
    ///   those kinds) and deliberately blank. Their remaining chords (the
    ///   aside interrupt, the sibling walks) are remappable
    ///   (`session.prev_sibling` / `session.next_sibling`) and a fixed keycap
    ///   row cannot render a remap faithfully (ADR-0205: chrome never
    ///   advertises what it cannot honour), so they are left to the Command
    ///   Palette.
    pub(crate) fn has_content(&self) -> bool {
        // A breadcrumb-identified page always carries row 2.
        if self.breadcrumbs.is_some() {
            return true;
        }
        match self.kind {
            ViewKind::Session => self.asides.is_some(),
            ViewKind::Settings | ViewKind::Dashboard => true,
            // Crumb-less aside/subagent pages cannot occur
            // (`event_loop::render` sets the crumb with the kind); nothing to
            // render if one ever did.
            ViewKind::Btw | ViewKind::Subagent => false,
        }
    }
}

/// The main view's live-asides chip: count + running count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AsidesChip {
    pub total: usize,
    pub running: usize,
}

/// Which view the header band is describing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ViewKind {
    Session,
    Btw,
    Subagent,
    Settings,
    Dashboard,
}

impl From<&ViewHeader<'_>> for ViewKind {
    fn from(header: &ViewHeader<'_>) -> Self {
        match header {
            ViewHeader::Session(_) => ViewKind::Session,
            ViewHeader::Dashboard(_) => ViewKind::Dashboard,
            ViewHeader::Btw(_) => ViewKind::Btw,
            ViewHeader::Subagent(_) => ViewKind::Subagent,
            ViewHeader::Settings => ViewKind::Settings,
        }
    }
}

/// Left/right content for the Main session view's head row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SessionHead<'a> {
    /// The session's persistent id (full string). Only its last four
    /// characters are shown, dimmed, as a disambiguating tag.
    pub session_id: &'a str,
    /// Tilde-shortened workspace path (e.g. `~/projects/xx`), or empty when unbound.
    pub workspace: &'a str,
    /// Active staffing role for this session (ADR-0244).
    pub role: Option<&'a str>,
    /// `true` while the session runs in unattended execution mode
    /// (`--unattended` / `/unattended on`). Shown as a warning-toned
    /// `UNATTENDED` tag on the right — the session's persistent mode flag.
    pub unattended: bool,
    /// `false` while the session's workspace filesystem confinement is
    /// disabled (`/confinement off`). Shown as a warning-toned `UNCONFINED`
    /// tag on the right.
    pub confined: bool,
    /// When switching to another session, holds the target session id.
    pub switching_target: Option<&'a str>,
    /// The *effective* chord of the Command Palette (ADR-0238: chrome renders
    /// the binding that fires, so a user remap shows through). `None` when the
    /// command has no chord at all — then no keycap is drawn.
    pub palette_key: Option<crate::keymap::Key>,
}

struct HeaderContent {
    title: &'static str,
    /// The identity tail that sits right after the title (session-id tail,
    /// dimmed). Empty when the variant has none.
    tag: String,
    /// Optional `[ROLE]`-style tag rendered in the brand tone right after the
    /// identity tag (the Subagent page's role). Empty when absent.
    badge: String,
    primary: String,
    meta: String,
    action: String,
}

/// Draw a single contextual header row. The primary action is always retained
/// on narrow terminals; descriptive text truncates first, while Subagent sibling
/// shortcuts appear when there is enough room for them to remain legible.
///
/// The band's background spans the rect's full width — the head is top-level
/// chrome pinned to the terminal's top edge, so its `body` surface reaches
/// both edges like the Subagent key-legend band at the bottom edge. The *text*
/// keeps the shared [`TRANSCRIPT_H_INSET`] horizontal inset (rendered as pad
/// spans) so it stays aligned with the transcript band below.
pub(crate) fn draw_view_header(
    frame: &mut Frame,
    rect: Rect,
    header: &ViewHeader<'_>,
    theme: &Theme,
) {
    let full_width = rect.width as usize;
    if full_width < STEP_MIN_WIDTH {
        return;
    }

    let content = match header {
        ViewHeader::Session(head) => {
            let mut action = String::new();
            if head.unattended {
                action.push_str("UNATTENDED ");
            }
            if !head.confined {
                action.push_str("UNCONFINED ");
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
            HeaderContent {
                title: " SESSION ",
                tag,
                badge,
                primary: head.workspace.to_string(),
                meta: String::new(),
                action,
            }
        }
        // Subagent and /btw are contextual views that replace the session head.
        ViewHeader::Btw(head) => HeaderContent {
            title: " /btw ",
            tag: String::new(),
            badge: String::new(),
            primary: "Side conversation".to_string(),
            meta: parent_status_label(head.parent).to_string(),
            // Row 1 is identity + status only — the exit affordance lives on
            // row 2 as the `Ctrl-x scene` namespace (ADR-0103 §3 / ADR-0298),
            // so no exit pair appears here.
            action: String::new(),
        },
        // The Subagent head carries the page's whole identity: uppercase
        // identity + `[ROLE]` tag + task title on the left, and pure index
        // metadata on the right — the sibling count `(i/n)`, shown only when
        // there is more than one sibling. The page has no shortcut legend
        // (ADR-0205: chrome carries no affordance it cannot honour, and the
        // page's three chords are one Esc and two remappable sibling walks).
        ViewHeader::Subagent(bar) => HeaderContent {
            title: " SUBAGENT ",
            tag: String::new(),
            badge: bar
                .role
                .as_ref()
                .map(|role| format!("[{}]", role.to_uppercase()))
                .unwrap_or_default(),
            primary: bar.label.clone(),
            meta: String::new(),
            action: if bar.total > 1 {
                format!("({}/{}) ", bar.index, bar.total)
            } else {
                String::new()
            },
        },
        ViewHeader::Settings => HeaderContent {
            title: " SETTINGS ",
            tag: String::new(),
            badge: String::new(),
            primary: String::new(),
            meta: String::new(),
            action: String::new(),
        },
        ViewHeader::Dashboard(head) => HeaderContent {
            title: " DASHBOARD ",
            tag: String::new(),
            badge: String::new(),
            primary: "all projects".to_string(),
            meta: String::new(),
            action: head.summary.clone(),
        },
    };

    let bg = theme.raised();
    let fill = Style::default().bg(bg);
    let title_style = fill.fg(theme.heading()).add_modifier(Modifier::BOLD);
    let tag_style = fill.fg(theme.dim());
    let badge_style = fill.fg(theme.brand()).add_modifier(Modifier::BOLD);
    let primary_style = fill.fg(theme.brand()).add_modifier(Modifier::BOLD);
    let meta_style = match header {
        ViewHeader::Btw(head)
            if matches!(
                head.parent,
                nuo_contracts::ParentStatus::NeedsApproval
                    | nuo_contracts::ParentStatus::NeedsInput
                    | nuo_contracts::ParentStatus::Failed
                    | nuo_contracts::ParentStatus::Interrupted
            ) =>
        {
            fill.fg(theme.warn()).add_modifier(Modifier::BOLD)
        }
        _ => fill.fg(theme.muted()),
    };
    // The session mode flag (`DELEGATED`) and the dashboard's `⚠ need
    // attention` fleet summary both read as persistent safety states, so they
    // take the warning tone; every other variant's right side is quiet
    // metadata (the Subagent sibling count, the dashboard's quiet fleet count).
    let action_style = match header {
        ViewHeader::Session(_) => fill.fg(theme.warn()).add_modifier(Modifier::BOLD),
        ViewHeader::Dashboard(head) if head.needs_attention => {
            fill.fg(theme.warn()).add_modifier(Modifier::BOLD)
        }
        _ => fill.fg(theme.muted()),
    };

    // The text column is the full row minus the shared horizontal inset on
    // each side; the inset itself is painted as pad spans so the band's
    // background still owns every cell of the row.
    let pad = TRANSCRIPT_H_INSET as usize;
    let text_width = full_width.saturating_sub(2 * pad);

    let title_width = content.title.width();
    // The tag renders as `<tag> ` (tag + one trailing space) right after the
    // title — the title already ends with a space, so the tag needs no
    // leading separator. The badge (`[ROLE]`) follows the same rule.
    let tag_width = if content.tag.is_empty() {
        0
    } else {
        content.tag.width() + 1
    };
    let badge_width = if content.badge.is_empty() {
        0
    } else {
        content.badge.width() + 1
    };
    let action_width = content.action.width();
    // A persistent, right-aligned `Ctrl+P palette` affordance on the main
    // session head. `Ctrl+P` is the Command Palette chord (the model bar and
    // footer hints advertise it), so surfacing it in the head's right-side
    // space keeps the shortcut discoverable on every session view.
    let palette = match header {
        ViewHeader::Session(head) => head
            .palette_key
            .map(|key| crate::components::keycap::KeyAffordance::from_key(key, "palette")),
        _ => None,
    };
    let palette_width = palette.map(|a| a.width()).unwrap_or(0);
    let right_separator = usize::from(action_width > 0 && palette_width > 0);
    let right_reserved = action_width + palette_width + right_separator;
    let left_budget =
        text_width.saturating_sub(title_width + tag_width + badge_width + right_reserved + 1);
    let left = fit_context(&content.primary, &content.meta, left_budget);
    let left_width: usize = left.iter().map(|(text, _)| text.width()).sum();
    let gap = text_width
        .saturating_sub(title_width + tag_width + badge_width + left_width + right_reserved);

    let mut spans = vec![Span::styled(" ".repeat(pad), fill)];
    spans.push(Span::styled(content.title, title_style));
    if !content.tag.is_empty() {
        spans.push(Span::styled(format!("{} ", content.tag), tag_style));
    }
    if !content.badge.is_empty() {
        spans.push(Span::styled(format!("{} ", content.badge), badge_style));
    }
    for (text, tone) in left {
        let style = match tone {
            Tone::Primary => primary_style,
            Tone::Meta => meta_style,
        };
        spans.push(Span::styled(text, style));
    }
    spans.push(Span::styled(" ".repeat(gap), fill));
    if !content.action.is_empty() {
        spans.push(Span::styled(content.action, action_style));
    }
    if let Some(palette) = palette {
        if right_separator > 0 {
            spans.push(Span::styled(" ", fill));
        }
        let [key_span, label_span] = palette.render_spans(theme, bg);
        spans.push(key_span);
        spans.push(label_span);
    }
    // Trailing pad (plus any shortfall after the right-aligned action) so the
    // band's background owns the row out to the terminal's right edge.
    let used = pad + title_width + tag_width + badge_width + left_width + gap + right_reserved;
    spans.push(Span::styled(
        " ".repeat(full_width.saturating_sub(used)),
        fill,
    ));

    frame.render_widget(Paragraph::new(Line::from(spans)), rect);
}

/// Draw the header band's second row: the view-level affordance legend
/// (ADR-0103 §3, demand-gated by ADR-0104). Row 1 carries identity +
/// status; this row carries *what the keys do in this view*.
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
    let note_style = fill.fg(theme.dim());

    if let Some(crumbs) = hints.breadcrumbs {
        let left = Span::styled(format!("   {crumbs}"), Style::default().fg(theme.fg()));
        // The crumb line's affordance is the scene namespace's opening key —
        // never `Esc back`. Esc does not close Scenes (ADR-0205
        // `[INV-TUI-CLEAN-02]`); `C-x` opens the namespace whose `w`/`k` do
        // (ADR-0298 §3). The keycap names the namespace, not one of its verbs,
        // because the crumb line is shared by pages that are already at their
        // home scene (where `w`/`k` have nothing to close).
        let affordance = crate::components::keycap::KeyAffordance::from_key(
            crate::keymap::Key::CTRL_X,
            SCENE_NAMESPACE_LABEL,
        );
        let [key_span, label_span] = affordance.render_spans(theme, bg);
        let right_pad = Span::styled("   ", fill);

        let left_len = crumbs.width() + 3;
        let right_len = affordance.width() + 3;
        let pad_len = (rect.width as usize).saturating_sub(left_len + right_len);
        let pad = " ".repeat(pad_len);

        let line = Line::from(vec![left, Span::raw(pad), key_span, label_span, right_pad]);
        frame.render_widget(Paragraph::new(line).style(fill), rect);
        return;
    }

    // Everything below is the *crumb-less* subset of pages. The aside and the
    // subagent task are identified by their breadcrumb (`Main › Aside`,
    // `Main › Subagent[role]`, set by `event_loop::render` whenever it sets
    // those kinds), so they were already rendered and returned above — a
    // hint set claiming one of those kinds with no breadcrumb is a caller bug,
    // not a state with a legend to invent (ADR-0238).
    debug_assert!(
        !matches!(hints.kind, ViewKind::Btw | ViewKind::Subagent),
        "a crumb-less {:?} page has no legend to render; breadcrumb-identified \
         pages must carry their crumb",
        hints.kind
    );

    // Leading descriptive segment (before the keycaps): the main conversation's
    // live-asides chip. The aside's parent status lives on row 1 (the page
    // header), not here.
    let note: Option<String> = match hints.kind {
        ViewKind::Session => hints.asides.as_ref().map(|chip| {
            if chip.running > 0 {
                format!("btw: {} total ({} active)", chip.total, chip.running)
            } else {
                format!("btw: {} total", chip.total)
            }
        }),
        ViewKind::Btw | ViewKind::Subagent | ViewKind::Settings | ViewKind::Dashboard => None,
    };

    let pairs: Vec<crate::components::keycap::KeyAffordance> = match hints.kind {
        ViewKind::Session => {
            let mut pairs = Vec::new();
            if hints.asides.is_some() {
                pairs.push(crate::components::keycap::KeyAffordance::from_key(
                    crate::keymap::Key::F5,
                    "asides",
                ));
            }
            pairs
        }
        ViewKind::Settings | ViewKind::Dashboard => {
            // These scenes' own exit is the `C-x` namespace, so their legend
            // names the namespace rather than a dismiss chord they do not have
            // (ADR-0298 §3).
            vec![crate::components::keycap::KeyAffordance::from_key(
                crate::keymap::Key::CTRL_X,
                SCENE_NAMESPACE_LABEL,
            )]
        }
        // Unreachable — a crumb-less aside/subagent page is a caller bug,
        // asserted above. Rendering nothing keeps a malformed hint set from
        // painting a legend that no surface honours (ADR-0205: chrome never
        // advertises what it cannot honour).
        ViewKind::Btw | ViewKind::Subagent => Vec::new(),
    };

    let width = rect.width as usize;
    let chosen: Vec<crate::components::keycap::KeyAffordance> = {
        let mut chosen = pairs.clone();
        loop {
            let note_width = note.as_ref().map(|n| n.width() + 4).unwrap_or(0);
            let pairs_width: usize = chosen.iter().map(|affordance| affordance.width()).sum();
            let needed =
                note_width + pairs_width + HEAD_HINTS_PAIR_GAP * chosen.len().saturating_sub(1);
            if needed <= width.saturating_sub(2 * HEAD_HINTS_MARGIN_MIN) || chosen.len() <= 1 {
                break;
            }
            chosen.pop();
        }
        chosen
    };

    let note_width = note.as_ref().map(|n| n.width()).unwrap_or(0);
    let pairs_width: usize = chosen.iter().map(|affordance| affordance.width()).sum();
    let gaps =
        HEAD_HINTS_PAIR_GAP * chosen.len().saturating_sub(1) + if note.is_some() { 4 } else { 0 };
    let content_width = note_width + pairs_width + gaps;
    let margin = ((width.saturating_sub(content_width)) / 2).max(HEAD_HINTS_MARGIN_MIN);

    let mut spans = vec![Span::styled(" ".repeat(margin), fill)];
    if let Some(note) = note {
        spans.push(Span::styled(format!("{note}    "), note_style));
    }
    for (i, affordance) in chosen.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" ".repeat(HEAD_HINTS_PAIR_GAP), fill));
        }
        let [key_span, label_span] = affordance.render_spans(theme, bg);
        spans.push(key_span);
        spans.push(label_span);
    }
    spans.push(Span::styled(
        " ".repeat(width.saturating_sub(margin + content_width)),
        fill,
    ));

    frame.render_widget(Paragraph::new(Line::from(spans)), rect);
}

const HEAD_HINTS_PAIR_GAP: usize = 3;
const HEAD_HINTS_MARGIN_MIN: usize = 2;

/// The row-2 legend's name for the `C-x` scene namespace (ADR-0298 §3). The
/// keycap names the namespace rather than one of its verbs — `w`/`k` close a
/// scene, but the same row is shared by pages already at their home scene,
/// where there is nothing to close.
const SCENE_NAMESPACE_LABEL: &str = "scene";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tone {
    Primary,
    Meta,
}

fn fit_context(primary: &str, meta: &str, budget: usize) -> Vec<(String, Tone)> {
    if budget == 0 {
        return Vec::new();
    }

    if meta.is_empty() {
        return vec![(truncate_to_width(primary, budget), Tone::Primary)];
    }

    let primary_width = primary.width();
    let meta_width = meta.width();
    const SEPARATOR: &str = "  ";
    let separator_width = SEPARATOR.width();

    if primary_width + separator_width + meta_width <= budget {
        return vec![
            (primary.to_string(), Tone::Primary),
            (SEPARATOR.to_string(), Tone::Meta),
            (meta.to_string(), Tone::Meta),
        ];
    }

    if budget > meta_width + separator_width {
        let primary_budget = budget - meta_width - separator_width;
        return vec![
            (truncate_to_width(primary, primary_budget), Tone::Primary),
            (SEPARATOR.to_string(), Tone::Meta),
            (meta.to_string(), Tone::Meta),
        ];
    }

    vec![(truncate_to_width(meta, budget), Tone::Meta)]
}

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

fn parent_status_label(parent: nuo_contracts::ParentStatus) -> &'static str {
    match parent {
        nuo_contracts::ParentStatus::Idle => "[main: idle]",
        nuo_contracts::ParentStatus::Running => "[main: running]",
        nuo_contracts::ParentStatus::NeedsApproval => "[⚠ main: approval needed]",
        nuo_contracts::ParentStatus::NeedsInput => "[⚠ main: input needed]",
        nuo_contracts::ParentStatus::Failed => "[⚠ main: failed]",
        nuo_contracts::ParentStatus::Interrupted => "[⚠ main: interrupted]",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rendered_row(width: u16, header: ViewHeader<'_>) -> String {
        rendered_cells(width, header, &Theme::default())
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    /// The raw styled cells of a rendered head row — for assertions about
    /// tone (which cell carries which foreground), not just content.
    fn rendered_cells(width: u16, header: ViewHeader<'_>, theme: &Theme) -> Vec<nuotc::Cell> {
        let mut terminal = nuotc::TestTerminal::new(width, 1);
        terminal.draw(|frame| {
            draw_view_header(frame, frame.area(), &header, theme);
        });
        terminal.buffer().content.clone()
    }

    #[test]
    fn btw_header_identifies_page_parent_state_and_return_action() {
        let row = rendered_row(
            64,
            ViewHeader::Btw(BtwHead {
                parent: nuo_contracts::ParentStatus::NeedsApproval,
            }),
        );
        assert!(row.starts_with("   /btw Side conversation  [⚠ main: approval needed]"));
        assert!(!row.trim_end().contains("Esc back"));
    }

    #[test]
    fn aside_page_legend_is_its_breadcrumb_plus_the_scene_namespace() {
        // The aside page is identified by its breadcrumb, and `draw_view_header_hints`
        // short-circuits on a crumb: the reachable row is the crumb line plus the
        // `C-x` scene-namespace pair. Esc is deliberately NOT offered — it does
        // not close a Scene, and the aside's other chords are remappable, so
        // they are left to the Command Palette (ADR-0205/0238/0298).
        let theme = Theme::default();
        let hints = ViewHints {
            kind: ViewKind::Btw,
            asides: None,
            breadcrumbs: Some("Main › Aside"),
        };
        let mut terminal = nuotc::TestTerminal::new(80, 1);
        terminal.draw(|frame| {
            draw_view_header_hints(frame, frame.area(), &hints, &theme);
        });
        let row: String = terminal
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(row.starts_with("   Main › Aside"), "crumb leads: {row}");
        assert!(
            row.contains("Ctrl-x"),
            "the scene namespace is offered: {row}"
        );
        assert!(row.contains("scene"), "…spelt as the namespace: {row}");
        assert!(
            !row.contains("Esc"),
            "Esc never advertises a scene exit: {row}"
        );
        assert!(
            !row.contains("asides") && !row.contains("interrupt"),
            "the aside's remappable chords are not advertised on this row: {row}"
        );
    }

    #[test]
    fn crumb_less_aside_hints_render_nothing() {
        let theme = Theme::default();
        let hints = ViewHints {
            kind: ViewKind::Btw,
            asides: None,
            breadcrumbs: None,
        };
        assert!(!hints.has_content(), "no crumb, no row");
        let mut terminal = nuotc::TestTerminal::new(80, 1);
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            terminal.draw(|frame| {
                draw_view_header_hints(frame, frame.area(), &hints, &theme);
            });
        }))
        .ok();
        let row: String = terminal
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(
            row.trim().is_empty(),
            "a malformed hint set must not paint a legend no surface honours: {row:?}"
        );
    }

    #[test]
    fn main_hints_legend_omits_interrupt_even_while_running() {
        let theme = Theme::default();
        let hints = ViewHints {
            kind: ViewKind::Session,
            asides: None,
            breadcrumbs: None,
        };
        assert!(!hints.has_content(), "no asides → no row at all");
        let mut terminal = nuotc::TestTerminal::new(80, 1);
        terminal.draw(|frame| {
            draw_view_header_hints(frame, frame.area(), &hints, &theme);
        });
        let row: String = terminal
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(!row.contains("Esc"), "no interrupt pair: {row}");
        assert!(!row.contains("F1"), "no global help pair: {row}");
    }

    #[test]
    fn hints_presence_is_demand_driven_per_page_kind() {
        let mk = |kind: ViewKind, asides: bool| ViewHints {
            kind,
            asides: asides.then_some(AsidesChip {
                total: 1,
                running: 0,
            }),
            breadcrumbs: None,
        };
        assert!(!mk(ViewKind::Session, false).has_content());
        assert!(mk(ViewKind::Session, true).has_content());
        assert!(mk(ViewKind::Settings, false).has_content());
        // Crumb-less: the aside and subagent pages have no legend of their own
        // (they are identified by their crumb, which is a caller bug to omit).
        assert!(!mk(ViewKind::Btw, false).has_content());
        assert!(!mk(ViewKind::Subagent, false).has_content());
        assert!(!mk(ViewKind::Subagent, true).has_content());

        // A breadcrumb-identified page always carries the row, whichever kind
        // it is: the crumb line plus the `Ctrl-x` namespace is the legend.
        let crumbs = |kind: ViewKind| ViewHints {
            kind,
            asides: None,
            breadcrumbs: Some("Main › Aside"),
        };
        assert!(crumbs(ViewKind::Btw).has_content());
        assert!(crumbs(ViewKind::Subagent).has_content());
        assert!(crumbs(ViewKind::Session).has_content());
    }

    #[test]
    fn main_hints_legend_shows_aside_chip_when_live() {
        let theme = Theme::default();
        let hints = ViewHints {
            kind: ViewKind::Session,
            asides: Some(AsidesChip {
                total: 2,
                running: 1,
            }),
            breadcrumbs: None,
        };
        let mut terminal = nuotc::TestTerminal::new(80, 1);
        terminal.draw(|frame| {
            draw_view_header_hints(frame, frame.area(), &hints, &theme);
        });
        let row: String = terminal
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(row.contains("btw: 2 total (1 active)"), "aside chip: {row}");
        assert!(row.contains("asides"), "F5 pair: {row}");
        assert!(!row.contains("Esc"), "no interrupt pair: {row}");
    }

    #[test]
    fn breadcrumbs_render_in_row_two_with_the_scene_namespace() {
        let theme = Theme::default();
        let hints = ViewHints {
            kind: ViewKind::Session,
            asides: None,
            breadcrumbs: Some("Main › Subagent[explore]"),
        };
        let mut terminal = nuotc::TestTerminal::new(80, 1);
        terminal.draw(|frame| {
            draw_view_header_hints(frame, frame.area(), &hints, &theme);
        });
        let row: String = terminal
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(row.contains("Main › Subagent[explore]"));
        assert!(
            row.contains("Ctrl-x scene"),
            "the scene namespace is the crumb line's affordance: {row}"
        );
        assert!(
            !row.contains("Esc"),
            "Esc never advertises a scene exit on this row: {row}"
        );
    }

    #[test]
    fn subagent_header_shows_identity_role_title_and_sibling_index() {
        let info = SubagentBarInfo {
            role: Some("explore".to_string()),
            label: "inspect the renderer".to_string(),
            index: 1,
            total: 2,
        };
        let row = rendered_row(80, ViewHeader::Subagent(&info));
        assert_eq!(
            row,
            "   SUBAGENT [EXPLORE] inspect the renderer                              (1/2)   "
        );
    }

    /// ADR-0298 §3: the dashboard is chrome-identical to every other scene —
    /// its head comes from the shared band, not a homegrown row. Row 1 carries
    /// `DASHBOARD` + scope on the left and the fleet summary on the right; row 2
    /// carries the `Ctrl-x scene` namespace (it has no breadcrumb: it is a peer
    /// scene, not a drill-in).
    #[test]
    fn dashboard_head_is_the_shared_band_with_fleet_summary() {
        let row = rendered_row(
            80,
            ViewHeader::Dashboard(DashboardHead {
                summary: "3 session(s)  1 running  360 tokens ".to_string(),
                needs_attention: false,
            }),
        );
        assert!(row.starts_with("   DASHBOARD all projects"), "{row}");
        assert!(
            row.contains("3 session(s)"),
            "fleet summary on the right: {row}"
        );
        assert!(row.trim_end().ends_with("tokens"), "{row}");

        // A quiet fleet renders the summary muted; one needing attention
        // escalates it to the warning tone.
        let theme = Theme::default();
        let quiet = rendered_cells(
            80,
            ViewHeader::Dashboard(DashboardHead {
                summary: "1 session(s) ".to_string(),
                needs_attention: false,
            }),
            &theme,
        );
        let urgent = rendered_cells(
            80,
            ViewHeader::Dashboard(DashboardHead {
                summary: "1 need attention ⚠ ".to_string(),
                needs_attention: true,
            }),
            &theme,
        );
        let summary_fg = |cells: &[nuotc::Cell]| {
            let idx = cells
                .iter()
                .position(|c| c.symbol() == "1")
                .expect("summary starts with the count");
            cells[idx].fg
        };
        assert_eq!(summary_fg(&quiet), theme.muted(), "quiet fleet is muted");
        assert_eq!(
            summary_fg(&urgent),
            theme.warn(),
            "a fleet needing attention takes the warning tone"
        );

        // Row 2 always renders for the dashboard, and it names the namespace.
        let hints = ViewHints {
            kind: ViewKind::Dashboard,
            asides: None,
            breadcrumbs: None,
        };
        assert!(
            hints.has_content(),
            "the dashboard always carries its namespace row"
        );
    }

    #[test]
    fn session_header_shows_id_tail_workspace_and_unattended() {
        let head = SessionHead {
            session_id: "sess-01a2b3c4",
            workspace: "~/projects/xx",
            role: Some("developer"),
            unattended: true,
            confined: true,
            switching_target: None,
            palette_key: Some(crate::keymap::Key::CTRL_L),
        };
        let row = rendered_row(80, ViewHeader::Session(&head));
        assert!(row.starts_with("   SESSION b3c4 [DEVELOPER] ~/projects/xx"));
        let pos = row.find("UNATTENDED").expect("mode flag on the right");
        assert!(
            row[pos..].contains("Ctrl-l palette"),
            "palette affordance after the mode flag: {row}"
        );
        assert!(row.trim_end().ends_with("palette"));
    }

    #[test]
    fn session_header_workspace_free_hides_workspace_path_and_shows_role_badge() {
        let head = SessionHead {
            session_id: "sess-01a2b3c4",
            workspace: "",
            role: Some("philosophist"),
            unattended: false,
            confined: true,
            switching_target: None,
            palette_key: Some(crate::keymap::Key::CTRL_L),
        };
        let row = rendered_row(80, ViewHeader::Session(&head));
        assert!(row.starts_with("   SESSION b3c4 [PHILOSOPHIST]"));
        assert!(!row.contains("~/"));
    }

    #[test]
    fn session_header_shows_custom_user_role_badge() {
        let head = SessionHead {
            session_id: "sess-01a2b3c4",
            workspace: "~/projects/xx",
            role: Some("security-auditor"),
            unattended: false,
            confined: true,
            switching_target: None,
            palette_key: Some(crate::keymap::Key::CTRL_L),
        };
        let row = rendered_row(80, ViewHeader::Session(&head));
        assert!(row.starts_with("   SESSION b3c4 [SECURITY-AUDITOR] ~/projects/xx"));
    }

    #[test]
    fn session_header_shows_switching_target_loading() {
        let head = SessionHead {
            session_id: "sess-01a2b3c4",
            workspace: "~/projects/xx",
            role: None,
            unattended: false,
            confined: true,
            switching_target: Some("7c405d7e"),
            palette_key: Some(crate::keymap::Key::CTRL_L),
        };
        let row = rendered_row(80, ViewHeader::Session(&head));
        assert!(
            row.contains("7c405d7e (loading…)"),
            "must show target loading in header tag: {row}"
        );
    }

    #[test]
    fn header_band_paints_the_full_row_width() {
        let theme = Theme::default();
        let head = SessionHead {
            session_id: "sess-01a2b3c4",
            workspace: "~/projects/xx",
            role: Some("developer"),
            unattended: true,
            confined: true,
            switching_target: None,
            palette_key: Some(crate::keymap::Key::CTRL_L),
        };
        let mut terminal = nuotc::TestTerminal::new(60, 1);
        terminal.draw(|frame| {
            draw_view_header(frame, frame.area(), &ViewHeader::Session(&head), &theme);
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
}
