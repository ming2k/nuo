use super::*;
use protocol::{TrustDomain, WorkspaceSecuritySnapshot, WorkspaceTrustState};

fn quarantined_snapshot() -> WorkspaceSecuritySnapshot {
    WorkspaceSecuritySnapshot {
        root: "/tmp/proj".to_string(),
        mcp: WorkspaceTrustState::Quarantined,
        skills: WorkspaceTrustState::Quarantined,
        hooks: WorkspaceTrustState::Absent,
        instructions: WorkspaceTrustState::Quarantined,
        ex_workspace: WorkspaceTrustState::Absent,
        user_assets: WorkspaceTrustState::Absent,
    }
}

#[test]
fn from_snapshot_returns_none_when_nothing_needs_review() {
    let trusted = WorkspaceSecuritySnapshot {
        root: "/tmp/proj".to_string(),
        mcp: WorkspaceTrustState::Trusted,
        skills: WorkspaceTrustState::Trusted,
        hooks: WorkspaceTrustState::Trusted,
        instructions: WorkspaceTrustState::Trusted,
        ex_workspace: WorkspaceTrustState::Trusted,
        user_assets: WorkspaceTrustState::Trusted,
    };
    assert!(PreAttachState::from_snapshot(&trusted).is_none());

    // A refusal is a decision: `Denied` is settled, so the gate must not
    // re-open and nag.
    let denied = WorkspaceSecuritySnapshot {
        root: "/tmp/proj".to_string(),
        mcp: WorkspaceTrustState::Denied,
        skills: WorkspaceTrustState::Trusted,
        hooks: WorkspaceTrustState::Absent,
        instructions: WorkspaceTrustState::Denied,
        ex_workspace: WorkspaceTrustState::Trusted,
        user_assets: WorkspaceTrustState::Absent,
    };
    assert!(PreAttachState::from_snapshot(&denied).is_none());
}

#[test]
fn from_snapshot_mounts_for_changed_content() {
    // The regression this pins. `Changed` used to escalate through an
    // in-session banner instead of gating, so the one state meaning "the
    // content you approved is not the content on disk" got a scroll-past
    // toast that re-fired on every attach.
    let changed = WorkspaceSecuritySnapshot {
        root: "/tmp/proj".to_string(),
        mcp: WorkspaceTrustState::Trusted,
        skills: WorkspaceTrustState::Trusted,
        hooks: WorkspaceTrustState::Trusted,
        instructions: WorkspaceTrustState::Changed,
        ex_workspace: WorkspaceTrustState::Trusted,
        user_assets: WorkspaceTrustState::Trusted,
    };
    let state = PreAttachState::from_snapshot(&changed).expect("changed instructions must mount");
    let question = state
        .model()
        .request()
        .questions
        .first()
        .expect("the gate asks one question");
    assert_eq!(
        question.options.len(),
        1,
        "only the changed domain is listed"
    );
    assert_eq!(
        question.options.first().expect("one option").label,
        "Project instructions",
        "the option label is the asset name, with no status syntax"
    );
    assert_eq!(state.rows().len(), 1);
    assert_eq!(
        state.rows().first().expect("one row").status,
        view::trust_gate::TrustStatus::Changed
    );
    assert!(
        question.question.contains("changed on disk"),
        "the prompt must say why it is asking: {}",
        question.question
    );
}

#[test]
fn every_offered_row_lines_up_with_its_option() {
    // The pairing the renderer and `apply` both rely on. Two independent
    // enumerations of the same snapshot would drift the moment one gained a
    // filter, and the drift would paint one asset's status beside another
    // asset's name — a mislabelled security decision.
    let snapshot = PreAttachState::acceptance_fixture();
    let question = snapshot
        .model()
        .request()
        .questions
        .first()
        .expect("the gate asks one question");
    assert_eq!(snapshot.rows().len(), question.options.len());
    for (row, option) in snapshot.rows().iter().zip(&question.options) {
        assert_eq!(row.label, option.label);
        assert_eq!(
            Some(row.domain),
            trust_gate::answer_to_domains(
                std::slice::from_ref(&vec![option.label.clone()]),
                &snapshot.offered_domains()
            )
            .first()
            .copied(),
            "an option's label must resolve back to the row's own domain"
        );
    }
}

#[test]
fn from_snapshot_mounts_for_quarantined() {
    let state = PreAttachState::from_snapshot(&quarantined_snapshot())
        .expect("quarantined snapshot must mount PreAttach");
    assert!(!state.acceptance());
    assert_eq!(
        state.model().request().id,
        trust_gate::TRUST_GATE_REQUEST_ID
    );
    // First option highlighted by default (single-select is live).
    assert_eq!(state.model().highlight(), 0);
}

