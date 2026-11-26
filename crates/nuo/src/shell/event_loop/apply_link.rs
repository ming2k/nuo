//! Server lifecycle, link establishment, declarative tab reconciliation, and
//! subscription refusal handlers for the TUI event loop (ADR-0260).

use tokio::sync::mpsc;

use crate::shell::App;
use crate::shell::app::state::ClientLinkPhase;

/// Apply a server-attested subscription acknowledgement, mounting the tab and
/// reconciling parked drafts when the first thread joins a session-free boot.
pub(super) fn apply_thread_subscribed(
    app: &mut App,
    thread_id: String,
    phase: protocol::ThreadPhase,
) -> bool {
    let first_thread = app.session.live_session_id.is_empty();
    app.session.subscribed_threads.insert(thread_id.clone());
    app.threads.register(thread_id.clone(), 1, phase.clone());
    let has_tab = app
        .surfaces
        .surfaces
        .tabs()
        .iter()
        .any(|t| t.thread_id() == Some(thread_id.as_str()));
    if !has_tab {
        let tab = crate::shell::surfaces::ClientTab::thread(&thread_id, None::<String>);
        app.open_tab(tab);
        app.activate_thread_workspace(&thread_id);
        app.threads.session_phase = phase;
        app.session.thread_phase_since = std::time::Instant::now();
    } else if app.session.current_session_id == thread_id {
        app.threads.session_phase = phase;
        app.session.thread_phase_since = std::time::Instant::now();
    }
    if first_thread {
        app.session.live_session_id = thread_id.clone();
        if app.session.current_session_id.is_empty() {
            app.session.current_session_id = thread_id.clone();
        }
        let draft_ids: Vec<String> = app
            .surfaces
            .drafts
            .known_thread_ids()
            .into_iter()
            .filter(|id| id != &thread_id)
            .collect();
        if !draft_ids.is_empty() {
            app.send_intent(protocol::AgentRequest::CheckThreadsExist {
                thread_ids: draft_ids,
            });
        }
    }
    true
}

/// Declarative thread reconciliation (ADR-0260): whenever the server admits
/// work (`Ready` / `Reloading`), flush any pending initial subscription and
/// ensure every open thread tab is subscribed on the current connection.
pub(super) fn reconcile_subscriptions(app: &mut App) {
    if !app.session.server_phase.admits_work() {
        return;
    }
    if let Some((target, project)) = app.session.pending_initial_subscribe.take() {
        app.threads.session_phase = protocol::ThreadPhase::Preparing {
            stage: protocol::PrepareStage::Thread,
        };
        app.session.thread_phase_since = std::time::Instant::now();
        app.subscribe_thread(target, project);
        return;
    }
    // Re-subscribe all open thread tabs after a reconnect or server restart.
    let active_tid = (!app.session.current_session_id.is_empty())
        .then(|| app.session.current_session_id.clone());
    let mut to_subscribe: Vec<String> = Vec::new();
    if let Some(ref tid) = active_tid
        && !app.session.subscribed_threads.contains(tid)
    {
        to_subscribe.push(tid.clone());
    }
    for tab in app.surfaces.surfaces.tabs() {
        if let Some(tid) = tab.thread_id()
            && !tid.is_empty()
            && !app.session.subscribed_threads.contains(tid)
            && !to_subscribe.iter().any(|existing| existing == tid)
        {
            to_subscribe.push(tid.to_string());
        }
    }
    if !to_subscribe.is_empty() {
        if active_tid.is_some() {
            app.threads.session_phase = protocol::ThreadPhase::Preparing {
                stage: protocol::PrepareStage::Thread,
            };
            app.session.thread_phase_since = std::time::Instant::now();
        }
        for tid in to_subscribe {
            app.session.subscribed_threads.insert(tid.clone());
            app.subscribe_thread(protocol::ThreadTarget::Attach { id: Some(tid) }, None);
        }
    }
}

