//! The context-plan inspector's row construction (ADR-0220).
//!
//! These tests assert the properties a snapshot cannot: that the row order is
//! the request's order, that an inactive row explains itself, that a lossy
//! decision rides its own row, and that the attribution residual is rendered
//! rather than rounded away.

use super::*;
use nuo_protocol::{
    Elision, NoticeKind, PlannedBlockView, Position, PrefixDrift, Rank, RequestTokenEstimate,
    Resume, Topic,
};

fn theme() -> Theme {
    Theme::default()
}

fn block(id: &str, stability: Stability, tokens: usize) -> PlannedBlockView {
    PlannedBlockView {
        id: id.to_owned(),
        topic: Topic::ProjectAuthority,
        rank: Rank::Directive,
        position: Position::Body,
        stability,
        tokens,
        elision: None,
        active: true,
        inactive_reason: None,
        body: Some(format!("body of {id}")),
    }
}

fn plan(blocks: Vec<PlannedBlockView>) -> ContextPlanView {
    let fixed_tokens = blocks.iter().filter(|b| b.active).map(|b| b.tokens).sum();
    ContextPlanView {
        blocks,
        fixed_tokens,
        window_tokens: 200_000,
        instructions_ceiling_tokens: 16_000,
        ..ContextPlanView::default()
    }
}

/// The row's full painted text: decoration prefix plus segments.
///
/// The prefix carries every detail label (`DetailLayout` puts `"History"`,
/// `"Attribution"`, … there), so a test reading only `row.text()` would be blind
/// to the very field names a reader sees.
fn row_text(row: &SelectableRow) -> String {
    let prefix = row
        .prefix
        .as_ref()
        .map(|p| p.text.clone())
        .unwrap_or_default();
    format!("{prefix}{}", row.text())
}

