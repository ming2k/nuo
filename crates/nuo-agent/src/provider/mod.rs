pub mod mock;
#[cfg(feature = "wire")]
pub mod wire;

use crate::error::Result;
use crate::message::{Message, ToolCall};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub use mock::MockProvider;
#[cfg(feature = "wire")]
pub use wire::{ModelCodecAdapter, WireProvider};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TokenUsage {
    pub prompt_tokens: usize,
    pub completion_tokens: usize,
    pub total_tokens: usize,
    pub cached_tokens: usize,
}

impl TokenUsage {
    pub fn new(prompt: usize, completion: usize) -> Self {
        Self {
            prompt_tokens: prompt,
            completion_tokens: completion,
            total_tokens: prompt + completion,
            cached_tokens: 0,
        }
    }

    pub fn accumulate(&mut self, other: &TokenUsage) {
        self.prompt_tokens += other.prompt_tokens;
        self.completion_tokens += other.completion_tokens;
        self.total_tokens += other.total_tokens;
        self.cached_tokens += other.cached_tokens;
    }
}

#[cfg(feature = "wire")]
impl From<nuo_model_codec::WireUsage> for TokenUsage {
    fn from(u: nuo_model_codec::WireUsage) -> Self {
        Self {
            prompt_tokens: u.prompt_tokens,
            completion_tokens: u.completion_tokens,
            total_tokens: u.total_tokens,
            cached_tokens: u.cached_tokens,
        }
    }
}

