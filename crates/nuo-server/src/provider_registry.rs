//! Provider composition root (ADR-0027 §5).
//!
//! This is the single place that knows every concrete provider. It aggregates
//! the per-provider `MODEL_PROVIDER_SPEC` tables from `providers/*`, constructs
//! the concrete `Provider` for a channel, resolves remote catalogs, and
//! dispatches quota queries through the typed [`QuotaPort`] declared on each
//! spec (`[INV-PROV-08]`) — no provider-id or base-URL string sniffing.
//!
//! `[INV-PROV-09]`: because this crate links every `providers/nuo-provider-*`
//! crate, the shipped binary links them all.
//!
//! Moved from `nuo-provider-adapters`' `registry/mod.rs` + `lib.rs::init()`;
//! the per-provider spec modules there were duplicates of the `providers/`
//! crates' own `spec.rs` and are deleted.
use nuo_provider::catalog::{Channel, Transport};
use std::borrow::Cow;
use std::sync::Arc;

pub use nuo_provider::spec::{
    ModelProviderSpec, PromptCachePolicy, QuotaPort, endpoint_for, unsupported_prompt_cache,
};

// Concrete provider selectors (the composition root's reason to exist).
use nuo_provider_anthropic::{AnthropicMessagesProvider, ThinkingConfig};
use nuo_provider_google_antigravity::{GoogleInteractionsProvider, GoogleProvider};
use nuo_provider_openai::{OpenAiChatCompletionsProvider, OpenAiResponsesProvider};
use nuo_provider_qoder as qoder;

/// The single registry of built-in model providers.
///
/// Each entry's `id` MUST be unique (`[INV-PROV-06]`). Every entry is the
/// authoritative definition owned by its `providers/nuo-provider-*` crate.
pub const MODEL_PROVIDER_SPECS: &[ModelProviderSpec] = &[
    nuo_provider_openai::MODEL_PROVIDER_SPEC,
    nuo_provider_openrouter::MODEL_PROVIDER_SPEC,
    nuo_provider_commandcode_plan::MODEL_PROVIDER_SPEC,
    nuo_provider_anthropic::MODEL_PROVIDER_SPEC,
    nuo_provider_google_antigravity::GOOGLE_MODEL_PROVIDER_SPEC,
    nuo_provider_deepseek::MODEL_PROVIDER_SPEC,
    nuo_provider_xai::MODEL_PROVIDER_SPEC,
    nuo_provider_chatgpt_plan::MODEL_PROVIDER_SPEC,
    nuo_provider_copilot::MODEL_PROVIDER_SPEC,
    nuo_provider_kimi::MODEL_PROVIDER_SPEC,
    nuo_provider_glm_cn::MODEL_PROVIDER_SPEC,
    nuo_provider_qianwen::MODEL_PROVIDER_SPEC,
    nuo_provider_qoder::MODEL_PROVIDER_SPEC,
    nuo_provider_codebuddy_cn::MODEL_PROVIDER_SPEC,
    nuo_provider_codebuddy_intl::INTL_MODEL_PROVIDER_SPEC,
    nuo_provider_opencode_plan::MODEL_PROVIDER_SPEC,
    nuo_provider_opencode_zen::MODEL_PROVIDER_SPEC,
    nuo_provider_google_antigravity::ANTIGRAVITY_MODEL_PROVIDER_SPEC,
];

// User-declared providers have exactly one registry — the `nuo-provider`
// substrate's, refreshed from `model_providers.toml`. Keeping a second copy
// here let the two disagree: after a restart this crate's copy was empty while
// the catalog still resolved the provider, so Add/EditConnection rejected a
// perfectly valid provider.
pub use nuo_provider::spec::{
    register_user_declared_provider, sync_user_declared_providers, user_declared_provider_spec,
};

/// The spec for a provider id: built-in first, then whatever the host last
/// declared.
///
/// The built-in table is this crate's own const, so resolution works before
/// [`init`] populates the `nuo-provider` registry (unit tests, early boot).
pub fn model_provider_spec(id: &str) -> Option<Arc<ModelProviderSpec>> {
    let canonical =
        nuo_provider::model_providers::canonical_provider_id(id).unwrap_or_else(|| id.to_string());
    if let Some(spec) = MODEL_PROVIDER_SPECS
        .iter()
        .find(|spec| spec.id == id || spec.id == canonical.as_str())
    {
        return Some(Arc::new(spec.clone()));
    }
    user_declared_provider_spec(id).or_else(|| user_declared_provider_spec(&canonical))
}

