//! Runtime derivation of connection entries — channels are derived, never
//! persisted.
//!
//! A connection declares *who* it connects to (model provider + credential +
//! client identity + optional overrides). This module derives the concrete
//! routes — one per model, each with its transport/endpoint/credential/
//! reasoning — from that declaration plus the model provider registry and the
//! catalog cache. Nothing here is written back; the stores stay the single
//! source of truth and two connections to the same provider never duplicate or
//! drift a route set.
//!
//! Resolution precedence is the single source of truth shared by startup and
//! runtime switching (ADR-0002): env var (`api_key_env`) →
//! `credentials.toml` → empty. OAuth connections resolve their bearer from the
//! host's credential store through a dynamic, per-connection credential source.
//!
//! Derivation is **pure**: every store it reads — connections, cache, routes,
//! credentials, and the host's provider declarations — arrives as a borrowed
//! argument. Nothing here resolves a path or opens a file, so a derivation can
//! be exercised with hand-built inputs and cannot observe a change between two
//! reads of the same input.

use nuo_wire::catalog::{Channel, ProviderEntry, Transport};
use nuo_wire::model::CapabilityOverrides;
use nuo_wire::{
    ClientProfile, ConnectionFilterPolicy, Effort, NamedFilterPolicy, ProviderDialect,
    ReasoningMode, SecretString, WireProtocol,
};
use nuo_persistence::config::{Credentials, RemoteCatalogCache};
use nuo_persistence::connections::Connection;
use nuo_persistence::connections::Connections;
use nuo_persistence::model_providers::ModelProviders;
use nuo_persistence::route_settings::RouteSettingsStore;
use nuo_providers::{CredentialHost, RemoteCatalogSource, model_provider_spec};

/// Everything a derivation reads. Borrowed, never lifted from disk here.
pub struct DerivationInputs<'a> {
    pub cache: &'a RemoteCatalogCache,
    pub routes: &'a RouteSettingsStore,
    pub creds: &'a Credentials,
    /// The host's provider declarations, as a value (ADR-0300 §1).
    pub providers: &'a ModelProviders,
    /// Where OAuth credentials live for dynamic sources (ADR-0303 §1).
    pub credentials: &'a CredentialHost,
}

/// Derive every entry from the connections store, in declaration order.
pub fn derive_entries(connections: &Connections, inputs: &DerivationInputs<'_>) -> Vec<ProviderEntry> {
    connections
        .connections
        .iter()
        .map(|conn| derive_entry(conn, inputs))
        .collect()
}

/// Derive one entry from one connection.
pub fn derive_entry(connection: &Connection, inputs: &DerivationInputs<'_>) -> ProviderEntry {
    let models = route_models(connection, inputs.cache, inputs.providers);
    let channels = models
        .iter()
        .filter_map(|model| {
            derive_channel(connection, model, inputs).map_err(|error| {
                tracing::warn!(connection = %connection.name, model, %error, "invalid model route");
            }).ok()
        })
        .collect();
    ProviderEntry {
        id: connection.name.clone(),
        name: connection.name.clone(),
        description: String::new(),
        channels,
        default_channel: 0,
        builtin: false,
    }
}

