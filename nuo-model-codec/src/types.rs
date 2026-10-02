//! Unified request, response, and canonical block-level IR wire types for model APIs.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WireRole {
    System,
    User,
    Assistant,
    Tool,
}

/// Prompt caching control directive for caching breakpoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheControl {
    Ephemeral,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireToolResult {
    pub call_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub output: String,
    pub is_error: bool,
}

/// Canonical content block IR representing typed components inside a message.
///
/// Preserves the strict temporal ordering of text, reasoning thinking, images,
/// and tool interactions across multi-vendor model boundaries.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    Thinking {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
    Image {
        media_type: String,
        data: String,
    },
    ToolCall(WireToolCall),
    ToolResult(WireToolResult),
}

impl ContentBlock {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { text: text.into() }
    }

    pub fn thinking(text: impl Into<String>) -> Self {
        Self::Thinking {
            text: text.into(),
            signature: None,
        }
    }

    pub fn image(media_type: impl Into<String>, base64_data: impl Into<String>) -> Self {
        Self::Image {
            media_type: media_type.into(),
            data: base64_data.into(),
        }
    }

    pub fn tool_call(
        id: impl Into<String>,
        name: impl Into<String>,
        arguments: serde_json::Value,
    ) -> Self {
        Self::ToolCall(WireToolCall {
            id: id.into(),
            name: name.into(),
            arguments,
        })
    }

    pub fn tool_result(call_id: impl Into<String>, output: impl Into<String>, is_error: bool) -> Self {
        Self::ToolResult(WireToolResult {
            call_id: call_id.into(),
            name: None,
            output: output.into(),
            is_error,
        })
    }

    pub fn tool_result_named(
        call_id: impl Into<String>,
        name: impl Into<String>,
        output: impl Into<String>,
        is_error: bool,
    ) -> Self {
        Self::ToolResult(WireToolResult {
            call_id: call_id.into(),
            name: Some(name.into()),
            output: output.into(),
            is_error,
        })
    }
}

/// Structured wire message consisting of ordered content blocks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireMessage {
    pub role: WireRole,
    pub blocks: Vec<ContentBlock>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
}

