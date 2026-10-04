//! Tests for the Anthropic provider module.
//!
//! Adapted from the original monolithic `anthropic_compat.rs` tests to the new
//! layered API: tests call the pure `request::body` / `response::*` functions
//! directly rather than the old `provider.request_body` method.

use super::request::{self, BodyInput};
use super::response;
use super::*;
use nuo_model_codec::{
    Effort, Message, PromptCacheMode, ReasoningMode, ResolvedCachePolicy, Role, Tool,
};
use serde_json::{Value, json};
use std::sync::Arc;

static DEFAULT_UNSUPPORTED_CACHE_PLAN: ResolvedCachePolicy = ResolvedCachePolicy::Unsupported;
static DEFAULT_EXPLICIT_CACHE_PLAN: ResolvedCachePolicy = ResolvedCachePolicy::Enabled {
    mode: PromptCacheMode::Explicit,
    retention: Some(nuo_model_codec::CacheRetention::FiveMinutes),
    routing_key: None,
    max_breakpoints: Some(4),
};

// request body shape

fn body_input<'a>(provider: &'a AnthropicMessagesProvider, stream: bool) -> BodyInput<'a> {
    BodyInput {
        model: &provider.endpoint.model,
        stream,
        instructions: None,
        tool_specs: None,
        max_tokens: provider.max_tokens,
        thinking: provider.thinking,
        cache_plan: &DEFAULT_UNSUPPORTED_CACHE_PLAN,
    }
}

/// Route capabilities with only the vision declaration varied.
fn caps_with_vision(vision: Option<bool>) -> nuo_model_codec::ModelCapabilities {
    nuo_model_codec::ModelCapabilities {
        family: "claude".into(),
        context_window: 200_000,
        max_output_tokens: None,
        thinking: nuo_model_codec::ReasoningSupport::None,
        tool_call: true,
        vision,
        effort_levels: Vec::new(),
    }
}

fn image_message(text: &str) -> Message {
    Message::new(Role::User, text).with_images(vec![nuo_model_codec::ImagePart {
        mime: "image/png".to_string(),
        data: "aGk=".to_string(),
    }])
}

#[test]
fn declared_text_only_route_projects_image_blocks_away() {
    // Anthropic `image` blocks were emitted unconditionally before ADR-0230, so
    // a route that declares no image input failed the whole turn.
    let provider =
        AnthropicMessagesProvider::new("k".to_string(), "minimax-m3".to_string(), "https://x");
    let body = request::body_with_capabilities(
        vec![image_message("look")],
        body_input(&provider, false),
        &caps_with_vision(Some(false)),
    );

    let blocks = body["messages"][0]["content"].as_array().unwrap();
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0]["type"], "text");
    assert_eq!(blocks[0]["text"], "look");
}

#[test]
fn undeclared_route_keeps_image_blocks() {
    let provider =
        AnthropicMessagesProvider::new("k".to_string(), "minimax-m3".to_string(), "https://x");
    let body = request::body_with_capabilities(
        vec![image_message("look")],
        body_input(&provider, false),
        &caps_with_vision(None),
    );

    let blocks = body["messages"][0]["content"].as_array().unwrap();
    assert_eq!(blocks[1]["type"], "image");
    assert_eq!(blocks[1]["source"]["media_type"], "image/png");
    assert_eq!(blocks[1]["source"]["data"], "aGk=");
}

