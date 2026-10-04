//! OpenAI-compatible chat completions — request construction.
//!
//! Pure functions turning the harness's `Vec<Message>` into the OpenAI
//! chat-completions request shape: a `messages` array of `{role, content,
//! tool_calls?, tool_call_id?}`, plus optional `tools`/`stream_options`. No
//! `reqwest`, no `async`, no I/O.
//!
//! OpenAI wire shape:
//! - URL: the chat-completions endpoint as configured (default
//!   `https://api.openai.com/v1/chat/completions`).
//! - Auth: `Authorization: Bearer <key>` — but **only when a key is set**. A
//!   keyless relay (empty key) sends no auth header at all, because some
//!   relays reject a malformed bearer token even when they'd otherwise ignore
//!   the key.
//! - Body: `{model, messages, stream, reasoning_effort?, tools?, stream_options?}`.
//!
//! Two correctness transformations live here:
//! 1. Images are stripped from non-vision models (the API rejects `image_url`
//!    on them; text content is preserved).
//! 2. Orphan tool results and unanswered tool calls are reconciled so the
//!    request is always wire-valid: every `tool` result references a known
//!    preceding `tool_call`, and every assistant `tool_calls` has its results.

use nuo_model_codec::{Effort, Message, OpenAiChatDialect, Role};
use serde_json::{Value, json};

/// The headers this wire format requires on every request, beyond the
/// always-present `User-Agent`. For OpenAI that is just the bearer auth header
/// — omitted when the key is empty (keyless relay). In Copilot mode the
/// client-identity headers (`Copilot-Integration-Id`, `Editor-Version`,
/// `Editor-Plugin-Version`) plus the per-turn headers (`x-initiator`,
/// `Openai-Intent`, `X-GitHub-Api-Version`) are appended so a
/// chat-completions request against `api.githubcopilot.com` is recognized as
/// a real Copilot Chat client (and so resolves the account's actual plan
/// entitlements) and carries the same metadata a Responses request would
/// (mirrors `responses::request::headers`).
pub fn headers(api_key: &str, dialect: OpenAiChatDialect) -> Vec<(&'static str, String)> {
    let mut h = Vec::new();
    if !api_key.trim().is_empty() {
        h.push(("Authorization", format!("Bearer {api_key}")));
    }
    if dialect == OpenAiChatDialect::Copilot {
        for (name, value) in nuo_provider_transport::COPILOT_CLIENT_HEADERS {
            h.push((*name, value.to_string()));
        }
        h.push(("x-initiator", "user".to_string()));
        h.push(("Openai-Intent", "conversation-edits".to_string()));
        h.push(("X-GitHub-Api-Version", "2026-06-01".to_string()));
    } else if dialect == OpenAiChatDialect::OpenRouter {
        // Optional OpenRouter app attribution. The User-Agent is already
        // supplied by Endpoint; the title makes dashboard traffic readable
        // without inventing a project URL for HTTP-Referer.
        h.push(("X-OpenRouter-Title", "Muta".to_string()));
    }
    // Qoder has no branch here on purpose. Its wire is built entirely by
    // `build_qoder_request` (the executor routes the Qoder dialect there and
    // never through this builder), so declaring its identity headers here
    // would be a second declaration site — the drift ADR-0265 forbids. The
    // surface (`OpenAiChatDialect::surface()`) is the one source.
    h
}

/// Inputs to [`body`]: the model id, whether this is a streaming request, and
/// the prepared tool schemas (OpenAI function-spec shape, optional).
pub struct BodyInput<'a> {
    pub model: &'a str,
    pub stream: bool,
    /// Structured instructions to project as the leading system message.
    pub instructions: Option<&'a nuo_model_codec::InstructionBundle>,
    /// OpenAI-shaped tool specs (`{type:"function", function:{...}}`), if any.
    pub tool_specs: Option<&'a [nuo_model_codec::ToolSpec]>,
    /// Optional OpenAI reasoning-effort override. `None` omits the field and
    /// keeps the model/provider default.
    pub reasoning_effort: Option<Effort>,
    /// Provider-specific behavior layered on the shared Chat Completions wire.
    pub dialect: OpenAiChatDialect,
    /// Cache controls resolved against this exact provider route.
    pub cache_plan: &'a nuo_model_codec::ResolvedCachePolicy,
}

