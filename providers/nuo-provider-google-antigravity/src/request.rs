//! Google Gemini — request construction.
//!
//! Pure functions that turn the harness's `Vec<Message>` into the Google
//! `generateContent` request shape: a `contents` array of `{role, parts}`,
//! with `systemInstruction` lifted to the top level and OpenAI-shaped tool
//! schemas converted to Google `functionDeclarations`. No `reqwest`, no
//! `async`, no I/O — these are testable in isolation.
//!
//! Google's wire shape:
//! - URL: `{base}/models/{model}:generateContent` (streaming adds
//!   `:streamGenerateContent?alt=sse`).
//! - Auth: header-based (`x-goog-api-key: <key>` for Google AI Studio API keys,
//!   `Authorization: Bearer <token>` for OAuth/IAM tokens). Clean URLs without
//!   key leakage in query parameters.
//! - Body:
//!   `{contents: [{role, parts}], systemInstruction?: {parts:[{text}]}, tools?}`.
//! - Roles are only `user` and `model` (assistant). Tool calls are assistant
//!   `functionCall` parts; tool results are user `functionResponse` parts.
use nuo_tool::{Message, Role};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};

use super::response::{TEXT_THOUGHT_SIGNATURE_META_KEY, THOUGHT_SIGNATURES_META_KEY};

/// The resolved reasoning-depth directive to stamp into `thinkingConfig` — the
/// wire form of a channel's [`nuo_provider::Effort`] override once it has been
/// clamped and translated for this Gemini model.
///
/// Gemini exposes **two mutually exclusive** depth surfaces, so a single field
/// is insufficient and the request never sends both (doing so is a 400):
/// - [`GoogleThinking::Level`] — Gemini 3.x `thinkingLevel` enum.
/// - [`GoogleThinking::Budget`] — Gemini 2.5 `thinkingBudget` integer tokens.
///
/// `None` here means "no override; let the server default stand" and is
/// distinct from an explicit off (Gemini 2.5 Flash honors a `0` budget; Gemini
/// 3.x cannot disable thinking at all, so there is no off path for it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoogleThinking {
    /// `thinkingLevel` for a Gemini 3.x model: one of `minimal`/`low`/
    /// `medium`/`high`. Never `none`/`xhigh`/`max` (Gemini 3.x rejects them);
    /// the resolver clamps those down before constructing this variant.
    Level(nuo_provider::Effort),
    /// `thinkingBudget` for a Gemini 2.5 model: a token count in the model's
    /// supported range. `0` disables thinking (Flash/Lite only — Pro's floor is
    /// `128`, so the resolver only emits `0` when the model's ladder allows
    /// [`nuo_provider::Effort::None`]).
    Budget(i64),
}

/// Resolve a channel's raw reasoning-effort override into the wire directive
/// this Gemini model expects, clamping it to the model's supported levels.
///
/// The model's **effort ladder** decides which surface applies: a
/// `thinkingLevel` ladder (e.g. `minimal..=high`) maps each rung
/// to a level enum; a `thinkingBudget` ladder (including `max`) maps each rung to a token bucket
/// (the bucket cap is `max_budget`, e.g. `24576` for Gemini 2.5 Flash or
/// `32768` for Pro). Returns `None` when there is nothing to stamp — an empty
/// ladder (non-reasoning / unknown model) or an unset override — leaving the
/// server default in place.
///
/// `max_budget` is only consulted for the budget surface; pass any value for a
/// level-only model.
pub fn resolve_thinking(
    effort: Option<nuo_provider::Effort>,
    effort_levels: &[nuo_provider::EffortLevel],
    max_budget: u32,
) -> Option<GoogleThinking> {
    // A model that does not advertise a depth ladder has no thinking surface to
    // stamp (non-reasoning models like Gemini 2.5 Flash-Lite / 2.0 Flash, or an
    // unknown model with a default empty ladder). Fall back to the server
    // default rather than guessing a form the upstream may reject.
    if effort_levels.is_empty() {
        return None;
    }
    let requested = effort?;
    // Gemini's ladders are always compiled-in known rungs, so extract the known
    // subset for ranking; `Other` (a provider tier outside the vocabulary) has
    // no Gemini surface to map onto and is ignored here.
    let known: Vec<nuo_provider::Effort> = effort_levels
        .iter()
        .filter_map(nuo_provider::EffortLevel::as_known)
        .collect();
    let clamped = requested.clamp_to(&known);
    // Decide the surface from the ladder's deepest rung: the budget ladder tops
    // out at `max`, the level ladder at `high`. This keeps the model-specific
    // choice in one place and never inspects the free-form model id string.
    let is_budget = known.contains(&nuo_provider::Effort::Max);
    Some(if is_budget {
        GoogleThinking::Budget(clamped.gemini_thinking_budget(max_budget))
    } else {
        GoogleThinking::Level(clamped)
    })
}

/// Inputs to [`body`]: the prepared tool schemas in OpenAI function-spec
/// shape, if any.
pub struct BodyInput<'a> {
    /// Structured instructions from the instruction manifest.
    pub instructions: Option<&'a nuo_provider::InstructionBundle>,
    pub tool_specs: Option<&'a [nuo_provider::capability::ToolSpec]>,
    /// Stamp `generationConfig.thinkingConfig.includeThoughts`. The Google API
    /// only returns reasoning *text* when the request asks for it, and rejects
    /// the flag with HTTP 400 on models that do not think — so this MUST be
    /// `true` only for reasoning models (`model.thinking.reasons()`).
    pub include_thoughts: bool,
    /// Resolved reasoning-depth directive for this model, or `None` to leave
    /// the server default in place. Built by [`resolve_thinking`] from the
    /// channel's effort override and the model's ladder; never carries both a
    /// level and a budget (Gemini 400s on that combination).
    pub thinking: Option<GoogleThinking>,
}

/// Build the Google `generateContent` request body from a message list.
pub fn body(messages: Vec<Message>, input: BodyInput<'_>) -> Value {
    body_inner(messages, input)
}

/// Build the request body with the channel's resolved capabilities, projecting
/// image parts for this route. The provider calls this; [`body`]
/// remains the entry point for standalone callers and tests that have no
/// channel view. `model` is the wire id, used only to label the projection's
/// diagnostic log.
pub fn body_with_capabilities(
    model: &str,
    messages: Vec<Message>,
    input: BodyInput<'_>,
    capabilities: &nuo_provider::ModelCapabilities,
) -> Value {
    let (messages, _dropped_images) =
        nuo_provider::project_images_for_route(model, messages, capabilities);
    body_inner(messages, input)
}

