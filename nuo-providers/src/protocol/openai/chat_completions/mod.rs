//! OpenAI-compatible chat-completions provider with native tool-call support
//! and a streaming filter that strips tool-call "echo" text (GLM/Qwen style).
//!
//! A thin executor over the pure [`request`], [`response`], and [`echo`]
//! layers plus the shared transport helpers. The provider struct holds only
//! the shared [`Endpoint`] (connection config) — every wire-format detail lives in a pure, independently
//! testable module.
//!
//! Module layout (mirrors the Google and Anthropic providers):
//!   - [`request`] — body / headers / message conversion (pure, no I/O)
//!   - [`response`] — usage, message, and stream-payload parsing (pure)
//!   - [`echo`] — the tool-call "echo" suppression filter (stateful, no I/O)
//!   - this file — the [`OpenAiChatCompletionsProvider`] executor + `Provider` impl

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use nuo_contracts::{
    CredentialSource, Effort, ModelRequest, Provider, ProviderError, ProviderErrorKind,
    ProviderPromptHints, ProviderStreamEvent, ResolvedAuth,
};
use std::sync::Arc;
use std::sync::Mutex;

use crate::transport::{decode_response_json, ensure_success};
use crate::{Client, ClientProfile, Endpoint};

pub mod echo;
pub mod request;
pub mod response;

/// OpenAI-compatible chat-completions provider.
///
/// Embeds the shared [`Endpoint`] plus the optional OpenAI
/// `reasoning_effort` override.
pub struct OpenAiChatCompletionsProvider {
    pub endpoint: Endpoint,
    pub reasoning_effort: Option<Effort>,
    /// Route-scoped prompt-cache capabilities, defaults, and affinity.
    pub prompt_cache: crate::PromptCacheConfig,
    /// Channel-scoped capability view. A trusted remote catalogue overrides the
    /// static baseline only for this provider/model route.
    pub capabilities: nuo_contracts::ModelCapabilities,
    /// When `true`, inject GitHub Copilot's required per-request headers
    /// (`x-initiator`, `Openai-Intent`, `X-GitHub-Api-Version`) in addition to
    /// the bearer. Flipped on by the catalog for Copilot OAuth channels that
    /// speak the chat-completions surface (the GPT-4o family and Copilot Free
    /// accounts, which do not have Responses-API access). Mirrors the same flag
    /// on [`OpenAiResponsesProvider`](crate::OpenAiResponsesProvider).
    pub dialect: nuo_contracts::OpenAiChatDialect,
    /// Pooled HTTP client reused across every request this provider makes.
    pub client: Client,
    /// Transport pipeline for request and stream transformation.
    pub pipeline: Arc<crate::pipeline::TransportPipeline>,
}

/// Canonical alias for the Chat Completions protocol provider.
pub type ChatCompletionsProvider = OpenAiChatCompletionsProvider;

impl OpenAiChatCompletionsProvider {
    pub fn new(api_key: String, model: String) -> Self {
        Self::with_base_url(api_key, model, "https://api.openai.com/v1/chat/completions")
    }

    pub fn with_base_url(api_key: String, model: String, base_url: &str) -> Self {
        Self::with_base_url_and_user_agent(api_key, model, base_url, crate::NUO_USER_AGENT)
    }

    pub fn with_base_url_and_user_agent(
        api_key: String,
        model: String,
        base_url: &str,
        user_agent: &str,
    ) -> Self {
        let capabilities = nuo_contracts::ModelCapabilities::for_channel(&model, None);
        Self {
            endpoint: Endpoint::from_static_key(api_key, model, base_url, "openai")
                .with_user_agent(user_agent),
            client: Client::new(),
            reasoning_effort: None,
            prompt_cache: crate::PromptCacheConfig::default(),
            capabilities,
            dialect: nuo_contracts::OpenAiChatDialect::Standard,
            pipeline: Arc::new(crate::pipeline::TransportPipeline::default()),
        }
    }

