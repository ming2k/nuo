//! Google Google — request construction.
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
//! - Auth: a plain API key rides in the `?key=` query param. A workspace-scoped
//!   console bearer is header-only (`x-goog-api-key`, ADR-0269) — the relay
//!   rejects it as a query param, and a token in a URL reaches access logs.
//! - Body:
//!   `{contents: [{role, parts}], systemInstruction?: {parts:[{text}]}, tools?}`.
//! - Roles are only `user` and `model` (assistant). Tool calls are assistant
//!   `functionCall` parts; tool results are user `functionResponse` parts.

use nuo_model_codec::{Message, Role};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};

use super::response::{TEXT_THOUGHT_SIGNATURE_META_KEY, THOUGHT_SIGNATURES_META_KEY};

/// The resolved reasoning-depth directive to stamp into `thinkingConfig` — the
/// wire form of a channel's [`nuo_model_codec::Effort`] override once it has been
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
    Level(nuo_model_codec::Effort),
    /// `thinkingBudget` for a Gemini 2.5 model: a token count in the model's
    /// supported range. `0` disables thinking (Flash/Lite only — Pro's floor is
    /// `128`, so the resolver only emits `0` when the model's ladder allows
    /// [`nuo_model_codec::Effort::None`]).
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
    effort: Option<nuo_model_codec::Effort>,
    effort_levels: &[nuo_model_codec::EffortLevel],
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
    let known: Vec<nuo_model_codec::Effort> = effort_levels
        .iter()
        .filter_map(nuo_model_codec::EffortLevel::as_known)
        .collect();
    let clamped = requested.clamp_to(&known);
    // Decide the surface from the ladder's deepest rung: the budget ladder tops
    // out at `max`, the level ladder at `high`. This keeps the model-specific
    // choice in one place and never inspects the free-form model id string.
    let is_budget = known.contains(&nuo_model_codec::Effort::Max);
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
    pub instructions: Option<&'a nuo_model_codec::InstructionBundle>,
    pub tool_specs: Option<&'a [nuo_model_codec::ToolSpec]>,
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
/// image parts for this route (ADR-0230). The provider calls this; [`body`]
/// remains the entry point for standalone callers and tests that have no
/// channel view. `model` is the wire id, used only to label the projection's
/// diagnostic log.
pub fn body_with_capabilities(
    model: &str,
    messages: Vec<Message>,
    input: BodyInput<'_>,
    capabilities: &nuo_model_codec::ModelCapabilities,
) -> Value {
    let (messages, _dropped_images) =
        crate::vision::project_images_for_route(model, messages, capabilities);
    body_inner(messages, input)
}

