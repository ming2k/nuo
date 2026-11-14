//! Provider connection CRUD: add, edit, rename, delete, and registration.
use nuo_engine::orchestration::round_response;
use nuo_persistence::config::{Config, Credentials, RemoteCatalogCache};
use nuo_persistence::connection_usage::ConnectionUsage;
use nuo_persistence::connections::{Connection, Connections};
use nuo_persistence::model_providers::ModelProviders;
use nuo_persistence::route_settings::RouteSettingsStore;
use nuo_protocol::{AgentResponse, RoundEvent, SecretString, WireProtocol};
use nuo_tool::{AgentNotice};

use tokio::sync::mpsc;

use super::{
    ActivateEnv, PendingOAuthAuthorization, ProviderEnv, activate, build_live_picker_state,
};
use crate::transport::thread_view::provider_key_status;
use nuo_persistence::thread::ProviderSelection;

pub(crate) struct AddConnectionParams {
    pub name: String,
    pub provider: String,
    pub api_key: SecretString,
    pub models: Vec<String>,
    pub auth: nuo_protocol::ConnectionAuth,
    pub headers: Option<std::collections::BTreeMap<String, String>>,
    pub budget: nuo_protocol::ConnectionBudget,
    pub entitlement: Option<nuo_protocol::Entitlement>,
}

/// Register a user-declared model provider surface.
/// Request-specific payload for [`register_provider`]: the declared surface of
/// a user-registered model provider.
pub(crate) struct RegisterProviderParams {
    pub id: String,
    pub label: Option<String>,
    pub root_url: String,
    pub protocol: Option<WireProtocol>,
    pub user_agent: Option<String>,
    pub headers: Option<std::collections::BTreeMap<String, String>>,
    pub catalog_format: Option<String>,
    pub dialect: Option<String>,
}

pub(crate) fn register_provider(params: RegisterProviderParams) -> Result<(), String> {
    let RegisterProviderParams {
        id,
        label,
        root_url,
        protocol,
        user_agent,
        headers,
        catalog_format,
        dialect,
    } = params;
    let mut store = ModelProviders::try_load()?;
    let existing = store.get_provider(&id).cloned();
    let protocol_roots = store
        .get_provider(&id)
        .map(|provider| provider.protocol_roots.clone())
        .unwrap_or_default();
    store.set_provider(
        &id,
        nuo_persistence::model_providers::UserDeclaredProvider {
            label,
            root_url,
            default_protocol: protocol,
            user_agent,
            headers,
            catalog: catalog_format
                .map(|format| {
                    if matches!(format.as_str(), "none" | "static") {
                        Ok(nuo_protocol::RemoteCatalogSource::None)
                    } else {
                        serde_json::from_value(serde_json::Value::String(format))
                            .map(nuo_protocol::RemoteCatalogSource::Endpoint)
                            .map_err(|e| e.to_string())
                    }
                })
                .transpose()?,
            dialect: dialect.map(|value| value.parse()).transpose()?,
            protocol_roots,
            catalog_root_url: existing.as_ref().and_then(|p| p.catalog_root_url.clone()),
            prompt_cache: existing.as_ref().and_then(|p| p.prompt_cache.clone()),
        },
    );
    ModelProviders::save(&store).map_err(|e| e.to_string())?;
    crate::composition::provider_registry::sync_user_declared_providers(&store)?;
    Ok(())
}

