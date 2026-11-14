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

mod history;
mod parts;
mod thinking;
mod tools;
mod url;

#[cfg(test)]
mod tests;

use nuo_tool::{Message, Role};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};

use self::history::reconcile_tool_history;
use self::parts::{TurnIdentity, message_parts};
use self::tools::google_tools;

pub use self::history::SKIP_THOUGHT_SIGNATURE_VALIDATOR;
pub use self::thinking::{
    GoogleThinking, resolve_thinking, thinking_budget_wire, thinking_level_wire,
};
pub use self::tools::sanitize_schema;
pub use self::url::{max_thinking_budget, stream_url, url};

/// Inputs to [`body`]: the prepared tool schemas in OpenAI function-spec
/// shape, if any.
pub struct BodyInput<'a> {
    /// Structured instructions from the instruction manifest.
    pub instructions: Option<&'a nuo_provider::InstructionText>,
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
    /// The wire identity this request addresses — the ingress authority and the
    /// envelope's wire model (`GoogleProvider::signature_origin`). Recorded so
    /// turns stamped by this route can be recognised on replay: a Google thought
    /// signature only validates on the backend that minted it, and replaying a
    /// foreign one is answered `400 Corrupted thought signature.`
    ///
    /// **Required, not optional.** An `Option` here would read as "no identity
    /// available" and silently disable replay protection for every caller that
    /// omitted it — the failure mode would be invisible (tokens dropped, tool
    /// quality silently degraded) and would spread to every future call site. A
    /// builder that genuinely has no identity must say so explicitly rather than
    /// inherit silence as a default.
    pub signature_origin: &'a str,
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
        nuo_provider::project_images_for_route(model, &messages, capabilities);
    body_inner(messages.into_owned(), input)
}

fn body_inner(messages: Vec<Message>, input: BodyInput<'_>) -> Value {
    let mut system = Vec::new();
    if let Some(instructions) = input.instructions
        && !instructions.is_empty()
    {
        // Emit stable static prefix (Base, Session, Task) followed by ephemeral nudges (ADR-0052, [INV-INST-03])
        let static_text = instructions.render_fixed().to_owned();
        if !static_text.is_empty() {
            system.push(static_text);
        }
        let ephemeral_text = instructions.render_transient().to_owned();
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
            system.push(message.content.clone());
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
            input.signature_origin,
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
                    json!(thinking_level_wire(effort));
            }
            GoogleThinking::Budget(tokens) => {
                body["generationConfig"]["thinkingConfig"]["thinkingBudget"] = json!(tokens);
            }
        }
    }
    body
}