#[test]
fn request_body_lifts_system_to_top_level() {
    let provider =
        AnthropicMessagesProvider::new("k".to_string(), "minimax-m3".to_string(), "https://x");
    let body = request::body(
        vec![
            Message::new(Role::System, "you are concise"),
            Message::new(Role::User, "hi"),
        ],
        body_input(&provider, false),
    );
    assert_eq!(body["system"], "you are concise");
    let msgs = body["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0]["role"], "user");
}

#[test]
fn request_body_serializes_tool_result_as_user_block() {
    let provider =
        AnthropicMessagesProvider::new("k".to_string(), "minimax-m3".to_string(), "https://x");
    let body = request::body(
        vec![
            Message::new(Role::User, "run it"),
            Message {
                role: Role::Assistant,
                content: String::new(),
                tool_calls: Some(vec![nuo_model_codec::ToolCall {
                    id: "toolu_1".to_string(),
                    name: "bash".to_string(),
                    arguments: "{}".to_string(),
                }]),
                ..Message::new(Role::Assistant, "")
            },
            Message {
                role: Role::Tool,
                content: "done".to_string(),
                tool_call_id: Some("toolu_1".to_string()),
                ..Message::new(Role::Tool, "")
            },
        ],
        body_input(&provider, false),
    );
    let msgs = body["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 3);
    assert_eq!(msgs[2]["role"], "user");
    assert_eq!(msgs[2]["content"][0]["type"], "tool_result");
    assert_eq!(msgs[2]["content"][0]["tool_use_id"], "toolu_1");
}

struct DummyTool;
#[async_trait]
impl Tool for DummyTool {
    fn name(&self) -> &str {
        "dummy"
    }
    fn description(&self) -> &str {
        "test"
    }
    fn parameters_schema(&self) -> Value {
        json!({"type":"object","properties":{}})
    }
    async fn execute(&self, _: &nuo_tool::ToolContext, _: Value) -> std::result::Result<nuo_tool::ToolOutput, nuo_tool::ToolError> {
        Ok(nuo_tool::ToolOutput::success("ok"))
    }
}

struct DummyTool2;
#[async_trait]
impl Tool for DummyTool2 {
    fn name(&self) -> &str {
        "dummy2"
    }
    fn description(&self) -> &str {
        "test2"
    }
    fn parameters_schema(&self) -> Value {
        json!({"type":"object","properties":{}})
    }
    async fn execute(&self, _: &nuo_tool::ToolContext, _: Value) -> std::result::Result<nuo_tool::ToolOutput, nuo_tool::ToolError> {
        Ok(nuo_tool::ToolOutput::success("ok"))
    }
}

#[test]
fn request_body_includes_tools_in_anthropic_shape() {
    let provider =
        AnthropicMessagesProvider::new("k".to_string(), "minimax-m3".to_string(), "https://x");
    let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(DummyTool)];
    let request =
        nuo_model_codec::ModelRequest::with_tools(vec![Message::new(Role::User, "hi")], &tools);
    let (messages, tool_specs) = request.into_parts();
    let body = request::body(
        messages,
        BodyInput {
            model: &provider.endpoint.model,
            stream: false,
            instructions: None,
            tool_specs: Some(&tool_specs),
            max_tokens: provider.max_tokens,
            thinking: provider.thinking,
            cache_plan: &DEFAULT_UNSUPPORTED_CACHE_PLAN,
        },
    );
    let tool = &body["tools"][0];
    assert_eq!(tool["name"], "dummy");
    assert!(tool.get("input_schema").is_some(), "needs input_schema");
    assert!(tool.get("function").is_none());
}

// prompt-caching breakpoints

/// Count every `cache_control` breakpoint across `tools` + `system` +
/// `messages`.
fn count_cache_breakpoints(body: &Value) -> usize {
    let mut n = 0;
    if let Some(tools) = body["tools"].as_array() {
        n += tools
            .iter()
            .filter(|t| t.get("cache_control").is_some())
            .count();
    }
    if let Some(system) = body["system"].as_array() {
        n += system
            .iter()
            .filter(|b| b.get("cache_control").is_some())
            .count();
    }
    if let Some(msgs) = body["messages"].as_array() {
        for msg in msgs {
            if let Some(blocks) = msg["content"].as_array() {
                n += blocks
                    .iter()
                    .filter(|b| b.get("cache_control").is_some())
                    .count();
            }
        }
    }
    n
}

#[test]
fn cache_breakpoints_use_all_four_slots() {
    let provider =
        AnthropicMessagesProvider::new("k".to_string(), "minimax-m3".to_string(), "https://x");
    let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(DummyTool)];
    let request = nuo_model_codec::ModelRequest::with_tools(
        vec![
            Message::new(Role::System, "you are a coding agent"),
            Message::new(Role::User, "do task A"),
            Message::new(Role::Assistant, "ok"),
            Message::new(Role::User, "now task B"),
            Message::new(Role::Assistant, "done"),
            Message::new(Role::User, "task C"),
        ],
        &tools,
    );
    let (messages, tool_specs) = request.into_parts();
    let body = request::body(
        messages,
        BodyInput {
            model: &provider.endpoint.model,
            stream: false,
            instructions: None,
            tool_specs: Some(&tool_specs),
            max_tokens: provider.max_tokens,
            thinking: provider.thinking,
            cache_plan: &DEFAULT_EXPLICIT_CACHE_PLAN,
        },
    );
    assert_eq!(body["tools"][0]["cache_control"]["type"], "ephemeral");
    assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");
    let msgs = body["messages"].as_array().unwrap();
    let last = &msgs[msgs.len() - 1]["content"][0];
    let prev = &msgs[msgs.len() - 2]["content"][0];
    assert_eq!(last["cache_control"]["type"], "ephemeral");
    assert_eq!(prev["cache_control"]["type"], "ephemeral");
    let earlier = &msgs[0]["content"][0];
    assert!(earlier.get("cache_control").is_none());
    assert_eq!(count_cache_breakpoints(&body), 4);
}