#[cfg(feature = "wire")]
impl From<TokenUsage> for nuo_model_codec::WireUsage {
    fn from(u: TokenUsage) -> Self {
        Self {
            prompt_tokens: u.prompt_tokens,
            completion_tokens: u.completion_tokens,
            total_tokens: u.total_tokens,
            cached_tokens: u.cached_tokens,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ModelRequest {
    pub messages: Vec<Message>,
    pub tools: Vec<serde_json::Value>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<usize>,
    pub thinking_budget: Option<usize>,
}

#[derive(Debug, Clone, Default)]
pub struct ModelResponse {
    pub content: Option<String>,
    pub thinking: Option<String>,
    pub tool_calls: Vec<ToolCall>,
    pub usage: TokenUsage,
}

impl ModelResponse {
    pub fn text(
        content: impl Into<String>,
        prompt_tokens: usize,
        completion_tokens: usize,
    ) -> Self {
        Self {
            content: Some(content.into()),
            thinking: None,
            tool_calls: Vec::new(),
            usage: TokenUsage::new(prompt_tokens, completion_tokens),
        }
    }

    pub fn tool_call(
        call_id: impl Into<String>,
        name: impl Into<String>,
        arguments: serde_json::Value,
        prompt_tokens: usize,
        completion_tokens: usize,
    ) -> Self {
        Self {
            content: None,
            thinking: None,
            tool_calls: vec![ToolCall {
                id: call_id.into(),
                name: name.into(),
                arguments,
            }],
            usage: TokenUsage::new(prompt_tokens, completion_tokens),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCallDelta {
    pub index: usize,
    pub id: Option<String>,
    pub name: Option<String>,
    pub arguments_delta: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ProviderDelta {
    pub content_delta: Option<String>,
    pub thinking_delta: Option<String>,
    pub tool_calls: Vec<ToolCall>,
    pub tool_call_delta: Option<(usize, Option<String>, Option<String>)>,
    pub tool_call_deltas: Vec<ToolCallDelta>,
    pub usage: Option<TokenUsage>,
    pub is_done: bool,
}

impl ProviderDelta {
    pub fn content(text: impl Into<String>) -> Self {
        Self {
            content_delta: Some(text.into()),
            thinking_delta: None,
            tool_calls: Vec::new(),
            tool_call_delta: None,
            tool_call_deltas: Vec::new(),
            usage: None,
            is_done: false,
        }
    }

    pub fn thinking(text: impl Into<String>) -> Self {
        Self {
            content_delta: None,
            thinking_delta: Some(text.into()),
            tool_calls: Vec::new(),
            tool_call_delta: None,
            tool_call_deltas: Vec::new(),
            usage: None,
            is_done: false,
        }
    }

    pub fn tool_call_chunk(
        index: usize,
        id: Option<String>,
        name: Option<String>,
        arguments_delta: Option<String>,
    ) -> Self {
        Self {
            content_delta: None,
            thinking_delta: None,
            tool_calls: Vec::new(),
            tool_call_delta: Some((index, name.clone(), arguments_delta.clone())),
            tool_call_deltas: vec![ToolCallDelta {
                index,
                id,
                name,
                arguments_delta,
            }],
            usage: None,
            is_done: false,
        }
    }

    pub fn from_response(resp: ModelResponse) -> Self {
        Self {
            content_delta: resp.content,
            thinking_delta: resp.thinking,
            tool_calls: resp.tool_calls,
            tool_call_delta: None,
            tool_call_deltas: Vec::new(),
            usage: Some(resp.usage),
            is_done: true,
        }
    }
}

#[derive(Default)]
struct InternalToolAccumulator {
    partial: BTreeMap<usize, (Option<String>, Option<String>, String)>,
}

impl InternalToolAccumulator {
    fn feed(&mut self, delta: &ToolCallDelta) {
        let entry = self
            .partial
            .entry(delta.index)
            .or_insert_with(|| (None, None, String::new()));
        if let Some(id) = &delta.id {
            entry.0 = Some(id.clone());
        }
        if let Some(name) = &delta.name {
            entry.1 = Some(name.clone());
        }
        if let Some(args) = &delta.arguments_delta {
            entry.2.push_str(args);
        }
    }

    fn finish(self) -> Vec<ToolCall> {
        let mut calls = Vec::new();
        for (_, (id_opt, name_opt, args_buf)) in self.partial {
            let name = name_opt.unwrap_or_default();
            if name.is_empty() {
                continue;
            }
            let id = id_opt.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            let trimmed = args_buf.trim();
            let arguments = if trimmed.is_empty() {
                serde_json::json!({})
            } else {
                serde_json::from_str(trimmed)
                    .unwrap_or_else(|_| serde_json::json!({ "raw": args_buf }))
            };
            calls.push(ToolCall {
                id,
                name,
                arguments,
            });
        }
        calls
    }
}

#[async_trait]
pub trait Provider: Send + Sync {
    /// Primary entry point: streams real-time completion deltas for the request.
    async fn stream(
        &self,
        request: ModelRequest,
    ) -> Result<futures::channel::mpsc::Receiver<Result<ProviderDelta>>>;

    /// Collects the delta stream into a single aggregated [`ModelResponse`].
    async fn complete(&self, request: ModelRequest) -> Result<ModelResponse> {
        use futures::StreamExt;
        let mut rx = self.stream(request).await?;
        let mut content_acc = String::new();
        let mut thinking_acc = String::new();
        let mut explicit_tool_calls = Vec::new();
        let mut tool_accumulator = InternalToolAccumulator::default();
        let mut usage = TokenUsage::default();

        while let Some(delta_res) = rx.next().await {
            let delta = delta_res?;
            if let Some(c) = delta.content_delta {
                content_acc.push_str(&c);
            }
            if let Some(th) = delta.thinking_delta {
                thinking_acc.push_str(&th);
            }
            if !delta.tool_calls.is_empty() {
                explicit_tool_calls = delta.tool_calls;
            } else {
                for tc in &delta.tool_call_deltas {
                    tool_accumulator.feed(tc);
                }
                if delta.tool_call_deltas.is_empty()
                    && let Some((index, name, args)) = delta.tool_call_delta
                {
                    tool_accumulator.feed(&ToolCallDelta {
                        index,
                        id: None,
                        name,
                        arguments_delta: args,
                    });
                }
            }
            if let Some(u) = delta.usage {
                usage = u;
            }
        }

        let tool_calls = if !explicit_tool_calls.is_empty() {
            explicit_tool_calls
        } else {
            tool_accumulator.finish()
        };

        Ok(ModelResponse {
            content: if content_acc.is_empty() {
                None
            } else {
                Some(content_acc)
            },
            thinking: if thinking_acc.is_empty() {
                None
            } else {
                Some(thinking_acc)
            },
            tool_calls,
            usage,
        })
    }
}
