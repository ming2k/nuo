//! Session-management, tool-toggle, and `/btw` aside handlers, extracted
//! verbatim from the agent background task's `match req { … }` dispatch.
//!
//! Each handler is one match arm, lifted unchanged. Parameters are named to
//! match the original loop locals (`session`, `agent`, `resp_tx`, `side`, …)
//! so the body reads exactly as it did inline.

use nuo_harness::Agent;
use crate::session_driver::send_harness_state_for_session;
use nuo_wire::{AgentResponse, LoopStatus};
use crate::mcp::McpRuntime;
use nuo_persistence::{config::Config, session::SessionStore};
use nuo_harness::skills::SkillRegistry;
use std::sync::Arc;
use tokio::sync::{RwLock as AsyncRwLock, mpsc};

use crate::session_view::{build_session_context, build_sessions_overview};
use crate::side::SideRegistry;

/// `AgentRequest::DeleteSession` — delete by id (or short-id prefix) and push
/// a fresh sessions-overview snapshot, or surface the storage error.
pub async fn delete(
    session: &Arc<SessionStore>,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    id: String,
) {
    let active_id_before = session.id().await;
    match session.delete(&id).await {
        Ok(deleted_id) => {
            if deleted_id == active_id_before {
                let new_id = session.id().await;
                let _ = resp_tx.send(AgentResponse::ConversationCleared { session_id: new_id });
            }
            let _ = resp_tx.send(AgentResponse::SessionsOverview(
                build_sessions_overview(session).await,
            ));
        }
        Err(error) => {
            let _ = resp_tx.send(AgentResponse::SessionsOverview(
                build_sessions_overview(session).await,
            ));
            let _ = resp_tx.send(AgentResponse::Error(error));
        }
    }
}

/// `AgentRequest::RenameSession` — set (or clear) a session's manual title by
/// id (or short-id prefix) and push a fresh sessions-overview snapshot, or
/// surface the storage error. `title = None` clears the manual override, so
/// the overview falls back to the AI-title / first-prompt preview (ADR-0022).
/// The pushed overview also refreshes the hosted session's monitor row: the
/// registry's broadcast-tap folds it into the tracker and republishes
/// `MonitorEvent::SessionUpdated` with the new title.
pub async fn rename(
    session: &Arc<SessionStore>,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    id: String,
    title: Option<String>,
) {
    match session.rename(&id, title).await {
        Ok(()) => {
            let overview = build_sessions_overview(session).await;
            let _ = resp_tx.send(AgentResponse::SessionsOverview(overview));
        }
        Err(error) => {
            let _ = resp_tx.send(AgentResponse::Error(error));
        }
    }
}

/// `AgentRequest::QuerySessionDetail` — full detail for one session (complete
/// last prompt, title, timestamps). Reply with [`AgentResponse::SessionDetail`]
/// for the session-info sub-view, or surface the storage error.
pub async fn detail(
    session: &Arc<SessionStore>,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    id: String,
) {
    match session.detail(&id).await {
        Ok(detail) => {
            let _ = resp_tx.send(AgentResponse::SessionDetail(detail));
        }
        Err(error) => {
            let _ = resp_tx.send(AgentResponse::Error(error));
        }
    }
}

/// Return a fresh sessions-picker snapshot without prescribing whether the
/// requesting frontend should display it.
pub async fn overview(session: &Arc<SessionStore>, resp_tx: &mpsc::UnboundedSender<AgentResponse>) {
    let _ = resp_tx.send(AgentResponse::SessionsOverview(
        build_sessions_overview(session).await,
    ));
}

/// Return the current session DAG without prescribing frontend navigation.
pub async fn tree(session: &Arc<SessionStore>, resp_tx: &mpsc::UnboundedSender<AgentResponse>) {
    let session_id = session.id().await;
    let tree = session.tree().await;
    let _ = resp_tx.send(AgentResponse::SessionTreeSnapshot { session_id, tree });
}