#[test]
fn disabled_cache_plan_omits_cache_breakpoints() {
    let provider =
        AnthropicMessagesProvider::new("k".to_string(), "minimax-m3".to_string(), "https://x");
    let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(DummyTool)];
    let request = nuo_model_codec::ModelRequest::with_tools(
        vec![
            Message::new(Role::System, "you are a coding agent"),
            Message::new(Role::User, "compact context summary"),
        ],
        &tools,
    );
    let (messages, tool_specs) = request.into_parts();
    let body = request::body(
        messages,
        BodyInput {
            model: &provider.endpoint.model,
            stream: false,
            instructions: None,
            tool_specs: Some(&tool_specs),
            max_tokens: provider.max_tokens,
            thinking: provider.thinking,
            cache_plan: &nuo_model_codec::ResolvedCachePolicy::Disabled,
        },
    );
    assert_eq!(count_cache_breakpoints(&body), 0);
}

#[test]
fn cache_breakpoints_never_exceed_four_cap() {
    let provider =
        AnthropicMessagesProvider::new("k".to_string(), "minimax-m3".to_string(), "https://x");
    let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(DummyTool), Arc::new(DummyTool2)];
    let history: Vec<Message> = (0..8)
        .flat_map(|i| {
            vec![
                Message::new(Role::User, format!("u{i}")),
                Message::new(Role::Assistant, format!("a{i}")),
            ]
        })
        .collect();
    let request = nuo_model_codec::ModelRequest::with_tools(history, &tools);
    let (messages, tool_specs) = request.into_parts();
    let body = request::body(
        messages,
        BodyInput {
            model: &provider.endpoint.model,
            stream: false,
            instructions: None,
            tool_specs: Some(&tool_specs),
            max_tokens: provider.max_tokens,
            thinking: provider.thinking,
            cache_plan: &DEFAULT_EXPLICIT_CACHE_PLAN,
        },
    );
    assert!(
        count_cache_breakpoints(&body) <= 4,
        "must not exceed the 4-breakpoint cap"
    );
}

#[test]
fn cache_breakpoints_use_default_five_minute_ttl() {
    let provider =
        AnthropicMessagesProvider::new("k".to_string(), "minimax-m3".to_string(), "https://x");
    let body = request::body(
        vec![
            Message::new(Role::System, "sys"),
            Message::new(Role::User, "hi"),
            Message::new(Role::Assistant, "hey"),
            Message::new(Role::User, "bye"),
        ],
        body_input(&provider, false),
    );
    let breakpoint_with_ttl = ["tools", "system"]
        .iter()
        .filter_map(|key| body[*key].as_array())
        .flatten()
        .chain(
            body["messages"]
                .as_array()
                .into_iter()
                .flatten()
                .flat_map(|m| m["content"].as_array().into_iter().flatten()),
        )
        .find(|b| b.get("cache_control").is_some() && b["cache_control"].get("ttl").is_some());
    assert!(
        breakpoint_with_ttl.is_none(),
        "no breakpoint should carry a ttl override"
    );
}

#[test]
fn cache_breakpoints_degrade_when_regions_absent() {
    let provider =
        AnthropicMessagesProvider::new("k".to_string(), "minimax-m3".to_string(), "https://x");
    let mut input = body_input(&provider, false);
    input.cache_plan = &DEFAULT_EXPLICIT_CACHE_PLAN;
    let body = request::body(
        vec![
            Message::new(Role::User, "first"),
            Message::new(Role::Assistant, "second"),
            Message::new(Role::User, "third"),
        ],
        input,
    );
    assert!(body.get("system").is_none());
    assert!(body.get("tools").is_none());
    assert_eq!(count_cache_breakpoints(&body), 2);
}

