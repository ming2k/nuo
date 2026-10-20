//! The PreAttach trust gate's decision lifecycle.
//!
//! The reported failure mode this pins: the user confirms with Enter, the
//! grant is dispatched, and the gate silently loops — because the server
//! could not persist the trust and the failure only ever landed in the
//! logs. The invariant now: a failed attestation round-trip is *rendered on
//! the gate itself*, the surface resumes the decision (submitting clears),
//! and a retry re-dispatches the full selection. They drive the applier
//! with the exact mutations the translator emits, so a future regression
//! fails here rather than on a user's screen.

// Integration test target: assertions fail by panicking, so the production
// no-panic lints are meaningless here (the exemption ends at the test boundary).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;
use crate::event_loop::UiRuntime;
use nuo_wire::{TrustDomain, WorkspaceSecuritySnapshot, WorkspaceTrustState};

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

fn mount_gate(app: &mut App) {
    crate::event_loop::apply::apply(
        app,
        &crate::event_loop::UiRuntime::minimal_for_test(),
        crate::event_loop::AppMutation::PreAttach(crate::PreAttachSignal {
            snapshot: quarantined_snapshot(),
        }),
    );
}

/// Enter confirms, the server reports the grant could not be persisted, and
/// the gate renders the failure and resumes the decision instead of looping
/// silently; a retry re-dispatches the full selection.
#[test]
fn a_failed_attestation_resumes_the_decision() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let runtime = UiRuntime::minimal_for_test();

    mount_gate(&mut app);

    // Enter: everything is pre-selected, so the decision trusts all three
    // present domains and the surface enters its submitting state.
    let decision = {
        let gate = app.pre_attach.as_mut().unwrap();
        gate.apply(crate::question_model::QuestionAction::Submit)
    };
    assert_eq!(
        decision,
        Some(crate::PreAttachDecision::Trust {
            domains: vec![
                TrustDomain::Mcp,
                TrustDomain::Skills,
                TrustDomain::Instructions,
            ],
        })
    );
    assert!(app.pre_attach.as_ref().unwrap().submitting());

    // The server's outcome for the failed grant (`AgentNotice::trust_changed`
    // on the persist-failure path): the surface must show it and unblock.
    crate::event_loop::apply::apply(
        &mut app,
        &runtime,
        crate::event_loop::AppMutation::PreAttachAttestationError(
            "Workspace trust failed: cannot open sqlite db".to_string(),
        ),
    );
    let gate = app
        .pre_attach
        .as_ref()
        .expect("gate stays mounted on failure");
    assert!(
        !gate.submitting(),
        "the failed round-trip unblocks the surface"
    );
    assert_eq!(
        gate.attestation_error(),
        Some("Workspace trust failed: cannot open sqlite db"),
    );

    // The user retries with the same (pre-selected) selection: the decision
    // is dispatched again, complete and uncorrupted.
    let retry = {
        let gate = app.pre_attach.as_mut().unwrap();
        gate.apply(crate::question_model::QuestionAction::Submit)
    };
    assert_eq!(
        retry,
        Some(crate::PreAttachDecision::Trust {
            domains: vec![
                TrustDomain::Mcp,
                TrustDomain::Skills,
                TrustDomain::Instructions,
            ],
        })
    );
}

/// With no gate mounted, a trust notice is ordinary session flow: the
/// mutation must not create a gate or touch anything else.
#[test]
fn trust_notices_without_a_mounted_gate_are_ignored() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let runtime = UiRuntime::minimal_for_test();

    crate::event_loop::apply::apply(
        &mut app,
        &runtime,
        crate::event_loop::AppMutation::PreAttachAttestationError(
            "Workspace trust failed: whatever".to_string(),
        ),
    );
    assert!(app.pre_attach.is_none(), "no gate is conjured by a notice");
}

/// The trusted-snapshot unmount still resolves the gate: a *successful*
/// grant clears the interstitial on the republished snapshot, error state
/// and all.
#[test]
fn the_trusted_snapshot_still_resolves_the_gate() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let runtime = UiRuntime::minimal_for_test();

    mount_gate(&mut app);
    // Submit, fail, then submit again — the retry is now in flight.
    {
        let gate = app.pre_attach.as_mut().unwrap();
        let _ = gate.apply(crate::question_model::QuestionAction::Submit);
    }
    crate::event_loop::apply::apply(
        &mut app,
        &runtime,
        crate::event_loop::AppMutation::PreAttachAttestationError(
            "Workspace trust failed: cannot open sqlite db".to_string(),
        ),
    );
    {
        let gate = app.pre_attach.as_mut().unwrap();
        let _ = gate.apply(crate::question_model::QuestionAction::Submit);
    }
    assert!(app.pre_attach.as_ref().unwrap().submitting());

    // The server republishes a fully-trusted snapshot (the Harness mutation
    // the attach sync delivers).
    let trusted = WorkspaceSecuritySnapshot {
        root: "/tmp/proj".to_string(),
        mcp: WorkspaceTrustState::Trusted,
        skills: WorkspaceTrustState::Trusted,
        hooks: WorkspaceTrustState::Absent,
        instructions: WorkspaceTrustState::Trusted,
        ex_workspace: WorkspaceTrustState::Absent,
        user_assets: WorkspaceTrustState::Absent,
    };
    crate::event_loop::apply::apply(
        &mut app,
        &runtime,
        crate::event_loop::AppMutation::Harness(nuo_wire::HarnessSnapshot {
            loop_status: nuo_wire::LoopStatus::Idle,
            round_counter: 0,
            retry_pending: false,
            unattended: false,
            confined: false,
            role: None,
            workspace: Some("/tmp/proj".to_string()),
            workspace_security: trusted,
        }),
    );
    assert!(app.pre_attach.is_none(), "the resolved gate unmounts");
}