/// The model ids a connection serves, in picker order (ADR-0199, ADR-0201).
/// Evaluates the 3-step Set Delta Algebra:
/// 1. S_base = (Baseline ∩ RemoteCatalog) or snapshot for the provider.
/// 2. S_provider = (S_base ∪ Provider.include) \ Provider.exclude
/// 3. S_effective = (S_provider ∪ Connection.include) \ Connection.exclude
pub fn route_models(
    connection: &Connection,
    cache: &RemoteCatalogCache,
    providers: &ModelProviders,
) -> Vec<String> {
    let Some(spec) = model_provider_spec(&connection.provider) else {
        // The loader rejects an unknown provider before this runs (ADR-0201
        // INV-2); deriving nothing is the safe floor if one slips through.
        return Vec::new();
    };
    let mut models = if spec.catalog_source != RemoteCatalogSource::None {
        match cache.connection_models.get(&connection.name) {
            Some(discovered)
                if !discovered.is_empty() || cache.model_lists.contains_key(&connection.name) =>
            {
                discovered.clone()
            }
            None if cache.model_lists.contains_key(&connection.name) => Vec::new(),
            _ => spec.models.iter().map(|m| (*m).to_string()).collect(),
        }
    } else {
        spec.models.iter().map(|m| (*m).to_string()).collect()
    };

    // Connection pipe admission gate (ADR-0203):
    // 1. Filter candidates through the connection pipe valve
    let filter_policy = connection.models.filter.as_ref().cloned().unwrap_or(
        if spec.baselines.is_empty() || spec.catalog_source != RemoteCatalogSource::None {
            ConnectionFilterPolicy::Named(NamedFilterPolicy::All)
        } else {
            ConnectionFilterPolicy::Named(NamedFilterPolicy::Baseline)
        },
    );
    let baseline_ids: std::collections::HashSet<&str> =
        spec.baselines.iter().map(|m| m.id).collect();
    models.retain(|id| filter_policy.admits(id, baseline_ids.contains(id.as_str())));

    // 2. Provider delta application (from model_providers.toml)
    if let Some(provider_scope) = providers.get(&connection.provider) {
        for id in provider_scope.included_ids() {
            if !models.contains(&id) {
                models.push(id);
            }
        }
        models.retain(|m| !provider_scope.is_excluded(m));
    }

    // 3. Connection sovereign injection (inject bypasses filter unconditionally!)
    for id in connection.models.included_ids() {
        if !models.contains(&id) {
            models.push(id);
        }
    }

    // 4. Connection absolute block (block prunes unconditionally!)
    models.retain(|m| !connection.models.is_excluded(m));

    models
}