#[test]
fn acceptance_fixture_lists_every_domain() {
    let state = PreAttachState::acceptance_fixture();
    assert!(state.acceptance());
    let q = state
        .model()
        .request()
        .questions
        .first()
        .expect("trust gate has one question");
    assert!(q.multi_select);
    let labels: Vec<&str> = q.options.iter().map(|o| o.label.as_str()).collect();
    assert!(labels.contains(&".nuo/mcp.json"));
    assert!(labels.contains(&".nuo/skills/"));
    assert!(labels.contains(&".nuo/hooks/"));
    assert!(labels.contains(&"Project instructions"));
    assert!(labels.contains(&".nuo/config.toml"));
    assert!(labels.contains(&"~/.config/nuo/config.toml"));
}

#[test]
fn the_workspace_path_appears_only_when_the_decision_crosses_the_project_boundary() {
    // A project-scoped decision happens in the directory the user just
    // opened, so the path is zero information there. It becomes
    // load-bearing exactly when a user-global asset is on the list.
    let project_only = PreAttachState::from_snapshot(&quarantined_snapshot())
        .expect("quarantined snapshot must mount");
    assert!(
        !project_only.shows_workspace(),
        "a project-only decision does not need to name the project"
    );

    let mut mixed = quarantined_snapshot();
    mixed.user_assets = WorkspaceTrustState::Quarantined;
    let with_global = PreAttachState::from_snapshot(&mixed).expect("user assets must mount");
    assert!(
        with_global.shows_workspace(),
        "a global asset must be distinguishable from the project's own"
    );
}

#[test]
fn navigate_then_submit_trust_all_emits_a_grant() {
    let mut state = PreAttachState::from_snapshot(&quarantined_snapshot())
        .expect("quarantined snapshot must mount");
    // By default, all options start selected. Pressing enter trusts all of them.
    let decision = state.apply(QuestionAction::Submit);
    assert_eq!(
        decision,
        Some(PreAttachDecision::Trust {
            domains: vec![
                TrustDomain::Mcp,
                TrustDomain::Skills,
                TrustDomain::Instructions,
            ],
        })
    );
    assert!(state.submitting());
    // While the decision is in flight the surface is inert — including for
    // Esc. A cancel here used to refuse every offered domain (the ones just
    // submitted included), so the outcome raced the in-flight grant.
    assert_eq!(state.apply(QuestionAction::Down), None);
    assert_eq!(state.apply(QuestionAction::Cancel), None);
    assert_eq!(state.apply(QuestionAction::Submit), None);
}

#[test]
fn space_toggles_and_submits_partial_selection() {
    let mut state = PreAttachState::from_snapshot(&quarantined_snapshot())
        .expect("quarantined snapshot must mount");
    // Option 0: .nuo/mcp.json, Option 1: .nuo/skills/, Option 2: AGENTS.md
    state.apply(QuestionAction::Down); // highlight -> 1 (.nuo/skills/)
    state.apply(QuestionAction::Toggle); // toggle off .nuo/skills/
    let decision = state.apply(QuestionAction::Submit);
    assert_eq!(
        decision,
        Some(PreAttachDecision::Trust {
            domains: vec![TrustDomain::Mcp, TrustDomain::Instructions,],
        })
    );
}

#[test]
fn esc_declines_without_quitting() {
    // A refusal is a decision, not an exit. The gate also opens for
    // `Changed` content on a live session, where terminating the terminal
    // would be an absurd answer to "someone changed AGENTS.md".
    let mut state = PreAttachState::from_snapshot(&quarantined_snapshot())
        .expect("quarantined snapshot must mount");
    let decision = state.apply(QuestionAction::Cancel);
    assert_eq!(
        decision,
        Some(PreAttachDecision::Decline {
            domains: vec![
                TrustDomain::Mcp,
                TrustDomain::Skills,
                TrustDomain::Instructions,
            ],
        }),
        "declining must name every offered domain so the refusal can be recorded"
    );
}

#[test]
fn a_partial_selection_declines_exactly_what_was_left_out() {
    // The domains to refuse are the ones *offered but unselected* — not
    // merely the ones still pending after a partial grant, which is how the
    // server-side pre-attach path had to infer it.
    let mut state = PreAttachState::from_snapshot(&quarantined_snapshot())
        .expect("quarantined snapshot must mount");
    state.apply(QuestionAction::Down); // highlight -> 1 (.nuo/skills/)
    state.apply(QuestionAction::Toggle); // toggle off .nuo/skills/
    let decision = state.apply(QuestionAction::Submit);
    assert_eq!(
        decision,
        Some(PreAttachDecision::Trust {
            domains: vec![TrustDomain::Mcp, TrustDomain::Instructions],
        }),
    );
}

#[test]
fn submitting_nothing_is_a_refusal_not_an_empty_grant() {
    // Dispatching a grant for zero domains would ask the server to do
    // nothing and leave the gate to re-open — the loop, with extra steps.
    let mut state = PreAttachState::from_snapshot(&quarantined_snapshot())
        .expect("quarantined snapshot must mount");
    for _ in 0..3 {
        state.apply(QuestionAction::Toggle); // clear all three selections
        state.apply(QuestionAction::Down);
    }
    let decision = state.apply(QuestionAction::Submit);
    assert_eq!(
        decision,
        Some(PreAttachDecision::Decline {
            domains: vec![
                TrustDomain::Mcp,
                TrustDomain::Skills,
                TrustDomain::Instructions,
            ],
        }),
    );
}