/// `AgentRequest::QueryTokenUsage` — snapshot the server-side token-source
/// ledger for one session and reply with
/// [`AgentResponse::TokenUsageReport`]. Attached frontends hold no local
/// ledger, so the context-usage modal reads the daemon's accounting through
/// this on-demand round-trip. Pure read: the ledger is shared across sessions
/// and filtered by `session_id`, so an unknown/empty id simply yields an
/// empty report.
pub fn token_usage(
    token_ledger: &nuo_wire::TokenSourceLedger,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    session_id: String,
) {
    let report = token_ledger.snapshot_for_session(&session_id);
    let _ = resp_tx.send(AgentResponse::TokenUsageReport { session_id, report });
}

/// `AgentRequest::QueryUsageStats` — aggregate the durable cross-session
/// usage store (ADR-0122) and reply with
/// [`AgentResponse::UsageStatsReport`]. Pure read over `data/usage/`;
/// independent of the live ledger and of any session, so it reflects days
/// whose sessions were long since deleted.
pub async fn usage_stats(resp_tx: &mpsc::UnboundedSender<AgentResponse>, event_cap: usize) {
    // Aggregating the report window is a CPU-bound deserialize + fold over
    // every day blob (~85-110 ms with a year of history), so it runs on the
    // blocking pool: opening the overlay must never stall the session
    // driver's async worker.
    let report = tokio::task::spawn_blocking(move || {
        nuo_persistence::usage_stats::UsageStatsStore::new().report(event_cap)
    })
    .await
    .unwrap_or_default();
    let _ = resp_tx.send(AgentResponse::UsageStatsReport { report });
}

/// `AgentRequest::QuerySessionContext` — build and push the
/// model/tools/permissions/skills/mcp snapshot for the Tools / Mcp / Skills /
/// Permissions manager modals.
pub fn query_context(
    agent: &Agent,
    skills_registry: &Arc<SkillRegistry>,
    mcp_runtime: &Arc<McpRuntime>,
    config: &Config,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
) {
    let snapshot = build_session_context(
        agent,
        skills_registry,
        &mcp_runtime.statuses_snapshot(),
        config,
    );
    let _ = resp_tx.send(AgentResponse::SessionContext(snapshot));
}

/// `AgentRequest::RevokePermission` — drop one cached always-allow rule and
/// push a refreshed snapshot, or report there was nothing matching.
pub fn revoke_permission(
    agent: &Agent,
    skills_registry: &Arc<SkillRegistry>,
    mcp_runtime: &Arc<McpRuntime>,
    config: &Config,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    tool: String,
    scope: String,
) {
    let removed = agent.revoke_allowed_tool(&tool, &scope);
    if removed {
        let snapshot = build_session_context(
            agent,
            skills_registry,
            &mcp_runtime.statuses_snapshot(),
            config,
        );
        let _ = resp_tx.send(AgentResponse::SessionContext(snapshot));
    } else {
        let _ = resp_tx.send(AgentResponse::Error(format!(
            "No cached always-allow rule for {} {}.",
            tool, scope
        )));
    }
}

/// `AgentRequest::ClearAllPermissions` — drop every cached always-allow rule
/// for this process and push a refreshed snapshot so the permissions manager
/// modal reflects the now-empty list.
pub fn clear_all_permissions(
    agent: &Agent,
    skills_registry: &Arc<SkillRegistry>,
    mcp_runtime: &Arc<McpRuntime>,
    config: &Config,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
) {
    agent.clear_allowed_tools();
    let snapshot = build_session_context(
        agent,
        skills_registry,
        &mcp_runtime.statuses_snapshot(),
        config,
    );
    let _ = resp_tx.send(AgentResponse::SessionContext(snapshot));
}

/// `AgentRequest::ToggleTool` — enable/disable a tool for the session and
/// push a refreshed snapshot. A no-op (unknown tool, or already in the target
/// state) still refreshes the snapshot so the modal settles rather than
/// leaving the row looking stale, plus surfaces a soft error.
pub fn toggle_tool(
    agent: &Agent,
    skills_registry: &Arc<SkillRegistry>,
    mcp_runtime: &Arc<McpRuntime>,
    config: &Config,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    name: String,
    enabled: bool,
) {
    let changed = agent.set_tool_enabled(&name, enabled);
    let snapshot = build_session_context(
        agent,
        skills_registry,
        &mcp_runtime.statuses_snapshot(),
        config,
    );
    if !changed {
        let _ = resp_tx.send(AgentResponse::Error(format!(
            "Tool '{}' is unknown or already {}.",
            name,
            if enabled { "enabled" } else { "disabled" }
        )));
    }
    let _ = resp_tx.send(AgentResponse::SessionContext(snapshot));
}