    /// Build a provider with dynamic credentials.
    pub fn with_credentials(
        credentials: std::sync::Arc<dyn CredentialSource>,
        model: String,
        base_url: &str,
        client_profile: impl Into<ClientProfile>,
    ) -> Self {
        let capabilities = nuo_contracts::ModelCapabilities::for_channel(&model, None);
        Self {
            endpoint: Endpoint::with_credentials(credentials, model, base_url, "openai")
                .with_client_profile(client_profile),
            client: Client::new(),
            reasoning_effort: None,
            prompt_cache: crate::PromptCacheConfig::default(),
            capabilities,
            dialect: nuo_contracts::OpenAiChatDialect::Standard,
            pipeline: Arc::new(crate::pipeline::TransportPipeline::default()),
        }
    }

    /// Stamp the attribution id (the registry does this with the channel entry
    /// id). Returns `self` for chaining.
    pub fn with_id(mut self, id: String) -> Self {
        self.endpoint.set_id(id);
        self
    }

    /// Set the OpenAI `reasoning_effort` override for models that expose it.
    /// `None` keeps the provider default.
    pub fn with_reasoning_effort(mut self, effort: Option<Effort>) -> Self {
        self.reasoning_effort = effort;
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

    /// Attach the effective provider-channel capability view.
    pub fn with_model_capabilities(
        mut self,
        capabilities: nuo_contracts::ModelCapabilities,
    ) -> Self {
        self.capabilities = capabilities;
        self
    }

    pub fn with_dialect(mut self, dialect: nuo_contracts::OpenAiChatDialect) -> Self {
        self.dialect = dialect;
        // The default pass-through pipeline owns the plain wire's dialect-
        // attributed headers (Copilot client headers, OpenRouter attribution);
        // `request::headers` remains the single declaration site and is
        // consulted per-request with the resolved bearer. A pipeline attached
        // later via `with_pipeline` replaces this one wholesale (ADR-0271).
        self.pipeline = Arc::new(
            crate::pipeline::TransportPipeline::default().with_pass_through_headers(
                move |token| {
                    request::headers(token, dialect)
                        .into_iter()
                        .filter(|(name, _)| *name != "Authorization")
                        .map(|(name, value)| (name.to_string(), value))
                        .collect()
                },
            ),
        );
        self
    }

    /// Declare the model's catalog provenance: the `source` the provider names
    /// (`system` / `custom`) and the presentation label. Both are wire-optional
    /// — only a dialect whose surface declares a matching
    /// [`ModelCarrier`](nuo_contracts::wire_surface::ModelCarrier) binding
    /// stamps them. The wire id remains the identity (ADR-0131).
    pub fn with_catalog_provenance(
        mut self,
        catalog_source: impl Into<String>,
        display_name: impl Into<String>,
    ) -> Self {
        self.endpoint.catalog_source = catalog_source.into();
        self.endpoint.display_name = display_name.into();
        self
    }

    /// Human-readable backend label for error messages and logs. The OpenAI
    /// chat-completions provider serves both the generic OpenAI-compatible
    /// surface and the GitHub Copilot chat surface behind one wire format;
    /// surfacing the right name in errors ("Copilot HTTP 400" vs "OpenAI HTTP
    /// 400") is essential for diagnosing which backend rejected a request.
    fn label(&self) -> &'static str {
        match self.dialect {
            nuo_contracts::OpenAiChatDialect::Copilot => "Copilot",
            nuo_contracts::OpenAiChatDialect::OpenRouter => "OpenRouter",
            nuo_contracts::OpenAiChatDialect::Qoder => "Qoder",
            nuo_contracts::OpenAiChatDialect::Standard => "OpenAI",
        }
    }

