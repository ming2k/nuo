//! OpenAI **Responses** API — request construction.
//!
//! Pure functions turning the harness's `Vec<Message>` into the Responses
//! request shape: top-level `instructions` (the system prompt) + an `input`
//! array of typed items (`message`, `function_call`, `function_call_output`),
//! plus optional `tools`/`reasoning`. No `reqwest`, no `async`, no I/O.
//!
//! Responses wire shape (ChatGPT Subscription backend):
//! - URL: the `/responses` endpoint (e.g. `https://chatgpt.com/backend-api/codex/responses`).
//! - Auth: `Authorization: Bearer <oauth access_token>` plus the optional
//!   `ChatGPT-Account-Id` header (applied by the provider, not here).
//! - Body: `{model, instructions?, input, tools?, reasoning?, tool_choice?, stream}`.
use nuo_provider::Effort;
use nuo_tool::{Message, Role};
use serde_json::{Value, json};

use super::tool_trace::{self, InputItem};

/// A request cannot be serialized without violating stateless replay or tool
/// trace invariants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestBuildError {
    EmptyReplayArtifact,
    MalformedReplayArtifact,
    InvalidToolTrace(String),
}

impl std::fmt::Display for RequestBuildError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyReplayArtifact => formatter.write_str(
                "Cannot replay this Responses turn: its provider output artifact is empty. The session was recorded by an incompatible client; start a new session instead of continuing with incomplete model state.",
            ),
            Self::MalformedReplayArtifact => formatter.write_str(
                "Cannot replay this Responses turn: its provider output artifact is malformed. Start a new session instead of continuing with incomplete model state.",
            ),
            Self::InvalidToolTrace(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for RequestBuildError {}

/// Inputs to [`body`]: the model id, whether this is a streaming request, the
/// prepared tool schemas (OpenAI function-spec shape, optional), the optional
/// reasoning-effort override, and the dialect whose surface this request speaks.
pub struct BodyInput<'a> {
    pub model: &'a str,
    pub stream: bool,
    /// Structured instructions from the instruction manifest.
    pub instructions: Option<&'a nuo_provider::InstructionText>,
    /// OpenAI-shaped tool specs (`{type:"function", function:{...}}`), if any.
    pub tool_specs: Option<&'a [nuo_provider::capability::ToolSpec]>,
    pub reasoning_effort: Option<Effort>,
    pub delivery: &'a nuo_provider::RequestDelivery,
    pub store: bool,
    /// Provider-specific behavior layered on the shared Responses wire. The
    /// builder never emits a field this dialect does not define (ADR-0083).
    pub dialect: nuo_provider::OpenAiResponsesDialect,
    pub cache_plan: &'a nuo_provider::ResolvedCachePolicy,
}

/// Build the Responses request body.
///
/// System messages are folded into the top-level `instructions` field; the
/// remaining conversation is projected to Responses `input` items. Tool calls
/// and results are reconciled so the request is always wire-valid: every
/// `function_call_output` references a preceding `function_call`, and every
/// `function_call` has its output (mirrors the chat-completions builder).
pub fn body(
    conversation: &[Message],
    temporary_context: &[Message],
    input: BodyInput<'_>,
) -> Result<Value, RequestBuildError> {
    let capabilities = nuo_provider::ModelCapabilities::for_channel(input.model, None);
    body_with_capabilities(conversation, temporary_context, input, &capabilities)
}

