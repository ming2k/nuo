//! Model rules, filtering, capability overrides, and favorites.
use nuo_engine::Agent;
use nuo_persistence::config::Config;
use nuo_persistence::connection_usage::ConnectionUsage;
use nuo_persistence::connections::Connections;
use nuo_persistence::model_providers::ModelProviders;
use nuo_persistence::route_settings::RouteSettingsStore;
use nuo_protocol::model::ModelTargetScope;
use nuo_protocol::{AgentResponse, Provider};
use std::sync::{Arc, RwLock};
use tokio::sync::mpsc;

use super::{ActivateEnv, ProviderEnv, activate, build_live_picker_state};

/// `AgentRequest::IncludeModel` — declare or include a model within a target scope (preset or connection).
pub(crate) async fn include_model(
    config: &mut Config,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    provider_usage: &mut ConnectionUsage,
    scope: ModelTargetScope,
    model: nuo_protocol::model::DeclaredModel,
) {
    match scope {
        ModelTargetScope::Provider(provider_id) => {
            let mut providers = ModelProviders::load();
            let provider = providers.get_or_create_mut(&provider_id);
            provider.exclude.retain(|id| id != &model.id);
            if let Some(pos) = provider.include.iter().position(|m| m.id == model.id) {
                provider.include[pos] = model;
            } else {
                provider.include.push(model);
            }
            if ModelProviders::save(&providers).is_err() {
                tracing::warn!("include_model: could not persist model_providers.toml");
                return;
            }
        }
        ModelTargetScope::Connection(connection_id) => {
            let mut connections = Connections::load();
            let Some(conn) = connections.get_mut(&connection_id) else {
                tracing::warn!(%connection_id, "include_model: unknown connection");
                return;
            };
            conn.models.exclude.retain(|id| id != &model.id);
            if let Some(pos) = conn.models.include.iter().position(|m| m.id == model.id) {
                conn.models.include[pos] = model;
            } else {
                conn.models.include.push(model);
            }
            if connections.save().is_err() {
                tracing::warn!("include_model: could not persist connections.toml");
                return;
            }
        }
    }
    nuo_model_routing::prune_stale_models(config, provider_usage);
    let _ = resp_tx.send(AgentResponse::ProviderPicker(build_live_picker_state(
        config,
        provider_usage,
    )));
}

/// `AgentRequest::ExcludeModel` — exclude/hide a model within a target scope.
pub(crate) async fn exclude_model(
    config: &mut Config,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    provider_usage: &mut ConnectionUsage,
    scope: ModelTargetScope,
    model_id: String,
) {
    match scope {
        ModelTargetScope::Provider(provider_id) => {
            let mut providers = ModelProviders::load();
            let provider = providers.get_or_create_mut(&provider_id);
            provider.include.retain(|m| m.id != model_id);
            if !provider.exclude.contains(&model_id) {
                provider.exclude.push(model_id);
            }
            if ModelProviders::save(&providers).is_err() {
                tracing::warn!("exclude_model: could not persist model_providers.toml");
                return;
            }
        }
        ModelTargetScope::Connection(connection_id) => {
            let mut connections = Connections::load();
            let Some(conn) = connections.get_mut(&connection_id) else {
                tracing::warn!(%connection_id, "exclude_model: unknown connection");
                return;
            };
            conn.models.include.retain(|m| m.id != model_id);
            if !conn.models.exclude.contains(&model_id) {
                conn.models.exclude.push(model_id);
            }
            if connections.save().is_err() {
                tracing::warn!("exclude_model: could not persist connections.toml");
                return;
            }
        }
    }
    nuo_model_routing::prune_stale_models(config, provider_usage);
    let _ = resp_tx.send(AgentResponse::ProviderPicker(build_live_picker_state(
        config,
        provider_usage,
    )));
}

/// `AgentRequest::ClearModelRule` — clear explicit inclusion, exclusion, or overrides for a model.
pub(crate) async fn clear_model_rule(
    config: &mut Config,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    provider_usage: &mut ConnectionUsage,
    scope: ModelTargetScope,
    model_id: String,
) {
    match scope {
        ModelTargetScope::Provider(provider_id) => {
            let mut providers = ModelProviders::load();
            if let Some(provider) = providers.model_providers.get_mut(&provider_id) {
                provider.include.retain(|m| m.id != model_id);
                provider.exclude.retain(|m| m != &model_id);
                provider.overrides.remove(&model_id);
                if ModelProviders::save(&providers).is_err() {
                    tracing::warn!("clear_model_rule: could not persist model_providers.toml");
                    return;
                }
            }
        }
        ModelTargetScope::Connection(connection_id) => {
            let mut connections = Connections::load();
            if let Some(conn) = connections.get_mut(&connection_id) {
                conn.models.include.retain(|m| m.id != model_id);
                conn.models.exclude.retain(|m| m != &model_id);
                conn.models.overrides.remove(&model_id);
                if connections.save().is_err() {
                    tracing::warn!("clear_model_rule: could not persist connections.toml");
                    return;
                }
            }
        }
    }
    nuo_model_routing::prune_stale_models(config, provider_usage);
    let _ = resp_tx.send(AgentResponse::ProviderPicker(build_live_picker_state(
        config,
        provider_usage,
    )));
}

