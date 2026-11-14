//! Provider-switch / favorite / default-model handlers.
//!
//! Each handler is one match arm of the agent background task's dispatch.
//! Provider instances live in the  store, credentials in
//! , and per-route facts in the catalog cache —
//! holds only *behavior* ( /  / favorites),
//! which is what these handlers persist there. Routes are derived by the
//! catalog at activation time.
use nuo_engine::Agent;
use nuo_engine::orchestration::round_response;
use nuo_persistence::config::{Config, Credentials};
use nuo_persistence::connection_usage::ConnectionUsage;
use nuo_persistence::connections::Connections;
use nuo_persistence::model_providers::ModelProviders;
use nuo_persistence::thread::{ProviderSelection, ThreadSession};
use nuo_protocol::{AgentResponse, CommandResult, Provider, RoundEvent, SecretString};
use nuo_tool::{AgentNotice};

use std::sync::{Arc, RwLock};
use tokio::sync::mpsc;

use crate::composition::agent_setup::{reseed_context_policy, reseed_instructions, reseed_tool_variants};
use crate::transport::thread_view::provider_key_status;

pub(crate) mod connection;
pub(crate) mod model_ops;
pub(crate) mod oauth;
pub(crate) mod quota;

pub use connection::*;
pub use model_ops::*;
pub use oauth::*;
pub use quota::*;

#[cfg(test)]
pub use nuo_persistence::connections::Connection;
#[cfg(test)]
pub use nuo_persistence::route_settings::RouteSettingsStore;
#[cfg(test)]
pub use nuo_protocol::WireProtocol;
#[cfg(test)]
pub use nuo_protocol::model::ModelTargetScope;

pub(crate) struct ProviderEnv<'a> {
    pub config: &'a mut Config,
    pub agent: &'a Agent,
    pub provider_for_task: &'a Arc<RwLock<Arc<dyn Provider>>>,
    pub session: &'a ThreadSession,
    pub resp_tx: &'a mpsc::UnboundedSender<AgentResponse>,
    pub provider_usage: &'a mut ConnectionUsage,
}

pub(crate) struct ActivateEnv<'a> {
    pub config: &'a Config,
    pub agent: &'a Agent,
    pub provider_for_task: &'a Arc<RwLock<Arc<dyn Provider>>>,
    pub session: Option<&'a ThreadSession>,
    pub resp_tx: &'a mpsc::UnboundedSender<AgentResponse>,
    pub provider_usage: &'a mut ConnectionUsage,
}

impl<'a> From<ProviderEnv<'a>> for ActivateEnv<'a> {
    fn from(env: ProviderEnv<'a>) -> Self {
        Self {
            config: env.config,
            agent: env.agent,
            provider_for_task: env.provider_for_task,
            session: Some(env.session),
            resp_tx: env.resp_tx,
            provider_usage: env.provider_usage,
        }
    }
}

pub(crate) async fn switch(
    ProviderEnv {
        config,
        agent,
        provider_for_task,
        session,
        resp_tx,
        provider_usage,
    }: ProviderEnv<'_>,
    provider_type: String,
    model: String,
    api_key: Option<SecretString>,
    base_url: Option<String>,
) {
    let connections = Connections::load();
    // A key entered in the TUI is the connection's credential; an environment
    // variable (`api_key_env`) still wins at catalog resolution time.
    if let Some(key) = api_key
        && connections.get(&provider_type).is_some()
    {
        let mut creds = Credentials::load();
        creds.set_api_key(&provider_type, Some(key));
        if creds.save().is_err() {
            tracing::warn!("switch: could not persist credential");
        }
    }
    if let Some(url) = base_url
        && !url.trim().is_empty()
        && let Some(connection) = connections.get(&provider_type)
    {
        let mut store = ModelProviders::load();
        if let Some(prov) = store.providers.get_mut(&connection.provider) {
            prov.root_url = url.trim().to_string();
            if let Err(error) = ModelProviders::save(&store)
                .map_err(|e| e.to_string())
                .and_then(|_| crate::composition::provider_registry::sync_user_declared_providers(&store))
            {
                let _ = resp_tx.send(AgentResponse::Error(error));
                return;
            }
        }
    }

    // Set the selection on the effective config so `activate` resolves the
    // right channel; the save below persists it as the global default, and
    // the session pin (written further below) records this session's own
    // choice for exact restore on resume. The active model always lives in the
    // shared `default_model` — every instance is multi-model capable.
    config.default_connection = provider_type.clone();
    config.default_model = Some(model.clone());
    if let Err(error) = config.save() {
        tracing::warn!(?error, "could not persist provider selection");
    }
    // Pin the provider + model to this session so resume restores it exactly.
    // Best-effort: a failed pin does not block the live switch.
    if let Err(error) = session
        .set_provider_selection(Some(ProviderSelection {
            connection: provider_type.clone(),
            model: Some(model.clone()),
        }))
        .await
    {
        tracing::warn!(?error, "could not persist session provider selection");
    }
    // Pass the session through so `activate` can surface the acknowledgment
    // toast + record the ledger entry for this genuine user-initiated switch.
    activate(
        ActivateEnv {
            config,
            agent,
            provider_for_task,
            session: Some(session),
            resp_tx,
            provider_usage,
        },
        provider_type,
        model,
    )
    .await;
}

