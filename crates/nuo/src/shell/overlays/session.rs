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
    pub sessions: &'a [nuo_protocol::ThreadOverview],
    pub expanded_sessions: Option<&'a std::collections::HashSet<String>>,
    pub selected: usize,
    pub scroll: &'a mut usize,
    pub follow: bool,
    pub startup_picker: bool,
    pub spinner_phase: usize,
    pub session_info_detail: bool,
    pub session_detail: Option<&'a nuo_protocol::ThreadDetail>,
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
    pub session: &'a nuo_protocol::ThreadOverview,
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
    pub fn session(&self) -> &'a nuo_protocol::ThreadOverview {
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

    /// The lineage marker that follows the lane of a child thread.
    pub fn lineage_marker(&self) -> String {
        if self.session.parent_id.is_some() {
            "⑂ ".to_string()
        } else {
            String::new()
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
    sessions: &'a [nuo_protocol::ThreadOverview],
    expanded_set: Option<&std::collections::HashSet<String>>,
) -> Vec<SessionPickerItem<'a>> {
    use std::collections::{HashMap, HashSet};

    let by_id: HashMap<&str, &'a nuo_protocol::ThreadOverview> =
        sessions.iter().map(|s| (s.id.as_str(), s)).collect();

    // Children indexed by parent id. A row is a child only when its parent is
    // present; otherwise it is an *anchorless* root (handled below).
    let mut children: HashMap<&str, Vec<&'a nuo_protocol::ThreadOverview>> = HashMap::new();
    let mut roots: Vec<&'a nuo_protocol::ThreadOverview> = Vec::new();
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
    let order = |a: &&'a nuo_protocol::ThreadOverview, b: &&'a nuo_protocol::ThreadOverview| {
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
        session: &'s nuo_protocol::ThreadOverview,
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
    let stranded: Vec<&'a nuo_protocol::ThreadOverview> = sessions
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
fn detail_body(detail: &nuo_protocol::ThreadDetail, theme: &Theme) -> Vec<Line<'static>> {
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
mod tests;
