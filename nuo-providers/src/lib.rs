//! Provider facade and factory consumed by the orchestration layer.
//!
//! Owns application-layer concrete model providers (`protocol::{openai, anthropic, google}`),
//! connection endpoints, the provider registry (`build_provider_for_channel`), model catalog
//! discovery ([`list_models`]), and OAuth2/PKCE credential flows ([`oauth`]).

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod client;
pub mod egress;
pub mod endpoint;
pub mod http;
pub mod json;
mod list_models;
pub mod network;
pub mod oauth;
pub mod pipeline;
pub mod prompt_cache;
pub mod protocol;
mod registry;
pub mod request;
pub mod sse;
pub mod transport;
pub mod usage;
pub mod vision;

// Concrete Provider Implementations (Application Layer)
pub use protocol::anthropic::{AnthropicMessagesProvider, Effort, ReasoningMode, ThinkingConfig};
pub use protocol::google::{GOOGLE_DEFAULT_BASE_URL, GoogleGeminiProvider, GoogleProvider};
pub use protocol::openai::{
    ChatCompletionsProvider, OpenAiChatCompletionsProvider, OpenAiResponsesProvider,
    ResponsesProvider,
};

// Transport & Pipeline
pub use client::Client;
pub use egress::{Egress, HttpResponse, NuoNetEgress, RequestParts};
pub use endpoint::{
    COPILOT_CLIENT_HEADERS, ClientCapabilities, ClientIdentity, ClientPreset, ClientProfile,
    ClientProfileSpec, Endpoint, NUO_USER_AGENT, OPENCODE_USER_AGENT, OPENCODE_VERSION,
    ZCODE_CLIENT_HEADERS, ZCODE_USER_AGENT,
};
pub use pipeline::TransportPipeline;
pub use prompt_cache::PromptCacheConfig;
pub use transport::{decode_response_json, ensure_success, retry_after_ms};
pub use vision::project_images_for_route;

// Registry & Catalog
pub use registry::effort_ladders;
pub use registry::{QoderCatalogSigning, build_catalog_signer, catalog_root_for_connection};

pub use list_models::{
    CatalogParser, CatalogShape, CatalogSignature, CatalogSigning, DiscoveredModel, ModelListError,
    RemoteCatalogOptions, RemoteCatalogRequest, RemoteCatalogUpdate, fetch_remote_catalog,
    list_models, models_endpoint_for, parser_for,
};

pub use oauth::{
    CredentialHost, CredentialSession, CredentialStore, CredentialStoreError, DeviceIdentity,
    FileCredentialStore, FileDeviceIdentity, InMemoryCredentialStore, OAuthCredentialSource,
    PerProcessIdentity, TokenSet,
};

pub use registry::{
    ANTHROPIC_BUILTIN_MODELS, ANTIGRAVITY_OAUTH_MODELS, CHATGPT_BUILTIN_MODELS,
    COPILOT_SEED_MODELS, DEEPSEEK_BUILTIN_MODELS, GOOGLE_BUILTIN_MODELS, KIMI_CODE_MODELS,
    MODEL_PROVIDER_SPECS, ModelProviderSpec, OPENAI_BUILTIN_MODELS, OPENCODE_CONSOLE_MODELS,
    OPENCODE_GO_MODELS, OPENCODE_ZEN_MODELS, OPENROUTER_BUILTIN_MODELS, PromptCachePolicy,
    RemoteCatalogSource, XAI_BUILTIN_MODELS,
    ZAI_CODE_MODELS, build_provider_for_channel, endpoint_for, model_provider_spec,
    register_user_declared_provider, route_for_model, sync_user_declared_providers,
    unsupported_prompt_cache, user_declared_provider_spec,
};

/// Public: the Qoder dialect's wire surface, for golden-wire integration
/// tests ([INV-WIRE-01], ADR-0271) and downstream dialect tooling.
pub use registry::qoder;
pub use usage::{
    AntigravityUsageFetcher, DeepSeekUsageFetcher, KimiUsageFetcher, OpenRouterUsageFetcher,
    ProviderUsageFetcher, SiliconFlowUsageFetcher, fetch_provider_usage,
};

/// Build a dynamic or static credential source for one connection (ADR-0267).
pub fn build_credential_source(
    host: &CredentialHost,
    connection_name: &str,
    auth: &nuo_model_codec::ConnectionAuth,
    api_key: nuo_host::SecretString,
    dialect: nuo_model_codec::ProviderDialect,
) -> std::sync::Arc<dyn nuo_model_codec::CredentialSource> {
    if auth.is_oauth() {
        std::sync::Arc::new(oauth::OAuthCredentialSource::new(
            host,
            connection_name,
            auth.clone(),
        ))
    } else if dialect == nuo_model_codec::ProviderDialect::Qoder {
        std::sync::Arc::new(oauth::qoder::QoderApiKeyCredentialSource::new(
            host,
            connection_name,
            api_key,
        ))
    } else {
        nuo_model_codec::static_credential(api_key)
    }
}
