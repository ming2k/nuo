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

// Unit tests assert by panicking: a failed `unwrap`/`expect`/`panic!` *is* the
// test result. `cfg_attr(test, ..)` scopes that exemption to the test build only,
// so the shipped graph stays under the workspace `deny` for these lints.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use protocol::capability::{ModelRequest, Provider, ProviderPromptHints, ProviderStreamEvent};
use protocol::{CredentialSource, ProviderError, ResolvedAuth};

use crate::request::COPILOT_CLIENT_HEADERS;
use provider::transport::{Client, HttpResponse, RequestBuilder};
use provider::transport::{decode_response_json, ensure_success};
use provider::{Endpoint, NUO_USER_AGENT, PromptCacheConfig};

pub mod request;
pub mod response;
pub mod signature;
pub mod spec;
pub mod thinking;

pub use spec::*;

// Re-export the model-capability enums so callers reaching them through the
// provider crate keep a stable path. They live in `protocol` because they
// are model capabilities, not transport details.
pub use protocol::effort::Effort;
pub use protocol::{ReasoningMode, ReasoningSupport};

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
    pub capabilities: protocol::ModelCapabilities,
    /// Use GitHub Copilot's bearer authentication and client headers for its
    /// `/v1/messages` adapter instead of stock Anthropic API-key headers.
    pub dialect: protocol::AnthropicMessagesDialect,
    /// Route-scoped prompt-cache capabilities, defaults, and affinity.
    pub prompt_cache: PromptCacheConfig,
}

impl AnthropicMessagesProvider {
    pub fn new(api_key: String, model: String, base_url: &str) -> Self {
        Self::with_base_url_and_user_agent(api_key, model, base_url, NUO_USER_AGENT)
    }

    /// Build a provider targeting a custom `/messages` base URL with the
    /// default `User-Agent`.
    pub fn with_base_url(api_key: String, model: String, base_url: &str) -> Self {
        Self::with_base_url_and_user_agent(api_key, model, base_url, NUO_USER_AGENT)
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
        let thinking = ThinkingConfig::for_model(&protocol::model::resolve(&model));
        let capabilities = protocol::ModelCapabilities::for_channel(&model, None);
        Self {
            endpoint: Endpoint::from_static_key(api_key, model, base_url, "anthropic")
                .with_user_agent(user_agent),
            client: Client::new(),
            max_tokens: 8192,
            thinking,
            capabilities,
            dialect: protocol::AnthropicMessagesDialect::Standard,
            prompt_cache: PromptCacheConfig::default(),
        }
    }

    /// Build a provider with dynamic credentials.
    pub fn with_credentials(
        credentials: std::sync::Arc<dyn CredentialSource>,
        model: String,
        base_url: &str,
    ) -> Self {
        let thinking = ThinkingConfig::for_model(&protocol::model::resolve(&model));
        let capabilities = protocol::ModelCapabilities::for_channel(&model, None);
        Self {
            endpoint: Endpoint::with_credentials(credentials, model, base_url, "anthropic"),
            client: Client::new(),
            max_tokens: 8192,
            thinking,
            capabilities,
            dialect: protocol::AnthropicMessagesDialect::Standard,
            prompt_cache: PromptCacheConfig::default(),
        }
    }

    /// Set an explicit User-Agent.
    pub fn with_user_agent(mut self, user_agent: impl Into<String>) -> Self {
        self.endpoint.set_user_agent(user_agent);
        self
    }