/// Re-apply the active session's provider/model pin to the live provider
/// holder (C6). Called after a session swap (`/session open`, `/session
/// resume`, `/session new`) so the live provider tracks the now-active
/// session's own pin, or falls back to the global default when the new session
/// has no pin (`None`). Builds a transient `Config` clone with the overlay so
/// the caller's immutable `&Config` is not mutated; activation writes only the
/// live holder, telemetry, and TUI snapshots — never `config.toml`.
///
/// The pin-less fallback re-reads `config.toml` from disk: multiple frontends
/// and sessions share this one server and therefore this one `Config` copy in
/// memory, but a model switch made from *another* session only persisted its
/// new global default through that session's `&mut Config` clone. Without the
/// re-read, `/new` (and any session swap into an unpinned session) would
/// re-activate the in-memory copy's stale default instead of the most
/// recently persisted cross-client default. Same for `ConnectionUsage`: its
/// in-memory copy misses the `last_models` pin another session wrote when it
/// activated its model. Parse failure falls back to the passed copies so a
/// corrupt file can never strand the swap.
pub async fn reapply_session_selection(
    config: &Config,
    agent: &Agent,
    provider_for_task: &Arc<RwLock<Arc<dyn Provider>>>,
    session: &ThreadSession,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    provider_usage: &mut ConnectionUsage,
) {
    // Overlay the session pin onto a throwaway clone so catalog resolution
    // picks the session's provider/model, not the global default.
    let mut effective = config.clone();
    let selection = session.provider_selection().await;
    let (provider_id, model_id): (String, Option<String>) = match &selection {
        Some(sel) => {
            effective.default_connection = sel.connection.clone();
            if let Some(model) = &sel.model {
                effective.default_model = Some(model.clone());
            }
            (sel.connection.clone(), sel.model.clone())
        }
        None => (
            nuo_model_routing::default_connection_id(config).to_string(),
            config.default_model.clone(),
        ),
    };
    let model = model_id.filter(|m| !m.is_empty()).unwrap_or_else(|| {
        nuo_model_routing::resolved_model_name_with_usage(&effective, &provider_id, provider_usage)
            .unwrap_or_default()
    });
    if agent.provider.provider_id() == provider_id && agent.provider.model() == model {
        // Even when the provider instance in memory does not need a rebuild,
        // announce the authoritative provider + model so any frontend whose
        // view state was displaying a previous session's pin re-anchors immediately.
        let _ = resp_tx.send(AgentResponse::ProviderSwitched {
            provider: provider_id,
            model,
        });
        return;
    }
    activate(
        ActivateEnv {
            config: &effective,
            agent,
            provider_for_task,
            session: None,
            resp_tx,
            provider_usage,
        },
        provider_id,
        model,
    )
    .await;
}

/// Record a provider switch's acknowledgment in the durable command ledger.
///
/// Routed through the slash-command funnel rather than writing the ledger
/// directly (ADR-0140): `/models` is an **act**, and the funnel is the one place
/// that decides what is durable. This path deliberately emits no `CommandResult`
/// — the switch is acknowledged by the `CommandAck` notice the caller already
/// sent, and a second reply would duplicate it.
pub(crate) async fn record_provider_ack(
    session: &ThreadSession,
    provider: &str,
    model: &str,
    ack: String,
) {
    crate::handlers::handlers_slash::record::record_act_only(
        session,
        "models",
        &format!("{provider} {model}"),
        CommandResult::Ack {
            title: ack,
            detail: None,
        },
    )
    .await;
}

