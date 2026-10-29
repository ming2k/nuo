//! Sessions picker.
use chrono::{Local, TimeZone};
use nuotc::{
    Frame, Style, {Line, Span},
};
use unicode_width::UnicodeWidthStr;

use super::common::{one_line, relative_time_at, truncate_ellipsis};
use crate::shell::render::Theme;
use nuo_ui_primitives::components::options::{ChoiceStyle, ChoiceTone, choice_style};
use nuo_ui_primitives::primitives::{
    FixedModalSpec, FooterHint, FooterHintWithBand, SCROLL_EDGE_MARGIN, breadcrumb_parts,
    draw_scrollbar, keyvocab, modal_area, modal_frame, modal_header, modal_header_parts,
    render_centered_body, render_modal_footer, render_modal_footer_with_extra, resolve_scroll,
};

/// Format an epoch-seconds timestamp as a local absolute date-time
/// (`YYYY-MM-DD HH:MM`). Used by the session-info sub-view, where a precise
/// creation/last-active time is more useful than the picker's compact relative
/// form. Falls back to `--` for an out-of-range timestamp.
fn absolute_time(ts: u64) -> String {
    Local
        .timestamp_opt(ts as i64, 0)
        .single()
        .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "--".to_string())
}

/// Draw the sessions picker: each row shows the session overview plus its
/// last-active time. Enter opens the selected session; `i` drills into a detail
/// sub-view (full last prompt, creation time, message count). When
/// `keymap_open` is true the body is replaced by the full keybindings list.
/// `scroll` is read AND written back (clamped to the body height) so the modal
/// is scrollable with `PageUp` / `PageDown` / `Ctrl+↑/↓` and the mouse wheel;
/// `follow` keeps the selection on screen after `↑/↓` navigation (cleared on
/// manual scroll, mirroring the other list modals).
///
/// `startup_picker` is `true` only when the picker opened at startup
/// (`nuo attach` with no id). In that mode Esc/click-outside quits the
/// program (there is no thread behind the modal yet), so the footer
/// hint reads "quit" instead of "close".
///
/// `session_info_detail` switches the body to the detail sub-view for the
/// session under `session_detail` (requested on demand when the sub-view
/// opens); `session_info_scroll` is that sub-view's own scroll slot.
/// Properties for rendering the Sessions modal.
pub struct SessionsModalProps<'a> {
    pub sessions: &'a [nuo_wire::SessionOverview],
    pub expanded_sessions: Option<&'a std::collections::HashSet<String>>,
    pub selected: usize,
    pub scroll: &'a mut usize,
    pub follow: bool,
    pub startup_picker: bool,
    pub spinner_phase: usize,
    pub session_info_detail: bool,
    pub session_detail: Option<&'a nuo_wire::SessionDetail>,
    pub session_info_scroll: &'a mut usize,
    pub sessions_loading: bool,
    pub running_sessions: Option<&'a std::collections::HashSet<String>>,
}

/// One row of the picker's projected lineage forest.
///
/// A single shape, not a `Trunk`/`Branch` pair: the pair encoded the depth-1
/// assumption in the type system (there was no way to express a grandchild),
/// which is exactly how forks of forks became unrepresentable and then
/// invisible (ADR-0120). Depth and lane geometry are data here, so rendering is
/// a pure function of this struct.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionPickerItem<'a> {
    /// The thread this row shows.
    pub session: &'a nuo_wire::SessionOverview,
    /// Nesting depth: `0` for a root, `1` for a direct child, and so on.
    pub depth: usize,
    /// Whether this row is the last of its sibling set in the emitted order —
    /// the `└─` vs `├─` decision. Siblings are listed newest first (ADR-0128),
    /// so the row that closes the elbow is the **oldest** child, not the newest.
    pub is_last: bool,
    /// Per-ancestor continuation flags, outermost first. `true` means that
    /// ancestor still has a following sibling, so its lane column paints `│`;
    /// `false` means the lane is closed and paints spaces. This is the whole
    /// graph geometry: rendering never recomputes relationships.
    pub ancestry: Vec<bool>,
    /// How many direct children this row has (collapsed or expanded).
    pub child_count: usize,
    /// Whether this row's children are shown.
    pub expanded: bool,
    /// `true` when `parent_id` names a thread absent from this snapshot. The row
    /// is a branch whose origin the picker cannot draw — it must be marked as
    /// such, never silently promoted to a root conversation (`[INV-LINEAGE-02]`).
    pub parent_missing: bool,
}

impl<'a> SessionPickerItem<'a> {
    /// The thread this row projects.
    pub fn session(&self) -> &'a nuo_wire::SessionOverview {
        self.session
    }

    /// The lane prefix for this row: `""` at depth 0, otherwise a run of
    /// ancestor columns then the `├─ `/`└─ ` elbow.
    pub fn lane_prefix(&self) -> String {
        if self.depth == 0 {
            return String::new();
        }
        let mut out = String::with_capacity(self.depth * 3);
        for (i, continues) in self.ancestry.iter().enumerate() {
            // The innermost column is the elbow; every outer one is a lane.
            if i + 1 == self.ancestry.len() {
                out.push_str(if self.is_last { "└─ " } else { "├─ " });
            } else {
                out.push_str(if *continues { "│  " } else { "   " });
            }
        }
        out
    }

    /// The lineage marker that follows the lane: a fork address when the store
    /// recorded one, else the bare branch glyph.
    pub fn lineage_marker(&self) -> String {
        match self.session.fork_point {
            Some(point) => format!("⑂{point} "),
            None if self.parent_missing => "⑂ ".to_string(),
            None => "⑂ ".to_string(),
        }
    }
}