    /// Apply the executor-owned envelope to a planned request: endpoint
    /// client-profile headers, session affinity, and auth-derived scoping.
    ///
    /// Wire headers (bearer, dialect signatures) come from the pipeline's
    /// [`OutboundPlan`](crate::pipeline::OutboundPlan) — the executor never
    /// decides the wire shape (ADR-0271). Kept `pub(crate)` for wire tests.
    #[allow(dead_code)]
    fn build_request_for_auth(
        &self,
        body: &serde_json::Value,
        auth: &ResolvedAuth,
    ) -> crate::request::RequestBuilder {
        let mut req =
            crate::request::RequestBuilder::new(http::Method::POST, self.endpoint.base_url())
                .header(http::header::USER_AGENT, self.endpoint.user_agent())
                .json(body);
        let copilot = self.dialect == nuo_contracts::OpenAiChatDialect::Copilot;
        for (name, value) in request::headers(auth.token.expose_secret(), self.dialect) {
            req = req.header(name, value);
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

    /// Attach an external phased transport pipeline to this provider.
    ///
    /// The pipeline plans the complete outbound request (URL, body bytes,
    /// header set) via [`TransportPipeline::plan_request`]; this provider only
    /// executes plans (ADR-0271). Attaching a shaped pipeline is what turns a
    /// plain chat-completions provider into a signed-surface one.
    pub fn with_pipeline(mut self, pipeline: crate::pipeline::TransportPipeline) -> Self {
        self.pipeline = std::sync::Arc::new(pipeline);
        self
    }

    /// Send a request with automatic token resolution, timeout stamping,
    /// and reactive force-refresh on HTTP 401 Unauthorized for OAuth channels.
    ///
    /// The wire is never built here: the attached pipeline plans the complete
    /// outbound request ([`OutboundPlan`](crate::pipeline::OutboundPlan)) and
    /// the executor only executes it (ADR-0271 §1). There is no branch on
    /// wire shape — a pass-through pipeline plans the plain chat-completions
    /// wire, a shaped pipeline plans its dialect's wire.
    async fn send_request(
        &self,
        body: &serde_json::Value,
        is_stream: bool,
        telemetry: &nuo_contracts::TransportTelemetry,
    ) -> Result<crate::egress::HttpResponse, ProviderError> {
        let auth = self
            .endpoint
            .resolve_auth()
            .await
            .map_err(|e| ProviderError::authentication(self.label(), e))?;

        self.pipeline.preflight_assert(&auth)?;

        let mut req = self.execute_plan(body, &auth, telemetry)?;
        if !is_stream {
            req = req.timeout(self.client.request_timeout());
        }
        let response = self.client.send_raw(req, self.label()).await?;

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
            let mut retry_req = self.execute_plan(body, &refreshed_auth, telemetry)?;
            if !is_stream {
                retry_req = retry_req.timeout(self.client.request_timeout());
            }
            return self.client.send(retry_req, self.label()).await;
        }

        ensure_success(response, self.label(), Some(&self.endpoint.model)).await
    }

    /// Plan the outbound request via the pipeline and stamp the executor-owned
    /// envelope (method, user agent, client-profile headers, auth-scoped
    /// headers, session affinity, body, telemetry).
    ///
    /// Header ownership split (ADR-0271 §1): the plan stamps dialect wire
    /// headers (bearer, COSY signature set); the executor stamps endpoint
    /// profile headers and auth-derived scoping headers — the latter are
    /// typed-`ResolvedAuth` projections shared by every protocol, not dialect
    /// wire.
    ///
    /// Public so golden-wire integration tests ([INV-WIRE-01], ADR-0271) in
    /// downstream crates can observe the exact outbound request.
    pub fn execute_plan(
        &self,
        body: &serde_json::Value,
        auth: &ResolvedAuth,
        telemetry: &nuo_contracts::TransportTelemetry,
    ) -> Result<crate::request::RequestBuilder, ProviderError> {
        let plan = self
            .pipeline
            .plan_request(self.endpoint.base_url(), body, auth)?;
        let seed = crate::request::RequestBuilder::new(http::Method::POST, plan.url)
            .header(http::header::USER_AGENT, self.endpoint.user_agent());
        let stamped = (plan.stamp_headers)(seed)?;
        let mut req = stamped
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(plan.body);
        for (name, value) in self.endpoint.headers() {
            req = req.header(name, value);
        }
        req = self
            .endpoint
            .attach_session_affinity_headers(req, self.prompt_cache.routing_key());
        Ok(self
            .endpoint
            .attach_auth_scoped_headers(req, auth)
            .with_telemetry(telemetry.clone()))
    }
}

#[async_trait]
impl Provider for OpenAiChatCompletionsProvider {
    fn provider_id(&self) -> String {
        self.endpoint.id.clone()
    }

    fn model(&self) -> String {
        self.endpoint.model.clone()
    }

    fn wire_protocol(&self) -> Option<nuo_contracts::WireProtocol> {
        Some(nuo_contracts::WireProtocol::ChatCompletions)
    }

    fn effort(&self) -> Option<Effort> {
        self.reasoning_effort
    }

    fn model_capabilities(&self) -> nuo_contracts::ModelCapabilities {
        self.capabilities.clone()
    }