fn body_inner(messages: Vec<Message>, input: BodyInput<'_>) -> Value {
    let mut system = Vec::new();
    if let Some(instructions) = input.instructions
        && !instructions.is_empty()
    {
        // Emit stable static prefix (Base, Session, Task) followed by ephemeral nudges (ADR-0052, [INV-INST-03])
        let static_text = instructions.render_system_instructions();
        if !static_text.is_empty() {
            system.push(static_text);
        }
        let ephemeral_text = instructions.render_ephemeral();
        if !ephemeral_text.is_empty() {
            system.push(ephemeral_text);
        }
    }
    let mut contents: Vec<Value> = Vec::new();
    let mut call_names: HashMap<String, String> = HashMap::new();
    let mut signed_calls: HashSet<String> = HashSet::new();
    // Identity of the immediately preceding assistant turn, so a tool result's
    // signature match is scoped to the turn that produced its call — never to
    // any earlier turn that happened to reuse a tool name.
    let mut pending_turn: Option<TurnIdentity> = None;
    let messages = reconcile_tool_history(messages);

    for message in messages {
        if message.role == Role::System {
            // Role::System in conversation is grandfathered for backward compatibility (ADR-0052, [INV-INST-01])
            system.push(message.content);
            continue;
        }

        let role = if message.role == Role::Assistant {
            "model"
        } else {
            "user"
        };
        let new_parts = message_parts(
            message,
            &mut call_names,
            &mut signed_calls,
            &mut pending_turn,
        );

        // Coalesce consecutive same-role turns into one `contents` entry:
        // Google rejects two adjacent `user` objects, so a tool-result-then-user
        // pair must merge into a single `user` with two parts.
        if let Some(previous) = contents.last_mut()
            && previous.get("role").and_then(Value::as_str) == Some(role)
            && let Some(parts) = previous.get_mut("parts").and_then(Value::as_array_mut)
        {
            parts.extend(new_parts);
            continue;
        }
        contents.push(json!({
            "role": role,
            "parts": new_parts
        }));
    }

    let mut body = json!({ "contents": contents });
    if !system.is_empty() {
        let parts: Vec<Value> = system
            .into_iter()
            .map(|text| json!({ "text": text }))
            .collect();
        body["systemInstruction"] = json!({
            "parts": parts
        });
    }
    if let Some(tools) = google_tools(input.tool_specs) {
        body["tools"] = tools;
    }
    // Request the model's reasoning *text* so it can be routed to
    // `ReasoningDelta` / `reasoning_content` instead of being withheld. The
    // flag is only stamped for reasoning models — the API 400s otherwise.
    if input.include_thoughts {
        body["generationConfig"]["thinkingConfig"]["includeThoughts"] = json!(true);
    }
    // Reasoning depth — the resolved surface for this Gemini model, clamped to
    // its supported levels. Level (Gemini 3.x) and budget (Gemini 2.5) are
    // mutually exclusive on the wire, so only one is ever stamped. Both live
    // under `thinkingConfig` alongside `includeThoughts`.
    if let Some(thinking) = input.thinking {
        match thinking {
            GoogleThinking::Level(effort) => {
                body["generationConfig"]["thinkingConfig"]["thinkingLevel"] =
                    json!(effort.as_str());
            }
            GoogleThinking::Budget(tokens) => {
                body["generationConfig"]["thinkingConfig"]["thinkingBudget"] = json!(tokens);
            }
        }
    }
    body
}

/// Keep only tool-call history Google can replay: tool results must reference
/// a known previous assistant call, and assistant calls must have a result.
///
/// The pairing is **signature-aware**: an assistant turn whose thought
/// signatures were lost (truncated stream, cross-provider resume) degrades its
/// `functionCall` parts to plain text in [`assistant_parts`], so its tool
/// results would arrive as `functionResponse` parts with no preceding
/// `functionCall` — the exact shape Google's Anthropic bridge rejects with
/// `unexpected tool_use_id found in tool_result blocks` (reproduced live
/// against `v1internal:generateContent`). Orphaned results are degraded to
/// text here so the wire never carries an unpaired id.
/// Bypass sentinel documented by Google Vertex AI / Gemini API for functionCall parts
/// that were not generated by Gemini API (e.g. historical tool calls migrated across models).
pub const SKIP_THOUGHT_SIGNATURE_VALIDATOR: &str = "skip_thought_signature_validator";

fn reconcile_tool_history(messages: Vec<Message>) -> Vec<Message> {
    let mut known_ids = HashSet::new();
    let mut messages: Vec<Message> = messages
        .into_iter()
        .filter(|message| match message.role {
            Role::Assistant => {
                if let Some(calls) = message.tool_calls.as_ref() {
                    for call in calls {
                        known_ids.insert(call.id.clone());
                    }
                }
                !assistant_is_empty(message)
            }
            Role::Tool => message
                .tool_call_id
                .as_ref()
                .is_some_and(|id| !id.is_empty() && known_ids.contains(id)),
            _ => true,
        })
        .collect();

    let answered: HashSet<String> = messages
        .iter()
        .filter_map(|m| {
            if m.role == Role::Tool {
                m.tool_call_id.clone()
            } else {
                None
            }
        })
        .collect();
    // Assistant turns with broken/incomplete arguments that cannot be parsed as JSON
    // degrade their calls to text and must not leave answered ids behind: their tool
    // results are stripped in the same pass and re-attached as degraded user-role text so
    // the causal fact survives without an unpaired id.
    // Valid cross-model tool calls are NOT degraded: they replay natively using
    // `thoughtSignature: "skip_thought_signature_validator"`.
    let mut degraded_answered: HashSet<String> = HashSet::new();
    for m in messages.iter() {
        if m.role != Role::Assistant {
            continue;
        }
        if let Some(calls) = m.tool_calls.as_ref() {
            for call in calls.iter().filter(|call| answered.contains(&call.id)) {
                if !call.arguments.trim().is_empty()
                    && serde_json::from_str::<Value>(call.arguments.trim()).is_err()
                {
                    degraded_answered.insert(call.id.clone());
                }
            }
        }
    }
    messages.retain_mut(|m| {
        if m.role != Role::Assistant {
            return true;
        }
        if let Some(calls) = m.tool_calls.as_mut() {
            calls.retain(|call| answered.contains(&call.id));
            if calls.is_empty() {
                m.tool_calls = None;
            }
        }
        !assistant_is_empty(m)
    });
    // Strip tool results whose producing call degraded to text, then splice in
    // a text-fact user message at the same position so the model still sees
    // the outcome (mirroring [`tool_result_parts`]' degradation wording).
    let mut reconciled: Vec<Message> = Vec::with_capacity(messages.len());
    for m in messages.into_iter() {
        if m.role == Role::Tool
            && let Some(id) = m.tool_call_id.as_ref()
            && degraded_answered.contains(id)
        {
            let content = m.content.clone();
            reconciled.push(Message {
                tool_call_id: None,
                role: Role::User,
                content: format!("[Tool result for call \"{id}\"]:\n{content}"),
                ..m
            });
            continue;
        }
        reconciled.push(m);
    }
    reconciled
}

fn assistant_is_empty(message: &Message) -> bool {
    message.content.is_empty()
        && message.images.as_ref().is_none_or(Vec::is_empty)
        && message.tool_calls.as_ref().is_none_or(Vec::is_empty)
}

/// Signature identity of one assistant turn. A call is replayable natively
/// only if *its own turn* carries signature material for it; matching by bare
/// tool name across turns would wrongly resurrect a result whose producing
/// call degraded to text whenever any earlier turn signed a call with the same
/// name (`read_text`, `find_files`, …).
///
/// The snapshot is taken **after** the assistant turn processed: it holds
/// exactly the ids/names this turn signed (plus the turn-fallback text
/// signature), so sibling results of the same turn all match while later
/// unsigned turns cannot borrow an earlier turn's name.
struct TurnIdentity {
    signed: HashSet<String>,
}

fn message_parts(
    message: Message,
    call_names: &mut HashMap<String, String>,
    signed_calls: &mut HashSet<String>,
    pending_turn: &mut Option<TurnIdentity>,
) -> Vec<Value> {
    match message.role {
        Role::Assistant => {
            let parts = assistant_parts(message, call_names, signed_calls);
            *pending_turn = Some(TurnIdentity {
                signed: signed_calls.clone(),
            });
            parts
        }
        Role::Tool => tool_result_parts(message, call_names, pending_turn),
        _ => user_parts(message),
    }
}

fn user_parts(message: Message) -> Vec<Value> {
    let mut parts = text_and_image_parts(message.content, message.images.unwrap_or_default(), true);
    if parts.is_empty() {
        parts.push(json!({ "text": "" }));
    }
    parts
}