#[test]
fn cache_breakpoints_skip_non_stampable_system_shape() {
    let provider =
        AnthropicMessagesProvider::new("k".to_string(), "minimax-m3".to_string(), "https://x");
    let mut input = body_input(&provider, false);
    input.cache_plan = &DEFAULT_EXPLICIT_CACHE_PLAN;
    let body = request::body(
        vec![
            Message::new(Role::System, ""),
            Message::new(Role::User, "hi"),
            Message::new(Role::Assistant, "yo"),
        ],
        input,
    );
    assert!(body.get("system").is_none() || body["system"].as_array().is_none());
    assert_eq!(count_cache_breakpoints(&body), 2);
}

// extended-thinking / effort stamping

#[test]
fn claude_request_body_omits_thinking_by_default() {
    let provider =
        AnthropicMessagesProvider::new("k".to_string(), "claude-opus-4-8".to_string(), "https://x");
    let body = request::body(
        vec![Message::new(Role::User, "hi")],
        body_input(&provider, false),
    );
    assert!(
        body.get("thinking").is_none(),
        "Claude defaults to thinking off (opt-in)"
    );
    assert!(
        body.get("output_config").is_none(),
        "no explicit effort omits output_config"
    );
}

#[test]
fn claude_request_body_injects_adaptive_thinking_when_opted_in() {
    let provider =
        AnthropicMessagesProvider::new("k".to_string(), "claude-opus-4-8".to_string(), "https://x")
            .with_thinking(ThinkingConfig::default().with_mode(ReasoningMode::Adaptive));
    let body = request::body(
        vec![Message::new(Role::User, "hi")],
        body_input(&provider, false),
    );
    assert_eq!(body["thinking"]["type"], "adaptive");
    assert_eq!(body["thinking"]["display"], "summarized");
    assert!(
        body.get("output_config").is_none(),
        "default high effort omits output_config"
    );
}

#[test]
fn haiku_uses_manual_thinking_not_adaptive_when_opted_in() {
    let provider = AnthropicMessagesProvider::new(
        "k".to_string(),
        "claude-haiku-4-5-20251001".to_string(),
        "https://x",
    )
    .with_thinking(
        ThinkingConfig::default()
            .with_mode(ReasoningMode::Adaptive)
            .with_effort(Effort::Max),
    );
    let body = request::body(
        vec![Message::new(Role::User, "hi")],
        body_input(&provider, false),
    );
    assert_eq!(body["thinking"]["type"], "enabled", "manual, not adaptive");
    let budget = body["thinking"]["budget_tokens"].as_u64().unwrap();
    assert!(budget > 0 && budget < u64::from(provider.max_tokens));
    assert!(
        body.get("output_config").is_none(),
        "Haiku rejects effort — output_config must be dropped"
    );
    assert_eq!(
        request::beta_header(&provider.capabilities, provider.thinking),
        Some("interleaved-thinking-2025-05-14"),
    );
}

#[test]
fn haiku_omits_thinking_and_beta_when_off() {
    let provider = AnthropicMessagesProvider::new(
        "k".to_string(),
        "claude-haiku-4-5-20251001".to_string(),
        "https://x",
    );
    let body = request::body(
        vec![Message::new(Role::User, "hi")],
        body_input(&provider, false),
    );
    assert!(body.get("thinking").is_none());
    assert_eq!(
        request::beta_header(&provider.capabilities, provider.thinking),
        None
    );
}

#[test]
fn sonnet_46_clamps_xhigh_to_high_but_opus_48_keeps_it() {
    let sonnet = AnthropicMessagesProvider::new(
        "k".to_string(),
        "claude-sonnet-4-6".to_string(),
        "https://x",
    )
    .with_thinking(
        ThinkingConfig::default()
            .with_mode(ReasoningMode::Adaptive)
            .with_effort(Effort::Xhigh),
    );
    let body = request::body(
        vec![Message::new(Role::User, "hi")],
        body_input(&sonnet, false),
    );
    assert_eq!(body["output_config"]["effort"], "high");

    let opus =
        AnthropicMessagesProvider::new("k".to_string(), "claude-opus-4-8".to_string(), "https://x")
            .with_thinking(
                ThinkingConfig::default()
                    .with_mode(ReasoningMode::Adaptive)
                    .with_effort(Effort::Xhigh),
            );
    let body = request::body(
        vec![Message::new(Role::User, "hi")],
        body_input(&opus, false),
    );
    assert_eq!(body["output_config"]["effort"], "xhigh");
}