/// Build a request with a provider-channel capability view. The provider calls
/// this for trusted remote metadata; [`body`] remains the static-baseline entry
/// point for standalone callers and tests.
pub fn body_with_capabilities(
    conversation: &[Message],
    temporary_context: &[Message],
    input: BodyInput<'_>,
    capabilities: &nuo_provider::ModelCapabilities,
) -> Result<Value, RequestBuildError> {
    let BodyInput {
        model: model_id,
        stream,
        instructions: input_instructions,
        tool_specs,
        reasoning_effort,
        delivery,
        store,
        dialect,
        cache_plan,
    } = input;

    // A remote continuation already owns the anchor assistant's function calls
    // server-side. Its new input therefore carries only their outputs, which
    // the tool-trace projection must retain without looking for local calls.
    let remote_call_ids = remote_parent_call_ids(conversation, delivery);

    // Fold system instructions and legacy messages into `instructions`,
    // project images for this route's declared support (the Responses API
    // rejects `input_image` on a route without it), and project a remote
    // continuation down to only the locally new suffix. The image policy is
    // shared with every other transport (`crate::vision`): only a
    // route that *declared* no image input loses its attachments.
    let (conversation, _dropped_images) =
        nuo_provider::project_images_for_route(model_id, conversation, capabilities);
    let messages: Vec<&Message> = conversation
        .iter()
        .chain(temporary_context.iter())
        .collect();
    let (mut instructions, ephemeral_instructions) = if let Some(bundle) = input_instructions {
        (
            bundle.render_fixed().to_owned(),
            bundle.render_transient().to_owned(),
        )
    } else {
        (String::new(), String::new())
    };
    let mut working: Vec<&Message> = Vec::with_capacity(messages.len());
    for (index, m) in messages.into_iter().enumerate() {
        match m.role {
            Role::System => {
                // Grandfather Role::System for backward compatibility into static instruction prefix (ADR-0052, [INV-INST-01])
                if !instructions.is_empty() {
                    instructions.push('\n');
                }
                instructions.push_str(&m.content);
            }
            _ => {
                if let nuo_provider::RequestDelivery::RemoteContinuation { input_start, .. } =
                    delivery
                    && index < *input_start
                {
                    continue;
                }
                working.push(m);
            }
        }
    }

    let working: Vec<&Message> = working.into_iter().filter(|m| valid_message(m)).collect();

    let mut input_items: Vec<InputItem> = Vec::new();
    // Ephemeral instruction slice is emitted as a developer item rather than polluting
    // the top-level instructions, preserving prefix cache reuse on OpenAI (ADR-0052, [INV-INST-03]).
    // Dialects that reject the developer role (DeepSeek) fold it directly into instructions.
    if !ephemeral_instructions.is_empty() {
        if dialect.supports_developer_role() {
            input_items.push(InputItem::plain(json!({
                "type": "message",
                "role": "developer",
                "content": [{ "type": "input_text", "text": ephemeral_instructions }]
            })));
        } else {
            if !instructions.is_empty() {
                instructions.push_str("\n\n");
            }
            instructions.push_str(&ephemeral_instructions);
        }
    }
    let last_assistant_idx = working.iter().rposition(|m| m.role == Role::Assistant);
    for (m_idx, m) in working.into_iter().enumerate() {
        match m.role {
            Role::User => {
                input_items.push(InputItem::plain(message_item("user", m, "input_text")));
            }
            Role::Assistant => {
                let is_latest_assistant = Some(m_idx) == last_assistant_idx;
                let is_deepseek = dialect == nuo_provider::OpenAiResponsesDialect::DeepSeek;
                if matches!(delivery, nuo_provider::RequestDelivery::OpaqueReplay) {
                    match m.provider_meta.as_ref().and_then(|meta| {
                        meta.get(nuo_provider::OPENAI_RESPONSE_OUTPUT_ARTIFACT_KEY)
                    }) {
                        Some(Value::Array(items)) if items.is_empty() => {
                            return Err(RequestBuildError::EmptyReplayArtifact);
                        }
                        Some(Value::Array(items)) => {
                            for item in items {
                                if !item.is_object()
                                    || item
                                        .get("type")
                                        .and_then(Value::as_str)
                                        .is_none_or(str::is_empty)
                                {
                                    return Err(RequestBuildError::MalformedReplayArtifact);
                                }
                            }
                            // DeepSeek requirement: if an assistant message performed tool calls,
                            // a reasoning item must precede it. Ensure one is present if the artifact
                            // was missing it (e.g. cross-model handoff or upstream omission).
                            if m.tool_calls.is_some()
                                && (!dialect.encrypted_reasoning()
                                    || dialect.dual_form_reasoning_replay())
                                && !items.iter().any(|item| {
                                    item.get("type").and_then(Value::as_str) == Some("reasoning")
                                })
                            {
                                let reasoning_text = if is_deepseek && !is_latest_assistant {
                                    ""
                                } else {
                                    m.reasoning_content
                                        .as_deref()
                                        .filter(|s| !s.is_empty())
                                        .unwrap_or("")
                                };
                                input_items.push(InputItem::plain(reasoning_item(
                                    Some(reasoning_text),
                                    dialect.dual_form_reasoning_replay(),
                                )));
                            }
                            if (is_deepseek || dialect.dual_form_reasoning_replay())
                                && !is_latest_assistant
                            {
                                let pruned_items: Vec<Value> = items
                                    .iter()
                                    .map(|item| {
                                        if item.get("type").and_then(Value::as_str)
                                            == Some("reasoning")
                                        {
                                            reasoning_item(
                                                Some(""),
                                                dialect.dual_form_reasoning_replay(),
                                            )
                                        } else {
                                            item.clone()
                                        }
                                    })
                                    .collect();
                                input_items.extend(
                                    pruned_items.into_iter().map(InputItem::provider_owned),
                                );
                            } else {
                                input_items
                                    .extend(items.iter().cloned().map(InputItem::provider_owned));
                            }
                            continue;
                        }
                        Some(_) => {
                            return Err(RequestBuildError::MalformedReplayArtifact);
                        }
                        None => {}
                    }
                }
                // When opaque artifact is not available (e.g. session reload, FullReplay,
                // or message originated from a different provider), reconstruct the items.
                // 1. Emit reasoning item if reasoning_content is present, or if tools were called
                // on a thinking-enabled dialect requiring unencrypted reasoning.
                // For DeepSeek, older historical assistant turns have reasoning pruned to "" to prevent
                // context saturation and V-shaped attention collapse while satisfying upstream schema verification.
                //
                // A *portable* gateway fronts several upstreams with contradictory
                // replay contracts behind one URL, so it gets the dual form:
                // `summary` and `content` always present together, with the
                // pruned (older-turn) case carrying empty `summary` parts only —
                // never an empty `reasoning_text`, which one upstream rejects as
                // "must be passed back to the API" (ADR-0142).
                if dialect.dual_form_reasoning_replay() {
                    // An item is only *needed* where the wire demands one or
                    // where a real chain exists: a turn that performed a tool
                    // call must be preceded by reasoning for the upstreams that
                    // validate the pairing, while a plain text reply needs none.
                    // Older turns drop their chain entirely (ADR-0083 hygiene:
                    // replaying every historical chain saturates the context and
                    // collapses attention), keeping only the empty item a tool
                    // call requires; the latest turn replays what it really had.
                    let chain = m.reasoning_content.as_deref().filter(|s| !s.is_empty());
                    let has_call = m.tool_calls.is_some();
                    let (text, needed) = if is_latest_assistant {
                        (chain, has_call || chain.is_some())
                    } else {
                        (None, has_call)
                    };
                    if needed {
                        input_items.push(InputItem::plain(reasoning_item(text, true)));
                    }
                } else if is_deepseek && !is_latest_assistant {
                    if m.tool_calls.is_some() {
                        input_items.push(InputItem::plain(reasoning_item(Some(""), false)));
                    }
                } else if let Some(reasoning) =
                    m.reasoning_content.as_deref().filter(|s| !s.is_empty())
                {
                    input_items.push(InputItem::plain(reasoning_item(Some(reasoning), false)));
                } else if m.tool_calls.is_some() && !dialect.encrypted_reasoning() {
                    input_items.push(InputItem::plain(reasoning_item(Some(""), false)));
                }
                // 2. Emit an assistant message item for any text content...
                if !m.content.is_empty() {
                    input_items.push(InputItem::plain(message_item(
                        "assistant",
                        m,
                        "output_text",
                    )));
                }
                // 3. ...then one typed function-call item per call, in order.
                // The Responses API requires a function_call item to precede its
                // function_call_output, and interleaves them with the message
                // flow, so emitting them right after the assistant text matches.
                if let Some(calls) = m.tool_calls.as_ref() {
                    for call in calls {
                        input_items.push(InputItem::function_call(call));
                    }
                }
            }
            Role::Tool => {
                let call_id = m.tool_call_id.clone().unwrap_or_default();
                input_items.push(InputItem::function_call_output(call_id, m.content.clone()));
            }
            Role::System => {} // folded into instructions above
        }
    }
    let input_items: Vec<Value> = tool_trace::project(input_items, remote_call_ids)
        .map_err(|error| RequestBuildError::InvalidToolTrace(error.to_string()))?
        .into_iter()
        .map(InputItem::into_wire)
        .collect();

    let mut body = json!({
        "model": model_id,
        "input": input_items,
        "stream": stream,
    });
    // Responses persistence mode:
    // - On ChatGPT Subscription backend (`chatgpt.com/backend-api/codex/responses`),
    //   the backend requires `store: false`.
    // - On the OpenAI platform Responses API (`api.openai.com`), `store: false`
    //   disables response retention on the server, while `store: true` retains
    //   response objects and enables `previous_response_id` continuation chains.
    // - DeepSeek's Responses API is purely stateless and does not define `store` (ADR-0083).
    if dialect.supports_store() {
        body["store"] = json!(store);
    }
    if !store && dialect.encrypted_reasoning() {
        // Stateless Responses continuation on OpenAI's own backend requires the
        // opaque encrypted reasoning items in the returned output; the next
        // turn replays that output verbatim. Backends that return plaintext
        // `reasoning` items (DeepSeek) neither define this grant nor need it.
        body["include"] = json!(["reasoning.encrypted_content"]);
    }
    if let nuo_provider::RequestDelivery::RemoteContinuation {
        continuation_id, ..
    } = delivery
    {
        body["previous_response_id"] = json!(continuation_id);
    }
    if !instructions.is_empty() {
        body["instructions"] = json!(instructions);
    }
    if let Some(specs) = format_tools(tool_specs, dialect.nested_tool_definitions()) {
        body["tools"] = specs;
        if dialect.supports_tool_choice() {
            body["tool_choice"] = json!("auto");
        }
        if dialect.supports_parallel_tool_calls() {
            body["parallel_tool_calls"] = json!(true);
        }
    }
    // Reasoning: GPT-5 models always reason. Request the most verbose summaries
    // the backend offers (`detailed`) so the reasoning trace carries real
    // detail — the raw chain-of-thought is never exposed (OpenAI encrypts it as
    // `reasoning.encrypted_content`), so `detailed` is the ceiling. codex
    // exposes the same lever (`model_reasoning_summary: detailed`). Attach the
    // effort override when the model exposes effort levels.
    //
    // Only the fields this dialect defines are emitted: a backend whose
    // `reasoning` object is `{effort}` alone must not receive OpenAI's
    // `summary` (DeepSeek, ADR-0083). An empty object is omitted entirely so
    // the documented server default applies instead of an empty override.
    let mut reasoning = serde_json::Map::new();
    if dialect.reasoning_summary() {
        reasoning.insert("summary".to_string(), json!("detailed"));
    }
    if let Some(effort) = reasoning_effort
        && !capabilities.effort_levels.is_empty()
    {
        reasoning.insert(
            "effort".to_string(),
            json!(effort.clamp_to_levels(&capabilities.effort_levels).as_str()),
        );
    }
    if !reasoning.is_empty() {
        body["reasoning"] = Value::Object(reasoning);
    }
    super::super::cache::project_responses_instructions_for_explicit_mode(&mut body, cache_plan);
    super::super::cache::apply(&mut body, cache_plan, "input");
    Ok(body)
}