/// Resolve the transport endpoint for **one model** of a model provider.
pub fn route_for_model(
    provider_id: &str,
    model_id: &str,
) -> Option<(
    nuo_inference_wire::WireProtocol,
    String,
    Option<Cow<'static, str>>,
)> {
    let spec = model_provider_spec(provider_id)?;
    let protocol = spec.model_protocol(model_id);
    Some((
        protocol,
        spec.endpoint(protocol).ok()?,
        spec.user_agent.clone(),
    ))
}

/// Construct the concrete `Provider` for a [`Channel`].
pub fn build_provider_for_channel(
    channel: &Channel,
    entry_id: &str,
    session_id: Option<&str>,
) -> Arc<dyn nuo_provider::Provider> {
    let credentials = channel.credentials_source();
    let prompt_cache = nuo_provider::PromptCacheConfig::new(
        channel.prompt_cache.clone(),
        channel.prompt_cache_preference,
        session_id.map(str::to_string),
    );
    match &channel.transport {
        Transport::GoogleGenerateContent {
            base_url,
            client_profile,
            effort,
            dialect,
        } => {
            let capabilities = channel.capabilities();
            let effective_effort = effective_channel_effort(*effort, &capabilities);
            let mut provider = GoogleProvider::with_credentials(
                credentials,
                channel.model.clone(),
                base_url,
                client_profile.clone(),
            )
            .with_reasoning_effort(effective_effort)
            .with_model_capabilities(capabilities)
            .with_prompt_cache(prompt_cache)
            .with_dialect(*dialect)
            .with_id(entry_id.to_string());
            if let Some(sid) = session_id {
                provider = provider.with_session_id(sid);
            }
            Arc::new(provider)
        }
        Transport::GoogleInteractions {
            base_url,
            client_profile,
            effort,
            dialect: _,
        } => {
            let capabilities = channel.capabilities();
            let effective_effort = effective_channel_effort(*effort, &capabilities);
            let mut provider = GoogleInteractionsProvider::with_credentials(
                credentials,
                channel.model.clone(),
                base_url,
            )
            .with_client_profile(client_profile.clone())
            .with_reasoning_effort(effective_effort)
            .with_model_capabilities(capabilities)
            .with_prompt_cache(prompt_cache)
            .with_id(entry_id.to_string());
            if let Some(sid) = session_id {
                provider = provider.with_session_id(sid);
            }
            Arc::new(provider)
        }
        Transport::AnthropicMessages {
            base_url,
            client_profile,
            effort,
            thinking,
            dialect,
        } => {
            let mut provider = AnthropicMessagesProvider::with_credentials(
                credentials,
                channel.model.clone(),
                base_url,
                client_profile.clone(),
            )
            .with_id(entry_id.to_string());
            if let Some(sid) = session_id {
                provider = provider.with_session_id(sid);
            }
            let capabilities = channel.capabilities();
            if let Some(max_tokens) = capabilities
                .max_output_tokens
                .or_else(|| nuo_provider_anthropic::anthropic_model_max_tokens(&channel.model))
            {
                provider = provider.with_max_tokens(max_tokens);
            }
            let mut cfg = ThinkingConfig::for_model(&nuo_provider::model::resolve(&channel.model));
            if let Some(mode) = thinking {
                cfg = cfg.with_mode(*mode);
            }
            if let Some(effort) = effort {
                cfg = cfg.with_effort(*effort);
            }
            provider = provider
                .with_thinking(cfg)
                .with_model_capabilities(capabilities)
                .with_prompt_cache(prompt_cache)
                .with_dialect(*dialect);
            Arc::new(provider)
        }
        Transport::OpenAiChatCompletions {
            base_url,
            client_profile,
            effort,
            dialect,
        } => {
            let capabilities = channel.capabilities();
            let effective_effort = effective_channel_effort(*effort, &capabilities);
            let (catalog_source, display_name) = channel.catalog_provenance();
            let mut provider = OpenAiChatCompletionsProvider::with_credentials(
                credentials,
                channel.model.clone(),
                base_url,
                client_profile.clone(),
            )
            .with_reasoning_effort(effective_effort)
            .with_prompt_cache(prompt_cache)
            .with_model_capabilities(capabilities)
            .with_dialect(*dialect)
            .with_catalog_provenance(catalog_source, display_name)
            .with_id(entry_id.to_string());
            if *dialect == nuo_provider::OpenAiChatDialect::Qoder {
                provider = provider.with_pipeline(qoder::build_qoder_pipeline());
            }
            if let Some(sid) = session_id {
                provider = provider.with_session_id(sid);
            }
            Arc::new(provider)
        }
        Transport::OpenAiResponses {
            base_url,
            client_profile,
            effort,
            dialect,
            state,
        } => {
            let capabilities = channel.capabilities();
            let effective_effort = effective_channel_effort(*effort, &capabilities);
            // Persistence needs both a surface that declares it and a dialect
            // whose schema defines `store` (ADR-0114). Anything less replays the
            // full local context and never anchors on `previous_response_id`.
            let store = state.is_stored() && dialect.supports_store();
            let mut provider = OpenAiResponsesProvider::with_credentials(
                credentials,
                channel.model.clone(),
                base_url,
            )
            .with_client_profile(client_profile.clone())
            .with_reasoning_effort(effective_effort)
            .with_model_capabilities(capabilities)
            .with_prompt_cache(prompt_cache)
            .with_dialect(*dialect)
            .with_store(store)
            .with_id(entry_id.to_string());
            if let Some(sid) = session_id {
                provider = provider.with_session_id(sid);
            }
            Arc::new(provider)
        }
    }
}

