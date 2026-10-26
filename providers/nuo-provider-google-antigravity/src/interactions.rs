//! Google Gemini Interactions API provider (`/v1beta/interactions`).
//!
//! Handles stateful incremental conversation turns with upstream server-side
//! retention (`store: true`) and `previous_interaction_id` continuation chains.

use async_trait::async_trait;
use futures::StreamExt;
use nuo_inference_wire::WireProtocol;
use nuo_provider::capability::{
    ModelRequest, Provider, ProviderEventStream, ProviderPromptHints, ProviderStreamEvent,
    ProviderTextStream,
};
use nuo_provider::transport::{Client, HttpResponse, RequestBuilder};
use nuo_provider::transport::{decode_response_json, ensure_success};
use nuo_provider::{ClientProfile, Endpoint, PromptCacheConfig};
use nuo_provider::{
    CredentialSource, Effort, ModelCapabilities, ProviderCompletion, ProviderCompletionMeta,
    ProviderError, RequestDelivery,
};
use nuo_tool::{Message, Role, TokenUsage};
use serde_json::json;
use std::sync::Arc;

pub struct GoogleInteractionsProvider {
    pub endpoint: Endpoint,
    pub reasoning_effort: Option<Effort>,
    pub capabilities: ModelCapabilities,
    pub store: bool,
    pub prompt_cache: PromptCacheConfig,
    pub client: Client,
}

impl GoogleInteractionsProvider {
    pub fn new(credentials: Arc<dyn CredentialSource>, model: String, base_url: &str) -> Self {
        let capabilities = ModelCapabilities::for_channel(&model, None);
        Self {
            endpoint: Endpoint::with_credentials(credentials, model, base_url, "google"),
            reasoning_effort: None,
            capabilities,
            store: true,
            prompt_cache: PromptCacheConfig::default(),
            client: Client::new(),
        }
    }

    pub fn with_credentials(
        credentials: Arc<dyn CredentialSource>,
        model: String,
        base_url: &str,
    ) -> Self {
        Self::new(credentials, model, base_url)
    }

    pub fn with_reasoning_effort(mut self, effort: Option<Effort>) -> Self {
        self.reasoning_effort = effort;
        self
    }

    pub fn with_model_capabilities(mut self, capabilities: ModelCapabilities) -> Self {
        self.capabilities = capabilities;
        self
    }

