//! GitHub Copilot live catalog discovery and `CCAModel` capability parsing.

use nuo_inference_wire::WireProtocol;
use nuo_protocol::{Availability, ReasoningSupport, clean_model_display_name};
use nuo_provider::catalog::{
    DiscoveredModel, ModelListError, RemoteCatalogOptions, RemoteCatalogRequest,
    RemoteCatalogUpdate, fetch_bearer_models_response, finalize_catalog_response,
};
use serde_json::Value;

/// Fetch and parse GitHub Copilot's `/models` catalog.
pub async fn fetch_remote_catalog(
    req: RemoteCatalogRequest<'_>,
    options: RemoteCatalogOptions<'_>,
) -> Result<RemoteCatalogUpdate, ModelListError> {
    let response = fetch_bearer_models_response(&req, options).await?;
    finalize_catalog_response(response, options, true, |json| {
        if !json.get("data").is_some_and(Value::is_array) {
            return Err(ModelListError::Parse(
                "response is missing the required data array".to_string(),
            ));
        }
        Ok(parse_copilot_models(json))
    })
}

pub fn parse_copilot_models(json: &Value) -> Vec<DiscoveredModel> {
    json.get("data")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| {
                    let id = entry.get("id").and_then(Value::as_str)?.to_string();
                    let capabilities = entry.get("capabilities")?;
                    copilot_model_from_capabilities(id, entry, capabilities)
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn copilot_model_from_capabilities(
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
    let advertised = entry.get("model_picker_enabled").and_then(Value::as_bool);
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
            .map(clean_model_display_name),
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

/// Decode Copilot's advertised route in deterministic priority order.
pub fn copilot_protocol(value: Option<&Value>) -> Option<WireProtocol> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_copilot_nested_capability_fields() {
        let json = serde_json::json!({
            "data": [
                {
                    "id": "claude-opus-4.6",
                    "name": "Claude Opus 4.6",
                    "model_picker_enabled": true,
                    "policy": { "state": "enabled", "terms": "Enable access to the latest Claude Opus 4.6 model from Anthropic." },
                    "supported_endpoints": ["/v1/messages", "/chat/completions"],
                    "capabilities": {
                        "family": "claude-opus-4.6",
                        "type": "chat",
                        "limits": {
                            "max_context_window_tokens": 200_000,
                            "max_output_tokens": 64_000
                        },
                        "supports": {
                            "adaptive_thinking": true,
                            "max_thinking_budget": 32_000,
                            "reasoning_effort": ["low", "medium", "high", "max"],
                            "tool_calls": true,
                            "vision": true
                        }
                    }
                },
                {
                    "id": "gpt-5.5",
                    "name": "GPT-5.5",
                    "model_picker_enabled": false,
                    "policy": { "state": "disabled", "terms": "Not enabled for this plan." },
                    "supported_endpoints": ["/responses"],
                    "capabilities": {
                        "family": "gpt-5.5",
                        "type": "chat",
                        "limits": {
                            "max_context_window_tokens": 400_000,
                            "max_output_tokens": 128_000
                        },
                        "supports": {
                            "reasoning_effort": ["low", "medium", "high"],
                            "tool_calls": true,
                            "vision": true
                        }
                    }
                },
                {
                    "id": "text-embedding-3-small",
                    "name": "Embedding",
                    "capabilities": { "type": "embeddings" }
                }
            ]
        });

        let models = parse_copilot_models(&json);
        assert_eq!(models.len(), 2);

        let opus = &models[0];
        assert_eq!(opus.id, "claude-opus-4.6");
        assert_eq!(opus.advertised, Some(true));
        assert_eq!(opus.availability, Some(Availability::usable()));
        assert_eq!(opus.protocol, Some(WireProtocol::AnthropicMessages));
        assert_eq!(opus.thinking, Some(ReasoningSupport::Adaptive));
        assert_eq!(opus.context_window, Some(200_000));
        assert_eq!(opus.max_output_tokens, Some(64_000));
        assert_eq!(opus.tool_call, Some(true));
        assert_eq!(opus.vision, Some(true));

        let gpt = &models[1];
        assert_eq!(gpt.id, "gpt-5.5");
        assert_eq!(gpt.advertised, Some(false));
        assert_eq!(
            gpt.availability
                .as_ref()
                .map(|a| (a.usable, a.reason.as_deref())),
            Some((false, Some("Not enabled for this plan.")))
        );
        assert_eq!(gpt.protocol, Some(WireProtocol::OpenAiResponses));
    }

    #[test]
    fn copilot_unconfigured_policy_is_undeclared_not_disabled() {
        let json = serde_json::json!({
            "data": [{
                "id": "gpt-5.5-preview",
                "model_picker_enabled": true,
                "policy": { "state": "unconfigured" },
                "capabilities": { "type": "chat" }
            }]
        });
        let models = parse_copilot_models(&json);
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].availability, None);
    }
}