/// `AgentRequest::ToggleMcpServer` — connect/disconnect a configured MCP server
/// for the live session (session-scoped; config.toml is untouched). The runtime
/// rebuilds the agent's tool list, then we push a refreshed snapshot. A failure
/// to connect surfaces as a soft error but still refreshes the snapshot so the
/// row settles on its new (Failed) status.
pub async fn toggle_mcp_server(
    agent: &Agent,
    skills_registry: &Arc<SkillRegistry>,
    mcp_runtime: &Arc<McpRuntime>,
    config: &Config,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    name: String,
    enabled: bool,
) {
    if let Err(error) = mcp_runtime.set_enabled(&name, enabled).await {
        let _ = resp_tx.send(AgentResponse::Error(error));
    }
    let snapshot = build_session_context(
        agent,
        skills_registry,
        &mcp_runtime.statuses_snapshot(),
        config,
    );
    let _ = resp_tx.send(AgentResponse::SessionContext(snapshot));
}

/// `AgentRequest::ReconnectMcpServer` — re-establish one server's connection on
/// demand (the `/mcp` modal's `r` action) and push a refreshed snapshot.
pub async fn reconnect_mcp_server(
    agent: &Agent,
    skills_registry: &Arc<SkillRegistry>,
    mcp_runtime: &Arc<McpRuntime>,
    config: &Config,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    name: String,
) {
    if let Err(error) = mcp_runtime.reconnect(&name).await {
        let _ = resp_tx.send(AgentResponse::Error(error));
    }
    let snapshot = build_session_context(
        agent,
        skills_registry,
        &mcp_runtime.statuses_snapshot(),
        config,
    );
    let _ = resp_tx.send(AgentResponse::SessionContext(snapshot));
}

/// `AgentRequest::ExitSideView` — detach from the `/btw` aside view and return
/// to the primary transcript (ADR-0103 §1). Non-destructive by default: the
/// aside's in-flight round is left alone and its session stays registered so
/// it can be re-entered later (the asides list shows it). The one carve-out
/// is the pristine rule (§4): an aside that never started a round has no user
/// content of its own, so it is dropped from the registry **and** its session
/// files are deleted — an opened-then-abandoned `/btw` never litters.
pub async fn detach_side_view(
    side: &Arc<AsyncRwLock<SideRegistry>>,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
) {
    let _detached_id = side.write().await.detach();
    crate::side::publish_btw_list(side, resp_tx).await;
    let _ = resp_tx.send(AgentResponse::SideViewClosed);
}

/// `AgentRequest::FocusSide` — jump the view into a live aside (ADR-0103 §5).
/// Re-opens it if needed, makes it the composer target, and emits
/// `SideViewOpened` carrying the aside's full persisted transcript so the
/// frontend rebuilds its side buffer (inherited parent context included —
/// ADR-0103 §6).
pub async fn focus_side(
    side: &Arc<AsyncRwLock<SideRegistry>>,
    primary_session: &Arc<SessionStore>,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    side_id: String,
) {
    let focused = side.write().await.focus(&side_id);
    if !focused {
        let _ = resp_tx.send(AgentResponse::Error(
            "That aside is no longer open.".to_string(),
        ));
        return;
    }
    emit_side_view_opened(side, primary_session, resp_tx, &side_id).await;
}

/// Build and send the `SideViewOpened` event for a registered aside: routing
/// keys plus the one-shot transcript back-fill (§6). Shared by `/btw` (new
/// aside) and `focus_side` (re-entry).
pub async fn emit_side_view_opened(
    side: &Arc<AsyncRwLock<SideRegistry>>,
    primary_session: &Arc<SessionStore>,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    side_id: &str,
) {
    let handle = side.read().await.handle(side_id);
    let Some(s) = handle else {
        return;
    };
    let messages = s.store.full_transcript().await;
    let commands = s.store.commands().await;
    let round_interrupts = s.store.round_interrupts().await;
    let retry_resolutions = s.store.retry_resolutions().await;
    let primary_id = primary_session.id().await;
    let _ = resp_tx.send(AgentResponse::SideViewOpened {
        side_id: s.id.clone(),
        primary_id,
        messages,
        commands,
        round_interrupts,
        retry_resolutions,
    });
}