fn text_of(body: &ContextPlanBody) -> String {
    body.rows
        .iter()
        .map(row_text)
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn the_empty_state_says_what_to_do_instead_of_showing_an_empty_table() {
    let body = build_context_plan_body(None, 0, false, &theme());
    let text = text_of(&body);
    assert!(text.contains("No request has been assembled"), "{text}");
    assert!(body.row_of_block.is_empty());
}

#[test]
fn the_rows_follow_the_plans_emission_order_and_name_every_block() {
    // `[INV-CTXPLAN-05]`: no client-side sort. A 12-token identity block stays
    // above a 900-token workspace block because that is the order the request
    // was assembled in.
    let plan = plan(vec![
        block("agent.identity", Stability::Route, 12),
        block("agent.precedence", Stability::Frozen, 900),
        block("project.rules", Stability::Workspace, 440),
    ]);
    let body = build_context_plan_body(Some(&plan), 0, false, &theme());
    let text = text_of(&body);
    let identity = text.find("agent.identity").expect("identity row");
    let precedence = text.find("agent.precedence").expect("precedence row");
    let rules = text.find("project.rules").expect("project.rules row");
    assert!(
        identity < precedence && precedence < rules,
        "the dialog re-ordered the request:\n{text}"
    );
}

#[test]
fn each_block_row_is_addressable_so_the_selection_can_be_followed() {
    let plan = plan(vec![
        block("agent.identity", Stability::Route, 12),
        block("agent.precedence", Stability::Frozen, 900),
    ]);
    let body = build_context_plan_body(Some(&plan), 0, false, &theme());
    assert_eq!(body.row_of_block.len(), 2);
    assert!(
        body.row_of_block[0] < body.row_of_block[1],
        "row_of_block must be ordered like the blocks"
    );
    assert!(body.row_of_block[1] < body.rows.len());
}

#[test]
fn a_band_header_appears_once_per_band() {
    let plan = plan(vec![
        block("agent.identity", Stability::Route, 12),
        block("agent.precedence", Stability::Frozen, 900),
        block("project.rules", Stability::Workspace, 440),
        block("workspace.topology", Stability::Workspace, 39),
    ]);
    let body = build_context_plan_body(Some(&plan), 0, false, &theme());
    let text = text_of(&body);
    assert_eq!(
        text.matches("Workspace 479 tokens").count(),
        1,
        "the workspace band opened more than one header:\n{text}"
    );
}

#[test]
fn an_inactive_row_explains_itself_instead_of_disappearing() {
    let mut inactive = block("security.web_untrusted", Stability::Route, 0);
    inactive.active = false;
    inactive.body = None;
    inactive.inactive_reason = Some("inactive for this request".to_string());
    let plan = plan(vec![
        block("agent.identity", Stability::Route, 12),
        inactive,
    ]);
    let body = build_context_plan_body(Some(&plan), 0, false, &theme());
    let text = text_of(&body);
    assert!(text.contains("security.web_untrusted"), "{text}");
    assert!(text.contains("inactive for this request"), "{text}");
}

#[test]
fn an_elided_row_names_the_loss_and_the_route_back() {
    let mut elided = block("project.rules", Stability::Workspace, 440);
    elided.elision = Some(Elision {
        full_tokens: 1_644,
        kept_tokens: 440,
        resume: Resume::File {
            path: "AGENTS.md".to_string(),
            from_line: 0,
        },
    });
    let plan = plan(vec![elided]);
    let body = build_context_plan_body(Some(&plan), 0, false, &theme());
    let text = text_of(&body);
    assert!(text.contains("elided 1204"), "{text}");
    assert!(text.contains("AGENTS.md"), "{text}");
}

#[test]
fn the_selected_blocks_body_is_expanded_in_place() {
    let plan = plan(vec![
        block("agent.identity", Stability::Route, 12),
        block("project.rules", Stability::Workspace, 440),
    ]);
    let closed = text_of(&build_context_plan_body(Some(&plan), 1, false, &theme()));
    assert!(!closed.contains("body of project.rules"), "{closed}");

    let open = text_of(&build_context_plan_body(Some(&plan), 1, true, &theme()));
    assert!(open.contains("body of project.rules"), "{open}");
    assert!(
        !open.contains("body of agent.identity"),
        "only the selected block expands:\n{open}"
    );
}

#[test]
fn the_attribution_residual_is_a_row_and_not_a_rounding() {
    let mut plan = plan(vec![block("agent.identity", Stability::Route, 12)]);
    plan.estimate = Some(RequestTokenEstimate::layered(41_000, 18, 9_412));
    let body = build_context_plan_body(Some(&plan), 0, false, &theme());
    let text = text_of(&body);
    assert!(text.contains("Attribution"), "{text}");
    assert!(
        text.contains("-6 tokens"),
        "the per-block sum (12) minus the measured region (18) must be shown:\n{text}"
    );
    assert!(text.contains("Tool schemas"), "{text}");
    assert!(
        text.contains("9412"),
        "tool schema cost must be visible:\n{text}"
    );
    assert!(text.contains("41000"), "{text}");
}

#[test]
fn an_unmeasured_plan_says_so_rather_than_showing_zero() {
    let plan = plan(vec![block("agent.identity", Stability::Route, 12)]);
    let text = text_of(&build_context_plan_body(Some(&plan), 0, false, &theme()));
    assert!(text.contains("not measured yet"), "{text}");
}

#[test]
fn a_head_drift_is_rendered_as_the_defect_it_is() {
    let mut plan = plan(vec![block("agent.precedence", Stability::Frozen, 152)]);
    plan.drift = PrefixDrift::Head {
        blocks: vec!["agent.precedence".to_string()],
    };
    let text = text_of(&build_context_plan_body(Some(&plan), 0, false, &theme()));
    assert!(text.contains("DEFECT"), "{text}");
}

#[test]
fn every_disclosed_loss_lands_in_the_notice_section() {
    let mut plan = plan(vec![block("project.rules", Stability::Workspace, 440)]);
    plan.notices = vec![nuo_protocol::ContextNotice {
        source_id: "project.rules".to_string(),
        kind: NoticeKind::Elided,
        message: "[project.rules: 1204 tokens elided; read `AGENTS.md` from line 0]".to_string(),
    }];
    let text = text_of(&build_context_plan_body(Some(&plan), 0, false, &theme()));
    assert!(text.contains("Loss disclosed"), "{text}");
    assert!(text.contains("1204 tokens elided"), "{text}");
}