    pub fn with_prompt_cache(mut self, config: PromptCacheConfig) -> Self {
        self.prompt_cache = config;
        self
    }

    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.endpoint = self.endpoint.with_id(id.into());
        self
    }

    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.endpoint = self.endpoint.with_session_id(session_id.into());
        self
    }

    pub fn with_client_profile(mut self, profile: impl Into<ClientProfile>) -> Self {
        self.endpoint.client_profile = profile.into();
        self
    }

    pub fn with_store(mut self, store: bool) -> Self {
        self.store = store;
        self
    }

    async fn send_interactions_request(
        &self,
        request: &ModelRequest,
        is_stream: bool,
    ) -> Result<HttpResponse, ProviderError> {
        let auth = self
            .endpoint
            .resolve_auth()
            .await
            .map_err(|e| ProviderError::authentication("Google", e))?;

        let (messages_to_send, previous_interaction_id) = match &request.delivery {
            RequestDelivery::RemoteContinuation {
                continuation_id,
                input_start,
            } => {
                let start = (*input_start).min(request.messages.len());
                (&request.messages[start..], Some(continuation_id.as_str()))
            }
            RequestDelivery::FullReplay | RequestDelivery::OpaqueReplay => {
                (request.messages.as_slice(), None)
            }
        };

        let base = self.endpoint.base_url.trim_end_matches('/');
        let url = if is_stream {
            format!("{base}/interactions?alt=sse")
        } else {
            format!("{base}/interactions")
        };

        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::CONTENT_TYPE,
            http::header::HeaderValue::from_static("application/json"),
        );

        let key = auth.token.expose_secret();
        if !key.is_empty() {
            if key.starts_with("ya29.") || key.starts_with("Bearer ") {
                let bearer = if key.starts_with("Bearer ") {
                    key.to_string()
                } else {
                    format!("Bearer {key}")
                };
                if let Ok(val) = bearer.parse() {
                    headers.insert(http::header::AUTHORIZATION, val);
                }
            } else if let Ok(val) = http::HeaderValue::from_str(key) {
                headers.insert(http::header::HeaderName::from_static("x-goog-api-key"), val);
            }
        }

        for (k, v) in self.endpoint.client_profile.headers() {
            if let (Ok(name), Ok(val)) = (k.parse::<http::header::HeaderName>(), v.parse()) {
                headers.insert(name, val);
            }
        }

        let mut parts = Vec::new();
        for msg in messages_to_send {
            match msg.role {
                Role::User | Role::Assistant => {
                    if !msg.content.is_empty() {
                        parts.push(json!({ "text": msg.content }));
                    }
                }
                Role::Tool => {
                    let call_id = msg.tool_call_id.as_deref().unwrap_or("");
                    parts.push(json!({
                        "function_response": {
                            "name": call_id,
                            "response": { "output": msg.content },
                        }
                    }));
                }
                Role::System => {}
            }
        }

        let input_val = if parts.len() == 1 && parts[0].get("text").is_some() {
            parts[0]["text"].clone()
        } else if !parts.is_empty() {
            json!(parts)
        } else {
            json!("")
        };

        let mut body = json!({
            "model": self.endpoint.model,
            "input": input_val,
            "stream": is_stream,
            "store": self.store,
        });

        if let Some(prev_id) = previous_interaction_id {
            body["previous_interaction_id"] = json!(prev_id);
        }

        if !request.instructions.is_empty() {
            let rendered = request.instructions.render_system_instructions();
            if !rendered.is_empty() {
                body["system_instruction"] = json!({
                    "parts": [{ "text": rendered }]
                });
            }
        }

        if !request.tool_specs.is_empty() {
            let decls: Vec<serde_json::Value> = request
                .tool_specs
                .iter()
                .map(|t| {
                    json!({
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.parameters,
                    })
                })
                .collect();
            body["tools"] = json!([{ "function_declarations": decls }]);
        }

        if let Some(effort) = self.reasoning_effort {
            body["generation_config"] = json!({
                "thinking_config": {
                    "include_thoughts": true,
                    "thinking_level": effort.as_str().to_uppercase(),
                }
            });
        }

        let req_builder = RequestBuilder::new(http::Method::POST, &url)
            .headers(headers)
            .json(&body);

        self.client
            .send_raw(req_builder, "Google")
            .await
            .map_err(|e| ProviderError::protocol("Google", e.to_string()))
    }
}

#[async_trait]
impl Provider for GoogleInteractionsProvider {
    fn model(&self) -> String {
        self.endpoint.model.clone()
    }

    fn provider_id(&self) -> String {
        self.endpoint.id.clone()
    }

    fn model_capabilities(&self) -> ModelCapabilities {
        self.capabilities.clone()
    }

    fn wire_protocol(&self) -> Option<WireProtocol> {
        Some(WireProtocol::GoogleInteractions)
    }

    fn prompt_hints(&self) -> ProviderPromptHints {
        ProviderPromptHints {
            system_guidance: "",
        }
    }

    async fn chat(&self, request: ModelRequest) -> Result<ProviderCompletion, ProviderError> {
        let response = self.send_interactions_request(&request, false).await?;
        let response = ensure_success(response, "Google", Some(&self.endpoint.model))
            .await
            .map_err(|e| {
                crate::response::clarify_error(e, &self.endpoint.model, &self.endpoint.base_url)
            })?;

        let response_json: serde_json::Value = decode_response_json(response, "Google").await?;
        let wire_resp =
            nuo_inference_wire::protocol::google_interactions::parse_response(&response_json)
                .map_err(|e| ProviderError::protocol("Google", e.to_string()))?;

        let message = Message::new(Role::Assistant, wire_resp.content.unwrap_or_default());
        let meta = ProviderCompletionMeta {
            usage: Some(TokenUsage {
                prompt_tokens: wire_resp.usage.prompt_tokens as i64,
                completion_tokens: wire_resp.usage.completion_tokens as i64,
                total_tokens: wire_resp.usage.total_tokens as i64,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: wire_resp.usage.cached_tokens as i64,
                cache_miss_input_tokens: 0,
                reasoning_tokens: 0,
            }),
            artifacts: None,
            continuation: None,
        };

        Ok(ProviderCompletion { message, meta })
    }