/// `AgentRequest::SetModelCapabilities` — update per-scope model capability overrides.
pub(crate) async fn set_model_capabilities(
    config: &mut Config,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    provider_usage: &mut ConnectionUsage,
    scope: ModelTargetScope,
    model_id: String,
    overrides: nuo_protocol::model::CapabilityOverrides,
) {
    match scope {
        ModelTargetScope::Provider(provider_id) => {
            let mut providers = ModelProviders::load();
            let provider = providers.get_or_create_mut(&provider_id);
            if overrides.is_empty() {
                provider.overrides.remove(&model_id);
            } else {
                provider.overrides.insert(model_id, overrides);
            }
            if ModelProviders::save(&providers).is_err() {
                tracing::warn!("set_model_capabilities: could not persist model_providers.toml");
                return;
            }
        }
        ModelTargetScope::Connection(connection_id) => {
            let mut connections = Connections::load();
            if let Some(conn) = connections.get_mut(&connection_id) {
                if overrides.is_empty() {
                    conn.models.overrides.remove(&model_id);
                } else {
                    conn.models.overrides.insert(model_id, overrides);
                }
                if connections.save().is_err() {
                    tracing::warn!("set_model_capabilities: could not persist connections.toml");
                    return;
                }
            }
        }
    }
    let _ = resp_tx.send(AgentResponse::ProviderPicker(build_live_picker_state(
        config,
        provider_usage,
    )));
}

/// `AgentRequest::EditProviderModel` — update the per-(connection, model)
/// reasoning overrides in the catalog cache. Connection metadata (name /
/// endpoint / credential) is untouched.
pub(crate) async fn edit_model(
    ProviderEnv {
        config,
        agent,
        provider_for_task,
        resp_tx,
        provider_usage,
        ..
    }: ProviderEnv<'_>,
    connection: String,
    model: String,
    effort: Option<String>,
    thinking: Option<bool>,
    overrides: Option<nuo_protocol::CapabilityOverrides>,
) {
    let valid_effort = effort.and_then(|e| {
        let t = e.trim();
        (!t.is_empty())
            .then(|| t.to_ascii_lowercase())
            .filter(|s| nuo_protocol::effort::Effort::parse(s).is_some())
    });

    // Guard the edit against a route that cannot be derived at all (an
    // unknown connection, or a model the surface does not serve). The derived
    // channel itself is not consulted: which knobs apply is the *provider's*
    // declaration, and the request boundary already ignores a knob a route does
    // not expose (ADR-0126). This is a validity check, not a knob selector.
    let stores = nuo_model_routing::Stores::load();
    let Some(conn) = stores.connections.get(&connection) else {
        return;
    };
    if let Err(error) = nuo_model_routing::derive_channel(conn, &model, &stores.inputs()) {
        let _ = resp_tx.send(AgentResponse::Error(error.to_string()));
        return;
    }

    let mut routes = RouteSettingsStore::load();
    let entry = routes.settings_for_mut(&connection, &model);
    // Depth and the thinking switch are written into the route's open tuning
    // map, keyed by the framework's own knob ids. There is no per-protocol
    // `match` here: whether a knob applies is the *provider's* declaration
    // (`Model::effort_levels` / `Model::thinking`), and the request
    // boundary already ignores a knob a route does not expose. Clearing the
    // switch on a route that has none is therefore unnecessary — and the old
    // `match` on `Transport` was exactly the pipe-versus-setting conflation
    // ADR-0126 removes.
    entry.tunings.set_effort(
        valid_effort
            .as_deref()
            .and_then(nuo_protocol::effort::Effort::parse),
    );
    entry.tunings.set_thinking(thinking);
    // Capability overrides (layer 1): `None` keeps the stored
    // record untouched; `Some(record)` replaces it wholesale (empty clears).
    if let Some(record) = overrides {
        entry.capability_overrides = (!record.is_empty()).then_some(record);
    }
    if entry.is_empty() {
        routes.remove(&connection, &model);
    }
    if routes.save().is_err() {
        tracing::warn!("edit_model: could not persist route settings");
    }

    let active_model =
        nuo_model_routing::resolved_model_name_with_usage(config, &connection, provider_usage)
            .unwrap_or_default();
    if config.default_connection.eq_ignore_ascii_case(&connection) && active_model == model {
        activate(
            ActivateEnv {
                config,
                agent,
                provider_for_task,
                session: None,
                resp_tx,
                provider_usage,
            },
            connection,
            model,
        )
        .await;
    } else {
        let _ = resp_tx.send(AgentResponse::ProviderPicker(build_live_picker_state(
            config,
            provider_usage,
        )));
    }
}