/// A failed attestation round-trip: the failure message is carried on the
/// surface, `submitting` clears so the decision resumes, and the next submit
/// re-dispatches the complete selection.
#[test]
fn attestation_failure_resumes_the_decision() {
    let mut state = PreAttachState::from_snapshot(&quarantined_snapshot())
        .expect("quarantined snapshot must mount");
    assert_eq!(state.attestation_error(), None);
    let _ = state.apply(QuestionAction::Submit);
    assert!(state.submitting());

    state.note_attestation_error("cannot open sqlite db".to_string());
    assert!(!state.submitting());
    assert_eq!(state.attestation_error(), Some("cannot open sqlite db"));

    let retry = state.apply(QuestionAction::Submit);
    assert!(matches!(retry, Some(PreAttachDecision::Trust { .. })));
    assert!(state.submitting());
    assert_eq!(state.attestation_error(), Some("cannot open sqlite db"));
}

/// One asset is a binary decision, not a selection.
///
/// The defect this pins: a one-row gate was declared multi-select, so
/// `Space` was a live toggle for a row the footer never advertised. Pressing
/// it deselected the only asset, and the next `Enter` dispatched a *refusal*
/// of the thing the user meant to grant — a security inversion produced by a
/// key the surface did not offer.
#[test]
fn a_single_asset_gate_cannot_be_deselected_by_a_key_it_does_not_advertise() {
    let mut state = PreAttachState::from_snapshot(&WorkspaceSecuritySnapshot {
        root: "/tmp/proj".to_string(),
        mcp: WorkspaceTrustState::Absent,
        skills: WorkspaceTrustState::Absent,
        hooks: WorkspaceTrustState::Absent,
        instructions: WorkspaceTrustState::Changed,
        ex_workspace: WorkspaceTrustState::Absent,
        user_assets: WorkspaceTrustState::Absent,
    })
    .expect("changed instructions must gate");
    assert!(
        !state.model().active_multi_select(),
        "one asset is a binary decision, not a multi-select"
    );
    state.apply(QuestionAction::Toggle);
    let decision = state.apply(QuestionAction::Submit);
    assert_eq!(
        decision,
        Some(PreAttachDecision::Trust {
            domains: vec![TrustDomain::Instructions],
        }),
        "a stray Space must not turn the grant into a refusal"
    );
}

#[test]
fn arrow_only_does_not_emit_decision() {
    let mut state = PreAttachState::from_snapshot(&quarantined_snapshot())
        .expect("quarantined snapshot must mount");
    assert_eq!(state.apply(QuestionAction::Down), None);
    assert_eq!(state.apply(QuestionAction::Up), None);
    assert_eq!(state.model().highlight(), 0);
}

/// `TrustDomain` exists and is enumerable — sanity check the contract hasn't
/// drifted out from under this module.
#[test]
fn trust_domain_all_is_still_enumerable() {
    assert!(!TrustDomain::ALL.is_empty());
}

#[test]
fn the_panel_claims_its_measure_and_yields_only_the_frame_margin() {
    // A percentage of the viewport is the wrong model: it narrowed the panel
    // on a narrow terminal (where the prompt most needs its columns) and
    // widened it on a wide one (where the row's two fields drift apart).
    let wide = Rect::new(0, 0, 200, 30);
    assert_eq!(
        panel_width(wide),
        PANEL_MAX_WIDTH,
        "a wide terminal keeps the measure and spends the rest on margin"
    );
    let comfortable = Rect::new(0, 0, 80 + PANEL_H_MARGIN, 30);
    assert_eq!(panel_width(comfortable), 80);
    let narrow = Rect::new(0, 0, 50, 30);
    assert_eq!(
        panel_width(narrow),
        50 - PANEL_H_MARGIN,
        "a narrow terminal gets everything but the margin"
    );
}

#[test]
fn the_panel_is_exactly_as_tall_as_what_it_paints() {
    // The defect this pins: a fixed `clamp(12, 28)` slab pinned a short
    // prompt to the top of the screen. The panel now sizes to its content and
    // is centered, so a short decision sits in the middle of the viewport.
    let area = Rect::new(0, 0, 80, 30);
    let panel = centered_panel(area, 56, 9);
    assert_eq!(panel.height, 9);
    assert_eq!(panel.y, area.y + (30 - 9) / 2);
}

#[test]
fn the_panel_never_exceeds_the_terminal() {
    let area = Rect::new(0, 0, 80, 30);
    let panel = centered_panel(area, u16::MAX, u16::MAX);
    assert!(panel.width <= area.width);
    assert!(panel.height <= area.height);
}
