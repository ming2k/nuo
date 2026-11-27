//! The context-plan inspector's **paint**: what actually reaches the screen.
//!
//! The row-construction tests (`overlays::context_plan::tests`) assert the
//! document's contents; these assert the rendered modal — that the accounting
//! block, the band headers, the block rows, and the drift verdict are all on
//! screen together, and that the expanded body pushes the tail down rather than
//! painting over it.
//!
//! Locked as a snapshot because the inspector's whole value is that an operator
//! can read the prompt's shape at a glance: a layout that wrapped a row, clipped
//! the drift, or hid an inactive block is a correctness failure, not cosmetics.

use super::helpers::painted_rows;
use crate::shell::render::Theme;
use nuo_protocol::{
    ContextNotice, ContextPlanView, Elision, NoticeKind, PlannedBlockView, Position, PrefixDrift,
    RequestTokenEstimate, Resume, Stability, Topic,
};
use nuo_view::model::layout::LayoutMap;
use nuo_view::model::selection::SelectionState;

fn block(
    id: &str,
    topic: Topic,
    stability: Stability,
    tokens: usize,
    body: &str,
) -> PlannedBlockView {
    PlannedBlockView {
        id: id.to_owned(),
        topic,
        rank: topic.rank(),
        position: Position::Body,
        stability,
        tokens,
        elision: None,
        active: true,
        inactive_reason: None,
        body: Some(body.to_owned()),
    }
}

/// A plan with one of each thing the inspector must render: a measured request,
/// an adaptive block, an elided workspace block, an inactive source, a disclosed
/// loss, and a moved suffix.
fn fixture() -> ContextPlanView {
    let mut rules = block(
        "project.rules",
        Topic::ProjectAuthority,
        Stability::Workspace,
        440,
        "# Project Rules\n\nFollow the rules.",
    );
    rules.elision = Some(Elision {
        full_tokens: 1_644,
        kept_tokens: 440,
        resume: Resume::File {
            path: "AGENTS.md".to_owned(),
            from_line: 0,
        },
    });
    let mut untrusted = block(
        "security.web_untrusted",
        Topic::UntrustedPolicy,
        Stability::Route,
        0,
        "",
    );
    untrusted.active = false;
    untrusted.body = None;
    untrusted.inactive_reason = Some("inactive for this request".to_owned());

    ContextPlanView {
        blocks: vec![
            block(
                "agent.identity",
                Topic::Identity,
                Stability::Route,
                18,
                "You are Nuo.",
            ),
            block(
                "agent.precedence",
                Topic::Precedence,
                Stability::Frozen,
                152,
                "Follow this order.",
            ),
            block(
                "workspace.roots",
                Topic::WorkspaceBoundary,
                Stability::Session,
                158,
                "The workspace is /data/projects/nuo.",
            ),
            rules,
            untrusted,
        ],
        fixed_tokens: 768,
        notices: vec![ContextNotice {
            source_id: "project.rules".to_owned(),
            kind: NoticeKind::Elided,
            message: "[project.rules: 1204 tokens elided; read `AGENTS.md` from line 0]".to_owned(),
        }],
        drift: PrefixDrift::Suffix {
            blocks: vec!["project.rules".to_owned()],
        },
        window_tokens: 200_000,
        instructions_ceiling_tokens: 16_000,
        estimate: Some(RequestTokenEstimate::layered(41_207, 768, 9_412)),
    }
}

fn render(plan: Option<&ContextPlanView>, selected: usize, body_open: bool, height: u16) -> String {
    let (width, _) = (100u16, height);
    let mut terminal = graticule::TestTerminal::new(width, height);
    terminal.draw(|f| {
        let mut layout_map = LayoutMap::default();
        let mut scroll = 0usize;
        let mut block_rows = Vec::new();
        let selection = SelectionState::default();
        let _ = crate::shell::overlays::draw_context_plan_modal(
            f,
            plan,
            false,
            selected,
            body_open,
            &mut scroll,
            &mut block_rows,
            &Theme::default(),
            &selection,
            &mut layout_map,
        );
    });
    painted_rows(terminal.buffer(), width, height).0
}

#[test]
fn the_inspector_paints_the_accounting_the_blocks_and_the_drift() {
    let plan = fixture();
    insta::assert_snapshot!(render(Some(&plan), 0, false, 48));
}

#[test]
fn an_expanded_body_pushes_the_tail_down_instead_of_painting_over_it() {
    // The body is a disclosure *inside* the document: the notice and drift rows
    // must still be reachable, which means they move down, not out.
    let plan = fixture();
    let closed = render(Some(&plan), 3, false, 60);
    let open = render(Some(&plan), 3, true, 60);
    assert!(
        closed.contains("Cacheable prefix"),
        "the closed inspector must show the drift section:\n{closed}"
    );
    assert!(
        open.contains("# Project Rules"),
        "the expanded body must be on screen:\n{open}"
    );
    let closed_drift = closed.find("Cacheable prefix").expect("closed drift row");
    let open_drift = open.find("Cacheable prefix").expect("open drift row");
    assert!(
        open_drift > closed_drift,
        "the expanded body must push the tail down, not replace it:\n{open}"
    );
    assert!(open.contains("Loss disclosed"), "{open}");
}

#[test]
fn the_loading_placeholder_is_not_an_empty_plan() {
    // A query in flight and a thread with no assembled request look identical on
    // screen only if the dialog lies about one of them (ADR-0220 §3).
    let grid = render(None, 0, false, 16);
    assert!(
        grid.contains("Querying") || grid.contains("No request"),
        "a `None` plan must say what it is:\n{grid}"
    );
}