    /// Attach extra HTTP headers.
    pub fn with_headers(mut self, headers: impl IntoIterator<Item = (String, String)>) -> Self {
        self.endpoint.headers.extend(headers);
        self
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
    pub fn with_model_capabilities(mut self, capabilities: protocol::ModelCapabilities) -> Self {
        self.capabilities = capabilities;
        self
    }

    pub fn with_prompt_cache(mut self, prompt_cache: PromptCacheConfig) -> Self {
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
    ) -> Result<protocol::ResolvedCachePolicy, ProviderError> {
        self.prompt_cache
            .resolve(request)
            .map_err(|e| ProviderError::invalid_request("Anthropic", e))
    }

    pub fn with_dialect(mut self, dialect: protocol::AnthropicMessagesDialect) -> Self {
        self.dialect = dialect;
        self
    }

    /// Stamp the attribution id. Returns `self` for chaining.
    pub fn with_id(mut self, id: String) -> Self {
        self.endpoint.set_id(id);
        self
    }

    fn effective_thinking(&self, request: &ModelRequest) -> ThinkingConfig {
        let always_on = self.capabilities.thinking == protocol::ReasoningSupport::AdaptiveAlwaysOn;
        if request.reasoning_effort.is_none() {
            return self.thinking;
        }
        let thinking_active = self.thinking.mode.is_on() || always_on;
        let (keep_on, resolved_effort) = protocol::ReasoningEffortControl::resolve_switchable(
            request.reasoning_effort,
            thinking_active,
            self.thinking.effort,
        );
        if !keep_on {
            let fallback_effort = if always_on && !self.capabilities.effort_levels.is_empty() {
                Effort::shallowest_nonzero(&self.capabilities.effort_levels).or(Some(Effort::Low))
            } else {
                None
            };
            return ThinkingConfig {
                mode: protocol::ReasoningMode::Off,
                effort: fallback_effort,
            };
        }
        ThinkingConfig {
            mode: if always_on {
                self.thinking.mode
            } else {
                protocol::ReasoningMode::Adaptive
            },
            effort: resolved_effort,
        }
    }

    fn build_request_body(
        &self,
        request: &ModelRequest,
        stream: bool,
        cache_plan: &protocol::ResolvedCachePolicy,
        thinking: ThinkingConfig,
    ) -> serde_json::Value {
        request::body_with_capabilities(
            &request.messages,
            request::BodyInput {
                model: &self.endpoint.model,
                stream,
                instructions: Some(&request.instructions),
                tool_specs: request.tool_specs(),
                max_tokens: self.max_tokens,
                thinking,
                cache_plan,
            },
            &self.capabilities,
        )
    }

    /// Apply the per-request auth + version + beta headers to a request builder.
    fn build_request_for_auth(
        &self,
        wire: &provider::WireRequest,
        auth: &ResolvedAuth,
        thinking: ThinkingConfig,
    ) -> Result<RequestBuilder, protocol::ProviderError> {
        let encoded = wire.encode()?;
        let mut req = RequestBuilder::new(http::Method::POST, self.endpoint.base_url())
            .header(http::header::USER_AGENT, self.endpoint.user_agent())
            .encoded(&encoded);
        for (name, value) in request::headers(
            auth.token.expose_secret(),
            &self.capabilities,
            thinking,
            self.dialect == protocol::AnthropicMessagesDialect::Copilot,
        ) {
            req = req.header(name, value);
        }
        for (name, value) in self.endpoint.headers() {
            if self.dialect != protocol::AnthropicMessagesDialect::Copilot
                || !COPILOT_CLIENT_HEADERS.iter().any(|(k, _)| *k == name)
            {
                req = req.header(name, value);
            }
        }
        req = self
            .endpoint
            .attach_session_affinity_headers(req, self.prompt_cache.routing_key());
        Ok(self.endpoint.attach_auth_scoped_headers(req, auth))
    }

    /// Send a projected request with automatic token resolution, timeout
    /// stamping, and reactive force-refresh on HTTP 401 Unauthorized for OAuth
    /// channels.
    async fn send_request(
        &self,
        wire: &provider::WireRequest,
        is_stream: bool,
        thinking: ThinkingConfig,
        telemetry: &protocol::TransportTelemetry,
    ) -> Result<HttpResponse, ProviderError> {
        // Prewarm races auth resolution: both are async, neither needs the
        // other, and the pool hit (or a fresh connection) is ready by the
        // time the request dispatches. A prewarm failure is advisory — the
        // dispatch below reports the real error — so it is logged, not
        // propagated.
        let prewarm = self.client.prewarm(self.endpoint.base_url(), "Anthropic");
        let auth_fut = self.endpoint.resolve_auth();
        let (prewarm_outcome, auth_result) = tokio::join!(prewarm, auth_fut);
        if let Err(error) = prewarm_outcome {
            tracing::debug!(error = %error, "prewarm skipped");
        }
        let auth = auth_result.map_err(|e| ProviderError::authentication("Anthropic", e))?;

        let mut req = self
            .build_request_for_auth(wire, &auth, thinking)?
            .with_telemetry(telemetry.clone());
        if !is_stream {
            req = req.timeout(self.client.request_timeout());
        }
        let response = self.client.send_raw(req, "Anthropic").await?;

        if response.status == http::StatusCode::UNAUTHORIZED && self.endpoint.is_negotiated() {
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
                .build_request_for_auth(wire, &refreshed_auth, thinking)?
                .with_telemetry(telemetry.clone());
            if !is_stream {
                retry_req = retry_req.timeout(self.client.request_timeout());
            }
            let response = self.client.send_raw(retry_req, "Anthropic").await?;
            return ensure_success(response, "Anthropic", Some(&self.endpoint.model)).await;
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

    fn wire_protocol(&self) -> Option<inference_wire::WireProtocol> {
        Some(inference_wire::WireProtocol::AnthropicMessages)
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

    fn model_capabilities(&self) -> protocol::ModelCapabilities {
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
        request: &ModelRequest,
    ) -> Result<protocol::ProviderCompletion, protocol::ProviderError> {
        let cache_plan = self.resolve_cache_plan(request)?;
        let thinking = self.effective_thinking(request);
        let body = self.build_request_body(request, false, &cache_plan, thinking);

        let wire = provider::WireRequest::new(protocol::WireProtocol::AnthropicMessages, body);
        let resp = self
            .send_request(
                &wire,
                false,
                thinking,
                &protocol::TransportTelemetry::default(),
            )
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
        Ok(protocol::ProviderCompletion {
            message: response::into_message(assembled),
            meta: protocol::ProviderCompletionMeta {
                usage,
                artifacts,
                continuation: None,
            },
        })
    }

    async fn stream_chat(
        &self,
        request: &ModelRequest,
        telemetry: protocol::TransportTelemetry,
    ) -> Result<BoxStream<'static, Result<String, protocol::ProviderError>>, protocol::ProviderError>
    {
        let cache_plan = self.resolve_cache_plan(request)?;
        let thinking = self.effective_thinking(request);
        let body = self.build_request_body(request, true, &cache_plan, thinking);

        let wire = provider::WireRequest::new(protocol::WireProtocol::AnthropicMessages, body);
        let response = self.send_request(&wire, true, thinking, &telemetry).await?;

        // Reuse the shared SSE byte reassembly; each payload is one Anthropic
        // event JSON. Map to text deltas only (this is the simple stream path).
        let stream = provider::transport::sse::data_payloads(response, "Anthropic").map(|item| {
            item.map_err(ProviderError::from)
                .map(|payload| response::stream_text(&payload))
        });
        Ok(stream.boxed())
    }

    async fn stream_chat_events(
        &self,
        request: &ModelRequest,
        telemetry: protocol::TransportTelemetry,
    ) -> Result<
        BoxStream<'static, Result<ProviderStreamEvent, protocol::ProviderError>>,
        protocol::ProviderError,
    > {
        let cache_plan = self.resolve_cache_plan(request)?;
        let thinking = self.effective_thinking(request);
        let body = self.build_request_body(request, true, &cache_plan, thinking);

        let wire = provider::WireRequest::new(protocol::WireProtocol::AnthropicMessages, body);
        let response = self.send_request(&wire, true, thinking, &telemetry).await?;

        let sig_stash = signature::SignatureStash::shared();
        let terminal_stash = sig_stash.clone();
        // Usage arrives split across `message_start` (input + cache counters)
        // and `message_delta` (output); the accumulator merges them so the
        // emitted Usage events carry the full counts.
        let mut usage_state = response::StreamUsage::default();
        let stream =
            provider::transport::sse::data_payloads(response, "Anthropic").flat_map(move |item| {
                let events: Vec<Result<ProviderStreamEvent, protocol::ProviderError>> = match item {
                    Ok(payload) => {
                        // Parse-once discipline: each payload is
                        // deserialized here and the resulting `Value` is handed
                        // to every consumer — the signature stash and the stream
                        // parser alike. Non-JSON payloads (relay keep-alives,
                        // stray comment lines) are skipped, not errors.
                        match serde_json::from_str::<serde_json::Value>(&payload) {
                            Ok(event) => {
                                sig_stash.on_event(&event);
                                match response::stream_events(&event, &mut usage_state) {
                                    Ok(parsed) => parsed.into_iter().map(Ok).collect(),
                                    Err(e) => {
                                        vec![Err(ProviderError::protocol("Anthropic", e))]
                                    }
                                }
                            }
                            Err(_) => Vec::new(),
                        }
                    }
                    Err(e) => vec![Err(ProviderError::from(e))],
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
                protocol::ProviderCompletionMeta {
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