fn effective_channel_effort(
    override_effort: Option<nuo_provider::Effort>,
    capabilities: &nuo_provider::ModelCapabilities,
) -> Option<nuo_provider::Effort> {
    override_effort.or_else(|| {
        let known: Vec<nuo_provider::Effort> = capabilities
            .effort_levels
            .iter()
            .filter_map(nuo_provider::EffortLevel::as_known)
            .collect();
        nuo_provider::Effort::channel_default(&capabilities.family, &known)
    })
}

struct NuoProviderFactory;

impl nuo_provider::ProviderFactory for NuoProviderFactory {
    fn build_provider_for_channel(
        &self,
        channel: &Channel,
        entry_id: &str,
        session_id: Option<&str>,
    ) -> Arc<dyn nuo_provider::Provider> {
        build_provider_for_channel(channel, entry_id, session_id)
    }

    fn build_credential_source(
        &self,
        host: &nuo_provider::CredentialHost,
        provider_id: &str,
        connection_name: &str,
        auth: &nuo_provider::ConnectionAuth,
        api_key: nuo_host::SecretString,
        dialect: nuo_provider::ProviderDialect,
    ) -> Arc<dyn nuo_provider::CredentialSource> {
        build_credential_source(host, provider_id, connection_name, auth, api_key, dialect)
    }
}

struct NuoCatalogDiscovery;

#[async_trait::async_trait]
impl nuo_provider::CatalogDiscovery for NuoCatalogDiscovery {
    async fn fetch_remote_catalog(
        &self,
        request: nuo_provider::RemoteCatalogRequest<'_>,
        options: nuo_provider::RemoteCatalogOptions<'_>,
    ) -> Result<nuo_provider::RemoteCatalogUpdate, nuo_provider::ModelListError> {
        nuo_provider_catalog::fetch_remote_catalog(request, options).await
    }
}

/// Build a dynamic or static credential source for one connection.
pub fn build_credential_source(
    host: &nuo_provider::CredentialHost,
    provider_id: &str,
    connection_name: &str,
    auth: &nuo_provider::ConnectionAuth,
    api_key: nuo_host::SecretString,
    dialect: nuo_provider::ProviderDialect,
) -> std::sync::Arc<dyn nuo_provider::CredentialSource> {
    if auth.is_negotiated() {
        std::sync::Arc::new(nuo_provider::auth::OAuthCredentialSource::new(
            host,
            connection_name,
            auth.clone(),
        ))
    } else if dialect == nuo_provider::ProviderDialect::Qoder {
        std::sync::Arc::new(nuo_provider_qoder::QoderApiKeyCredentialSource::new(
            host,
            connection_name,
            api_key,
        ))
    } else if dialect == nuo_provider::ProviderDialect::CodeBuddy {
        // BYOK lane: a pasted CodeBuddy CLI token or API key; the header shape
        // is decided by the credential source itself, and the realm gate
        // ([INV-PROV-REALM-01]) refuses CLI-file tokens minted outside this
        // channel's account line. The channel is selected by the connection's
        // provider id — both realms share the `CodeBuddy` dialect, so the
        // dialect alone cannot discriminate (ADR-0097).
        let allowed = match provider_id {
            "codebuddy-intl" => nuo_provider_codebuddy_intl::ALLOWED_CREDENTIAL_DOMAINS,
            _ => nuo_provider_codebuddy_cn::ALLOWED_CREDENTIAL_DOMAINS,
        };
        std::sync::Arc::new(nuo_provider_codebuddy_auth::CodeBuddyByokSource::new(
            connection_name,
            api_key,
            allowed,
        ))
    } else {
        nuo_provider::static_credential(api_key)
    }
}