/// Build the chat-completions request body.
///
/// Strips images for non-vision models and reconciles tool calls/results so
/// the request is always wire-valid (see the module docs).
///
/// Projection contract (ADR-0048): the `messages` passed here are a clone of
/// the round's working scratch, itself cloned from the session's authoritative
/// `model_window` at turn start. This builder reads only wire-relevant fields
/// via [`message_obj`] — `role`, `content`, `tool_calls`, `tool_call_id`,
/// `images` — so durable sidecars (`children`, `subagent_meta`, `origin`) never
/// reach the wire. Serialization is therefore a pure projection of the
/// session: no field on the wire exists that the session did not produce.
pub fn body(messages: Vec<Message>, input: BodyInput<'_>) -> Value {
    let capabilities = nuo_model_codec::ModelCapabilities::for_channel(input.model, None);
    body_with_capabilities(messages, input, &capabilities)
}

/// Build a request with a provider-channel capability view. The provider calls
/// this for trusted remote metadata; [`body`] remains the static-baseline entry
/// point for standalone callers and tests.
pub fn body_with_capabilities(
    messages: Vec<Message>,
    input: BodyInput<'_>,
    capabilities: &nuo_model_codec::ModelCapabilities,
) -> Value {
    let BodyInput {
        model: model_id,
        stream,
        instructions,
        tool_specs,
        reasoning_effort,
        dialect,
        cache_plan,
    } = input;

    let mut messages = messages;
    if let Some(instructions) = instructions
        && !instructions.is_empty()
    {
        let text = instructions.render_combined();
        if !text.is_empty() {
            messages.insert(0, Message::new(Role::System, text));
        }
    }

    // A route that declared no image input has its attachments dropped (text
    // survives; the model simply does not see the pixels, and OpenAI rejects
    // `image_url` outright). The policy lives in `crate::vision` so all four
    // transports project identically, and an *undeclared* route keeps its
    // images instead of losing them silently (ADR-0230).
    let (messages, _dropped_images) =
        nuo_provider_transport::vision::project_images_for_route(model_id, messages, capabilities);

    // OpenAI rejects any `tool` message whose `tool_call_id` does not match
    // a `tool_call` on a preceding assistant message. Drop orphan tool
    // results (e.g. from text-fallback calls or older saved sessions) so the
    // request can never fail with "tool_call_id is not found".
    let mut known_ids = std::collections::HashSet::new();
    let mut messages: Vec<Message> = messages
        .into_iter()
        .filter(|message| {
            if !valid_provider_message(message) {
                return false;
            }
            match message.role {
                Role::Assistant => {
                    if let Some(calls) = message.tool_calls.as_ref() {
                        for call in calls {
                            known_ids.insert(call.id.clone());
                        }
                    }
                    true
                }
                Role::Tool => message
                    .tool_call_id
                    .as_ref()
                    .is_some_and(|id| !id.is_empty() && known_ids.contains(id)),
                _ => true,
            }
        })
        .collect();

    // Every assistant `tool_calls` must be followed by a corresponding
    // `tool` result message. Collect the ids that *did* get a result, then
    // strip unanswered calls from every assistant message so the request is
    // always valid — whether the turn was interrupted, the session was
    // mid-tool when saved, or older turns lost their results.
    let answered: std::collections::HashSet<String> = messages
        .iter()
        .filter_map(|m| {
            if m.role == Role::Tool {
                m.tool_call_id.clone()
            } else {
                None
            }
        })
        .collect();
    messages.retain_mut(|m| {
        if m.role != Role::Assistant {
            return true;
        }
        if let Some(calls) = m.tool_calls.as_mut() {
            calls.retain(|c| answered.contains(&c.id));
            if calls.is_empty() {
                m.tool_calls = None;
            }
        }
        // Keep the message only if it still carries content or at least one
        // surviving tool call; a completely empty assistant message is illegal
        // on the wire.
        !m.content.is_empty() || m.tool_calls.as_ref().is_some_and(|calls| !calls.is_empty())
    });

    let tool_specs = tool_specs.map(|specs| {
        json!(
            specs
                .iter()
                .map(|spec| {
                    json!({
                        "type": "function",
                        "function": {
                            "name": spec.name,
                            "description": spec.description,
                            "parameters": spec.parameters,
                        }
                    })
                })
                .collect::<Vec<_>>()
        )
    });

    let mut body = json!({
        "model": model_id,
        "messages": messages
            .into_iter()
            .map(|message| message_obj(message, dialect))
            .collect::<Vec<_>>(),
        "stream": stream,
    });
    if stream {
        // Ask the endpoint to include a terminal `usage` chunk so the
        // streaming path can book real token counts. Standard OpenAI & most
        // OpenAI-compatible relays honour this; relays that don't recognise it
        // ignore the unknown field harmlessly.
        body["stream_options"] = json!({ "include_usage": true });
    }
    if let Some(effort) = reasoning_effort
        && !capabilities.effort_levels.is_empty()
    {
        let clamped = effort.clamp_to_levels(&capabilities.effort_levels);
        let effort = clamped.as_str();
        if dialect == OpenAiChatDialect::OpenRouter {
            body["reasoning"] = json!({ "effort": effort });
        } else {
            body["reasoning_effort"] = json!(effort);
        }
    }
    if let Some(specs) = tool_specs {
        body["tools"] = specs;
    }
    super::super::cache::apply(&mut body, cache_plan, "messages");
    body
}

