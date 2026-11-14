//! Google native provider (REST to the versioned Google base).
//!
//! A thin executor over the pure [`request`] and [`response`] layers plus the
//! shared transport helpers. Google AI Studio's transport uses standard header
//! authentication (`x-goog-api-key` for API keys, `Authorization: Bearer` for
//! OAuth tokens) with clean URLs without query parameter key leakage, and the
//! base URL is versioned (`.../v1beta`) with the per-call model path appended
//! at request time. Everything else reuses the shared SSE decoder and HTTP
//! helpers.
//!
//! Module layout (mirrors the OpenAI and Anthropic providers):
//! - [`request`] — body / url construction (pure, no I/O)
//! - [`response`] — usage, message, and stream-payload parsing (pure)
//! - this file — the [`GoogleProvider`] executor + `Provider` impl

// Unit tests assert by panicking: a failed `unwrap`/`expect`/`panic!` *is* the
// test result. `cfg_attr(test, ..)` scopes that exemption to the test build only,
// so the shipped graph stays under the workspace `deny` for these lints.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use nuo_protocol::capability::{ModelRequest, Provider, ProviderPromptHints, ProviderStreamEvent};
use nuo_protocol::{ProviderError, ProviderErrorKind};

use serde_json::{Map, Value};
use std::sync::{Arc, Mutex};

use nuo_provider::transport::{Client, HttpResponse, RequestBuilder};
use nuo_provider::transport::{decode_response_json, ensure_success};
use nuo_provider::{Endpoint, NUO_USER_AGENT, PromptCacheConfig};

pub mod catalog;
pub mod interactions;
pub mod oauth;
pub mod request;
pub mod response;
pub mod spec_antigravity;
pub mod spec_google;
pub mod usage;

pub use interactions::GoogleInteractionsProvider;
pub use spec_antigravity::MODEL_PROVIDER_SPEC as ANTIGRAVITY_MODEL_PROVIDER_SPEC;
pub use spec_google::MODEL_PROVIDER_SPEC as GOOGLE_MODEL_PROVIDER_SPEC;
pub use usage::*;

/// Official Google REST base, versioned. The provider appends the per-call
/// model path (`/models/{id}:generateContent` / `:streamGenerateContent`), so a
/// 中转站/relay overrides this with its own host carrying the `/v1beta` prefix.
pub const GOOGLE_DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";

fn google_completion(
    response_json: &Value,
    signature_origin: &str,
) -> Result<nuo_protocol::ProviderCompletion, nuo_protocol::ProviderError> {
    let root = response_json.get("response").unwrap_or(response_json);
    let mut message =
        response::message(response_json).map_err(|e| ProviderError::protocol("Google", e))?;
    // Record which wire identity minted this turn's thought signatures before
    // the sidecar leaves the message. Replay is gated on this exact match: a
    // signature only validates on the backend that issued it.
    stamp_signature_origin(
        message.provider_meta.get_or_insert_with(Map::new),
        signature_origin,
    );
    let artifacts = message.provider_meta.take();
    Ok(nuo_protocol::ProviderCompletion {
        message,
        meta: nuo_protocol::ProviderCompletionMeta {
            usage: response::usage(&root["usageMetadata"]),
            artifacts,
            continuation: None,
        },
    })
}

/// Stamp the minting wire identity onto a turn's provider sidecar.
///
/// Only turns that actually carry signature material are stamped: an unsigned
/// turn has nothing to protect, and an origin stamp with no tokens would make
/// the sidecar look like replay evidence when it is not.
fn stamp_signature_origin(meta: &mut Map<String, Value>, origin: &str) {
    let carries_signature = [
        response::THOUGHT_SIGNATURES_META_KEY,
        response::TEXT_THOUGHT_SIGNATURE_META_KEY,
        response::TERMINAL_THOUGHT_SIGNATURE_META_KEY,
    ]
    .iter()
    .any(|key| meta.get(*key).is_some_and(|value| !value.is_null()));
    if carries_signature {
        meta.insert(
            response::THOUGHT_SIGNATURE_ORIGIN_META_KEY.to_string(),
            Value::String(origin.to_string()),
        );
    }
}

/// Google native provider.
///
/// Embeds the shared [`Endpoint`] (connection config). The provider holds no wire-format-unique fields:
/// Google's transport differences (key as query param, versioned base, native
/// function declarations) are confined to [`request`] / [`response`].
pub struct GoogleProvider {
    pub endpoint: Endpoint,
    /// Pooled HTTP client reused across every request this provider makes.
    pub client: Client,
    /// Channel-scoped capability view. A trusted remote catalogue overrides the
    /// static baseline only for this provider/model route.
    pub capabilities: nuo_protocol::ModelCapabilities,
    /// Channel-scoped reasoning-effort override. `None` leaves the model's
    /// server-default thinking level in place; `Some(e)` pins it, translated
    /// onto `thinkingConfig` (`thinkingLevel` for Gemini 3.x, a
    /// `thinkingBudget` bucket for Gemini 2.5) at request-build time.
    pub reasoning_effort: Option<nuo_protocol::Effort>,
    /// Antigravity Google Cloud companion project ID (`cloudaicompanionProject`).
    /// When set, the provider routes requests in `v1internal` envelope shape with
    /// `Authorization: Bearer` authentication to the Antigravity backend.
    pub project_id: Option<String>,
    /// Route-scoped prompt-cache capabilities and defaults. The public Google
    /// route currently advertises only implicit caching, so resolution
    /// validates user intent without inventing resource-cache lifecycle.
    pub prompt_cache: PromptCacheConfig,
    /// Sticky, channel-scoped flag: this upstream has refused our
    /// `thinkingConfig` at least once (INVALID_ARGUMENT naming the field), so
    /// the thinking-disclosure surface is unavailable on this route. Reasoning
    /// itself continues — the model still thinks server-side — but the chain
    /// cannot be disclosed and requests stop asking for it. See
    /// [`Self::note_thinking_rejected`].
    thinking_rejected: Arc<Mutex<bool>>,
    pub dialect: nuo_protocol::dialects::GoogleGenerateContentDialect,
}