/// Shared tail of [`switch`] and [`add`]: rebuild the active provider through the
/// catalog, swap it into the shared holder, re-seed mid-turn relief, and push
/// the key + picker snapshots.
pub(crate) async fn activate(
    ActivateEnv {
        config,
        agent,
        provider_for_task,
        session,
        resp_tx,
        provider_usage,
    }: ActivateEnv<'_>,
    provider_type: String,
    model: String,
) {
    // `[INV-ATTACH-03]` (ADR-0133): credential resolution is lazy and
    // single-flight at the first request. Eagerly refreshing here made a model
    // switch wait on the network.
    let session_id = agent.thread_id();
    let rules = match crate::runtime::usage_cache::shared_usage_cache().get(&provider_type) {
        Some(nuo_protocol::ConnectionUsageState::Available(usage)) => usage.model_rules.clone(),
        _ => Vec::new(),
    };
    let Some(new_p) = nuo_model_routing::build_provider_for_model_with_rules(
        config,
        &provider_type,
        Some(&model),
        session_id.as_deref(),
        &rules,
    )
    .or_else(|| nuo_model_routing::build_provider_for(config, &provider_type)) else {
        tracing::warn!(
            provider_type = %provider_type,
            model = %model,
            "activate refused: catalog or quota rule refused provider/channel",
        );
        let _ = resp_tx.send(AgentResponse::Error(format!(
            "Model '{model}' on '{provider_type}' is unavailable or unconfigured."
        )));
        // Re-push the picker so the UI reflects that nothing switched.
        let _ = resp_tx.send(AgentResponse::ProviderPicker(build_live_picker_state(
            config,
            provider_usage,
        )));
        return;
    };
    *provider_for_task
        .write()
        .unwrap_or_else(|error| error.into_inner()) = new_p;

    reseed_context_policy(agent, config);
    // The gate itself needs no re-install: its settings closure resolves the
    // window from the live provider on every projection, so the thresholds already
    // follow this switch.
    reseed_tool_variants(agent, config);
    reseed_instructions(agent, config);

    let _ = resp_tx.send(AgentResponse::ProviderKeys(provider_key_status(config)));
    provider_usage.record(&provider_type);
    provider_usage.record_model(&provider_type, &model);
    if let Err(error) = provider_usage.save() {
        tracing::warn!(?error, "could not persist model usage telemetry");
    }
    let ack = format!("Connection switched to {provider_type} ({model})");
    let _ = resp_tx.send(AgentResponse::ProviderSwitched {
        provider: provider_type.clone(),
        model: model.clone(),
    });
    if let Some(session) = session {
        let session_id = session.id().await;
        let _ = resp_tx.send(round_response(
            &session_id,
            RoundEvent::Notice(AgentNotice::command_ack(ack.clone())),
        ));
        record_provider_ack(session, &provider_type, &model, ack).await;
    }
    let _ = resp_tx.send(AgentResponse::ProviderPicker(build_live_picker_state(
        config,
        provider_usage,
    )));
}

/// Close a completed catalog-sync pass: re-derive global state, emit the command
/// acknowledgement, and publish the final provider keys and picker. Per-channel
/// changes and warnings are streamed by [`apply_connection_update`]; this only
/// settles the pass.
pub fn apply_catalog_sync_outcome(
    config: &mut Config,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    provider_usage: &mut ConnectionUsage,
    outcome: nuo_model_routing::RoutingSyncOutcome,
    session_id: Option<String>,
) {
    if outcome.changed {
        nuo_model_routing::sync_fitted_model_registry();
        nuo_model_routing::prune_stale_models(config, provider_usage);
    }

    if let Some(session_id) = session_id {
        let ack = if !outcome.failures.is_empty() && !outcome.changed {
            "Model refresh failed to reach upstream".to_string()
        } else if outcome.changed {
            "Model list updated".to_string()
        } else {
            "Model list refreshed (up to date)".to_string()
        };
        let _ = resp_tx.send(round_response(
            &session_id,
            RoundEvent::Notice(AgentNotice::command_ack(ack)),
        ));
    }

    let _ = resp_tx.send(AgentResponse::ProviderKeys(provider_key_status(config)));
    let _ = resp_tx.send(AgentResponse::ProviderPicker(build_live_picker_state(
        config,
        provider_usage,
    )));
}

pub(crate) fn build_live_picker_state(
    config: &Config,
    provider_usage: &ConnectionUsage,
) -> nuo_protocol::ProviderPickerSnapshot {
    let dynamic_rules = collect_cached_dynamic_rules();
    nuo_model_routing::build_picker_state_with_rules(config, provider_usage, &dynamic_rules)
}

#[cfg(test)]
pub(crate) mod tests;
