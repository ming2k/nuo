//! Provider-switch / favorite / default-model handlers.
//!
//! Each handler is one match arm of the agent background task's dispatch.
//! Provider instances live in the `providers.toml` state store, credentials in
//! `credentials.toml`, and per-route facts in the catalog cache — `config`
//! holds only *behavior* (`default_provider` / `default_model` / favorites),
//! which is what these handlers persist there. Routes are derived by the
//! catalog at activation time.

use nuo_harness::Agent;
use crate::catalog;
use nuo_harness::orchestration::round_response;
use nuo_wire::model::ModelTargetScope;
use nuo_wire::{
    AgentNotice, AgentResponse, ClientIdentity, CommandRecord, CommandResult, Provider, RoundEvent,
    SecretString, WireProtocol,
};
use nuo_persistence::config::{Config, Credentials, RemoteCatalogCache};
use nuo_persistence::connection_usage::ConnectionUsage;
use nuo_persistence::connections::{Connection, Connections};
use nuo_persistence::model_providers::ModelProviders;
use nuo_persistence::route_settings::RouteSettingsStore;
use nuo_persistence::session::{ProviderSelection, SessionStore};
use std::sync::{Arc, RwLock};
use tokio::sync::mpsc;

use crate::agent_setup::{reseed_prune_threshold, reseed_tool_variants};
use crate::session_view::provider_key_status;

/// `AgentRequest::SwitchProvider` — persist the chosen key/url/model/default,
/// rebuild the provider through the catalog so resolution stays shared with
/// startup, swap it into the shared holder, re-seed mid-turn relief, and push
/// the picker + key snapshots.
///
/// The switch writes the selection to the global `config.toml`
/// (`default_provider`/`default_model`) so the next launch — a fresh session
/// without a pin — lands on the switched provider, and additionally pins the
/// selection to this session's store so resuming *this* session restores its
/// own choice. Other live sessions keep their in-memory selection and live
/// provider; only fresh sessions follow the new global default.
/// Bundled handler environment: the plumbing arguments every provider
/// request handler threads through (config, agent, shared provider slot,
/// session store, response channel, connection usage). Handlers keep only
/// their request-specific parameters.
pub(crate) struct ProviderEnv<'a> {
    pub config: &'a mut Config,
    pub agent: &'a Agent,
    pub provider_for_task: &'a Arc<RwLock<Arc<dyn Provider>>>,
    pub session: &'a SessionStore,
    pub resp_tx: &'a mpsc::UnboundedSender<AgentResponse>,
    pub provider_usage: &'a mut ConnectionUsage,
}

pub(crate) struct AddConnectionParams {
    pub name: String,
    pub provider: String,
    pub api_key: SecretString,
    pub models: Vec<String>,
    pub auth: nuo_wire::ConnectionAuth,
    pub client_identity: Option<ClientIdentity>,
}

/// Register a user-declared model provider surface (ADR-0258).
pub(crate) fn register_provider(
    id: String,
    label: Option<String>,
    root_url: String,
    protocol: Option<WireProtocol>,
    client_profile: Option<nuo_wire::ClientPreset>,
    user_agent: Option<String>,
    catalog_format: Option<String>,
    dialect: Option<String>,
) -> Result<(), String> {
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
            client_profile,
            user_agent,
            catalog: catalog_format
                .map(|format| {
                    if matches!(format.as_str(), "none" | "static") {
                        Ok(nuo_wire::RemoteCatalogSource::None)
                    } else {
                        serde_json::from_value(serde_json::Value::String(format))
                            .map(nuo_wire::RemoteCatalogSource::Endpoint)
                            .map_err(|e| e.to_string())
                    }
                })
                .transpose()?,
            dialect: dialect.map(|value| value.parse()).transpose()?,
            protocol_roots,
            catalog_root_url: existing.as_ref().and_then(|p| p.catalog_root_url.clone()),
            prompt_cache: existing.as_ref().and_then(|p| p.prompt_cache.clone()),
            client_profile_sensitive: existing
                .as_ref()
                .is_some_and(|p| p.client_profile_sensitive),
        },
    );
    ModelProviders::save(&store).map_err(|e| e.to_string())?;
    crate::provider_registry::sync_user_declared_providers(&store)?;
    Ok(())
}

pub(crate) struct PendingOAuthAuthorization {
    pub auth: nuo_wire::ConnectionAuth,
    pub tokens: nuo_oauth::TokenSet,
}

