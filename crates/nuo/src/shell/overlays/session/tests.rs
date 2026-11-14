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
fn overview(id: &str, parent: Option<&str>, created_at: u64) -> nuo_protocol::ThreadOverview {
    nuo_protocol::ThreadOverview {
        id: id.into(),
        title: Some(id.into()),
        overview: id.into(),
        created_at,
        updated_at: created_at,
        message_count: 1,
        active: false,
        parent_id: parent.map(str::to_string),
        origin: if parent.is_some() {
            nuo_protocol::ThreadOrigin::Trunk
        } else {
            nuo_protocol::ThreadOrigin::Trunk
        },
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
    let mut grandchild = overview("grandchild", Some("child"), 30);
    grandchild.title = Some("Grand Branch".into());

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
        output.contains("└─ ⑂ Child Branch"),
        "the depth-1 branch paints its elbow and address: {output}"
    );
    assert!(
        output.contains("└─ ⑂ Grand Branch"),
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
    use nuo_protocol::{ThreadOrigin, ThreadOverview};
    let theme = Theme::default();
    let selection = nuo_view::model::selection::SelectionState::None;
    let mut layout_map = nuo_view::model::layout::LayoutMap::default();
    let mut scroll = 0;
    let mut info_scroll = 0;

    let s1 = ThreadOverview {
        id: "s1".into(),
        title: Some("Auth Refactor".into()),
        overview: "Auth Refactor".into(),
        created_at: 100,
        updated_at: 100,
        message_count: 5,
        active: false,
        parent_id: None,
        origin: ThreadOrigin::Trunk,
        digest: None,
    };
    let s2 = ThreadOverview {
        id: "s2".into(),
        title: Some("Regex Branch".into()),
        overview: "Regex Branch".into(),
        created_at: 110,
        updated_at: 110,
        message_count: 2,
        active: false,
        parent_id: Some("s1".into()),
        origin: ThreadOrigin::Trunk,
        // A real branch records where it diverged; the picker paints that
        // address rather than inventing a name for it (ADR-0120).
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
        output_expanded.contains("└─ ⑂ Regex Branch"),
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

    let s1 = nuo_protocol::ThreadOverview {
        id: "s1".into(),
        title: Some("Running Task".into()),
        overview: "Running Task".into(),
        created_at: 100,
        updated_at: 100,
        message_count: 5,
        active: true,
        parent_id: None,
        origin: nuo_protocol::ThreadOrigin::Trunk,
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