/// Project session overviews into the lineage forest (ADR-0120, ADR-0128).
///
/// Guarantees, in order of how badly a browser needs them:
///
/// 1. **Nothing is dropped.** Every session in `sessions` is emitted exactly
///    once — including the thread the user is attached to, and including a
///    branch whose parent is not in this snapshot (`[INV-LINEAGE-02]`).
/// 2. **Position is deterministic, newest first.** Roots and siblings are
///    ordered by `created_at` **descending** (id breaks ties), never by
///    `updated_at`: the thread a reader is most likely looking for is the one
///    that began most recently, while a liveness signal that changes on every
///    model token must never move a row under the cursor (`[INV-LINEAGE-03]`).
///    The descending direction is per sibling set, so a branch still paints
///    beneath the trunk it came from, however much newer it is.
/// 3. **Geometry is computed once.** `depth`, `is_last` and `ancestry` are
///    produced here; the renderer only paints them.
/// 4. **Corrupt lineage terminates.** A `parent_id` cycle is broken by visiting
///    each row at most once, so a bad database cannot hang the picker.
///
/// `expanded_set` names the rows whose children are shown. Collapsed rows still
/// report their `child_count` so the badge reads `[⑂ 3]` before expanding.
pub fn project_session_rows<'a>(
    sessions: &'a [nuo_wire::SessionOverview],
    expanded_set: Option<&std::collections::HashSet<String>>,
) -> Vec<SessionPickerItem<'a>> {
    use std::collections::{HashMap, HashSet};

    let by_id: HashMap<&str, &'a nuo_wire::SessionOverview> =
        sessions.iter().map(|s| (s.id.as_str(), s)).collect();

    // Children indexed by parent id. A row is a child only when its parent is
    // present; otherwise it is an *anchorless* root (handled below).
    let mut children: HashMap<&str, Vec<&'a nuo_wire::SessionOverview>> = HashMap::new();
    let mut roots: Vec<&'a nuo_wire::SessionOverview> = Vec::new();
    let mut parent_missing: HashSet<&str> = HashSet::new();

    for session in sessions {
        match session.parent_id.as_deref() {
            Some(parent) if by_id.contains_key(parent) && parent != session.id => {
                children.entry(parent).or_default().push(session);
            }
            // No parent at all: a genuine root.
            None => roots.push(session),
            // A parent that is not in the snapshot. The picker must still show
            // the row — it is a branch of *something*, and hiding it is how the
            // agent's own forks used to vanish (`[INV-LINEAGE-02]`).
            Some(_) => {
                parent_missing.insert(session.id.as_str());
                roots.push(session);
            }
        }
    }

    // Deterministic order: newest creation first, id as the tiebreak so equal
    // timestamps still form a total order. Descending on `created_at` only —
    // `updated_at` is a liveness signal and must not move a row (`[INV-LINEAGE-03]`).
    let order = |a: &&'a nuo_wire::SessionOverview, b: &&'a nuo_wire::SessionOverview| {
        (b.created_at, a.id.as_str()).cmp(&(a.created_at, b.id.as_str()))
    };
    roots.sort_by(order);
    for list in children.values_mut() {
        list.sort_by(order);
    }

    let is_expanded = |id: &str| expanded_set.map(|set| set.contains(id)).unwrap_or(false);

    let mut rows: Vec<SessionPickerItem<'a>> = Vec::new();
    let mut visited: HashSet<&str> = HashSet::new();

    // Iterative depth-first walk (an explicit stack, not recursion: a deep or
    // adversarially long chain must not be able to overflow the render stack).
    // Each frame carries the geometry its children inherit.
    struct Frame<'s> {
        session: &'s nuo_wire::SessionOverview,
        depth: usize,
        is_last: bool,
        ancestry: Vec<bool>,
    }

    let root_count = roots.len();
    let mut stack: Vec<Frame<'a>> = roots
        .iter()
        .enumerate()
        .rev()
        .map(|(index, session)| Frame {
            session,
            depth: 0,
            // A root has no elbow, but the flag is inherited by its subtree as
            // the outermost lane column, so it must be truthful.
            is_last: index + 1 == root_count,
            ancestry: Vec::new(),
        })
        .collect();

    while let Some(frame) = stack.pop() {
        if !visited.insert(frame.session.id.as_str()) {
            // A cycle (or a duplicate row) — emit once and stop descending.
            continue;
        }
        let kids = children
            .get(frame.session.id.as_str())
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let expanded = !kids.is_empty() && is_expanded(frame.session.id.as_str());

        rows.push(SessionPickerItem {
            session: frame.session,
            depth: frame.depth,
            is_last: frame.is_last,
            ancestry: frame.ancestry.clone(),
            child_count: kids.len(),
            expanded,
            parent_missing: parent_missing.contains(frame.session.id.as_str()),
        });

        if !expanded {
            continue;
        }

        // Push in reverse so the stack pops them in ascending order.
        let last_index = kids.len().saturating_sub(1);
        for (index, kid) in kids.iter().enumerate().rev() {
            let mut ancestry = frame.ancestry.clone();
            if frame.depth > 0 {
                // Our own continuation becomes an outer column for our children.
                ancestry.push(!frame.is_last);
            } else {
                // Depth-0 roots have no elbow, but their children still need a
                // column per ancestor level, so the root contributes one.
                ancestry.push(true);
            }
            stack.push(Frame {
                session: kid,
                depth: frame.depth + 1,
                is_last: index == last_index,
                ancestry,
            });
        }
    }

    // Unreachable-row pass. Two different situations produce an unvisited row,
    // and conflating them is what would re-introduce the original defect in
    // reverse (collapsed children would show as roots):
    //
    // - **Hidden by a collapsed ancestor.** The user closed a subtree. This row
    //   is intentionally not painted; it is reachable the moment its ancestor
    //   expands. Nothing to do here.
    // - **Stuck in a cycle.** Corrupt lineage with no root above it. It would
    //   never be painted at any expansion state, so it must be listed
    //   (`[INV-LINEAGE-02]`).
    //
    // Walking up from an unvisited row distinguishes them: reaching an emitted
    // row means the chain is sound (just folded); returning to the walk's own
    // starting point means a cycle.
    let stranded: Vec<&'a nuo_wire::SessionOverview> = sessions
        .iter()
        .filter(|session| !visited.contains(session.id.as_str()))
        .filter(|session| {
            let mut cursor = session.parent_id.as_deref();
            let mut steps = 0usize;
            while let Some(parent) = cursor {
                if visited.contains(parent) {
                    return false; // sound chain, merely folded
                }
                if parent == session.id.as_str() || steps > sessions.len() {
                    return true; // cycle (or a chain longer than the row set)
                }
                cursor = by_id.get(parent).and_then(|row| row.parent_id.as_deref());
                steps += 1;
            }
            true // no root reachable at all
        })
        .collect();

    // The stranded tail keeps the same total order as every other sibling set,
    // so a cycle's rows do not land in whatever sequence the wire happened to
    // serialize them in (`[INV-LINEAGE-03]`).
    let mut stranded = stranded;
    stranded.sort_by(order);

    for session in stranded {
        rows.push(SessionPickerItem {
            session,
            depth: 0,
            is_last: false,
            ancestry: Vec::new(),
            child_count: children.get(session.id.as_str()).map_or(0, Vec::len),
            expanded: false,
            parent_missing: true,
        });
    }

    rows
}

