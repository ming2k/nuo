//! Anthropic-compatible `/messages` provider with native tool-call support.
//!
//! A thin executor over the pure [`request`], [`response`], [`signature`], and
//! [`thinking`] layers plus the shared transport helpers. The provider struct
//! holds the shared [`Endpoint`] (connection config) and only two Anthropic-unique fields:
//! `max_tokens` (the Messages API requires it) and `thinking` (the resolved
//! reasoning config). Everything else — body construction, header assembly,
//! cache breakpoints, thinking stamps, response/stream parsing — lives in a
//! pure, independently testable module.
//!
//! Module layout (mirrors the Google and OpenAI providers):
//! - [`request`] — body / headers / cache-breakpoint + thinking stamping (pure)
//! - [`response`] — usage, message assembly, stream-payload parsing (pure)
//! - [`signature`] — thinking-signature fragment accumulator (stateful, no I/O)
//! - [`thinking`] — the resolved `ThinkingConfig` knobs
//! - this file — the [`AnthropicMessagesProvider`] executor + `Provider` impl

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use nuo_model_codec::{
    CredentialSource, ModelRequest, Provider, ProviderError, ProviderPromptHints,
    ProviderStreamEvent, ResolvedAuth,
};

use nuo_provider_transport::transport::{decode_response_json, ensure_success};
use nuo_provider_transport::{Client, ClientProfile, Endpoint};

pub mod request;
pub mod response;
pub mod signature;
pub mod spec;
pub mod thinking;

pub use spec::*;

// Re-export the model-capability enums so callers reaching them through the
// provider crate keep a stable path. They live in `nuo-wire` because they
// are model capabilities, not transport details.
pub use nuo_model_codec::effort::Effort;
pub use nuo_model_codec::{ReasoningMode, ReasoningSupport};
pub use thinking::ThinkingConfig;

/// Anthropic-compatible `/messages` provider.
///
/// Embeds the shared [`Endpoint`], plus the two Anthropic-unique fields:
/// `max_tokens` (the Messages API requires it) and
/// `thinking` (the resolved reasoning/effort config).
pub struct AnthropicMessagesProvider {
    pub endpoint: Endpoint,
    /// Pooled HTTP client reused across every request this provider makes.
    pub client: Client,
    /// `max_tokens` sent on every `/messages` request. The Messages API
    /// requires this field; it caps the response length.
    pub max_tokens: u32,
    /// Resolved thinking/effort knobs stamped onto every request body.
    pub thinking: ThinkingConfig,
    /// Channel-scoped capability view. A trusted remote catalogue overrides the
    /// static baseline only for this provider/model route.
    pub capabilities: nuo_model_codec::ModelCapabilities,
    /// Use GitHub Copilot's bearer authentication and client headers for its
    /// `/v1/messages` adapter instead of stock Anthropic API-key headers.
    pub dialect: nuo_model_codec::AnthropicMessagesDialect,
    /// Route-scoped prompt-cache capabilities, defaults, and affinity.
    pub prompt_cache: nuo_provider_transport::PromptCacheConfig,
}

impl AnthropicMessagesProvider {
    pub fn new(api_key: String, model: String, base_url: &str) -> Self {
        Self::with_base_url_and_user_agent(api_key, model, base_url, nuo_provider_transport::NUO_USER_AGENT)
    }

    /// Build a provider targeting a custom `/messages` base URL with the
    /// default `User-Agent`.
    pub fn with_base_url(api_key: String, model: String, base_url: &str) -> Self {
        Self::with_base_url_and_user_agent(api_key, model, base_url, nuo_provider_transport::NUO_USER_AGENT)
    }

    /// Build a provider targeting a custom `/messages` base URL with an
    /// explicit `User-Agent`.
    pub fn with_base_url_and_user_agent(
        api_key: String,
        model: String,
        base_url: &str,
        user_agent: &str,
    ) -> Self {
        // Default the thinking/effort config to opt-in off (ADR-0046).
        let thinking = ThinkingConfig::for_model(&nuo_model_codec::model::resolve(&model));
        let capabilities = nuo_model_codec::ModelCapabilities::for_channel(&model, None);
        Self {
            endpoint: Endpoint::from_static_key(api_key, model, base_url, "anthropic")
                .with_user_agent(user_agent),
            client: Client::new(),
            max_tokens: 8192,
            thinking,
            capabilities,
            dialect: nuo_model_codec::AnthropicMessagesDialect::Standard,
            prompt_cache: nuo_provider_transport::PromptCacheConfig::default(),
        }
    }