/// `AgentRequest::InterruptSide` — interrupt the in-flight round of one aside
/// (ADR-0103 §2). Esc inside an aside view resolves here; interrupting an
/// aside never closes it. Mirrors the primary interrupt's eager idle flip so
/// the aside's own activity surfaces collapse immediately, without touching
/// the primary's lifecycle.
pub async fn interrupt_side(
    side: &Arc<AsyncRwLock<SideRegistry>>,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    side_id: String,
) {
    let target = side.read().await.handle(&side_id);
    let Some(s) = target else {
        return;
    };
    // Park the user-interrupt reason before cancelling so the aside round's
    // tail labels its own unwind (C11) — Esc Esc inside an aside view.
    s.lifecycle
        .record_interrupt(nuo_wire::RoundInterruptReason::User);
    s.agent.reject_pending_permissions();
    s.agent.reject_pending_user_questions();
    s.agent.reject_pending_inputs();
    // Session-scoped PermissionsCleared + eager idle snapshot, exactly like
    // the primary's `interrupt` — but scoped to the aside's session id so the
    // primary chrome is untouched.
    let _ = resp_tx.send(AgentResponse::PermissionsCleared);
    send_harness_state_for_session(resp_tx, &s.id, &s.agent, &s.store, LoopStatus::Idle).await;
    s.lifecycle.cancel_current().await;
}