/// Draw the Sessions picker modal.
pub fn draw_sessions_modal(
    frame: &mut Frame,
    props: SessionsModalProps<'_>,
    theme: &Theme,
    selection: &nuo_view::model::selection::SelectionState,
    layout_map: &mut nuo_view::model::layout::LayoutMap,
) -> nuotc::Rect {
    let SessionsModalProps {
        sessions,
        expanded_sessions,
        selected,
        scroll,
        follow,
        startup_picker,
        spinner_phase,
        session_info_detail,
        session_detail,
        session_info_scroll,
        sessions_loading,
        running_sessions,
    } = props;
    let area = modal_area(frame, FixedModalSpec::SESSIONS);
    let f = modal_frame(frame, area, theme, true, true);

    // Destructive delete: custom band 70 so it outlives plain secondaries
    // (it is a one-key destructive action the user must be able to find).
    let close_label = if startup_picker { "quit" } else { "close" };
    let list_footer_hints: [FooterHint; 4] = [
        FooterHint::navigation(keyvocab::ARROWS_UD, "navigate"),
        FooterHint::primary(crate::shell::keymap::Key::ENTER.display(), "open"),
        FooterHint::secondary("Tab", "expand"),
        FooterHint::always(crate::shell::keymap::Key::ESC.display(), close_label),
    ];
    let list_extra: [FooterHintWithBand; 3] = [
        FooterHint::with_band("N", "new", 40),
        FooterHint::with_band("I", "info", 55),
        FooterHint::with_band("D", "delete", 70),
    ];

    // Detail sub-view (`i`): a focused read-out of the selected session. Its
    // own footer (`Esc` → back to list) and own scroll slot; Esc is handled by
    // the event loop's CloseModal arm, which backs out one sub-layer per press
    // (a sub-page back-out never leaves the Scene).
    // The header is a breadcrumb (`Sessions › Info`) — the modal hierarchy
    // convention: a sub-page keeps the same modal but shows where it sits.
    if session_info_detail {
        let header = breadcrumb_parts("Threads", "Info");
        modal_header_parts(frame, f.header, &header, theme);
        let detail_footer: [FooterHint; 1] = [FooterHint::always(
            crate::shell::keymap::Key::ESC.display(),
            "list",
        )];
        let body = match session_detail {
            None => {
                let spin = theme.glyphs.spinner_frame(spinner_phase);
                vec![
                    Line::from(""),
                    Line::from(vec![
                        Span::styled(format!("{spin} "), Style::default().fg(theme.primary)),
                        Span::styled("Loading thread detail…", Style::default().fg(theme.muted())),
                    ]),
                ]
            }
            Some(detail) => detail_body(detail, theme),
        };
        // Selectable document: the detail read-out (session id, title,
        // timestamps, last prompt) is exactly the text a user would want to
        // copy out of this sub-view.
        let rows: Vec<nuo_view::components::selectable_body::SelectableRow> = body
            .into_iter()
            .map(nuo_view::components::selectable_body::SelectableRow::from_line)
            .collect();
        nuo_view::components::selectable_body::render_selectable_body(
            frame,
            f.body,
            &rows,
            session_info_scroll,
            None,
            theme,
            selection,
            layout_map,
        );
        if let Some(fo) = f.footer {
            render_modal_footer(frame, fo, &detail_footer, theme);
        }
        return area;
    }

    modal_header(frame, f.header, "Threads", theme);

    let body_width = f.body.width as usize;

    let projected = project_session_rows(sessions, expanded_sessions);

    if projected.is_empty() {
        let body = if sessions_loading {
            let spin = theme.glyphs.spinner_frame(spinner_phase);
            vec![
                Line::from(""),
                Line::from(vec![
                    Span::styled(format!("{spin} "), Style::default().fg(theme.primary)),
                    Span::styled("Loading threads…", Style::default().fg(theme.muted())),
                ]),
            ]
        } else {
            vec![Line::from(vec![Span::styled(
                "No other threads found.",
                Style::default().fg(theme.muted()),
            )])]
        };
        render_centered_body(frame, f.body, body);
        if let Some(fo) = f.footer {
            render_modal_footer_with_extra(frame, fo, &list_footer_hints, &list_extra, theme);
        }
        return area;
    }

    // Windowed render: only build the rows that will actually be painted.
    let visible = f.body.height as usize;
    let follow_idx = if follow { Some(selected) } else { None };
    let (start, max_scroll) = resolve_scroll(
        scroll,
        visible,
        projected.len(),
        follow_idx,
        SCROLL_EDGE_MARGIN,
    );
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let end = (start + visible).min(projected.len());
    let mut body: Vec<Line> = Vec::with_capacity(end - start);
    for i in start..end {
        let Some(item) = projected.get(i) else {
            break;
        };
        let session = item.session();
        let is_selected = i == selected;
        let s: ChoiceStyle = choice_style(ChoiceTone::Filled, is_selected, theme);
        let meta = relative_time_at(session.updated_at, now);
        let meta_w = meta.width();
        const COL_GUTTER: usize = 2;

        // Geometry is data (ADR-0120): the lane prefix carries every ancestor
        // column plus this row's elbow, and is empty at depth 0.
        let lane = item.lane_prefix();
        let lineage = item.lineage_marker();
        let marker = if item.parent_missing {
            // The origin exists but is not in this snapshot; say so rather than
            // pretending the thread is a root conversation.
            let parent = session.parent_id.as_deref().unwrap_or("?");
            format!("{lineage}from {} ", short_id(parent))
        } else if item.depth > 0 || session.parent_id.is_some() {
            lineage
        } else {
            String::new()
        };

        let is_running = running_sessions.is_some_and(|r| r.contains(&session.id));
        let run_badge = if is_running {
            let spin = theme.glyphs.spinner_frame(spinner_phase);
            format!(" [{spin} RUNNING]")
        } else {
            String::new()
        };
        // The child badge only means something when the row has descendants.
        let tree_badge = if item.child_count > 0 {
            if item.expanded {
                format!(" [\u{25bc} \u{2442} {}]", item.child_count)
            } else {
                format!(" [\u{2442} {}]", item.child_count)
            }
        } else {
            String::new()
        };

        let prefix = format!("{lane}{marker}");
        let prefix_w = prefix.width();
        let badge = format!("{run_badge}{tree_badge}");
        let badge_w = badge.width();
        let is_current = session.active;
        let current_badge = if is_current { " [current]" } else { "" };
        let current_w = current_badge.width();
        let col1_budget =
            body_width.saturating_sub(meta_w + prefix_w + badge_w + current_w + COL_GUTTER);

        let display_title = session
            .title
            .as_deref()
            .filter(|t| !t.trim().is_empty())
            .unwrap_or(if !session.overview.trim().is_empty() {
                session.overview.as_str()
            } else {
                "(untitled)"
            });
        let overview = truncate_ellipsis(&one_line(display_title), col1_budget);
        let left_w = prefix_w + overview.width() + badge_w + current_w;
        let pad = body_width.saturating_sub(left_w + meta_w);

        // A branch reads as derived: its geometry and name are dimmed next to
        // the trunk's plain name, and everything brightens under selection.
        let lane_style = if is_selected {
            Style::default().bg(s.bg).fg(theme.primary)
        } else {
            Style::default().bg(s.bg).fg(theme.dim())
        };
        let text_style = if is_selected || item.depth == 0 {
            Style::default().bg(s.bg).fg(s.fg)
        } else {
            Style::default().bg(s.bg).fg(theme.muted())
        };
        let badge_style = if is_selected {
            Style::default().bg(s.bg).fg(theme.primary)
        } else if is_running {
            Style::default().bg(s.bg).fg(theme.warning)
        } else {
            Style::default().bg(s.bg).fg(theme.brand())
        };
        let current_style = if is_selected {
            Style::default().bg(s.bg).fg(theme.primary)
        } else {
            Style::default().bg(s.bg).fg(theme.ok())
        };

        let spans = vec![
            Span::styled(prefix, lane_style),
            Span::styled(overview, text_style),
            Span::styled(badge, badge_style),
            Span::styled(current_badge.to_string(), current_style),
            Span::styled(" ".repeat(pad), Style::default().bg(s.bg)),
            Span::styled(meta, Style::default().bg(s.bg).fg(s.dim)),
        ];
        body.push(Line::from(spans));
    }

    // The window is already the visible slice, so render it at scroll 0 and
    // draw the scrollbar against the true `max_scroll` of the full list.
    let mut para = nuotc::Paragraph::new(body);
    frame.render_widget(&mut para, f.body);
    draw_scrollbar(frame, f.body, start, max_scroll, theme);

    if let Some(fo) = f.footer {
        render_modal_footer_with_extra(frame, fo, &list_footer_hints, &list_extra, theme);
    }
    area
}