/// Calls already stored behind `previous_response_id` are valid parents for
/// tool outputs in the locally new suffix. Only the cursor anchor can own such
/// calls; older calls are already settled inside the remote chain.
fn remote_parent_call_ids(
    messages: &[Message],
    delivery: &nuo_provider::RequestDelivery,
) -> Vec<String> {
    let nuo_provider::RequestDelivery::RemoteContinuation { input_start, .. } = delivery else {
        return Vec::new();
    };
    let Some(anchor_index) = input_start.checked_sub(1) else {
        return Vec::new();
    };
    messages
        .get(anchor_index)
        .and_then(|message| message.tool_calls.as_ref())
        .into_iter()
        .flatten()
        .map(|call| call.id.clone())
        .collect()
}

/// Build a replayable `reasoning` input item.
///
/// `dual_form` emits both part families — `summary` (`summary_text`) and
/// `content` (`reasoning_text`) — in one item, which is the only shape a
/// translating gateway accepts when it fronts upstreams enforcing opposite
/// replay contracts (ADR-0142). The single-form case is DeepSeek's native
/// contract: `content` parts only, `summary` never mentioned.
///
/// A pruned older turn passes `Some("")`: that yields an empty `summary` part
/// list and *no* `content` part, because an empty `reasoning_text` is rejected
/// by those same upstreams.
fn reasoning_item(reasoning: Option<&str>, dual_form: bool) -> Value {
    let text = reasoning.unwrap_or("");
    if dual_form {
        // An empty summary is an empty *array*, not a part carrying `""`.
        let mut item = json!({
            "type": "reasoning",
            "summary": if text.is_empty() {
                json!([])
            } else {
                json!([{ "type": "summary_text", "text": text }])
            },
        });
        if !text.is_empty() {
            item["content"] = json!([{ "type": "reasoning_text", "text": text }]);
        }
        return item;
    }
    json!({
        "type": "reasoning",
        "content": [{ "type": "reasoning_text", "text": text }]
    })
}