pub(crate) struct ActivateEnv<'a> {
    pub config: &'a Config,
    pub agent: &'a Agent,
    pub provider_for_task: &'a Arc<RwLock<Arc<dyn Provider>>>,
    pub session: Option<&'a SessionStore>,
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
                .and_then(|_| crate::provider_registry::sync_user_declared_providers(&store))
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
        client_identity,
    } = params;
    let mut connections = Connections::load();
    // The name IS the connection's identity (ADR-0201 INV-3): reject a
    // duplicate with a suggested alternative instead of silently renaming it.
    let name = match connections.check_new_name(&name) {
        Ok(name) => name,
        Err(reason) => {
            reject(resp_tx, "Could not add connection", &reason);
            return;
        }
    };
    // The provider must resolve (ADR-0201 INV-2): an unknown value is a hard
    // error and never degrades into a different connection kind.
    let Some(spec) = crate::provider_registry::model_provider_spec(&provider) else {
        reject(
            resp_tx,
            "Could not add connection",
            &format!("unknown model provider '{provider}'"),
        );
        return;
    };
    let is_open_universe = spec.baselines.is_empty()
        && spec.catalog_source == nuo_provider::RemoteCatalogSource::None;
    let trimmed_key = api_key.expose_secret().trim();
    // Pasted API key on an OAuth provider → ordinary ApiKey auth.
    let auth = match (auth, !trimmed_key.is_empty()) {
        (a, true) if a.is_oauth() => nuo_wire::ConnectionAuth::ApiKey,
        (other, _) => other,
    };
    // Sanitized declared model ids. A curated provider owns its model universe,
    // so the list is an initial inclusion set; an open-universe provider declares its own.
    let declared_models: Vec<String> = models
        .iter()
        .map(|m| nuo_wire::sanitize_model_id(m))
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

    let client_identity = client_identity.unwrap_or_else(|| {
        if auth.subscription_provider() == Some("google-antigravity") {
            ClientIdentity::Antigravity
        } else {
            spec.user_agent
                .as_deref()
                .map(ClientIdentity::from_user_agent)
                .unwrap_or_default()
        }
    });

    // Pre-connection authorization is a single-use, session-local value. It
    // never enters the global store under a generic provider namespace.
    let pending_tokens = if auth.is_oauth() {
        match pending_authorization {
            Some(pending) if pending.auth == auth && pending.tokens.is_valid() => {
                Some(pending.tokens)
            }
            _ => {
                let _ = resp_tx.send(AgentResponse::ConnectStatus(
                    nuo_wire::ConnectStatus::Failed {
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
        let mut store = match crate::credentials_host::store().lock().await {
            Ok(store) => store,
            Err(error) => {
                let _ = resp_tx.send(AgentResponse::ConnectStatus(
                    nuo_wire::ConnectStatus::Failed {
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
                nuo_wire::ConnectStatus::Failed {
                    provider: name.clone(),
                    message: format!("could not persist OAuth credentials: {error}"),
                },
            ));
            return;
        }
        stored_oauth = true;
    }

    let mut stored_api_key = false;
    if auth == nuo_wire::ConnectionAuth::ApiKey && !trimmed_key.is_empty() {
        let mut creds = Credentials::load();
        creds.set_api_key(&name, Some(SecretString::from(trimmed_key)));
        let save_err = creds.save().err().map(|e| e.to_string());
        if let Some(error_msg) = save_err {
            if stored_oauth && let Ok(mut store) = crate::credentials_host::store().lock().await {
                store.remove(&name);
                let _ = store.commit().await;
            }
            let _ = resp_tx.send(AgentResponse::ConnectStatus(
                nuo_wire::ConnectStatus::Failed {
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
    let mut model_rules = nuo_wire::model::ModelScopeConfig {
        filter: provider_rules.filter.or_else(|| {
            Some(nuo_wire::ConnectionFilterPolicy::Named(
                if spec.catalog_source == nuo_provider::RemoteCatalogSource::None {
                    nuo_wire::NamedFilterPolicy::Baseline
                } else {
                    nuo_wire::NamedFilterPolicy::All
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
                    .push(nuo_wire::model::DeclaredModel {
                        id,
                        ..Default::default()
                    });
            }
        }
    }

    // Step 2: Publish connection to Connections store (purified pipe, ADR-0258).
    let connection = Connection {
        name: name.clone(),
        provider: provider.clone(),
        auth: auth.clone(),
        api_key_env: None,
        client_identity,
        models: model_rules,
        catalog_dimensions: Default::default(),
    };
    connections.connections.push(connection);
    let conn_save_err = connections.save().err().map(|e| e.to_string());
    if let Some(error_msg) = conn_save_err {
        tracing::error!(%error_msg, "add: could not persist connection; rolling back credentials");
        if stored_oauth && let Ok(mut store) = crate::credentials_host::store().lock().await {
            store.remove(&name);
            let _ = store.commit().await;
        }
        if stored_api_key {
            let mut creds = Credentials::load();
            creds.remove_api_key(&name);
            let _ = creds.save();
        }
        let _ = resp_tx.send(AgentResponse::ConnectStatus(
            nuo_wire::ConnectStatus::Failed {
                provider: name.clone(),
                message: format!("could not persist connection store: {error_msg}"),
            },
        ));
        return;
    }

    // Step 3: activating a connection is a *model-switch* concern, not a
    // connection-management concern (ADR-0201): `/connections` adds a
    // connection to the store, it never hijacks the session's live model. Only
    // two bootstrap cases may activate here:
    //   * the session has no usable provider at all (the `NoProvider` sentinel
    //     from startup) — the first added connection is the only way to get a
    //     working channel, so it steps in;
    //   * no default connection is configured yet (`default_connection` empty).
    // Everything else leaves the current selection untouched: the user picks a
    // model explicitly via `/models` (`SwitchConnection` / `SetDefaultModel`).
    let no_live_provider = nuo_harness::NoProvider::is(&**provider_for_task
        .read()
        .unwrap_or_else(|error| error.into_inner()));
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
        let _ = resp_tx.send(AgentResponse::ProviderPicker(catalog::build_picker_state(
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
    if auth.is_subscription() && auth.subscription_provider() != Some("google-antigravity") {
        let outcome = catalog::sync_connection_catalog(&name).await;
        if outcome.changed {
            catalog::sync_fitted_model_registry();
            catalog::prune_stale_models_on_disk();
        }
        for failure in &outcome.failures {
            let _ = resp_tx.send(AgentResponse::ConnectStatus(
                nuo_wire::ConnectStatus::CatalogSyncWarning {
                    provider: failure.connection.clone(),
                    message: failure.message.clone(),
                    kind: if failure.refused {
                        nuo_wire::CatalogSyncFailure::Refused
                    } else {
                        nuo_wire::CatalogSyncFailure::Transient
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
/// (ADR-0201 INV-2, INV-3).
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
    client_identity: Option<ClientIdentity>,
) {
    let mut connections = Connections::load();
    let trimmed_key = api_key.expose_secret().trim();
    // The provider must resolve (ADR-0201 INV-2).
    if crate::provider_registry::model_provider_spec(&provider).is_none() {
        reject(
            resp_tx,
            "Could not edit connection",
            &format!("unknown model provider '{provider}'"),
        );
        return;
    }
    let Some(instance) = connections.get_mut(&name) else {
        reject(
            resp_tx,
            "Could not edit connection",
            &format!("no connection named '{name}'"),
        );
        return;
    };
    instance.provider = provider;
    if let Some(ci) = client_identity {
        instance.client_identity = ci;
    }
    if !instance.auth.is_oauth() && !trimmed_key.is_empty() {
        let mut creds = Credentials::load();
        creds.set_api_key(&name, Some(SecretString::from(trimmed_key)));
        if creds.save().is_err() {
            tracing::warn!("edit: could not persist credential");
        }
    }
    if connections.save().is_err() {
        tracing::warn!("edit: could not persist connection");
    }
    catalog::prune_stale_models(config, provider_usage);
    // Only rebuild the live provider when editing the active one (so a new
    // endpoint/key takes effect); editing an inactive provider just refreshes
    // the persisted state + the picker snapshot without switching.
    if config.default_connection.eq_ignore_ascii_case(&name) {
        let model = catalog::resolved_model_name_with_usage(config, &name, provider_usage)
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
        let _ = resp_tx.send(AgentResponse::ProviderPicker(catalog::build_picker_state(
            config,
            provider_usage,
        )));
    }
}

/// `AgentRequest::RenameConnection` — rename a connection, rewriting every hard
/// join key in one transaction (ADR-0201 INV-4): `credentials.toml`,
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
    let mut locked_auth = match crate::credentials_host::store().lock().await {
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

    catalog::prune_stale_models(config, provider_usage);
    if was_default {
        let model = catalog::resolved_model_name_with_usage(config, &new_name, provider_usage)
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
        let _ = resp_tx.send(AgentResponse::ProviderPicker(catalog::build_picker_state(
            config,
            provider_usage,
        )));
    }
}

/// `AgentRequest::IncludeModel` — declare or include a model within a target scope (preset or connection) (ADR-0199).
pub(crate) async fn include_model(
    config: &mut Config,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    provider_usage: &mut ConnectionUsage,
    scope: ModelTargetScope,
    model: nuo_wire::model::DeclaredModel,
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
    catalog::prune_stale_models(config, provider_usage);
    let _ = resp_tx.send(AgentResponse::ProviderPicker(catalog::build_picker_state(
        config,
        provider_usage,
    )));
}

/// `AgentRequest::ExcludeModel` — exclude/hide a model within a target scope (ADR-0199).
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
    catalog::prune_stale_models(config, provider_usage);
    let _ = resp_tx.send(AgentResponse::ProviderPicker(catalog::build_picker_state(
        config,
        provider_usage,
    )));
}

/// `AgentRequest::ClearModelRule` — clear explicit inclusion, exclusion, or overrides for a model (ADR-0199).
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
    catalog::prune_stale_models(config, provider_usage);
    let _ = resp_tx.send(AgentResponse::ProviderPicker(catalog::build_picker_state(
        config,
        provider_usage,
    )));
}

/// `AgentRequest::SetModelCapabilities` — update per-scope model capability overrides (ADR-0199).
pub(crate) async fn set_model_capabilities(
    config: &mut Config,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    provider_usage: &mut ConnectionUsage,
    scope: ModelTargetScope,
    model_id: String,
    overrides: nuo_wire::model::CapabilityOverrides,
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
    let _ = resp_tx.send(AgentResponse::ProviderPicker(catalog::build_picker_state(
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
    overrides: Option<nuo_wire::CapabilityOverrides>,
) {
    let valid_effort = effort.and_then(|e| {
        let t = e.trim();
        (!t.is_empty())
            .then(|| t.to_ascii_lowercase())
            .filter(|s| nuo_wire::effort::Effort::parse(s).is_some())
    });

    // Resolve the route's transport to decide which knobs apply (Anthropic
    // honors thinking; OpenAI/Responses carry effort only; Google ignores both).
    let stores = catalog::Stores::load();
    let Some(conn) = stores.connections.get(&connection) else {
        return;
    };
    let channel =
        match catalog::derive_channel(conn, &model, &stores.inputs()) {
            Ok(channel) => channel,
            Err(error) => {
                let _ = resp_tx.send(AgentResponse::Error(error.to_string()));
                return;
            }
        };
    let transport = channel.transport;

    let mut routes = RouteSettingsStore::load();
    let entry = routes.settings_for_mut(&connection, &model);
    match transport {
        nuo_wire::catalog::Transport::Anthropic { .. } => {
            entry.effort = valid_effort;
            entry.thinking = thinking;
        }
        nuo_wire::catalog::Transport::OpenAi { .. }
        | nuo_wire::catalog::Transport::OpenAiResponses { .. } => {
            entry.effort = valid_effort;
            entry.thinking = None;
        }
        nuo_wire::catalog::Transport::Google { .. } => {}
    }
    // Capability overrides (ADR-0149 layer 1): `None` keeps the stored
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

    let active_model = catalog::resolved_model_name_with_usage(config, &connection, provider_usage)
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
        let _ = resp_tx.send(AgentResponse::ProviderPicker(catalog::build_picker_state(
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
    overrides: Option<nuo_wire::CapabilityOverrides>,
) {
    let valid_effort = effort.and_then(|e| {
        let t = e.trim();
        (!t.is_empty())
            .then(|| t.to_ascii_lowercase())
            .filter(|s| nuo_wire::effort::Effort::parse(s).is_some())
    });

    let provider_id = config.default_connection.clone();
    let mut routes = RouteSettingsStore::load();
    let entry = routes.settings_for_mut(&provider_id, &model);
    entry.effort = valid_effort;
    entry.thinking = thinking;
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
        catalog::resolved_model_name_with_usage(config, &provider_id, provider_usage)
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
        let _ = resp_tx.send(AgentResponse::ProviderPicker(catalog::build_picker_state(
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
    match crate::credentials_host::store().lock().await {
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
    catalog::prune_stale_models(config, provider_usage);

    let was_active = config.default_connection == id;
    if was_active {
        config.default_connection =
            catalog::effective_default_connection_id(config, &catalog::Stores::load());
        config.default_model = None;
    }
    if let Err(error) = config.save_preserving_connection_selection() {
        tracing::warn!(?error, "could not persist deleted connection");
    }

    if was_active {
        let fallback = config.default_connection.clone();
        let model = catalog::resolved_model_name_with_usage(config, &fallback, provider_usage)
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
        let _ = resp_tx.send(AgentResponse::ProviderPicker(catalog::build_picker_state(
            config,
            provider_usage,
        )));
    }
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
    session: &SessionStore,
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
            catalog::default_connection_id(config).to_string(),
            config.default_model.clone(),
        ),
    };
    let model = model_id.filter(|m| !m.is_empty()).unwrap_or_else(|| {
        catalog::resolved_model_name_with_usage(&effective, &provider_id, provider_usage)
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

/// `AgentRequest::AuthorizeOAuth` — run an OAuth login before a connection
/// exists ("+ Add connection → xAI OAuth / ChatGPT OAuth"). `auth`
/// selects which provider's flow to run. The returned token set remains
/// session-local until `AddProvider` consumes it into the final connection id.
pub async fn authorize(
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    method: nuo_wire::LoginMethod,
    auth: nuo_wire::ConnectionAuth,
) -> Option<nuo_oauth::TokenSet> {
    let Some(cfg) = auth
        .oauth_provider_id()
        .and_then(nuo_oauth::oauth_config)
    else {
        let _ = resp_tx.send(AgentResponse::ConnectStatus(
            nuo_wire::ConnectStatus::Failed {
                provider: "oauth".to_string(),
                message: "not an OAuth provider".to_string(),
            },
        ));
        return None;
    };
    let label = cfg.provider_id.to_string();
    run_oauth(resp_tx, &label, method, cfg).await
}

/// `AgentRequest::ConnectProvider` — re-auth an existing OAuth connection, then
/// activate it.
///
/// After a successful login, runs live catalog sync so the connection's
/// model list reflects the account's real entitlements immediately (rather
/// than waiting for the next launch). Catalog sync failures are non-fatal: the
/// connection keeps its previous model subset.
pub async fn connect(
    config: &mut Config,
    agent: &Agent,
    provider_for_task: &Arc<RwLock<Arc<dyn Provider>>>,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    provider_usage: &mut ConnectionUsage,
    provider_id: String,
    method: nuo_wire::LoginMethod,
) {
    if run_oauth_for_connect(resp_tx, provider_id.clone(), method).await {
        connect_post_oauth(
            config,
            agent,
            provider_for_task,
            resp_tx,
            provider_usage,
            provider_id,
        )
        .await;
    }
}

/// Run the OAuth portion of connect in a non-blocking way.
pub async fn run_oauth_for_connect(
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    provider_id: String,
    method: nuo_wire::LoginMethod,
) -> bool {
    let connections = Connections::load();
    let auth_mode = connections
        .get(&provider_id)
        .map(|p| p.auth.clone())
        .unwrap_or_default();
    let Some(cfg) = auth_mode
        .oauth_provider_id()
        .and_then(nuo_oauth::oauth_config)
    else {
        let _ = resp_tx.send(AgentResponse::ConnectStatus(
            nuo_wire::ConnectStatus::Failed {
                provider: provider_id,
                message: "not an OAuth provider".to_string(),
            },
        ));
        return false;
    };
    let Some(mut tokens) = run_oauth(resp_tx, &provider_id, method, cfg).await else {
        return false;
    };
    let mut store = match crate::credentials_host::store().lock().await {
        Ok(store) => store,
        Err(error) => {
            let _ = resp_tx.send(AgentResponse::ConnectStatus(
                nuo_wire::ConnectStatus::Failed {
                    provider: provider_id,
                    message: format!("could not lock OAuth credential store: {error}"),
                },
            ));
            return false;
        }
    };
    if tokens.refresh.is_empty()
        && let Some(previous) = store.get(&provider_id)
        && !previous.refresh.is_empty()
    {
        tokens.refresh = previous.refresh.clone();
    }
    store.set(&provider_id, tokens);
    if let Err(error) = store.commit().await {
        let _ = resp_tx.send(AgentResponse::ConnectStatus(
            nuo_wire::ConnectStatus::Failed {
                provider: provider_id,
                message: format!("could not persist OAuth credentials: {error}"),
            },
        ));
        return false;
    }
    let _ = resp_tx.send(AgentResponse::ConnectStatus(
        nuo_wire::ConnectStatus::Done {
            provider: provider_id.clone(),
        },
    ));
    true
}

/// Run the post-OAuth catalog sync and activation logic for connect.
pub async fn connect_post_oauth(
    config: &mut Config,
    agent: &Agent,
    provider_for_task: &Arc<RwLock<Arc<dyn Provider>>>,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    provider_usage: &mut ConnectionUsage,
    provider_id: String,
) {
    // Live catalog sync: fetch the provider's actual model list with the
    // fresh token so the picker shows the account's real entitlements right
    // away. A failure keeps the previous subset; each failure is reported back
    // as a warning so the user knows *why* the list did not refresh.
    let outcome = catalog::sync_connection_catalog(&provider_id).await;
    if outcome.changed {
        catalog::sync_fitted_model_registry();
    }
    catalog::prune_stale_models(config, provider_usage);
    for failure in &outcome.failures {
        let _ = resp_tx.send(AgentResponse::ConnectStatus(
            nuo_wire::ConnectStatus::CatalogSyncWarning {
                provider: failure.connection.clone(),
                message: failure.message.clone(),
                kind: if failure.refused {
                    nuo_wire::CatalogSyncFailure::Refused
                } else {
                    nuo_wire::CatalogSyncFailure::Transient
                },
            },
        ));
    }
    let model = catalog::build_picker_state(config, provider_usage)
        .rows
        .into_iter()
        .find(|r| r.id == provider_id)
        .map(|r| r.model)
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
        provider_id,
        model,
    )
    .await;
}

/// Shared OAuth exchange for any provider: browser loopback (PKCE) or device
/// code. Persistence belongs to the caller because only it knows the final,
/// exact connection namespace.
async fn run_oauth(
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    label: &str,
    method: nuo_wire::LoginMethod,
    cfg: nuo_oauth::OAuthConfig,
) -> Option<nuo_oauth::TokenSet> {
    use nuo_oauth::OAuth;

    let oauth = OAuth::new(cfg.clone(), crate::credentials_host::host());

    let login = match oauth.begin_login(method).await {
        Ok(login) => login,
        Err(error) => {
            let _ = resp_tx.send(AgentResponse::ConnectStatus(
                nuo_wire::ConnectStatus::Failed {
                    provider: label.to_string(),
                    message: error.to_string(),
                },
            ));
            return None;
        }
    };
    let prompt = login.prompt();
    let _ = resp_tx.send(AgentResponse::ConnectStatus(
        nuo_wire::ConnectStatus::Pending {
            provider: label.to_string(),
            url: prompt.url.clone(),
            user_code: prompt.user_code.clone().unwrap_or_default(),
            message: prompt.message.clone(),
        },
    ));

    let tokens = match login.complete().await {
        Ok(t) => t,
        Err(error) => {
            let message = oauth.format_login_error(&error);
            let _ = resp_tx.send(AgentResponse::ConnectStatus(
                nuo_wire::ConnectStatus::Failed {
                    provider: label.to_string(),
                    message,
                },
            ));
            return None;
        }
    };
    let now_ms = chrono::Utc::now().timestamp_millis();
    let token_set =
        nuo_oauth::build_token_set_from_login(&oauth, &label, tokens, now_ms).await;
    Some(token_set)
}

pub(crate) async fn refresh_oauth_if_needed(_config: &Config, provider_id: &str) {
    let connections = Connections::load();
    let Some(instance) = connections.get(provider_id) else {
        return;
    };
    if !instance.auth.is_oauth() {
        return;
    }
    let source =
        nuo_oauth::OAuthCredentialSource::new(
            &crate::credentials_host::host(),
            provider_id,
            instance.auth.clone(),
        );
    if let Err(error) = nuo_wire::CredentialSource::resolve_auth(&source).await {
        tracing::warn!(error = %error, provider = %provider_id, "OAuth token resolution failed");
    }
}

/// Record a provider switch's acknowledgment in the durable command ledger.
async fn record_provider_ack(session: &SessionStore, provider: &str, model: &str, ack: String) {
    let record = CommandRecord::new("models", format!("{provider} {model}")).with_result(
        CommandResult::Ack {
            title: ack,
            detail: None,
        },
    );
    if let Err(error) = session.mutate_commands(|c| c.push(record)).await {
        tracing::warn!(?error, "could not persist provider-switch ack");
    }
}

/// Shared tail of [`switch`] and [`add`]: rebuild the active provider through the
/// catalog, swap it into the shared holder, re-seed mid-turn relief, and push
/// the key + picker snapshots.
async fn activate(
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
    refresh_oauth_if_needed(config, &provider_type).await;

    let session_id = agent.thread_id();
    let Some(new_p) = catalog::build_provider_for_model(
        config,
        &provider_type,
        Some(&model),
        session_id.as_deref(),
    )
    .or_else(|| catalog::build_provider_for(config, &provider_type)) else {
        tracing::warn!(
            provider_type = %provider_type,
            model = %model,
            "activate refused: catalog could not resolve a real provider/channel",
        );
        let _ = resp_tx.send(AgentResponse::Error(format!(
            "No connection configured for '{provider_type}'. \
             Add one with /connections before sending a message."
        )));
        // Re-push the picker so the UI reflects that nothing switched.
        let _ = resp_tx.send(AgentResponse::ProviderPicker(catalog::build_picker_state(
            config,
            provider_usage,
        )));
        return;
    };
    *provider_for_task
        .write()
        .unwrap_or_else(|error| error.into_inner()) = new_p;

    reseed_prune_threshold(agent, config);
    reseed_tool_variants(agent, config);

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
    let _ = resp_tx.send(AgentResponse::ProviderPicker(catalog::build_picker_state(
        config,
        provider_usage,
    )));
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
    let _ = resp_tx.send(AgentResponse::ProviderPicker(catalog::build_picker_state(
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
    let stores = catalog::Stores::load();
    let entries = catalog::derive_entries(&stores.connections, &stores.inputs());
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

/// Apply one connection's streamed catalog-sync result (ADR-0227). Called as each
/// connection completes, so a slow sibling never delays this one's picker
/// update.
pub fn apply_connection_update(
    config: &mut Config,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    provider_usage: &mut ConnectionUsage,
    update: catalog::ConnectionUpdate,
) {
    if update.changed {
        catalog::sync_fitted_model_registry();
        catalog::prune_stale_models(config, provider_usage);
    }
    if let Some(error) = &update.error {
        let _ = resp_tx.send(AgentResponse::ConnectStatus(
            nuo_wire::ConnectStatus::CatalogSyncWarning {
                provider: update.connection.clone(),
                message: error.clone(),
                kind: if update.refused {
                    nuo_wire::CatalogSyncFailure::Refused
                } else {
                    nuo_wire::CatalogSyncFailure::Transient
                },
            },
        ));
    }
    let _ = resp_tx.send(AgentResponse::ProviderPicker(catalog::build_picker_state(
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
    outcome: catalog::CatalogSyncOutcome,
    session_id: Option<String>,
) {
    if outcome.changed {
        catalog::sync_fitted_model_registry();
        catalog::prune_stale_models(config, provider_usage);
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
    let _ = resp_tx.send(AgentResponse::ProviderPicker(catalog::build_picker_state(
        config,
        provider_usage,
    )));
}

/// Mask an API key for safe display (e.g. `sk-12...abcd`).
fn mask_api_key(key: &str) -> Option<String> {
    let trimmed = key.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.len() <= 8 {
        Some("********".to_string())
    } else {
        Some(format!(
            "{}...{}",
            &trimmed[..4],
            &trimmed[trimmed.len() - 4..]
        ))
    }
}

/// `AgentRequest::QueryConnectionDetail` — return connection details immediately and query live provider usage in background.
pub(crate) async fn query_connection_detail(
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    id: String,
    force_refresh: bool,
) {
    let stores = catalog::Stores::load();
    let Some(connection) = stores.connections.get(&id) else {
        return;
    };

    let entry = catalog::derive_entry(connection, &stores.inputs());
    let (protocol, base_url) = entry
        .default_channel()
        .map(|c| match &c.transport {
            nuo_harness::Transport::OpenAi { base_url, .. } => {
                ("openai".to_string(), base_url.clone())
            }
            nuo_harness::Transport::OpenAiResponses { base_url, .. } => {
                ("openai_responses".to_string(), base_url.clone())
            }
            nuo_harness::Transport::Anthropic { base_url, .. } => {
                ("anthropic".to_string(), base_url.clone())
            }
            nuo_harness::Transport::Google { base_url, .. } => {
                ("google".to_string(), base_url.clone())
            }
        })
        .unwrap_or_else(|| {
            let spec = crate::provider_registry::model_provider_spec(&connection.provider);
            let p = spec
                .as_ref()
                .map(|s| s.protocol)
                .unwrap_or(WireProtocol::ChatCompletions);
            let url = spec.map(|s| s.root_url.to_string()).unwrap_or_default();
            (p.to_string(), url)
        });

    let provider_label =
        nuo_wire::model_providers::model_provider_label(&connection.provider).to_string();

    let raw_key = catalog::resolve_credential(connection, &stores.creds);
    let api_key_masked = mask_api_key(raw_key.expose_secret());

    let api_key_source = if connection.auth.is_oauth() {
        "OAuth".to_string()
    } else if let Some(env) = connection.api_key_env.as_deref() {
        if std::env::var(env).is_ok() {
            format!("Environment (${env})")
        } else {
            format!("Missing (${env} not set)")
        }
    } else if stores.creds.api_key(&connection.name).is_some() {
        "credentials.toml".to_string()
    } else {
        "Not configured".to_string()
    };

    let user_agent = entry
        .default_channel()
        .map(|c| c.transport.user_agent().to_string())
        .filter(|ua| !ua.is_empty())
        .unwrap_or_else(|| connection.client_identity.user_agent().to_string());

    let models = entry
        .channels
        .iter()
        .map(|c| c.model.clone())
        .collect::<Vec<_>>();
    let model_info = entry
        .channels
        .iter()
        .map(crate::catalog::channel_model_info)
        .collect::<Vec<_>>();
    let usage_store = nuo_persistence::connection_usage::ConnectionUsage::load();
    let active_model = usage_store
        .last_model_for(&connection.name)
        .filter(|m| entry.offers_model(m))
        .map(|m| m.to_string())
        .or_else(|| entry.default_channel().map(|c| c.model.clone()));
    let active_channel = active_model
        .as_deref()
        .and_then(|m| entry.channel_for_model(m))
        .or_else(|| entry.default_channel());
    let active_channel_info = active_channel.map(crate::catalog::channel_model_info);
    let active_model_effort = active_channel_info.as_ref().and_then(|info| {
        let show = match info.protocol.as_str() {
            "anthropic" => info.thinking == Some(true),
            _ => info.effort.is_some(),
        };
        if show { info.effort.clone() } else { None }
    });
    let active_model_thinking = active_channel_info.as_ref().and_then(|info| info.thinking);

    let auth_type = if connection.auth.is_oauth() {
        format!("OAuth ({:?})", connection.auth)
    } else if connection.api_key_env.is_some() {
        "API Key (Environment)".to_string()
    } else {
        "API Key".to_string()
    };

    let mut initial_detail = nuo_wire::ConnectionDetail {
        name: connection.name.clone(),
        provider: connection.provider.clone(),
        provider_label,
        protocol,
        base_url: base_url.clone(),
        auth_type,
        api_key_masked,
        api_key_source,
        client_identity: if connection.client_identity != ClientIdentity::Native {
            connection.client_identity.clone()
        } else if connection.auth.subscription_provider() == Some("google-antigravity")
            || connection.provider == "google-antigravity"
        {
            ClientIdentity::Antigravity
        } else {
            connection.client_identity.clone()
        },
        user_agent,
        models,
        model_info,
        active_model,
        active_model_effort,
        active_model_thinking,
        usage: nuo_wire::ConnectionUsageState::Fetching,
    };

    if force_refresh {
        crate::usage_cache::shared_usage_cache().invalidate(&connection.name);
    } else if let Some(cached) = crate::usage_cache::shared_usage_cache().get(&connection.name) {
        initial_detail.usage = cached;
        let _ = resp_tx.send(AgentResponse::ConnectionDetail(initial_detail));
        return;
    }

    // Phase 1: Send local detail snapshot immediately so UI renders instantly.
    let _ = resp_tx.send(AgentResponse::ConnectionDetail(initial_detail.clone()));

    // Phase 2: Async remote query in background task.
    let resp_tx_bg = resp_tx.clone();
    let conn_id = connection.name.clone();
    let conn_auth = connection.auth.clone();
    let provider = connection.provider.clone();
    let raw_key_str = raw_key.expose_secret().to_string();
    tokio::spawn(async move {
        let (api_key, project, is_oauth) = if conn_auth.is_oauth() {
            let source =
                nuo_oauth::OAuthCredentialSource::new(
                    &crate::credentials_host::host(),
                    &conn_id,
                    conn_auth.clone(),
                );
            match nuo_wire::CredentialSource::resolve_auth(&source).await {
                Ok(auth) => {
                    let project = auth
                        .extension::<nuo_model_codec::GoogleAuthMetadata>()
                        .map(|m| m.project_id.clone());
                    (auth.token.expose_secret().to_string(), project, true)
                }
                Err(err) => {
                    initial_detail.usage = nuo_wire::ConnectionUsageState::Error(err);
                    let _ = resp_tx_bg.send(AgentResponse::ConnectionDetail(initial_detail));
                    return;
                }
            }
        } else {
            (raw_key_str, None, false)
        };

        let mut usage = crate::provider_registry::fetch_provider_usage_ext(
            &provider,
            &base_url,
            &api_key,
            project.as_deref(),
        )
        .await;

        if is_oauth
            && let nuo_wire::ConnectionUsageState::Error(ref err) = usage
            && is_auth_error(err)
        {
            let source =
                nuo_oauth::OAuthCredentialSource::new(
                    &crate::credentials_host::host(),
                    &conn_id,
                    conn_auth.clone(),
                );
            let rejected = SecretString::from(api_key.as_str());
            if let Ok(refreshed) =
                nuo_wire::CredentialSource::force_refresh_after_rejection(&source, &rejected)
                    .await
            {
                let refreshed_project = refreshed
                    .extension::<nuo_model_codec::GoogleAuthMetadata>()
                    .map(|m| m.project_id.as_str())
                    .or(project.as_deref());
                usage = crate::provider_registry::fetch_provider_usage_ext(
                    &provider,
                    &base_url,
                    refreshed.token.expose_secret(),
                    refreshed_project,
                )
                .await;
            }
        }

        crate::usage_cache::shared_usage_cache().put(&conn_id, usage.clone());
        initial_detail.usage = usage;
        let _ = resp_tx_bg.send(AgentResponse::ConnectionDetail(initial_detail));
    });
}

/// Query all connections' live provider usage concurrently and stream updates.
pub(crate) async fn query_all_connections_usage(
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    force_refresh: bool,
) {
    use futures::StreamExt;
    let stores = catalog::Stores::load();
    let connections: Vec<String> = stores
        .connections
        .connections
        .iter()
        .filter(|c| {
            crate::provider_registry::model_provider_spec(&c.provider)
                .and_then(|s| s.quota)
                .is_some()
        })
        .map(|c| c.name.clone())
        .collect();

    let mut stream = futures::stream::iter(connections)
        .map(|conn_id| {
            let tx = resp_tx.clone();
            async move {
                query_connection_detail(&tx, conn_id, force_refresh).await;
            }
        })
        .buffer_unordered(4);

    while stream.next().await.is_some() {}
}

fn is_auth_error(err: &str) -> bool {
    let lower = err.to_ascii_lowercase();
    lower.contains("401")
        || lower.contains("unauthorized")
        || lower.contains("unauthenticated")
        || lower.contains("invalid_token")
        || lower.contains("token expired")
}

#[cfg(test)]
#[allow(clippy::await_holding_lock)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn record_provider_ack_appends_durable_ack_to_command_ledger() {
        let tmp = tempfile::tempdir().unwrap();
        let session = SessionStore::for_path(tmp.path().join("session.json"));
        record_provider_ack(
            &session,
            "111xianyu",
            "k3",
            "Connection switched to 111xianyu (k3)".to_string(),
        )
        .await;

        let commands = session.commands().await;
        assert_eq!(commands.len(), 1);
        let record = &commands[0];
        assert_eq!(record.name, "models");
        assert_eq!(record.args, "111xianyu k3");
        assert_eq!(record.status, nuo_wire::CommandStatus::Success);
        match &record.result {
            Some(nuo_wire::CommandResult::Ack { title, .. }) => {
                assert_eq!(title, "Connection switched to 111xianyu (k3)");
            }
            other => panic!("expected a durable Ack result, got {other:?}"),
        }
    }

    #[test]
    fn wire_protocol_parser_is_exact_and_rejects_unknown_labels() {
        assert_eq!(
            "anthropic-messages".parse::<WireProtocol>(),
            Ok(WireProtocol::AnthropicMessages)
        );
        assert_eq!(
            "responses".parse::<WireProtocol>(),
            Ok(WireProtocol::Responses)
        );
        assert_eq!(
            "openai-responses".parse::<WireProtocol>(),
            Ok(WireProtocol::Responses)
        );
        assert_eq!(
            "chat-completions".parse::<WireProtocol>(),
            Ok(WireProtocol::ChatCompletions)
        );
        assert!("openai".parse::<WireProtocol>().is_err());
        assert!("future".parse::<WireProtocol>().is_err());
    }

    #[test]
    fn connection_names_are_unique_and_suggest_an_alternative() {
        let mut connections = Connections::default();
        connections.connections.push(Connection {
            name: "my-relay".to_string(),
            ..Default::default()
        });
        // A different name is accepted (trimmed), a duplicate is rejected with
        // a suggestion, and an empty name is rejected (ADR-0201 INV-3).
        assert_eq!(
            connections.check_new_name(" My Relay ").unwrap(),
            "My Relay"
        );
        let err = connections.check_new_name("my-relay").unwrap_err();
        assert!(err.contains("my-relay-2"), "{err}");
        assert!(connections.check_new_name("  ").is_err());
    }

    #[tokio::test]
    async fn add_provider_oauth_auth_mismatch_never_writes_connection_or_token() {
        let _guard = nuo_persistence::paths::TEST_OVERRIDE_GUARD
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let dirs = nuo_persistence::paths::Dirs {
            config_dir: dir.path().join("config"),
            data_dir: dir.path().join("data"),
            state_dir: dir.path().join("state"),
            cache_dir: dir.path().join("cache"),
            runtime_dir: None,
        };
        nuo_persistence::paths::set_test_default(Some(dirs));

        let (resp_tx, mut resp_rx) = tokio::sync::mpsc::unbounded_channel();
        let session = SessionStore::for_path(dir.path().join("session.json"));
        let mut config = Config::default();
        let mut usage = ConnectionUsage::default();
        let agent = nuo_harness::Agent::builder(
            Arc::new(nuo_harness::NoProvider),
            Vec::new(),
            nuo_harness::AgentIdentity::default(),
        )
        .build();
        let provider_for_task = Arc::new(std::sync::RwLock::new(agent.provider.clone()));

        let env = ProviderEnv {
            config: &mut config,
            agent: &agent,
            provider_for_task: &provider_for_task,
            session: &session,
            resp_tx: &resp_tx,
            provider_usage: &mut usage,
        };

        // Pending auth is for Antigravity, but params requests ChatGPT
        let pending = PendingOAuthAuthorization {
            auth: nuo_wire::ConnectionAuth::subscription("google-antigravity"),
            tokens: nuo_oauth::TokenSet {
                access: "tok".into(),
                refresh: "ref".into(),
                expires_ms: 1000,
                id_token: None,
                token_type: None,
                scope: None,
                user_email: None,
                attributes: serde_json::Map::new(),
            },
        };

        let params = AddConnectionParams {
            name: "Mismatched Provider".to_string(),
            provider: "openai-subscription".to_string(),
            api_key: "".into(),
            models: vec!["gpt-5.6".to_string()],
            auth: nuo_wire::ConnectionAuth::subscription("chatgpt"),
            client_identity: None,
        };

        add(env, params, Some(pending)).await;

        // Verify response was ConnectStatus::Failed
        let resp = resp_rx.recv().await.unwrap();
        assert!(matches!(
            resp,
            AgentResponse::ConnectStatus(nuo_wire::ConnectStatus::Failed { .. })
        ));

        // Verify no connection was written
        let conns = Connections::load();
        assert!(conns.connections.is_empty());

        // Verify no token was written
        let store = crate::credentials_host::store();
        assert!(store.read("anything").unwrap().is_none());
        assert!(
            store.read("probe").unwrap().is_none(),
            "no credential may be written for a rejected connection"
        );

        nuo_persistence::paths::set_test_default(None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn query_connection_detail_sends_initial_and_background_detail() {
        let _guard = nuo_persistence::paths::TEST_OVERRIDE_GUARD
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let dirs = nuo_persistence::paths::Dirs {
            config_dir: dir.path().join("config"),
            data_dir: dir.path().join("data"),
            state_dir: dir.path().join("state"),
            cache_dir: dir.path().join("cache"),
            runtime_dir: None,
        };
        nuo_persistence::paths::set_test_default(Some(dirs));

        let mut store = nuo_persistence::model_providers::ModelProviders::default();
        store.set_provider(
            "test-prov",
            nuo_persistence::model_providers::UserDeclaredProvider {
                label: Some("Test Relay".to_string()),
                root_url: "https://example.com".to_string(),
                default_protocol: Some(WireProtocol::ChatCompletions),
                client_profile: None,
                user_agent: None,
                catalog: None,
                dialect: None,
                protocol_roots: vec![],
                catalog_root_url: None,
                prompt_cache: None,
                client_profile_sensitive: false,
            },
        );
        ModelProviders::save(&store).unwrap();
        crate::provider_registry::sync_user_declared_providers(&store).unwrap();

        let mut conns = Connections::default();
        conns.connections.push(Connection {
            name: "test-relay".to_string(),
            provider: "test-prov".to_string(),
            ..Default::default()
        });
        conns.save().unwrap();

        let (resp_tx, mut resp_rx) = tokio::sync::mpsc::unbounded_channel();
        query_connection_detail(&resp_tx, "test-relay".to_string(), false).await;

        // Phase 1: immediate detail with Fetching usage
        let initial = resp_rx.recv().await.expect("initial response");
        match initial {
            AgentResponse::ConnectionDetail(detail) => {
                assert_eq!(detail.name, "test-relay");
                assert_eq!(detail.usage, nuo_wire::ConnectionUsageState::Fetching);
            }
            other => panic!("expected ConnectionDetail, got {other:?}"),
        }

        // Phase 2: background resolution
        let final_resp = resp_rx.recv().await.expect("final response");
        match final_resp {
            AgentResponse::ConnectionDetail(detail) => {
                assert_eq!(detail.name, "test-relay");
                // Unsupported since base_url is example.com and no API key is set
                assert!(matches!(
                    detail.usage,
                    nuo_wire::ConnectionUsageState::Error(_)
                        | nuo_wire::ConnectionUsageState::Unsupported
                ));
            }
            other => panic!("expected final ConnectionDetail, got {other:?}"),
        }

        nuo_persistence::paths::set_test_default(None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn connection_update_surfaces_warning_and_picker_then_final_keys() {
        let _guard = nuo_persistence::paths::TEST_OVERRIDE_GUARD
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let dirs = nuo_persistence::paths::Dirs {
            config_dir: dir.path().join("config"),
            data_dir: dir.path().join("data"),
            state_dir: dir.path().join("state"),
            cache_dir: dir.path().join("cache"),
            runtime_dir: None,
        };
        nuo_persistence::paths::set_test_default(Some(dirs));

        let mut config = Config::default();
        let mut usage = ConnectionUsage::default();
        let (resp_tx, mut resp_rx) = tokio::sync::mpsc::unbounded_channel();

        apply_connection_update(
            &mut config,
            &resp_tx,
            &mut usage,
            catalog::ConnectionUpdate {
                connection: "gmain".to_string(),
                changed: false,
                error: Some("network error".to_string()),
                refused: false,
            },
        );

        let warning = resp_rx.recv().await.expect("warning expected");
        match warning {
            AgentResponse::ConnectStatus(nuo_wire::ConnectStatus::CatalogSyncWarning {
                provider,
                message,
                kind,
            }) => {
                assert_eq!(provider, "gmain");
                assert_eq!(message, "network error");
                assert_eq!(kind, nuo_wire::CatalogSyncFailure::Transient);
            }
            other => panic!("expected CatalogSyncWarning, got {other:?}"),
        }
        let picker = resp_rx.recv().await.expect("picker expected");
        assert!(matches!(picker, AgentResponse::ProviderPicker(_)));

        apply_catalog_sync_outcome(
            &mut config,
            &resp_tx,
            &mut usage,
            catalog::CatalogSyncOutcome::default(),
            None,
        );

        let keys = resp_rx.recv().await.expect("keys expected");
        assert!(matches!(keys, AgentResponse::ProviderKeys(_)));
        let picker = resp_rx.recv().await.expect("picker expected");
        assert!(matches!(picker, AgentResponse::ProviderPicker(_)));

        nuo_persistence::paths::set_test_default(None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn adr0199_handlers_mutate_preset_and_connection_scopes() {
        let _guard = nuo_persistence::paths::TEST_OVERRIDE_GUARD
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let dirs = nuo_persistence::paths::Dirs {
            config_dir: dir.path().join("config"),
            data_dir: dir.path().join("data"),
            state_dir: dir.path().join("state"),
            cache_dir: dir.path().join("cache"),
            runtime_dir: None,
        };
        nuo_persistence::paths::set_test_default(Some(dirs));

        let mut config = Config::default();
        let mut usage = ConnectionUsage::default();
        let (resp_tx, mut resp_rx) = tokio::sync::mpsc::unbounded_channel();

        // 1. Include on provider scope
        include_model(
            &mut config,
            &resp_tx,
            &mut usage,
            ModelTargetScope::Provider("deepseek".into()),
            nuo_wire::model::DeclaredModel {
                id: "deepseek-v4-preview".into(),
                context_window: Some(1_000_000),
                ..Default::default()
            },
        )
        .await;

        let picker = resp_rx.recv().await.expect("picker after include");
        assert!(matches!(picker, AgentResponse::ProviderPicker(_)));

        let providers = ModelProviders::load();
        let ds = providers.get("deepseek").expect("provider scope saved");
        assert_eq!(ds.include.len(), 1);
        assert_eq!(ds.include[0].id, "deepseek-v4-preview");

        // 2. Exclude on provider scope
        exclude_model(
            &mut config,
            &resp_tx,
            &mut usage,
            ModelTargetScope::Provider("deepseek".into()),
            "deepseek-chat".into(),
        )
        .await;
        let _ = resp_rx.recv().await;

        let providers = ModelProviders::load();
        let ds = providers.get("deepseek").unwrap();
        assert_eq!(ds.exclude, vec!["deepseek-chat"]);

        // 3. Clear rule on provider scope
        clear_model_rule(
            &mut config,
            &resp_tx,
            &mut usage,
            ModelTargetScope::Provider("deepseek".into()),
            "deepseek-chat".into(),
        )
        .await;
        let _ = resp_rx.recv().await;

        let providers = ModelProviders::load();
        let ds = providers.get("deepseek").unwrap();
        assert!(!ds.exclude.contains(&"deepseek-chat".to_string()));

        nuo_persistence::paths::set_test_default(None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn add_provider_snapshots_rules_without_materializing_model_ids() {
        let _guard = nuo_persistence::paths::TEST_OVERRIDE_GUARD
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let dirs = nuo_persistence::paths::Dirs {
            config_dir: dir.path().join("config"),
            data_dir: dir.path().join("data"),
            state_dir: dir.path().join("state"),
            cache_dir: dir.path().join("cache"),
            runtime_dir: None,
        };
        nuo_persistence::paths::set_test_default(Some(dirs));

        let mut config = Config::default();
        let session = SessionStore::for_path(dir.path().join("session.json"));
        let agent = nuo_harness::Agent::builder(
            Arc::new(nuo_harness::NoProvider),
            Vec::new(),
            nuo_harness::AgentIdentity::default(),
        )
        .build();
        let provider_for_task = Arc::new(std::sync::RwLock::new(agent.provider.clone()));
        let (resp_tx, _resp_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut usage = ConnectionUsage::default();

        let env = ProviderEnv {
            config: &mut config,
            agent: &agent,
            provider_for_task: &provider_for_task,
            session: &session,
            resp_tx: &resp_tx,
            provider_usage: &mut usage,
        };

        let params = AddConnectionParams {
            name: "openai-test".to_string(),
            provider: "openai".to_string(),
            api_key: SecretString::from("sk-test"),
            models: vec!["gpt-4o".to_string(), "gpt-4o-mini".to_string()],
            auth: nuo_wire::ConnectionAuth::ApiKey,
            client_identity: None,
        };

        add(env, params, None).await;

        let conns = Connections::load();
        let conn = conns.get("openai-test").expect("connection was saved");
        // ADR-0203 [INV-CATALOG-05]: filter rule is persisted, not materialized model ID list!
        assert!(
            conn.models.include.is_empty(),
            "curated connection must not persist materialized model list"
        );
        assert_eq!(
            conn.models.filter,
            Some(nuo_wire::ConnectionFilterPolicy::Named(
                nuo_wire::NamedFilterPolicy::All
            ))
        );

        nuo_persistence::paths::set_test_default(None);
    }

    /// A session that already runs a model keeps it when a new connection is
    /// added via `/connections`: the add handler must not touch the live
    /// provider holder, the global default, or the session pin. Activation is
    /// a model-switch concern (`/models`), not a connection-management one.
    #[tokio::test(flavor = "multi_thread")]
    async fn add_connection_never_hijacks_an_active_session_model() {
        use nuo_wire::ModelRequest;

        struct LiveStubProvider;
        #[async_trait::async_trait]
        impl nuo_wire::Provider for LiveStubProvider {
            async fn chat(
                &self,
                _request: ModelRequest,
            ) -> Result<nuo_wire::ProviderCompletion, nuo_wire::ProviderError> {
                unreachable!("stub is never invoked")
            }
            async fn stream_chat(
                &self,
                _request: ModelRequest,
            ) -> Result<
                futures::stream::BoxStream<'static, Result<String, nuo_wire::ProviderError>>,
                nuo_wire::ProviderError,
            > {
                unreachable!("stub is never invoked")
            }
            fn provider_id(&self) -> String {
                "existing-conn".to_string()
            }
            fn model(&self) -> String {
                "existing-model".to_string()
            }
        }

        let _guard = nuo_persistence::paths::TEST_OVERRIDE_GUARD
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let dirs = nuo_persistence::paths::Dirs {
            config_dir: dir.path().join("config"),
            data_dir: dir.path().join("data"),
            state_dir: dir.path().join("state"),
            cache_dir: dir.path().join("cache"),
            runtime_dir: None,
        };
        nuo_persistence::paths::set_test_default(Some(dirs));

        // A session pinned to an existing connection/model (as after /models).
        let mut config = Config {
            default_connection: "existing-conn".to_string(),
            default_model: Some("existing-model".to_string()),
            ..Default::default()
        };
        let session = SessionStore::for_path(dir.path().join("session.json"));
        session
            .set_provider_selection(Some(ProviderSelection {
                connection: "existing-conn".to_string(),
                model: Some("existing-model".to_string()),
            }))
            .await
            .ok();

        // A live, non-sentinel provider holder (NOT NoProvider).
        let agent = nuo_harness::Agent::builder(
            Arc::new(LiveStubProvider),
            Vec::new(),
            nuo_harness::AgentIdentity::default(),
        )
        .build();
        let provider_for_task = Arc::new(std::sync::RwLock::new(agent.provider.clone()));
        let (resp_tx, mut resp_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut usage = ConnectionUsage::default();

        let env = ProviderEnv {
            config: &mut config,
            agent: &agent,
            provider_for_task: &provider_for_task,
            session: &session,
            resp_tx: &resp_tx,
            provider_usage: &mut usage,
        };
        let params = AddConnectionParams {
            name: "second-conn".to_string(),
            provider: "openai".to_string(),
            api_key: SecretString::from("sk-test"),
            models: vec!["gpt-4o".to_string()],
            auth: nuo_wire::ConnectionAuth::ApiKey,
            client_identity: None,
        };

        add(env, params, None).await;

        // The connection itself was still persisted — connection management happened.
        assert!(
            Connections::load().get("second-conn").is_some(),
            "the new connection must be created"
        );

        // ...but the live provider, global default, and session pin are untouched.
        assert_eq!(
            provider_for_task
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .provider_id(),
            "existing-conn",
            "live provider must not be swapped by /connections"
        );
        assert_eq!(
            config.default_connection, "existing-conn",
            "global default must not follow the newly added connection"
        );
        assert_eq!(
            config.default_model.as_deref(),
            Some("existing-model"),
            "global default model must not follow the newly added connection"
        );
        let pin = session.provider_selection().await;
        assert_eq!(
            pin.as_ref().map(|s| s.connection.as_str()),
            Some("existing-conn"),
            "session pin must keep its original connection"
        );
        assert_eq!(
            pin.as_ref().and_then(|s| s.model.as_deref()),
            Some("existing-model"),
            "session pin must keep its original model"
        );

        // No ProviderSwitched broadcast may fire — that would flip the TUI.
        while let Ok(resp) = resp_rx.try_recv() {
            assert!(
                !matches!(resp, AgentResponse::ProviderSwitched { .. }),
                "add must never broadcast ProviderSwitched"
            );
        }

        nuo_persistence::paths::set_test_default(None);
    }
}