    /// Build a provider with dynamic credentials.
    pub fn with_credentials(
        credentials: std::sync::Arc<dyn CredentialSource>,
        model: String,
        base_url: &str,
        client_profile: impl Into<ClientProfile>,
    ) -> Self {
        let thinking = ThinkingConfig::for_model(&nuo_model_codec::model::resolve(&model));
        let capabilities = nuo_model_codec::ModelCapabilities::for_channel(&model, None);
        Self {
            endpoint: Endpoint::with_credentials(credentials, model, base_url, "anthropic")
                .with_client_profile(client_profile),
            client: Client::new(),
            max_tokens: 8192,
            thinking,
            capabilities,
            dialect: nuo_model_codec::AnthropicMessagesDialect::Standard,
            prompt_cache: nuo_provider_transport::PromptCacheConfig::default(),
        }
    }

    /// Set the `max_tokens` sent on every `/messages` request.
    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    /// Override the thinking/effort configuration.
    pub fn with_thinking(mut self, thinking: ThinkingConfig) -> Self {
        self.thinking = thinking;
        self
    }

    /// Attach the effective provider-channel capability view.
    pub fn with_model_capabilities(
        mut self,
        capabilities: nuo_model_codec::ModelCapabilities,
    ) -> Self {
        self.capabilities = capabilities;
        self
    }

    pub fn with_prompt_cache(mut self, prompt_cache: nuo_provider_transport::PromptCacheConfig) -> Self {
        self.prompt_cache = prompt_cache;
        self
    }

    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.endpoint = self.endpoint.with_session_id(session_id);
        self
    }

    fn resolve_cache_plan(
        &self,
        request: &ModelRequest,
    ) -> Result<nuo_model_codec::ResolvedCachePolicy, ProviderError> {
        self.prompt_cache
            .resolve(request)
            .map_err(|e| ProviderError::invalid_request("Anthropic", e))
    }

    pub fn with_dialect(mut self, dialect: nuo_model_codec::AnthropicMessagesDialect) -> Self {
        self.dialect = dialect;
        self
    }

    /// Stamp the attribution id. Returns `self` for chaining.
    pub fn with_id(mut self, id: String) -> Self {
        self.endpoint.set_id(id);
        self
    }

    /// Apply the per-request auth + version + beta headers to a request builder.
    fn build_request_for_auth(
        &self,
        body: &serde_json::Value,
        auth: &ResolvedAuth,
    ) -> nuo_provider_transport::request::RequestBuilder {
        let mut req =
            nuo_provider_transport::request::RequestBuilder::new(http::Method::POST, self.endpoint.base_url())
                .header(http::header::USER_AGENT, self.endpoint.user_agent())
                .json(body);
        for (name, value) in request::headers(
            auth.token.expose_secret(),
            &self.capabilities,
            self.thinking,
            self.dialect == nuo_model_codec::AnthropicMessagesDialect::Copilot,
        ) {
            req = req.header(name, value);
        }
        for (name, value) in self.endpoint.headers() {
            if self.dialect != nuo_model_codec::AnthropicMessagesDialect::Copilot
                || !nuo_provider_transport::COPILOT_CLIENT_HEADERS
                    .iter()
                    .any(|(k, _)| *k == name)
            {
                req = req.header(name, value);
            }
        }
        req = self
            .endpoint
            .attach_session_affinity_headers(req, self.prompt_cache.routing_key());
        self.endpoint.attach_auth_scoped_headers(req, auth)
    }

    /// Send a request with automatic token resolution, timeout stamping,
    /// and reactive force-refresh on HTTP 401 Unauthorized for OAuth channels.
    async fn send_request(
        &self,
        body: &serde_json::Value,
        is_stream: bool,
        telemetry: &nuo_model_codec::TransportTelemetry,
    ) -> Result<nuo_provider_transport::HttpResponse, ProviderError> {
        let auth = self
            .endpoint
            .resolve_auth()
            .await
            .map_err(|e| ProviderError::authentication("Anthropic", e))?;
        let mut req = self
            .build_request_for_auth(body, &auth)
            .with_telemetry(telemetry.clone());
        if !is_stream {
            req = req.timeout(self.client.request_timeout());
        }
        let response = self.client.send_raw(req, "Anthropic").await?;

        if response.status == http::StatusCode::UNAUTHORIZED && self.endpoint.is_oauth() {
            tracing::warn!(
                provider = %self.endpoint.id,
                model = %self.endpoint.model,
                "OAuth token rejected by Anthropic (401 Unauthorized); attempting force-refresh and retry"
            );
            let refreshed_auth = self
                .endpoint
                .force_refresh_auth_after(&auth.token)
                .await
                .map_err(|error| ProviderError::authentication("Anthropic", error))?;
            let mut retry_req = self
                .build_request_for_auth(body, &refreshed_auth)
                .with_telemetry(telemetry.clone());
            if !is_stream {
                retry_req = retry_req.timeout(self.client.request_timeout());
            }
            return self.client.send(retry_req, "Anthropic").await;
        }

        ensure_success(response, "Anthropic", Some(&self.endpoint.model)).await
    }
}