/// `AgentRequest::EditModelReasoning` — update the per-(connection, model)
/// reasoning overrides for the currently active connection. Serves the model
/// `e` editor for any model; the setting is scoped to the connection that
/// actually serves it (a model id can be served by more than one connection).
/// If the edited model is the active one, the live provider is re-activated
/// so the new settings take effect at once.
pub(crate) async fn edit_model_reasoning(
    ProviderEnv {
        config,
        agent,
        provider_for_task,
        resp_tx,
        provider_usage,
        ..
    }: ProviderEnv<'_>,
    model: String,
    effort: Option<String>,
    thinking: Option<bool>,
    overrides: Option<nuo_protocol::CapabilityOverrides>,
) {
    let valid_effort = effort.and_then(|e| {
        let t = e.trim();
        (!t.is_empty())
            .then(|| t.to_ascii_lowercase())
            .filter(|s| nuo_protocol::effort::Effort::parse(s).is_some())
    });

    let provider_id = config.default_connection.clone();
    let mut routes = RouteSettingsStore::load();
    let entry = routes.settings_for_mut(&provider_id, &model);
    entry.tunings.set_effort(
        valid_effort
            .as_deref()
            .and_then(nuo_protocol::effort::Effort::parse),
    );
    entry.tunings.set_thinking(thinking);
    if let Some(record) = overrides {
        entry.capability_overrides = (!record.is_empty()).then_some(record);
    }
    if entry.is_empty() {
        routes.remove(&provider_id, &model);
    }
    if routes.save().is_err() {
        tracing::warn!("edit_model_reasoning: could not persist route settings");
    }

    // Re-activate if this model is the live one so the change applies now.
    let active_model =
        nuo_model_routing::resolved_model_name_with_usage(config, &provider_id, provider_usage)
            .unwrap_or_default();
    if active_model == model {
        activate(
            ActivateEnv {
                config,
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
    } else {
        let _ = resp_tx.send(AgentResponse::ProviderPicker(build_live_picker_state(
            config,
            provider_usage,
        )));
    }
}

/// `AgentRequest::ToggleFavorite` — flip the model id in the favorites list.
pub async fn toggle_favorite(
    config: &mut Config,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    provider_usage: &ConnectionUsage,
    id: String,
) {
    if let Some(pos) = config.favorites.iter().position(|fav| *fav == id) {
        config.favorites.remove(pos);
    } else {
        config.favorites.push(id.clone());
    }
    if let Err(error) = config.save_preserving_connection_selection() {
        tracing::warn!(?error, "could not persist favorites");
    }
    let _ = resp_tx.send(AgentResponse::ProviderPicker(build_live_picker_state(
        config,
        provider_usage,
    )));
}

/// `AgentRequest::SetDefaultModel` — make `id` the default AND activate it.
pub async fn set_default_model(
    config: &mut Config,
    agent: &Agent,
    provider_for_task: &Arc<RwLock<Arc<dyn Provider>>>,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    provider_usage: &mut ConnectionUsage,
    id: String,
) {
    let stores = nuo_model_routing::Stores::load();
    let entries = nuo_model_routing::derive_entries(&stores.connections, &stores.inputs());
    let current_provider_id = config.default_connection.clone();
    let current_offers = entries
        .iter()
        .find(|e| e.id == current_provider_id)
        .is_some_and(|e| e.offers_model(&id));
    let provider_id = if current_offers {
        current_provider_id
    } else {
        let Some(first_match) = entries.iter().find(|e| e.offers_model(&id)) else {
            tracing::warn!(model = %id, "set_default_model: model is not served by any connection");
            return;
        };
        first_match.id.clone()
    };

    config.default_connection = provider_id.clone();
    config.default_model = Some(id.clone());
    if let Err(error) = config.save() {
        tracing::warn!(?error, "could not persist default model");
    }

    activate(
        ActivateEnv {
            config,
            agent,
            provider_for_task,
            session: None,
            resp_tx,
            provider_usage,
        },
        provider_id,
        id,
    )
    .await;
}

pub(crate) fn collect_cached_dynamic_rules()
-> std::collections::HashMap<String, Vec<nuo_protocol::ModelAvailabilityRule>> {
    let mut map = std::collections::HashMap::new();
    let cache = crate::runtime::usage_cache::shared_usage_cache();
    let stores = nuo_model_routing::Stores::load();
    for conn in &stores.connections.connections {
        if let Some(nuo_protocol::ConnectionUsageState::Available(usage)) = cache.get(&conn.name) {
            if !usage.model_rules.is_empty() {
                map.insert(conn.name.clone(), usage.model_rules.clone());
                map.entry(conn.provider.clone())
                    .or_insert_with(|| usage.model_rules.clone());
            }
        }
    }
    map
}