impl WireMessage {
    pub fn new(role: WireRole, blocks: Vec<ContentBlock>) -> Self {
        Self {
            role,
            blocks,
            name: None,
            cache_control: None,
        }
    }

    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: WireRole::System,
            blocks: vec![ContentBlock::text(content)],
            name: None,
            cache_control: None,
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: WireRole::User,
            blocks: vec![ContentBlock::text(content)],
            name: None,
            cache_control: None,
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: WireRole::Assistant,
            blocks: vec![ContentBlock::text(content)],
            name: None,
            cache_control: None,
        }
    }

    pub fn with_block(mut self, block: ContentBlock) -> Self {
        self.blocks.push(block);
        self
    }

    pub fn with_thinking(mut self, thinking: impl Into<String>) -> Self {
        self.blocks.insert(0, ContentBlock::thinking(thinking));
        self
    }

    pub fn with_cache_control(mut self, cache_control: CacheControl) -> Self {
        self.cache_control = Some(cache_control);
        self
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Primary text representation concatenated from all text blocks.
    pub fn text_content(&self) -> String {
        let mut text = String::new();
        for block in &self.blocks {
            if let ContentBlock::Text { text: t } = block {
                text.push_str(t);
            }
        }
        text
    }

    /// Extracted reasoning thinking text if present.
    pub fn thinking_content(&self) -> Option<String> {
        for block in &self.blocks {
            if let ContentBlock::Thinking { text, .. } = block {
                return Some(text.clone());
            }
        }
        None
    }

    /// Collects all tool calls contained within this message.
    pub fn tool_calls(&self) -> Vec<WireToolCall> {
        let mut calls = Vec::new();
        for block in &self.blocks {
            if let ContentBlock::ToolCall(call) = block {
                calls.push(call.clone());
            }
        }
        calls
    }

    /// Collects all tool results contained within this message.
    pub fn tool_results(&self) -> Vec<WireToolResult> {
        let mut results = Vec::new();
        for block in &self.blocks {
            if let ContentBlock::ToolResult(res) = block {
                results.push(res.clone());
            }
        }
        results
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireTool {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

impl WireTool {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: serde_json::Value,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parameters,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct WireUsage {
    pub prompt_tokens: usize,
    pub completion_tokens: usize,
    pub total_tokens: usize,
    pub cached_tokens: usize,
}

impl WireUsage {
    pub fn new(prompt: usize, completion: usize) -> Self {
        Self {
            prompt_tokens: prompt,
            completion_tokens: completion,
            total_tokens: prompt + completion,
            cached_tokens: 0,
        }
    }

    pub fn accumulate(&mut self, other: &WireUsage) {
        self.prompt_tokens += other.prompt_tokens;
        self.completion_tokens += other.completion_tokens;
        self.total_tokens += other.total_tokens;
        self.cached_tokens += other.cached_tokens;
    }
}

#[derive(Debug, Clone, Default)]
pub struct WireRequest {
    pub messages: Vec<WireMessage>,
    pub tools: Vec<WireTool>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<usize>,
    pub thinking_budget: Option<usize>,
}

#[derive(Debug, Clone, Default)]
pub struct WireResponse {
    pub content: Option<String>,
    pub thinking: Option<String>,
    pub tool_calls: Vec<WireToolCall>,
    pub usage: WireUsage,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireToolCallChunk {
    pub index: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments_delta: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct WireChunk {
    pub delta_content: Option<String>,
    pub delta_thinking: Option<String>,
    pub delta_tool_calls: Vec<WireToolCallChunk>,
    pub usage: Option<WireUsage>,
    pub is_done: bool,
}

impl WireChunk {
    pub fn content(delta: impl Into<String>) -> Self {
        Self {
            delta_content: Some(delta.into()),
            delta_thinking: None,
            delta_tool_calls: Vec::new(),
            usage: None,
            is_done: false,
        }
    }

    pub fn thinking(delta: impl Into<String>) -> Self {
        Self {
            delta_content: None,
            delta_thinking: Some(delta.into()),
            delta_tool_calls: Vec::new(),
            usage: None,
            is_done: false,
        }
    }

    pub fn done() -> Self {
        Self {
            delta_content: None,
            delta_thinking: None,
            delta_tool_calls: Vec::new(),
            usage: None,
            is_done: true,
        }
    }
}

/// In-flight state of a streaming tool call being assembled across multiple SSE frames.
#[derive(Debug, Clone, Default)]
struct PartialToolCall {
    id: Option<String>,
    name: Option<String>,
    arguments_buf: String,
}

/// Stateful stream reducer aggregating chunks into full messages and multi-tool calls without data loss.
#[derive(Debug, Default)]
pub struct StreamAccumulator {
    content_buf: String,
    thinking_buf: String,
    tool_calls: BTreeMap<usize, PartialToolCall>,
    usage: WireUsage,
    is_done: bool,
}

impl StreamAccumulator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds an incoming [`WireChunk`] into the stateful reducer.
    pub fn feed(&mut self, chunk: &WireChunk) {
        if let Some(content) = &chunk.delta_content {
            self.content_buf.push_str(content);
        }

        if let Some(thinking) = &chunk.delta_thinking {
            self.thinking_buf.push_str(thinking);
        }

        for tc in &chunk.delta_tool_calls {
            let entry = self.tool_calls.entry(tc.index).or_default();
            if let Some(id) = &tc.id {
                entry.id = Some(id.clone());
            }
            if let Some(name) = &tc.name {
                entry.name = Some(name.clone());
            }
            if let Some(args_delta) = &tc.arguments_delta {
                entry.arguments_buf.push_str(args_delta);
            }
        }

        if let Some(usage) = &chunk.usage {
            self.usage = *usage;
        }

        if chunk.is_done {
            self.is_done = true;
        }
    }

    /// Whether the stream has received completion signal.
    pub fn is_done(&self) -> bool {
        self.is_done
    }

    /// Current accumulated content text.
    pub fn current_content(&self) -> &str {
        &self.content_buf
    }

    /// Current accumulated thinking text.
    pub fn current_thinking(&self) -> &str {
        &self.thinking_buf
    }

    /// Consolidates accumulated state into a finalized [`WireResponse`].
    pub fn finish(self) -> WireResponse {
        let mut assembled_tool_calls = Vec::with_capacity(self.tool_calls.len());

        for (_, partial) in self.tool_calls {
            let name = partial.name.unwrap_or_default();
            if name.is_empty() {
                continue;
            }

            let id = partial
                .id
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

            let trimmed_args = partial.arguments_buf.trim();
            let arguments = if trimmed_args.is_empty() {
                serde_json::json!({})
            } else {
                serde_json::from_str(trimmed_args).unwrap_or_else(|_| {
                    // If not valid JSON, wrap raw string in a value object
                    serde_json::json!({ "raw": partial.arguments_buf })
                })
            };

            assembled_tool_calls.push(WireToolCall {
                id,
                name,
                arguments,
            });
        }

        WireResponse {
            content: if self.content_buf.is_empty() {
                None
            } else {
                Some(self.content_buf)
            },
            thinking: if self.thinking_buf.is_empty() {
                None
            } else {
                Some(self.thinking_buf)
            },
            tool_calls: assembled_tool_calls,
            usage: self.usage,
        }
    }
}