#[async_trait]
impl Provider for AnthropicMessagesProvider {
    fn provider_id(&self) -> String {
        self.endpoint.id.clone()
    }

    fn model(&self) -> String {
        self.endpoint.model.clone()
    }

    fn wire_protocol(&self) -> Option<nuo_model_codec::WireProtocol> {
        Some(nuo_model_codec::WireProtocol::AnthropicMessages)
    }

    fn effort(&self) -> Option<Effort> {
        // Effort is only live while thinking is actually on: an opted-out
        // channel must not stamp a depth onto its turns (ADR-0046).
        if self.thinking.mode == ReasoningMode::Adaptive {
            self.thinking.effort
        } else {
            None
        }
    }

    fn model_capabilities(&self) -> nuo_model_codec::ModelCapabilities {
        self.capabilities.clone()
    }

    fn prompt_hints(&self) -> ProviderPromptHints {
        // No protocol hint: thinking signatures are carried as opaque
        // `provider_meta` and replayed only into the wire `thinking` block's
        // `signature` field — they never enter any content channel the model
        // can read, so there is nothing for a prompt note to guard against.
        ProviderPromptHints {
            system_guidance: "",
        }
    }

    fn usage_supported(&self) -> bool {
        true
    }

    async fn chat(
        &self,
        request: ModelRequest,
    ) -> Result<nuo_model_codec::ProviderCompletion, nuo_model_codec::ProviderError> {
        let cache_plan = self.resolve_cache_plan(&request)?;
        let ModelRequest {
            instructions,
            mut messages,
            tool_specs,
            temporary_context,
            transport_telemetry,
            ..
        } = request;
        messages.extend(temporary_context);
        let body = request::body_with_capabilities(
            messages,
            request::BodyInput {
                model: &self.endpoint.model,
                stream: false,
                instructions: Some(&instructions),
                tool_specs: (!tool_specs.is_empty()).then_some(tool_specs.as_slice()),
                max_tokens: self.max_tokens,
                thinking: self.thinking,
                cache_plan: &cache_plan,
            },
            &self.capabilities,
        );

        let resp = self
            .send_request(&body, false, &transport_telemetry)
            .await?;
        let response_json: serde_json::Value = decode_response_json(resp, "Anthropic").await?;

        let assembled = response::assemble_message(&response_json)
            .map_err(|e| ProviderError::protocol("Anthropic", e))?;

        let usage = response::usage(&response_json["usage"]);
        let artifacts = assembled.thinking_signature.as_ref().map(|signature| {
            let mut map = serde_json::Map::new();
            map.insert(
                "thinking_signature".to_string(),
                serde_json::Value::String(signature.clone()),
            );
            map
        });
        Ok(nuo_model_codec::ProviderCompletion {
            message: response::into_message(assembled),
            meta: nuo_model_codec::ProviderCompletionMeta {
                usage,
                artifacts,
                continuation: None,
            },
        })
    }

