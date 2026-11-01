//! Attach readiness contract tests (ADR-0133).
//!
//! `[INV-ATTACH-04]`: the client gates input on the server-attested `Ready`
//! transition, queues pre-`Ready` prompts, and treats a post-Welcome failure as
//! a retained disclosure rather than a reason to exit.

use super::new_app_for_relay_tests;
use crate::shell::event_loop::AppMutation as M;

fn apply_phase(app: &mut crate::shell::app::App, phase: nuo_protocol::ThreadPhase) {
    let runtime = crate::shell::event_loop::UiRuntime::minimal_for_test();
    let _ = crate::shell::event_loop::apply::apply(app, &runtime, M::SessionPhase(phase));
}

#[test]
fn client_queues_until_ready_then_flushes() {
    let mut app = new_app_for_relay_tests();
    app.session_phase = nuo_protocol::ThreadPhase::Preparing {
        stage: nuo_protocol::PrepareStage::Skills,
    };
    app.pending_prompts
        .push(nuo_protocol::AgentRequest::Prompt {
            text: "hello".to_string(),
            images: Vec::new(),
            sent_at_ms: None,
        });
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    app.tx = tx;

    apply_phase(&mut app, nuo_protocol::ThreadPhase::Ready);

    assert_eq!(app.session_phase, nuo_protocol::ThreadPhase::Ready);
    assert!(app.pending_prompts.is_empty());
    assert!(matches!(
        rx.try_recv(),
        Ok(nuo_protocol::AgentRequest::Prompt { text, .. }) if text == "hello"
    ));
}

#[test]
fn post_welcome_failure_is_disclosed_and_never_quits() {
    let mut app = new_app_for_relay_tests();
    app.switching_session = Some("abc".to_string());

    apply_phase(
        &mut app,
        nuo_protocol::ThreadPhase::Failed {
            code: Some(nuo_protocol::ERR_SESSION_UNRESUMABLE.to_string()),
            message: "could not load".to_string(),
        },
    );

    assert!(app.switching_session.is_none());
    assert!(matches!(
        app.session_phase,
        nuo_protocol::ThreadPhase::Failed { .. }
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

/// `[INV-OUTBOX-06]` — settling on a thread re-reads its queue.
///
/// The driver pushes `QueueUpdated` after every queue *change*, but that is an
/// event, not a fact that survives a disconnect or an attach to an already-live
/// session. Without the read the projection starts empty while the driver holds
/// items, so the user is shown "nothing pending" and then receives prompts they
/// never saw coming. The read rides the same `Ready` transition that flushes
/// pre-`Ready` prompts.
#[test]
fn ready_re_reads_the_authoritative_queue() {
    let mut app = new_app_for_relay_tests();
    app.current_session_id = "s1".to_string();
    app.session_phase = nuo_protocol::ThreadPhase::Preparing {
        stage: nuo_protocol::PrepareStage::Skills,
    };
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    app.tx = tx;

    apply_phase(&mut app, nuo_protocol::ThreadPhase::Ready);

    let sent: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
    assert!(
        sent.iter().any(|request| matches!(
            request,
            nuo_protocol::AgentRequest::QueueSnapshot { thread_id } if thread_id == "s1"
        )),
        "arriving on a thread must ask the driver what it already holds: {sent:?}"
    );
}
