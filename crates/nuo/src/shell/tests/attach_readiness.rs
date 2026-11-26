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
    let preparing = protocol::ThreadPhase::Preparing {
        stage: protocol::PrepareStage::Thread,
    };
    let app = crate::shell::build_app(crate::shell::AppBoot {
        live_session_init: &live_session_id,
        initial_provider: "openai".to_string(),
        initial_model: "gpt-5".to_string(),
        session_phase_init: preparing.clone(),
        server_phase_init: protocol::ServerPhase::Ready,
        pending_initial_subscribe: None,
        restored: Vec::new(),
        tui_config: std::sync::Arc::new(view::tui_config::TuiConfig::default()),
        command_catalog: tool::CommandCatalog::default(),
        initial_pending_prompts: Vec::new(),
        input_history: Vec::new(),
        input_history_config: view::tui_config::InputHistoryConfig::default(),
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
    assert_eq!(app.threads.session_phase, preparing);
    let past_hysteresis = std::time::Duration::from_millis(200);
    let connected = crate::shell::app::state::ClientLinkPhase::Connected {
        server_id: "srv-1".to_string(),
    };
    assert_eq!(
        crate::shell::event_loop::render::select_empty_state_guidance(
            &connected,
            past_hysteresis,
            &app.session.server_phase,
            past_hysteresis,
            app.session.switching_session.as_deref(),
            &app.threads.session_phase,
            past_hysteresis,
            true,
        ),
        crate::shell::render::EmptyStateGuidance::ThreadPhase(preparing),
    );
    assert_eq!(
        crate::shell::event_loop::render::select_lifecycle_status(
            false,
            &connected,
            past_hysteresis,
            &app.session.server_phase,
            past_hysteresis,
            app.session.switching_session.as_deref(),
            &app.threads.session_phase,
            past_hysteresis,
        ),
        Some("preparing · thread store…".to_string()),
    );
}

#[test]
fn render_projection_enforces_link_server_resuming_thread_priority_ladder_and_hysteresis() {
    use crate::shell::app::state::ClientLinkPhase;
    use crate::shell::event_loop::render::{
        TRANSIENT_WAIT_HYSTERESIS, select_empty_state_guidance, select_lifecycle_status,
    };
    use crate::shell::render::EmptyStateGuidance;
    use std::time::Duration;

    let fast = Duration::from_millis(30);
    let slow = TRANSIENT_WAIT_HYSTERESIS + Duration::from_millis(10);
    let connected = ClientLinkPhase::Connected {
        server_id: "srv-1".to_string(),
    };
    let connecting = ClientLinkPhase::Connecting {
        label: "Starting Nuo server…".to_string(),
    };
    let reconnecting = ClientLinkPhase::Reconnecting {
        endpoint: "127.0.0.1:9800".to_string(),
        attempt: 2,
        max_attempts: 10,
        delay_ms: Some(500),
    };
    let ready_server = protocol::ServerPhase::Ready;
    let init_server = protocol::ServerPhase::Initializing {
        stage: protocol::ServerInitStage::Storage,
    };
    let draining_server = protocol::ServerPhase::Draining {
        reason: Some("SIGTERM".into()),
    };
    let reloading_server = protocol::ServerPhase::Reloading;
    let skills_phase = protocol::ThreadPhase::Preparing {
        stage: protocol::PrepareStage::Skills,
    };

    // Sub-120ms transient waits suppress flashing labels via QuietWait (ADR-0260 [INV-SRVPHASE-05]).
    assert_eq!(
        select_empty_state_guidance(
            &connecting,
            fast,
            &ready_server,
            fast,
            None,
            &protocol::ThreadPhase::Ready,
            fast,
            true
        ),
        EmptyStateGuidance::QuietWait
    );
    assert_eq!(
        select_lifecycle_status(
            false,
            &connecting,
            fast,
            &ready_server,
            fast,
            None,
            &protocol::ThreadPhase::Ready,
            fast
        ),
        None
    );
    assert_eq!(
        select_empty_state_guidance(
            &connected,
            slow,
            &init_server,
            fast,
            None,
            &protocol::ThreadPhase::Ready,
            fast,
            true
        ),
        EmptyStateGuidance::QuietWait
    );
    assert_eq!(
        select_empty_state_guidance(
            &connected,
            slow,
            &ready_server,
            slow,
            None,
            &skills_phase,
            fast,
            true
        ),
        EmptyStateGuidance::QuietWait
    );

    // Non-transient states (Reconnecting, Draining, Failed) appear immediately at 0ms.
    assert!(matches!(
        select_empty_state_guidance(
            &reconnecting,
            Duration::ZERO,
            &ready_server,
            Duration::ZERO,
            None,
            &protocol::ThreadPhase::Ready,
            Duration::ZERO,
            true
        ),
        EmptyStateGuidance::Connecting { ref label } if label.contains("Reconnecting to 127.0.0.1:9800")
    ));
    assert_eq!(
        select_empty_state_guidance(
            &connected,
            slow,
            &draining_server,
            Duration::ZERO,
            None,
            &skills_phase,
            Duration::ZERO,
            true
        ),
        EmptyStateGuidance::ServerPhase(draining_server)
    );

    // Once past 120ms, Server initializing outranks thread/resuming states.
    assert_eq!(
        select_empty_state_guidance(
            &connected,
            slow,
            &init_server,
            slow,
            Some("7c405d7e"),
            &skills_phase,
            slow,
            true
        ),
        EmptyStateGuidance::ServerPhase(init_server.clone())
    );
    assert_eq!(
        select_lifecycle_status(
            false,
            &connected,
            slow,
            &init_server,
            slow,
            Some("7c405d7e"),
            &skills_phase,
            slow
        ),
        Some("initializing server · storage…".to_string())
    );
    // Dead link outranks server initializing.
    assert_eq!(
        select_lifecycle_status(
            true,
            &connected,
            slow,
            &init_server,
            slow,
            Some("7c405d7e"),
            &skills_phase,
            slow
        ),
        Some("server link lost".to_string()),
        "dead server link must override any server or thread phase"
    );
    // Reloading is non-blocking: status bar discloses it, hero does not block.
    assert_eq!(
        select_lifecycle_status(
            false,
            &connected,
            slow,
            &reloading_server,
            Duration::ZERO,
            None,
            &protocol::ThreadPhase::Ready,
            slow
        ),
        Some("reloading server…".to_string())
    );
    assert_eq!(
        select_empty_state_guidance(
            &connected,
            slow,
            &reloading_server,
            Duration::ZERO,
            None,
            &protocol::ThreadPhase::Ready,
            slow,
            true
        ),
        EmptyStateGuidance::Tour
    );

    assert_eq!(
        select_empty_state_guidance(
            &connected,
            slow,
            &ready_server,
            slow,
            Some("7c405d7e"),
            &skills_phase,
            slow,
            true
        ),
        EmptyStateGuidance::ResumingThread {
            short_id: "7c405d7e".to_string(),
            phase: skills_phase.clone(),
        }
    );
    assert_eq!(
        select_lifecycle_status(
            false,
            &connected,
            slow,
            &ready_server,
            slow,
            Some("7c405d7e"),
            &skills_phase,
            slow
        ),
        Some("resuming 7c405d7e · preparing skills…".to_string())
    );
    assert_eq!(
        select_empty_state_guidance(
            &connected,
            slow,
            &ready_server,
            slow,
            None,
            &protocol::ThreadPhase::Ready,
            slow,
            true
        ),
        EmptyStateGuidance::Tour
    );
    assert_eq!(
        select_empty_state_guidance(
            &connected,
            slow,
            &ready_server,
            slow,
            None,
            &protocol::ThreadPhase::Ready,
            slow,
            false
        ),
        EmptyStateGuidance::NeedsProvider
    );
    assert_eq!(
        select_lifecycle_status(
            false,
            &connected,
            slow,
            &ready_server,
            slow,
            None,
            &protocol::ThreadPhase::Ready,
            slow
        ),
        None
    );
}

#[test]
fn server_phase_ready_flushes_deferred_initial_subscribe() {
    let mut app = new_app_for_relay_tests();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    app.link.tx = tx;
    app.session.server_phase = protocol::ServerPhase::Initializing {
        stage: protocol::ServerInitStage::Storage,
    };
    app.session.pending_initial_subscribe = Some((
        protocol::ThreadTarget::New { options: None },
        Some(std::path::PathBuf::from("/tmp/proj")),
    ));
    let runtime = crate::shell::event_loop::UiRuntime::minimal_for_test();

    let _ = crate::shell::event_loop::apply::apply(
        &mut app,
        &runtime,
        M::ServerPhase(protocol::ServerPhase::Initializing {
            stage: protocol::ServerInitStage::Runtime,
        }),
    );
    assert!(
        rx.try_recv().is_err(),
        "must not subscribe while server is still initializing"
    );

    let _ = crate::shell::event_loop::apply::apply(
        &mut app,
        &runtime,
        M::ServerPhase(protocol::ServerPhase::Ready),
    );
    assert_eq!(app.session.server_phase, protocol::ServerPhase::Ready);
    assert!(app.session.pending_initial_subscribe.is_none());
    assert!(
        matches!(
            app.threads.session_phase,
            protocol::ThreadPhase::Preparing {
                stage: protocol::PrepareStage::Thread
            }
        ),
        "must enter Preparing(Thread) while awaiting Subscribed"
    );
    assert!(matches!(
        rx.try_recv(),
        Ok(crate::shell::client::ClientOutbound::Subscribe {
            target: protocol::ThreadTarget::New { .. },
            ..
        })
    ));
}

#[test]
fn link_established_reconciles_open_tabs_across_server_restart_without_destroying_app() {
    let mut app = new_app_for_relay_tests();
    app.surfaces
        .surfaces
        .open_tab(crate::shell::surfaces::ClientTab::thread(
            "thread-a",
            None::<String>,
        ));
    app.surfaces
        .surfaces
        .open_tab(crate::shell::surfaces::ClientTab::thread(
            "thread-b",
            None::<String>,
        ));
    app.session.current_session_id = "thread-a".to_string();
    app.link.known_server_id = Some("srv-old".to_string());
    app.link.link_down = true;
    app.link.relink_requested = true;

    let (new_tx, mut new_rx) = tokio::sync::mpsc::unbounded_channel();
    let runtime = crate::shell::event_loop::UiRuntime::minimal_for_test();
    let _ = crate::shell::event_loop::apply::apply(
        &mut app,
        &runtime,
        M::LinkEstablished {
            server_id: "srv-restarted".to_string(),
            server_phase: protocol::ServerPhase::Ready,
            persistence_health: None,
            req_tx: new_tx,
        },
    );

    assert!(!app.link.link_down);
    assert!(!app.link.relink_requested);
    assert_eq!(app.link.known_server_id.as_deref(), Some("srv-restarted"));

    let mut subscribed = Vec::new();
    while let Ok(crate::shell::client::ClientOutbound::Subscribe {
        target: protocol::ThreadTarget::Attach { id: Some(id) },
        ..
    }) = new_rx.try_recv()
    {
        subscribed.push(id);
    }
    assert_eq!(
        subscribed,
        vec!["thread-a".to_string(), "thread-b".to_string()]
    );
}

#[test]
fn subscribe_refused_gone_and_pick_required_recover_in_place() {
    let mut app = new_app_for_relay_tests();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    app.link.tx = tx;
    app.session.current_session_id = "stale-thread".to_string();
    app.session.live_session_id = "stale-thread".to_string();
    app.surfaces
        .surfaces
        .open_tab(crate::shell::surfaces::ClientTab::thread(
            "stale-thread",
            None::<String>,
        ));
    let runtime = crate::shell::event_loop::UiRuntime::minimal_for_test();

    let _ = crate::shell::event_loop::apply::apply(
        &mut app,
        &runtime,
        M::SubscribeRefused(crate::shell::client::AttachError::from_server_refusal(
            Some(protocol::ERR_SESSION_GONE.to_string()),
            "session 'stale-thread' no longer exists",
        )),
    );
    assert!(
        !app.surfaces
            .surfaces
            .tabs()
            .iter()
            .any(|t| t.thread_id() == Some("stale-thread")),
        "stale tab must be retired on ERR_SESSION_GONE"
    );
    assert!(matches!(
        rx.try_recv(),
        Ok(crate::shell::client::ClientOutbound::Subscribe {
            target: protocol::ThreadTarget::Picker { .. },
            ..
        })
    ));

    let _ = crate::shell::event_loop::apply::apply(&mut app, &runtime, M::PickRequired(Vec::new()));
    assert!(matches!(
        rx.try_recv(),
        Ok(crate::shell::client::ClientOutbound::Subscribe {
            target: protocol::ThreadTarget::Picker { .. },
            ..
        })
    ));
}

#[test]
fn zero_thread_dashboard_skips_unaddressed_thread_intents_cleanly() {
    let mut app = new_app_for_relay_tests();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    app.link.tx = tx;
    app.session.current_session_id.clear();
    app.session.live_session_id.clear();
    app.session.subscribed_threads.clear();

    assert!(
        !app.send_intent(protocol::AgentRequest::Interrupt),
        "thread-scoped intent must be skipped cleanly when 0 threads are subscribed"
    );
    assert!(rx.try_recv().is_err());
    assert!(!app.link.link_down);
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
