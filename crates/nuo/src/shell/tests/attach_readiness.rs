//! Attach readiness contract tests (ADR-0133).
//!
//! `[INV-ATTACH-04]`: the client gates input on the server-attested `Ready`
//! transition, queues pre-`Ready` prompts, and treats a post-Welcome failure as
//! a retained disclosure rather than a reason to exit.

use super::new_app_for_relay_tests;
use crate::shell::event_loop::AppMutation as M;

fn apply_phase(app: &mut crate::shell::app::App, phase: nuo_wire::SessionPhase) {
    let runtime = crate::shell::event_loop::UiRuntime::minimal_for_test();
    let _ = crate::shell::event_loop::apply::apply(app, &runtime, M::SessionPhase(phase));
}

#[test]
fn client_queues_until_ready_then_flushes() {
    let mut app = new_app_for_relay_tests();
    app.session_phase = nuo_wire::SessionPhase::Preparing {
        stage: nuo_wire::PrepareStage::Skills,
    };
    app.pending_prompts.push(nuo_wire::AgentRequest::Prompt {
        text: "hello".to_string(),
        images: Vec::new(),
        sent_at_ms: None,
    });
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    app.tx = tx;

    apply_phase(&mut app, nuo_wire::SessionPhase::Ready);

    assert_eq!(app.session_phase, nuo_wire::SessionPhase::Ready);
    assert!(app.pending_prompts.is_empty());
    assert!(matches!(
        rx.try_recv(),
        Ok(nuo_wire::AgentRequest::Prompt { text, .. }) if text == "hello"
    ));
}

#[test]
fn post_welcome_failure_is_disclosed_and_never_quits() {
    let mut app = new_app_for_relay_tests();
    app.switching_session = Some("abc".to_string());

    apply_phase(
        &mut app,
        nuo_wire::SessionPhase::Failed {
            code: Some(nuo_wire::ERR_SESSION_UNRESUMABLE.to_string()),
            message: "could not load".to_string(),
        },
    );

    assert!(app.switching_session.is_none());
    assert!(matches!(
        app.session_phase,
        nuo_wire::SessionPhase::Failed { .. }
    ));
    assert!(
        !app.should_quit.load(std::sync::atomic::Ordering::SeqCst),
        "a post-Welcome assembly failure must not terminate the client"
    );
    assert!(
        app.notice_log()
            .lines()
            .iter()
            .any(|notice| notice.text.contains("could not load")),
        "the failure must be retained in the notice log"
    );
}
