//! OpenAI **Responses** API provider — the wire format spoken by the ChatGPT
//! Subscription backend (`chatgpt.com/backend-api/codex/responses`).
//!
//! Unlike the chat-completions [`OpenAiChatCompletionsProvider`](crate::OpenAiChatCompletionsProvider),
//! this provider:
//! - sends dynamic OAuth credentials or static tokens,
//! - attaches the optional `ChatGPT-Account-Id` header for ChatGPT Subscriptions,
//! - builds a Responses request (`instructions` + `input` items) and parses
//!   `response.*` streaming events,
//! - supports self-healing reactive force-refresh on HTTP 401 Unauthorized for OAuth channels.

pub mod request;
pub mod response;
mod tool_trace;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use nuo_model_codec::{
    CredentialSource, Effort, ModelRequest, Provider, ProviderError, ProviderErrorKind,
    ProviderPromptHints, ProviderStreamEvent, ResolvedAuth,
};
use std::sync::{Arc, Mutex};

use crate::transport::{decode_response_json, ensure_success};
use crate::{Client, ClientProfile, Endpoint};

fn parse_retry_after_from_message(message: &str) -> Option<u64> {
    let lower = message.to_ascii_lowercase();
    let idx = lower.find("try again in")?;
    let remainder = lower[idx + "try again in".len()..].trim_start();
    let num_end = remainder.find(|c: char| !c.is_ascii_digit() && c != '.')?;
    let val_str = &remainder[..num_end];
    let unit_part = remainder[num_end..].trim_start();
    let val = val_str.parse::<f64>().ok()?;
    if unit_part.starts_with("ms") {
        Some(val as u64)
    } else if unit_part.starts_with('s') || unit_part.starts_with("sec") {
        Some((val * 1000.0) as u64)
    } else {
        None
    }
}

fn parse_responses_stream_error(
    error_val: &serde_json::Value,
    label: &'static str,
) -> ProviderError {
    let code = error_val.get("code").and_then(serde_json::Value::as_str);
    let message = error_val
        .get("message")
        .and_then(serde_json::Value::as_str)
        .filter(|m| !m.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            if let Some(code) = code {
                format!("Responses stream error: {code}")
            } else if let Some(s) = error_val.as_str() {
                s.to_string()
            } else {
                format!("Responses stream error: {error_val}")
            }
        });

    let retry_delay = parse_retry_after_from_message(&message);

    match code {
        Some("server_is_overloaded" | "slow_down") => {
            ProviderError::new(label, ProviderErrorKind::Unavailable, message)
                .with_status(503)
                .retryable(retry_delay.or(Some(2000)))
        }
        Some("rate_limit_exceeded") => {
            ProviderError::new(label, ProviderErrorKind::RateLimited, message)
                .with_status(429)
                .retryable(retry_delay)
        }
        Some("context_length_exceeded" | "context_window_exceeded") => {
            ProviderError::new(label, ProviderErrorKind::ContextOverflow, message).with_status(400)
        }
        Some("insufficient_quota" | "usage_not_included") => {
            ProviderError::new(label, ProviderErrorKind::Unavailable, message).with_status(402)
        }
        Some(
            "cyber_policy" | "misalignment_policy_violation" | "invalid_prompt" | "bio_policy",
        ) => ProviderError::new(label, ProviderErrorKind::InvalidRequest, message).with_status(400),
        _ => {
            let lower = message.to_ascii_lowercase();
            if lower.contains("overloaded") || lower.contains("capacity") {
                ProviderError::new(label, ProviderErrorKind::Unavailable, message)
                    .with_status(503)
                    .retryable(retry_delay.or(Some(2000)))
            } else if lower.contains("rate limit") {
                ProviderError::new(label, ProviderErrorKind::RateLimited, message)
                    .with_status(429)
                    .retryable(retry_delay)
            } else {
                let mut err = ProviderError::new(label, ProviderErrorKind::Protocol, message);
                if let Some(delay) = retry_delay {
                    err = err.retryable(Some(delay));
                }
                err
            }
        }
    }
}