fn body_inner(messages: Vec<Message>, input: BodyInput<'_>) -> Value {
    let mut system = Vec::new();
    if let Some(instructions) = input.instructions
        && !instructions.is_empty()
    {
        let text = instructions.render_combined();
        if !text.is_empty() {
            system.push(text);
        }
    }
    let mut contents: Vec<Value> = Vec::new();
    let mut call_names: HashMap<String, String> = HashMap::new();
    let mut signed_calls: HashSet<String> = HashSet::new();
    let messages = reconcile_tool_history(messages);

    for message in messages {
        if message.role == Role::System {
            system.push(message.content);
            continue;
        }

        let role = if message.role == Role::Assistant {
            "model"
        } else {
            "user"
        };
        let new_parts = message_parts(message, &mut call_names, &mut signed_calls);

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
        body["systemInstruction"] = json!({
            "parts": [{ "text": system.join("\n\n") }]
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
    messages
}

fn assistant_is_empty(message: &Message) -> bool {
    message.content.is_empty()
        && message.images.as_ref().is_none_or(Vec::is_empty)
        && message.tool_calls.as_ref().is_none_or(Vec::is_empty)
}

fn message_parts(
    message: Message,
    call_names: &mut HashMap<String, String>,
    signed_calls: &mut HashSet<String>,
) -> Vec<Value> {
    match message.role {
        Role::Assistant => assistant_parts(message, call_names, signed_calls),
        Role::Tool => tool_result_parts(message, call_names, signed_calls),
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
            if let Some(signature) = sig {
                signed_calls.insert(call.id.clone());
                if !call.name.is_empty() {
                    signed_calls.insert(call.name.clone());
                }
                let mut function_call = json!({
                    "name": call.name,
                    "args": parse_json_object(&call.arguments),
                });
                if !call.id.is_empty() {
                    function_call["id"] = json!(call.id);
                }
                let mut part = json!({ "functionCall": function_call });
                part["thoughtSignature"] = json!(signature);
                parts.push(part);
            } else {
                // ADR-0297: Unsigned tool call lacking a valid Google thought signature
                // (e.g. from Claude/GPT or foreign completion format). Emitting an unsigned
                // `functionCall` triggers Google HTTP 400 INVALID_ARGUMENT. Degrade to
                // objective natural language dialogue fact to preserve causal truth
                // while strictly honoring Google's cryptographic validation boundary.
                let args_str = if call.arguments.trim().is_empty() {
                    "{}"
                } else {
                    call.arguments.trim()
                };
                parts.push(json!({
                    "text": format!("[Executed tool \"{}\" with arguments: {}]", call.name, args_str)
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
    let trimmed = text.trim();
    if trimmed.starts_with("[Called tool ") && trimmed.ends_with(']') {
        return String::new();
    }
    let mut result = text.to_string();
    while let Some(start) = result.find("[Called tool ") {
        if let Some(end) = result[start..].find(']') {
            let candidate = &result[start..=start + end];
            if candidate.contains("with arguments:") {
                result.replace_range(start..=start + end, "");
                continue;
            }
        }
        break;
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
    signed_calls: &HashSet<String>,
) -> Vec<Value> {
    let id = message.tool_call_id.unwrap_or_default();
    let name = call_names.get(&id).cloned().unwrap_or_default();
    let is_signed = (!id.is_empty() && signed_calls.contains(&id))
        || (!name.is_empty() && signed_calls.contains(&name));

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
        // ADR-0297: Degraded tool result for unsigned / cross-provider tool calls.
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
    images: Vec<nuo_model_codec::ImagePart>,
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

fn google_tools(tool_specs: Option<&[nuo_model_codec::ToolSpec]>) -> Option<Value> {
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

fn parse_json_object(text: &str) -> Value {
    if text.trim().is_empty() {
        return json!({});
    }
    match serde_json::from_str::<Value>(text) {
        Ok(value) if value.is_object() => value,
        _ => json!({}),
    }
}

fn tool_response_payload(text: &str) -> Value {
    match serde_json::from_str::<Value>(text) {
        Ok(value) if value.is_object() => value,
        Ok(value) => json!({ "result": value }),
        Err(_) => json!({ "content": text }),
    }
}

/// The non-streaming generateContent URL. `api_key` is embedded as a query
/// param only for a plain API-key credential; an org-scoped credential is
/// header-carried and the URL stays keyless (ADR-0269).
pub fn url(base_url: &str, model: &str, api_key: Option<&str>) -> String {
    let query = api_key.map(|key| format!("?key={key}")).unwrap_or_default();
    format!("{base_url}/models/{model}:generateContent{query}")
}

/// The streaming streamGenerateContent URL (SSE framing via `alt=sse`).
pub fn stream_url(base_url: &str, model: &str, api_key: Option<&str>) -> String {
    let query = api_key
        .map(|key| format!("?alt=sse&key={key}"))
        .unwrap_or_else(|| "?alt=sse".to_string());
    format!("{base_url}/models/{model}:streamGenerateContent{query}")
}

/// The maximum `thinkingBudget` a Gemini 2.5 model accepts, in tokens — the cap
/// [`nuo_model_codec::Effort::gemini_thinking_budget`] buckets against. Gemini 2.5 Flash tops
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
        tool_specs: Option<&'a [nuo_model_codec::ToolSpec]>,
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
    fn caps_with_vision(vision: Option<bool>) -> nuo_model_codec::ModelCapabilities {
        nuo_model_codec::ModelCapabilities {
            family: "google".into(),
            context_window: 1_000_000,
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
    fn declared_text_only_route_projects_inline_data_away() {
        // Google's `inline_data` used to be emitted unconditionally, so a route
        // without image input failed the whole turn (ADR-0230).
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
                Some(&[nuo_model_codec::ToolSpec {
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
        let call = nuo_model_codec::ToolCall {
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
        let answered = nuo_model_codec::ToolCall {
            id: "answered".to_string(),
            name: "list_dir".to_string(),
            arguments: "{}".to_string(),
        };
        let unanswered = nuo_model_codec::ToolCall {
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
    fn degrades_unsigned_foreign_tool_calls_to_safe_dialogue_facts() {
        let call = nuo_model_codec::ToolCall {
            id: "call_foreign".to_string(),
            name: "write_todos".to_string(),
            arguments: r#"{"items":[{"content":"design","status":"in_progress"}]}"#.to_string(),
        };
        // Unsigned assistant message (e.g. from Claude/GPT or foreign completion format, no provider_meta)
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

        // ADR-0297: Assistant turn degrades unsigned tool calls to safe natural language text facts,
        // preventing Google HTTP 400 INVALID_ARGUMENT ("Function call is missing a thought_signature").
        let model_parts = body["contents"][1]["parts"].as_array().unwrap();
        assert_eq!(model_parts.len(), 2);
        assert_eq!(model_parts[0]["text"], "Planning tasks");
        assert_eq!(
            model_parts[1]["text"],
            r#"[Executed tool "write_todos" with arguments: {"items":[{"content":"design","status":"in_progress"}]}]"#
        );
        assert!(model_parts[1].get("functionCall").is_none());

        // Tool result is degraded to user text fact, coalesced with the following user prompt
        assert_eq!(body["contents"][2]["role"], "user");
        let user_parts = body["contents"][2]["parts"].as_array().unwrap();
        assert_eq!(
            user_parts[0]["text"],
            "[Tool result for \"write_todos\"]:\nTodo list updated"
        );
        assert!(user_parts[0].get("functionResponse").is_none());
        assert_eq!(user_parts[1]["text"], "proceed");
    }

    #[test]
    fn mixed_signed_and_unsigned_tool_calls_degrade_granularly() {
        let signed_call = nuo_model_codec::ToolCall {
            id: "call_signed".to_string(),
            name: "list_dir".to_string(),
            arguments: r#"{"path":"."}"#.to_string(),
        };
        let unsigned_call = nuo_model_codec::ToolCall {
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
        // Unsigned call is degraded to text fact
        assert_eq!(
            model_parts[2]["text"],
            r#"[Executed tool "default_api:execute_command" with arguments: {"command":"cargo test"}]"#
        );

        let user_parts = body["contents"][2]["parts"].as_array().unwrap();
        assert_eq!(user_parts.len(), 2);
        // Signed result is native functionResponse
        assert_eq!(user_parts[0]["functionResponse"]["name"], "list_dir");
        // Unsigned result is degraded to text fact
        assert_eq!(
            user_parts[1]["text"],
            "[Tool result for \"default_api:execute_command\"]:\ntests passed"
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
    fn resolve_thinking_level_for_gemini_3x() {
        // Gemini 3.x uses a level ladder; each rung maps to thinkingLevel,
        // clamping down from unsupported depths.
        use nuo_model_codec::effort::Effort;
        let gemini_level: &[Effort] = &[
            Effort::Minimal,
            Effort::Low,
            Effort::Medium,
            Effort::High,
        ];
        let level: Vec<nuo_model_codec::EffortLevel> = gemini_level
            .iter()
            .copied()
            .map(Into::into)
            .collect();
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
        use nuo_model_codec::effort::Effort;
        let gemini_budget: &[Effort] = &[
            Effort::Minimal,
            Effort::Low,
            Effort::Medium,
            Effort::High,
            Effort::Max,
        ];
        let budget: Vec<nuo_model_codec::EffortLevel> = gemini_budget
            .iter()
            .copied()
            .map(Into::into)
            .collect();
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
            resolve_thinking(Some(nuo_model_codec::Effort::High), &[], 24576),
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
                Some(GoogleThinking::Level(nuo_model_codec::Effort::Medium)),
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