/// Discard assistant turns that are completely empty (no content, no tool
/// calls). System messages never reach here (folded into `instructions`).
fn valid_message(message: &Message) -> bool {
    if message.role == Role::Assistant {
        let empty = message.content.is_empty()
            && message
                .tool_calls
                .as_ref()
                .map(|calls| calls.is_empty())
                .unwrap_or(true)
            && !message.provider_meta.as_ref().is_some_and(|meta| {
                meta.contains_key(nuo_provider::OPENAI_RESPONSE_OUTPUT_ARTIFACT_KEY)
            });
        return !empty;
    }
    true
}

/// Build a Responses `message` item: `{type:"message", role, content:[...]}`.
/// `text_part` selects `input_text` (user) vs `output_text` (assistant). Inline
/// images become `input_image` parts.
fn message_item(role: &str, m: &Message, text_part: &str) -> Value {
    let mut parts: Vec<Value> = Vec::new();
    if !m.content.is_empty() {
        parts.push(json!({ "type": text_part, "text": m.content }));
    }
    if let Some(images) = m.images.as_ref() {
        for image in images {
            parts.push(json!({
                "type": "input_image",
                "image_url": format!("data:{};base64,{}", image.mime, image.data)
            }));
        }
    }
    json!({ "type": "message", "role": role, "content": parts })
}

