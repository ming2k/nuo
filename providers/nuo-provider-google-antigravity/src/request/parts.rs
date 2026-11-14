//! Message-part construction: text, images, function calls, and signatures.

use nuo_tool::{Message, Role};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};

use super::history::SKIP_THOUGHT_SIGNATURE_VALIDATOR;
use super::tools::tool_response_payload;
use crate::response::{
    TERMINAL_THOUGHT_SIGNATURE_META_KEY, TEXT_THOUGHT_SIGNATURE_META_KEY,
    THOUGHT_SIGNATURE_ORIGIN_META_KEY, THOUGHT_SIGNATURES_META_KEY,
};

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
pub(super) struct TurnIdentity {
    pub(super) signed: HashSet<String>,
}

pub(super) fn message_parts(
    message: Message,
    call_names: &mut HashMap<String, String>,
    signed_calls: &mut HashSet<String>,
    pending_turn: &mut Option<TurnIdentity>,
    origin: &str,
) -> Vec<Value> {
    match message.role {
        Role::Assistant => {
            let parts = assistant_parts(message, call_names, signed_calls, origin);
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
    origin: &str,
) -> Vec<Value> {
    // A signature is only replayable on the route that minted it. When this
    // turn came from a different wire identity (a model switch, a provider
    // switch, a relay vs. first-party route), reuse is exactly what the
    // upstream answers `400 Corrupted thought signature.` to — so the tokens
    // are dropped rather than forwarded on faith.
    let same_origin = signatures_belong_to(&message, origin);
    let thought_signatures = if same_origin {
        thought_signatures_by_call(&message)
    } else {
        HashMap::new()
    };
    let sanitized_content = sanitize_assistant_content(&message.content);
    // Byte-identity check: a text-part signature validates against the exact
    // bytes of the part that minted it. The parser only records one when the
    // answer WAS a single text part, and this second gate covers the remaining
    // rewrite (`sanitize_assistant_content`) that would silently change those
    // bytes. Anything else loses the signature and keeps the text.
    let text_is_untouched = sanitized_content == message.content;
    let text_thought_sig = same_origin
        .then(|| text_thought_signature(&message))
        .flatten()
        .filter(|_| text_is_untouched);
    // The turn-level signature signs the turn (not a part's bytes), so it is the
    // documented fallback for the turn's function-call group. When the text part
    // already claimed the token it is not claimed again — one token, one site.
    let turn_signature = same_origin
        .then(|| {
            terminal_thought_signature(&message)
                .or_else(|| text_thought_signature(&message).filter(|_| text_thought_sig.is_none()))
        })
        .flatten();

    let mut parts =
        text_and_image_parts(sanitized_content, message.images.unwrap_or_default(), false);
    if let Some(signature) = text_thought_sig
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
                .or(turn_signature.as_ref());
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

/// Last-mile wire guard: strip harness tool-invocation narration from assistant
/// text before it is replayed as a `model` part.
///
/// # This is NOT the authority for this policy
///
/// The authoritative scrub is `nuo_engine::dispatch::tool_call::
/// strip_fallback_invocation_markup`, which runs when a fallback invocation is
/// promoted to a native `ToolCall` and is a strict **superset** of this function
/// (same balanced-brace walk, plus ChatML/Hermes sentinel tokens and embedded
/// JSON-object excision). This function cannot call it: the dependency runs
/// harness → provider, never the reverse, and a provider must not reach into the
/// harness's dispatch layer.
///
/// So the duplication is deliberate and must stay **narrower than** the
/// authority, never divergent in kind: this is a wire guard for histories that
/// reached the provider without passing through promotion (an imported session,
/// a client-persisted transcript, a foreign provider's turn).
///
/// Editing one without reading the other is how these drift, so the coupling is
/// made safe by construction elsewhere: a text-part `thoughtSignature` is only
/// replayed when the sanitized bytes equal the original (see `assistant_parts`).
/// Divergence therefore costs a signature — never a corrupted one on the wire.
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
    provider_meta_string(message, TEXT_THOUGHT_SIGNATURE_META_KEY)
}

/// The **turn-level** signature: minted on the terminal zero-length text part
/// (or on one delta of a multi-delta answer). It signs the turn rather than any
/// single part's bytes, which is exactly what makes it the correct signature for
/// the turn's leading `functionCall` and the wrong one for a text part.
fn terminal_thought_signature(message: &Message) -> Option<String> {
    provider_meta_string(message, TERMINAL_THOUGHT_SIGNATURE_META_KEY)
}

fn provider_meta_string(message: &Message, key: &str) -> Option<String> {
    message
        .provider_meta
        .as_ref()
        .and_then(|meta| meta.get(key))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Whether this turn's signatures were minted by the route we are about to
/// address.
///
/// A Google thought signature is an opaque token that only validates on the
/// backend that minted it — replaying a Vertex signature to Google AI Studio
/// (or an AI Studio signature to `v1internal`) answers `400 Corrupted thought
/// signature.` even though nothing about the request is malformed. The parser
/// records the minting identity ([`THOUGHT_SIGNATURE_ORIGIN_META_KEY`]), so
/// replay is gated on an exact match instead of on optimism.
///
/// A turn with **no** recorded origin cannot prove its tokens valid here, so they
/// are not replayed — unproven is treated as foreign. Dropping the field is
/// explicitly accepted by the upstream; only reusing a foreign token is rejected.
fn signatures_belong_to(message: &Message, origin: &str) -> bool {
    provider_meta_string(message, THOUGHT_SIGNATURE_ORIGIN_META_KEY).as_deref() == Some(origin)
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
