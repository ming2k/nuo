//! The `opencode` provider preset: the OpenCode **Console** subscription and
//! inference surface (`opencode.ai/inference/…`), cataloged and routed by the
//! account-scoped `/console/api/config` catalog
//! ([`CatalogShape::OpencodeConsole`], ADR-0269). The catalog advertises the
//! served models *and* their routing: a per-model `provider.{npm,api}` override
//! selects the wire protocol and inference root, so the live catalog is
//! authoritative for both.

use nuo_model_codec::reasoning::ReasoningSupport;
use nuo_model_codec::{Model, WireProtocol};
use serde::Deserialize;
use std::collections::BTreeMap;

use nuo_provider::*;

/// Baseline seed models for the OpenCode Console preset.
pub use nuo_model_codec::model_providers::OPENCODE_CONSOLE_MODELS;

/// Baseline capability metadata for models served by OpenCode Console.
pub const MODELS: &[Model] = &[
    Model {
        id: "claude-sonnet-4-6",
        family: "claude",
        context_window: 1_000_000,
        thinking: ReasoningSupport::AnthropicAdaptive,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::AnthropicMessages,
        model_guidance: "",
        effort_levels: effort_ladders::CLAUDE_NO_XHIGH,
    },
    Model {
        id: "deepseek-v4-flash",
        family: "deepseek",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::LOW_HIGH_MAX,
    },
    Model {
        id: "glm-5.2",
        family: "glm",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::GLM_5,
    },
];

inventory::submit!(nuo_model_codec::model::BaselineModels(MODELS));

pub const MODEL_PROVIDER_SPEC: ModelProviderSpec = ModelProviderSpec {
    dialect: nuo_model_codec::ProviderDialect::Standard,
    protocol_roots: std::borrow::Cow::Borrowed(&[
        (
            WireProtocol::AnthropicMessages,
            std::borrow::Cow::Borrowed("https://opencode.ai/inference/anthropic/v1"),
        ),
        (
            WireProtocol::GoogleGemini,
            std::borrow::Cow::Borrowed("https://opencode.ai/inference/google/v1beta"),
        ),
    ]),
    catalog_root_url: Some(std::borrow::Cow::Borrowed("https://opencode.ai/console")),
    prompt_cache: nuo_provider::PromptCachePolicy::Compiled(nuo_provider::unsupported_prompt_cache),
    id: std::borrow::Cow::Borrowed("opencode"),
    baselines: MODELS,
    // Per-model routes come from the account catalog (protocol + optional
    // root override); the default root serves OpenAI chat/responses surfaces
    // and any model the catalog does not override.
    root_url: std::borrow::Cow::Borrowed("https://opencode.ai/inference/openai/v1"),
    user_agent: Some(std::borrow::Cow::Borrowed(
        nuo_model_codec::client_identity::OPENCODE_USER_AGENT,
    )),
    protocol: WireProtocol::ChatCompletions,
    catalog_source: RemoteCatalogSource::Endpoint(CatalogShape::OpencodeConsole),
    default_client_profile: nuo_model_codec::ClientPreset::Native,
    client_profile_sensitive: false,
    models: OPENCODE_CONSOLE_MODELS,
};

#[derive(Debug, Clone, Deserialize)]
struct DevProvider {
    /// The provider-default npm; models without a `provider` override ride it.
    #[serde(default)]
    npm: Option<String>,
    #[serde(default)]
    models: BTreeMap<String, DevModel>,
}