    fn prompt_hints(&self) -> ProviderPromptHints {
        // No protocol hint: the OpenAI wire surface uses native tool calls by
        // construction, and the `ToolCallEchoFilter` deterministically strips
        // any text-mirrored call regardless of prompting. An in-prompt note
        // would only restate facts the model already has and the harness
        // already enforces.
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
    ) -> Result<nuo_contracts::ProviderCompletion, nuo_contracts::ProviderError> {
        let cache_plan = self
            .prompt_cache
            .resolve(&request)
            .map_err(|e| ProviderError::invalid_request(self.label(), e))?;
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
                reasoning_effort: self.reasoning_effort,
                dialect: self.dialect,
                cache_plan: &cache_plan,
            },
            &self.capabilities,
        );

        let label = self.label();
        let resp = self
            .send_request(&body, false, &transport_telemetry)
            .await?;
        let response_json: serde_json::Value = decode_response_json(resp, label).await?;

        if let Some(err) = response_json.get("error") {
            return Err(ProviderError::new(
                label,
                ProviderErrorKind::Protocol,
                format!("{label} Error: {}", err),
            ));
        }

        let usage = response::usage(&response_json["usage"]);

        let choice = &response_json["choices"][0]["message"];
        let message = response::message(choice, |raw, had_native| {
            let emitted = echo::ToolCallEchoFilter::filter_content(raw, had_native);
            tracing::debug!(
                target: "nuo_contracts::provider",
                provider = %self.endpoint.id,
                model = %self.endpoint.model,
                raw_chars = raw.len(),
                emitted_chars = emitted.len(),
                suppressed_chars = raw.len().saturating_sub(emitted.len()),
                native_tool_calls = had_native,
                "openai chat echo summary",
            );
            emitted
        });
        Ok(nuo_contracts::ProviderCompletion {
            message,
            meta: nuo_contracts::ProviderCompletionMeta {
                usage,
                ..Default::default()
            },
        })
    }

    async fn stream_chat(
        &self,
        request: ModelRequest,
    ) -> Result<
        BoxStream<'static, Result<String, nuo_contracts::ProviderError>>,
        nuo_contracts::ProviderError,
    > {
        let cache_plan = self
            .prompt_cache
            .resolve(&request)
            .map_err(|e| ProviderError::invalid_request(self.label(), e))?;
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
                reasoning_effort: self.reasoning_effort,
                dialect: self.dialect,
                cache_plan: &cache_plan,
            },
            &self.capabilities,
        );

        let response = self.send_request(&body, true, &transport_telemetry).await?;

        let pipeline = self.pipeline.clone();
        let label = self.label();
        let stream = crate::sse::data_payloads(response, label).map(move |item| {
            let data = item?;
            let data = pipeline.unwrap_payload(&data, label)?;
            Ok(response::stream_text(&data))
        });

        Ok(stream.boxed())
    }

    async fn stream_chat_events(
        &self,
        request: ModelRequest,
    ) -> Result<
        BoxStream<'static, Result<ProviderStreamEvent, nuo_contracts::ProviderError>>,
        nuo_contracts::ProviderError,
    > {
        let cache_plan = self
            .prompt_cache
            .resolve(&request)
            .map_err(|e| ProviderError::invalid_request(self.label(), e))?;
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
                reasoning_effort: self.reasoning_effort,
                dialect: self.dialect,
                cache_plan: &cache_plan,
            },
            &self.capabilities,
        );

        let response = self.send_request(&body, true, &transport_telemetry).await?;

        // Tool-call echo filter shared between the body and the end-of-stream
        // flush: it suppresses any content that mirrors a native tool call
        // before it becomes a `TextDelta`. SSE byte reassembly (incl.
        // multi-byte UTF-8 split across chunks) is handled by
        // `sse::data_payloads`; each payload is then parsed into the OpenAI
        // event shape and fed through the echo filter.
        let echo_filter = Arc::new(Mutex::new(echo::ToolCallEchoFilter::new()));
        let filter_for_body = Arc::clone(&echo_filter);
        let reasoning_details =
            Arc::new(Mutex::new(response::ReasoningDetailsAccumulator::default()));
        let reasoning_details_for_body = Arc::clone(&reasoning_details);
        let collect_reasoning_details =
            self.dialect == nuo_contracts::OpenAiChatDialect::OpenRouter;
        let pipeline = self.pipeline.clone();
        let label = self.label();
        let body = crate::sse::data_payloads(response, label).map(move |item| {
            let data = item?;
            let data = pipeline.unwrap_payload(&data, label)?;
            if data.is_empty() {
                return Ok::<Vec<Result<ProviderStreamEvent, ProviderError>>, ProviderError>(
                    Vec::new(),
                );
            }
            // Parse-once discipline (ADR-0184): the single deserialization
            // here doubles as the validity check — a non-JSON payload is a
            // decode error, and the parsed `Value` feeds the stream parser
            // (which then fans events out through the echo filter).
            let event: serde_json::Value = serde_json::from_str(&data).map_err(|_| {
                ProviderError::new(
                    label,
                    ProviderErrorKind::Decode,
                    "Invalid JSON in stream payload",
                )
            })?;
            if collect_reasoning_details {
                reasoning_details_for_body
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .observe(&event);
            }
            let parsed = response::stream_events(&event);
            // Recover from a poisoned mutex: a prior panic in this critical
            // section must not take down subsequent stream chunks.
            let mut filter = filter_for_body
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let mut events: Vec<Result<ProviderStreamEvent, ProviderError>> = Vec::new();
            for event in parsed {
                events.extend(filter.observe(event).into_iter().map(Ok));
            }
            Ok::<_, ProviderError>(events)
        });
        // Flush any buffered non-echo text once the byte stream ends, and log a
        // per-turn stream summary so empty responses are diagnosable.
        let provider_id = self.endpoint.id.clone();
        let model = self.endpoint.model.clone();
        let tail = futures::stream::once(async move {
            let mut filter = echo_filter
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let emitted = filter.finish();
            tracing::debug!(
                target: "nuo_contracts::provider",
                provider = %provider_id,
                model = %model,
                content_fed_chars = filter.fed_chars,
                content_emitted_chars = filter.emitted_chars,
                echo_suppressed_chars = filter.fed_chars.saturating_sub(filter.emitted_chars),
                reasoning_chars = filter.reasoning_chars,
                tool_call_deltas = filter.tool_call_deltas,
                "openai stream summary",
            );
            let mut events: Vec<Result<ProviderStreamEvent, ProviderError>> = Vec::new();
            if !emitted.is_empty() {
                events.push(Ok(ProviderStreamEvent::TextDelta(emitted)));
            }
            let artifacts = collect_reasoning_details
                .then(|| {
                    reasoning_details
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .artifacts()
                })
                .flatten();
            events.push(Ok(ProviderStreamEvent::Completed(
                nuo_contracts::ProviderCompletionMeta {
                    artifacts,
                    ..Default::default()
                },
            )));
            Ok::<_, ProviderError>(events)
        });
        Ok(body
            .chain(tail)
            .flat_map(|result| match result {
                Ok(events) => futures::stream::iter(events),
                Err(error) => futures::stream::iter(vec![Err(error)]),
            })
            .boxed())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nuo_contracts::{Message, Role, Tool};

    // resolved-variant schema reaches the request body

    /// Minimal Tool stand-in carrying a variant id, so resolving a toolset and
    /// preparing its schemas can be exercised without the whole tools crate.
    struct DummyTool {
        name: &'static str,
        variant: &'static str,
        desc: &'static str,
    }
    #[async_trait]
    impl Tool for DummyTool {
        fn name(&self) -> &str {
            self.name
        }
        fn variant(&self) -> &str {
            self.variant
        }
        fn description(&self) -> &str {
            self.desc
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }
        async fn call(&self, _: &str) -> Result<String, String> {
            Ok(String::new())
        }
    }

    fn tool_desc_at(body: &serde_json::Value, idx: usize) -> &str {
        body["tools"][idx]["function"]["description"]
            .as_str()
            .unwrap_or("")
    }

    fn body_with_tools(tools: &[Arc<dyn Tool>]) -> serde_json::Value {
        let request = ModelRequest::with_tools(vec![Message::new(Role::User, "go")], tools);
        let (messages, tool_specs) = request.into_parts();
        static DEFAULT_CACHE_PLAN: nuo_contracts::ResolvedCachePolicy =
            nuo_contracts::ResolvedCachePolicy::Unsupported;
        request::body(
            messages,
            request::BodyInput {
                model: "test-model",
                stream: false,
                instructions: None,
                tool_specs: Some(&tool_specs),
                reasoning_effort: None,
                dialect: nuo_contracts::OpenAiChatDialect::Standard,
                cache_plan: &DEFAULT_CACHE_PLAN,
            },
        )
    }

    #[test]
    fn model_request_emits_the_selected_variants_schema() {
        // A `read_text` capability with two variants; the agent resolves a
        // selection before handing the toolset to the provider, so whichever
        // variant is selected is the one whose schema reaches the request body.
        let toolset = nuo_contracts::ToolSet::from_tools(vec![
            Arc::new(DummyTool {
                name: "read_text",
                variant: "default",
                desc: "default wording",
            }) as Arc<dyn Tool>,
            Arc::new(DummyTool {
                name: "read_text",
                variant: "terse",
                desc: "terse wording",
            }) as Arc<dyn Tool>,
        ]);

        // Default selection → default variant's description in the body.
        let body = body_with_tools(&toolset.default_view());
        assert_eq!(tool_desc_at(&body, 0), "default wording");
        assert_eq!(body["tools"][0]["function"]["name"], "read_text");

        // Selecting the terse variant → terse description in the body, same name.
        let mut selection = nuo_contracts::VariantSelection::new();
        selection.insert("read_text".to_string(), "terse".to_string());
        let body = body_with_tools(&toolset.resolve(&selection));
        assert_eq!(tool_desc_at(&body, 0), "terse wording");
        assert_eq!(body["tools"][0]["function"]["name"], "read_text");
        assert_eq!(body["tools"][0]["type"], "function");
    }

    #[test]
    fn prompt_hints_emit_no_system_guidance() {
        let provider =
            OpenAiChatCompletionsProvider::new("test-key".to_string(), "test-model".to_string());
        // No protocol note: native tool calls are the wire default and the
        // ToolCallEchoFilter strips text-mirrored calls regardless.
        assert!(provider.prompt_hints().system_guidance.is_empty());
    }

    #[test]
    fn opencode_go_request_carries_session_and_client_headers() {
        let provider = OpenAiChatCompletionsProvider::with_base_url_and_user_agent(
            "test-key".to_string(),
            "glm-5.2".to_string(),
            "https://opencode.ai/inference/openai/v1/chat/completions",
            crate::OPENCODE_USER_AGENT,
        )
        .with_id("opencode-go".to_string())
        .with_session_id("ses_wire_test_123");

        let auth = nuo_contracts::ResolvedAuth::new("test-token");
        let body = serde_json::json!({"model": "glm-5.2"});
        let req = provider
            .build_request_for_auth(&body, &auth)
            .build("Test")
            .unwrap();

        let headers = &req.headers;
        assert_eq!(
            headers.get("x-opencode-session").unwrap().to_str().unwrap(),
            "ses_wire_test_123"
        );
        assert_eq!(
            headers.get("x-opencode-client").unwrap().to_str().unwrap(),
            "cli"
        );
        assert!(
            headers
                .get("x-opencode-request")
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("req_")
        );
        assert_eq!(
            headers.get("user-agent").unwrap().to_str().unwrap(),
            crate::OPENCODE_USER_AGENT
        );
        assert!(
            headers.get("x-opencode-org-id").is_none(),
            "a keyless credential must not claim a workspace"
        );
    }

    #[test]
    fn workspace_scoped_credential_carries_the_org_header() {
        let provider = OpenAiChatCompletionsProvider::with_base_url(
            "st-token".to_string(),
            "glm-5.2".to_string(),
            "https://opencode.ai/inference/openai/v1/chat/completions",
        );
        let auth = nuo_contracts::ResolvedAuth::new("st-token").with_extension(
            nuo_contracts::OpencodeAuthMetadata {
                org_id: "wrk_workspace_1".to_string(),
            },
        );
        let req = provider
            .build_request_for_auth(&serde_json::json!({"model": "glm-5.2"}), &auth)
            .build("Test")
            .unwrap();
        assert_eq!(
            req.headers
                .get("x-opencode-org-id")
                .and_then(|v| v.to_str().ok())
                .unwrap(),
            "wrk_workspace_1"
        );
    }

    // Golden-wire tests live in nuo-providers (`registry::qoder::wire_golden`)
    // — the only crate that can reference both the executor and the Qoder wire
    // implementation without inverting the dependency graph (ADR-0271).
}
