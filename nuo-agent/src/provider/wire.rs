//! Multi-vendor model provider backed by the `model-wire` protocol engine.
//!
//! Provides first-class support for OpenAI, Anthropic, Google Gemini, DeepSeek, and Ollama.

use super::{ModelRequest, ModelResponse, Provider, TokenUsage};
use crate::error::{AgentError, Result};
use crate::message::{Role, ToolCall};
use async_trait::async_trait;
use nuo_model_codec::{
    ContentBlock, Endpoint, WireClient, WireMessage, WireRequest, WireRole, WireTool,
};

/// A universal model provider powered by the `model-wire` multi-vendor protocol engine.
#[derive(Clone)]
pub struct WireProvider {
    client: WireClient,
    endpoint: Endpoint,
}

impl WireProvider {
    /// Creates a provider for an arbitrary endpoint.
    pub fn new(endpoint: Endpoint) -> Self {
        Self {
            client: WireClient::new(),
            endpoint,
        }
    }

    /// Creates a provider with a shared, pre-configured [`WireClient`] for connection pool reuse.
    pub fn with_client(endpoint: Endpoint, client: WireClient) -> Self {
        Self { client, endpoint }
    }

    /// Creates a provider pre-configured for OpenAI endpoints.
    pub fn openai(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self::new(Endpoint::openai(api_key, model))
    }

    /// Creates a provider pre-configured for Anthropic Messages endpoints (Claude).
    pub fn anthropic(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self::new(Endpoint::anthropic(api_key, model))
    }

    /// Creates a provider pre-configured for DeepSeek endpoints.
    pub fn deepseek(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self::new(Endpoint::deepseek(api_key, model))
    }

    /// Creates a provider pre-configured for Google Gemini endpoints.
    pub fn google(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self::new(Endpoint::google(api_key, model))
    }

    /// Creates a provider pre-configured for local Ollama endpoints.
    pub fn ollama(model: impl Into<String>) -> Self {
        Self::new(Endpoint::ollama(model))
    }

    fn to_wire_request(&self, request: &ModelRequest) -> WireRequest {
        let wire_messages = request
            .messages
            .iter()
            .enumerate()
            .map(|(idx, m)| {
                let role = match m.role {
                    Role::System => WireRole::System,
                    Role::User => WireRole::User,
                    Role::Assistant => WireRole::Assistant,
                    Role::Tool => WireRole::Tool,
                };

                let mut blocks = Vec::new();

                if let Some(th) = &m.thinking {
                    blocks.push(ContentBlock::thinking(th.clone()));
                }

                if !m.content.is_empty() {
                    blocks.push(ContentBlock::text(m.content.clone()));
                }

                for tc in &m.tool_calls {
                    blocks.push(ContentBlock::tool_call(
                        tc.id.clone(),
                        tc.name.clone(),
                        tc.arguments.clone(),
                    ));
                }

                for tr in &m.tool_results {
                    blocks.push(ContentBlock::tool_result_named(
                        tr.call_id.clone(),
                        tr.name.clone(),
                        tr.output.clone(),
                        tr.is_error,
                    ));
                }

                let mut wire_msg = WireMessage::new(role, blocks);

                if let Some(name) = &m.name {
                    wire_msg = wire_msg.with_name(name.clone());
                }

                // Automatically mark system messages with ephemeral cache control for prompt caching
                if idx == 0 && m.role == Role::System {
                    wire_msg =
                        wire_msg.with_cache_control(nuo_model_codec::CacheControl::Ephemeral);
                }

                wire_msg
            })
            .collect();

        let wire_tools = request
            .tools
            .iter()
            .filter_map(|t| {
                let func = t.get("function")?;
                let name = func.get("name")?.as_str()?.to_string();
                let description = func
                    .get("description")
                    .and_then(|d| d.as_str())
                    .unwrap_or("")
                    .to_string();
                let parameters = func
                    .get("parameters")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                Some(WireTool {
                    name,
                    description,
                    parameters,
                })
            })
            .collect();

        WireRequest {
            messages: wire_messages,
            tools: wire_tools,
            temperature: request.temperature,
            max_tokens: request.max_tokens,
            thinking_budget: request.thinking_budget,
        }
    }
}

#[async_trait]
impl Provider for WireProvider {
    async fn complete(&self, request: ModelRequest) -> Result<ModelResponse> {
        let wire_req = self.to_wire_request(&request);
        let resp = self
            .client
            .execute(&self.endpoint, &wire_req)
            .await
            .map_err(|err| AgentError::Provider(err.to_string()))?;

        Ok(ModelResponse {
            content: resp.content,
            thinking: resp.thinking,
            tool_calls: resp
                .tool_calls
                .into_iter()
                .map(|tc| ToolCall {
                    id: tc.id,
                    name: tc.name,
                    arguments: tc.arguments,
                })
                .collect(),
            usage: TokenUsage {
                prompt_tokens: resp.usage.prompt_tokens,
                completion_tokens: resp.usage.completion_tokens,
                total_tokens: resp.usage.total_tokens,
                cached_tokens: resp.usage.cached_tokens,
            },
        })
    }

    async fn stream(
        &self,
        request: ModelRequest,
    ) -> Result<futures::channel::mpsc::Receiver<Result<super::ProviderDelta>>> {
        let wire_req = self.to_wire_request(&request);
        let mut wire_stream = self
            .client
            .execute_stream(&self.endpoint, &wire_req)
            .await
            .map_err(|err| AgentError::Provider(err.to_string()))?;

        let (mut tx, rx) = futures::channel::mpsc::channel(64);
        use futures::{SinkExt, StreamExt};

        tokio::spawn(async move {
            while let Some(chunk_res) = wire_stream.next().await {
                match chunk_res {
                    Ok(chunk) => {
                        let deltas: Vec<super::ToolCallDelta> = chunk
                            .delta_tool_calls
                            .iter()
                            .map(|tc| super::ToolCallDelta {
                                index: tc.index,
                                id: tc.id.clone(),
                                name: tc.name.clone(),
                                arguments_delta: tc.arguments_delta.clone(),
                            })
                            .collect();

                        let primary_delta = deltas
                            .first()
                            .map(|tc| (tc.index, tc.name.clone(), tc.arguments_delta.clone()));

                        let delta = super::ProviderDelta {
                            content_delta: chunk.delta_content,
                            thinking_delta: chunk.delta_thinking,
                            tool_calls: Vec::new(),
                            tool_call_delta: primary_delta,
                            tool_call_deltas: deltas,
                            usage: chunk.usage.map(Into::into),
                            is_done: chunk.is_done,
                        };
                        if tx.send(Ok(delta)).await.is_err() {
                            return;
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(Err(AgentError::Provider(e.to_string()))).await;
                        return;
                    }
                }
            }
        });

        Ok(rx)
    }
}