/// Discard messages the OpenAI endpoint rejects or misuses: empty assistant
/// turns (no content, no tool calls) and the system role when tool calls are
/// present (Kimi/Qwen interleave system content with tool execution and refuse
/// a leading system message in that case).
fn valid_provider_message(message: &Message) -> bool {
    if message.role == Role::Assistant {
        let empty = message.content.is_empty()
            && message
                .tool_calls
                .as_ref()
                .map(|calls| calls.is_empty())
                .unwrap_or(true);
        return !empty;
    }
    if message.role == Role::System {
        return message
            .tool_calls
            .as_ref()
            .is_none_or(|calls| calls.is_empty());
    }
    true
}

/// Convert a harness [`Message`] to an OpenAI message object (role + content,
/// with optional `tool_calls` / `tool_call_id`).
pub fn message_obj(m: Message, dialect: OpenAiChatDialect) -> Value {
    let openrouter_reasoning_details = m
        .provider_meta
        .as_ref()
        .and_then(|meta| meta.get(super::response::OPENROUTER_REASONING_DETAILS_META_KEY))
        .cloned();
    let reasoning_content = m.reasoning_content.clone();
    let mut map = json!({
        "role": match m.role {
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::System => "system",
            Role::Tool => "tool",
        },
        "content": content(&m),
    });
    if let Some(tool_calls) = m.tool_calls {
        map["tool_calls"] = json!(
            tool_calls
                .into_iter()
                .map(|tc| {
                    json!({
                        "id": tc.id,
                        "type": "function",
                        "function": {"name": tc.name, "arguments": tc.arguments}
                    })
                })
                .collect::<Vec<_>>()
        );
    }
    if let Some(tool_call_id) = m.tool_call_id {
        map["tool_call_id"] = json!(tool_call_id);
    }
    if dialect == OpenAiChatDialect::OpenRouter && m.role == Role::Assistant {
        if let Some(details) = openrouter_reasoning_details {
            map["reasoning_details"] = details;
        } else if let Some(reasoning) = reasoning_content.filter(|value| !value.is_empty()) {
            map["reasoning"] = json!(reasoning);
        }
    }
    map
}