    async fn stream_chat(
        &self,
        request: ModelRequest,
    ) -> Result<
        BoxStream<'static, Result<String, nuo_model_codec::ProviderError>>,
        nuo_model_codec::ProviderError,
    > {
        let cache_plan = self.resolve_cache_plan(&request)?;
        let ModelRequest {
            instructions,
            mut messages,
            tool_specs,
            temporary_context,
            transport_telemetry,
            ..
        } = request;
        messages.extend(temporary_context);
        let body = request::body_with_capabilities(
            messages,
            request::BodyInput {
                model: &self.endpoint.model,
                stream: true,
                instructions: Some(&instructions),
                tool_specs: (!tool_specs.is_empty()).then_some(tool_specs.as_slice()),
                max_tokens: self.max_tokens,
                thinking: self.thinking,
                cache_plan: &cache_plan,
            },
            &self.capabilities,
        );

        let response = self.send_request(&body, true, &transport_telemetry).await?;

        // Reuse the shared SSE byte reassembly; each payload is one Anthropic
        // event JSON. Map to text deltas only (this is the simple stream path).
        let stream = nuo_provider_transport::sse::data_payloads(response, "Anthropic")
            .map(|item| item.map(|payload| response::stream_text(&payload)));
        Ok(stream.boxed())
    }

    async fn stream_chat_events(
        &self,
        request: ModelRequest,
    ) -> Result<
        BoxStream<'static, Result<ProviderStreamEvent, nuo_model_codec::ProviderError>>,
        nuo_model_codec::ProviderError,
    > {
        let cache_plan = self.resolve_cache_plan(&request)?;
        let ModelRequest {
            instructions,
            mut messages,
            tool_specs,
            temporary_context,
            transport_telemetry,
            ..
        } = request;
        messages.extend(temporary_context);
        let body = request::body_with_capabilities(
            messages,
            request::BodyInput {
                model: &self.endpoint.model,
                stream: true,
                instructions: Some(&instructions),
                tool_specs: (!tool_specs.is_empty()).then_some(tool_specs.as_slice()),
                max_tokens: self.max_tokens,
                thinking: self.thinking,
                cache_plan: &cache_plan,
            },
            &self.capabilities,
        );

        let response = self.send_request(&body, true, &transport_telemetry).await?;

        let sig_stash = signature::SignatureStash::shared();
        let terminal_stash = sig_stash.clone();
        // Usage arrives split across `message_start` (input + cache counters)
        // and `message_delta` (output); the accumulator merges them so the
        // emitted Usage events carry the full counts.
        let mut usage_state = response::StreamUsage::default();
        let stream = nuo_provider_transport::sse::data_payloads(response, "Anthropic").flat_map(move |item| {
            let events: Vec<Result<ProviderStreamEvent, nuo_model_codec::ProviderError>> = match item
            {
                Ok(payload) => {
                    // Parse-once discipline (ADR-0184): each payload is
                    // deserialized here and the resulting `Value` is handed
                    // to every consumer — the signature stash and the stream
                    // parser alike. Non-JSON payloads (relay keep-alives,
                    // stray comment lines) are skipped, not errors.
                    match serde_json::from_str::<serde_json::Value>(&payload) {
                        Ok(event) => {
                            sig_stash.on_event(&event);
                            match response::stream_events(&event, &mut usage_state) {
                                Ok(parsed) => parsed.into_iter().map(Ok).collect(),
                                Err(e) => vec![Err(ProviderError::protocol("Anthropic", e))],
                            }
                        }
                        Err(_) => Vec::new(),
                    }
                }
                Err(e) => vec![Err(e)],
            };
            futures::stream::iter(events)
        });
        let terminal = futures::stream::once(async move {
            let artifacts = terminal_stash.take().map(|signature| {
                let mut map = serde_json::Map::new();
                map.insert(
                    "thinking_signature".to_string(),
                    serde_json::Value::String(signature),
                );
                map
            });
            Ok(ProviderStreamEvent::Completed(
                nuo_model_codec::ProviderCompletionMeta {
                    artifacts,
                    ..Default::default()
                },
            ))
        });
        Ok(stream.chain(terminal).boxed())
    }
}

#[cfg(test)]
mod tests;
