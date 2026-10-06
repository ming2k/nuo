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

use nuo_model_codec::catalog::{Channel, Transport};
use nuo_model_codec::{Provider, RemoteCatalogSource};
use std::borrow::Cow;
use std::sync::Arc;

pub use nuo_provider::spec::{
    ModelProviderSpec, PromptCachePolicy, QuotaPort, endpoint_for, unsupported_prompt_cache,
};

// Concrete provider selectors (the composition root's reason to exist).
use nuo_provider_anthropic::{AnthropicMessagesProvider, ThinkingConfig};
use nuo_provider_google::GoogleProvider;
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
    nuo_provider_google::GOOGLE_MODEL_PROVIDER_SPEC,
    nuo_provider_deepseek::MODEL_PROVIDER_SPEC,
    nuo_provider_xai::MODEL_PROVIDER_SPEC,
    nuo_provider_chatgpt_plan::MODEL_PROVIDER_SPEC,
    nuo_provider_copilot::MODEL_PROVIDER_SPEC,
    nuo_provider_kimi::MODEL_PROVIDER_SPEC,
    nuo_provider_zai::MODEL_PROVIDER_SPEC,
    nuo_provider_qianwen::MODEL_PROVIDER_SPEC,
    nuo_provider_qoder::MODEL_PROVIDER_SPEC,
    nuo_provider_opencode::CONSOLE_SPEC,
    nuo_provider_opencode::PLAN_SPEC,
    nuo_provider_opencode::ZEN_SPEC,
    nuo_provider_google::ANTIGRAVITY_MODEL_PROVIDER_SPEC,
];

static USER_DECLARED_SPECS: std::sync::RwLock<
    std::collections::BTreeMap<String, Arc<ModelProviderSpec>>,
> = std::sync::RwLock::new(std::collections::BTreeMap::new());

pub fn user_declared_provider_spec(id: &str) -> Option<Arc<ModelProviderSpec>> {
    USER_DECLARED_SPECS
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .get(id)
        .cloned()
}

pub fn register_user_declared_provider(
    spec: ModelProviderSpec,
) -> Result<Arc<ModelProviderSpec>, String> {
    if MODEL_PROVIDER_SPECS.iter().any(|builtin| builtin.id == spec.id) {
        return Err(format!(
            "provider `{}` collides with a built-in provider",
            spec.id
        ));
    }
    spec.validate()?;
    let spec = Arc::new(spec);
    USER_DECLARED_SPECS
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .insert(spec.id.to_string(), spec.clone());
    Ok(spec)
}

/// Replace the dynamic snapshot atomically; removed entries are released once readers finish.
pub fn sync_user_declared_providers(
    declared: &impl AsRef<
        std::collections::BTreeMap<String, nuo_model_codec::model_providers::UserDeclaredProvider>,
    >,
) -> Result<(), String> {
    let mut next = std::collections::BTreeMap::new();
    for (id, provider) in declared.as_ref() {
        provider.validate(id)?;
        let wire = provider
            .default_protocol
            .unwrap_or(nuo_model_codec::WireProtocol::ChatCompletions);
        let dialect = provider.dialect.unwrap_or_default();
        let catalog_source = provider.catalog.clone().unwrap_or_else(|| {
            RemoteCatalogSource::Endpoint(match dialect {
                nuo_model_codec::ProviderDialect::Antigravity => {
                    nuo_model_codec::CatalogShape::GoogleCloudCode
                }
                nuo_model_codec::ProviderDialect::ChatGpt => nuo_model_codec::CatalogShape::Codex,
                _ => nuo_model_codec::CatalogShape::from_wire_protocol(wire),
            })
        });
        let spec = ModelProviderSpec {
            id: Cow::Owned(id.clone()),
            baselines: &[],
            root_url: Cow::Owned(provider.root_url.clone()),
            user_agent: provider.user_agent.clone().map(Cow::Owned),
            protocol: wire,
            dialect,
            protocol_roots: Cow::Owned(
                provider
                    .protocol_roots
                    .iter()
                    .map(|(wire, root)| (*wire, Cow::Owned(root.clone())))
                    .collect(),
            ),
            catalog_root_url: provider.catalog_root_url.clone().map(Cow::Owned),
            models: &[],
            catalog_source,
            default_client_profile: provider
                .client_profile
                .unwrap_or(nuo_model_codec::ClientPreset::Native),
            client_profile_sensitive: provider.client_profile_sensitive,
            prompt_cache: PromptCachePolicy::Declared(
                provider.prompt_cache.clone().unwrap_or_default(),
            ),
            quota: None,
        };
        spec.validate()?;
        next.insert(id.clone(), Arc::new(spec));
    }
    *USER_DECLARED_SPECS
        .write()
        .unwrap_or_else(|e| e.into_inner()) = next;
    Ok(())
}

