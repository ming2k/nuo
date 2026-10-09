//! Sessions picker.

use chrono::{Local, TimeZone};
use nuotc::{
    Frame, Style, {Line, Span},
};
use unicode_width::UnicodeWidthStr;

use super::common::{one_line, relative_time_at, truncate_ellipsis};
use crate::components::options::{ChoiceStyle, ChoiceTone, choice_style};
use crate::primitives::{
    FixedModalSpec, FooterHint, FooterHintWithBand, SCROLL_EDGE_MARGIN, breadcrumb_parts,
    draw_scrollbar, keyvocab, modal_area, modal_frame, modal_header, modal_header_parts,
    render_centered_body, render_modal_footer, render_modal_footer_with_extra, resolve_scroll,
};
use crate::render::Theme;

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
}

/// A projected row item in the sessions picker modal (ADR-0251).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionPickerItem<'a> {
    Trunk {
        session: &'a nuo_wire::SessionOverview,
        child_count: usize,
        expanded: bool,
    },
    Branch {
        session: &'a nuo_wire::SessionOverview,
        is_last: bool,
    },
}

impl<'a> SessionPickerItem<'a> {
    pub fn session(&self) -> &'a nuo_wire::SessionOverview {
        match self {
            Self::Trunk { session, .. } => session,
            Self::Branch { session, .. } => session,
        }
    }
}

/// Project session overviews into a hierarchical trunk-first list with expandable timeline branches (ADR-0251).
pub fn project_session_rows<'a>(
    sessions: &'a [nuo_wire::SessionOverview],
    expanded_set: Option<&std::collections::HashSet<String>>,
) -> Vec<SessionPickerItem<'a>> {
    use nuo_wire::SessionForkKind;

    let mut trunks: Vec<&'a nuo_wire::SessionOverview> = Vec::new();
    let mut children_by_parent: std::collections::HashMap<
        &str,
        Vec<&'a nuo_wire::SessionOverview>,
    > = std::collections::HashMap::new();

    let all_ids: std::collections::HashSet<&str> = sessions.iter().map(|s| s.id.as_str()).collect();

    for s in sessions {
        let is_branch = s.fork_kind == SessionForkKind::Aside
            || (s.parent_id.is_some() && s.fork_kind != SessionForkKind::Trunk);
        if is_branch
            && let Some(ref pid) = s.parent_id
            && all_ids.contains(pid.as_str())
        {
            children_by_parent.entry(pid.as_str()).or_default().push(s);
            continue;
        }
        trunks.push(s);
    }

    let mut rows = Vec::new();
    for trunk in trunks {
        let children = children_by_parent.get(trunk.id.as_str());
        let child_count = children.as_ref().map(|c| c.len()).unwrap_or(0);
        let is_expanded = child_count > 0
            && expanded_set
                .map(|set| set.contains(&trunk.id))
                .unwrap_or(false);

        rows.push(SessionPickerItem::Trunk {
            session: trunk,
            child_count,
            expanded: is_expanded,
        });

        if is_expanded && let Some(child_list) = children {
            for (idx, child) in child_list.iter().enumerate() {
                let is_last = idx + 1 == child_list.len();
                rows.push(SessionPickerItem::Branch {
                    session: child,
                    is_last,
                });
            }
        }
    }

    rows
}