/// `AgentRequest::AddConnection` — create a connection to a model provider,
/// persist it to the state store, set its credential, then activate it. For
/// OAuth providers the TUI runs [`authorize`] first, then calls this with
/// `auth` set.
pub(crate) async fn add(
    ProviderEnv {
        config,
        agent,
        provider_for_task,
        session,
        resp_tx,
        provider_usage,
    }: ProviderEnv<'_>,
    params: AddConnectionParams,
    pending_authorization: Option<PendingOAuthAuthorization>,
) {
    let AddConnectionParams {
        name,
        provider,
        api_key,
        models,
        auth,
        headers,
        budget,
        entitlement: explicit_entitlement,
    } = params;
    let mut connections = Connections::load();
    // The name IS the connection's identity (INV-3): reject a
    // duplicate with a suggested alternative instead of silently renaming it.
    let name = match connections.check_new_name(&name) {
        Ok(name) => name,
        Err(reason) => {
            reject(resp_tx, "Could not add connection", &reason);
            return;
        }
    };
    // The provider must resolve (INV-2): an unknown value is a hard
    // error and never degrades into a different connection kind.
    let Some(spec) = crate::composition::provider_registry::model_provider_spec(&provider) else {
        reject(
            resp_tx,
            "Could not add connection",
            &format!("unknown model provider '{provider}'"),
        );
        return;
    };
    let provider = spec.id.to_string();
    let is_open_universe =
        spec.baselines.is_empty() && spec.catalog_source == nuo_protocol::RemoteCatalogSource::None;
    let trimmed_key = api_key.expose_secret().trim();
    // Pasted API key on a negotiated provider → ordinary Static auth.
    let auth = match (auth, !trimmed_key.is_empty()) {
        (a, true) if a.is_negotiated() => nuo_protocol::ConnectionAuth::Static,
        (other, _) => other,
    };
    // Sanitized declared model ids. A curated provider owns its model universe,
    // so the list is an initial inclusion set; an open-universe provider declares its own.
    let declared_models: Vec<String> = models
        .iter()
        .map(|m| nuo_protocol::sanitize_model_id(m))
        .filter(|m| !m.is_empty())
        .collect();
    // An open-universe provider must declare at least one model — nothing else supplies them.
    if is_open_universe && declared_models.is_empty() {
        reject(
            resp_tx,
            "Could not add connection",
            "a custom provider without baseline models must declare at least one model",
        );
        return;
    }
    let active_model = declared_models
        .first()
        .cloned()
        .or_else(|| spec.models.first().map(|m| (*m).to_string()))
        .unwrap_or_default();

    // Pre-connection authorization is a single-use, session-local value. It
    // never enters the global store under a generic provider namespace.
    let pending_tokens = if auth.is_negotiated() {
        match pending_authorization {
            Some(pending) if pending.auth == auth && pending.tokens.is_valid() => {
                Some(pending.tokens)
            }
            _ => {
                let _ = resp_tx.send(AgentResponse::ConnectStatus(
                    nuo_protocol::ConnectStatus::Failed {
                        provider: name.clone(),
                        message: "OAuth authorization is missing or invalid; authorize this connection again"
                            .to_string(),
                    },
                ));
                return;
            }
        }
    } else {
        None
    };

    // Step 1: Write credentials to disk FIRST so a visible connection never exists without credentials.
    let mut stored_oauth = false;
    if let Some(tokens) = pending_tokens {
        let mut store = match crate::composition::credentials_host::store().lock().await {
            Ok(store) => store,
            Err(error) => {
                let _ = resp_tx.send(AgentResponse::ConnectStatus(
                    nuo_protocol::ConnectStatus::Failed {
                        provider: name.clone(),
                        message: format!("could not lock OAuth credential store: {error}"),
                    },
                ));
                return;
            }
        };
        store.set(&name, tokens);
        if let Err(error) = store.commit().await {
            let _ = resp_tx.send(AgentResponse::ConnectStatus(
                nuo_protocol::ConnectStatus::Failed {
                    provider: name.clone(),
                    message: format!("could not persist OAuth credentials: {error}"),
                },
            ));
            return;
        }
        stored_oauth = true;
    }

    let mut stored_api_key = false;
    if auth == nuo_protocol::ConnectionAuth::Static && !trimmed_key.is_empty() {
        let mut creds = Credentials::load();
        creds.set_api_key(&name, Some(SecretString::from(trimmed_key)));
        let save_err = creds.save().err().map(|e| e.to_string());
        if let Some(error_msg) = save_err {
            if stored_oauth && let Ok(mut store) = crate::composition::credentials_host::store().lock().await {
                store.remove(&name);
                let _ = store.commit().await;
            }
            let _ = resp_tx.send(AgentResponse::ConnectStatus(
                nuo_protocol::ConnectStatus::Failed {
                    provider: name.clone(),
                    message: format!("could not persist API key: {error_msg}"),
                },
            ));
            return;
        }
        stored_api_key = true;
    }

    // Snapshot the provider's valve policy at creation time. Curated baseline
    // ids remain derived from the provider spec and are never frozen into the
    // connection; only explicit filter/inject/block rules are copied.
    let provider_rules = ModelProviders::load()
        .get(&provider)
        .cloned()
        .unwrap_or_default();
    let mut model_rules = nuo_protocol::model::ModelScopeConfig {
        filter: provider_rules.filter.or_else(|| {
            Some(nuo_protocol::ConnectionFilterPolicy::Named(
                if spec.catalog_source == nuo_protocol::RemoteCatalogSource::None {
                    nuo_protocol::NamedFilterPolicy::Baseline
                } else {
                    nuo_protocol::NamedFilterPolicy::All
                },
            ))
        }),
        include: provider_rules.include,
        exclude: provider_rules.exclude,
        // Provider capability overrides stay in their own cascade layer.
        overrides: std::collections::BTreeMap::new(),
    };
    if is_open_universe {
        for id in declared_models {
            if !model_rules.include.iter().any(|model| model.id == id) {
                model_rules
                    .include
                    .push(nuo_protocol::model::DeclaredModel {
                        id,
                        ..Default::default()
                    });
            }
        }
    }

    // Step 2: Publish connection to Connections store (purified pipe).
    let entitlement = explicit_entitlement.unwrap_or_else(|| {
        if auth.is_negotiated() {
            nuo_protocol::Entitlement::FlatRate
        } else {
            nuo_protocol::Entitlement::Metered
        }
    });
    let connection = Connection {
        name: name.clone(),
        provider: provider.clone(),
        auth: auth.clone(),
        api_key_env: None,
        _legacy_client_identity: None,
        headers: headers.unwrap_or_default(),
        models: model_rules,
        catalog_dimensions: Default::default(),
        budget,
        entitlement,
    };
    connections.connections.push(connection);
    let conn_save_err = connections.save().err().map(|e| e.to_string());
    if let Some(error_msg) = conn_save_err {
        tracing::error!(%error_msg, "add: could not persist connection; rolling back credentials");
        if stored_oauth && let Ok(mut store) = crate::composition::credentials_host::store().lock().await {
            store.remove(&name);
            let _ = store.commit().await;
        }
        if stored_api_key {
            let mut creds = Credentials::load();
            creds.remove_api_key(&name);
            let _ = creds.save();
        }
        let _ = resp_tx.send(AgentResponse::ConnectStatus(
            nuo_protocol::ConnectStatus::Failed {
                provider: name.clone(),
                message: format!("could not persist connection store: {error_msg}"),
            },
        ));
        return;
    }

    // Step 3: activating a connection is a *model-switch* concern, not a
    // connection-management concern: `/connections` adds a
    // connection to the store, it never hijacks the session's live model. Only
    // two bootstrap cases may activate here:
    //   * the session has no usable provider at all (the `NoProvider` sentinel
    //     from startup) — the first added connection is the only way to get a
    //     working channel, so it steps in;
    //   * no default connection is configured yet (`default_connection` empty).
    // Everything else leaves the current selection untouched: the user picks a
    // model explicitly via `/models` (`SwitchConnection` / `SetDefaultModel`).
    let no_live_provider = nuo_engine::NoProvider::is(
        &**provider_for_task
            .read()
            .unwrap_or_else(|error| error.into_inner()),
    );
    let no_default_configured = config.default_connection.trim().is_empty();
    if !no_live_provider && !no_default_configured {
        // Still record the new connection as *known-good* telemetry for the
        // picker (its models appear in the usage-recency ordering) without
        // touching the live provider, the session pin, or the global default.
        provider_usage.record(&name);
        if let Err(error) = provider_usage.save() {
            tracing::warn!(?error, "add: could not persist model usage telemetry");
        }
        let ack = format!(
            "Connection '{name}' added. Switch to it with /models when you want to use it."
        );
        let _ = resp_tx.send(AgentResponse::ProviderKeys(provider_key_status(config)));
        let session_id = session.id().await;
        let _ = resp_tx.send(round_response(
            &session_id,
            RoundEvent::Notice(AgentNotice::command_ack(ack)),
        ));
        let _ = resp_tx.send(AgentResponse::ProviderPicker(build_live_picker_state(
            config,
            provider_usage,
        )));
        return;
    }

    config.default_connection = name.clone();
    config.default_model = Some(active_model.clone());
    if let Err(error) = config.save() {
        tracing::warn!(?error, "add: could not persist selection");
    }
    // Pin the newly-added connection to this session only in the bootstrap
    // case where the add IS the activation (see the guard above): a session
    // that already runs a model keeps its own pin untouched.
    if let Err(error) = session
        .set_provider_selection(Some(ProviderSelection {
            connection: name.clone(),
            model: Some(active_model.clone()),
        }))
        .await
    {
        tracing::warn!(?error, "could not persist session provider selection");
    }
    // For OAuth providers, run live catalog sync right away so the picker
    // shows the account's real entitlements immediately rather than the seed
    // list. A failure keeps the seed; each failure is reported back as a
    // warning so the user knows the list may be incomplete.
    if auth.is_negotiated() && auth.negotiated_provider() != Some("google-antigravity") {
        let outcome = nuo_model_routing::sync_connection_routes(&name).await;
        if outcome.changed {
            nuo_model_routing::sync_fitted_model_registry();
            nuo_model_routing::prune_stale_models_on_disk();
        }
        for failure in &outcome.failures {
            let _ = resp_tx.send(AgentResponse::ConnectStatus(
                nuo_protocol::ConnectStatus::CatalogSyncWarning {
                    provider: failure.connection.clone(),
                    message: failure.message.clone(),
                    kind: if failure.refused {
                        nuo_protocol::CatalogSyncFailure::Refused
                    } else {
                        nuo_protocol::CatalogSyncFailure::Transient
                    },
                },
            ));
        }
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
        name,
        active_model,
    )
    .await;
}

