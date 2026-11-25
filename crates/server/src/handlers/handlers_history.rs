//! Input-history and route-settings services for the frontend:
//! the server is the source of truth for the shared SQLite store, and the
//! frontend reaches it only through these wire requests — it never opens the
//! database directly.
//!
//! Every access goes through the single-writer actor: reads take a
//! reader snapshot off the handle, writes are actor commands. This module used
//! to open its own engine per request, racing the writer for the SQLite write
//! lock.
//!
//! ## Why the durable verbs are `async` and run on the blocking pool
//!
//! These handlers execute inside the session driver, which is an async task on
//! the shared server runtime. A synchronous reader checkout or a `*_blocking`
//! actor verb parks the calling thread until SQLite answers — and on a
//! current-thread runtime it parks the only thread that could drive the ack it
//! is waiting for. Under many concurrent clients that is a latent hang, not
//! merely a slow handler, so every blocking verb is bridged through
//! `spawn_blocking`.
use tokio::sync::mpsc::UnboundedSender;

use persistence::db::get_persistence_handle;
use protocol::events::AgentResponse;

/// `AgentRequest::QueryInputHistory`: load the persisted prompt history.
pub async fn query_input_history(resp_tx: &UnboundedSender<AgentResponse>) {
    let rows = tokio::task::spawn_blocking(|| {
        get_persistence_handle()
            .with_reader(|reader| reader.load_input_history(protocol::history::HISTORY_CAP))
            .unwrap_or_default()
    })
    .await
    .unwrap_or_default();
    let _ = resp_tx.send(AgentResponse::InputHistory(rows));
}

/// `AgentRequest::RecordInputHistory`: merge entries into the shared store.
///
/// A failed write is logged, not retried: input history is rebuildable
/// convenience data, and stalling the turn that produced it would cost more
/// than one missing recall row.
pub async fn record_input_history(entries: Vec<protocol::HistoryEntry>, dedup: bool) {
    let result = tokio::task::spawn_blocking(move || {
        get_persistence_handle().save_input_history_blocking(entries, dedup)
    })
    .await;
    match result {
        Ok(Ok(())) => {}
        Ok(Err(error)) => tracing::warn!(%error, "input history record failed"),
        Err(error) => tracing::warn!(%error, "input history task panicked"),
    }
}

/// `AgentRequest::DeleteInputHistoryEntry`: remove one row by content and
/// timestamp.
pub async fn delete_input_history_entry(text: String, created_at_ms: u64) {
    let result = tokio::task::spawn_blocking(move || {
        get_persistence_handle().delete_input_history_entry_blocking(&text, created_at_ms)
    })
    .await;
    match result {
        Ok(Ok(_)) => {}
        Ok(Err(error)) => tracing::warn!(%error, "input history delete failed"),
        Err(error) => tracing::warn!(%error, "input history task panicked"),
    }
}

/// `AgentRequest::QueryRouteSettings`: the stored capability overrides for
/// one provider/model route (the model editor's prefill).
pub fn query_route_settings(
    provider_id: &str,
    model: &str,
    resp_tx: &UnboundedSender<AgentResponse>,
) {
    let overrides = persistence::route_settings::RouteSettingsStore::load()
        .settings_for(provider_id, model)
        .and_then(|r| r.capability_overrides.clone());
    let _ = resp_tx.send(AgentResponse::RouteSettings {
        provider_id: provider_id.to_string(),
        model: model.to_string(),
        overrides,
    });
}

/// `AgentRequest::SearchHistory`: BM25 full-text search over every persisted
/// transcript entry in the shared store. `workspace: None`
/// searches all project buckets — the Archivist's cross-project recall plane.
///
/// Two-stage recall (Layer 3's deterministic leg): the strict
/// AND-form query runs first; when it recalls nothing, the same words are
/// re-queried OR-joined (`search_history_relaxed`) so a gist whose words
/// never co-occur still surfaces its candidates. Fail-open: any engine error
/// degrades to an empty hit list.
pub async fn search_history(
    query: &str,
    workspace: Option<&str>,
    limit: Option<usize>,
    resp_tx: &UnboundedSender<AgentResponse>,
) {
    const DEFAULT_LIMIT: usize = 20;
    const MAX_LIMIT: usize = 100;
    let limit = limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let workspace_path = workspace.map(std::path::PathBuf::from);
    let query = query.to_string();
    // The FTS scan is CPU + page-cache work over the shared store. It runs on
    // the blocking pool: many clients may search while sessions are mid-round,
    // and parking an async worker on SQLite would starve the drivers those
    // clients are attached to.
    let hits = tokio::task::spawn_blocking(move || {
        get_persistence_handle()
            .with_reader(|reader| {
                let strict = reader.search_history(&query, workspace_path.as_deref(), limit)?;
                if !strict.is_empty() {
                    return Ok(strict);
                }
                reader.search_history_relaxed(&query, workspace_path.as_deref(), limit)
            })
            .unwrap_or_else(|error| {
                tracing::warn!(%error, "history search failed");
                Vec::new()
            })
    })
    .await
    .unwrap_or_default();
    let hits = hits
        .into_iter()
        .map(|hit| {
            let workspace = hit
                .workspace_root
                .clone()
                .unwrap_or_else(|| "workspace-free".to_string());
            protocol::HistorySearchHit {
                entry_id: hit.entry_id,
                session_id: hit.session_id,
                workspace,
                session_title: hit.session_title,
                role: hit.role,
                snippet: hit.snippet,
                score: hit.score,
            }
        })
        .collect();
    let _ = resp_tx.send(AgentResponse::HistorySearch(hits));
}