/// `AgentRequest::CloseSide` — close one aside for real (ADR-0103 §5, the
/// asides modal's `D` action): cancel any in-flight round, drop the registry
/// entry, and delete the aside's session files so it disappears from the
/// asides list and `/sessions`. If the closed aside was the focused view, the
/// harness also emits `SideViewClosed`.
pub async fn close_side(
    side: &Arc<AsyncRwLock<SideRegistry>>,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    side_id: String,
) {
    let was_active = side.read().await.active().is_some_and(|s| s.id == side_id);
    if let Some(s) = side.write().await.remove(&side_id) {
        // The aside's files are deleted right below, so the record itself is
        // moot; parking the reason still labels the unwind if the tail races
        // the removal (C11).
        s.lifecycle
            .record_interrupt(nuo_wire::RoundInterruptReason::Superseded);
        s.agent.reject_pending_permissions();
        s.agent.reject_pending_user_questions();
        s.agent.reject_pending_inputs();
        s.lifecycle.cancel_current().await;
        let _ = s.store.delete(&s.id).await;
    }
    crate::side::publish_btw_list(side, resp_tx).await;
    if was_active {
        let _ = resp_tx.send(AgentResponse::SideViewClosed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nuo_wire::Message;

    /// A store with one persisted user prompt (so it appears in `list()`),
    /// plus the response channel a frontend would hold.
    async fn store_with_prompt() -> (tempfile::TempDir, Arc<SessionStore>) {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(SessionStore::for_path(dir.path().join("session.json")));
        store
            .replace_messages(vec![Message::new(
                nuo_wire::Role::User,
                "first prompt",
            )])
            .await
            .unwrap();
        (dir, store)
    }

    #[tokio::test]
    async fn rename_replies_with_a_fresh_sessions_overview() {
        let (_dir, store) = store_with_prompt().await;
        let old_id = store.id().await;
        let (resp_tx, mut resp_rx) = mpsc::unbounded_channel();
        // Reset to a new session so `old_id` becomes an alternative switch candidate (ADR-0250)
        let _ = store.reset().await;

        rename(
            &store,
            &resp_tx,
            old_id[..8].to_string(),
            Some("new title".to_string()),
        )
        .await;

        let Some(AgentResponse::SessionsOverview(items)) = resp_rx.recv().await else {
            panic!("expected a sessions-overview push after a rename");
        };
        let row = items.iter().find(|item| item.id == old_id).unwrap();
        assert_eq!(row.overview, "new title");
    }

    #[tokio::test]
    async fn rename_unknown_id_replies_with_the_storage_error() {
        let (_dir, store) = store_with_prompt().await;
        let (resp_tx, mut resp_rx) = mpsc::unbounded_channel();

        rename(&store, &resp_tx, "deadbeef".to_string(), None).await;

        let Some(AgentResponse::Error(error)) = resp_rx.recv().await else {
            panic!("expected an error reply for an unknown id");
        };
        assert_eq!(error, "No session matches 'deadbeef'.");
    }

    #[tokio::test]
    async fn overview_query_returns_data_without_a_navigation_signal() {
        let (_dir, store) = store_with_prompt().await;
        // Reset so the persisted session becomes an alternative switch candidate (ADR-0250)
        let _ = store.reset().await;
        let (resp_tx, mut resp_rx) = mpsc::unbounded_channel();

        overview(&store, &resp_tx).await;

        let Some(AgentResponse::SessionsOverview(items)) = resp_rx.recv().await else {
            panic!("expected a sessions-overview snapshot");
        };
        assert!(!items.is_empty());
        assert!(resp_rx.try_recv().is_err(), "query must not navigate");
    }

    #[tokio::test]
    async fn tree_query_returns_the_current_dag_without_navigation() {
        let (_dir, store) = store_with_prompt().await;
        let (resp_tx, mut resp_rx) = mpsc::unbounded_channel();
        let expected = store.tree().await;

        tree(&store, &resp_tx).await;

        let Some(AgentResponse::SessionTreeSnapshot {
            session_id,
            tree: snapshot,
        }) = resp_rx.recv().await
        else {
            panic!("expected a session-tree snapshot");
        };
        assert_eq!(session_id, store.id().await);
        assert_eq!(
            serde_json::to_value(snapshot).unwrap(),
            serde_json::to_value(expected).unwrap()
        );
        assert!(resp_rx.try_recv().is_err(), "query must not navigate");
    }

    #[tokio::test]
    async fn delete_unpersisted_active_session_clears_conversation_and_pushes_overview() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(SessionStore::for_path(dir.path().join("session.json")));
        let initial_id = store.id().await;
        let (resp_tx, mut resp_rx) = mpsc::unbounded_channel();

        delete(&store, &resp_tx, initial_id.clone()).await;

        let Some(AgentResponse::ConversationCleared { session_id: new_id }) = resp_rx.recv().await
        else {
            panic!("expected ConversationCleared response when deleting active session");
        };
        assert_ne!(new_id, initial_id);

        let Some(AgentResponse::SessionsOverview(items)) = resp_rx.recv().await else {
            panic!("expected SessionsOverview response after delete");
        };
        // The newly reset active session must be present and tagged active for the monitor tracker.
        let active_row = items
            .iter()
            .find(|item| item.active)
            .expect("active session must be present");
        assert_eq!(active_row.id, new_id);
        assert!(!items.iter().any(|item| item.id == initial_id));
    }

    #[tokio::test]
    async fn delete_persisted_active_session_clears_conversation_and_pushes_overview() {
        let (_dir, store) = store_with_prompt().await;
        let initial_id = store.id().await;
        let (resp_tx, mut resp_rx) = mpsc::unbounded_channel();

        delete(&store, &resp_tx, initial_id.clone()).await;

        let Some(AgentResponse::ConversationCleared { session_id: new_id }) = resp_rx.recv().await
        else {
            panic!("expected ConversationCleared response when deleting active session");
        };
        assert_ne!(new_id, initial_id);

        let Some(AgentResponse::SessionsOverview(items)) = resp_rx.recv().await else {
            panic!("expected SessionsOverview response after delete");
        };
        assert!(!items.iter().any(|item| item.id == initial_id));
        let active_row = items
            .iter()
            .find(|item| item.active)
            .expect("active session must be present");
        assert_eq!(active_row.id, new_id);
    }

    #[tokio::test]
    async fn delete_already_absent_session_is_idempotent() {
        let (_dir, store) = store_with_prompt().await;
        let _ = store.reset().await;
        let (resp_tx, mut resp_rx) = mpsc::unbounded_channel();
        let fake_uuid = uuid::Uuid::new_v4().to_string();

        delete(&store, &resp_tx, fake_uuid.clone()).await;

        let Some(AgentResponse::SessionsOverview(items)) = resp_rx.recv().await else {
            panic!("expected SessionsOverview response after idempotent delete");
        };
        assert!(!items.is_empty());
    }
}