fn assistant_parts(
    message: Message,
    call_names: &mut HashMap<String, String>,
    signed_calls: &mut HashSet<String>,
) -> Vec<Value> {
    let thought_signatures = thought_signatures_by_call(&message);
    let text_thought_sig = text_thought_signature(&message);
    let fallback_signature = text_thought_sig.clone();

    let sanitized_content = sanitize_assistant_content(&message.content);
    let mut parts =
        text_and_image_parts(sanitized_content, message.images.unwrap_or_default(), false);
    if let Some(signature) = &text_thought_sig
        && let Some(part) = parts
            .iter_mut()
            .find(|part| part.get("text").and_then(Value::as_str).is_some())
    {
        part["thoughtSignature"] = json!(signature);
    }
    if let Some(calls) = message.tool_calls {
        for call in calls {
            call_names.insert(call.id.clone(), call.name.clone());
            let sig = thought_signatures
                .get(&call.id)
                .or_else(|| thought_signatures.get(&call.name))
                .or(fallback_signature.as_ref());
            let signature = sig
                .cloned()
                .unwrap_or_else(|| SKIP_THOUGHT_SIGNATURE_VALIDATOR.to_string());

            let parsed = if call.arguments.trim().is_empty() {
                Some(json!({}))
            } else {
                serde_json::from_str::<Value>(call.arguments.trim()).ok()
            };

            if let Some(valid_json) = parsed {
                signed_calls.insert(call.id.clone());
                if !call.name.is_empty() {
                    signed_calls.insert(call.name.clone());
                }
                let mut function_call = json!({
                    "name": call.name,
                    "args": valid_json,
                });
                if !call.id.is_empty() {
                    function_call["id"] = json!(call.id);
                }
                let mut part = json!({ "functionCall": function_call });
                part["thoughtSignature"] = json!(signature);
                parts.push(part);
            } else {
                // Incomplete or truncated arguments from interrupted streams must never leak raw syntax debris
                parts.push(json!({
                    "text": format!("[Aborted tool \"{}\" (incomplete arguments)]", call.name)
                }));
            }
        }
    }
    if parts.is_empty() {
        parts.push(json!({ "text": "" }));
    }
    parts
}

fn sanitize_assistant_content(text: &str) -> String {
    let prefixes = ["[Called tool ", "[Executed tool "];
    if !prefixes.iter().any(|marker| text.contains(marker)) {
        return text.to_string();
    }

    let mut result = text.to_string();
    for marker in prefixes {
        while let Some(start) = result.find(marker) {
            let after_marker = &result[start + marker.len()..];
            let args_marker = "with arguments:";
            if let Some(args_rel_idx) = after_marker.find(args_marker) {
                let after_args = &after_marker[args_rel_idx + args_marker.len()..];
                let after_args_trimmed = after_args.trim_start();
                let leading_spaces = after_args.len() - after_args_trimmed.len();
                if let Some(brace_rel) = after_args_trimmed.find('{') {
                    let brace_in_result = start
                        + marker.len()
                        + args_rel_idx
                        + args_marker.len()
                        + leading_spaces
                        + brace_rel;
                    if let Some(end_brace) =
                        nuo_inference_wire::find_balanced_object(&result, brace_in_result)
                    {
                        let after_brace = &result[end_brace + 1..];
                        let trimmed_after = after_brace.trim_start();
                        if trimmed_after.starts_with(']') {
                            let offset = after_brace.len() - trimmed_after.len();
                            let end = end_brace + 1 + offset;
                            result.replace_range(start..=end, "");
                            continue;
                        }
                    }
                }
            } else if let Some(end_bracket) = after_marker.find(']') {
                let end = start + marker.len() + end_bracket;
                result.replace_range(start..=end, "");
                continue;
            }
            break;
        }
    }
    result.trim().to_string()
}

