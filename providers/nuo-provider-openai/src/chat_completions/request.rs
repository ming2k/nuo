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
//! Three correctness transformations live here:
//! 1. Images are stripped from non-vision models (the API rejects `image_url`
//!    on them; text content is preserved).
//! 2. Orphan tool results and unanswered tool calls are reconciled so the
//!    request is always wire-valid: every `tool` result references a known
//!    preceding `tool_call`, and every assistant `tool_calls` has its results.
//! 3. Reasoning replay is projected per dialect: an assistant turn's CoT
//!    reaches the wire only where that surface defines a replayable field
//!    (ADR-0083).
use nuo_provider::{Effort, OpenAiChatDialect};
use nuo_tool::{Message, Role};
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
    headers_with_lane(api_key, dialect, CodeBuddyAuthLane::Bearer)
}

/// Which header shape a CodeBuddy credential should ride.
///
/// CodeBuddy recognizes two credential lanes at the wire level: the logged-in
/// OIDC bearer (`Authorization: Bearer <jwt>`) and a pasted platform key
/// (`X-API-Key: <key>`). The upstream server accepts both; the CLI's `iam.md`
/// assigns them different roles (subscription login vs API-key mode). The
/// default [`CodeBuddyAuthLane::Bearer`] covers every other dialect down to
/// the previous universal shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CodeBuddyAuthLane {
    /// OIDC bearer from the subscription lane's login. The header is
    /// `Authorization: Bearer <token>`.
    #[default]
    Bearer,
    /// Pasted platform key from the BYOK lane. The header is `X-API-Key`.
    ApiKey,
}

pub const COPILOT_CLIENT_HEADERS: &[(&str, &str)] = &[
    ("Copilot-Integration-Id", "vscode-chat"),
    ("Editor-Version", "vscode/1.107.0"),
    ("Editor-Plugin-Version", "copilot-chat/0.35.0"),
];

/// Credential lane decision overload of [`headers`].
pub fn headers_with_lane(
    api_key: &str,
    dialect: OpenAiChatDialect,
    lane: CodeBuddyAuthLane,
) -> Vec<(&'static str, String)> {
    let mut h = Vec::new();
    if !api_key.trim().is_empty() {
        // For CodeBuddy the lane decides the header shape; every other
        // dialect keeps the universal `Authorization: Bearer`.
        let codebuddy = dialect == OpenAiChatDialect::CodeBuddy;
        let use_bearer = !codebuddy || lane == CodeBuddyAuthLane::Bearer;
        if use_bearer {
            h.push(("Authorization", format!("Bearer {api_key}")));
        } else {
            h.push(("X-API-Key", api_key.to_string()));
        }
    }
    if dialect == OpenAiChatDialect::Copilot {
        for (name, value) in COPILOT_CLIENT_HEADERS {
            h.push((*name, value.to_string()));
        }
        h.push(("x-initiator", "user".to_string()));
        h.push(("Openai-Intent", "conversation-edits".to_string()));
        h.push(("X-GitHub-Api-Version", "2026-06-01".to_string()));
    } else if dialect == OpenAiChatDialect::OpenRouter {
        // Optional OpenRouter app attribution. The User-Agent is already
        // supplied by Endpoint; the title makes dashboard traffic readable
        // without inventing a project URL for HTTP-Referer.
        h.push(("X-OpenRouter-Title", "Nuo".to_string()));
    } else if dialect == OpenAiChatDialect::CodeBuddy {
        const CODEBUDDY_CLIENT_HEADERS: &[(&str, &str)] = &[
            ("X-Product", "SaaS"),
            ("X-IDE-Type", "CLI"),
            ("X-IDE-Name", "CodeBuddy"),
            ("X-IDE-Version", "2.172.1"),
            ("X-Agent-Intent", "craft"),
            ("X-Agent-Runtime", "local-agent"),
        ];
        for (name, value) in CODEBUDDY_CLIENT_HEADERS {
            h.push((*name, value.to_string()));
        }
    }

    // Qoder has no branch here on purpose. Its wire is built entirely by
    // `build_qoder_request` (the executor routes the Qoder dialect there and
    // never through this builder), so declaring its identity headers here
    // would be a second declaration site — the drift forbids. The
    // surface (`OpenAiChatDialect::surface()`) is the one source.
    h
}