#[test]
fn unknown_relay_model_omits_thinking_by_default() {
    let provider = AnthropicMessagesProvider::new(
        "k".to_string(),
        "some-unknown-relay-model".to_string(),
        "https://x",
    );
    let body = request::body(
        vec![Message::new(Role::User, "hi")],
        body_input(&provider, false),
    );
    assert!(
        body.get("thinking").is_none(),
        "unknown relay model defaults to thinking off"
    );
}

#[test]
fn known_relay_model_also_defaults_to_thinking_off() {
    let provider =
        AnthropicMessagesProvider::new("k".to_string(), "minimax-m3".to_string(), "https://x");
    let body = request::body(
        vec![Message::new(Role::User, "hi")],
        body_input(&provider, false),
    );
    assert!(
        body.get("thinking").is_none(),
        "known relay model also defaults to thinking off (opt-in)"
    );
}

#[test]
fn non_default_effort_is_stamped_into_output_config() {
    let provider =
        AnthropicMessagesProvider::new("k".to_string(), "claude-opus-4-8".to_string(), "https://x")
            .with_thinking(
                ThinkingConfig::default()
                    .with_mode(ReasoningMode::Adaptive)
                    .with_effort(Effort::Max),
            );
    let body = request::body(
        vec![Message::new(Role::User, "hi")],
        body_input(&provider, false),
    );
    assert_eq!(body["output_config"]["effort"], "max");
    assert_eq!(body["thinking"]["type"], "adaptive");
}

#[test]
fn effort_clamps_to_model_support_levels() {
    let cfg = ThinkingConfig::default().with_effort(Effort::Xhigh);
    let common: Vec<nuo_model_codec::EffortLevel> = nuo_model_codec::COMMON_LADDER
        .iter()
        .copied()
        .map(Into::into)
        .collect();
    let resolved = cfg.resolve_for(&common);
    assert_eq!(
        resolved.effort,
        Some(Effort::High),
        "xhigh clamps to high on a common-only model"
    );
    let claude_levels: &[Effort] = &[
        Effort::Low,
        Effort::Medium,
        Effort::High,
        Effort::Xhigh,
        Effort::Max,
    ];
    let claude: Vec<nuo_model_codec::EffortLevel> = claude_levels
        .iter()
        .copied()
        .map(Into::into)
        .collect();
    let resolved_claude = cfg.resolve_for(&claude);
    assert_eq!(resolved_claude.effort, Some(Effort::Xhigh));
}

#[test]
fn explicit_high_effort_is_honored_not_swallowed() {
    let provider =
        AnthropicMessagesProvider::new("k".to_string(), "claude-opus-4-8".to_string(), "https://x")
            .with_thinking(ThinkingConfig::default().with_effort(Effort::High));
    let body = request::body(
        vec![Message::new(Role::User, "hi")],
        body_input(&provider, false),
    );
    assert_eq!(
        body["output_config"]["effort"], "high",
        "an explicitly-pinned high effort must be emitted, not swallowed"
    );
}

#[test]
fn effort_without_thinking_stays_decoupled() {
    let provider =
        AnthropicMessagesProvider::new("k".to_string(), "claude-opus-4-8".to_string(), "https://x")
            .with_thinking(ThinkingConfig::default().with_effort(Effort::Medium));
    let body = request::body(
        vec![Message::new(Role::User, "hi")],
        body_input(&provider, false),
    );
    assert_eq!(body["output_config"]["effort"], "medium");
    assert!(
        body.get("thinking").is_none(),
        "effort alone must not enable thinking — the two stay decoupled"
    );
}

// extended-thinking replay (message conversion)

