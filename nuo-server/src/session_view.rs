//! Snapshotting and lookup helpers for the session-context modal and the
//! session picker. Pure reads — no mutation of agent or session state.
//!
//! Extracted verbatim from `main.rs` to keep the binary entry-point focused on
//! wiring rather than presentation shaping.

use nuo_harness::Agent;
use crate::catalog;
use nuo_wire::{
    McpConnectionStatus, McpServerInfo, ModelInfo, SessionContextSnapshot, SessionOverview,
};
use nuo_persistence::{config::Config, session::SessionStore};
use nuo_harness::skills::SkillRegistry;

/// First 8 characters of a session id — short enough for picker rows while
/// still disambiguating in practice.
pub fn short_session_id(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

/// Whether each provider has a usable API key (env var or config).
/// Keyless providers (the mock fixture) always report `true`.
///
/// Derived from the provider catalog so the readiness signal and the actual
/// provider construction share one resolution path.
pub fn provider_key_status(_config: &Config) -> Vec<(String, bool)> {
    catalog::build_catalog()
        .iter()
        .map(|entry| (entry.id.clone(), entry.key_ready()))
        .collect()
}

/// Build a render-ready snapshot of the live session for the session-context
/// modal. Pulls model info from the catalog, tools/permissions/skills from the
/// agent, and MCP per-server tool names by matching the `mcp__<server>__*`
/// naming convention against the agent's installed tools.
///
/// Sent in reply to [`nuo_wire::AgentRequest::QuerySessionContext`] and re-sent
/// after any mutation ([`nuo_wire::AgentRequest::RevokePermission`] /
/// [`nuo_wire::AgentRequest::ToggleTool`]) so the modal always reflects the
/// post-change state.
pub fn build_session_context(
    agent: &Agent,
    _skills_registry: &SkillRegistry,
    mcp_statuses: &[(String, McpConnectionStatus)],
    config: &Config,
) -> SessionContextSnapshot {
    let provider_id = catalog::default_provider_id(config).to_string();
    let model = catalog::resolved_model_name(config, &provider_id).unwrap_or_default();

    // Catalog entry carries the authoritative display metadata; fall back to
    // the raw model id / empty when the provider isn't a known catalog entry.
    let entry = catalog::build_catalog()
        .into_iter()
        .find(|e| e.id == provider_id);
    let display_name = entry
        .as_ref()
        .map(|e| e.name.clone())
        .unwrap_or_else(|| model.clone());
    let description = entry
        .as_ref()
        .map(|e| e.description.clone())
        .unwrap_or_default();
    let context_window = entry
        .as_ref()
        .and_then(|e| e.channels.iter().find(|c| c.model == model))
        .map(|c| c.capabilities().context_window)
        .or_else(|| entry.as_ref().map(|e| e.context_window()))
        .unwrap_or(0);
    let api_key_ready = entry.as_ref().map(|e| e.key_ready()).unwrap_or(false);

    let model_info = ModelInfo {
        provider: provider_id,
        capabilities: derive_capabilities(&agent.provider.model_capabilities()),
        display_name,
        model,
        context_window,
        api_key_ready,
        description,
    };

    let tools = agent.snapshot_tools();
    let permissions = agent.allowed_tools_structured();
    let skills = agent.snapshot_skills();

    // Per-server tool names: match the agent's installed tools by their
    // `mcp__<server>__<tool>` naming convention. The status enum only carries a
    // count, so this is where the per-server list is reconstructed.
    let mcp = mcp_statuses
        .iter()
        .map(|(name, status)| {
            let prefix = format!("mcp:{}", name);
            let tool_names: Vec<String> = tools
                .iter()
                .filter(|t| t.source == prefix)
                .map(|t| t.name.clone())
                .collect();
            let (connected, disabled, failure) = match status {
                McpConnectionStatus::Connected { .. } => (true, false, None),
                McpConnectionStatus::Disabled => (false, true, None),
                McpConnectionStatus::Failed(reason) => (false, false, Some(reason.clone())),
                McpConnectionStatus::Connecting => (false, false, None),
            };
            McpServerInfo {
                name: name.clone(),
                connected,
                disabled,
                failure,
                tool_names,
            }
        })
        .collect();

    SessionContextSnapshot {
        model: model_info,
        tools,
        permissions,
        skills,
        mcp,
    }
}

/// Heuristic model-capability hints for the Tools / Mcp / Skills / Permissions
/// managers. The live provider exposes the channel-scoped capability view, so
/// a provider's remote catalogue can override a static model baseline.
///
/// Vision is listed only when some layer **declared** it (ADR-0230): these
/// hints are read as a statement about the route, and an undeclared route has
/// nothing to state. (The *request* path is permissive instead — an undeclared
/// route still carries images — but that is `accepts_images`, a policy rather
/// than a claim.)
pub fn derive_capabilities(capabilities: &nuo_wire::ModelCapabilities) -> Vec<String> {
    let mut caps = Vec::new();
    if capabilities.tool_call {
        caps.push("tool calling".to_string());
    }
    if capabilities.reasoning() {
        caps.push("reasoning".to_string());
    }
    if capabilities.vision == Some(true) {
        caps.push("vision".to_string());
    }
    caps
}

/// Render-ready list of past sessions for the picker. Failures (unreadable
/// store, corrupt index) degrade to an empty list rather than surfacing an
/// error: the picker is non-modal and a missing list is recoverable.
pub async fn build_sessions_overview(session: &SessionStore) -> Vec<SessionOverview> {
    let mut overview: Vec<SessionOverview> = match session.list().await {
        Ok(items) => items
            .into_iter()
            .map(|item| SessionOverview {
                id: item.id,
                overview: item.overview,
                created_at: item.created_at,
                updated_at: item.updated_at,
                message_count: item.message_count,
                active: item.active,
                parent_id: item.parent_id,
                fork_kind: item.fork_kind,
                digest: item.digest,
            })
            .collect(),
        Err(_) => Vec::new(),
    };

    if !overview.iter().any(|item| item.active) {
        let summary = session.active_summary().await;
        overview.push(SessionOverview {
            id: summary.id,
            overview: summary.overview,
            created_at: summary.created_at,
            updated_at: summary.updated_at,
            message_count: summary.message_count,
            active: true,
            parent_id: summary.parent_id,
            fork_kind: summary.fork_kind,
            digest: summary.digest,
        });
        overview.sort_by_key(|item| std::cmp::Reverse(item.updated_at));
    }

    overview
}
