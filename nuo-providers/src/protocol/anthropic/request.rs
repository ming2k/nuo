//! Anthropic Messages — request construction.
//!
//! Pure functions turning the harness's `Vec<Message>` into the Anthropic
//! `/messages` request shape: a `messages` array of `{role, content: [blocks]}`,
//! `system` lifted to the top level, `tools` in Anthropic's `{name,
//! input_schema}` shape, prompt-cache breakpoints, and extended-thinking /
//! effort stamps. No `reqwest`, no `async`, no I/O.
//!
//! Anthropic wire shape:
//! - Auth: `x-api-key: <key>` + `anthropic-version: 2023-06-01`. Manual
//!   extended thinking + tools additionally needs `anthropic-beta:
//!   interleaved-thinking-2025-05-14` (see [`beta_header`]).
//! - Body: `{model, messages, system?, tools?, max_tokens, stream,
//!   thinking?, output_config?}`. `system` is a `[{type:"text", text,
//!   cache_control?}]` block array (an array is required to host a cache
//!   breakpoint). `content` is always a block array.
//! - Prompt caching: up to 4 `cache_control: {"type":"ephemeral"}` breakpoints
//!   stamped across `tools → system → messages` (last tool, last system block,
//!   and the two newest messages) so the stable prefix is cached at 0.1× input
//!   cost. See `stamp_cache_control` and friends.

use nuo_contracts::{Message, ReasoningSupport, Role};
use serde_json::{Value, json};

use super::thinking::ThinkingConfig;

/// The `anthropic-version` header pinned for the Messages API. opencode-go's
/// `/v1/messages` surface accepts this value; it is the canonical stable
/// version advertised by Anthropic-compatible relays.
pub const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Inputs to [`body`]: the model id, whether this is a streaming request, the
/// prepared tool schemas (OpenAI function-spec shape — converted here), the
/// required `max_tokens`, and the resolved thinking/effort config.
pub struct BodyInput<'a> {
    pub model: &'a str,
    pub stream: bool,
    /// Structured instructions from the instruction manifest.
    pub instructions: Option<&'a nuo_contracts::InstructionBundle>,
    /// OpenAI-shaped tool specs (`{type:"function", function:{...}}`), if any.
    /// Converted to Anthropic's `{name, description, input_schema}` shape.
    pub tool_specs: Option<&'a [nuo_contracts::ToolSpec]>,
    pub max_tokens: u32,
    pub thinking: ThinkingConfig,
    pub cache_plan: &'a nuo_contracts::ResolvedCachePolicy,
}

/// Build the `/messages` request body from the harness message list.
pub fn body(messages: Vec<Message>, input: BodyInput<'_>) -> Value {
    let capabilities = nuo_contracts::ModelCapabilities::for_channel(input.model, None);
    body_with_capabilities(messages, input, &capabilities)
}