#[test]
fn assistant_message_replays_signed_thinking_block() {
    let prior = Message {
        role: Role::Assistant,
        content: "answer".to_string(),
        reasoning_content: Some("let me think".to_string()),
        provider_meta: Some({
            let mut m = serde_json::Map::new();
            m.insert(
                "thinking_signature".to_string(),
                Value::String("sig_abc".to_string()),
            );
            m
        }),
        ..Message::new(Role::Assistant, "")
    };
    let wire = request::message_obj(prior);
    let blocks = wire["content"]
        .as_array()
        .expect("content is a block array");
    assert_eq!(blocks[0]["type"], "thinking");
    assert_eq!(blocks[0]["thinking"], "let me think");
    assert_eq!(
        blocks[0]["signature"], "sig_abc",
        "signature must round-trip"
    );
    assert_eq!(blocks[1]["type"], "text");
    assert_eq!(blocks[1]["text"], "answer");
}

#[test]
fn assistant_message_omits_unsigned_thinking_without_signature() {
    let prior = Message {
        role: Role::Assistant,
        content: "x".to_string(),
        reasoning_content: Some("hmm".to_string()),
        provider_meta: None,
        ..Message::new(Role::Assistant, "")
    };
    let wire = request::message_obj(prior);
    let blocks = wire["content"].as_array().unwrap();
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0]["type"], "text");
    assert_eq!(blocks[0]["text"], "x");
}

#[test]
fn assistant_message_omits_thinking_block_when_no_reasoning() {
    let prior = Message::new(Role::Assistant, "just text");
    let wire = request::message_obj(prior);
    let blocks = wire["content"].as_array().unwrap();
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0]["type"], "text");
}

// usage parsing

#[test]
fn anthropic_usage_folds_cache_tokens_into_prompt_total() {
    let usage = json!({
        "input_tokens": 200,
        "output_tokens": 50,
        "cache_creation_input_tokens": 5000,
        "cache_read_input_tokens": 8000,
    });
    let parsed = response::usage(&usage).expect("usage parses");
    assert_eq!(
        parsed.prompt_tokens, 13200,
        "cache tokens folded into prompt"
    );
    assert_eq!(parsed.completion_tokens, 50);
    assert_eq!(parsed.total_tokens, 13250);
    assert_eq!(parsed.cache_creation_input_tokens, 5000);
    assert_eq!(parsed.cache_read_input_tokens, 8000);
}

#[test]
fn anthropic_usage_without_cache_fields_defaults_to_zero() {
    let usage = json!({"input_tokens": 100, "output_tokens": 30});
    let parsed = response::usage(&usage).expect("usage parses");
    assert_eq!(parsed.prompt_tokens, 100);
    assert_eq!(parsed.total_tokens, 130);
    assert_eq!(parsed.cache_creation_input_tokens, 0);
    assert_eq!(parsed.cache_read_input_tokens, 0);
}

#[test]
fn anthropic_usage_absent_returns_none() {
    assert!(response::usage(&json!({})).is_none());
    let parsed = response::usage(&json!({"output_tokens": 5})).unwrap();
    assert_eq!(parsed.prompt_tokens, 0);
    assert_eq!(parsed.completion_tokens, 5);
    assert_eq!(parsed.total_tokens, 5);
}

#[test]
fn prompt_hints_emit_no_system_guidance() {
    let provider = AnthropicMessagesProvider::new(
        "k".to_string(),
        "claude-sonnet-4-6".to_string(),
        "https://x",
    );
    // No protocol note: thinking signatures travel as opaque provider_meta
    // and replay into the wire thinking block only — never into content the
    // model can read, so there is nothing for a prompt note to guard.
    assert!(provider.prompt_hints().system_guidance.is_empty());
}

#[test]
fn workspace_scoped_credential_carries_the_org_header() {
    let provider = AnthropicMessagesProvider::new(
        "st-token".to_string(),
        "qwen3.6-plus".to_string(),
        "https://opencode.ai/inference/anthropic/v1/messages",
    );
    let auth = nuo_model_codec::ResolvedAuth::new("st-token").with_extension(
        nuo_model_codec::OpencodeAuthMetadata {
            org_id: "wrk_workspace_1".to_string(),
        },
    );
    let req = provider
        .build_request_for_auth(&json!({"model": "qwen3.6-plus"}), &auth)
        .build("Anthropic")
        .expect("request builds");
    assert_eq!(
        req.headers
            .get("x-opencode-org-id")
            .and_then(|value| value.to_str().ok())
            .expect("workspace header present"),
        "wrk_workspace_1"
    );
    // The Console relay accepts the stock Anthropic credential header
    // unchanged (ADR-0269 probe P1), so the wire keeps `x-api-key`.
    assert!(req.headers.get("x-api-key").is_some());
}
