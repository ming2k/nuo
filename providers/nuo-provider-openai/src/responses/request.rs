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

use nuo_model_codec::{Effort, Message, Role};
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
/// reasoning-effort override, and the ChatGPT account id.
pub struct BodyInput<'a> {
    pub model: &'a str,
    pub stream: bool,
    /// Structured instructions from the instruction manifest.
    pub instructions: Option<&'a nuo_model_codec::InstructionBundle>,
    /// OpenAI-shaped tool specs (`{type:"function", function:{...}}`), if any.
    pub tool_specs: Option<&'a [nuo_model_codec::ToolSpec]>,
    pub reasoning_effort: Option<Effort>,
    pub delivery: &'a nuo_model_codec::RequestDelivery,
    pub store: bool,
    pub cache_plan: &'a nuo_model_codec::ResolvedCachePolicy,
}

/// Build the Responses request body.
///
/// System messages are folded into the top-level `instructions` field; the
/// remaining conversation is projected to Responses `input` items. Tool calls
/// and results are reconciled so the request is always wire-valid: every
/// `function_call_output` references a preceding `function_call`, and every
/// `function_call` has its output (mirrors the chat-completions builder).
pub fn body(messages: Vec<Message>, input: BodyInput<'_>) -> Result<Value, RequestBuildError> {
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
) -> Result<Value, RequestBuildError> {
    let BodyInput {
        model: model_id,
        stream,
        instructions: input_instructions,
        tool_specs,
        reasoning_effort,
        delivery,
        store,
        cache_plan,
    } = input;

    // A remote continuation already owns the anchor assistant's function calls
    // server-side. Its new input therefore carries only their outputs, which
    // the tool-trace projection must retain without looking for local calls.
    let remote_call_ids = remote_parent_call_ids(&messages, delivery);

    // Fold system instructions and legacy messages into `instructions`,
    // project images for this route's declared support (the Responses API
    // rejects `input_image` on a route without it), and project a remote
    // continuation down to only the locally new suffix. The image policy is
    // shared with every other transport (`crate::vision`, ADR-0230): only a
    // route that *declared* no image input loses its attachments.
    let (messages, _dropped_images) =
        nuo_provider_transport::vision::project_images_for_route(model_id, messages, capabilities);
    let mut instructions = input_instructions
        .map(|b| b.render_combined())
        .unwrap_or_default();
    let mut working: Vec<Message> = Vec::with_capacity(messages.len());
    for (index, m) in messages.into_iter().enumerate() {
        match m.role {
            Role::System => {
                if !instructions.is_empty() {
                    instructions.push('\n');
                }
                instructions.push_str(&m.content);
            }
            _ => {
                if let nuo_model_codec::RequestDelivery::RemoteContinuation { input_start, .. } =
                    delivery
                    && index < *input_start
                {
                    continue;
                }
                working.push(m);
            }
        }
    }

    let working: Vec<Message> = working.into_iter().filter(valid_message).collect();

    let mut input_items: Vec<InputItem> = Vec::new();
    for m in working {
        match m.role {
            Role::User => {
                input_items.push(InputItem::plain(message_item("user", &m, "input_text")));
            }
            Role::Assistant => {
                if matches!(delivery, nuo_model_codec::RequestDelivery::OpaqueReplay) {
                    match m.provider_meta.as_ref().and_then(|meta| {
                        meta.get(nuo_model_codec::OPENAI_RESPONSE_OUTPUT_ARTIFACT_KEY)
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
                            input_items
                                .extend(items.iter().cloned().map(InputItem::provider_owned));
                            continue;
                        }
                        Some(_) => {
                            return Err(RequestBuildError::MalformedReplayArtifact);
                        }
                        None => {}
                    }
                }
                // Emit an assistant message item for any text content...
                if !m.content.is_empty() {
                    input_items.push(InputItem::plain(message_item(
                        "assistant",
                        &m,
                        "output_text",
                    )));
                }
                // ...then one typed function-call item per call, in order.
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
                input_items.push(InputItem::function_call_output(call_id, m.content));
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
        // Responses persistence mode:
        // - On ChatGPT Subscription backend (`chatgpt.com/backend-api/codex/responses`),
        //   the backend requires `store: false`.
        // - On the OpenAI platform Responses API (`api.openai.com`), `store: false`
        //   disables response retention on the server, while `store: true` retains
        //   response objects and enables `previous_response_id` continuation chains.
        "store": store,
    });
    if !store {
        // Stateless Responses continuation requires opaque reasoning items in
        // the returned output. The next turn replays that output verbatim.
        body["include"] = json!(["reasoning.encrypted_content"]);
    }
    if let nuo_model_codec::RequestDelivery::RemoteContinuation {
        previous_response_id,
        ..
    } = delivery
    {
        body["previous_response_id"] = json!(previous_response_id);
    }
    if !instructions.is_empty() {
        body["instructions"] = json!(instructions);
    }
    if let Some(specs) = flatten_tools(tool_specs) {
        body["tools"] = specs;
        body["tool_choice"] = json!("auto");
        body["parallel_tool_calls"] = json!(true);
    }
    // Reasoning: GPT-5 models always reason. Request the most verbose summaries
    // the backend offers (`detailed`) so the reasoning trace carries real
    // detail — the raw chain-of-thought is never exposed (OpenAI encrypts it as
    // `reasoning.encrypted_content`), so `detailed` is the ceiling. codex
    // exposes the same lever (`model_reasoning_summary: detailed`). Attach the
    // effort override when the model exposes effort levels.
    let mut reasoning = serde_json::Map::new();
    reasoning.insert("summary".to_string(), json!("detailed"));
    if let Some(effort) = reasoning_effort
        && !capabilities.effort_levels.is_empty()
    {
        reasoning.insert(
            "effort".to_string(),
            json!(effort.clamp_to_levels(&capabilities.effort_levels).as_str()),
        );
    }
    body["reasoning"] = Value::Object(reasoning);
    super::super::cache::project_responses_instructions_for_explicit_mode(&mut body, cache_plan);
    super::super::cache::apply(&mut body, cache_plan, "input");
    Ok(body)
}

/// Calls already stored behind `previous_response_id` are valid parents for
/// tool outputs in the locally new suffix. Only the cursor anchor can own such
/// calls; older calls are already settled inside the remote chain.
fn remote_parent_call_ids(
    messages: &[Message],
    delivery: &nuo_model_codec::RequestDelivery,
) -> Vec<String> {
    let nuo_model_codec::RequestDelivery::RemoteContinuation { input_start, .. } = delivery else {
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
                meta.contains_key(nuo_model_codec::OPENAI_RESPONSE_OUTPUT_ARTIFACT_KEY)
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

/// Flatten the OpenAI function-spec tool shape (`{type:"function",
/// Translate provider-neutral [`ToolSpec`]s into the OpenAI Responses tool
/// shape (`{type:"function", name, description, parameters, strict:false}`).
fn flatten_tools(tool_specs: Option<&[nuo_model_codec::ToolSpec]>) -> Option<Value> {
    let specs = tool_specs?;
    let out: Vec<Value> = specs
        .iter()
        .map(|spec| {
            json!({
                "type": "function",
                "name": spec.name,
                "description": spec.description,
                "parameters": spec.parameters.clone(),
                "strict": false,
            })
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
pub fn headers(
    access_token: &str,
    account_id: Option<&str>,
    copilot: bool,
    chatgpt: bool,
) -> Vec<(&'static str, String)> {
    let mut h = vec![("Authorization", format!("Bearer {access_token}"))];
    if copilot {
        for (name, value) in nuo_provider_transport::COPILOT_CLIENT_HEADERS {
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
mod tests {
    use super::*;
    use nuo_model_codec::ToolCall;

    static DEFAULT_DELIVERY: nuo_model_codec::RequestDelivery =
        nuo_model_codec::RequestDelivery::FullReplay;
    static DEFAULT_CACHE_PLAN: nuo_model_codec::ResolvedCachePolicy =
        nuo_model_codec::ResolvedCachePolicy::Unsupported;

    fn test_body_input<'a>(
        model: &'a str,
        stream: bool,
        tool_specs: Option<&'a [nuo_model_codec::ToolSpec]>,
        reasoning_effort: Option<Effort>,
        delivery: &'a nuo_model_codec::RequestDelivery,
        store: bool,
        cache_plan: &'a nuo_model_codec::ResolvedCachePolicy,
    ) -> BodyInput<'a> {
        BodyInput {
            model,
            stream,
            instructions: None,
            tool_specs,
            reasoning_effort,
            delivery,
            store,
            cache_plan,
        }
    }

    fn assistant_with_call(call: ToolCall, content: &str) -> Message {
        Message {
            tool_calls: Some(vec![call]),
            ..Message::new(Role::Assistant, content)
        }
    }

    #[test]
    fn system_messages_fold_into_instructions() {
        let body = body(
            vec![
                Message::new(Role::System, "be concise"),
                Message::new(Role::User, "hi"),
            ],
            test_body_input(
                "gpt-5.6-sol",
                true,
                None,
                None,
                &DEFAULT_DELIVERY,
                false,
                &DEFAULT_CACHE_PLAN,
            ),
        )
        .unwrap();
        assert_eq!(body["instructions"], "be concise");
        let input = body["input"].as_array().unwrap();
        assert_eq!(input.len(), 1);
        assert_eq!(input[0]["role"], "user");
        assert_eq!(input[0]["content"][0]["type"], "input_text");
    }

    #[test]
    fn function_call_and_output_round_trip_as_items() {
        let call = ToolCall {
            id: "call_1".into(),
            name: "bash".into(),
            arguments: "{\"command\":\"ls\"}".into(),
        };
        let result = Message {
            tool_call_id: Some("call_1".into()),
            ..Message::new(Role::Tool, "file.txt")
        };
        let body = body(
            vec![
                Message::new(Role::User, "list files"),
                assistant_with_call(call, "running it"),
                result,
            ],
            test_body_input(
                "gpt-5.6-sol",
                true,
                None,
                None,
                &DEFAULT_DELIVERY,
                false,
                &DEFAULT_CACHE_PLAN,
            ),
        )
        .unwrap();
        let input = body["input"].as_array().unwrap();
        // user, assistant(message), function_call, function_call_output.
        assert_eq!(input.len(), 4);
        assert_eq!(input[2]["type"], "function_call");
        assert_eq!(input[2]["call_id"], "call_1");
        assert_eq!(input[3]["type"], "function_call_output");
        assert_eq!(input[3]["output"], "file.txt");
    }

    #[test]
    fn duplicate_provider_call_ids_are_uniquified_with_their_outputs() {
        let first_call = ToolCall {
            id: "call_244115".into(),
            name: "first".into(),
            arguments: "{}".into(),
        };
        let second_call = ToolCall {
            id: "call_244115".into(),
            name: "second".into(),
            arguments: "{}".into(),
        };
        let body = body(
            vec![
                Message::new(Role::User, "first turn"),
                assistant_with_call(first_call.clone(), ""),
                Message::tool_result(&first_call, "first result"),
                Message::new(Role::User, "second turn"),
                assistant_with_call(second_call.clone(), ""),
                Message::tool_result(&second_call, "second result"),
            ],
            test_body_input(
                "deepseek-v4-flash",
                true,
                None,
                None,
                &nuo_model_codec::RequestDelivery::OpaqueReplay,
                false,
                &DEFAULT_CACHE_PLAN,
            ),
        )
        .unwrap();

        let input = body["input"].as_array().unwrap();
        let calls: Vec<&Value> = input
            .iter()
            .filter(|item| item["type"] == "function_call")
            .collect();
        let outputs: Vec<&Value> = input
            .iter()
            .filter(|item| item["type"] == "function_call_output")
            .collect();
        assert_eq!(calls.len(), 2);
        assert_eq!(outputs.len(), 2);
        assert_eq!(calls[0]["call_id"], "call_244115");
        assert_ne!(calls[0]["call_id"], calls[1]["call_id"]);
        assert_eq!(outputs[0]["call_id"], calls[0]["call_id"]);
        assert_eq!(outputs[1]["call_id"], calls[1]["call_id"]);
    }

    #[test]
    fn duplicate_ids_inside_opaque_replay_preserve_provider_items() {
        fn opaque_assistant(call: ToolCall, item_id: &str) -> Message {
            let mut provider_meta = serde_json::Map::new();
            provider_meta.insert(
                nuo_model_codec::OPENAI_RESPONSE_OUTPUT_ARTIFACT_KEY.to_string(),
                serde_json::json!([{
                    "id": item_id,
                    "type": "function_call",
                    "call_id": call.id,
                    "name": call.name,
                    "arguments": call.arguments,
                    "status": "completed"
                }]),
            );
            Message {
                tool_calls: Some(vec![call]),
                provider_meta: Some(provider_meta),
                ..Message::new(Role::Assistant, "")
            }
        }

        let first = ToolCall {
            id: "duplicate".into(),
            name: "first".into(),
            arguments: "{}".into(),
        };
        let second = ToolCall {
            id: "duplicate".into(),
            name: "second".into(),
            arguments: "{}".into(),
        };
        let body = body(
            vec![
                opaque_assistant(first.clone(), "fc_1"),
                Message::tool_result(&first, "one"),
                opaque_assistant(second.clone(), "fc_2"),
                Message::tool_result(&second, "two"),
            ],
            test_body_input(
                "deepseek-v4-flash",
                true,
                None,
                None,
                &nuo_model_codec::RequestDelivery::OpaqueReplay,
                false,
                &DEFAULT_CACHE_PLAN,
            ),
        )
        .unwrap();

        let input = body["input"].as_array().unwrap();
        assert_eq!(input[0]["id"], "fc_1");
        assert_eq!(input[0]["status"], "completed");
        assert_eq!(input[0]["call_id"], "duplicate");
        assert_eq!(input[1]["call_id"], "duplicate");
        assert_eq!(input[2]["id"], "fc_2");
        assert_eq!(input[2]["status"], "completed");
        assert_eq!(input[2]["call_id"], "call_nuo_1");
        assert_eq!(input[3]["call_id"], "call_nuo_1");
    }

    #[test]
    fn remote_continuation_keeps_outputs_for_calls_in_the_cursor_anchor() {
        let call = ToolCall {
            id: "remote_call".into(),
            name: "lookup".into(),
            arguments: "{}".into(),
        };
        let delivery = nuo_model_codec::RequestDelivery::RemoteContinuation {
            previous_response_id: "resp_1".into(),
            input_start: 2,
        };
        let body = body(
            vec![
                Message::new(Role::User, "look it up"),
                assistant_with_call(call.clone(), ""),
                Message::tool_result(&call, "done"),
            ],
            test_body_input(
                "gpt-5.6-sol",
                true,
                None,
                None,
                &delivery,
                true,
                &DEFAULT_CACHE_PLAN,
            ),
        )
        .unwrap();

        assert_eq!(body["previous_response_id"], "resp_1");
        let input = body["input"].as_array().unwrap();
        assert_eq!(input.len(), 1);
        assert_eq!(input[0]["type"], "function_call_output");
        assert_eq!(input[0]["call_id"], "remote_call");
        assert_eq!(input[0]["output"], "done");
    }

    #[test]
    fn unanswered_function_calls_are_stripped() {
        let call = ToolCall {
            id: "call_x".into(),
            name: "bash".into(),
            arguments: "{}".into(),
        };
        let body = body(
            vec![
                Message::new(Role::User, "go"),
                assistant_with_call(call, "thinking"),
            ],
            test_body_input(
                "gpt-5.6-sol",
                false,
                None,
                None,
                &DEFAULT_DELIVERY,
                false,
                &DEFAULT_CACHE_PLAN,
            ),
        )
        .unwrap();
        let input = body["input"].as_array().unwrap();
        // Only user + assistant message survive; the unanswered call is dropped.
        assert_eq!(input.len(), 2);
        assert!(input.iter().all(|i| i["type"] != "function_call"));
    }

    #[test]
    fn reasoning_summary_always_requested_with_effort_when_supported() {
        let body = body(
            vec![Message::new(Role::User, "hi")],
            test_body_input(
                "gpt-5.6-sol",
                true,
                None,
                Some(Effort::Medium),
                &DEFAULT_DELIVERY,
                false,
                &DEFAULT_CACHE_PLAN,
            ),
        )
        .unwrap();
        // The most verbose summaries the backend offers — the raw chain of
        // thought is never exposed, so `detailed` is the ceiling.
        assert_eq!(body["reasoning"]["summary"], "detailed");
        assert_eq!(body["reasoning"]["effort"], "medium");
    }

    #[test]
    fn stateless_requests_preserve_encrypted_reasoning_and_parallel_tools() {
        let request = body_with_capabilities(
            vec![Message::new(Role::User, "hi")],
            test_body_input(
                "gpt-5.6-sol",
                true,
                Some(&[nuo_model_codec::ToolSpec {
                    name: "lookup".to_string(),
                    description: "lookup".to_string(),
                    parameters: serde_json::json!({"type": "object"}),
                }]),
                None,
                &nuo_model_codec::RequestDelivery::OpaqueReplay,
                false,
                &nuo_model_codec::ResolvedCachePolicy::Unsupported,
            ),
            &nuo_model_codec::ModelCapabilities::for_channel("gpt-5.6-sol", None),
        )
        .unwrap();
        assert_eq!(
            request["include"],
            serde_json::json!(["reasoning.encrypted_content"])
        );
        assert_eq!(request["parallel_tool_calls"], true);
    }

    #[test]
    fn headers_include_account_id_when_present() {
        let h = headers("tok", Some("acct-1"), false, true);
        assert_eq!(h[0].0, "Authorization");
        assert_eq!(h[1], ("originator", "codex_cli_rs".to_string()));
        assert_eq!(h[2].0, "ChatGPT-Account-Id");
        assert_eq!(h[2].1, "acct-1");
        // No account id → no header.
        assert_eq!(headers("tok", None, false, true).len(), 2);
        // A third-party Responses endpoint gets neither Codex header.
        assert_eq!(headers("tok", Some("acct-1"), false, false).len(), 1);
    }

    #[test]
    fn headers_inject_copilot_set_and_drop_account_id() {
        // Copilot mode: the account id is ignored and Copilot's required
        // headers replace the ChatGPT account-id header.
        let h = headers("tok", Some("acct-1"), true, false);
        assert_eq!(h[0].0, "Authorization");
        assert_eq!(h[0].1, "Bearer tok");
        let names: Vec<&str> = h.iter().map(|(n, _)| *n).collect();
        assert_eq!(
            names,
            [
                "Authorization",
                "Copilot-Integration-Id",
                "Editor-Version",
                "Editor-Plugin-Version",
                "x-initiator",
                "Openai-Intent",
                "X-GitHub-Api-Version"
            ]
        );
        assert_eq!(h[1].1, "vscode-chat");
        assert_eq!(h[2].1, "vscode/1.107.0");
        assert_eq!(h[3].1, "copilot-chat/0.35.0");
        assert_eq!(h[4].1, "user");
        assert_eq!(h[5].1, "conversation-edits");
        assert_eq!(h[6].1, "2026-06-01");
        // No ChatGPT-Account-Id leaks through in Copilot mode.
        assert!(h.iter().all(|(n, _)| *n != "ChatGPT-Account-Id"));
    }

    #[test]
    fn has_input_image_detects_image_parts() {
        let with_image = json!({
            "input": [
                {"role": "user", "content": [
                    {"type": "input_text", "text": "what is this?"},
                    {"type": "input_image", "image_url": "data:..."}
                ]}
            ]
        });
        assert!(has_input_image(&with_image));

        let text_only = json!({
            "input": [{"role": "user", "content": [{"type": "input_text", "text": "hi"}]}]
        });
        assert!(!has_input_image(&text_only));

        // Defensive: a missing or non-array input is no image.
        assert!(!has_input_image(&json!({"model": "x"})));
        assert!(!has_input_image(&json!({"input": "not-an-array"})));
    }

    #[test]
    fn user_message_with_image_serializes_as_input_image_string_url() {
        let mut msg = Message::new(Role::User, "look at this");
        msg.images = Some(vec![nuo_model_codec::message::ImagePart {
            mime: "image/png".to_string(),
            data: "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=".to_string(),
        }]);

        let remote = nuo_model_codec::RemoteModelMetadata {
            vision: Some(true),
            ..Default::default()
        };
        let caps = nuo_model_codec::ModelCapabilities::for_channel("gpt-4o", Some(&remote));

        let payload = body_with_capabilities(
            vec![msg],
            test_body_input(
                "gpt-4o",
                false,
                None,
                None,
                &DEFAULT_DELIVERY,
                false,
                &DEFAULT_CACHE_PLAN,
            ),
            &caps,
        )
        .unwrap();

        let input = payload["input"].as_array().expect("input array");
        assert_eq!(input.len(), 1);
        let content = input[0]["content"].as_array().expect("content array");
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["type"], "input_text");
        assert_eq!(content[0]["text"], "look at this");

        assert_eq!(content[1]["type"], "input_image");
        assert_eq!(
            content[1]["image_url"],
            "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII="
        );
    }

    #[test]
    fn declared_text_only_route_projects_remote_continuation_images_away() {
        // ADR-0230: the projection runs before the delivery split, so an image
        // carried by the locally-new suffix of a remote continuation is dropped
        // for a route that declared no image input — the Responses API 400s on
        // `input_image` outright.
        let mut msg = Message::new(Role::User, "look at this");
        msg.images = Some(vec![nuo_model_codec::message::ImagePart {
            mime: "image/png".to_string(),
            data: "aGk=".to_string(),
        }]);

        let remote = nuo_model_codec::RemoteModelMetadata {
            vision: Some(false),
            ..Default::default()
        };
        let caps = nuo_model_codec::ModelCapabilities::for_channel("gpt-4o", Some(&remote));

        let payload = body_with_capabilities(
            vec![msg],
            test_body_input(
                "gpt-4o",
                false,
                None,
                None,
                &DEFAULT_DELIVERY,
                false,
                &DEFAULT_CACHE_PLAN,
            ),
            &caps,
        )
        .unwrap();

        let content = payload["input"][0]["content"]
            .as_array()
            .expect("content array");
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["type"], "input_text");
    }

    #[test]
    fn empty_opaque_replay_artifact_fails_closed() {
        let mut provider_meta = serde_json::Map::new();
        provider_meta.insert(
            nuo_model_codec::OPENAI_RESPONSE_OUTPUT_ARTIFACT_KEY.to_string(),
            serde_json::json!([]),
        );
        let assistant = Message {
            provider_meta: Some(provider_meta),
            ..Message::new(Role::Assistant, "previous answer")
        };
        let error = body(
            vec![assistant, Message::new(Role::User, "continue")],
            test_body_input(
                "gpt-5.6-sol",
                true,
                None,
                None,
                &nuo_model_codec::RequestDelivery::OpaqueReplay,
                false,
                &DEFAULT_CACHE_PLAN,
            ),
        )
        .unwrap_err();
        assert_eq!(error, RequestBuildError::EmptyReplayArtifact);
        assert!(error.to_string().contains("start a new session"));
    }

    #[test]
    fn orphan_tool_output_fails_instead_of_disappearing() {
        let error = body(
            vec![Message {
                tool_call_id: Some("missing_call".to_string()),
                ..Message::new(Role::Tool, "result")
            }],
            test_body_input(
                "gpt-5.6-sol",
                true,
                None,
                None,
                &DEFAULT_DELIVERY,
                false,
                &DEFAULT_CACHE_PLAN,
            ),
        )
        .unwrap_err();
        assert!(error.to_string().contains("unknown call_id `missing_call`"));
    }
}