fn text_thought_signature(message: &Message) -> Option<String> {
    message
        .provider_meta
        .as_ref()
        .and_then(|meta| meta.get(TEXT_THOUGHT_SIGNATURE_META_KEY))
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn thought_signatures_by_call(message: &Message) -> HashMap<String, String> {
    message
        .provider_meta
        .as_ref()
        .and_then(|meta| meta.get(THOUGHT_SIGNATURES_META_KEY))
        .and_then(Value::as_object)
        .map(|signatures| {
            signatures
                .iter()
                .filter_map(|(id, signature)| {
                    signature
                        .as_str()
                        .map(|signature| (id.clone(), signature.to_string()))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn tool_result_parts(
    message: Message,
    call_names: &HashMap<String, String>,
    pending_turn: &Option<TurnIdentity>,
) -> Vec<Value> {
    let id = message.tool_call_id.unwrap_or_default();
    let name = call_names.get(&id).cloned().unwrap_or_default();
    // Signature match is scoped to the turn that produced this call: the turn
    // snapshot (taken before the assistant turn merged its own signatures in)
    // plus the per-call entries it just added.
    let is_signed = pending_turn.as_ref().is_some_and(|turn| {
        (!id.is_empty() && turn.signed.contains(&id))
            || (!name.is_empty() && turn.signed.contains(&name))
    });

    if is_signed {
        let mut function_response = json!({
            "name": name,
            "response": tool_response_payload(&message.content),
        });
        if !id.is_empty() {
            function_response["id"] = json!(id);
        }
        vec![json!({ "functionResponse": function_response })]
    } else {
        // Degraded tool result for unsigned / cross-provider tool calls.
        // Google's alternating role constraint expects text in the user role.
        let label = if !name.is_empty() {
            format!("[Tool result for \"{name}\"]:\n{}", message.content)
        } else if !id.is_empty() {
            format!("[Tool result for call \"{id}\"]:\n{}", message.content)
        } else {
            format!("[Tool result]:\n{}", message.content)
        };
        vec![json!({ "text": label })]
    }
}

fn text_and_image_parts(
    text: String,
    images: Vec<nuo_tool::ImagePart>,
    include_empty_text: bool,
) -> Vec<Value> {
    let mut parts = Vec::new();
    if !text.is_empty() || (include_empty_text && images.is_empty()) {
        parts.push(json!({ "text": text }));
    }
    for image in images {
        parts.push(json!({
            "inline_data": {
                "mime_type": image.mime,
                "data": image.data,
            }
        }));
    }
    parts
}

fn google_tools(tool_specs: Option<&[nuo_provider::capability::ToolSpec]>) -> Option<Value> {
    let specs = tool_specs?;
    if specs.is_empty() {
        return None;
    }
    let declarations = specs
        .iter()
        .map(|spec| {
            json!({
                "name": spec.name,
                "description": spec.description,
                "parameters": sanitize_schema(&spec.parameters),
            })
        })
        .collect::<Vec<_>>();
    Some(json!([{ "functionDeclarations": declarations }]))
}

/// Sanitize and normalize a JSON Schema into Google Gemini's OpenAPI Schema subset.
///
/// Google Gemini's REST API (`generateContent` / `streamGenerateContent`) maps
/// function declaration parameter schemas directly to the Protobuf `Schema` definition
/// (`google.ai.generativelanguage.v1beta.Schema`).
///
/// Standard JSON Schema (Draft 7 / 2020-12, TypeScript/Pydantic/MCP schemas) allows
/// keywords like `const`, `oneOf`, `allOf`, `$schema`, `additionalProperties`, `title`,
/// `default`, and array-based `type: ["string", "null"]` which Google's Protobuf JSON parser
/// strictly rejects with HTTP 400 (`Invalid JSON payload received. Unknown name "..."`).
///
/// This sanitizer recursively converts standard JSON Schema constructs into Gemini-compatible
/// Schema representations:
/// - `const: "v"` -> `enum: ["v"]`, `type: "string"`
/// - `oneOf` -> merged `enum` (if string literals) or converted to `anyOf`
/// - `allOf` -> flattened and merged into parent object
/// - `type: [T, "null"]` -> `type: T, nullable: true`
/// - `$schema`, `additionalProperties`, `title`, `default`, etc. -> stripped / migrated to description
/// - Missing `type` inferred from `properties`, `items`, or `enum`
/// - Recursive sanitation of nested `properties`, `items`, and `anyOf`.
pub fn sanitize_schema(value: &Value) -> Value {
    let Value::Object(map) = value else {
        return json!({ "type": "object" });
    };

    let mut map = map.clone();

    // 1. Flatten allOf / all_of
    if let Some(all_of) = map.remove("allOf").or_else(|| map.remove("all_of"))
        && let Value::Array(schemas) = all_of
    {
        for sub_schema in schemas {
            if let Value::Object(sub_map) = sanitize_schema(&sub_schema) {
                for (k, v) in sub_map {
                    match k.as_str() {
                        "properties" => {
                            if let Value::Object(sub_p) = v {
                                if let Some(Value::Object(p)) = map.get_mut("properties") {
                                    p.extend(sub_p);
                                } else {
                                    map.insert("properties".to_string(), Value::Object(sub_p));
                                }
                            }
                        }
                        "required" => {
                            if let Value::Array(sub_r) = v {
                                if let Some(Value::Array(r)) = map.get_mut("required") {
                                    r.extend(sub_r);
                                } else {
                                    map.insert("required".to_string(), Value::Array(sub_r));
                                }
                            }
                        }
                        "description" => {
                            map.entry("description".to_string()).or_insert(v);
                        }
                        "type" => {
                            map.entry("type".to_string()).or_insert(v);
                        }
                        _ => {
                            map.entry(k).or_insert(v);
                        }
                    }
                }
            }
        }
    }

    // 2. Handle oneOf / one_of
    if let Some(one_of) = map.remove("oneOf").or_else(|| map.remove("one_of"))
        && let Value::Array(variants) = one_of
    {
        let mut string_enums = Vec::new();
        let mut all_simple_strings = !variants.is_empty();

        for variant in &variants {
            if let Value::Object(vmap) = variant {
                if let Some(Value::String(c)) = vmap.get("const") {
                    string_enums.push(c.clone());
                } else if let Some(Value::Array(e)) = vmap.get("enum") {
                    if e.iter().all(|val| val.is_string()) {
                        for val in e {
                            if let Some(s) = val.as_str() {
                                string_enums.push(s.to_string());
                            }
                        }
                    } else {
                        all_simple_strings = false;
                        break;
                    }
                } else {
                    all_simple_strings = false;
                    break;
                }
            } else {
                all_simple_strings = false;
                break;
            }
        }

        if all_simple_strings && !string_enums.is_empty() {
            map.insert("type".to_string(), json!("string"));
            map.insert("enum".to_string(), json!(string_enums));
        } else {
            let sanitized_variants: Vec<Value> = variants.iter().map(sanitize_schema).collect();
            map.insert("anyOf".to_string(), json!(sanitized_variants));
        }
    }

    // 3. Normalize any_of -> anyOf
    if let Some(any_of) = map.remove("any_of") {
        map.entry("anyOf".to_string()).or_insert(any_of);
    }
    if let Some(Value::Array(variants)) = map.get_mut("anyOf") {
        for variant in variants.iter_mut() {
            *variant = sanitize_schema(variant);
        }
    }

    // 4. Handle const -> enum & type
    if let Some(c) = map.remove("const") {
        match c {
            Value::String(s) => {
                if !map.contains_key("enum") {
                    map.insert("enum".to_string(), json!([s]));
                }
                map.entry("type".to_string()).or_insert(json!("string"));
            }
            Value::Bool(_) => {
                map.entry("type".to_string()).or_insert(json!("boolean"));
            }
            Value::Number(n) => {
                let type_name = if n.is_i64() || n.is_u64() {
                    "integer"
                } else {
                    "number"
                };
                map.entry("type".to_string()).or_insert(json!(type_name));
            }
            Value::Null => {
                map.insert("nullable".to_string(), json!(true));
            }
            _ => {}
        }
    }

    // 5. Handle type (array of types vs string vs missing)
    if let Some(t) = map.remove("type") {
        match t {
            Value::Array(types) => {
                let mut non_null_types = Vec::new();
                let mut is_nullable = false;
                for item in types {
                    if let Some(type_str) = item.as_str() {
                        if type_str.eq_ignore_ascii_case("null") {
                            is_nullable = true;
                        } else {
                            non_null_types.push(type_str.to_lowercase());
                        }
                    }
                }
                if is_nullable {
                    map.insert("nullable".to_string(), json!(true));
                }
                if non_null_types.len() == 1 {
                    map.insert("type".to_string(), json!(non_null_types[0]));
                } else if non_null_types.len() > 1 {
                    let any_of_variants: Vec<Value> = non_null_types
                        .into_iter()
                        .map(|t_name| json!({ "type": t_name }))
                        .collect();
                    map.insert("anyOf".to_string(), json!(any_of_variants));
                }
            }
            Value::String(s) => {
                let lower = s.to_lowercase();
                if lower == "null" {
                    map.insert("nullable".to_string(), json!(true));
                } else {
                    map.insert("type".to_string(), json!(lower));
                }
            }
            _ => {}
        }
    }

    // Infer type if missing
    if !map.contains_key("type") && !map.contains_key("anyOf") {
        if map.contains_key("properties") {
            map.insert("type".to_string(), json!("object"));
        } else if map.contains_key("items") {
            map.insert("type".to_string(), json!("array"));
        } else if let Some(Value::Array(e)) = map.get("enum") {
            if e.iter().all(|val| val.is_string()) {
                map.insert("type".to_string(), json!("string"));
            }
        } else {
            map.insert("type".to_string(), json!("object"));
        }
    }

    // 6. Handle properties
    if let Some(Value::Object(props_map)) = map.get_mut("properties") {
        for (_, prop_schema) in props_map.iter_mut() {
            *prop_schema = sanitize_schema(prop_schema);
        }
    }

    // 7. Handle required
    if let Some(req) = map.get_mut("required") {
        if let Value::Array(req_arr) = req {
            let mut valid_req = Vec::new();
            for item in req_arr.iter() {
                if let Some(s) = item.as_str()
                    && !valid_req.contains(&s.to_string())
                {
                    valid_req.push(s.to_string());
                }
            }
            if valid_req.is_empty() {
                map.remove("required");
            } else {
                *req = json!(valid_req);
            }
        } else {
            map.remove("required");
        }
    }

    // 8. Handle items
    if let Some(items) = map.get_mut("items") {
        match items {
            Value::Object(_) => {
                *items = sanitize_schema(items);
            }
            Value::Array(item_arr) => {
                let sanitized_arr: Vec<Value> = item_arr.iter().map(sanitize_schema).collect();
                *items = json!({ "anyOf": sanitized_arr });
            }
            _ => {
                map.remove("items");
            }
        }
    }

    // 9. Handle enum (must be string array)
    if let Some(e) = map.get_mut("enum") {
        if let Value::Array(arr) = e {
            let mut string_enums = Vec::new();
            for item in arr.iter() {
                let s = match item {
                    Value::String(s) => s.clone(),
                    Value::Number(n) => n.to_string(),
                    Value::Bool(b) => b.to_string(),
                    _ => continue,
                };
                if !string_enums.contains(&s) {
                    string_enums.push(s);
                }
            }
            if string_enums.is_empty() {
                map.remove("enum");
            } else {
                *e = json!(string_enums);
            }
        } else {
            map.remove("enum");
        }
    }

    // 10. Handle title and description
    if let Some(title) = map.remove("title")
        && !map.contains_key("description")
        && let Value::String(t_str) = title
        && !t_str.is_empty()
    {
        map.insert("description".to_string(), json!(t_str));
    }

    // 11. Retain ONLY supported Gemini Schema fields
    let allowed_keys = [
        "type",
        "format",
        "description",
        "nullable",
        "enum",
        "properties",
        "required",
        "items",
        "minItems",
        "maxItems",
        "anyOf",
        "example",
    ];

    map.retain(|k, _| allowed_keys.contains(&k.as_str()));

    Value::Object(map)
}

fn tool_response_payload(text: &str) -> Value {
    match serde_json::from_str::<Value>(text) {
        Ok(value) if value.is_object() => value,
        Ok(value) => json!({ "result": value }),
        Err(_) => json!({ "content": text }),
    }
}

/// The non-streaming generateContent URL.
pub fn url(base_url: &str, model: &str) -> String {
    format!("{base_url}/models/{model}:generateContent")
}

/// The streaming streamGenerateContent URL (SSE framing via `alt=sse`).
pub fn stream_url(base_url: &str, model: &str) -> String {
    format!("{base_url}/models/{model}:streamGenerateContent?alt=sse")
}

/// The maximum `thinkingBudget` a Gemini 2.5 model accepts, in tokens — the cap
/// [`nuo_provider::Effort::gemini_thinking_budget`] buckets against. Gemini 2.5 Flash tops
/// out at `24576`; Pro at `32768`; Flash-Lite (floor `512`) also at `24576`.
/// Any other model id (Gemini 3.x, non-reasoning) returns `0`, signaling "no
/// budget surface" so [`resolve_thinking`] never constructs a `Budget` for it.
///
/// Kept free-form on the model id rather than a registry field because the cap
/// is an intrinsic property of the model family, not a capability the static
/// `Model` carries today, and it only matters for the 2.5 budget path.
pub fn max_thinking_budget(model: &str) -> u32 {
    if model.starts_with("gemini-2.5-pro") {
        32768
    } else if model.starts_with("gemini-2.5") {
        // 2.5-flash and 2.5-flash-lite share the 24576 cap.
        24576
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_body_input<'a>(
        tool_specs: Option<&'a [nuo_provider::capability::ToolSpec]>,
        include_thoughts: bool,
        thinking: Option<GoogleThinking>,
    ) -> BodyInput<'a> {
        BodyInput {
            instructions: None,
            tool_specs,
            include_thoughts,
            thinking,
        }
    }

    /// Route capabilities with only the vision declaration varied.
    fn caps_with_vision(vision: Option<bool>) -> nuo_provider::ModelCapabilities {
        nuo_provider::ModelCapabilities {
            family: "google".into(),
            context_window: 1_000_000,
            max_output_tokens: None,
            thinking: nuo_provider::ReasoningSupport::None,
            tool_call: true,
            vision,
            effort_levels: Vec::new(),
            effort_entities: Vec::new(),
        }
    }

    fn image_message(text: &str) -> Message {
        Message::new(Role::User, text).with_images(vec![nuo_tool::ImagePart {
            mime: "image/png".to_string(),
            data: "aGk=".to_string(),
        }])
    }

    #[test]
    fn declared_text_only_route_projects_inline_data_away() {
        // Google's `inline_data` used to be emitted unconditionally, so a route
        // without image input failed the whole turn.
        let body = body_with_capabilities(
            "gemini-test",
            vec![image_message("look")],
            test_body_input(None, false, None),
            &caps_with_vision(Some(false)),
        );

        let parts = body["contents"][0]["parts"].as_array().unwrap();
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0]["text"], "look");
        assert!(body["contents"][0]["parts"][0].get("inline_data").is_none());
    }

    #[test]
    fn undeclared_route_keeps_inline_data() {
        let body = body_with_capabilities(
            "gemini-test",
            vec![image_message("look")],
            test_body_input(None, false, None),
            &caps_with_vision(None),
        );

        let parts = body["contents"][0]["parts"].as_array().unwrap();
        assert_eq!(parts[1]["inline_data"]["mime_type"], "image/png");
        assert_eq!(parts[1]["inline_data"]["data"], "aGk=");
    }

    #[test]
    fn preserves_system_harness_context() {
        let body = body(
            vec![
                Message::new(Role::System, "pursuit and tools"),
                Message::new(Role::User, "continue"),
            ],
            test_body_input(None, false, None),
        );

        assert_eq!(
            body["systemInstruction"]["parts"][0]["text"],
            "pursuit and tools"
        );
        assert_eq!(body["contents"][0]["role"], "user");
    }

    #[test]
    fn fallback_tool_results_are_user_context() {
        let body = body(
            vec![
                Message::new(Role::Assistant, "{\"tool\":\"read_text\"}"),
                Message::new(Role::Tool, "file contents"),
                Message::new(Role::User, "next"),
            ],
            test_body_input(None, false, None),
        );

        assert_eq!(body["contents"][1]["role"], "user");
        assert_eq!(body["contents"][1]["parts"][0]["text"], "next");
    }

    #[test]
    fn declares_google_function_tools() {
        let body = body(
            vec![Message::new(Role::User, "list files")],
            test_body_input(
                Some(&[nuo_provider::capability::ToolSpec {
                    name: "list_dir".to_string(),
                    description: "List files".to_string(),
                    parameters: json!({
                        "type": "object",
                        "properties": {"path": {"type": "string"}},
                        "required": ["path"]
                    }),
                }]),
                false,
                None,
            ),
        );

        assert_eq!(
            body["tools"][0]["functionDeclarations"][0]["name"],
            "list_dir"
        );
        assert_eq!(
            body["tools"][0]["functionDeclarations"][0]["parameters"]["properties"]["path"]["type"],
            "string"
        );
    }

    #[test]
    fn include_thoughts_stamps_thinking_config() {
        let body = body(
            vec![Message::new(Role::User, "why is the sky blue?")],
            test_body_input(None, true, None),
        );
        // Reasoning text is only surfaced when explicitly requested, and the
        // flag lives under generationConfig.thinkingConfig.
        assert_eq!(
            body["generationConfig"]["thinkingConfig"]["includeThoughts"],
            true
        );
    }

    #[test]
    fn include_thoughts_off_omits_thinking_config() {
        let body = body(
            vec![Message::new(Role::User, "hi")],
            test_body_input(None, false, None),
        );
        assert!(body.get("generationConfig").is_none());
    }

    #[test]
    fn replays_tool_calls_and_results_as_function_parts() {
        let call = nuo_tool::ToolCall {
            id: "call_1".to_string(),
            name: "list_dir".to_string(),
            arguments: r#"{"path":"."}"#.to_string(),
        };
        let mut provider_meta = serde_json::Map::new();
        provider_meta.insert(
            THOUGHT_SIGNATURES_META_KEY.to_string(),
            json!({ "call_1": "sig-1" }),
        );
        provider_meta.insert(
            TEXT_THOUGHT_SIGNATURE_META_KEY.to_string(),
            json!("text-sig"),
        );
        let body = body(
            vec![
                Message::new(Role::User, "inspect"),
                Message {
                    tool_calls: Some(vec![call.clone()]),
                    provider_meta: Some(provider_meta),
                    ..Message::new(Role::Assistant, "checking")
                },
                Message::tool_result(&call, "Cargo.toml"),
            ],
            test_body_input(None, false, None),
        );

        assert_eq!(
            body["contents"][1]["parts"][1]["functionCall"]["name"],
            "list_dir"
        );
        assert_eq!(
            body["contents"][1]["parts"][1]["functionCall"]["args"]["path"],
            "."
        );
        assert_eq!(
            body["contents"][1]["parts"][0]["thoughtSignature"],
            "text-sig"
        );
        assert_eq!(body["contents"][1]["parts"][1]["thoughtSignature"], "sig-1");
        assert_eq!(
            body["contents"][2]["parts"][0]["functionResponse"]["name"],
            "list_dir"
        );
        assert_eq!(
            body["contents"][2]["parts"][0]["functionResponse"]["response"]["content"],
            "Cargo.toml"
        );
    }

    #[test]
    fn strips_unanswered_tool_calls_and_orphan_results() {
        let answered = nuo_tool::ToolCall {
            id: "answered".to_string(),
            name: "list_dir".to_string(),
            arguments: "{}".to_string(),
        };
        let unanswered = nuo_tool::ToolCall {
            id: "unanswered".to_string(),
            name: "grep".to_string(),
            arguments: "{}".to_string(),
        };
        let mut provider_meta = serde_json::Map::new();
        provider_meta.insert(
            THOUGHT_SIGNATURES_META_KEY.to_string(),
            json!({ "answered": "sig-answered" }),
        );
        let body = body(
            vec![
                Message::new(Role::User, "go"),
                Message {
                    tool_calls: Some(vec![answered.clone(), unanswered]),
                    provider_meta: Some(provider_meta),
                    ..Message::new(Role::Assistant, "")
                },
                Message::tool_result(&answered, "{}"),
                Message {
                    tool_call_id: Some("orphan".to_string()),
                    ..Message::new(Role::Tool, "ignored")
                },
            ],
            test_body_input(None, false, None),
        );

        let parts = body["contents"][1]["parts"].as_array().unwrap();
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0]["functionCall"]["id"], "answered");
        assert_eq!(parts[0]["thoughtSignature"], "sig-answered");
        assert_eq!(
            body["contents"][2]["parts"][0]["functionResponse"]["id"],
            "answered"
        );
    }

    #[test]
    fn replays_unsigned_assistant_turn_tool_results_with_skip_thought_signature_validator() {
        let lost_call = nuo_tool::ToolCall {
            id: "toolu_vrtx_01LostSignature".to_string(),
            name: "find_files".to_string(),
            arguments: r#"{"path":"docs"}"#.to_string(),
        };
        let kept_call = nuo_tool::ToolCall {
            id: "toolu_vrtx_01KeptSignature".to_string(),
            name: "code_query".to_string(),
            arguments: "{}".to_string(),
        };
        // Turn 1 keeps its signature material; turn 2 lost provider_meta.
        let mut signed_meta = serde_json::Map::new();
        signed_meta.insert(
            THOUGHT_SIGNATURES_META_KEY.to_string(),
            json!({ "toolu_vrtx_01KeptSignature": "sig-kept" }),
        );
        let body = body(
            vec![
                Message::new(Role::User, "investigate"),
                Message {
                    tool_calls: Some(vec![kept_call.clone()]),
                    provider_meta: Some(signed_meta),
                    ..Message::new(Role::Assistant, "planning")
                },
                Message::tool_result(&kept_call, "outline text"),
                Message {
                    tool_calls: Some(vec![lost_call.clone()]),
                    provider_meta: None,
                    ..Message::new(Role::Assistant, "")
                },
                Message::tool_result(&lost_call, "file list"),
                Message::new(Role::User, "continue"),
            ],
            test_body_input(None, false, None),
        );

        // Turn 1's signed pair replays natively
        let kept_parts = body["contents"][1]["parts"].as_array().unwrap();
        assert_eq!(
            kept_parts[1]["functionCall"]["id"],
            "toolu_vrtx_01KeptSignature"
        );
        assert_eq!(kept_parts[1]["thoughtSignature"], "sig-kept");

        // Turn 2's unsigned call replays natively with the documented Google bypass sentinel
        let lost_parts = body["contents"][3]["parts"].as_array().unwrap();
        assert_eq!(
            lost_parts[0]["functionCall"]["id"],
            "toolu_vrtx_01LostSignature"
        );
        assert_eq!(
            lost_parts[0]["thoughtSignature"],
            SKIP_THOUGHT_SIGNATURE_VALIDATOR
        );

        // Turn 2's tool result replays natively as functionResponse
        let tail = body["contents"][4]["parts"].as_array().unwrap();
        assert_eq!(
            tail[0]["functionResponse"]["id"],
            "toolu_vrtx_01LostSignature"
        );
        assert_eq!(
            tail[0]["functionResponse"]["response"]["content"],
            "file list"
        );
        assert_eq!(tail[1]["text"], "continue");
    }

    #[test]
    fn text_signature_alone_still_replays_calls_and_results() {
        // A turn with only a text-part thought signature (no per-call map)
        // falls back to signing every call on the turn, so its results stay
        // native and must not be degraded.
        let call = nuo_tool::ToolCall {
            id: "toolu_vrtx_01TextSigOnly".to_string(),
            name: "read_text".to_string(),
            arguments: "{}".to_string(),
        };
        let mut text_sig_meta = serde_json::Map::new();
        text_sig_meta.insert(
            TEXT_THOUGHT_SIGNATURE_META_KEY.to_string(),
            json!("sig-text-fallback"),
        );
        let body = body(
            vec![
                Message::new(Role::User, "read it"),
                Message {
                    tool_calls: Some(vec![call.clone()]),
                    provider_meta: Some(text_sig_meta),
                    ..Message::new(Role::Assistant, "")
                },
                Message::tool_result(&call, "bytes"),
            ],
            test_body_input(None, false, None),
        );

        let parts = body["contents"][1]["parts"].as_array().unwrap();
        assert_eq!(parts[0]["functionCall"]["id"], "toolu_vrtx_01TextSigOnly");
        let result = body["contents"][2]["parts"][0]["functionResponse"].clone();
        assert_eq!(result["id"], "toolu_vrtx_01TextSigOnly");
    }

    #[test]
    fn unsigned_tool_call_uses_skip_sentinel_and_does_not_borrow_earlier_signature_by_name() {
        // ADR-0046: Earlier turn signed `read_text`. A later turn ran `read_text`
        // without signatures. The later result must NOT borrow the earlier turn's
        // signature by name; it must use the documented skip sentinel.
        let unsigned_dup = nuo_tool::ToolCall {
            id: "call_unsigned_dup".to_string(),
            name: "read_text".to_string(), // same name as the signed earlier call
            arguments: "{}".to_string(),
        };
        let signed_dup = nuo_tool::ToolCall {
            id: "call_signed_dup".to_string(),
            name: "read_text".to_string(),
            arguments: "{}".to_string(),
        };
        let mut signed_meta = serde_json::Map::new();
        signed_meta.insert(
            THOUGHT_SIGNATURES_META_KEY.to_string(),
            json!({ "call_signed_dup": "sig-earlier" }),
        );
        let body = body(
            vec![
                Message::new(Role::User, "go"),
                Message {
                    tool_calls: Some(vec![signed_dup.clone()]),
                    provider_meta: Some(signed_meta),
                    ..Message::new(Role::Assistant, "")
                },
                Message::tool_result(&signed_dup, "earlier"),
                Message {
                    tool_calls: Some(vec![unsigned_dup.clone()]),
                    provider_meta: None, // this turn lost its signatures
                    ..Message::new(Role::Assistant, "")
                },
                Message::tool_result(&unsigned_dup, "later output"),
            ],
            test_body_input(None, false, None),
        );

        // Turn 1 carries the earlier signature
        let earlier_parts = body["contents"][1]["parts"].as_array().unwrap();
        assert_eq!(earlier_parts[0]["thoughtSignature"], "sig-earlier");

        // The unsigned later call replays natively with the skip sentinel rather than borrowing sig-earlier
        let later_parts = body["contents"][3]["parts"].as_array().unwrap();
        assert_eq!(later_parts[0]["functionCall"]["id"], "call_unsigned_dup");
        assert_eq!(
            later_parts[0]["thoughtSignature"],
            SKIP_THOUGHT_SIGNATURE_VALIDATOR
        );
        let tail = body["contents"][4]["parts"].as_array().unwrap();
        assert_eq!(tail[0]["functionResponse"]["id"], "call_unsigned_dup");
    }

    #[test]
    fn replays_unsigned_foreign_tool_calls_with_skip_thought_signature_validator() {
        let call = nuo_tool::ToolCall {
            id: "call_foreign".to_string(),
            name: "write_todos".to_string(),
            arguments: r#"{"items":[{"content":"design","status":"in_progress"}]}"#.to_string(),
        };
        // Unsigned assistant message (e.g. from Claude/GPT or DeepSeek, no provider_meta)
        let body = body(
            vec![
                Message::new(Role::User, "create tasks"),
                Message {
                    tool_calls: Some(vec![call.clone()]),
                    provider_meta: None,
                    ..Message::new(Role::Assistant, "Planning tasks")
                },
                Message::tool_result(&call, "Todo list updated"),
                Message::new(Role::User, "proceed"),
            ],
            test_body_input(None, false, None),
        );

        // Foreign tool call replays natively with documented Google bypass sentinel
        let model_parts = body["contents"][1]["parts"].as_array().unwrap();
        assert_eq!(model_parts.len(), 2);
        assert_eq!(model_parts[0]["text"], "Planning tasks");
        assert_eq!(model_parts[1]["functionCall"]["name"], "write_todos");
        assert_eq!(
            model_parts[1]["thoughtSignature"],
            SKIP_THOUGHT_SIGNATURE_VALIDATOR
        );

        // Tool result replays natively as functionResponse, followed by user prompt
        assert_eq!(body["contents"][2]["role"], "user");
        let user_parts = body["contents"][2]["parts"].as_array().unwrap();
        assert_eq!(user_parts[0]["functionResponse"]["name"], "write_todos");
        assert_eq!(
            user_parts[0]["functionResponse"]["response"]["content"],
            "Todo list updated"
        );
        assert_eq!(user_parts[1]["text"], "proceed");
    }

    #[test]
    fn degrades_incomplete_truncated_unsigned_tool_call_without_syntax_leaks() {
        let broken_call = nuo_tool::ToolCall {
            id: "call_broken".to_string(),
            name: "read_text".to_string(),
            arguments: r#"{"limit":120,"offset":1,"path": "docs/adr/018"#.to_string(),
        };
        let body = body(
            vec![
                Message::new(Role::User, "read file"),
                Message {
                    tool_calls: Some(vec![broken_call.clone()]),
                    provider_meta: None,
                    ..Message::new(Role::Assistant, "Inspecting path")
                },
                Message::tool_result(&broken_call, "Operation aborted"),
            ],
            test_body_input(None, false, None),
        );

        let model_parts = body["contents"][1]["parts"].as_array().unwrap();
        assert_eq!(model_parts.len(), 2);
        assert_eq!(model_parts[0]["text"], "Inspecting path");
        // Incomplete / truncated arguments must not leak raw syntax debris
        assert_eq!(
            model_parts[1]["text"],
            r#"[Aborted tool "read_text" (incomplete arguments)]"#
        );
        assert!(model_parts[1].get("functionCall").is_none());
    }

    #[test]
    fn replays_mixed_signed_and_unsigned_tool_calls_natively() {
        let signed_call = nuo_tool::ToolCall {
            id: "call_signed".to_string(),
            name: "list_dir".to_string(),
            arguments: r#"{"path":"."}"#.to_string(),
        };
        let unsigned_call = nuo_tool::ToolCall {
            id: "call_unsigned".to_string(),
            name: "default_api:execute_command".to_string(),
            arguments: r#"{"command":"cargo test"}"#.to_string(),
        };
        let mut provider_meta = serde_json::Map::new();
        provider_meta.insert(
            THOUGHT_SIGNATURES_META_KEY.to_string(),
            json!({ "call_signed": "sig-signed-call" }),
        );

        let body = body(
            vec![
                Message::new(Role::User, "run task"),
                Message {
                    tool_calls: Some(vec![signed_call.clone(), unsigned_call.clone()]),
                    provider_meta: Some(provider_meta),
                    ..Message::new(Role::Assistant, "Running tools")
                },
                Message::tool_result(&signed_call, "Cargo.toml"),
                Message::tool_result(&unsigned_call, "tests passed"),
            ],
            test_body_input(None, false, None),
        );

        let model_parts = body["contents"][1]["parts"].as_array().unwrap();
        assert_eq!(model_parts.len(), 3);
        assert_eq!(model_parts[0]["text"], "Running tools");
        // Signed call remains native functionCall with signature
        assert_eq!(model_parts[1]["functionCall"]["name"], "list_dir");
        assert_eq!(model_parts[1]["thoughtSignature"], "sig-signed-call");
        // Unsigned call replays natively with skip sentinel
        assert_eq!(
            model_parts[2]["functionCall"]["name"],
            "default_api:execute_command"
        );
        assert_eq!(
            model_parts[2]["thoughtSignature"],
            SKIP_THOUGHT_SIGNATURE_VALIDATOR
        );

        let user_parts = body["contents"][2]["parts"].as_array().unwrap();
        assert_eq!(user_parts.len(), 2);
        // Both results are native functionResponses
        assert_eq!(user_parts[0]["functionResponse"]["name"], "list_dir");
        assert_eq!(
            user_parts[1]["functionResponse"]["name"],
            "default_api:execute_command"
        );
    }

    #[test]
    fn sanitizes_legacy_mimicked_tool_call_text_from_assistant_prose() {
        let body = body(
            vec![
                Message::new(Role::User, "do something"),
                Message::new(
                    Role::Assistant,
                    "[Called tool read_text with arguments: {\"limit\": 90, \"offset\": 100}]",
                ),
                Message::new(Role::User, "pls go on"),
            ],
            test_body_input(None, false, None),
        );

        let model_turn = &body["contents"][1];
        let model_parts = model_turn["parts"].as_array().unwrap();
        assert_eq!(model_parts[0]["text"], "");
    }

    #[test]
    fn sanitizes_legacy_mimicked_tool_call_with_inner_brackets() {
        let body = body(
            vec![
                Message::new(Role::User, "save draft"),
                Message::new(
                    Role::Assistant,
                    "[Called tool write_file with arguments: {\"content\": \"# Doc\\n[ADR-0080]: link\\n[item1, item2]\"}]",
                ),
                Message::new(Role::User, "continue"),
            ],
            test_body_input(None, false, None),
        );

        let model_turn = &body["contents"][1];
        let model_parts = model_turn["parts"].as_array().unwrap();
        // Inner brackets in the arguments object must never cause premature truncation
        assert_eq!(model_parts[0]["text"], "");
    }

    #[test]
    fn sanitizes_executed_tool_facts_with_markdown_links_and_brackets() {
        let prose = r#"I have recorded the architecture decision:

[Executed tool "write_file" with arguments: {"content":"--- id: ADR-0080 title: \"Time-Varying Rate Cards\"\nscope: provider/nuo-provider\n---\n# 0080\nSee [ADR-0065] for details."}]"#;
        let body = body(
            vec![
                Message::new(Role::User, "write adr"),
                Message::new(Role::Assistant, prose),
                Message::new(Role::User, "next step"),
            ],
            test_body_input(None, false, None),
        );

        let model_turn = &body["contents"][1];
        let model_parts = model_turn["parts"].as_array().unwrap();
        // The historical executed fact is cleanly stripped from prose so the model is not prompted to mimic it
        assert_eq!(
            model_parts[0]["text"],
            "I have recorded the architecture decision:"
        );
    }

    #[test]
    fn resolve_thinking_level_for_gemini_3x() {
        // Gemini 3.x uses a level ladder; each rung maps to thinkingLevel,
        // clamping down from unsupported depths.
        use nuo_provider::effort::Effort;
        let gemini_level: &[Effort] = &[Effort::Minimal, Effort::Low, Effort::Medium, Effort::High];
        let level: Vec<nuo_provider::EffortLevel> =
            gemini_level.iter().copied().map(Into::into).collect();
        assert_eq!(
            resolve_thinking(Some(Effort::High), &level, 0),
            Some(GoogleThinking::Level(Effort::High))
        );
        // max/xhigh clamp down to high (Gemini 3.x has no deeper rung).
        assert_eq!(
            resolve_thinking(Some(Effort::Max), &level, 0),
            Some(GoogleThinking::Level(Effort::High))
        );
        assert_eq!(
            resolve_thinking(Some(Effort::Minimal), &level, 0),
            Some(GoogleThinking::Level(Effort::Minimal))
        );
        // No override → server default.
        assert_eq!(resolve_thinking(None, &level, 0), None);
    }

    #[test]
    fn resolve_thinking_budget_for_gemini_2_5() {
        // Gemini 2.5 uses a budget ladder; rungs map to token buckets against
        // the model's max (Flash: 24576).
        use nuo_provider::effort::Effort;
        let gemini_budget: &[Effort] = &[
            Effort::Minimal,
            Effort::Low,
            Effort::Medium,
            Effort::High,
            Effort::Max,
        ];
        let budget: Vec<nuo_provider::EffortLevel> =
            gemini_budget.iter().copied().map(Into::into).collect();
        assert_eq!(
            resolve_thinking(Some(Effort::Medium), &budget, 24576),
            Some(GoogleThinking::Budget(12288))
        );
        assert_eq!(
            resolve_thinking(Some(Effort::Max), &budget, 24576),
            Some(GoogleThinking::Budget(24576))
        );
        // Pro's larger cap scales the bucket.
        assert_eq!(
            resolve_thinking(Some(Effort::Medium), &budget, 32768),
            Some(GoogleThinking::Budget(16384))
        );
    }

    #[test]
    fn resolve_thinking_empty_ladder_is_no_override() {
        // A non-reasoning / unknown model (empty ladder) never stamps thinking.
        assert_eq!(
            resolve_thinking(Some(nuo_provider::Effort::High), &[], 24576),
            None
        );
    }

    #[test]
    fn body_stamps_thinking_level_and_budget() {
        // Level → thinkingLevel string.
        let level_body = body(
            vec![Message::new(Role::User, "think")],
            test_body_input(
                None,
                false,
                Some(GoogleThinking::Level(nuo_provider::Effort::Medium)),
            ),
        );
        assert_eq!(
            level_body["generationConfig"]["thinkingConfig"]["thinkingLevel"],
            "medium"
        );
        // Budget → thinkingBudget integer.
        let budget_body = body(
            vec![Message::new(Role::User, "think")],
            test_body_input(None, false, Some(GoogleThinking::Budget(8192))),
        );
        assert_eq!(
            budget_body["generationConfig"]["thinkingConfig"]["thinkingBudget"],
            8192
        );
    }

    #[test]
    fn max_thinking_budget_per_gemini_2_5_model() {
        assert_eq!(max_thinking_budget("gemini-2.5-pro"), 32768);
        assert_eq!(max_thinking_budget("gemini-2.5-flash"), 24576);
        assert_eq!(max_thinking_budget("gemini-2.5-flash-lite"), 24576);
        // Gemini 3.x and non-reasoning models have no budget surface.
        assert_eq!(max_thinking_budget("gemini-3.7-flash"), 0);
        assert_eq!(max_thinking_budget("gemini-3.5-flash"), 0);
        assert_eq!(max_thinking_budget("gemini-2.0-flash"), 0);
    }

    #[test]
    fn sanitize_schema_converts_const_to_enum_and_type() {
        let input = json!({
            "const": "execute_task"
        });
        let sanitized = sanitize_schema(&input);
        assert_eq!(sanitized["type"], "string");
        assert_eq!(sanitized["enum"], json!(["execute_task"]));
        assert!(sanitized.get("const").is_none());
    }

    #[test]
    fn sanitize_schema_converts_one_of_literals_to_flat_enum() {
        let input = json!({
            "oneOf": [
                { "const": "list" },
                { "const": "kill" },
                { "const": "kill_all" }
            ]
        });
        let sanitized = sanitize_schema(&input);
        assert_eq!(sanitized["type"], "string");
        assert_eq!(sanitized["enum"], json!(["list", "kill", "kill_all"]));
        assert!(sanitized.get("oneOf").is_none());
        assert!(sanitized.get("anyOf").is_none());
    }

    #[test]
    fn sanitize_schema_converts_one_of_objects_to_any_of() {
        let input = json!({
            "one_of": [
                {
                    "type": "object",
                    "properties": {
                        "kind": { "const": "text" },
                        "content": { "type": "string" }
                    },
                    "required": ["kind", "content"]
                },
                {
                    "type": "object",
                    "properties": {
                        "kind": { "const": "image" },
                        "url": { "type": "string" }
                    },
                    "required": ["kind", "url"]
                }
            ]
        });
        let sanitized = sanitize_schema(&input);
        assert!(sanitized.get("one_of").is_none());
        assert!(sanitized.get("oneOf").is_none());
        let any_of = sanitized["anyOf"].as_array().expect("must be array");
        assert_eq!(any_of.len(), 2);
        assert_eq!(any_of[0]["type"], "object");
        assert_eq!(any_of[0]["properties"]["kind"]["enum"], json!(["text"]));
        assert!(any_of[0]["properties"]["kind"].get("const").is_none());
        assert_eq!(any_of[1]["properties"]["kind"]["enum"], json!(["image"]));
    }

    #[test]
    fn sanitize_schema_handles_all_of_merging() {
        let input = json!({
            "allOf": [
                {
                    "type": "object",
                    "properties": { "id": { "type": "string" } },
                    "required": ["id"]
                },
                {
                    "type": "object",
                    "properties": { "name": { "type": "string" } },
                    "required": ["name"]
                }
            ]
        });
        let sanitized = sanitize_schema(&input);
        assert_eq!(sanitized["type"], "object");
        assert!(sanitized["properties"]["id"].is_object());
        assert!(sanitized["properties"]["name"].is_object());
        assert_eq!(sanitized["required"], json!(["id", "name"]));
        assert!(sanitized.get("allOf").is_none());
    }

    #[test]
    fn sanitize_schema_handles_nullable_type_array() {
        let input = json!({
            "type": ["string", "null"],
            "description": "An optional string property",
            "title": "Ignored Title"
        });
        let sanitized = sanitize_schema(&input);
        assert_eq!(sanitized["type"], "string");
        assert_eq!(sanitized["nullable"], true);
        assert_eq!(sanitized["description"], "An optional string property");
        assert!(sanitized.get("title").is_none());
    }

    #[test]
    fn sanitize_schema_strips_forbidden_meta_fields() {
        let input = json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "properties": {
                "count": { "type": "integer", "default": 0, "minimum": 0 }
            },
            "additionalProperties": false,
            "patternProperties": { "^x-": { "type": "string" } }
        });
        let sanitized = sanitize_schema(&input);
        assert_eq!(sanitized["type"], "object");
        assert!(sanitized["properties"]["count"].is_object());
        assert!(sanitized.get("$schema").is_none());
        assert!(sanitized.get("additionalProperties").is_none());
        assert!(sanitized.get("patternProperties").is_none());
        assert!(sanitized["properties"]["count"].get("default").is_none());
        assert!(sanitized["properties"]["count"].get("minimum").is_none());
    }

    #[test]
    fn sanitize_schema_handles_deeply_nested_subagent_items_with_const() {
        // Reproduce exact wire path:
        // parameters.properties[0].value.items.one_of[0].properties[0].value (const)
        let input = json!({
            "type": "object",
            "properties": {
                "Subagents": {
                    "type": "array",
                    "items": {
                        "one_of": [
                            {
                                "type": "object",
                                "properties": {
                                    "TypeName": {
                                        "type": "string",
                                        "const": "researcher"
                                    },
                                    "Role": {
                                        "type": "string"
                                    }
                                },
                                "required": ["TypeName", "Role"]
                            }
                        ]
                    }
                }
            }
        });
        let sanitized = sanitize_schema(&input);
        assert_eq!(sanitized["type"], "object");
        let subagents = &sanitized["properties"]["Subagents"];
        assert_eq!(subagents["type"], "array");
        let items = &subagents["items"];
        assert!(items.get("one_of").is_none());
        let any_of = items["anyOf"].as_array().expect("anyOf array");
        assert_eq!(any_of.len(), 1);
        let type_name_prop = &any_of[0]["properties"]["TypeName"];
        assert_eq!(type_name_prop["type"], "string");
        assert_eq!(type_name_prop["enum"], json!(["researcher"]));
        assert!(type_name_prop.get("const").is_none());
    }
}