/// Canonical alias for the Google Gemini protocol provider.
pub type GoogleGeminiProvider = GoogleProvider;

impl GoogleProvider {
    pub fn new(api_key: String, model: String) -> Self {
        Self::with_base_url(api_key, model, GOOGLE_DEFAULT_BASE_URL)
    }

    /// Build a provider targeting a custom versioned base URL (e.g. a
    /// Google-format relay). A trailing slash on `base_url` is tolerated
    /// (stripped).
    pub fn with_base_url(api_key: String, model: String, base_url: &str) -> Self {
        Self::with_base_url_and_user_agent(api_key, model, base_url, NUO_USER_AGENT)
    }

    /// Build a provider targeting a custom versioned base URL with an explicit
    /// `User-Agent`. A trailing slash on `base_url` is tolerated (stripped).
    pub fn with_base_url_and_user_agent(
        api_key: String,
        model: String,
        base_url: &str,
        user_agent: &str,
    ) -> Self {
        let capabilities = nuo_protocol::ModelCapabilities::for_channel(&model, None);
        Self {
            endpoint: Endpoint::from_static_key(
                api_key,
                model,
                base_url.trim_end_matches('/'),
                "google",
            )
            .with_user_agent(user_agent),
            client: Client::new(),
            capabilities,
            reasoning_effort: None,
            project_id: None,
            prompt_cache: PromptCacheConfig::default(),
            thinking_rejected: Arc::new(Mutex::new(false)),
            dialect: nuo_protocol::dialects::GoogleGenerateContentDialect::GenerativeLanguage,
        }
    }

