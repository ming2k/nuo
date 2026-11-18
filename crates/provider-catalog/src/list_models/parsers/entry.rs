//! Entry-shape parsing shared by every protocol.

use inference_wire::WireProtocol;
use protocol::{CatalogShape, ReasoningSupport};

use serde_json::Value;

use super::super::{DiscoveredModel, ModelListError};
use super::clean_display_label;
use super::parser_for;

/// Parse a standard catalog response.
pub(crate) fn parse_models_with_dimensions(
    protocol: CatalogShape,
    json: &Value,
    _dimensions: &[(&str, &str)],
) -> Vec<DiscoveredModel> {
    parser_for(protocol).parse_json(json)
}

pub(crate) fn validate_catalog_shape(
    protocol: CatalogShape,
    json: &Value,
) -> Result<(), ModelListError> {
    let valid = match protocol {
        CatalogShape::OpenAi | CatalogShape::Anthropic => {
            json.get("data").is_some_and(Value::is_array)
        }
        CatalogShape::Google => json.get("models").is_some_and(Value::is_array),
    };
    valid.then_some(()).ok_or_else(|| {
        ModelListError::Parse(
            match protocol {
                CatalogShape::OpenAi | CatalogShape::Anthropic => {
                    "response is missing the required data array"
                }
                CatalogShape::Google => "response is missing the required models collection",
            }
            .to_string(),
        )
    })
}

/// Extract the ChatGPT Codex `{models:[...]}` catalog. The endpoint assigns a
/// numeric priority (lower first), uses `slug` as the request model id, and
/// explicitly marks picker visibility and reasoning tiers.
pub(crate) fn parse_data_models(json: &Value) -> Vec<DiscoveredModel> {
    let Some(data) = json.get("data").and_then(Value::as_array) else {
        return Vec::new();
    };
    data.iter()
        .filter_map(discovered_model_from_entry)
        .collect()
}

/// Read one `data[]` entry: the mandatory `id` plus any advertised capability
/// fields (absent fields stay `None` — the caller decides whether to trust
/// and persist them). Non-chat entries (Copilot also lists embedding models,
/// tagged `capabilities.type != "chat"`) are skipped entirely rather than
/// surfaced with empty capabilities.
pub(crate) fn discovered_model_from_entry(entry: &Value) -> Option<DiscoveredModel> {
    let id = entry.get("id").and_then(Value::as_str)?.to_string();
    // Some OpenAI-compatible gateways publish limits and modalities under a
    // nested `metadata` object instead of flat fields. Read both shapes;
    // absence still means "the endpoint did not say".
    let metadata = entry.get("metadata");
    if let Some(output_modalities) = entry
        .get("architecture")
        .and_then(|architecture| architecture.get("output_modalities"))
        .and_then(Value::as_array)
        && !output_modalities
            .iter()
            .any(|value| value.as_str() == Some("text"))
    {
        return None;
    }
    let openrouter_reasoning = entry.get("reasoning").filter(|value| value.is_object());
    let reasoning = match entry.get("supports_thinking_type").and_then(Value::as_str) {
        Some("only") | Some("both") => Some(true),
        Some("no") => Some(false),
        _ => entry
            .get("supports_reasoning")
            .and_then(Value::as_bool)
            .or_else(|| openrouter_reasoning.map(|_| true)),
    };
    let string_list = |value: Option<&Value>| -> Option<Vec<String>> {
        value.and_then(Value::as_array).map(|levels| {
            levels
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
    };
    let effort_levels = string_list(
        entry
            .get("think_efforts")
            .and_then(|efforts| efforts.get("valid_efforts"))
            .or_else(|| openrouter_reasoning.and_then(|value| value.get("supported_efforts"))),
    );
    let protocol = entry
        .get("supported_endpoints")
        .and_then(Value::as_array)
        .and_then(|endpoints| {
            let has = |needle: &str| {
                endpoints.iter().any(|e| {
                    e.as_str()
                        .is_some_and(|s| s == needle || s.ends_with(needle))
                })
            };
            if has("/messages") && !has("/chat/completions") && !has("/responses") {
                Some(WireProtocol::AnthropicMessages)
            } else if has("/responses") {
                // OpenAI Responses API is the modern, reasoning-native protocol
                // (ADR-0083). When an endpoint advertises both /responses and
                // /chat/completions (e.g. CommandCode for DeepSeek/GPT), the
                // Responses API takes precedence over legacy Chat Completions.
                Some(WireProtocol::OpenAiResponses)
            } else if has("/chat/completions") {
                Some(WireProtocol::OpenAiChatCompletions)
            } else {
                None
            }
        });
    Some(DiscoveredModel {
        id,
        availability: None,
        advertised: None,
        protocol,
        endpoint: None,
        family: None,
        // Anthropic publishes `display_name`, Kimi `display_name`, and
        // OpenRouter a plain `name`; the stock OpenAI `/models` shape carries
        // none of them. Any of the three is accepted — the first one present
        // wins, and absence is normal.
        name: entry
            .get("display_name")
            .or_else(|| entry.get("name"))
            .and_then(Value::as_str)
            .map(clean_display_label),
        context_window: entry
            .get("context_length")
            .or_else(|| entry.get("context_window"))
            .or_else(|| entry.get("max_context_window"))
            .or_else(|| entry.get("max_context_length"))
            .or_else(|| entry.get("context_limit"))
            .or_else(|| metadata.and_then(|metadata| metadata.get("context_limit")))
            .or_else(|| metadata.and_then(|metadata| metadata.get("context_window")))
            .and_then(Value::as_u64)
            .map(|length| length as usize),
        max_output_tokens: entry
            .get("max_output_tokens")
            .or_else(|| entry.get("max_completion_tokens"))
            .or_else(|| entry.get("output_budget"))
            .or_else(|| {
                entry
                    .get("top_provider")
                    .and_then(|provider| provider.get("max_completion_tokens"))
            })
            .or_else(|| metadata.and_then(|metadata| metadata.get("output_budget")))
            .or_else(|| metadata.and_then(|metadata| metadata.get("max_output_tokens")))
            .and_then(Value::as_u64)
            .and_then(|length| u32::try_from(length).ok()),
        reasoning,
        thinking: openrouter_reasoning.map(|_| ReasoningSupport::ReasoningContent),
        tool_call: entry
            .get("supported_parameters")
            .and_then(Value::as_array)
            .map(|parameters| {
                parameters
                    .iter()
                    .any(|value| value.as_str() == Some("tools"))
            }),
        vision: entry
            .get("supports_image_in")
            .and_then(Value::as_bool)
            .or_else(|| {
                entry
                    .get("architecture")
                    .and_then(|architecture| architecture.get("input_modalities"))
                    .or_else(|| entry.get("input_modalities"))
                    .or_else(|| {
                        metadata
                            .and_then(|metadata| metadata.get("architecture"))
                            .and_then(|architecture| architecture.get("input_modalities"))
                    })
                    .or_else(|| metadata.and_then(|metadata| metadata.get("input_modalities")))
                    .and_then(Value::as_array)
                    .map(|modalities| {
                        modalities
                            .iter()
                            .any(|value| value.as_str() == Some("image"))
                    })
            }),
        effort_levels,
        catalog_source: None,
        wire_entities: None,
    })
}