/// Build a request with a provider-channel capability view. The provider calls
/// this for trusted remote metadata; [`body`] remains the static-baseline entry
/// point for standalone callers and tests.
pub fn body_with_capabilities(
    messages: Vec<Message>,
    input: BodyInput<'_>,
    capabilities: &nuo_contracts::ModelCapabilities,
) -> Value {
    let BodyInput {
        model: model_id,
        stream,
        instructions,
        tool_specs,
        max_tokens,
        thinking,
        cache_plan,
    } = input;

    // Image projection is shared across transports (ADR-0230): Anthropic used
    // to emit `image` blocks unconditionally, so a route that declares no image
    // input failed the whole turn on a request the client could have projected
    // correctly. The prose always survives; only the pixels are dropped.
    let (messages, _dropped_images) =
        crate::vision::project_images_for_route(model_id, messages, capabilities);

    let tool_specs = tool_specs.map(|specs| {
        json!(
            specs
                .iter()
                .map(|spec| {
                    // The harness carries provider-neutral ToolSpecs; Anthropic
                    // wants {name, description, input_schema}. The `parameters`
                    // object is already a JSON-Schema fragment and maps verbatim.
                    json!({
                        "name": spec.name,
                        "description": spec.description,
                        "input_schema": spec.parameters.clone(),
                    })
                })
                .collect::<Vec<_>>()
        )
    });

    // Pull leading system message(s) out of the list; Anthropic carries
    // system as a top-level string, not a role.
    let mut system_text = instructions
        .map(|b| b.render_combined())
        .unwrap_or_default();
    let mut conversation: Vec<Message> = Vec::with_capacity(messages.len());
    for message in messages {
        if message.role == Role::System {
            if !system_text.is_empty() {
                system_text.push_str("\n\n");
            }
            system_text.push_str(&message.content);
            continue;
        }
        conversation.push(message);
    }

    // Every assistant `tool_calls` must be followed by a corresponding `tool`
    // result. Collect the ids that got a result, then strip unanswered calls.
    let answered: std::collections::HashSet<String> = conversation
        .iter()
        .filter_map(|m| {
            if m.role == Role::Tool {
                m.tool_call_id.clone()
            } else {
                None
            }
        })
        .collect();
    conversation.retain_mut(|m| {
        if m.role != Role::Assistant {
            return true;
        }
        if let Some(calls) = m.tool_calls.as_mut() {
            calls.retain(|c| answered.contains(&c.id));
            if calls.is_empty() {
                m.tool_calls = None;
            }
        }
        !m.content.is_empty() || m.tool_calls.as_ref().is_some_and(|calls| !calls.is_empty())
    });

    let mut body = json!({
        "model": model_id,
        "messages": conversation.into_iter().map(message_obj).collect::<Vec<_>>(),
        "max_tokens": max_tokens,
        "stream": stream,
    });
    if let Some(specs) = tool_specs {
        body["tools"] = specs;
    }
    if !system_text.is_empty() {
        body["system"] = json!(system_text);
    }

    if let nuo_contracts::ResolvedCachePolicy::Enabled {
        mode, retention, ..
    } = cache_plan
    {
        let control = cache_control(*retention);
        match mode {
            nuo_contracts::PromptCacheMode::Automatic => {
                body["cache_control"] = control;
            }
            nuo_contracts::PromptCacheMode::Explicit => {
                stamp_caching_breakpoints(&mut body, &system_text, &control);
            }
            nuo_contracts::PromptCacheMode::Implicit => {}
        }
    }
    stamp_thinking(&mut body, capabilities, max_tokens, thinking);
    body
}

/// The `anthropic-beta` header value this request requires, if any.
///
/// Manual extended thinking (`thinking:{type:"enabled"}`, used by Haiku 4.5)
/// combined with tool use requires interleaved thinking, gated behind a beta
/// header. Adaptive-thinking models need no beta header. Returns `None` when
/// the resolved model is not a manual-thinking model or the user has thinking
/// turned off.
pub fn beta_header(
    capabilities: &nuo_contracts::ModelCapabilities,
    thinking: ThinkingConfig,
) -> Option<&'static str> {
    thinking
        .needs_manual_beta(capabilities.thinking)
        .then_some("interleaved-thinking-2025-05-14")
}

/// The headers this wire format requires on every request, beyond the
/// always-present `User-Agent`: the auth header, the version, and the optional
/// beta header for manual thinking.
pub fn headers(
    api_key: &str,
    capabilities: &nuo_contracts::ModelCapabilities,
    thinking: ThinkingConfig,
    copilot: bool,
) -> Vec<(&'static str, String)> {
    if copilot {
        let mut headers = vec![("Authorization", format!("Bearer {api_key}"))];
        for (name, value) in crate::COPILOT_CLIENT_HEADERS {
            headers.push((*name, value.to_string()));
        }
        headers.push(("x-initiator", "user".to_string()));
        headers.push(("Openai-Intent", "conversation-edits".to_string()));
        headers.push(("X-GitHub-Api-Version", "2026-06-01".to_string()));
        if let Some(beta) = beta_header(capabilities, thinking) {
            headers.push(("anthropic-beta", beta.to_string()));
        }
        return headers;
    }
    let mut headers = vec![
        ("x-api-key", api_key.to_string()),
        ("anthropic-version", ANTHROPIC_VERSION.to_string()),
    ];
    if let Some(beta) = beta_header(capabilities, thinking) {
        headers.push(("anthropic-beta", beta.to_string()));
    }
    headers
}

// prompt-caching breakpoint stamping

/// Hard cap on breakpoints across tools + system + messages combined
/// (a 5th returns HTTP 400).
const MAX_BREAKPOINTS: usize = 4;