/// Refuse a connection request with a user-visible error. The add / edit /
/// rename handlers reject rather than silently degrading or renaming
/// (INV-2, INV-3).
fn reject(resp_tx: &mpsc::UnboundedSender<AgentResponse>, title: &str, reason: &str) {
    let _ = resp_tx.send(AgentResponse::Error(format!("{title}: {reason}")));
}

/// `AgentRequest::EditConnection` — update a connection's model provider,
/// endpoint override, credential, and client identity in place. Keyed by
/// `name`; renaming is the separate [`rename`] transaction.
pub(crate) async fn edit(
    ProviderEnv {
        config,
        agent,
        provider_for_task,
        resp_tx,
        provider_usage,
        ..
    }: ProviderEnv<'_>,
    name: String,
    provider: String,
    api_key: SecretString,
    headers: Option<std::collections::BTreeMap<String, String>>,
    budget: Option<nuo_protocol::ConnectionBudget>,
    entitlement: Option<nuo_protocol::Entitlement>,
) {
    let mut connections = Connections::load();
    let trimmed_key = api_key.expose_secret().trim();
    // The provider must resolve (INV-2).
    let Some(spec) = crate::composition::provider_registry::model_provider_spec(&provider) else {
        reject(
            resp_tx,
            "Could not edit connection",
            &format!("unknown model provider '{provider}'"),
        );
        return;
    };
    let Some(instance) = connections.get_mut(&name) else {
        reject(
            resp_tx,
            "Could not edit connection",
            &format!("no connection named '{name}'"),
        );
        return;
    };
    instance.provider = spec.id.to_string();
    if let Some(h) = headers {
        instance.headers = h;
    }
    if let Some(b) = budget {
        instance.budget = b;
    }
    if let Some(ent) = entitlement {
        instance.entitlement = ent;
    }
    if !instance.auth.is_negotiated() && !trimmed_key.is_empty() {
        let mut creds = Credentials::load();
        creds.set_api_key(&name, Some(SecretString::from(trimmed_key)));
        if creds.save().is_err() {
            tracing::warn!("edit: could not persist credential");
        }
    }
    if connections.save().is_err() {
        tracing::warn!("edit: could not persist connection");
    }
    nuo_model_routing::prune_stale_models(config, provider_usage);
    // Only rebuild the live provider when editing the active one (so a new
    // endpoint/key takes effect); editing an inactive provider just refreshes
    // the persisted state + the picker snapshot without switching.
    if config.default_connection.eq_ignore_ascii_case(&name) {
        let model =
            nuo_model_routing::resolved_model_name_with_usage(config, &name, provider_usage)
                .unwrap_or_default();
        activate(
            ActivateEnv {
                config,
                agent,
                provider_for_task,
                session: None,
                resp_tx,
                provider_usage,
            },
            name,
            model,
        )
        .await;
    } else {
        let _ = resp_tx.send(AgentResponse::ProviderKeys(provider_key_status(config)));
        let _ = resp_tx.send(AgentResponse::ProviderPicker(build_live_picker_state(
            config,
            provider_usage,
        )));
    }
}