/// Draw the Sessions picker modal.
pub fn draw_sessions_modal(
    frame: &mut Frame,
    props: SessionsModalProps<'_>,
    theme: &Theme,
    selection: &crate::model::selection::SelectionState,
    layout_map: &mut crate::model::layout::LayoutMap,
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
    } = props;
    let area = modal_area(frame, FixedModalSpec::SESSIONS);
    let f = modal_frame(frame, area, theme, true, true);

    // Destructive delete: custom band 70 so it outlives plain secondaries
    // (it is a one-key destructive action the user must be able to find).
    let close_label = if startup_picker { "quit" } else { "close" };
    let list_footer_hints: [FooterHint; 4] = [
        FooterHint::navigation(keyvocab::ARROWS_UD, "navigate"),
        FooterHint::key_primary(crate::keymap::Key::ENTER, "open"),
        FooterHint::secondary("Tab", "expand"),
        FooterHint::key_always(crate::keymap::Key::ESC, close_label),
    ];
    let list_extra: [FooterHintWithBand; 3] = [
        FooterHint::with_band("N", "new", 40),
        FooterHint::with_band("I", "info", 55),
        FooterHint::with_band("D", "delete", 70),
    ];

    // Detail sub-view (`i`): a focused read-out of the selected session. Its
    // own footer (`Esc` → back to list) and own scroll slot; Esc is handled by
    // the event loop's CloseModal arm, which backs out one sub-layer per press
    // (a sub-page back-out never leaves the Scene, ADR-0298).
    // The header is a breadcrumb (`Sessions › Info`) — the modal hierarchy
    // convention: a sub-page keeps the same modal but shows where it sits.
    if session_info_detail {
        let header = breadcrumb_parts("Threads", "Info");
        modal_header_parts(frame, f.header, &header, theme);
        let detail_footer: [FooterHint; 1] =
            [FooterHint::key_always(crate::keymap::Key::ESC, "list")];
        let body = match session_detail {
            None => {
                let spin = theme.glyphs.spinner_frame(spinner_phase);
                vec![
                    Line::from(""),
                    Line::from(vec![
                        Span::styled(format!("{spin} "), Style::default().fg(theme.primary)),
                        Span::styled(
                            "Loading thread detail…",
                            Style::default().fg(theme.muted()),
                        ),
                    ]),
                ]
            }
            Some(detail) => detail_body(detail, theme),
        };
        // Selectable document: the detail read-out (session id, title,
        // timestamps, last prompt) is exactly the text a user would want to
        // copy out of this sub-view.
        let rows: Vec<crate::components::selectable_body::SelectableRow> = body
            .into_iter()
            .map(crate::components::selectable_body::SelectableRow::from_line)
            .collect();
        crate::components::selectable_body::render_selectable_body(
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

        match item {
            SessionPickerItem::Trunk {
                child_count,
                expanded,
                ..
            } => {
                let badge = if *child_count > 0 {
                    if *expanded {
                        format!(" [▼ ⑂ {}]", child_count)
                    } else {
                        format!(" [⑂ {}]", child_count)
                    }
                } else {
                    String::new()
                };
                let badge_w = badge.width();
                let col1_budget = body_width.saturating_sub(meta_w + badge_w + COL_GUTTER);
                let overview = truncate_ellipsis(&one_line(&session.overview), col1_budget);
                let left_w = overview.width() + badge_w;
                let pad = body_width.saturating_sub(left_w + meta_w);
                let badge_style = if is_selected {
                    Style::default().bg(s.bg).fg(theme.primary)
                } else {
                    Style::default().bg(s.bg).fg(theme.brand())
                };
                let spans = vec![
                    Span::styled(overview, Style::default().bg(s.bg).fg(s.fg)),
                    Span::styled(badge, badge_style),
                    Span::styled(" ".repeat(pad), Style::default().bg(s.bg)),
                    Span::styled(meta, Style::default().bg(s.bg).fg(s.dim)),
                ];
                body.push(Line::from(spans));
            }
            SessionPickerItem::Branch { is_last, .. } => {
                let prefix = if *is_last {
                    "  └─ ⑂ "
                } else {
                    "  ├─ ⑂ "
                };
                let prefix_w = prefix.width();
                let col1_budget = body_width.saturating_sub(meta_w + prefix_w + COL_GUTTER);
                let overview = truncate_ellipsis(&one_line(&session.overview), col1_budget);
                let left_w = prefix_w + overview.width();
                let pad = body_width.saturating_sub(left_w + meta_w);
                let branch_prefix_style = if is_selected {
                    Style::default().bg(s.bg).fg(theme.primary)
                } else {
                    Style::default().bg(s.bg).fg(theme.dim())
                };
                let branch_text_style = if is_selected {
                    Style::default().bg(s.bg).fg(s.fg)
                } else {
                    Style::default().bg(s.bg).fg(theme.muted())
                };
                let spans = vec![
                    Span::styled(prefix, branch_prefix_style),
                    Span::styled(overview, branch_text_style),
                    Span::styled(" ".repeat(pad), Style::default().bg(s.bg)),
                    Span::styled(meta, Style::default().bg(s.bg).fg(s.dim)),
                ];
                body.push(Line::from(spans));
            }
        }
    }

    // The window is already the visible slice, so render it at scroll 0 and
    // draw the scrollbar against the true `max_scroll` of the full list.
    let para = nuotc::Paragraph::new(body);
    frame.render_widget(para, f.body);
    draw_scrollbar(frame, f.body, start, max_scroll, theme);

    if let Some(fo) = f.footer {
        render_modal_footer_with_extra(frame, fo, &list_footer_hints, &list_extra, theme);
    }
    area
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
        let selection = crate::model::selection::SelectionState::None;
        let mut layout_map = crate::model::layout::LayoutMap::default();
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
                },
                &theme,
                &selection,
                &mut layout_map,
            );
        });

        let output: String = term
            .buffer()
            .rows()
            .iter()
            .map(|r| r.iter().map(|c| c.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
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
        let selection = crate::model::selection::SelectionState::None;
        let mut layout_map = crate::model::layout::LayoutMap::default();
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
                },
                &theme,
                &selection,
                &mut layout_map,
            );
        });

        let output: String = term
            .buffer()
            .rows()
            .iter()
            .map(|r| r.iter().map(|c| c.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(output.contains("No other threads found."));
        assert!(!output.contains("Loading threads…"));
    }

    #[test]
    fn project_session_rows_groups_by_trunk_and_asides() {
        use nuo_wire::{SessionForkKind, SessionOverview};
        let s1 = SessionOverview {
            id: "s1".into(),
            overview: "Trunk 1".into(),
            created_at: 100,
            updated_at: 100,
            message_count: 5,
            active: false,
            parent_id: None,
            fork_kind: SessionForkKind::Trunk,
            digest: None,
        };
        let s2 = SessionOverview {
            id: "s2".into(),
            overview: "Aside 1".into(),
            created_at: 110,
            updated_at: 110,
            message_count: 2,
            active: false,
            parent_id: Some("s1".into()),
            fork_kind: SessionForkKind::Aside,
            digest: None,
        };
        let s3 = SessionOverview {
            id: "s3".into(),
            overview: "Trunk 2".into(),
            created_at: 120,
            updated_at: 120,
            message_count: 10,
            active: false,
            parent_id: None,
            fork_kind: SessionForkKind::Trunk,
            digest: None,
        };
        let sessions = vec![s1, s2, s3];

        // 1. Collapsed state
        let collapsed = project_session_rows(&sessions, None);
        assert_eq!(collapsed.len(), 2, "only 2 trunk rows when collapsed");
        match &collapsed[0] {
            SessionPickerItem::Trunk {
                session,
                child_count,
                expanded,
            } => {
                assert_eq!(session.id, "s1");
                assert_eq!(*child_count, 1);
                assert!(!*expanded);
            }
            _ => panic!("expected trunk"),
        }

        // 2. Expanded state
        let mut expanded_set = std::collections::HashSet::new();
        expanded_set.insert("s1".to_string());
        let expanded = project_session_rows(&sessions, Some(&expanded_set));
        assert_eq!(expanded.len(), 3, "2 trunks + 1 expanded branch");
        match &expanded[0] {
            SessionPickerItem::Trunk {
                session,
                child_count,
                expanded,
            } => {
                assert_eq!(session.id, "s1");
                assert_eq!(*child_count, 1);
                assert!(*expanded);
            }
            _ => panic!("expected trunk"),
        }
        match &expanded[1] {
            SessionPickerItem::Branch { session, is_last } => {
                assert_eq!(session.id, "s2");
                assert!(*is_last);
            }
            _ => panic!("expected branch"),
        }
    }

    #[test]
    fn sessions_modal_renders_trunk_badges_and_expanded_branches() {
        use nuo_wire::{SessionForkKind, SessionOverview};
        let theme = Theme::default();
        let selection = crate::model::selection::SelectionState::None;
        let mut layout_map = crate::model::layout::LayoutMap::default();
        let mut scroll = 0;
        let mut info_scroll = 0;

        let s1 = SessionOverview {
            id: "s1".into(),
            overview: "Auth Refactor".into(),
            created_at: 100,
            updated_at: 100,
            message_count: 5,
            active: false,
            parent_id: None,
            fork_kind: SessionForkKind::Trunk,
            digest: None,
        };
        let s2 = SessionOverview {
            id: "s2".into(),
            overview: "Regex Aside".into(),
            created_at: 110,
            updated_at: 110,
            message_count: 2,
            active: false,
            parent_id: Some("s1".into()),
            fork_kind: SessionForkKind::Aside,
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
                },
                &theme,
                &selection,
                &mut layout_map,
            );
        });

        let output_collapsed: String = term
            .buffer()
            .rows()
            .iter()
            .map(|r| r.iter().map(|c| c.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(output_collapsed.contains("Auth Refactor"));
        assert!(output_collapsed.contains("[⑂ 1]"));
        assert!(!output_collapsed.contains("Regex Aside"));

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
                },
                &theme,
                &selection,
                &mut layout_map,
            );
        });

        let output_expanded: String = term_exp
            .buffer()
            .rows()
            .iter()
            .map(|r| r.iter().map(|c| c.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(output_expanded.contains("Auth Refactor"));
        assert!(output_expanded.contains("[▼ ⑂ 1]"));
        assert!(output_expanded.contains("└─ ⑂ Regex Aside"));
    }
}