/// Stamp cache breakpoints across the ordered request regions
/// (`tools → system → messages`) within the 4-breakpoint budget: last tool,
/// last system block, and the two newest messages. No-op for absent regions.
fn stamp_caching_breakpoints(body: &mut Value, system_text: &str, control: &Value) {
    let mut breakpoints = 0usize;

    if !body["tools"].is_null()
        && breakpoints < MAX_BREAKPOINTS
        && stamp_last_array_element(&mut body["tools"], control)
    {
        breakpoints += 1;
    }

    if !system_text.is_empty() {
        // `system` must be a block *array* to carry a breakpoint; a bare string
        // cannot. Emit one text block and stamp it within budget.
        let mut sys_block = json!({"type":"text","text": system_text});
        if breakpoints < MAX_BREAKPOINTS && stamp_cache_control(&mut sys_block, control) {
            breakpoints += 1;
        }
        body["system"] = json!([sys_block]);
    }

    // Mark up to two trailing messages (newest, then second-newest) within the
    // remaining breakpoint budget.
    stamp_message_history_breakpoints(
        &mut body["messages"],
        MAX_BREAKPOINTS.saturating_sub(breakpoints),
        control,
    );
}

fn cache_control(retention: Option<nuo_contracts::CacheRetention>) -> Value {
    let mut control = json!({"type": "ephemeral"});
    if let Some(retention) = retention {
        control["ttl"] = json!(match retention {
            nuo_contracts::CacheRetention::FiveMinutes => "5m",
            nuo_contracts::CacheRetention::OneHour => "1h",
            nuo_contracts::CacheRetention::InMemory
            | nuo_contracts::CacheRetention::ThirtyMinutes
            | nuo_contracts::CacheRetention::TwentyFourHours => {
                unreachable!("cache plan was resolved against Anthropic capabilities")
            }
        });
    }
    control
}

/// The standard 5-minute breakpoint marker, the cheapest cache tier. Returns
/// `false` if `block` is not a JSON object, so callers can short-circuit budget
/// accounting on non-stampable shapes.
fn stamp_cache_control(block: &mut Value, control: &Value) -> bool {
    if let Some(obj) = block.as_object_mut() {
        obj.insert("cache_control".to_string(), control.clone());
        true
    } else {
        false
    }
}

/// Stamp a breakpoint on the last element of a JSON array (e.g. the last tool
/// definition, or the last content block of a message). Returns `false` if the
/// value is not a non-empty array.
fn stamp_last_array_element(arr: &mut Value, control: &Value) -> bool {
    if let Some(items) = arr.as_array_mut()
        && let Some(last) = items.last_mut()
    {
        stamp_cache_control(last, control)
    } else {
        false
    }
}

/// Stamp breakpoints on the trailing messages' last content blocks, newest then
/// second-newest, capped at `budget` (and 2 — beyond two there is no value).
fn stamp_message_history_breakpoints(messages: &mut Value, budget: usize, control: &Value) {
    let Some(msgs) = messages.as_array_mut() else {
        return;
    };
    let mut stamped = 0usize;
    let cap = budget.min(2);
    for msg in msgs.iter_mut().rev() {
        if stamped >= cap {
            break;
        }
        if stamp_last_array_element(&mut msg["content"], control) {
            stamped += 1;
        }
    }
}

// extended-thinking / effort stamping

/// The `budget_tokens` value for MANUAL extended thinking on models without
/// adaptive thinking (Haiku 4.5). The Messages API requires `budget_tokens <
/// max_tokens`, so we reserve roughly half the output budget for reasoning,
/// clamped to a sane floor/ceiling.
fn manual_thinking_budget(max_tokens: u32) -> u32 {
    (max_tokens / 2).clamp(1024, 32_000)
}