/// The spec for a provider id: built-in first, then whatever the host last
/// declared.
pub fn model_provider_spec(id: &str) -> Option<Arc<ModelProviderSpec>> {
    let canonical = nuo_model_codec::model_providers::canonical_provider_id(id)
        .unwrap_or_else(|| id.to_string());
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
    nuo_model_codec::WireProtocol,
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
) -> Arc<dyn Provider> {
    let credentials = channel.credentials_source();
    let prompt_cache = nuo_provider_transport::PromptCacheConfig::new(
        channel.prompt_cache.clone(),
        channel.prompt_cache_preference,
        session_id.map(str::to_string),
    );
    match &channel.transport {
        Transport::Google {
            base_url,
            client_profile,
            effort,
            dialect,
        } => {
            let capabilities = channel.capabilities();
            let mut provider = GoogleProvider::with_credentials(
                credentials,
                channel.model.clone(),
                base_url,
                client_profile.clone(),
            )
            .with_reasoning_effort(*effort)
            .with_model_capabilities(capabilities)
            .with_prompt_cache(prompt_cache)
            .with_dialect(*dialect)
            .with_id(entry_id.to_string());
            if let Some(sid) = session_id {
                provider = provider.with_session_id(sid);
            }
            Arc::new(provider)
        }
        Transport::Anthropic {
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
            let mut cfg = ThinkingConfig::for_model(&nuo_model_codec::model::resolve(&channel.model));
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
        Transport::OpenAi {
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
            if *dialect == nuo_model_codec::OpenAiChatDialect::Qoder {
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
        } => {
            let capabilities = channel.capabilities();
            let effective_effort = effective_channel_effort(*effort, &capabilities);
            let mut provider =
                OpenAiResponsesProvider::with_credentials(credentials, channel.model.clone(), base_url)
                    .with_client_profile(client_profile.clone())
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
    }
}

fn effective_channel_effort(
    override_effort: Option<nuo_model_codec::Effort>,
    capabilities: &nuo_model_codec::ModelCapabilities,
) -> Option<nuo_model_codec::Effort> {
    override_effort.or_else(|| {
        let known: Vec<nuo_model_codec::Effort> = capabilities
            .effort_levels
            .iter()
            .filter_map(nuo_model_codec::EffortLevel::as_known)
            .collect();
        nuo_model_codec::Effort::channel_default(&capabilities.family, &known)
    })
}

struct NuoProviderFactory;

impl nuo_provider::ProviderFactory for NuoProviderFactory {
    fn build_provider_for_channel(
        &self,
        channel: &Channel,
        entry_id: &str,
        session_id: Option<&str>,
    ) -> Arc<dyn Provider> {
        build_provider_for_channel(channel, entry_id, session_id)
    }

    fn build_credential_source(
        &self,
        host: &nuo_provider::CredentialHost,
        connection_name: &str,
        auth: &nuo_model_codec::ConnectionAuth,
        api_key: nuo_host::SecretString,
        dialect: nuo_model_codec::ProviderDialect,
    ) -> Arc<dyn nuo_model_codec::CredentialSource> {
        build_credential_source(host, connection_name, auth, api_key, dialect)
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

/// Build a dynamic or static credential source for one connection (ADR-0267).
pub fn build_credential_source(
    host: &nuo_provider::CredentialHost,
    connection_name: &str,
    auth: &nuo_model_codec::ConnectionAuth,
    api_key: nuo_host::SecretString,
    dialect: nuo_model_codec::ProviderDialect,
) -> std::sync::Arc<dyn nuo_model_codec::CredentialSource> {
    if auth.is_oauth() {
        std::sync::Arc::new(nuo_oauth::OAuthCredentialSource::new(
            host,
            connection_name,
            auth.clone(),
        ))
    } else if dialect == nuo_model_codec::ProviderDialect::Qoder {
        std::sync::Arc::new(nuo_provider_qoder::QoderApiKeyCredentialSource::new(
            host,
            connection_name,
            api_key,
        ))
    } else {
        nuo_model_codec::static_credential(api_key)
    }
}

/// Initialize and register concrete provider factories into the canonical
/// `nuo-provider` substrate. Both composition roots (`nuo` CLI and the daemon
/// bootstrap) call this exactly once.
pub fn init() {
    nuo_provider::register_provider_factory(Box::new(NuoProviderFactory));
    nuo_provider::register_catalog_discovery(Box::new(NuoCatalogDiscovery));
    nuo_provider::register_catalog_signer_builder(Box::new(|store, connection_id, bearer| {
        nuo_provider_qoder::build_catalog_signer(store, connection_id, bearer)
    }));
    nuo_provider::register_provider_specs(MODEL_PROVIDER_SPECS.iter().cloned());
    register_oauth_providers();
}

/// Register every vendor's OAuth surface with the generic engine. The engine
/// crate links no vendor; this composition root is the only place that knows
/// them all (ADR-0015).
fn register_oauth_providers() {
    let surfaces = [
        nuo_provider_google::oauth::providers(),
        nuo_provider_xai::oauth::providers(),
        nuo_provider_chatgpt_plan::oauth::providers(),
        nuo_provider_copilot::oauth::providers(),
        nuo_provider_qoder::oauth_provider::providers(),
        nuo_provider_opencode::oauth::providers(),
    ];
    for providers in surfaces {
        for provider in providers {
            nuo_oauth::register_oauth_provider(provider);
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
    let Ok(client) = nuo_provider_transport::http::Http::control_plane() else {
        return nuo_wire::ConnectionUsageState::Error(
            "could not build the HTTP client".to_string(),
        );
    };
    fetch_provider_usage_with_client(&client, port, base_url, key).await
}

/// Query provider usage with an explicit HTTP client.
pub async fn fetch_provider_usage_with_client(
    client: &nuo_provider_transport::http::Http,
    port: QuotaPort,
    base_url: &str,
    api_key: &str,
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
            nuo_provider_google::AntigravityUsageFetcher
                .fetch_usage(client, base_url, api_key)
                .await
        }
        QuotaPort::SiliconFlow => {
            nuo_provider_siliconflow::SiliconFlowUsageFetcher
                .fetch_usage(client, base_url, api_key)
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