/// Translate provider-neutral [`ToolSpec`]s into the wire tool shape:
/// - Nested shape for DeepSeek Responses (`{type:"function", function:{name, description, parameters}}`)
/// - Flattened shape for OpenAI Responses (`{type:"function", name, description, parameters, strict:false}`).
fn format_tools(
    tool_specs: Option<&[nuo_provider::capability::ToolSpec]>,
    nested: bool,
) -> Option<Value> {
    let specs = tool_specs?;
    let out: Vec<Value> = specs
        .iter()
        .map(|spec| {
            if nested {
                json!({
                    "type": "function",
                    "function": {
                        "name": spec.name,
                        "description": spec.description,
                        "parameters": spec.parameters.clone(),
                    }
                })
            } else {
                json!({
                    "type": "function",
                    "name": spec.name,
                    "description": spec.description,
                    "parameters": spec.parameters.clone(),
                    "strict": false,
                })
            }
        })
        .collect();
    if out.is_empty() {
        None
    } else {
        Some(Value::Array(out))
    }
}

/// The per-request auth + provider headers for the Responses surface. Beyond
/// the always-present `Authorization: Bearer`:
/// - **ChatGPT/Codex mode** (`chatgpt == true`): `originator: codex_cli_rs` identifies
///   the client and the ChatGPT account id is attached as
///   `ChatGPT-Account-Id` when known.
/// - **Copilot mode** (`copilot == true`): GitHub Copilot's required headers
///   are attached instead — the client-identity headers
///   (`Copilot-Integration-Id`, `Editor-Version`, `Editor-Plugin-Version`)
///   that let the backend resolve the account's actual plan entitlements,
///   plus the per-turn headers `x-initiator` (treated as a user-initiated
///   turn by default; the harness does not currently distinguish agent
///   turns), `Openai-Intent: conversation-edits`, and `X-GitHub-Api-Version`.
///   The ChatGPT account-id header is omitted. Vision turns additionally need
///   `Copilot-Vision-Request: true`, but that depends on the request body
///   (whether an `input_image` part is present), so it is injected by the
///   provider's request builder (see [`has_input_image`]) rather than this
///   header list.
const COPILOT_CLIENT_HEADERS: &[(&str, &str)] = &[
    ("Copilot-Integration-Id", "vscode-chat"),
    ("Editor-Version", "vscode/1.107.0"),
    ("Editor-Plugin-Version", "copilot-chat/0.35.0"),
];