/// Resolve one model's route: transport/endpoint/user-agent plus the resolved
/// credential, reasoning knobs, and remote capability metadata.
pub fn derive_channel(
    connection: &Connection,
    model: &str,
    inputs: &DerivationInputs<'_>,
) -> Result<Channel, nuo_wire::ProviderError> {
    let DerivationInputs {
        cache,
        routes,
        creds,
        providers,
        credentials: credential_host,
    } = inputs;
    // 4-layer descending capability cascade (ADR-0199):
    // Connection Overrides > Provider Overrides > Remote-Catalog Advertised Metadata > Baseline Registry Spec
    let provider_scope = providers.get(&connection.provider);

    let mut remote = cache.remote_metadata_for(&connection.name, model).cloned();
    if let Some(provider_declared) = provider_scope.and_then(|p| p.find_included(model)) {
        let r = remote.get_or_insert_with(Default::default);
        r.protocol = provider_declared.protocol.or(r.protocol);
        r.context_window = provider_declared.context_window.or(r.context_window);
        r.max_output_tokens = provider_declared.max_output_tokens.or(r.max_output_tokens);
        r.thinking = provider_declared.thinking.or(r.thinking);
        r.vision = provider_declared.vision.or(r.vision);
        r.tool_call = provider_declared.tool_call.or(r.tool_call);
    }
    if let Some(declared) = connection.models.find_included(model) {
        let r = remote.get_or_insert_with(Default::default);
        r.protocol = declared.protocol.or(r.protocol);
        r.context_window = declared.context_window.or(r.context_window);
        r.max_output_tokens = declared.max_output_tokens.or(r.max_output_tokens);
        r.thinking = declared.thinking.or(r.thinking);
        r.vision = declared.vision.or(r.vision);
        r.tool_call = declared.tool_call.or(r.tool_call);
    }

    let route_settings = routes.settings_for(&connection.name, model);

    let mut effective_overrides = CapabilityOverrides::default();
    if let Some(po) = provider_scope.and_then(|p| p.overrides.get(model)) {
        effective_overrides = effective_overrides.merge_with(po);
    }
    if let Some(io) = connection.models.overrides.get(model) {
        effective_overrides = effective_overrides.merge_with(io);
    }
    if let Some(ro) = route_settings.and_then(|r| r.capability_overrides.as_ref()) {
        effective_overrides = effective_overrides.merge_with(ro);
    }
    if let Some(protocol) = effective_overrides.protocol {
        remote.get_or_insert_with(Default::default).protocol = Some(protocol);
    }
    let user_overrides = (!effective_overrides.is_empty()).then_some(effective_overrides);
    let prompt_cache = model_provider_spec(&connection.provider)
        .map(|provider| provider.prompt_cache.resolve(model))
        .unwrap_or_else(nuo_wire::PromptCacheCapabilities::unsupported);
    let prompt_cache_preference = route_settings
        .and_then(|settings| settings.prompt_cache)
        .unwrap_or_default();

    // Effort applies to OpenAI/Anthropic/Google alike; thinking is an
    // Anthropic-protocol switch.
    let effort = route_settings
        .and_then(|r| r.effort.as_deref())
        .and_then(Effort::parse);
    let thinking = route_settings.map(|r| match r.thinking {
        Some(false) => ReasoningMode::Off,
        _ => ReasoningMode::Adaptive,
    });

    let (protocol, base_url, client_profile, dialect) =
        base_route(connection, model, remote.as_ref())?;

    let api_key = resolve_credential(connection, creds);
    let credentialed = nuo_providers::build_credential_source(
        credential_host,
        &connection.name,
        &connection.auth,
        api_key,
        dialect,
    );

    let transport = match protocol {
        WireProtocol::GoogleGemini => Transport::Google {
            base_url,
            client_profile,
            effort,
            dialect: dialect.google(),
        },
        WireProtocol::AnthropicMessages => Transport::Anthropic {
            base_url,
            client_profile,
            effort,
            thinking,
            dialect: dialect.anthropic(),
        },
        WireProtocol::Responses => Transport::OpenAiResponses {
            base_url,
            client_profile,
            effort,
            dialect: dialect.openai_responses(),
        },
        WireProtocol::ChatCompletions => Transport::OpenAi {
            base_url,
            client_profile,
            effort,
            dialect: dialect.openai_chat(),
        },
    };

    Ok(Channel {
        id: model.to_string(),
        label: model.to_string(),
        transport,
        credentials: credentialed,
        model: model.to_string(),
        remote,
        user_overrides,
        prompt_cache,
        prompt_cache_preference,
    })
}

/// Model protocol metadata overrides the provider-scoped baseline and default,
/// and a catalog-advertised root override replaces the spec's route for that
/// protocol (ADR-0269). Dialect and credentials remain provider-owned.
fn base_route(
    connection: &Connection,
    model: &str,
    remote: Option<&nuo_wire::RemoteModelMetadata>,
) -> Result<(WireProtocol, String, ClientProfile, ProviderDialect), nuo_wire::ProviderError> {
    let spec = model_provider_spec(&connection.provider).ok_or_else(|| {
        nuo_wire::ProviderError::invalid_request(
            &connection.provider,
            "unknown model provider",
        )
    })?;
    let protocol = remote
        .and_then(|r| r.protocol)
        .unwrap_or_else(|| spec.model_protocol(model));

    if !spec.dialect.supports(protocol) {
        return Err(nuo_wire::ProviderError::invalid_request(
            &connection.provider,
            format!(
                "model `{model}` selects {protocol}, which is incompatible with provider dialect {:?}",
                spec.dialect
            ),
        ));
    }
    let client_profile = if connection.client_identity != ClientProfile::Native
        || spec.default_client_profile != nuo_wire::ClientPreset::Native
    {
        effective_client_profile(connection)
    } else if let Some(pua) = spec.user_agent.as_deref() {
        ClientProfile::from_user_agent(pua)
    } else {
        ClientProfile::Native
    };

    // The advertised root is an **API root**, not a full endpoint: the suffix
    // is appended by the same ADR-0259 algebra the compiled spec uses.
    let base_url = match remote.and_then(|r| r.endpoint.as_deref()) {
        Some(root) => {
            let root = nuo_wire::ApiRoot::parse(root).map_err(|error| {
                nuo_wire::ProviderError::invalid_request(
                    &connection.provider,
                    format!("catalog-advertised root for model `{model}`: {error}"),
                )
            })?;
            nuo_providers::endpoint_for(spec.dialect, &root, protocol)
        }
        None => spec.endpoint(protocol).map_err(|error| {
            nuo_wire::ProviderError::invalid_request(&connection.provider, error)
        })?,
    };

    Ok((protocol, base_url, client_profile, spec.dialect))
}