pub(super) fn apply_server_phase(app: &mut App, phase: protocol::ServerPhase) -> bool {
    let draining = matches!(phase, protocol::ServerPhase::Draining { .. });
    if app.session.server_phase != phase {
        app.session.server_phase = phase;
        app.session.server_phase_since = std::time::Instant::now();
    }
    if draining {
        app.session.switching_session = None;
    }
    reconcile_subscriptions(app);
    true
}

pub(super) fn apply_link_established(
    app: &mut App,
    server_id: String,
    server_phase: protocol::ServerPhase,
    persistence_health: Option<protocol::monitor::PersistenceHealth>,
    req_tx: mpsc::UnboundedSender<crate::shell::client::ClientOutbound>,
) -> bool {
    let restarted = app
        .link
        .known_server_id
        .as_deref()
        .is_some_and(|prev| !prev.is_empty() && prev != server_id);
    app.link.tx = req_tx;
    app.link.link_down = false;
    app.link.relink_requested = false;
    app.link.known_server_id = Some(server_id.clone());
    app.link.phase = ClientLinkPhase::Connected { server_id };
    app.link.phase_since = std::time::Instant::now();
    app.session.persistence_health = persistence_health;
    app.session.server_phase = server_phase;
    app.session.server_phase_since = std::time::Instant::now();
    // Fresh wire connection starts with zero subscribed threads.
    app.session.subscribed_threads.clear();

    if restarted {
        tracing::info!("reconnected to a restarted Nuo server; reconciling open tabs");
    }
    reconcile_subscriptions(app);
    true
}

pub(super) fn apply_subscribe_refused(
    app: &mut App,
    error: crate::shell::client::AttachError,
) -> bool {
    if error.is_server_not_ready() {
        return true;
    }
    if error.is_session_gone() {
        let gone_id = app.session.current_session_id.clone();
        if !gone_id.is_empty() {
            app.session.subscribed_threads.remove(&gone_id);
            if let Some(pos) = app
                .surfaces
                .surfaces
                .tabs()
                .iter()
                .position(|t| t.thread_id() == Some(gone_id.as_str()))
            {
                app.close_tab(pos);
            }
            if app.session.current_session_id == gone_id {
                app.session.current_session_id.clear();
            }
            if app.session.live_session_id == gone_id {
                app.session.live_session_id.clear();
            }
        }
        app.record_client_notice(
            crate::shell::app::notice_catalog::ClientNotice::AttachSessionGone {
                detail: error.message,
            },
        );
        if app.session.current_session_id.is_empty() {
            let options = app.session.initial_init_options.clone();
            let project = app.session.project_scope.clone();
            app.subscribe_thread(
                protocol::ThreadTarget::Picker {
                    options: Some(options),
                },
                project,
            );
        }
        return true;
    }
    if error.is_session_unresumable() {
        app.record_client_notice(
            crate::shell::app::notice_catalog::ClientNotice::AttachSessionUnresumable {
                detail: error.message,
            },
        );
        if app.session.current_session_id.is_empty() || app.session.subscribed_threads.is_empty() {
            let options = app.session.initial_init_options.clone();
            let project = app.session.project_scope.clone();
            app.subscribe_thread(
                protocol::ThreadTarget::Picker {
                    options: Some(options),
                },
                project,
            );
        }
        return true;
    }
    app.record_client_notice(
        crate::shell::app::notice_catalog::ClientNotice::SessionPrepFailed {
            detail: error.message,
        },
    );
    true
}

pub(super) fn apply_pick_required(app: &mut App, threads: Vec<protocol::ThreadOverview>) -> bool {
    app.session.sessions_overview = threads;
    let options = app.session.initial_init_options.clone();
    let project = app.session.project_scope.clone();
    app.subscribe_thread(
        protocol::ThreadTarget::Picker {
            options: Some(options),
        },
        project,
    );
    true
}