/// `AgentRequest::RenameConnection` — rename a connection, rewriting every hard
/// join key in one transaction (INV-4): `credentials.toml`,
/// `auth.toml`, and `config.toml`'s `default_connection`.
///
/// Every *live* store keyed by the connection name is re-keyed too — the
/// catalog cache (whose ETag validator is name-independent, so it stays valid),
/// usage recency, and per-route effort/thinking settings. None of these expire
/// on their own: leaving them under the old name would strand a live validator,
/// reset the user's own route settings to defaults, and leave orphan entries no
/// code ever reads again.
///
/// Historical session and telemetry *records* are the one exception — they keep
/// the name they were written under rather than being rewritten retroactively,
/// because a past event happened under the name that was true at the time.
pub(crate) async fn rename(
    ProviderEnv {
        config,
        agent,
        provider_for_task,
        resp_tx,
        provider_usage,
        ..
    }: ProviderEnv<'_>,
    from: String,
    to: String,
) {
    let mut connections = Connections::load();
    if connections.get(&from).is_none() {
        reject(
            resp_tx,
            "Could not rename connection",
            &format!("no connection named '{from}'"),
        );
        return;
    }
    let trimmed_to = to.trim();
    if trimmed_to.is_empty() {
        reject(resp_tx, "Could not rename connection", "a name is required");
        return;
    }
    // A case-only rename targets the same connection and is always allowed.
    let same_connection = from.eq_ignore_ascii_case(trimmed_to);
    if !same_connection && connections.contains(trimmed_to) {
        let suggestion = connections.suggest_name(trimmed_to);
        reject(
            resp_tx,
            "Could not rename connection",
            &format!("a connection named '{trimmed_to}' already exists; try '{suggestion}'"),
        );
        return;
    }
    let new_name = trimmed_to.to_string();

    // Stage every hard join key before writing anything.
    let mut creds = Credentials::load();
    let mut locked_auth = match crate::composition::credentials_host::store().lock().await {
        Ok(store) => store,
        Err(error) => {
            reject(
                resp_tx,
                "Could not rename connection",
                &format!("could not lock the OAuth credential store: {error}"),
            );
            return;
        }
    };
    let carried_key = creds.api_key(&from).cloned();
    let carried_tokens = locked_auth.remove(&from);
    let was_default = config.default_connection.eq_ignore_ascii_case(&from);
    let Some(instance) = connections.get_mut(&from) else {
        unreachable!("existence checked above");
    };
    instance.name = new_name.clone();
    if was_default {
        config.default_connection = new_name.clone();
    }

    // Persist in dependency order, rolling back the earlier writes on failure
    // so no join key is left pointing at the old name.
    if let Some(key) = carried_key.clone() {
        creds.set_api_key(&new_name, Some(key));
    } else {
        creds.remove_api_key(&new_name);
    }
    if let Some(tokens) = carried_tokens.clone() {
        locked_auth.set(&new_name, tokens);
    }
    let mut failure: Option<String> = None;
    if let Err(error) = creds.save() {
        failure = Some(format!("could not persist credentials.toml: {error}"));
    }
    if failure.is_none()
        && let Err(error) = locked_auth.commit().await
    {
        failure = Some(format!("could not persist auth.toml: {error}"));
    }
    if failure.is_none()
        && let Err(error) = connections.save()
    {
        failure = Some(format!("could not persist connections.toml: {error}"));
    }
    if failure.is_none()
        && was_default
        && let Err(error) = config.save()
    {
        failure = Some(format!("could not persist config.toml: {error}"));
    }

    if let Some(reason) = failure {
        // Roll back the in-memory stores and the name so a retry is clean.
        if let Some(instance) = connections.get_mut(&new_name) {
            instance.name = from.clone();
        }
        if let Some(tokens) = carried_tokens {
            locked_auth.remove(&new_name);
            locked_auth.set(&from, tokens);
        }
        let _ = locked_auth.commit().await;
        let mut rollback_creds = Credentials::load();
        rollback_creds.remove_api_key(&new_name);
        if let Some(key) = carried_key {
            rollback_creds.set_api_key(&from, Some(key));
        }
        let _ = rollback_creds.save();
        let _ = connections.save();
        if was_default {
            config.default_connection = from.clone();
            let _ = config.save();
        }
        reject(resp_tx, "Could not rename connection", &reason);
        return;
    }

    // Re-key every live name-keyed store to the new name. This runs only after
    // the hard join keys persisted successfully, so a rolled-back rename never
    // strands these entries under a name that no longer exists.
    if let Err(error) =
        RemoteCatalogCache::modify(|cache| cache.rename_connection(&from, &new_name)).await
    {
        tracing::warn!(?error, connection = %new_name, "could not re-key the catalog cache on rename");
    }
    provider_usage.rename_connection(&from, &new_name);
    if let Err(error) = provider_usage.save() {
        tracing::warn!(?error, connection = %new_name, "could not persist usage recency on rename");
    }
    let mut routes = RouteSettingsStore::load();
    routes.rename_connection(&from, &new_name);
    if routes.save().is_err() {
        tracing::warn!(
            connection = %new_name,
            "could not persist route settings on rename"
        );
    }

    nuo_model_routing::prune_stale_models(config, provider_usage);
    if was_default {
        let model =
            nuo_model_routing::resolved_model_name_with_usage(config, &new_name, provider_usage)
                .unwrap_or_default();
        activate(
            ActivateEnv {
                config,
                agent,
                provider_for_task,
                session: None,
                resp_tx,
                provider_usage,
            },
            new_name,
            model,
        )
        .await;
    } else {
        let _ = resp_tx.send(AgentResponse::ProviderKeys(provider_key_status(config)));
        let _ = resp_tx.send(AgentResponse::ProviderPicker(build_live_picker_state(
            config,
            provider_usage,
        )));
    }
}