    async fn stream_chat(
        &self,
        request: ModelRequest,
    ) -> Result<ProviderTextStream, ProviderError> {
        let events = self.stream_chat_events(request).await?;
        let text_stream = events.filter_map(|ev| async move {
            match ev {
                Ok(ProviderStreamEvent::TextDelta(txt)) => Some(Ok(txt)),
                _ => None,
            }
        });
        Ok(text_stream.boxed())
    }

    async fn stream_chat_events(
        &self,
        request: ModelRequest,
    ) -> Result<ProviderEventStream, ProviderError> {
        let response = self.send_interactions_request(&request, true).await?;
        let response = ensure_success(response, "Google", Some(&self.endpoint.model))
            .await
            .map_err(|e| {
                crate::response::clarify_error(e, &self.endpoint.model, &self.endpoint.base_url)
            })?;

        let stream =
            nuo_provider::transport::sse::data_payloads(response, "Google").flat_map(move |item| {
                let mut events: Vec<Result<ProviderStreamEvent, ProviderError>> = Vec::new();
                match item {
                    Ok(payload) => {
                        if payload == "[DONE]" {
                            events.push(Ok(ProviderStreamEvent::Completed(
                                ProviderCompletionMeta::default(),
                            )));
                        } else if let Ok(json_val) =
                            serde_json::from_str::<serde_json::Value>(&payload)
                        {
                            match nuo_inference_wire::protocol::google_interactions::parse_chunk(
                                &json_val,
                            ) {
                                Ok(Some(chunk)) => {
                                    if let Some(content) = chunk.delta_content {
                                        events.push(Ok(ProviderStreamEvent::TextDelta(content)));
                                    }
                                    if let Some(thinking) = chunk.delta_thinking {
                                        events.push(Ok(ProviderStreamEvent::ReasoningDelta(
                                            thinking,
                                        )));
                                    }
                                    for tc in chunk.delta_tool_calls {
                                        events.push(Ok(ProviderStreamEvent::ToolCallDelta {
                                            index: tc.index,
                                            id: tc.id,
                                            name: tc.name,
                                            arguments: tc.arguments_delta.unwrap_or_default(),
                                        }));
                                    }
                                    if chunk.is_done {
                                        let meta = ProviderCompletionMeta {
                                            usage: chunk.usage.map(|u| TokenUsage {
                                                prompt_tokens: u.prompt_tokens as i64,
                                                completion_tokens: u.completion_tokens as i64,
                                                total_tokens: u.total_tokens as i64,
                                                cache_creation_input_tokens: 0,
                                                cache_read_input_tokens: u.cached_tokens as i64,
                                                cache_miss_input_tokens: 0,
                                                reasoning_tokens: 0,
                                            }),
                                            artifacts: None,
                                            continuation: None,
                                        };
                                        events.push(Ok(ProviderStreamEvent::Completed(meta)));
                                    }
                                }
                                Ok(None) => {}
                                Err(e) => {
                                    events.push(Err(ProviderError::protocol(
                                        "Google",
                                        e.to_string(),
                                    )));
                                }
                            }
                        }
                    }
                    Err(e) => {
                        events.push(Err(ProviderError::protocol("Google", e.to_string())));
                    }
                }
                futures::stream::iter(events)
            });

        Ok(stream.boxed())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn google_interactions_provider_metadata() {
        let creds = nuo_provider::static_credential("test-key");
        let provider = GoogleInteractionsProvider::new(
            creds,
            "gemini-3.8-flash".to_string(),
            "https://generativelanguage.googleapis.com/v1beta",
        )
        .with_id("google-main");

        assert_eq!(provider.model(), "gemini-3.8-flash");
        assert_eq!(provider.provider_id(), "google-main");
        assert_eq!(
            provider.wire_protocol(),
            Some(WireProtocol::GoogleInteractions)
        );
    }
}
