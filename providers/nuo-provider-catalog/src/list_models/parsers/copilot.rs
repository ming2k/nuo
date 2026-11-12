//! Copilot capability and endpoint policy parsing.

use nuo_inference_wire::WireProtocol;
use nuo_provider::{Availability, ReasoningSupport};
use serde_json::Value;

use super::super::DiscoveredModel;
use super::clean_display_label;

pub(super) fn copilot_model_from_capabilities(
    id: String,
    entry: &Value,
    capabilities: &Value,
) -> Option<DiscoveredModel> {
    if let Some(kind) = capabilities.get("type").and_then(Value::as_str)
        && kind != "chat"
    {
        return None;
    }
    let limits = capabilities.get("limits");
    let supports = capabilities.get("supports");
    let effort_levels: Option<Vec<String>> = supports
        .and_then(|s| s.get("reasoning_effort"))
        .and_then(Value::as_array)
        .map(|levels| {
            levels
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        });
    let reasoning = effort_levels
        .as_ref()
        .map(|levels| !levels.is_empty())
        .or_else(|| {
            supports
                .and_then(|s| s.get("adaptive_thinking"))
                .and_then(Value::as_bool)
        });
    let thinking = if supports
        .and_then(|s| s.get("adaptive_thinking"))
        .and_then(Value::as_bool)
        == Some(true)
    {
        Some(ReasoningSupport::Adaptive)
    } else if supports
        .and_then(|s| s.get("max_thinking_budget"))
        .and_then(Value::as_u64)
        .is_some()
    {
        Some(ReasoningSupport::Manual)
    } else {
        reasoning.map(|enabled| {
            if enabled {
                ReasoningSupport::ReasoningContent
            } else {
                ReasoningSupport::None
            }
        })
    };
    let protocol = copilot_protocol(entry.get("supported_endpoints"));
    // `model_picker_enabled` is *picker/listing* metadata — it sits beside
    // `model_picker_category` and `model_picker_price_category` in the vendor's
    // `CCAModel` — so it maps to listing intent, never to availability.
    let advertised = entry.get("model_picker_enabled").and_then(Value::as_bool);
    // Availability is the vendor's `policy.state`, a closed vocabulary
    // (`enabled`/`disabled`/`unconfigured`/`unknown`). Only an explicit
    // `disabled` is a declaration; `unconfigured`, `unknown`, and an absent
    // policy mean *undeclared*, not "off". `terms` is the provider's own
    // free-text explanation and is carried verbatim as the reason.
    let availability = match entry
        .get("policy")
        .and_then(|policy| policy.get("state"))
        .and_then(Value::as_str)
    {
        Some("disabled") => Some(Availability {
            usable: false,
            status_label: None,
            reason: entry
                .get("policy")
                .and_then(|policy| policy.get("terms"))
                .and_then(Value::as_str)
                .filter(|terms| !terms.is_empty())
                .map(str::to_string),
            resets_at_ms: None,
        }),
        Some("enabled") => Some(Availability::usable()),
        _ => None,
    };
    Some(DiscoveredModel {
        id,
        availability,
        advertised,
        protocol,
        endpoint: None,
        family: capabilities
            .get("family")
            .and_then(Value::as_str)
            .map(str::to_string),
        name: entry
            .get("name")
            .or_else(|| entry.get("display_name"))
            .and_then(Value::as_str)
            .map(clean_display_label),
        context_window: limits
            .and_then(|l| l.get("max_context_window_tokens"))
            .and_then(Value::as_u64)
            .map(|length| length as usize),
        max_output_tokens: limits
            .and_then(|l| l.get("max_output_tokens"))
            .and_then(Value::as_u64)
            .and_then(|length| u32::try_from(length).ok()),
        reasoning,
        thinking,
        tool_call: supports
            .and_then(|s| s.get("tool_calls"))
            .and_then(Value::as_bool),
        vision: supports
            .and_then(|s| s.get("vision"))
            .and_then(Value::as_bool),
        effort_levels,
        catalog_source: None,
        wire_entities: None,
    })
}

/// Decode Copilot's advertised route in deterministic priority order. Messages
/// is checked first because it requires a distinct wire format; Responses is
/// next; Chat Completions is the explicit final route. Missing or unfamiliar
/// entries leave the channel's configured fallback untouched.
pub(super) fn copilot_protocol(value: Option<&Value>) -> Option<WireProtocol> {
    let endpoints = value?.as_array()?;
    let has = |needle| endpoints.iter().any(|value| value.as_str() == Some(needle));
    if has("/v1/messages") {
        Some(WireProtocol::AnthropicMessages)
    } else if has("/responses") {
        Some(WireProtocol::OpenAiResponses)
    } else if has("/chat/completions") {
        Some(WireProtocol::OpenAiChatCompletions)
    } else {
        None
    }
}