/// Resolve the sparse connection override over the provider's recommended
/// client profile.
fn effective_client_profile(connection: &Connection) -> ClientProfile {
    if connection.client_identity != ClientProfile::Native {
        return connection.client_identity.clone();
    }
    model_provider_spec(&connection.provider)
        .map(|spec| ClientProfile::from(spec.default_client_profile))
        .unwrap_or(ClientProfile::Native)
}

/// The **statically configured** credential for a connection: env var
/// (`api_key_env`) → `credentials.toml` → empty.
///
/// An OAuth connection has no configured key: its bearer is dynamic, resolved
/// per request by the [`nuo_wire::CredentialSource`] that
/// [`nuo_providers::build_credential_source`] builds. Snapshotting a stored
/// access token here would hand callers the exact value that source exists to
/// refresh, so this function returns nothing for one and the caller says so.
pub fn resolve_credential(connection: &Connection, creds: &Credentials) -> SecretString {
    if connection.auth.is_oauth() {
        return SecretString::from("");
    }
    if let Some(env) = connection.api_key_env.as_deref()
        && let Ok(value) = std::env::var(env)
        && !value.trim().is_empty()
    {
        return SecretString::from(value);
    }
    if let Some(key) = creds.api_key(&connection.name).filter(|k| !k.is_empty()) {
        return key.clone();
    }
    // The OpenCode relay surfaces share one key env var (`OPENCODE_API_KEY`):
    // Zen (`/zen/v1`) and Go (`/zen/go/v1`) both authenticate with it.
    if matches!(connection.provider.as_str(), "opencode-go" | "opencode-zen")
        && let Ok(value) = std::env::var("OPENCODE_API_KEY")
        && !value.trim().is_empty()
    {
        return SecretString::from(value);
    }
    SecretString::default()
}

/// The **effective** availability verdict for `(connection, model)`, plus
/// whether a user override is what made it usable (ADR-0273).
///
/// The provider's declaration lives in the model's
/// [`nuo_wire::RemoteModelMetadata`]; this resolves it against the
/// connection's own scope. A model the connection (or the provider scope)
/// explicitly injected stays usable even when the provider declared otherwise:
/// ADR-0203 `[INV-CATALOG-04]` gives sovereign injection unconditional
/// precedence, and ADR-0273 `[INV-AVAIL-05]` requires the override to be
/// *disclosed* rather than silently erasing the upstream declaration.
///
/// This is the single source of truth for "may this be run"; the daemon gate
/// and the picker projection both derive from it, so a client can never be the
/// only place availability is enforced.
pub fn effective_availability(
    connection: &Connection,
    model: &str,
    remote: Option<&nuo_wire::RemoteModelMetadata>,
    providers: &ModelProviders,
) -> (nuo_wire::Availability, bool) {
    let declared = remote.map(nuo_wire::RemoteModelMetadata::availability_or_usable);
    let declared = declared.unwrap_or_default();
    if declared.usable {
        return (declared, false);
    }
    let sovereign = connection
        .models
        .included_ids()
        .iter()
        .any(|id| id == model)
        || providers
            .get(&connection.provider)
            .is_some_and(|scope| scope.included_ids().iter().any(|id| id == model));
    if sovereign {
        tracing::info!(
            connection = %connection.name,
            model,
            "user scope overrides a provider-declared unavailable model"
        );
        (nuo_wire::Availability::usable(), true)
    } else {
        (declared, false)
    }
}