#[derive(Debug, Clone, Deserialize)]
struct DevModel {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    family: Option<String>,
    #[serde(default)]
    reasoning: bool,
    #[serde(default)]
    reasoning_options: Vec<DevReasoningOption>,
    #[serde(default)]
    tool_call: bool,
    #[serde(default)]
    limit: DevLimit,
    #[serde(default)]
    modalities: DevModalities,
    #[serde(default)]
    provider: Option<DevModelProvider>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct DevModelProvider {
    #[serde(default)]
    npm: Option<String>,
    #[serde(default)]
    api: Option<String>,
}

fn protocol_for_npm(npm: &str) -> Option<WireProtocol> {
    match npm {
        "@ai-sdk/anthropic" => Some(WireProtocol::AnthropicMessages),
        "@ai-sdk/google" => Some(WireProtocol::GoogleGemini),
        "@ai-sdk/openai" => Some(WireProtocol::Responses),
        "@ai-sdk/openai-compatible" => Some(WireProtocol::ChatCompletions),
        _ => None,
    }
}

#[derive(Debug, Clone, Deserialize)]
struct DevReasoningOption {
    r#type: String,
    #[serde(default)]
    values: Vec<Option<String>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct DevLimit {
    #[serde(default)]
    context: Option<u64>,
    #[serde(default)]
    output: Option<u64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct DevModalities {
    #[serde(default)]
    input: Vec<String>,
    #[serde(default)]
    #[allow(dead_code)]
    output: Vec<String>,
}

/// Extract the account catalog from a Console `/api/config` response:
/// `{"config":{"provider":{"opencode":{npm, models:{…}}}}}` (ADR-0269).
pub fn parse_config_catalog(
    json: &serde_json::Value,
) -> Vec<nuo_provider::DiscoveredModel> {
    let Some(provider_json) = json
        .get("config")
        .and_then(|config| config.get("provider"))
        .and_then(|providers| providers.get("opencode"))
    else {
        return Vec::new();
    };
    let Ok(provider) = serde_json::from_value::<DevProvider>(provider_json.clone()) else {
        return Vec::new();
    };
    let default_protocol = provider.npm.as_deref().and_then(protocol_for_npm);
    let mut models: Vec<_> = provider
        .models
        .into_iter()
        .map(|(key, model)| from_dev_model(key, model, default_protocol))
        .collect();
    models.sort_by(|a, b| a.id.cmp(&b.id));
    models.dedup_by(|a, b| a.id == b.id);
    models
}

fn from_dev_model(
    key: String,
    m: DevModel,
    default_protocol: Option<WireProtocol>,
) -> nuo_provider::DiscoveredModel {
    let id = if m.id.trim().is_empty() { key } else { m.id };
    let modalities_in = &m.modalities.input;
    let reasoning = Some(m.reasoning);
    let thinking = Some(if m.reasoning {
        ReasoningSupport::ReasoningContent
    } else {
        ReasoningSupport::None
    });
    let effort_levels = m
        .reasoning_options
        .iter()
        .filter(|opt| opt.r#type == "effort")
        .flat_map(|opt| opt.values.iter().flatten())
        .cloned()
        .collect::<Vec<_>>();
    let protocol = m
        .provider
        .as_ref()
        .and_then(|p| p.npm.as_deref())
        .and_then(protocol_for_npm)
        .or(default_protocol);
    nuo_provider::DiscoveredModel {
        id,
        availability: None,
        advertised: None,
        protocol,
        endpoint: m.provider.as_ref().and_then(|p| p.api.clone()),
        family: m.family.clone(),
        name: (!m.name.trim().is_empty()).then(|| m.name.clone()),
        context_window: m.limit.context.map(|c| c as usize),
        max_output_tokens: m.limit.output.map(|o| o as u32),
        reasoning,
        thinking,
        tool_call: Some(m.tool_call),
        vision: if modalities_in.is_empty() {
            None
        } else {
            Some(modalities_in.iter().any(|m| m == "image"))
        },
        effort_levels: (!effort_levels.is_empty()).then_some(effort_levels),
        catalog_source: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_model() -> DevModel {
        DevModel {
            id: "glm-5.3".to_string(),
            name: "GLM-5.3".to_string(),
            family: Some("glm".to_string()),
            reasoning: true,
            reasoning_options: vec![DevReasoningOption {
                r#type: "effort".to_string(),
                values: vec![
                    Some("low".to_string()),
                    Some("high".to_string()),
                    Some("max".to_string()),
                ],
            }],
            tool_call: true,
            limit: DevLimit {
                context: Some(1_000_000),
                output: Some(131_072),
            },
            modalities: DevModalities {
                input: vec!["text".to_string()],
                output: vec!["text".to_string()],
            },
            provider: None,
        }
    }

    #[test]
    fn maps_capabilities_and_effort_ladder() {
        let dm = from_dev_model("k".to_string(), sample_model(), None);
        assert_eq!(dm.id, "glm-5.3");
        assert_eq!(dm.family.as_deref(), Some("glm"));
        assert_eq!(dm.context_window, Some(1_000_000));
        assert_eq!(dm.max_output_tokens, Some(131_072));
        assert_eq!(dm.thinking, Some(ReasoningSupport::ReasoningContent));
        assert_eq!(dm.reasoning, Some(true));
        assert_eq!(dm.tool_call, Some(true));
        assert_eq!(dm.vision, Some(false));
        assert_eq!(
            dm.effort_levels,
            Some(vec![
                "low".to_string(),
                "high".to_string(),
                "max".to_string()
            ])
        );
        assert_eq!(dm.protocol, None);
        assert_eq!(dm.endpoint, None);
    }

    #[test]
    fn vision_is_derived_from_input_modalities() {
        let mut m = sample_model();
        m.modalities.input = vec!["text".to_string(), "image".to_string()];
        assert_eq!(from_dev_model("k".to_string(), m, None).vision, Some(true));
    }

    #[test]
    fn parses_console_config_catalog_with_routing_overrides() {
        let raw = serde_json::json!({
            "config": {
                "provider": {
                    "opencode": {
                        "npm": "@ai-sdk/openai-compatible",
                        "models": {
                            "m1": {
                                "name": "Model 1",
                                "reasoning": false,
                                "tool_call": true
                            },
                            "c1": {
                                "name": "Claude 1",
                                "reasoning": true,
                                "tool_call": true,
                                "provider": {
                                    "npm": "@ai-sdk/anthropic",
                                    "api": "https://opencode.ai/inference/anthropic/v1"
                                }
                            },
                            "g1": {
                                "name": "GPT 1",
                                "provider": { "npm": "@ai-sdk/openai" }
                            },
                            "u1": {
                                "name": "Unknown npm",
                                "provider": { "npm": "@ai-sdk/mystery" }
                            }
                        }
                    }
                }
            }
        });
        let models = parse_config_catalog(&raw);
        assert_eq!(models.len(), 4);
        let ids: Vec<_> = models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["c1", "g1", "m1", "u1"]);
        let find = |id: &str| models.iter().find(|m| m.id == id).unwrap();
        assert_eq!(find("m1").protocol, Some(WireProtocol::ChatCompletions));
        assert_eq!(find("m1").endpoint, None);
        assert_eq!(find("c1").protocol, Some(WireProtocol::AnthropicMessages));
        assert_eq!(
            find("c1").endpoint.as_deref(),
            Some("https://opencode.ai/inference/anthropic/v1")
        );
        assert_eq!(find("g1").protocol, Some(WireProtocol::Responses));
        assert_eq!(find("g1").endpoint, None);
        assert_eq!(find("u1").protocol, Some(WireProtocol::ChatCompletions));
        assert_eq!(find("m1").effort_levels, None);
    }

    #[test]
    fn empty_models_map_is_an_empty_catalog() {
        let raw = serde_json::json!({
            "config": { "provider": { "opencode": { "npm": "@ai-sdk/openai-compatible", "models": {} } } }
        });
        assert!(parse_config_catalog(&raw).is_empty());
    }

    #[test]
    fn non_console_shape_parses_to_nothing() {
        let raw = serde_json::json!({ "opencode-go": { "models": {} } });
        assert!(parse_config_catalog(&raw).is_empty());
    }
}