fn decode_stream_payload(
    data: &str,
    label: &'static str,
) -> Result<serde_json::Value, ProviderError> {
    let value = serde_json::from_str::<serde_json::Value>(data).map_err(|error| {
        ProviderError::new(
            label,
            ProviderErrorKind::Decode,
            format!("Invalid JSON in Responses stream: {error}"),
        )
    })?;
    match value["type"].as_str() {
        Some("response.failed") => {
            let err_val = value
                .get("response")
                .and_then(|r| r.get("error"))
                .unwrap_or(&value["response"]);
            Err(parse_responses_stream_error(err_val, label))
        }
        Some("error") => {
            let err_val = value.get("error").unwrap_or(&value);
            Err(parse_responses_stream_error(err_val, label))
        }
        _ => {
            if let Some(err_val) = value.get("error")
                && !err_val.is_null()
            {
                return Err(parse_responses_stream_error(err_val, label));
            }
            Ok(value)
        }
    }
}

fn models_etag(headers: &http::header::HeaderMap) -> Option<String> {
    headers
        .get("x-models-etag")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// OpenAI Responses-API provider (ChatGPT Subscription backend).
pub struct OpenAiResponsesProvider {
    pub endpoint: Endpoint,
    pub reasoning_effort: Option<Effort>,
    /// Channel-scoped capability view. A trusted remote catalogue overrides the
    /// static baseline only for this provider/model route.
    pub capabilities: nuo_model_codec::ModelCapabilities,
    /// When `true`, attach ChatGPT Subscription headers (`originator: nuo` and
    /// `ChatGPT-Account-Id`).
    pub dialect: nuo_model_codec::OpenAiResponsesDialect,
    /// Whether upstream persists response state and accepts
    /// `previous_response_id`. Stateless DeepSeek and subscription backends
    /// force this off.
    pub store: bool,
    /// Route-scoped prompt-cache capabilities, defaults, and affinity.
    pub prompt_cache: crate::PromptCacheConfig,
    /// Pooled HTTP client reused across every request this provider makes.
    pub client: Client,
}

/// Canonical alias for the Responses protocol provider.
pub type ResponsesProvider = OpenAiResponsesProvider;

impl OpenAiResponsesProvider {
    pub fn new(
        credentials: std::sync::Arc<dyn CredentialSource>,
        model: String,
        base_url: &str,
    ) -> Self {
        let capabilities = nuo_model_codec::ModelCapabilities::for_channel(&model, None);
        Self {
            endpoint: Endpoint::with_credentials(credentials, model, base_url, "chatgpt"),
            client: Client::new(),
            reasoning_effort: None,
            capabilities,
            dialect: nuo_model_codec::OpenAiResponsesDialect::Standard,
            store: true,
            prompt_cache: crate::PromptCacheConfig::default(),
        }
    }

    /// Build a provider with static API key string.
    pub fn from_static_key(api_key: String, model: String, base_url: &str) -> Self {
        Self::new(nuo_model_codec::static_credential(api_key), model, base_url)
    }

    /// Build a provider with dynamic credentials.
    pub fn with_credentials(
        credentials: std::sync::Arc<dyn CredentialSource>,
        model: String,
        base_url: &str,
    ) -> Self {
        Self::new(credentials, model, base_url)
    }

    pub fn with_user_agent(mut self, user_agent: &str) -> Self {
        self.endpoint.client_profile = ClientProfile::from_user_agent(user_agent);
        self
    }

    pub fn with_client_profile(mut self, profile: impl Into<ClientProfile>) -> Self {
        self.endpoint.client_profile = profile.into();
        self
    }

    pub fn with_id(mut self, id: String) -> Self {
        self.endpoint.set_id(id);
        self
    }

    pub fn with_reasoning_effort(mut self, effort: Option<Effort>) -> Self {
        self.reasoning_effort = effort;
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

    pub fn with_dialect(mut self, dialect: nuo_model_codec::OpenAiResponsesDialect) -> Self {
        self.dialect = dialect;
        if dialect != nuo_model_codec::OpenAiResponsesDialect::Standard {
            self.store = false;
        }
        self
    }

    pub fn with_store(mut self, store: bool) -> Self {
        self.store = store;
        self
    }

    pub fn with_prompt_cache(mut self, prompt_cache: crate::PromptCacheConfig) -> Self {
        self.prompt_cache = prompt_cache;
        self
    }

    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.endpoint = self.endpoint.with_session_id(session_id);
        self
    }

    /// Human-readable backend label for error messages and logs.
    fn label(&self) -> &'static str {
        match self.dialect {
            nuo_model_codec::OpenAiResponsesDialect::Copilot => "Copilot",
            nuo_model_codec::OpenAiResponsesDialect::DeepSeek => "DeepSeek",
            nuo_model_codec::OpenAiResponsesDialect::ChatGpt => "ChatGPT",
            nuo_model_codec::OpenAiResponsesDialect::Standard => "OpenAI Responses",
        }
    }

    /// Build the HTTP request for the given payload and resolved credentials.
    fn build_request_for_auth(
        &self,
        body: &serde_json::Value,
        auth: &ResolvedAuth,
        turn_state: Option<&str>,
    ) -> crate::request::RequestBuilder {
        let mut req =
            crate::request::RequestBuilder::new(http::Method::POST, self.endpoint.base_url())
                .header(http::header::USER_AGENT, self.endpoint.user_agent())
                .json(body);
        let copilot = self.dialect == nuo_model_codec::OpenAiResponsesDialect::Copilot;
        let chatgpt = self.dialect == nuo_model_codec::OpenAiResponsesDialect::ChatGpt;
        let is_copilot_vision = copilot && request::has_input_image(body);
        let account_id = auth
            .extension::<nuo_model_codec::ChatGptAuthMetadata>()
            .map(|m| m.account_id.as_str());
        for (name, value) in request::headers(
            auth.token.expose_secret(),
            account_id,
            copilot,
            chatgpt,
        ) {
            req = req.header(name, value);
        }
        if is_copilot_vision {
            req = req.header("Copilot-Vision-Request", "true");
        }
        if chatgpt {
            let session_id = self
                .prompt_cache
                .routing_key()
                .filter(|id| !id.trim().is_empty())
                .unwrap_or_else(|| self.endpoint.effective_session_id());
            req = req
                .header("session-id", session_id)
                .header("thread-id", session_id);
            if let Some(turn_state) = turn_state {
                req = req.header("x-codex-turn-state", turn_state);
            }
            req = req.header(
                "x-codex-routing-hint",
                format!("model={}", self.endpoint.model),
            );
        }
        for (name, value) in self.endpoint.headers() {
            if !copilot
                || !crate::COPILOT_CLIENT_HEADERS
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
        turn_context: &nuo_model_codec::ProviderTurnContext,
        telemetry: &nuo_model_codec::TransportTelemetry,
    ) -> Result<crate::egress::HttpResponse, ProviderError> {
        let auth = self
            .endpoint
            .resolve_auth()
            .await
            .map_err(|e| ProviderError::authentication(self.label(), e))?;
        let acct_id = auth
            .extension::<nuo_model_codec::ChatGptAuthMetadata>()
            .map(|m| m.account_id.as_str());
        let mut turn_state = turn_context.slot(format!(
            "codex:{}:{}:{:?}",
            self.endpoint.base_url(),
            self.endpoint.model,
            acct_id
        ));
        let mut req = self
            .build_request_for_auth(body, &auth, turn_state.get().map(String::as_str))
            .with_telemetry(telemetry.clone());
        if !is_stream {
            req = req.timeout(self.client.request_timeout());
        }
        let mut response = self.client.send_raw(req, self.label()).await?;

        if response.status == http::StatusCode::UNAUTHORIZED && self.endpoint.is_oauth() {
            tracing::warn!(
                provider = %self.endpoint.id,
                model = %self.endpoint.model,
                "OAuth token rejected by {} (401 Unauthorized); attempting force-refresh and retry",
                self.label()
            );
            let refreshed_auth = self
                .endpoint
                .force_refresh_auth_after(&auth.token)
                .await
                .map_err(|error| ProviderError::authentication(self.label(), error))?;
            let refreshed_acct = refreshed_auth
                .extension::<nuo_model_codec::ChatGptAuthMetadata>()
                .map(|m| m.account_id.as_str());
            if refreshed_acct != acct_id {
                turn_state = turn_context.slot(format!(
                    "codex:{}:{}:{:?}",
                    self.endpoint.base_url(),
                    self.endpoint.model,
                    refreshed_acct
                ));
            }
            let mut retry_req = self
                .build_request_for_auth(body, &refreshed_auth, turn_state.get().map(String::as_str))
                .with_telemetry(telemetry.clone());
            if !is_stream {
                retry_req = retry_req.timeout(self.client.request_timeout());
            }
            response = self.client.send_raw(retry_req, self.label()).await?;
        }

        let response = ensure_success(response, self.label(), Some(&self.endpoint.model)).await?;
        if self.dialect == nuo_model_codec::OpenAiResponsesDialect::ChatGpt
            && let Some(value) = response
                .headers
                .get("x-codex-turn-state")
                .and_then(|value| value.to_str().ok())
                .filter(|value| !value.is_empty())
        {
            // The first token is immutable for this round, including retries.
            let _ = turn_state.set(value.to_string());
        }
        Ok(response)
    }

    fn build_body(
        &self,
        request: ModelRequest,
        stream: bool,
    ) -> Result<serde_json::Value, ProviderError> {
        let cache_plan = self
            .prompt_cache
            .resolve(&request)
            .map_err(|e| ProviderError::invalid_request(self.label(), e))?;
        let ModelRequest {
            instructions,
            mut messages,
            tool_specs,
            delivery,
            temporary_context,
            ..
        } = request;
        messages.extend(temporary_context);
        request::body_with_capabilities(
            messages,
            request::BodyInput {
                model: &self.endpoint.model,
                stream,
                instructions: Some(&instructions),
                tool_specs: (!tool_specs.is_empty()).then_some(tool_specs.as_slice()),
                reasoning_effort: self.reasoning_effort,
                delivery: &delivery,
                store: self.store,
                cache_plan: &cache_plan,
            },
            &self.capabilities,
        )
        .map_err(|error| ProviderError::invalid_request(self.label(), error.to_string()))
    }

    /// The ChatGPT Subscription Responses endpoint is streaming-only. Collect
    /// its canonical event stream for callers of the provider's completion
    /// interface instead of maintaining a second, unsupported wire path.
    async fn collect_streaming_completion(
        &self,
        request: ModelRequest,
    ) -> Result<nuo_model_codec::ProviderCompletion, ProviderError> {
        let mut stream = self.stream_chat_events(request).await?;
        let mut streamed_usage = None;
        let mut completion_meta = None;

        while let Some(event) = stream.next().await {
            let event = event?;
            match event {
                ProviderStreamEvent::Usage(usage) => streamed_usage = Some(usage),
                ProviderStreamEvent::Completed(mut meta) => {
                    if completion_meta.is_some() {
                        return Err(ProviderError::protocol(
                            self.label(),
                            "Responses stream emitted more than one terminal completion event.",
                        ));
                    }
                    if meta.usage.is_none() {
                        meta.usage = streamed_usage;
                    }
                    completion_meta = Some(meta);
                }
                ProviderStreamEvent::ModelCatalogEtag(_)
                | ProviderStreamEvent::TextDelta(_)
                | ProviderStreamEvent::ReasoningDelta(_)
                | ProviderStreamEvent::ToolCallDelta { .. } => {
                    if completion_meta.is_some() {
                        return Err(ProviderError::protocol(
                            self.label(),
                            "Responses stream emitted data after its terminal completion event.",
                        ));
                    }
                }
            }
        }

        let meta = completion_meta.ok_or_else(|| {
            ProviderError::protocol(
                self.label(),
                "Responses stream ended without a terminal completion event.",
            )
        })?;
        let output = meta
            .artifacts
            .as_ref()
            .and_then(|artifacts| {
                artifacts.get(nuo_model_codec::OPENAI_RESPONSE_OUTPUT_ARTIFACT_KEY)
            })
            .filter(|output| output.as_array().is_some_and(|items| !items.is_empty()))
            .ok_or_else(|| {
                ProviderError::protocol(
                    self.label(),
                    "Responses stream completed without a valid output artifact.",
                )
            })?;
        let message = response::message(output);

        Ok(nuo_model_codec::ProviderCompletion { message, meta })
    }
}

#[async_trait]
impl Provider for OpenAiResponsesProvider {
    fn provider_id(&self) -> String {
        self.endpoint.id.clone()
    }

    fn model(&self) -> String {
        self.endpoint.model.clone()
    }

    fn wire_protocol(&self) -> Option<nuo_model_codec::WireProtocol> {
        Some(nuo_model_codec::WireProtocol::Responses)
    }

    fn effort(&self) -> Option<Effort> {
        self.reasoning_effort
    }

    fn model_capabilities(&self) -> nuo_model_codec::ModelCapabilities {
        self.capabilities.clone()
    }

    fn route_fingerprint(&self) -> nuo_model_codec::RouteFingerprint {
        nuo_model_codec::RouteFingerprint(format!(
            "openai-responses:{}:{}:{}",
            self.endpoint.base_url,
            self.endpoint.model,
            if self.store { "stored" } else { "local" }
        ))
    }

    fn continuation_mode(&self) -> nuo_model_codec::ContinuationMode {
        if self.store {
            nuo_model_codec::ContinuationMode::RemoteStored
        } else {
            nuo_model_codec::ContinuationMode::OpaqueReplay
        }
    }

    fn prompt_hints(&self) -> ProviderPromptHints {
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
        if self.dialect == nuo_model_codec::OpenAiResponsesDialect::ChatGpt {
            return self.collect_streaming_completion(request).await;
        }
        let label = self.label();
        // `build_body` consumes the request, so the attempt's telemetry handle
        // is lifted out first (ADR-0232).
        let transport_telemetry = request.transport_telemetry.clone();
        let turn_context = Arc::clone(&request.turn_context);
        let body = self.build_body(request, false)?;
        let resp = self
            .send_request(&body, false, &turn_context, &transport_telemetry)
            .await?;
        let value: serde_json::Value = decode_response_json(resp, label).await?;
        if let Some(err) = value.get("error") {
            return Err(ProviderError::new(
                label,
                ProviderErrorKind::Protocol,
                format!("{label} Error: {}", err),
            ));
        }
        let output = value
            .get("output")
            .and_then(serde_json::Value::as_array)
            .filter(|items| !items.is_empty())
            .ok_or_else(|| {
                ProviderError::protocol(label, "Responses completion contains no output items.")
            })?;
        let output = serde_json::Value::Array(output.clone());
        let mut artifacts = serde_json::Map::new();
        artifacts.insert(
            nuo_model_codec::OPENAI_RESPONSE_OUTPUT_ARTIFACT_KEY.to_string(),
            output.clone(),
        );
        let continuation =
            value["id"]
                .as_str()
                .map(|response_id| nuo_model_codec::ContinuationCursor {
                    route: self.route_fingerprint(),
                    local_head: String::new(),
                    response_id: response_id.to_string(),
                });
        Ok(nuo_model_codec::ProviderCompletion {
            message: response::message(&output),
            meta: nuo_model_codec::ProviderCompletionMeta {
                usage: response::usage(&value["usage"]),
                artifacts: Some(artifacts),
                continuation,
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
        let label = self.label();
        // `build_body` consumes the request, so the attempt's telemetry handle
        // is lifted out first (ADR-0232).
        let transport_telemetry = request.transport_telemetry.clone();
        let turn_context = Arc::clone(&request.turn_context);
        let body = self.build_body(request, true)?;
        let resp = self
            .send_request(&body, true, &turn_context, &transport_telemetry)
            .await?;
        let stream = crate::sse::data_payloads(resp, label).map(|item| {
            let data = item?;
            let value = decode_stream_payload(&data, label)?;
            // Accumulate only output_text deltas on the text-only path.
            if value["type"].as_str() == Some("response.output_text.delta") {
                Ok(value["delta"].as_str().unwrap_or("").to_string())
            } else {
                Ok(String::new())
            }
        });
        Ok(stream.boxed())
    }

    async fn stream_chat_events(
        &self,
        request: ModelRequest,
    ) -> Result<
        BoxStream<'static, Result<ProviderStreamEvent, nuo_model_codec::ProviderError>>,
        nuo_model_codec::ProviderError,
    > {
        let label = self.label();
        // `build_body` consumes the request, so the attempt's telemetry handle
        // is lifted out first (ADR-0232).
        let transport_telemetry = request.transport_telemetry.clone();
        let turn_context = Arc::clone(&request.turn_context);
        let body = self.build_body(request, true)?;
        let resp = self
            .send_request(&body, true, &turn_context, &transport_telemetry)
            .await?;
        let model_catalog_etag = models_etag(&resp.headers);

        // One stateful parser threads the function-call item state across the
        // whole stream; each SSE payload becomes zero or more events. Terminal
        // usage arrives as a `Usage` event here, which the harness books
        // directly (mirrors the chat-completions streaming path) — no stashing
        // into the turn is needed.
        let parser = Arc::new(Mutex::new(response::ResponsesStream::new()));
        let route = self.route_fingerprint();
        let stream = crate::sse::data_payloads(resp, label).map(move |item| {
            let data = item?;
            let value = decode_stream_payload(&data, label)?;
            let mut p = parser.lock().unwrap_or_else(|e| e.into_inner());
            let mut events = p
                .parse_value(&value)
                .map_err(|error| ProviderError::protocol(label, error))?;
            for event in &mut events {
                if let ProviderStreamEvent::Completed(meta) = event
                    && let Some(artifacts) = meta.artifacts.as_mut()
                    && let Some(response_id) = artifacts
                        .remove(nuo_model_codec::OPENAI_RESPONSE_ID_ARTIFACT_KEY)
                        .and_then(|value| value.as_str().map(str::to_string))
                {
                    meta.continuation = Some(nuo_model_codec::ContinuationCursor {
                        route: route.clone(),
                        local_head: String::new(),
                        response_id,
                    });
                }
            }
            Ok::<_, ProviderError>(events)
        });
        let events = stream.flat_map(|result| match result {
            Ok(events) => futures::stream::iter(events.into_iter().map(Ok).collect::<Vec<_>>()),
            Err(error) => futures::stream::iter(vec![Err(error)]),
        });
        let controls = futures::stream::iter(
            model_catalog_etag
                .into_iter()
                .map(|etag| Ok(ProviderStreamEvent::ModelCatalogEtag(etag))),
        );
        Ok(controls.chain(events).boxed())
    }
}

#[cfg(test)]
mod stream_protocol_tests {
    use super::*;

    /// A throwaway handle for tests that assert on routing, not telemetry.
    fn telemetry() -> nuo_model_codec::TransportTelemetry {
        nuo_model_codec::TransportTelemetry::new()
    }

    #[tokio::test]
    async fn chatgpt_routing_state_is_sticky_only_within_one_round() {
        use mockito::{Matcher, Server};
        let mut server = Server::new_async().await;
        let provider = OpenAiResponsesProvider::from_static_key(
            "test".into(),
            "gpt-6-astra".into(),
            &server.url(),
        )
        .with_dialect(nuo_model_codec::OpenAiResponsesDialect::ChatGpt)
        .with_session_id("session-astra");
        let round = nuo_model_codec::ProviderTurnContext::default();
        let body = serde_json::json!({"model": "gpt-6-astra"});

        // First response establishes routing; later responses must not replace it.
        for (request_token, response_token) in [
            (None, "route-a"),
            (Some("route-a"), "route-b"),
            (Some("route-a"), "route-c"),
        ] {
            let mock = server
                .mock("POST", "/")
                .match_header("session-id", "session-astra")
                .match_header("thread-id", "session-astra")
                .match_header(
                    "x-codex-turn-state",
                    request_token.map(Matcher::from).unwrap_or(Matcher::Missing),
                )
                .with_status(200)
                .with_header("x-codex-turn-state", response_token)
                .create_async()
                .await;
            provider
                .send_request(&body, true, &round, &telemetry())
                .await
                .unwrap();
            mock.assert_async().await;
            mock.remove_async().await;
        }
        // A new user round (or a concurrent auxiliary request) has no old token.
        let mock = server
            .mock("POST", "/")
            .match_header("x-codex-turn-state", Matcher::Missing)
            .with_status(200)
            .create_async()
            .await;
        provider
            .send_request(
                &body,
                true,
                &nuo_model_codec::ProviderTurnContext::default(),
                &telemetry(),
            )
            .await
            .unwrap();
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn chatgpt_routing_state_ignores_failed_responses_and_other_routes() {
        use mockito::{Matcher, Server};
        let mut server = Server::new_async().await;
        let round = nuo_model_codec::ProviderTurnContext::default();
        let body = serde_json::json!({});
        for (dialect, status, expected) in [
            (nuo_model_codec::OpenAiResponsesDialect::ChatGpt, 503, None),
            (nuo_model_codec::OpenAiResponsesDialect::ChatGpt, 200, None),
            (nuo_model_codec::OpenAiResponsesDialect::Standard, 200, None),
            (
                nuo_model_codec::OpenAiResponsesDialect::ChatGpt,
                200,
                Some("route-a"),
            ),
        ] {
            let provider = OpenAiResponsesProvider::from_static_key(
                "test".into(),
                "gpt-6-astra".into(),
                &server.url(),
            )
            .with_dialect(dialect);
            let mock = server
                .mock("POST", "/")
                .match_header(
                    "x-codex-turn-state",
                    expected.map(Matcher::from).unwrap_or(Matcher::Missing),
                )
                .with_status(status)
                .with_header("x-codex-turn-state", "route-a")
                .create_async()
                .await;
            let response = provider
                .send_request(&body, true, &round, &telemetry())
                .await;
            assert_eq!(response.is_ok(), status == 200);
            mock.assert_async().await;
            mock.remove_async().await;
        }
    }

    #[tokio::test]
    async fn chatgpt_routing_state_follows_oauth_account_on_retry() {
        use futures::future::BoxFuture;
        use mockito::{Matcher, Server};
        #[derive(Debug)]
        struct RefreshingAuth(&'static str);
        impl CredentialSource for RefreshingAuth {
            fn resolve_auth(&self) -> BoxFuture<'_, Result<ResolvedAuth, String>> {
                Box::pin(async {
                    Ok(ResolvedAuth::new("old").with_extension(nuo_model_codec::ChatGptAuthMetadata {
                        account_id: "account-a".to_string(),
                    }))
                })
            }
            fn force_refresh(&self) -> BoxFuture<'_, Result<ResolvedAuth, String>> {
                Box::pin(async {
                    Ok(ResolvedAuth::new("new").with_extension(nuo_model_codec::ChatGptAuthMetadata {
                        account_id: self.0.to_string(),
                    }))
                })
            }
            fn is_oauth(&self) -> bool {
                true
            }
        }
        for account in ["account-a", "account-b"] {
            let mut server = Server::new_async().await;
            let provider = OpenAiResponsesProvider::with_credentials(
                Arc::new(RefreshingAuth(account)),
                "gpt-6-astra".into(),
                &server.url(),
            )
            .with_dialect(nuo_model_codec::OpenAiResponsesDialect::ChatGpt);
            let round = nuo_model_codec::ProviderTurnContext::default();
            let body = serde_json::json!({});
            let warmup = server
                .mock("POST", "/")
                .with_status(200)
                .with_header("x-codex-turn-state", "route-a")
                .create_async()
                .await;
            provider
                .send_request(&body, true, &round, &telemetry())
                .await
                .unwrap();
            warmup.assert_async().await;
            warmup.remove_async().await;
            let rejected = server
                .mock("POST", "/")
                .match_header("authorization", "Bearer old")
                .match_header("x-codex-turn-state", "route-a")
                .with_status(401)
                .create_async()
                .await;
            let retried = server
                .mock("POST", "/")
                .match_header("authorization", "Bearer new")
                .match_header("chatgpt-account-id", account)
                .match_header(
                    "x-codex-turn-state",
                    if account == "account-a" {
                        Matcher::from("route-a")
                    } else {
                        Matcher::Missing
                    },
                )
                .with_status(200)
                .create_async()
                .await;
            provider
                .send_request(&body, true, &round, &telemetry())
                .await
                .unwrap();
            rejected.assert_async().await;
            retried.assert_async().await;
        }
    }

    #[test]
    fn malformed_sse_payload_is_a_decode_error() {
        let error = decode_stream_payload("{", "ChatGPT").unwrap_err();
        assert_eq!(error.kind(), ProviderErrorKind::Decode);
    }

    #[test]
    fn terminal_sse_failures_are_not_silently_ignored() {
        for payload in [
            r#"{"type":"response.failed","response":{"error":{"message":"denied"}}}"#,
            r#"{"type":"error","error":{"message":"denied"}}"#,
        ] {
            let error = decode_stream_payload(payload, "ChatGPT").unwrap_err();
            assert_eq!(error.kind(), ProviderErrorKind::Protocol);
        }
    }

    #[test]
    fn server_is_overloaded_and_slow_down_are_classified_as_unavailable_and_retryable() {
        for payload in [
            r#"{"type":"error","error":{"code":"server_is_overloaded","message":"Our servers are currently overloaded. Please try again later.","type":"service_unavailable_error"}}"#,
            r#"{"type":"response.failed","response":{"error":{"code":"slow_down","message":"Please slow down."}}}"#,
        ] {
            let error = decode_stream_payload(payload, "ChatGPT").unwrap_err();
            assert_eq!(error.kind(), ProviderErrorKind::Unavailable);
            assert_eq!(error.status(), Some(503));
            assert!(matches!(
                error.retry_disposition(),
                nuo_model_codec::RetryDisposition::Retry { .. }
            ));
        }
    }

    #[test]
    fn rate_limit_with_parsed_duration_is_classified_as_rate_limited_and_retryable() {
        let payload = r#"{"type":"response.failed","response":{"error":{"code":"rate_limit_exceeded","message":"Rate limit reached. Please try again in 11.054s."}}}"#;
        let error = decode_stream_payload(payload, "ChatGPT").unwrap_err();
        assert_eq!(error.kind(), ProviderErrorKind::RateLimited);
        assert_eq!(error.status(), Some(429));
        assert_eq!(
            error.retry_disposition(),
            nuo_model_codec::RetryDisposition::Retry {
                retry_after_ms: Some(11054)
            }
        );
    }

    #[test]
    fn context_length_exceeded_is_classified_as_context_overflow() {
        let payload = r#"{"type":"response.failed","response":{"error":{"code":"context_length_exceeded","message":"Your input exceeds the context window."}}}"#;
        let error = decode_stream_payload(payload, "ChatGPT").unwrap_err();
        assert_eq!(error.kind(), ProviderErrorKind::ContextOverflow);
        assert_eq!(error.status(), Some(400));
    }

    #[test]
    fn models_etag_header_becomes_a_catalog_control_event() {
        let mut headers = http::header::HeaderMap::new();
        headers.insert("X-Models-Etag", "  etag-42  ".parse().unwrap());
        assert_eq!(models_etag(&headers).as_deref(), Some("etag-42"));
    }

    #[test]
    fn deepseek_uses_stateless_opaque_replay() {
        let provider = OpenAiResponsesProvider::from_static_key(
            "test-key".to_string(),
            "deepseek-v4-flash".to_string(),
            "https://api.deepseek.com/v1/responses",
        )
        .with_dialect(nuo_model_codec::OpenAiResponsesDialect::DeepSeek);

        assert!(!provider.store);
        assert_eq!(
            provider.continuation_mode(),
            nuo_model_codec::ContinuationMode::OpaqueReplay
        );
    }

    #[test]
    fn workspace_scoped_credential_carries_the_org_header() {
        let provider = OpenAiResponsesProvider::from_static_key(
            "st-token".to_string(),
            "gpt-5.2".to_string(),
            "https://opencode.ai/inference/openai/v1/responses",
        );
        let auth = nuo_model_codec::ResolvedAuth::new("st-token").with_extension(
            nuo_model_codec::OpencodeAuthMetadata {
                org_id: "wrk_workspace_1".to_string(),
            },
        );
        let req = provider
            .build_request_for_auth(&serde_json::json!({"model": "gpt-5.2"}), &auth, None)
            .build("OpenAI")
            .expect("request builds");
        assert_eq!(
            req.headers
                .get("x-opencode-org-id")
                .and_then(|value| value.to_str().ok())
                .expect("workspace header present"),
            "wrk_workspace_1"
        );
    }
}
