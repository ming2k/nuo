//! Attach readiness contract tests (ADR-0133).
//!
//! `[INV-ATTACH-04]`: the client gates input on the server-attested `Ready`
//! transition, queues pre-`Ready` prompts, and treats a post-Welcome failure as
//! a retained disclosure rather than a reason to exit.

use super::new_app_for_relay_tests;
use crate::shell::event_loop::AppMutation as M;

fn apply_phase(app: &mut crate::shell::app::App, phase: protocol::ThreadPhase) {
    let runtime = crate::shell::event_loop::UiRuntime::minimal_for_test();
    let _ = crate::shell::event_loop::apply::apply(
        app,
        &runtime,
        M::SessionPhase {
            thread_id: None,
            phase,
        },
    );
}

#[test]
fn client_queues_until_ready_then_flushes() {
    let mut app = new_app_for_relay_tests();
    app.threads.session_phase = protocol::ThreadPhase::Preparing {
        stage: protocol::PrepareStage::Skills,
    };
    app.composer
        .pending_prompts
        .push(protocol::AgentRequest::Prompt {
            text: "hello".to_string(),
            images: Vec::new(),
            sent_at_ms: None,
        });
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    app.link.tx = tx;

    apply_phase(&mut app, protocol::ThreadPhase::Ready);

    assert_eq!(app.threads.session_phase, protocol::ThreadPhase::Ready);
    assert!(app.composer.pending_prompts.is_empty());
    assert!(matches!(
        rx.try_recv().ok().and_then(|o| o.into_request()),
        Some(protocol::AgentRequest::Prompt { text, .. }) if text == "hello"
    ));
}

#[test]
fn post_welcome_failure_is_disclosed_and_never_quits() {
    let mut app = new_app_for_relay_tests();
    app.session.switching_session = Some("abc".to_string());

    apply_phase(
        &mut app,
        protocol::ThreadPhase::Failed {
            code: Some(protocol::ERR_SESSION_UNRESUMABLE.to_string()),
            message: "could not load".to_string(),
        },
    );

    assert!(app.session.switching_session.is_none());
    assert!(matches!(
        app.threads.session_phase,
        protocol::ThreadPhase::Failed { .. }
    ));
    assert!(
        !app.link
            .should_quit
            .load(std::sync::atomic::Ordering::SeqCst),
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
    app.session.current_session_id = "s1".to_string();
    app.threads.session_phase = protocol::ThreadPhase::Preparing {
        stage: protocol::PrepareStage::Skills,
    };
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    app.link.tx = tx;

    apply_phase(&mut app, protocol::ThreadPhase::Ready);

    let sent: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok())
        .filter_map(|o| o.into_request())
        .collect();
    assert!(
        sent.iter().any(|request| matches!(
            request,
            protocol::AgentRequest::QueueSnapshot { thread_id } if thread_id == "s1"
        )),
        "arriving on a thread must ask the driver what it already holds: {sent:?}"
    );
}

#[test]
fn build_app_with_non_ready_boot_phase_never_spoofs_switching_session() {
    let live_session_id = "32aa8e10-183d-4b41-835e-04fb8e64b0dd".to_string();
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let app = crate::shell::build_app(crate::shell::AppBoot {
        live_session_init: &live_session_id,
        initial_provider: "openai".to_string(),
        initial_model: "gpt-5".to_string(),
        session_phase_init: protocol::ThreadPhase::Binding,
        restored: Vec::new(),
        tui_config: std::sync::Arc::new(crate::config::TuiConfig::default()),
        command_catalog: tool::CommandCatalog::default(),
        initial_pending_prompts: Vec::new(),
        input_history: Vec::new(),
        input_history_config: crate::config::InputHistoryConfig::default(),
        startup_overlay: crate::shell::StartupOverlay::None,
        profile: graticule::TerminalProfile::default(),
        token_ledger: None,
        tx,
        should_quit: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
    });

    assert!(
        app.session.switching_session.is_none(),
        "build_app must never put the raw boot UUID into switching_session"
    );
    assert_eq!(app.threads.session_phase, protocol::ThreadPhase::Binding);
    assert_eq!(
        crate::shell::event_loop::render::select_empty_state_guidance(
            app.session.switching_session.as_deref(),
            &app.threads.session_phase,
            true,
        ),
        crate::shell::render::EmptyStateGuidance::ThreadPhase(protocol::ThreadPhase::Binding),
    );
    assert_eq!(
        crate::shell::event_loop::render::select_lifecycle_status(
            false,
            app.session.switching_session.as_deref(),
            &app.threads.session_phase,
        ),
        Some("binding thread…".to_string()),
    );
}

#[test]
fn render_projection_composes_resuming_with_live_phase_and_prioritizes_link_down() {
    use crate::shell::event_loop::render::{select_empty_state_guidance, select_lifecycle_status};
    use crate::shell::render::EmptyStateGuidance;

    let skills_phase = protocol::ThreadPhase::Preparing {
        stage: protocol::PrepareStage::Skills,
    };
    assert_eq!(
        select_empty_state_guidance(Some("7c405d7e"), &skills_phase, true),
        EmptyStateGuidance::ResumingThread {
            short_id: "7c405d7e".to_string(),
            phase: skills_phase.clone(),
        }
    );
    assert_eq!(
        select_lifecycle_status(false, Some("7c405d7e"), &skills_phase),
        Some("resuming 7c405d7e · preparing skills…".to_string())
    );
    assert_eq!(
        select_lifecycle_status(true, Some("7c405d7e"), &skills_phase),
        Some("server link lost".to_string()),
        "dead server link must override any in-flight phase"
    );
    assert_eq!(
        select_empty_state_guidance(None, &protocol::ThreadPhase::Ready, true),
        EmptyStateGuidance::Tour
    );
    assert_eq!(
        select_empty_state_guidance(None, &protocol::ThreadPhase::Ready, false),
        EmptyStateGuidance::NeedsProvider
    );
    assert_eq!(
        select_lifecycle_status(false, None, &protocol::ThreadPhase::Ready),
        None
    );
}

#[tokio::test]
async fn submitting_chat_while_thread_phase_failed_preserves_draft_and_discloses_failure() {
    let mut app = new_app_for_relay_tests();
    let runtime = crate::shell::event_loop::UiRuntime::minimal_for_test();
    app.threads.session_phase = protocol::ThreadPhase::Failed {
        code: Some(protocol::ERR_SESSION_UNRESUMABLE.to_string()),
        message: "corrupt store".to_string(),
    };

    crate::shell::event_loop::actions::commands::handle_send_chat(
        &mut app,
        &runtime,
        "s1",
        "keep my draft".to_string(),
    )
    .await;

    assert!(
        app.composer.pending_prompts.is_empty(),
        "failed thread must never queue prompts into a dead pending_prompts buffer"
    );
    assert_eq!(app.composer.input, "keep my draft");
    assert_eq!(app.composer.cursor_position, "keep my draft".len());
    assert!(
        app.notice_log()
            .lines()
            .iter()
            .any(|n| n.text.contains("corrupt store")),
        "must disclose the real preparation failure instead of QueuedWhilePreparing"
    );
    assert!(
        !app.notice_log()
            .lines()
            .iter()
            .any(|n| n.text.contains("will be sent when it is ready")),
        "must not lie that the prompt will be sent when ready"
    );
}