/// Inputs to [`body`]: the model id, whether this is a streaming request, and
/// the prepared tool schemas (OpenAI function-spec shape, optional).
pub struct BodyInput<'a> {
    pub model: &'a str,
    pub stream: bool,
    /// Structured instructions to project as the leading system message.
    pub instructions: Option<&'a nuo_provider::InstructionText>,
    /// OpenAI-shaped tool specs (`{type:"function", function:{...}}`), if any.
    pub tool_specs: Option<&'a [nuo_provider::capability::ToolSpec]>,
    /// Optional OpenAI reasoning-effort override. `None` omits the field and
    /// keeps the model/provider default.
    pub reasoning_effort: Option<Effort>,
    /// Provider-specific behavior layered on the shared Chat Completions wire.
    pub dialect: OpenAiChatDialect,
    /// Cache controls resolved against this exact provider route.
    pub cache_plan: &'a nuo_provider::ResolvedCachePolicy,
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
pub fn body(
    conversation: &[Message],
    temporary_context: &[Message],
    input: BodyInput<'_>,
) -> Value {
    let capabilities = nuo_provider::ModelCapabilities::for_channel(input.model, None);
    body_with_capabilities(conversation, temporary_context, input, &capabilities)
}

/// Build a request with a provider-channel capability view. The provider calls
/// this for trusted remote metadata; [`body`] remains the static-baseline entry
/// point for standalone callers and tests.
///
/// The window is **borrowed**. Provider-specific cleanup (orphan tool results,
/// unanswered calls, DeepSeek reasoning hygiene) materializes a copy only of
/// the messages it actually changes; every other message is serialized from the
/// caller's window in place.
pub fn body_with_capabilities(
    conversation: &[Message],
    temporary_context: &[Message],
    input: BodyInput<'_>,
    capabilities: &nuo_provider::ModelCapabilities,
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

    // A route that declared no image input has its attachments dropped (text
    // survives; the model simply does not see the pixels, and OpenAI rejects
    // `image_url` outright). The policy lives in `crate::vision` so all four
    // transports project identically, and an *undeclared* route keeps its
    // images instead of losing them silently.
    let (conversation, _dropped_images) =
        nuo_provider::project_images_for_route(model_id, conversation, capabilities);

    let mut system_message: Option<Message> = None;
    if let Some(instructions) = instructions
        && !instructions.is_empty()
    {
        // Emit stable static instructions followed by ephemeral nudges (ADR-0052, [INV-INST-03])
        let static_text = instructions.render_fixed().to_owned();
        let ephemeral_text = instructions.render_transient().to_owned();
        let text = match (static_text.is_empty(), ephemeral_text.is_empty()) {
            (false, false) => format!("{static_text}\n{ephemeral_text}"),
            (false, true) => static_text,
            (true, false) => ephemeral_text,
            (true, true) => String::new(),
        };
        if !text.is_empty() {
            system_message = Some(Message::new(Role::System, text));
        }
    }

    // OpenAI rejects any `tool` message whose `tool_call_id` does not match
    // a `tool_call` on a preceding assistant message. Drop orphan tool
    // results (e.g. from text-fallback calls or older saved sessions) so the
    // request can never fail with "tool_call_id is not found".
    let mut known_ids = std::collections::HashSet::new();
    let mut refs: Vec<&Message> =
        Vec::with_capacity(conversation.len() + temporary_context.len() + 1);
    if let Some(system) = system_message.as_ref() {
        refs.push(system);
    }
    for message in conversation.iter().chain(temporary_context.iter()) {
        if !valid_provider_message(message) {
            continue;
        }
        match message.role {
            Role::Assistant => {
                if let Some(calls) = message.tool_calls.as_ref() {
                    for call in calls {
                        known_ids.insert(call.id.clone());
                    }
                }
                refs.push(message);
            }
            Role::Tool => {
                if message
                    .tool_call_id
                    .as_ref()
                    .is_some_and(|id| !id.is_empty() && known_ids.contains(id))
                {
                    refs.push(message);
                }
            }
            _ => refs.push(message),
        }
    }

    // Every assistant `tool_calls` must be followed by a corresponding
    // `tool` result message. Collect the ids that *did* get a result, then
    // strip unanswered calls from every assistant message so the request is
    // always valid — whether the turn was interrupted, the session was
    // mid-tool when saved, or older turns lost their results.
    let answered: std::collections::HashSet<&str> = refs
        .iter()
        .filter(|m| m.role == Role::Tool)
        .filter_map(|m| m.tool_call_id.as_deref())
        .collect();
    // Copy only the assistant turns that actually lose a call; everything else
    // stays borrowed. A turn left with no content and no surviving call is
    // dropped (an empty assistant message is illegal on the wire).
    let mut cleaned: Vec<std::borrow::Cow<'_, Message>> = Vec::with_capacity(refs.len());
    for message in refs {
        if message.role != Role::Assistant
            || !message
                .tool_calls
                .as_ref()
                .is_some_and(|calls| calls.iter().any(|c| !answered.contains(c.id.as_str())))
        {
            cleaned.push(std::borrow::Cow::Borrowed(message));
            continue;
        }
        let mut owned = message.clone();
        if let Some(calls) = owned.tool_calls.as_mut() {
            calls.retain(|c| answered.contains(c.id.as_str()));
            if calls.is_empty() {
                owned.tool_calls = None;
            }
        }
        if owned.content.is_empty() && owned.tool_calls.is_none() {
            continue;
        }
        cleaned.push(std::borrow::Cow::Owned(owned));
    }

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

    // DeepSeek CoT hygiene: upstream requires `reasoning_content` on tool-calling assistant
    // messages. However, replaying historical CoT across older rounds saturates the context
    // window and triggers V-shaped attention collapse / repetitive loops. We preserve the full
    // reasoning_content on the active (latest) assistant turn only; older assistant turns with
    // tool_calls have their reasoning pruned to "" (satisfying upstream schema verification
    // without inflating the prompt with historical thinking).
    let last_assistant_idx = cleaned
        .iter()
        .rposition(|message| message.role == Role::Assistant);
    let wire_messages: Vec<Value> = cleaned
        .iter_mut()
        .enumerate()
        .map(|(idx, message)| {
            if dialect.replays_reasoning_content()
                && Some(idx) != last_assistant_idx
                && message.reasoning_content.is_some()
            {
                match message {
                    std::borrow::Cow::Borrowed(borrowed) => {
                        let mut owned = (*borrowed).clone();
                        owned.reasoning_content = None;
                        *message = std::borrow::Cow::Owned(owned);
                    }
                    std::borrow::Cow::Owned(owned) => owned.reasoning_content = None,
                }
            }
            message_obj(message, dialect)
        })
        .collect();

    let mut body = json!({
        "model": model_id,
        "messages": wire_messages,
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
pub fn message_obj(m: &Message, dialect: OpenAiChatDialect) -> Value {
    let openrouter_reasoning_details = m
        .provider_meta
        .as_ref()
        .and_then(|meta| meta.get(super::response::OPENROUTER_REASONING_DETAILS_META_KEY))
        .cloned();
    let reasoning_content = m
        .reasoning_content
        .as_deref()
        .filter(|value| !value.is_empty());
    let mut map = json!({
        "role": match m.role {
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::System => "system",
            Role::Tool => "tool",
        },
        "content": content(m),
    });
    if let Some(tool_calls) = m.tool_calls.as_ref() {
        map["tool_calls"] = json!(
            tool_calls
                .iter()
                .map(|tc| {
                    json!({
                        "id": &tc.id,
                        "type": "function",
                        "function": {"name": &tc.name, "arguments": &tc.arguments}
                    })
                })
                .collect::<Vec<_>>()
        );
    }
    if let Some(tool_call_id) = m.tool_call_id.as_deref() {
        map["tool_call_id"] = json!(tool_call_id);
    }
    if m.role == Role::Assistant {
        if dialect == OpenAiChatDialect::OpenRouter {
            if let Some(details) = openrouter_reasoning_details {
                map["reasoning_details"] = details;
            } else if let Some(reasoning) = &reasoning_content {
                map["reasoning"] = json!(reasoning);
            }
        }
        // DeepSeek concatenates a replayed CoT into the context whenever the
        // request carries `tools`, so an assistant turn that thought must carry
        // it back under the documented field name — not OpenRouter's unified
        // `reasoning` object (ADR-0083). If tools were called, reasoning_content
        // is strictly mandatory on the wire; supply an empty string if pruned.
        if dialect.replays_reasoning_content() {
            if let Some(reasoning) = &reasoning_content {
                map["reasoning_content"] = json!(reasoning);
            } else if map.get("tool_calls").is_some() {
                map["reasoning_content"] = json!("");
            }
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
mod tests;
