//! Input-history and route-settings services for the frontend (ADR-0197):
//! the daemon is the source of truth for the shared SQLite store, and the
//! frontend reaches it only through these wire requests — it never opens the
//! database directly.
//!
//! Every access goes through the single-writer actor (ADR-0231): reads take a
//! reader snapshot off the handle, writes are actor commands. This module used
//! to open its own engine per request, racing the writer for the SQLite write
//! lock.

use tokio::sync::mpsc::UnboundedSender;

use nuo_wire::events::AgentResponse;
use nuo_persistence::db::get_persistence_handle;

/// `AgentRequest::QueryInputHistory`: load the persisted prompt history.
pub fn query_input_history(resp_tx: &UnboundedSender<AgentResponse>) {
    let rows = get_persistence_handle()
        .reader()
        .ok()
        .and_then(|reader| {
            reader
                .load_input_history(nuo_wire::history::HISTORY_CAP)
                .ok()
        })
        .unwrap_or_default();
    let _ = resp_tx.send(AgentResponse::InputHistory(rows));
}

/// `AgentRequest::RecordInputHistory`: merge entries into the shared store.
pub fn record_input_history(entries: Vec<nuo_wire::HistoryEntry>, dedup: bool) {
    let result = get_persistence_handle().save_input_history_blocking(entries, dedup);
    if let Err(error) = result {
        tracing::warn!(%error, "input history record failed");
    }
}

/// `AgentRequest::DeleteInputHistoryEntry`: remove one row by content and
/// timestamp.
pub fn delete_input_history_entry(text: &str, created_at_ms: u64) {
    let result = get_persistence_handle().delete_input_history_entry_blocking(text, created_at_ms);
    if let Err(error) = result {
        tracing::warn!(%error, "input history delete failed");
    }
}

/// `AgentRequest::QueryRouteSettings`: the stored capability overrides for
/// one provider/model route (the model editor's prefill).
pub fn query_route_settings(
    provider_id: &str,
    model: &str,
    resp_tx: &UnboundedSender<AgentResponse>,
) {
    let overrides = nuo_persistence::route_settings::RouteSettingsStore::load()
        .settings_for(provider_id, model)
        .and_then(|r| r.capability_overrides.clone());
    let _ = resp_tx.send(AgentResponse::RouteSettings {
        provider_id: provider_id.to_string(),
        model: model.to_string(),
        overrides,
    });
}

/// `AgentRequest::SearchHistory`: BM25 full-text search over every persisted
/// transcript entry in the shared store (ADR-0208). `workspace: None`
/// searches all project buckets — the Archivist's cross-project recall plane.
///
/// Two-stage recall (ADR-0208 Layer 3's deterministic leg): the strict
/// AND-form query runs first; when it recalls nothing, the same words are
/// re-queried OR-joined (`search_history_relaxed`) so a gist whose words
/// never co-occur still surfaces its candidates. Fail-open: any engine error
/// degrades to an empty hit list.
pub fn search_history(
    query: &str,
    workspace: Option<&str>,
    limit: Option<usize>,
    resp_tx: &UnboundedSender<AgentResponse>,
) {
    const DEFAULT_LIMIT: usize = 20;
    const MAX_LIMIT: usize = 100;
    let limit = limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let filter = workspace.map(|w| nuo_wire::WorkspaceFilter::Path(w.into()));
    let hits = get_persistence_handle()
        .reader()
        .map_err(|e| format!("could not open sqlite db: {e}"))
        .and_then(|reader| {
            let strict = reader
                .search_history(query, filter.as_ref(), limit)
                .map_err(|e| format!("history search failed: {e}"))?;
            if !strict.is_empty() {
                return Ok(strict);
            }
            reader
                .search_history_relaxed(query, filter.as_ref(), limit)
                .map_err(|e| format!("history search failed: {e}"))
        })
        .unwrap_or_else(|error| {
            tracing::warn!(%error, "history search failed");
            Vec::new()
        });
    let hits = hits
        .into_iter()
        .map(|hit| {
            let workspace = hit
                .workspace_root
                .clone()
                .unwrap_or_else(|| "workspace-free".to_string());
            nuo_wire::HistorySearchHit {
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
