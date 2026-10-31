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
use crate::shell::event_loop::UiRuntime;
use nuo_protocol::{TrustDomain, WorkspaceSecuritySnapshot, WorkspaceTrustState};

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
    crate::shell::event_loop::apply::apply(
        app,
        &crate::shell::event_loop::UiRuntime::minimal_for_test(),
        crate::shell::event_loop::AppMutation::PreAttach(crate::shell::PreAttachSignal {
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
        gate.apply(crate::shell::question_model::QuestionAction::Submit)
    };
    assert_eq!(
        decision,
        Some(crate::shell::PreAttachDecision::Trust {
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
    crate::shell::event_loop::apply::apply(
        &mut app,
        &runtime,
        crate::shell::event_loop::AppMutation::PreAttachAttestationError(
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
        gate.apply(crate::shell::question_model::QuestionAction::Submit)
    };
    assert_eq!(
        retry,
        Some(crate::shell::PreAttachDecision::Trust {
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

    crate::shell::event_loop::apply::apply(
        &mut app,
        &runtime,
        crate::shell::event_loop::AppMutation::PreAttachAttestationError(
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
        let _ = gate.apply(crate::shell::question_model::QuestionAction::Submit);
    }
    crate::shell::event_loop::apply::apply(
        &mut app,
        &runtime,
        crate::shell::event_loop::AppMutation::PreAttachAttestationError(
            "Workspace trust failed: cannot open sqlite db".to_string(),
        ),
    );
    {
        let gate = app.pre_attach.as_mut().unwrap();
        let _ = gate.apply(crate::shell::question_model::QuestionAction::Submit);
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
    crate::shell::event_loop::apply::apply(
        &mut app,
        &runtime,
        crate::shell::event_loop::AppMutation::Harness(nuo_protocol::HarnessSnapshot {
            loop_status: nuo_protocol::LoopStatus::Idle,
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

/// A refused workspace resolves to `Denied`, which is a decision — the gate
/// must unmount and never re-open on the same content.
///
/// The failure mode this pins is the nagging loop the refusal exists to stop:
/// if `Denied` still counted as "needs review", the user would refuse, watch the
/// gate vanish, and get the identical question on the next snapshot republish.
#[test]
fn a_denied_snapshot_retires_the_gate_and_never_re_opens() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let runtime = UiRuntime::minimal_for_test();

    mount_gate(&mut app);
    assert!(app.pre_attach.is_some());

    // The server recorded the refusal: every offered domain is now `Denied`.
    let denied = WorkspaceSecuritySnapshot {
        root: "/tmp/proj".to_string(),
        mcp: WorkspaceTrustState::Denied,
        skills: WorkspaceTrustState::Denied,
        hooks: WorkspaceTrustState::Absent,
        instructions: WorkspaceTrustState::Denied,
        ex_workspace: WorkspaceTrustState::Absent,
        user_assets: WorkspaceTrustState::Absent,
    };
    let snapshot = |security: WorkspaceSecuritySnapshot| {
        crate::shell::event_loop::AppMutation::Harness(nuo_protocol::HarnessSnapshot {
            loop_status: nuo_protocol::LoopStatus::Idle,
            round_counter: 0,
            retry_pending: false,
            unattended: false,
            confined: false,
            role: None,
            workspace: Some("/tmp/proj".to_string()),
            workspace_security: security,
        })
    };
    crate::shell::event_loop::apply::apply(&mut app, &runtime, snapshot(denied.clone()));
    assert!(
        app.pre_attach.is_none(),
        "a recorded refusal unmounts the gate"
    );

    // A later periodic republish of the same denied snapshot must not resurrect
    // it — neither via the latch nor via the mount arm itself.
    crate::shell::event_loop::apply::apply(&mut app, &runtime, snapshot(denied.clone()));
    assert!(
        app.pre_attach.is_none(),
        "a denied workspace must not be re-gated on republish"
    );
    crate::shell::event_loop::apply::apply(
        &mut app,
        &runtime,
        crate::shell::event_loop::AppMutation::PreAttach(crate::shell::PreAttachSignal {
            snapshot: denied,
        }),
    );
    assert!(
        app.pre_attach.is_none(),
        "the mount arm itself must treat Denied as settled"
    );
}

/// Content that changes *again* after a refusal is a new question, and the gate
/// must ask it — `Denied` covers the bytes that were refused, not the domain
/// forever.
#[test]
fn changed_content_after_a_refusal_is_asked_again() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let runtime = UiRuntime::minimal_for_test();

    // A fresh app: no latch, nothing dismissed.
    crate::shell::event_loop::apply::apply(
        &mut app,
        &runtime,
        crate::shell::event_loop::AppMutation::PreAttach(crate::shell::PreAttachSignal {
            snapshot: WorkspaceSecuritySnapshot {
                root: "/tmp/proj".to_string(),
                mcp: WorkspaceTrustState::Denied,
                skills: WorkspaceTrustState::Absent,
                hooks: WorkspaceTrustState::Absent,
                instructions: WorkspaceTrustState::Changed,
                ex_workspace: WorkspaceTrustState::Absent,
                user_assets: WorkspaceTrustState::Absent,
            },
        }),
    );
    let gate = app
        .pre_attach
        .as_ref()
        .expect("changed content after a refusal must re-open the gate");
    let options = &gate.model().request().questions[0].options;
    assert_eq!(
        options.len(),
        1,
        "only the domain that changed is offered, not the one already refused"
    );
    assert_eq!(options[0].label, "AGENTS.md  ·  [Changed]");
}