/// `AgentRequest::DeleteProvider` — remove a connection entirely: drop
/// it from the connection store, its credential, its catalog-cache records,
/// and its OAuth tokens, and prune its model ids from favorites. When the
/// deleted connection was the active one, fall back to the effective default and
/// re-activate so the live provider never points at a removed entry.
pub(crate) async fn delete(
    ProviderEnv {
        config,
        agent,
        provider_for_task,
        resp_tx,
        provider_usage,
        ..
    }: ProviderEnv<'_>,
    id: String,
) {
    let mut connections = Connections::load();
    let Some(deleted) = connections.remove(&id) else {
        return;
    };
    let _ = deleted;
    if connections.save().is_err() {
        tracing::warn!("delete: could not persist connection store");
    }
    // Clean up the credential and any OAuth tokens stored for this connection.
    let mut creds = Credentials::load();
    creds.remove_api_key(&id);
    if creds.save().is_err() {
        tracing::warn!("delete: could not persist credentials");
    }
    match crate::composition::credentials_host::store().lock().await {
        Ok(mut auth_store) => {
            if auth_store.remove(&id).is_some()
                && let Err(error) = auth_store.commit().await
            {
                tracing::error!(?error, connection_id = %id, "could not remove OAuth credential");
            }
        }
        Err(error) => {
            tracing::error!(?error, connection_id = %id, "could not open OAuth credential store");
        }
    }
    if let Err(error) = RemoteCatalogCache::modify(|cache| cache.remove_connection(&id)).await {
        tracing::warn!(?error, connection_id = %id, "could not persist catalog cache on delete");
    }
    // The deleted connection's route settings go with it (state, not cache).
    let mut routes = RouteSettingsStore::load();
    routes.retain_connection_except(&id);
    if routes.save().is_err() {
        tracing::warn!("delete: could not persist route settings");
    }
    nuo_model_routing::prune_stale_models(config, provider_usage);

    let was_active = config.default_connection == id;
    if was_active {
        config.default_connection = nuo_model_routing::effective_default_connection_id(
            config,
            &nuo_model_routing::Stores::load(),
        );
        config.default_model = None;
    }
    if let Err(error) = config.save_preserving_connection_selection() {
        tracing::warn!(?error, "could not persist deleted connection");
    }

    if was_active {
        let fallback = config.default_connection.clone();
        let model =
            nuo_model_routing::resolved_model_name_with_usage(config, &fallback, provider_usage)
                .unwrap_or_default();
        activate(
            ActivateEnv {
                config,
                agent,
                provider_for_task,
                session: None,
                resp_tx,
                provider_usage,
            },
            fallback,
            model,
        )
        .await;
    } else {
        // Deleting an inactive connection: refresh the picker + key snapshots
        // without switching the live provider.
        let _ = resp_tx.send(AgentResponse::ProviderKeys(provider_key_status(config)));
        let _ = resp_tx.send(AgentResponse::ProviderPicker(build_live_picker_state(
            config,
            provider_usage,
        )));
    }
}

/// Apply one connection's streamed catalog-sync result. Called as each
/// connection completes, so a slow sibling never delays this one's picker
/// update.
pub fn apply_connection_update(
    config: &mut Config,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    provider_usage: &mut ConnectionUsage,
    update: nuo_model_routing::ConnectionUpdate,
) {
    if update.changed {
        nuo_model_routing::sync_fitted_model_registry();
        nuo_model_routing::prune_stale_models(config, provider_usage);
    }
    if let Some(error) = &update.error {
        let _ = resp_tx.send(AgentResponse::ConnectStatus(
            nuo_protocol::ConnectStatus::CatalogSyncWarning {
                provider: update.connection.clone(),
                message: error.clone(),
                kind: if update.refused {
                    nuo_protocol::CatalogSyncFailure::Refused
                } else {
                    nuo_protocol::CatalogSyncFailure::Transient
                },
            },
        ));
    }
    let _ = resp_tx.send(AgentResponse::ProviderPicker(build_live_picker_state(
        config,
        provider_usage,
    )));
}