/// Render a message's `content` field: an array of typed parts when images are
/// present (text + `image_url` data URLs), otherwise a plain string.
pub fn content(m: &Message) -> Value {
    match &m.images {
        Some(images) if !images.is_empty() => {
            let mut parts = Vec::new();
            if !m.content.is_empty() {
                parts.push(json!({ "type": "text", "text": m.content }));
            }
            for image in images {
                parts.push(json!({
                    "type": "image_url",
                    "image_url": {
                        "url": format!("data:{};base64,{}", image.mime, image.data)
                    }
                }));
            }
            Value::Array(parts)
        }
        _ => Value::String(m.content.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nuo_model_codec::ToolCall;

    static DEFAULT_CACHE_PLAN: nuo_model_codec::ResolvedCachePolicy =
        nuo_model_codec::ResolvedCachePolicy::Unsupported;

    fn test_body_input<'a>(
        model: &'a str,
        stream: bool,
        tool_specs: Option<&'a [nuo_model_codec::ToolSpec]>,
        reasoning_effort: Option<Effort>,
        cache_plan: &'a nuo_model_codec::ResolvedCachePolicy,
    ) -> BodyInput<'a> {
        BodyInput {
            model,
            stream,
            instructions: None,
            tool_specs,
            reasoning_effort,
            dialect: OpenAiChatDialect::Standard,
            cache_plan,
        }
    }

    /// Route capabilities with only the vision declaration varied, for the
    /// image-projection tests below.
    fn caps_with_vision(vision: Option<bool>) -> nuo_model_codec::ModelCapabilities {
        nuo_model_codec::ModelCapabilities {
            family: "test".into(),
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
    fn declared_text_only_route_strips_image_parts() {
        let body = body_with_capabilities(
            vec![image_message("look")],
            test_body_input("test-model", false, None, None, &DEFAULT_CACHE_PLAN),
            &caps_with_vision(Some(false)),
        );

        // The prose survives as a plain string; the `image_url` part is gone, so
        // the API cannot reject the request for it.
        assert_eq!(body["messages"][0]["content"], "look");
    }

    #[test]
    fn undeclared_route_keeps_image_parts() {
        // ADR-0230: an endpoint that advertises no vision field must not have
        // its images silently stripped — the request goes out as-is and a
        // provider that cannot take it says so.
        let body = body_with_capabilities(
            vec![image_message("look")],
            test_body_input("test-model", false, None, None, &DEFAULT_CACHE_PLAN),
            &caps_with_vision(None),
        );

        assert_eq!(body["messages"][0]["content"][0]["type"], "text");
        assert_eq!(body["messages"][0]["content"][1]["type"], "image_url");
        assert_eq!(
            body["messages"][0]["content"][1]["image_url"]["url"],
            "data:image/png;base64,aGk="
        );
    }

    #[test]
    fn request_filters_empty_assistant_history() {
        let body = super::body(
            vec![
                Message::new(Role::User, "hello"),
                Message::new(Role::Assistant, ""),
                Message::new(Role::User, "again"),
            ],
            test_body_input("test-model", true, None, None, &DEFAULT_CACHE_PLAN),
        );

        assert_eq!(body["messages"].as_array().unwrap().len(), 2);
        assert_eq!(body["messages"][1]["content"], "again");
    }

    #[test]
    fn request_includes_reasoning_effort_when_configured() {
        let body = super::body(
            vec![Message::new(Role::User, "think")],
            test_body_input(
                "gpt-5.5",
                false,
                None,
                Some(Effort::Xhigh),
                &DEFAULT_CACHE_PLAN,
            ),
        );

        assert_eq!(body["reasoning_effort"], "xhigh");
    }

    #[test]
    fn openrouter_uses_unified_reasoning_and_replays_details() {
        let mut assistant = Message::new(Role::Assistant, "");
        assistant.reasoning_content = Some("checking".into());
        assistant.tool_calls = Some(vec![ToolCall {
            id: "call_1".into(),
            name: "read".into(),
            arguments: "{}".into(),
        }]);
        assistant.provider_meta = Some({
            let mut meta = serde_json::Map::new();
            meta.insert(
                super::super::response::OPENROUTER_REASONING_DETAILS_META_KEY.into(),
                serde_json::json!([{
                    "type": "reasoning.text",
                    "text": "checking",
                    "signature": "sig",
                    "index": 0
                }]),
            );
            meta
        });
        let tool_result = Message {
            role: Role::Tool,
            content: "ok".into(),
            tool_call_id: Some("call_1".into()),
            ..Message::new(Role::Tool, "")
        };
        let capabilities = nuo_model_codec::ModelCapabilities {
            family: "nex".into(),
            context_window: 262_144,
            max_output_tokens: Some(235_929),
            thinking: nuo_model_codec::ReasoningSupport::ReasoningContent,
            tool_call: true,
            vision: Some(true),
            effort_levels: vec![
                Effort::None.into(),
                Effort::Medium.into(),
                Effort::High.into(),
            ],
        };
        let body = body_with_capabilities(
            vec![Message::new(Role::User, "inspect"), assistant, tool_result],
            BodyInput {
                model: "nex-agi/nex-n2.5-pro:free",
                stream: true,
                instructions: None,
                tool_specs: None,
                reasoning_effort: Some(Effort::High),
                dialect: OpenAiChatDialect::OpenRouter,
                cache_plan: &DEFAULT_CACHE_PLAN,
            },
            &capabilities,
        );

        assert_eq!(body["reasoning"]["effort"], "high");
        assert!(body.get("reasoning_effort").is_none());
        assert_eq!(
            body["messages"][1]["reasoning_details"][0]["signature"],
            "sig"
        );
    }

    #[test]
    fn openrouter_headers_include_app_attribution() {
        let headers = headers("sk-or-test", OpenAiChatDialect::OpenRouter);
        assert!(
            headers
                .iter()
                .any(|(name, value)| { *name == "Authorization" && value == "Bearer sk-or-test" })
        );
        assert!(
            headers
                .iter()
                .any(|(name, value)| { *name == "X-OpenRouter-Title" && value == "Muta" })
        );
    }

    #[test]
    fn request_injects_prompt_cache_key_when_present() {
        let cache_plan = nuo_model_codec::ResolvedCachePolicy::Enabled {
            mode: nuo_model_codec::PromptCacheMode::Implicit,
            retention: None,
            routing_key: Some("session-42".into()),
            max_breakpoints: None,
        };
        let body = super::body(
            vec![Message::new(Role::User, "hi")],
            test_body_input("kimi-k2.7-code", false, None, None, &cache_plan),
        );
        assert_eq!(body["prompt_cache_key"], "session-42");
    }

    #[test]
    fn request_omits_prompt_cache_key_when_absent() {
        let body = super::body(
            vec![Message::new(Role::User, "hi")],
            test_body_input("gpt-5.5", false, None, None, &DEFAULT_CACHE_PLAN),
        );
        assert!(body.get("prompt_cache_key").is_none());
    }

    #[test]
    fn request_drops_orphan_tool_results() {
        let matched = ToolCall {
            id: "call_matched".to_string(),
            name: "read_text".to_string(),
            arguments: "{}".to_string(),
        };
        let assistant_with_call = Message {
            role: Role::Assistant,
            content: String::new(),
            content_blob: None,
            display_content: None,
            reasoning_content: None,
            provider_meta: None,
            tool_calls: Some(vec![matched.clone()]),
            tool_call_id: None,
            images: None,
            provider: None,
            model: None,
            effort: None,
            hidden: false,
            children: None,
            subagent_meta: None,
            origin: None,
            timestamp: None,
            sent_at_ms: None,
            cache_frozen: false,
        };
        let good_result = Message {
            role: Role::Tool,
            content: "ok".to_string(),
            tool_call_id: Some("call_matched".to_string()),
            ..Message::new(Role::Tool, "")
        };
        let orphan_result = Message {
            tool_call_id: Some("call_orphan".to_string()),
            ..Message::new(Role::Tool, "orphan")
        };
        let empty_id_result = Message {
            tool_call_id: Some(String::new()),
            ..Message::new(Role::Tool, "empty id")
        };

        let body = super::body(
            vec![
                Message::new(Role::User, "hi"),
                assistant_with_call,
                good_result,
                orphan_result,
                empty_id_result,
            ],
            test_body_input("test-model", false, None, None, &DEFAULT_CACHE_PLAN),
        );

        let messages = body["messages"].as_array().unwrap();
        // user, assistant(tool_calls), and only the matched tool result survive.
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[2]["role"], "tool");
        assert_eq!(messages[2]["tool_call_id"], "call_matched");
    }

    #[test]
    fn strips_unanswered_tool_calls() {
        let call_a = ToolCall {
            id: "a".into(),
            name: "bash".into(),
            arguments: "{}".into(),
        };
        let call_b = ToolCall {
            id: "b".into(),
            name: "read_text".into(),
            arguments: "{}".into(),
        };
        let call_c = ToolCall {
            id: "c".into(),
            name: "grep".into(),
            arguments: "{}".into(),
        };

        // Case 1: trailing unanswered assistant (the original bug)
        let body = super::body(
            vec![
                Message::new(Role::User, "go"),
                Message {
                    tool_calls: Some(vec![call_a.clone()]),
                    ..Message::new(Role::Assistant, "")
                },
            ],
            test_body_input("test-model", false, None, None, &DEFAULT_CACHE_PLAN),
        );
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(
            msgs.len(),
            1,
            "trailing unanswered assistant must be dropped"
        );

        // Case 2: assistant with content but no tool result
        let body = super::body(
            vec![
                Message::new(Role::User, "go"),
                Message {
                    tool_calls: Some(vec![call_a.clone()]),
                    ..Message::new(Role::Assistant, "let me think")
                },
            ],
            test_body_input("test-model", false, None, None, &DEFAULT_CACHE_PLAN),
        );
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 2);
        assert!(
            msgs[1]["content"]
                .as_str()
                .unwrap_or("")
                .contains("let me think")
        );
        assert!(
            msgs[1].get("tool_calls").is_none(),
            "unanswered calls must be stripped"
        );

        // Case 3: partially answered call set
        let body = super::body(
            vec![
                Message::new(Role::User, "go"),
                Message {
                    tool_calls: Some(vec![call_a.clone(), call_b.clone()]),
                    ..Message::new(Role::Assistant, "")
                },
                Message {
                    tool_call_id: Some("a".into()),
                    ..Message::new(Role::Tool, "result a")
                },
            ],
            test_body_input("test-model", false, None, None, &DEFAULT_CACHE_PLAN),
        );
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 3);
        let calls = msgs[1]["tool_calls"].as_array().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["id"], "a");

        // Case 4: multiple consecutive unanswered assistants
        let body = super::body(
            vec![
                Message::new(Role::User, "go"),
                Message {
                    tool_calls: Some(vec![call_a.clone()]),
                    ..Message::new(Role::Assistant, "first")
                },
                Message {
                    tool_calls: Some(vec![call_b.clone()]),
                    ..Message::new(Role::Assistant, "second")
                },
            ],
            test_body_input("test-model", false, None, None, &DEFAULT_CACHE_PLAN),
        );
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 3, "both assistants kept for their content");
        assert!(msgs[1].get("tool_calls").is_none());
        assert!(msgs[2].get("tool_calls").is_none());

        // Case 5: fully healthy conversation (no stripping)
        let body = super::body(
            vec![
                Message::new(Role::User, "go"),
                Message {
                    tool_calls: Some(vec![call_a.clone(), call_c.clone()]),
                    ..Message::new(Role::Assistant, "")
                },
                Message {
                    tool_call_id: Some("a".into()),
                    ..Message::new(Role::Tool, "ok")
                },
                Message {
                    tool_call_id: Some("c".into()),
                    ..Message::new(Role::Tool, "ok")
                },
                Message::new(Role::Assistant, "all done"),
            ],
            test_body_input("test-model", false, None, None, &DEFAULT_CACHE_PLAN),
        );
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 5, "healthy conversation untouched");
        assert_eq!(msgs[1]["tool_calls"].as_array().unwrap().len(), 2);
        assert_eq!(msgs[4]["content"], "all done");
    }
}