/// Initialize and register concrete provider factories into the canonical
/// `nuo-provider` substrate. Both composition roots (`nuo` CLI and the server
/// bootstrap) call this exactly once.
pub fn init() {
    nuo_provider::register_provider_factory(Box::new(NuoProviderFactory));
    nuo_provider::register_catalog_discovery(Box::new(NuoCatalogDiscovery));
    nuo_provider::register_catalog_signer_builder(Box::new(|store, connection_id, bearer| {
        nuo_provider_qoder::build_catalog_signer(store, connection_id, bearer)
    }));
    nuo_provider::register_provider_specs(MODEL_PROVIDER_SPECS.iter().cloned());
    // Seed user-declared providers now, so the registry is complete from boot
    // rather than only after the first catalog load. Without this, a restarted
    // server could reject a connection to a provider that exists on disk.
    if let Err(error) = nuo_provider::sync_user_declared_providers(
        &nuo_persistence::model_providers::ModelProviders::load(),
    ) {
        tracing::warn!(%error, "could not load user-declared model providers");
    }
    register_oauth_providers();
}

/// Register every vendor's OAuth surface with the generic engine. The engine
/// crate links no vendor; this composition root is the only place that knows
/// them all (ADR-0015).
fn register_oauth_providers() {
    let surfaces = [
        nuo_provider_google_antigravity::oauth::providers(),
        nuo_provider_xai::oauth::providers(),
        nuo_provider_chatgpt_plan::oauth::providers(),
        nuo_provider_copilot::oauth::providers(),
        nuo_provider_qoder::oauth_provider::providers(),
        nuo_provider_codebuddy_cn::oauth::providers(),
        nuo_provider_codebuddy_intl::oauth::providers(),
    ];
    for providers in surfaces {
        for provider in providers {
            nuo_provider::auth::register_oauth_provider(provider);
        }
    }
}

/// Query provider usage for a connection through its spec's typed quota port
/// (`[INV-PROV-08]`). No provider-id or base-URL substring matching.
pub async fn fetch_provider_usage(
    provider: &str,
    base_url: &str,
    api_key: &str,
) -> nuo_wire::ConnectionUsageState {
    fetch_provider_usage_ext(provider, base_url, api_key, None).await
}

/// Query provider usage with optional provider-scoped project metadata.
pub async fn fetch_provider_usage_ext(
    provider: &str,
    base_url: &str,
    api_key: &str,
    project: Option<&str>,
) -> nuo_wire::ConnectionUsageState {
    let Some(spec) = model_provider_spec(provider) else {
        return nuo_wire::ConnectionUsageState::Unsupported;
    };
    let Some(port) = spec.quota else {
        return nuo_wire::ConnectionUsageState::Unsupported;
    };
    let key = api_key.trim();
    if key.is_empty() {
        return nuo_wire::ConnectionUsageState::Error("API key is not configured".to_string());
    }
    let Ok(client) = nuo_provider::transport::Http::control_plane() else {
        return nuo_wire::ConnectionUsageState::Error(
            "could not build the HTTP client".to_string(),
        );
    };
    fetch_provider_usage_with_client_ext(&client, port, base_url, key, project).await
}

/// Query provider usage with an explicit HTTP client.
pub async fn fetch_provider_usage_with_client(
    client: &nuo_provider::transport::Http,
    port: QuotaPort,
    base_url: &str,
    api_key: &str,
) -> nuo_wire::ConnectionUsageState {
    fetch_provider_usage_with_client_ext(client, port, base_url, api_key, None).await
}

/// Query provider usage with an explicit HTTP client and optional project context.
pub async fn fetch_provider_usage_with_client_ext(
    client: &nuo_provider::transport::Http,
    port: QuotaPort,
    base_url: &str,
    api_key: &str,
    project: Option<&str>,
) -> nuo_wire::ConnectionUsageState {
    let result = match port {
        QuotaPort::DeepSeekBalance => {
            nuo_provider_deepseek::DeepSeekUsageFetcher
                .fetch_usage(client, base_url, api_key)
                .await
        }
        QuotaPort::KimiBalance => {
            nuo_provider_kimi::KimiUsageFetcher
                .fetch_usage(client, base_url, api_key)
                .await
        }
        QuotaPort::CommandCodeCredits => {
            nuo_provider_commandcode_plan::CommandCodeUsageFetcher
                .fetch_usage(client, base_url, api_key)
                .await
        }
        QuotaPort::OpenRouterKey => {
            nuo_provider_openrouter::OpenRouterUsageFetcher
                .fetch_usage(client, base_url, api_key)
                .await
        }
        QuotaPort::Antigravity => {
            nuo_provider_google_antigravity::AntigravityUsageFetcher
                .fetch_usage_with_project(client, base_url, api_key, project)
                .await
        }
        QuotaPort::Qoder => {
            nuo_provider_qoder::QoderUsageFetcher
                .fetch_usage(client, base_url, api_key)
                .await
        }
    };
    match result {
        Ok(usage) => nuo_wire::ConnectionUsageState::Available(Box::new(usage)),
        Err(err) => nuo_wire::ConnectionUsageState::Error(err),
    }
}

#[cfg(test)]
mod tests;