    /// Build a provider with dynamic credentials.
    pub fn with_credentials(
        credentials: std::sync::Arc<dyn nuo_protocol::CredentialSource>,
        model: String,
        base_url: &str,
    ) -> Self {
        let capabilities = nuo_protocol::ModelCapabilities::for_channel(&model, None);
        Self {
            endpoint: Endpoint::with_credentials(
                credentials,
                model,
                base_url.trim_end_matches('/'),
                "google",
            ),
            client: Client::new(),
            capabilities,
            reasoning_effort: None,
            project_id: None,
            prompt_cache: PromptCacheConfig::default(),
            thinking_rejected: Arc::new(Mutex::new(false)),
            dialect: nuo_protocol::dialects::GoogleGenerateContentDialect::GenerativeLanguage,
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

    /// Set the attribution id (provider/solution id) so assistant responses are
    /// attributed to the logical model.
    pub fn with_id(mut self, id: String) -> Self {
        self.endpoint.set_id(id);
        self
    }

    /// Attach the effective provider-channel capability view.
    pub fn with_model_capabilities(
        mut self,
        capabilities: nuo_protocol::ModelCapabilities,
    ) -> Self {
        self.capabilities = capabilities;
        self
    }

    /// Set the channel-scoped reasoning-effort override. `None` leaves the
    /// server default in place. Clamped to the resolved model's supported
    /// `effort_levels` at request-build time, then translated onto Google's
    /// `thinkingConfig` (`thinkingLevel` for Gemini 3.x, a `thinkingBudget`
    /// bucket for Gemini 2.5).
    pub fn with_reasoning_effort(mut self, effort: Option<nuo_protocol::Effort>) -> Self {
        self.reasoning_effort = effort;
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

    pub fn with_dialect(
        mut self,
        dialect: nuo_protocol::dialects::GoogleGenerateContentDialect,
    ) -> Self {
        self.dialect = dialect;
        self
    }

    /// Attach the Antigravity project ID (`cloudaicompanionProject`).
    pub fn with_project_id(mut self, project_id: impl Into<String>) -> Self {
        self.project_id = Some(project_id.into());
        self
    }

    /// Whether this provider is configured for Google Antigravity `v1internal` protocol.
    pub fn is_antigravity(&self) -> bool {
        self.dialect == nuo_protocol::dialects::GoogleGenerateContentDialect::Antigravity
    }

    /// The wire identity this channel currently addresses — the envelope's
    /// `model` on Antigravity, the model id itself on a native Google route.
    ///
    /// This is the identity that mints and validates thought signatures, so it
    /// is what replay is gated on (see
    /// [`request::BodyInput::signature_origin`]).
    pub fn effective_wire_model(&self) -> String {
        if self.is_antigravity() {
            effective_antigravity_wire_model(
                &self.capabilities,
                &self.endpoint.model,
                self.reasoning_effort,
            )
        } else {
            self.endpoint.model.clone()
        }
    }

    /// The full signature-minting identity of this route: the ingress authority
    /// **and** the wire model.
    ///
    /// A Google thought signature only validates on the backend that issued it,
    /// and the model id alone cannot express that — the same id is served by
    /// Google AI Studio, by the Cloud Code `v1internal` surface, and by any
    /// relay/中转站 configured through `with_base_url`. Comparing only the model
    /// would let a token minted by one backend be replayed to another, which is
    /// precisely the cross-backend case answered with
    /// `400 "Corrupted thought signature."`.
    pub fn signature_origin(&self) -> String {
        format!(
            "{}/{}",
            self.endpoint.ingress_host(),
            self.effective_wire_model()
        )
    }

    /// Build the thinkingless streaming request for a channel whose upstream
    /// refused `thinkingConfig`. Shared shape with
    /// [`Self::stream_chat_events`] minus the thinking-disclosure surface; kept
    /// as a separate method so the downgrade path is explicit and testable.
    async fn stream_chat_events_without_thinking(
        &self,
        request: &ModelRequest,
        telemetry: nuo_protocol::TransportTelemetry,
    ) -> Result<
        BoxStream<'static, Result<ProviderStreamEvent, nuo_protocol::ProviderError>>,
        nuo_protocol::ProviderError,
    > {
        let (response, shape) = self
            .send_google_request(request, &telemetry, true, true, None)
            .await?;
        let response = ensure_success(response, "Google", Some(&self.endpoint.model))
            .await
            .map_err(|e| {
                response::clarify_error_with_shape(
                    e,
                    &self.endpoint.model,
                    &self.endpoint.base_url,
                    Some(&shape),
                )
            })?;
        Ok(self.wrap_event_stream(response))
    }

    /// Send one Google request, returning the response **and** the shape of the
    /// body that was actually put on the wire.
    ///
    /// The shape travels with the response so every refusal path can fold the
    /// sent evidence into the error (see [`response::RequestShape`]): Google's
    /// `400` body names no field, so without this the notice can only say
    /// "invalid argument".
    async fn send_google_request(
        &self,
        request: &ModelRequest,
        telemetry: &nuo_protocol::TransportTelemetry,
        is_stream: bool,
        omit_thinking: bool,
        timeout: Option<std::time::Duration>,
    ) -> Result<(HttpResponse, response::RequestShape), ProviderError> {
        let _cache_plan = self
            .prompt_cache
            .resolve(request)
            .map_err(|e| ProviderError::invalid_request("Google", e))?;
        let prewarm = self.client.prewarm(self.endpoint.base_url(), "Google");
        let auth_fut = self.endpoint.resolve_auth();
        let (prewarm_outcome, auth_result) = tokio::join!(prewarm, auth_fut);
        if let Err(error) = prewarm_outcome {
            tracing::debug!(error = %error, "prewarm skipped");
        }
        let auth = auth_result.map_err(|e| ProviderError::authentication("Google", e))?;
        let (url, headers, wire) =
            self.prepare_request_for_auth(request, is_stream, omit_thinking, &auth);
        let shape = self.request_shape(&wire.body);
        let encoded = wire.encode()?;

        let mut req_builder = RequestBuilder::new(http::Method::POST, &url)
            .headers(headers.clone())
            .encoded(&encoded)
            .tls_profile(crate::spec_antigravity::ANTIGRAVITY_TLS_PROFILE)
            .with_telemetry(telemetry.clone());
        if let Some(t) = timeout {
            req_builder = req_builder.timeout(t);
        }
        let response = self.client.send_raw(req_builder, "Google").await?;

        if response.status == http::StatusCode::UNAUTHORIZED && self.endpoint.is_negotiated() {
            tracing::warn!(
                model = %self.endpoint.model,
                "OAuth token rejected by Google (401 Unauthorized); attempting force-refresh and retry"
            );
            let refreshed_auth = self
                .endpoint
                .force_refresh_auth_after(&auth.token)
                .await
                .map_err(|error| ProviderError::authentication("Google", error))?;
            let (retry_url, retry_headers, retry_wire) =
                self.prepare_request_for_auth(request, is_stream, omit_thinking, &refreshed_auth);
            let retry_shape = self.request_shape(&retry_wire.body);
            let retry_encoded = retry_wire.encode()?;
            let mut retry_builder = RequestBuilder::new(http::Method::POST, &retry_url)
                .headers(retry_headers)
                .encoded(&retry_encoded)
                .tls_profile(crate::spec_antigravity::ANTIGRAVITY_TLS_PROFILE)
                .with_telemetry(telemetry.clone());
            if let Some(t) = timeout {
                retry_builder = retry_builder.timeout(t);
            }
            return Ok((
                self.client.send_raw(retry_builder, "Google").await?,
                retry_shape,
            ));
        }

        Ok((response, shape))
    }

    /// Read the shape of a prepared body back into a [`response::RequestShape`],
    /// resolving the wire model id exactly as the envelope did.
    fn request_shape(&self, body: &serde_json::Value) -> response::RequestShape {
        response::RequestShape::of(&self.effective_wire_model(), body)
    }

    fn wrap_event_stream(
        &self,
        response: HttpResponse,
    ) -> BoxStream<'static, Result<ProviderStreamEvent, ProviderError>> {
        let next_tool_index = std::sync::Arc::new(std::sync::Mutex::new(0usize));
        let thought_signatures = Arc::new(Mutex::new(Map::new()));
        // Signature *sites* are accumulated separately and the replay decision
        // is deferred to the terminal frame, because the answer text only
        // exists as a whole once every delta has arrived: a text-part signature
        // is replayable only when the merged answer is byte-identical to the one
        // signed delta. Guessing before the stream ends is what puts a token
        // next to bytes it never signed (the upstream answers
        // `400 Corrupted thought signature`).
        let text_signature_sites = Arc::new(Mutex::new(response::TextSignatureSites::default()));
        let terminal_signatures = thought_signatures.clone();
        let terminal_text_signature = text_signature_sites.clone();

        let signature_origin = self.signature_origin();
        let stream = nuo_provider::transport::sse::data_payloads(response, "Google").flat_map({
            let next_tool_index = next_tool_index.clone();
            move |item| {
                let events: Vec<Result<ProviderStreamEvent, ProviderError>> = match item {
                    Ok(payload) => {
                        // Single parse: parse once, hand the value
                        // to the payload assembler. Non-JSON data payloads
                        // (heartbeats aside — those are filtered upstream)
                        // surface the same decode error as before.
                        match serde_json::from_str::<serde_json::Value>(&payload) {
                            Err(_) => vec![Err(ProviderError::new(
                                "Google",
                                ProviderErrorKind::Decode,
                                "Invalid JSON in stream payload",
                            ))],
                            Ok(parsed_json) => {
                                let parsed = response::stream_payload_value(&parsed_json);
                                if !parsed.thought_signatures.is_empty() {
                                    let mut guard = thought_signatures
                                        .lock()
                                        .unwrap_or_else(|e| e.into_inner());
                                    for (id, signature) in &parsed.thought_signatures {
                                        guard.insert(id.clone(), Value::String(signature.clone()));
                                    }
                                    for event in &parsed.events {
                                        if let ProviderStreamEvent::ToolCallDelta {
                                            id: Some(id),
                                            name: Some(name),
                                            ..
                                        } = event
                                            && let Some(sig) = guard.get(id).cloned()
                                        {
                                            guard.insert(name.clone(), sig);
                                        }
                                    }
                                }
                                {
                                    let mut sites = text_signature_sites
                                        .lock()
                                        .unwrap_or_else(|e| e.into_inner());
                                    sites.observe(&parsed);
                                }
                                parsed
                                    .events
                                    .into_iter()
                                    .map(|event| match event {
                                        ProviderStreamEvent::ToolCallDelta {
                                            id,
                                            name,
                                            arguments,
                                            ..
                                        } => {
                                            let mut guard = next_tool_index
                                                .lock()
                                                .unwrap_or_else(|e| e.into_inner());
                                            let index = *guard;
                                            *guard += 1;
                                            Ok(ProviderStreamEvent::ToolCallDelta {
                                                index,
                                                id,
                                                name,
                                                arguments,
                                            })
                                        }
                                        event => Ok(event),
                                    })
                                    .collect()
                            }
                        }
                    }
                    Err(error) => vec![Err(ProviderError::from(error))],
                };
                futures::stream::iter(events)
            }
        });
        let terminal = futures::stream::once(async move {
            let mut signatures = std::mem::take(
                &mut *terminal_signatures
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()),
            );
            let sites = terminal_text_signature
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .resolve();
            let mut artifacts = Map::new();
            // A turn-level signature is the documented signature for the turn's
            // function-call group; seed any call that streamed unsigned so the
            // next turn keeps native tool replay instead of degrading to text.
            if let Some(turn_signature) = sites.turn_signature.as_deref() {
                let seed = Value::String(turn_signature.to_string());
                for key in &sites.call_keys {
                    signatures
                        .entry(key.clone())
                        .or_insert_with(|| seed.clone());
                }
            }
            if !signatures.is_empty() {
                artifacts.insert(
                    response::THOUGHT_SIGNATURES_META_KEY.to_string(),
                    Value::Object(signatures),
                );
            }
            if let Some(signature) = sites.text_signature {
                artifacts.insert(
                    response::TEXT_THOUGHT_SIGNATURE_META_KEY.to_string(),
                    Value::String(signature),
                );
            }
            if let Some(signature) = sites.turn_signature {
                artifacts.insert(
                    response::TERMINAL_THOUGHT_SIGNATURE_META_KEY.to_string(),
                    Value::String(signature),
                );
            }
            if !artifacts.is_empty() {
                // Only a turn that actually minted signatures records its
                // origin: replay is gated on an exact wire-identity match.
                artifacts.insert(
                    response::THOUGHT_SIGNATURE_ORIGIN_META_KEY.to_string(),
                    Value::String(signature_origin),
                );
            }
            Ok(ProviderStreamEvent::Completed(
                nuo_protocol::ProviderCompletionMeta {
                    artifacts: (!artifacts.is_empty()).then_some(artifacts),
                    ..Default::default()
                },
            ))
        });
        stream.chain(terminal).boxed()
    }

    /// Record that this channel's upstream rejected our `thinkingConfig` and
    /// report whether this is new information (first observation). The flag is
    /// channel-scoped and sticky for the process lifetime: the rejection is a
    /// property of the upstream route, not of an individual request, so later
    /// turns skip straight to the thinkingless form instead of paying one
    /// failed request each. A new observation is exactly-once so the caller
    /// logs/notices it once per process.
    fn note_thinking_rejected(&self) -> bool {
        let mut guard = self
            .thinking_rejected
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if *guard {
            false
        } else {
            *guard = true;
            true
        }
    }

    /// Whether this channel's upstream has already refused our
    /// `thinkingConfig` (sticky, per process).
    fn thinking_was_rejected(&self) -> bool {
        *self
            .thinking_rejected
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    /// Build the thinkingless non-streaming request for a channel whose
    /// upstream refused `thinkingConfig`. The model still thinks server-side
    /// (Gemini 3.x cannot reason with thinking off); what changes is that we
    /// stop asking for the chain to be disclosed, which is the upstream's
    /// prerogative — the turn itself is not an error.
    async fn chat_without_thinking(
        &self,
        request: &ModelRequest,
    ) -> Result<nuo_protocol::ProviderCompletion, nuo_protocol::ProviderError> {
        tracing::warn!(
            model = %self.endpoint.model,
            "upstream rejected thinkingConfig; retrying without disclosed thinking (chain withheld by upstream)"
        );
        let (response, shape) = self
            .send_google_request(
                request,
                &nuo_protocol::TransportTelemetry::default(),
                false,
                true,
                Some(self.client.request_timeout()),
            )
            .await?;
        let response = ensure_success(response, "Google", Some(&self.endpoint.model))
            .await
            .map_err(|e| {
                response::clarify_error_with_shape(
                    e,
                    &self.endpoint.model,
                    &self.endpoint.base_url,
                    Some(&shape),
                )
            })?;

        let response_json: serde_json::Value = decode_response_json(response, "Google").await?;
        let root = response_json.get("response").unwrap_or(&response_json);

        if let Some(err) = response_json.get("error").or_else(|| root.get("error")) {
            let provider_err = ProviderError::new(
                "Google",
                ProviderErrorKind::Protocol,
                format!("Google Error: {}", err),
            );
            return Err(response::clarify_error_with_shape(
                provider_err,
                &self.endpoint.model,
                &self.endpoint.base_url,
                Some(&shape),
            ));
        }

        google_completion(&response_json, &self.signature_origin())
    }

    #[cfg(test)]
    fn prepare_request(
        &self,
        request: &ModelRequest,
        is_stream: bool,
        omit_thinking: bool,
    ) -> (String, http::header::HeaderMap, nuo_provider::WireRequest) {
        let auth = nuo_protocol::ResolvedAuth::default();
        self.prepare_request_for_auth(request, is_stream, omit_thinking, &auth)
    }

    fn prepare_request_for_auth(
        &self,
        request: &ModelRequest,
        is_stream: bool,
        omit_thinking: bool,
        auth: &nuo_protocol::ResolvedAuth,
    ) -> (String, http::header::HeaderMap, nuo_provider::WireRequest) {
        let key = auth.token.expose_secret();
        let is_claude = self.endpoint.model.starts_with("claude-");
        let include_thoughts = self.capabilities.reasoning() && !omit_thinking && !is_claude;
        let thinking = if omit_thinking || is_claude {
            None
        } else {
            request::resolve_thinking(
                self.reasoning_effort,
                &self.capabilities.effort_levels,
                request::max_thinking_budget(&self.endpoint.model),
            )
        };
        // The exact wire identity this turn addresses. It gates signature
        // replay: a Google thought signature is only valid on the backend that
        // minted it, so a turn from a different wire identity is replayed with
        // its tokens dropped rather than forwarded on faith.
        // The replay gate compares the *full* minting identity: a token only
        // validates on the backend that issued it, and the model id alone cannot
        // distinguish AI Studio from Cloud Code or from a relay.
        let signature_origin = self.signature_origin();
        let mut messages = request.messages.clone();
        messages.extend(request.temporary_context.iter().cloned());
        // Shared route-scoped image projection: Google's
        // `inline_data` parts were previously emitted unconditionally, so a
        // route that declares no image input failed the turn instead of having
        // the pixels projected away.
        let raw_body = request::body_with_capabilities(
            &self.endpoint.model,
            messages,
            request::BodyInput {
                instructions: Some(&request.instructions),
                tool_specs: request.tool_specs(),
                include_thoughts,
                thinking,
                signature_origin: &signature_origin,
            },
            &self.capabilities,
        );

        let mut headers = http::header::HeaderMap::new();
        if let Ok(ua) = self.endpoint.user_agent().parse() {
            headers.insert("User-Agent", ua);
        }
        headers.insert(
            http::header::CONTENT_TYPE,
            http::header::HeaderValue::from_static("application/json"),
        );

        let client_headers = self
            .endpoint
            .headers()
            .into_iter()
            .map(|(k, v)| (k, v.to_string()));
        let affinity_headers = self
            .endpoint
            .session_affinity_headers(self.prompt_cache.routing_key());
        let scoped_headers = self.endpoint.auth_scoped_headers(auth);
        for (k, v) in client_headers.chain(affinity_headers).chain(scoped_headers) {
            if let (Ok(hname), Ok(hval)) = (
                http::header::HeaderName::from_bytes(k.as_bytes()),
                http::header::HeaderValue::from_str(&v),
            ) {
                headers.insert(hname, hval);
            }
        }

        if self.is_antigravity() {
            let action = if is_stream {
                "streamGenerateContent"
            } else {
                "generateContent"
            };
            let base = self.endpoint.base_url.trim_end_matches('/');
            let url = if is_stream {
                format!("{base}/v1internal:{action}?alt=sse")
            } else {
                format!("{base}/v1internal:{action}")
            };

            if !key.is_empty()
                && let Ok(bearer) = format!("Bearer {key}").parse()
            {
                headers.insert("Authorization", bearer);
            }

            // Wire truth (live capture 2026-10-14): the shipped `agy` sends no
            // `x-goog-api-client` (nor any other client-identity header) on
            // Cloud Code calls — the four-header set above is the whole story.

            let project = auth
                .extension::<nuo_protocol::auth::GoogleAuthMetadata>()
                .map(|m| m.project_id.as_str())
                .or(self.project_id.as_deref())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or("aicode-consumers");

            let effective_model = self.effective_wire_model();
            let wrapped_body = serde_json::json!({
                "project": project,
                "requestId": uuid::Uuid::new_v4().to_string(),
                "userAgent": self.endpoint.user_agent(),
                "model": effective_model,
                "request": raw_body
            });

            (
                url,
                headers,
                nuo_provider::WireRequest::new(
                    nuo_protocol::WireProtocol::GoogleGenerateContent,
                    wrapped_body,
                ),
            )
        } else {
            // Google AI Studio official best-practice authentication:
            // Standard API keys ride in the `x-goog-api-key` header; OAuth/IAM
            // bearer tokens ride in `Authorization: Bearer`.
            // URLs stay clean without `?key=` query parameters to prevent secret
            // leakage in proxy logs, browser history, and request telemetry.
            if !key.is_empty() {
                if key.starts_with("ya29.") || key.starts_with("Bearer ") {
                    let bearer_val = if key.starts_with("Bearer ") {
                        key.to_string()
                    } else {
                        format!("Bearer {key}")
                    };
                    if let Ok(val) = http::header::HeaderValue::from_str(&bearer_val) {
                        headers.insert(http::header::AUTHORIZATION, val);
                    }
                } else if let Ok(val) = http::header::HeaderValue::from_str(key) {
                    headers.insert(http::header::HeaderName::from_static("x-goog-api-key"), val);
                }
            }
            let url = if is_stream {
                request::stream_url(&self.endpoint.base_url, &self.endpoint.model)
            } else {
                request::url(&self.endpoint.base_url, &self.endpoint.model)
            };
            (
                url,
                headers,
                nuo_provider::WireRequest::new(
                    nuo_protocol::WireProtocol::GoogleGenerateContent,
                    raw_body,
                ),
            )
        }
    }

    // Accessors (base_url / model_id / user_agent / api_key / id) are forwarded
    // from the embedded [`Endpoint`]; see `self.endpoint.*`.
}

#[async_trait]
impl Provider for GoogleProvider {
    fn provider_id(&self) -> String {
        self.endpoint.id.clone()
    }

    fn model(&self) -> String {
        self.endpoint.model.clone()
    }

    fn wire_protocol(&self) -> Option<nuo_inference_wire::WireProtocol> {
        Some(nuo_inference_wire::WireProtocol::GoogleGenerateContent)
    }

    // `effort()` keeps its default (`None`): the Gemini `thinkingLevel` /
    // `thinkingBudget` mapping has no user-facing depth vocabulary that
    // matches the shared `Effort` tiers one-to-one, so the transcript stays
    // quiet rather than showing a translated label that could mislead.

    fn model_capabilities(&self) -> nuo_protocol::ModelCapabilities {
        self.capabilities.clone()
    }
    fn prompt_hints(&self) -> ProviderPromptHints {
        // No protocol hint: Google's wire surface uses native function calls by
        // construction, and tool-result replay as `functionResponse` parts is
        // the provider's own convention that the model already follows. An
        // in-prompt note would only restate facts the harness already enforces.
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
    ) -> Result<nuo_protocol::ProviderCompletion, nuo_protocol::ProviderError> {
        let omit = self.thinking_was_rejected();
        let (response, shape) = self
            .send_google_request(
                request,
                &nuo_protocol::TransportTelemetry::default(),
                false,
                omit,
                Some(self.client.request_timeout()),
            )
            .await?;
        let response = match ensure_success(response, "Google", Some(&self.endpoint.model)).await {
            Ok(response) => response,
            Err(e) => {
                // Elastic downgrade (see `stream_chat_events`): when the
                // upstream rejects our `thinkingConfig`, retry the identical
                // turn with thinking omitted rather than failing it.
                //
                // Classify the *upstream* error, then augment. The augmented
                // message names the thinking surface on purpose, so testing it
                // would match every 400 and latch the channel into
                // chain-withholding for an unrelated fault.
                let rejected = response::rejects_thinking_config_error(&e);
                let e = response::clarify_error_with_shape(
                    e,
                    &self.endpoint.model,
                    &self.endpoint.base_url,
                    Some(&shape),
                );
                if rejected && self.note_thinking_rejected() {
                    return Box::pin(self.chat_without_thinking(request)).await;
                }
                return Err(e);
            }
        };

        let response_json: serde_json::Value = decode_response_json(response, "Google").await?;
        let root = response_json.get("response").unwrap_or(&response_json);

        if let Some(err) = response_json.get("error").or_else(|| root.get("error")) {
            let provider_err = ProviderError::new(
                "Google",
                ProviderErrorKind::Protocol,
                format!("Google Error: {}", err),
            );
            return Err(response::clarify_error_with_shape(
                provider_err,
                &self.endpoint.model,
                &self.endpoint.base_url,
                Some(&shape),
            ));
        }

        google_completion(&response_json, &self.signature_origin())
    }

    async fn stream_chat(
        &self,
        request: &ModelRequest,
        telemetry: nuo_protocol::TransportTelemetry,
    ) -> Result<
        BoxStream<'static, Result<String, nuo_protocol::ProviderError>>,
        nuo_protocol::ProviderError,
    > {
        let omit = self.thinking_was_rejected();
        let (response, shape) = self
            .send_google_request(request, &telemetry, true, omit, None)
            .await?;
        let response = match ensure_success(response, "Google", Some(&self.endpoint.model)).await {
            Ok(response) => response,
            Err(e) => {
                // Elastic downgrade (see `stream_chat_events`): an upstream
                // refusing `thinkingConfig` withholds the chain; the text
                // stream itself is still served. The event-stream path is the
                // one the agent actually drives (and the one that would
                // downgrade-and-retry), so here it suffices to surface the
                // classified error — the sticky flag below is still set so the
                // *next* text stream skips the doomed stamp instead of paying
                // a failed request.
                // Classify the upstream error before augmenting it — see
                // `rejects_thinking_config_error` for why the order matters.
                let rejected = response::rejects_thinking_config_error(&e);
                let e = response::clarify_error_with_shape(
                    e,
                    &self.endpoint.model,
                    &self.endpoint.base_url,
                    Some(&shape),
                );
                if rejected {
                    self.note_thinking_rejected();
                }
                return Err(e);
            }
        };

        // SSE byte reassembly (incl. multi-byte UTF-8 split across chunks) is
        // handled by `sse::data_payloads`; here we only map each payload to the
        // Google `streamGenerateContent` text shape.
        let stream = nuo_provider::transport::sse::data_payloads(response, "Google").map(|item| {
            item.map_err(ProviderError::from)
                .map(|payload| response::stream_text(&payload))
        });

        Ok(stream.boxed())
    }

    async fn stream_chat_events(
        &self,
        request: &ModelRequest,
        telemetry: nuo_protocol::TransportTelemetry,
    ) -> Result<
        BoxStream<'static, Result<ProviderStreamEvent, nuo_protocol::ProviderError>>,
        nuo_protocol::ProviderError,
    > {
        let omit = self.thinking_was_rejected();
        let (response, shape) = self
            .send_google_request(request, &telemetry, true, omit, None)
            .await?;
        let response = match ensure_success(response, "Google", Some(&self.endpoint.model)).await {
            Ok(response) => response,
            Err(e) => {
                // Elastic downgrade: an upstream that rejects
                // `thinkingConfig` is withholding the chain, not failing the
                // turn. Retry the identical request with the
                // thinking-disclosure surface omitted so the answer still
                // streams; the sticky flag (`note_thinking_rejected`) makes
                // this at most one extra request per channel per process.
                //
                // Classify the upstream error before augmenting it — the
                // augmented text names the thinking surface on purpose, so
                // testing it would match every 400.
                let rejected = response::rejects_thinking_config_error(&e);
                let e = response::clarify_error_with_shape(
                    e,
                    &self.endpoint.model,
                    &self.endpoint.base_url,
                    Some(&shape),
                );
                if rejected && self.note_thinking_rejected() {
                    tracing::warn!(
                        model = %self.endpoint.model,
                        "upstream rejected thinkingConfig; streaming without disclosed thinking (chain withheld by upstream)"
                    );
                    return Box::pin(
                        self.stream_chat_events_without_thinking(request, telemetry.clone()),
                    )
                    .await;
                }
                return Err(e);
            }
        };

        Ok(self.wrap_event_stream(response))
    }
}

/// The Antigravity **compiled baseline** wire-entity surface (ADR-0074).
///
/// This is the *offline floor* — entity facts for model ids a live
/// `fetchAvailableModels` snapshot may never have covered (offline session,
/// seed-only connection). The live catalog is authoritative when it ran: its
/// observed `wire_entities` ride `ModelCapabilities.effort_entities` and are
/// consumed by [`nuo_protocol::ModelCapabilities::wire_entity_resolution`]
/// directly, with zero family-prefix matching. This table exists only so the
/// offline floor behaves identically to the live surface; its rows cite their
/// provenance and it is *data*, not inference.
///
/// Wire facts (live-verified; see `docs/reference/openapi/antigravity.yml`):
/// - `gemini-3.8-flash*` family: registers the concrete per-level entities
///   `-tiered`/`-high`/`-medium`/`-low`; depth rides in-band (`thinkingLevel`).
/// - `gemini-4-argon` family: registers only `-{low,medium,high}`; the bare
///   id 404s `Requested entity was not found` (live 2026-10-23).
/// - `claude-{opus,sonnet}-5-5` family: registers only
///   `-{low,medium,high,max}`; the bare id 404s (live 2026-10-17, real `agy`
///   HttpClientLogError; corroborated upstream by CLIProxyAPI #6366 and
///   OmniRoute #15691).
/// - `claude-opus-4-6` (`-thinking` remap), `gpt-oss-120b` (`-medium` remap),
///   `claude-sonnet-4-6`, `gemini-3.5-flash-lite`: verbatim ids / single
///   named entities, handled structurally in the fn body below.
///
/// One family row of the offline floor.
///
/// `broadcast = true` marks an **in-band-depth** family: the first entity IS
/// the default and the wire id never changes with effort — depth rides
/// `thinkingConfig` (gemini-3.8-flash). `broadcast = false` marks a
/// **suffix-depth** family: effort ranks across the per-level entities
/// (argon, claude-5-5).
struct BaselineWireFamily {
    base: &'static str,
    default_entity: &'static str,
    entities: &'static [&'static str],
    broadcast: bool,
}

static BASELINE_WIRE_FAMILIES: &[BaselineWireFamily] = &[
    BaselineWireFamily {
        base: "gemini-3.8-flash",
        default_entity: "gemini-3.8-flash-tiered",
        entities: &[
            "gemini-3.8-flash-tiered",
            "gemini-3.8-flash-high",
            "gemini-3.8-flash-medium",
            "gemini-3.8-flash-low",
        ],
        broadcast: true,
    },
    BaselineWireFamily {
        base: "gemini-4-argon",
        default_entity: "gemini-4-argon-medium",
        entities: &[
            "gemini-4-argon-low",
            "gemini-4-argon-medium",
            "gemini-4-argon-high",
        ],
        broadcast: false,
    },
    BaselineWireFamily {
        base: "claude-opus-5-5",
        default_entity: "claude-opus-5-5-high",
        entities: &[
            "claude-opus-5-5-low",
            "claude-opus-5-5-medium",
            "claude-opus-5-5-high",
            "claude-opus-5-5-max",
        ],
        broadcast: false,
    },
    BaselineWireFamily {
        base: "claude-sonnet-5-5",
        default_entity: "claude-sonnet-5-5-high",
        entities: &[
            "claude-sonnet-5-5-low",
            "claude-sonnet-5-5-medium",
            "claude-sonnet-5-5-high",
            "claude-sonnet-5-5-max",
        ],
        broadcast: false,
    },
    BaselineWireFamily {
        base: "claude-opus-4-6",
        default_entity: "claude-opus-4-6-thinking",
        entities: &["claude-opus-4-6-thinking"],
        broadcast: true,
    },
];

/// Resolve the wire model id for the Antigravity envelope.
///
/// Priority (ADR-0074, evidence over inference):
/// 1. **Live observation** — `capabilities.effort_entities`, the account's
///    real `fetchAvailableModels` surface, via the typed
///    [`nuo_protocol::WireEntityResolution`] decision.
/// 2. **Compiled baseline** — `BASELINE_WIRE_ENTITIES`, matching the exact
///    family base id only (never a substring scan).
/// 3. **Structural remaps / verbatim** — single named entities and pass-through.
fn effective_antigravity_wire_model(
    capabilities: &nuo_protocol::ModelCapabilities,
    model: &str,
    effort: Option<nuo_protocol::Effort>,
) -> String {
    use nuo_protocol::WireEntityResolution;

    // 1. Live observation first: the account's real wired surface. The
    //    account-default row is FIRST in the observed order the parser
    //    recorded (envelope bare-row when upstream registered one bare, or
    //    `agentModelSorts` head otherwise) — so `default_idx = 0` mirrors
    //    `defaultAgentModelId` positional precedent.
    if let WireEntityResolution::PerLevel { entities, .. } =
        capabilities.wire_entity_resolution(model)
    {
        let borrowed: Vec<&str> = entities.iter().map(String::as_str).collect();
        return WireEntityResolution::resolve_observed(&borrowed, 0, effort);
    }

    // 2. Compiled baseline (offline floor), exact family base id match.
    if let Some(family) = BASELINE_WIRE_FAMILIES.iter().find(|f| f.base == model) {
        return match (family.broadcast, effort) {
            // In-band depth: the wire id never changes with effort.
            (true, Some(_) | None) => return family.default_entity.to_string(),
            // Suffix depth: rank via the shared codec mechanism.
            (false, Some(_) | None) => WireEntityResolution::resolve_observed(
                family.entities,
                family
                    .entities
                    .iter()
                    .position(|e| *e == family.default_entity)
                    .unwrap_or(0),
                effort,
            ),
        };
    }
    // 3. Structural single-entity remaps and verbatim ids.
    match model {
        "claude-opus-4-6" => "claude-opus-4-6-thinking".to_string(),
        "gpt-oss-120b" => "gpt-oss-120b-medium".to_string(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests;