pub fn headers(
    access_token: &str,
    account_id: Option<&str>,
    copilot: bool,
    chatgpt: bool,
) -> Vec<(&'static str, String)> {
    let mut h = vec![("Authorization", format!("Bearer {access_token}"))];
    if copilot {
        for (name, value) in COPILOT_CLIENT_HEADERS {
            h.push((*name, value.to_string()));
        }
        h.push(("x-initiator", "user".to_string()));
        h.push(("Openai-Intent", "conversation-edits".to_string()));
        h.push(("X-GitHub-Api-Version", "2026-06-01".to_string()));
        return h;
    }
    if !chatgpt {
        return h;
    }
    h.push(("originator", "codex_cli_rs".to_string()));
    if let Some(id) = account_id.filter(|id| !id.trim().is_empty()) {
        h.push(("ChatGPT-Account-Id", id.to_string()));
    }
    h
}

/// Whether a Responses request body carries an image input, i.e. any
/// `input_image` content part anywhere in the `input` items array. Copilot
/// requires `Copilot-Vision-Request: true` on such turns; this scan is the
/// signal the provider's request builder uses to set it. Defensive: a missing
/// or non-array `input` is treated as no image.
pub fn has_input_image(body: &Value) -> bool {
    body.get("input")
        .and_then(Value::as_array)
        .is_some_and(|items| {
            items.iter().any(|item| {
                item.get("content")
                    .and_then(Value::as_array)
                    .is_some_and(|parts| {
                        parts.iter().any(|part| {
                            part.get("type").and_then(Value::as_str) == Some("input_image")
                        })
                    })
            })
        })
}

#[cfg(test)]
mod tests;