/// Resolve the thinking/effort config against the model's registered capability
/// and stamp `thinking` / `output_config` onto the body accordingly.
///
/// See [`super::thinking::ThinkingConfig`] for the orthogonality of the two
/// knobs and the opt-in default.
fn stamp_thinking(
    body: &mut Value,
    capabilities: &nuo_contracts::ModelCapabilities,
    max_tokens: u32,
    thinking: ThinkingConfig,
) {
    // The model registry (`ReasoningSupport` + `effort_levels`) is the single
    // source of truth for *how* a model reasons on the wire.
    let resolved = thinking.resolve_for(&capabilities.effort_levels);
    let want = resolved.mode.is_on();
    match capabilities.thinking {
        ReasoningSupport::AnthropicManual if want => {
            body["thinking"] = json!({
                "type": "enabled",
                "budget_tokens": manual_thinking_budget(max_tokens),
            });
        }
        ReasoningSupport::AnthropicAdaptiveAlwaysOn => {
            body["thinking"] = json!({ "type": "adaptive", "display": "summarized" });
        }
        ReasoningSupport::AnthropicAdaptiveOnByDefault => {
            // Sonnet 5: omitting the field RUNS thinking. Honor opt-OUT with
            // `{type:"disabled"}`; opt-in emits adaptive.
            if want {
                body["thinking"] = json!({ "type": "adaptive", "display": "summarized" });
            } else {
                body["thinking"] = json!({ "type": "disabled" });
            }
        }
        // Opted in, plus the conservative default for any other Anthropic-wire
        // model (third-party relays, user-defined ids) the user turned on.
        _ if want => {
            body["thinking"] = json!({ "type": "adaptive", "display": "summarized" });
        }
        _ => {}
    }
    // Effort rides in the top-level `output_config`, never inside `thinking`.
    // Emit ONLY for models that advertise an effort vocabulary.
    if !capabilities.effort_levels.is_empty()
        && let Some(effort) = resolved.effort
    {
        body["output_config"] = json!({ "effort": effort.as_str() });
    }
}

// message conversion

/// Convert a harness [`Message`] to an Anthropic message object.
///
/// Anthropic roles are `user` and `assistant` only; `tool` results become
/// `user` messages carrying `tool_result` blocks. Content is always a block
/// array; plain text becomes `[{type:"text", text}]`, and images become
/// `image` blocks.
pub fn message_obj(m: Message) -> Value {
    match m.role {
        Role::Tool => {
            json!({
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": m.tool_call_id.unwrap_or_default(),
                    "content": m.content,
                }],
            })
        }
        Role::Assistant => {
            let mut blocks: Vec<Value> = Vec::new();
            // Replay the prior turn's thinking FIRST (Anthropic requires a
            // thinking block to precede text/tool_use). Echo it back only when
            // we have reasoning text AND its server-assigned signature.
            if let Some(reasoning) = m.reasoning_content.as_ref()
                && !reasoning.is_empty()
                && let Some(sig) = thinking_signature_of(&m)
            {
                blocks.push(json!({
                    "type": "thinking",
                    "thinking": reasoning,
                    "signature": sig,
                }));
            }
            if !m.content.is_empty() {
                blocks.push(json!({"type":"text","text": m.content}));
            }
            if let Some(calls) = m.tool_calls.as_ref() {
                for call in calls {
                    blocks.push(json!({
                        "type": "tool_use",
                        "id": call.id,
                        "name": call.name,
                        "input": parse_arguments(&call.arguments),
                    }));
                }
            }
            json!({"role": "assistant", "content": blocks})
        }
        _ => {
            // user / system-fallback: content as typed blocks (text + images).
            let blocks = content_blocks(&m);
            json!({"role": "user", "content": blocks})
        }
    }
}

/// Read the persisted thinking-block signature out of a message's
/// `provider_meta` sidecar. Provider-internal: only [`message_obj`] calls this.
fn thinking_signature_of(m: &Message) -> Option<String> {
    m.provider_meta
        .as_ref()?
        .get("thinking_signature")?
        .as_str()
        .map(str::to_string)
}

/// Build the Anthropic content block array for a user/system message: a text
/// block for the prose, plus an `image` block per attachment.
fn content_blocks(m: &Message) -> Vec<Value> {
    let mut blocks = Vec::new();
    if !m.content.is_empty() {
        blocks.push(json!({"type":"text","text": m.content}));
    }
    if let Some(images) = m.images.as_ref() {
        for image in images {
            blocks.push(json!({
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": image.mime,
                    "data": image.data,
                },
            }));
        }
    }
    if blocks.is_empty() {
        blocks.push(json!({"type":"text","text":""}));
    }
    blocks
}

/// Parse a tool-call `arguments` string into a JSON value for the `input`
/// field. The harness stores arguments as a JSON string (possibly empty);
/// Anthropic requires a JSON object.
fn parse_arguments(arguments: &str) -> Value {
    if arguments.is_empty() {
        return json!({});
    }
    serde_json::from_str::<Value>(arguments).unwrap_or(json!({}))
}