/// The first 8 characters of a session id — enough to recognize an identifier
/// the user may have seen elsewhere (a log line, a tab), short enough to sit in
/// a lane marker.
fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

/// Build the session-info sub-view body: a label/value read-out (id, title,
/// created/last-active timestamps, message count) followed by the full last
/// effective user prompt, wrapped to the modal width.
fn detail_body(detail: &nuo_wire::SessionDetail, theme: &Theme) -> Vec<Line<'static>> {
    let label = Style::default().fg(theme.dim());
    let value = Style::default().fg(theme.fg());
    let kv = |k: &str, v: String| {
        Line::from(vec![
            Span::styled(format!("{k}: "), label),
            Span::styled(v, value),
        ])
    };
    let mut lines: Vec<Line> = Vec::new();
    lines.push(kv("ID", detail.id.clone()));
    if let Some(title) = &detail.title {
        lines.push(kv("Title", title.clone()));
    }
    // The Chronicler's digest (ADR-0022 evolution): the resume-time
    // working-memory projection. Intent in one line; the history checklist
    // as terse bullets — enough to reorient without opening the transcript.
    if let Some(digest) = &detail.digest {
        lines.push(kv("Intent", one_line(&digest.intent)));
        if !digest.history.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled("History", label)));
            for entry in &digest.history {
                lines.push(Line::from(vec![
                    Span::styled("  • ", label),
                    Span::styled(one_line(entry), value),
                ]));
            }
        }
    }
    lines.push(kv("Created", absolute_time(detail.created_at)));
    lines.push(kv(
        "Last active",
        format!(
            "{} ({})",
            absolute_time(detail.updated_at),
            relative_time_at(
                detail.updated_at,
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0)
            )
        ),
    ));
    lines.push(kv("Messages", detail.message_count.to_string()));
    if detail.active {
        lines.push(Line::from(vec![
            Span::styled("State: ", label),
            Span::styled("active (this session)", value),
        ]));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled("Last prompt", label)));
    match &detail.last_prompt {
        Some(prompt) => {
            for raw in prompt.lines() {
                // Flatten any stray control chars so the row never spills.
                let flat: String = one_line(raw);
                lines.push(Line::from(Span::styled(flat, value)));
            }
            if prompt.trim().is_empty() {
                lines.push(Line::from(Span::styled(
                    "(empty)",
                    Style::default().fg(theme.muted()),
                )));
            }
        }
        None => lines.push(Line::from(Span::styled(
            "(no user prompt yet)",
            Style::default().fg(theme.muted()),
        ))),
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use nuotc::TestTerminal;

    #[test]
    fn sessions_modal_shows_loading_spinner_while_fetching() {
        let theme = Theme::default();
        let selection = nuo_view::model::selection::SelectionState::None;
        let mut layout_map = nuo_view::model::layout::LayoutMap::default();
        let mut scroll = 0;
        let mut info_scroll = 0;

        let mut term = TestTerminal::new(80, 24);
        term.draw(|frame| {
            draw_sessions_modal(
                frame,
                SessionsModalProps {
                    sessions: &[],
                    expanded_sessions: None,
                    selected: 0,
                    scroll: &mut scroll,
                    follow: false,
                    startup_picker: false,
                    spinner_phase: 0,
                    session_info_detail: false,
                    session_detail: None,
                    session_info_scroll: &mut info_scroll,
                    sessions_loading: true,
                    running_sessions: None,
                },
                &theme,
                &selection,
                &mut layout_map,
            );
        });

        let output: String = crate::shell::grid_rows(term.buffer()).join("\n");
        assert!(
            output.contains("Loading threads…"),
            "must show loading indicator while sessions_loading=true: {output}"
        );
        assert!(
            !output.contains("No other threads"),
            "must NOT mislead with no previous threads while loading: {output}"
        );
    }

    #[test]
    fn sessions_modal_shows_empty_message_only_after_loading_finishes() {
        let theme = Theme::default();
        let selection = nuo_view::model::selection::SelectionState::None;
        let mut layout_map = nuo_view::model::layout::LayoutMap::default();
        let mut scroll = 0;
        let mut info_scroll = 0;

        let mut term = TestTerminal::new(80, 24);
        term.draw(|frame| {
            draw_sessions_modal(
                frame,
                SessionsModalProps {
                    sessions: &[],
                    expanded_sessions: None,
                    selected: 0,
                    scroll: &mut scroll,
                    follow: false,
                    startup_picker: false,
                    spinner_phase: 0,
                    session_info_detail: false,
                    session_detail: None,
                    session_info_scroll: &mut info_scroll,
                    sessions_loading: false,
                    running_sessions: None,
                },
                &theme,
                &selection,
                &mut layout_map,
            );
        });

        let output: String = crate::shell::grid_rows(term.buffer()).join("\n");
        assert!(output.contains("No other threads found."));
        assert!(!output.contains("Loading threads…"));
    }

    /// A session row with the fields these tests care about spelled out, so a
    /// new wire field does not turn every assertion into a fixture edit.
    fn overview(id: &str, parent: Option<&str>, created_at: u64) -> nuo_wire::SessionOverview {
        nuo_wire::SessionOverview {
            id: id.into(),
            title: Some(id.into()),
            overview: id.into(),
            created_at,
            updated_at: created_at,
            message_count: 1,
            active: false,
            parent_id: parent.map(str::to_string),
            fork_kind: if parent.is_some() {
                nuo_wire::SessionForkKind::Fork
            } else {
                nuo_wire::SessionForkKind::Trunk
            },
            fork_point: parent.map(|_| nuo_wire::ForkPoint::at_round(1)),
            digest: None,
        }
    }

    fn expanded(ids: &[&str]) -> std::collections::HashSet<String> {
        ids.iter().map(|id| (*id).to_string()).collect()
    }

    fn ids(rows: &[SessionPickerItem<'_>]) -> Vec<String> {
        rows.iter().map(|row| row.session().id.clone()).collect()
    }

    /// `[INV-LINEAGE-02]`: nothing may be dropped. A branch of a branch is a
    /// first-class row; before ADR-0120 a depth-2 fork was collected as a child
    /// and then never emitted, so it was invisible and unselectable.
    #[test]
    fn deep_fork_chain_remains_visible() {
        let sessions = vec![
            overview("T", None, 10),
            overview("F1", Some("T"), 20),
            overview("F2", Some("F1"), 30),
            overview("F3", Some("F2"), 40),
        ];

        // Collapsed: only the roots are listed, but the child counts chain.
        let collapsed = project_session_rows(&sessions, None);
        assert_eq!(ids(&collapsed), vec!["T"]);
        assert_eq!(collapsed[0].child_count, 1);

        // Fully expanded: every link of the chain is present, in depth order.
        let all = project_session_rows(&sessions, Some(&expanded(&["T", "F1", "F2"])));
        assert_eq!(ids(&all), vec!["T", "F1", "F2", "F3"]);
        assert_eq!(all[1].depth, 1);
        assert_eq!(all[2].depth, 2);
        assert_eq!(all[3].depth, 3);
    }

    /// `[INV-LINEAGE-02]`: the agent's own branches must not be laundered into
    /// unrelated roots. The picker's snapshot drops the attached thread, so a
    /// fork of the thread you are sitting in arrives with a `parent_id` that is
    /// not in the set — that row must stay marked, not become a conversation.
    #[test]
    fn orphaned_branch_is_marked_not_promoted() {
        let sessions = vec![
            overview("F1", Some("ACTIVE"), 20),
            overview("OTHER", None, 10),
        ];

        let rows = project_session_rows(&sessions, None);
        assert_eq!(
            ids(&rows),
            vec!["F1", "OTHER"],
            "a branch is still listed, newest thread first"
        );
        let orphan = &rows[0];
        assert!(
            orphan.parent_missing,
            "the origin is absent, and that is stated"
        );
        assert_eq!(
            orphan.depth, 0,
            "an anchorless branch sits at the root lane"
        );
        assert!(
            orphan.lane_prefix().is_empty(),
            "there is no ancestor lane to paint"
        );
    }

    /// `[INV-LINEAGE-02]`: every session is emitted exactly once, whatever the
    /// shape of the lineage — including rows caught in a `parent_id` cycle,
    /// which must terminate instead of hanging the render loop.
    #[test]
    fn projection_emits_every_row_exactly_once() {
        let mut sessions = vec![
            overview("T", None, 10),
            overview("A", Some("T"), 20),
            overview("B", Some("T"), 30),
            overview("A1", Some("A"), 40),
        ];
        let all = project_session_rows(&sessions, Some(&expanded(&["T", "A", "B"])));
        assert_eq!(all.len(), sessions.len());
        let mut seen: Vec<String> = ids(&all);
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), sessions.len(), "no row is emitted twice");

        // A cycle has no root; the rows must still appear (as anchorless
        // branches) rather than vanish or loop forever.
        sessions.push(overview("X", Some("Y"), 50));
        sessions.push(overview("Y", Some("X"), 60));
        let cyc = project_session_rows(&sessions, Some(&expanded(&["T", "A", "B"])));
        assert_eq!(cyc.len(), sessions.len());
        assert!(ids(&cyc).iter().any(|id| id == "X"));
        assert!(ids(&cyc).iter().any(|id| id == "Y"));
    }

    /// The rendered modal, not just the projection: a depth-2 chain paints a
    /// nested lane and each row states its own fork address. This is the
    /// user-visible half of the depth-1 defect — before ADR-0120 the grandchild
    /// was not painted at any expansion state.
    #[test]
    fn sessions_modal_paints_a_two_level_fork_chain() {
        let theme = Theme::default();
        let selection = nuo_view::model::selection::SelectionState::None;
        let mut layout_map = nuo_view::model::layout::LayoutMap::default();
        let mut scroll = 0;
        let mut info_scroll = 0;

        let mut trunk = overview("trunk", None, 10);
        trunk.title = Some("Trunk Work".into());
        let mut child = overview("child", Some("trunk"), 20);
        child.title = Some("Child Branch".into());
        child.fork_point = Some(nuo_wire::ForkPoint::at_round(1));
        let mut grandchild = overview("grandchild", Some("child"), 30);
        grandchild.title = Some("Grand Branch".into());
        grandchild.fork_point = Some(nuo_wire::ForkPoint::at_turn(2, 1));

        let sessions = vec![trunk, child, grandchild];
        let expanded = std::collections::HashSet::from(["trunk".to_string(), "child".to_string()]);

        let mut term = TestTerminal::new(90, 28);
        term.draw(|frame| {
            draw_sessions_modal(
                frame,
                SessionsModalProps {
                    sessions: &sessions,
                    expanded_sessions: Some(&expanded),
                    selected: 0,
                    scroll: &mut scroll,
                    follow: false,
                    startup_picker: false,
                    spinner_phase: 0,
                    session_info_detail: false,
                    session_detail: None,
                    session_info_scroll: &mut info_scroll,
                    sessions_loading: false,
                    running_sessions: None,
                },
                &theme,
                &selection,
                &mut layout_map,
            );
        });

        let output: String = crate::shell::grid_rows(term.buffer()).join("\n");
        assert!(output.contains("Trunk Work"), "{output}");
        assert!(
            output.contains("└─ ⑂r1 Child Branch"),
            "the depth-1 branch paints its elbow and address: {output}"
        );
        assert!(
            output.contains("└─ ⑂r2.1 Grand Branch"),
            "the depth-2 branch paints a nested lane: {output}"
        );
    }

    /// `[INV-LINEAGE-03]`: order is a pure function of lineage and creation
    /// time. A liveness signal (`updated_at`) must never move a visible row —
    /// a list that reorders while a model streams cannot be navigated.
    #[test]
    fn projection_is_stable_across_updates() {
        let mut sessions = vec![
            overview("T", None, 10),
            overview("A", Some("T"), 20),
            overview("B", Some("T"), 30),
        ];
        let before = ids(&project_session_rows(&sessions, Some(&expanded(&["T"])))).to_vec();

        // A child finishes a long round: `updated_at` leaps past everything.
        sessions[2].updated_at = u64::MAX;
        let after = ids(&project_session_rows(&sessions, Some(&expanded(&["T"])))).to_vec();

        assert_eq!(before, after, "positions do not depend on liveness");

        // The same guarantee holds with the snapshot delivered in any order.
        sessions.reverse();
        assert_eq!(
            ids(&project_session_rows(&sessions, Some(&expanded(&["T"])))),
            before
        );
    }

    /// The lane geometry is data, so it can be asserted without a terminal:
    /// sibling elbows, a continuing parent lane, and a closing elbow. Siblings
    /// are listed newest first (ADR-0128), so the *last* row of a sibling set is
    /// the oldest one and it is that row which closes the elbow.
    #[test]
    fn lane_geometry_paints_continuations_and_closing_elbows() {
        let sessions = vec![
            overview("T", None, 10),
            overview("A", Some("T"), 20),
            overview("A1", Some("A"), 30),
            overview("A2", Some("A"), 40),
            overview("B", Some("T"), 50),
            overview("OTHER", None, 60),
        ];
        let rows = project_session_rows(&sessions, Some(&expanded(&["T", "A"])));
        let lane = |id: &str| {
            rows.iter()
                .find(|row| row.session().id == id)
                .map(SessionPickerItem::lane_prefix)
                .unwrap_or_else(|| panic!("row {id} missing"))
        };

        assert_eq!(lane("T"), "", "a root has no lane");
        assert_eq!(lane("B"), "├─ ", "B is the newest child, so T continues");
        assert_eq!(lane("A"), "└─ ", "the oldest child closes T's elbow");
        assert_eq!(
            lane("A2"),
            "│  ├─ ",
            "A2 is the newest grandchild, so A's lane stays open"
        );
        assert_eq!(
            lane("A1"),
            "│  └─ ",
            "the oldest grandchild closes the elbow"
        );
        assert_eq!(lane("OTHER"), "", "another root starts a new lane");
    }

    /// The browser leads with the most recently *created* thread, at every
    /// level of the forest: roots and each sibling set are newest first, while a
    /// newer branch still hangs beneath the trunk it came from. Ordering reads
    /// `created_at` only, so it never depends on snapshot arrival order
    /// (`[INV-LINEAGE-03]`, ADR-0128).
    #[test]
    fn siblings_are_ordered_newest_creation_first() {
        let sessions = vec![
            overview("old", None, 10),
            overview("new", None, 30),
            overview("mid", None, 20),
            // A branch created after every root stays nested under `old`.
            overview("old-kid", Some("old"), 40),
        ];

        let rows = project_session_rows(&sessions, Some(&expanded(&["old"])));
        assert_eq!(
            ids(&rows),
            vec!["new", "mid", "old", "old-kid"],
            "roots newest first; the branch keeps its trunk's position"
        );
        assert_eq!(rows[3].depth, 1, "a newer child does not jump the queue");
    }

    #[test]
    fn sessions_modal_renders_trunk_badges_and_expanded_branches() {
        use nuo_wire::{SessionForkKind, SessionOverview};
        let theme = Theme::default();
        let selection = nuo_view::model::selection::SelectionState::None;
        let mut layout_map = nuo_view::model::layout::LayoutMap::default();
        let mut scroll = 0;
        let mut info_scroll = 0;

        let s1 = SessionOverview {
            id: "s1".into(),
            title: Some("Auth Refactor".into()),
            overview: "Auth Refactor".into(),
            created_at: 100,
            updated_at: 100,
            message_count: 5,
            active: false,
            parent_id: None,
            fork_kind: SessionForkKind::Trunk,
            fork_point: None,
            digest: None,
        };
        let s2 = SessionOverview {
            id: "s2".into(),
            title: Some("Regex Branch".into()),
            overview: "Regex Branch".into(),
            created_at: 110,
            updated_at: 110,
            message_count: 2,
            active: false,
            parent_id: Some("s1".into()),
            fork_kind: SessionForkKind::Fork,
            // A real branch records where it diverged; the picker paints that
            // address rather than inventing a name for it (ADR-0120).
            fork_point: Some(nuo_wire::ForkPoint::at_round(1)),
            digest: None,
        };
        let sessions = vec![s1, s2];

        // 1. Collapsed test
        let mut term = TestTerminal::new(80, 24);
        term.draw(|frame| {
            draw_sessions_modal(
                frame,
                SessionsModalProps {
                    sessions: &sessions,
                    expanded_sessions: None,
                    selected: 0,
                    scroll: &mut scroll,
                    follow: false,
                    startup_picker: false,
                    spinner_phase: 0,
                    session_info_detail: false,
                    session_detail: None,
                    session_info_scroll: &mut info_scroll,
                    sessions_loading: false,
                    running_sessions: None,
                },
                &theme,
                &selection,
                &mut layout_map,
            );
        });

        let output_collapsed: String = crate::shell::grid_rows(term.buffer()).join("\n");
        assert!(output_collapsed.contains("Auth Refactor"));
        assert!(output_collapsed.contains("[⑂ 1]"));
        assert!(!output_collapsed.contains("Regex Branch"));

        // 2. Expanded test
        let mut expanded_set = std::collections::HashSet::new();
        expanded_set.insert("s1".to_string());
        let mut term_exp = TestTerminal::new(80, 24);
        term_exp.draw(|frame| {
            draw_sessions_modal(
                frame,
                SessionsModalProps {
                    sessions: &sessions,
                    expanded_sessions: Some(&expanded_set),
                    selected: 0,
                    scroll: &mut scroll,
                    follow: false,
                    startup_picker: false,
                    spinner_phase: 0,
                    session_info_detail: false,
                    session_detail: None,
                    session_info_scroll: &mut info_scroll,
                    sessions_loading: false,
                    running_sessions: None,
                },
                &theme,
                &selection,
                &mut layout_map,
            );
        });

        let output_expanded: String = crate::shell::grid_rows(term_exp.buffer()).join("\n");
        assert!(output_expanded.contains("Auth Refactor"));
        assert!(output_expanded.contains("[▼ ⑂ 1]"));
        // The branch row paints its lane elbow plus the persisted fork address
        // (ADR-0120) — not an invented title, and not a bare glyph.
        assert!(
            output_expanded.contains("└─ ⑂r1 Regex Branch"),
            "a branch states where it diverged: {output_expanded}"
        );
    }

    #[test]
    fn sessions_modal_renders_running_indicator_for_active_threads() {
        let theme = Theme::default();
        let selection = nuo_view::model::selection::SelectionState::None;
        let mut layout_map = nuo_view::model::layout::LayoutMap::default();
        let mut scroll = 0;
        let mut info_scroll = 0;

        let s1 = nuo_wire::SessionOverview {
            id: "s1".into(),
            title: Some("Running Task".into()),
            overview: "Running Task".into(),
            created_at: 100,
            updated_at: 100,
            message_count: 5,
            active: true,
            parent_id: None,
            fork_kind: nuo_wire::SessionForkKind::Trunk,
            fork_point: None,
            digest: None,
        };
        let sessions = vec![s1];
        let mut running_set = std::collections::HashSet::new();
        running_set.insert("s1".to_string());

        let mut term = TestTerminal::new(80, 24);
        term.draw(|frame| {
            draw_sessions_modal(
                frame,
                SessionsModalProps {
                    sessions: &sessions,
                    expanded_sessions: None,
                    selected: 0,
                    scroll: &mut scroll,
                    follow: false,
                    startup_picker: false,
                    spinner_phase: 0,
                    session_info_detail: false,
                    session_detail: None,
                    session_info_scroll: &mut info_scroll,
                    sessions_loading: false,
                    running_sessions: Some(&running_set),
                },
                &theme,
                &selection,
                &mut layout_map,
            );
        });

        let output: String = crate::shell::grid_rows(term.buffer()).join("\n");
        assert!(
            output.contains("RUNNING"),
            "must render RUNNING indicator for active running threads: {output}"
        );
    }
}
