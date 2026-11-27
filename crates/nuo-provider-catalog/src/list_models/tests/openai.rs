//! OpenAI-shaped, Codex, Kimi, DeepSeek, OpenRouter, and Copilot catalogs.

use super::*;

#[test]
fn parses_openai_data_ids() {
    let json = serde_json::json!({
        "data": [
            { "id": "gpt-5.6-sol", "object": "model" },
            { "id": "gpt-5.5", "object": "model" },
            { "id": "gpt-5.4-mini", "object": "model" }
        ]
    });
    let mut got: Vec<String> = parse_models(CatalogShape::OpenAi, &json)
        .into_iter()
        .map(|model| model.id)
        .collect();
    got.sort();
    assert_eq!(got, vec!["gpt-5.4-mini", "gpt-5.5", "gpt-5.6-sol"]);
}

#[test]
fn parses_kimi_platform_capability_fields() {
    // The Kimi Code platform's live response shape (recorded 2026-07 from
    // GET https://api.kimi.com/coding/v1/models): every entry advertises
    // its context length, reasoning/thinking support, and image input;
    // K3 additionally lists its effort tiers.
    let json = serde_json::json!({
        "data": [
            {
                "id": "kimi-for-coding",
                "display_name": "kimi-for-coding",
                "context_length": 262144,
                "supports_reasoning": true,
                "supports_image_in": true,
                "supports_video_in": true,
                "supports_thinking_type": "only"
            },
            {
                "id": "k3",
                "display_name": "k3",
                "context_length": 1048576,
                "supports_reasoning": true,
                "supports_image_in": true,
                "supports_video_in": true,
                "supports_thinking_type": "only",
                "think_efforts": {
                    "support": true,
                    "valid_efforts": ["max"],
                    "default_effort": "max"
                }
            }
        ],
        "object": "list"
    });
    let models = parse_models(CatalogShape::OpenAi, &json);
    assert_eq!(models.len(), 2);
    let k3 = &models[1];
    assert_eq!(k3.id, "k3");
    assert_eq!(k3.context_window, Some(1_048_576));
    assert_eq!(k3.reasoning, Some(true));
    assert_eq!(k3.vision, Some(true));
    assert_eq!(k3.effort_levels, Some(vec!["max".to_string()]));
    // The legacy entry has no effort field → None, not an empty vec.
    assert_eq!(models[0].id, "kimi-for-coding");
    assert_eq!(models[0].context_window, Some(262_144));
    assert_eq!(models[0].effort_levels, None);
}

#[test]
fn parses_openrouter_catalog_capability_fields() {
    let json = serde_json::json!({
        "data": [
            {
                "id": "nex-agi/nex-n2.5-pro:free",
                "context_length": 262144,
                "architecture": {
                    "input_modalities": ["text", "image"],
                    "output_modalities": ["text"]
                },
                "top_provider": { "max_completion_tokens": 235929 },
                "supported_parameters": [
                    "max_tokens", "reasoning", "reasoning_effort", "tool_choice", "tools"
                ],
                "reasoning": {
                    "mandatory": false,
                    "supported_efforts": ["high", "medium", "none"],
                    "default_effort": "high"
                }
            },
            {
                "id": "example/image-generator",
                "architecture": {
                    "input_modalities": ["text"],
                    "output_modalities": ["image"]
                }
            }
        ]
    });

    let models = parse_models(CatalogShape::OpenAi, &json);
    assert_eq!(models.len(), 1);
    let nex = &models[0];
    assert_eq!(nex.context_window, Some(262_144));
    assert_eq!(nex.max_output_tokens, Some(235_929));
    assert_eq!(nex.reasoning, Some(true));
    assert_eq!(nex.thinking, Some(ReasoningSupport::ReasoningContent));
    assert_eq!(nex.tool_call, Some(true));
    assert_eq!(nex.vision, Some(true));
    assert_eq!(
        nex.effort_levels,
        Some(vec!["high".into(), "medium".into(), "none".into()])
    );
}

#[test]
fn parses_nested_metadata_capability_fields() {
    // OpenDesign Cloud publishes limits and modalities under `metadata`
    // (`context_limit`, `output_budget`, `architecture.input_modalities`)
    // instead of the flat OpenRouter/OpenAI fields. Both shapes must land
    // in the same discovered capability record.
    let json = serde_json::json!({
        "object": "list",
        "data": [
            {
                "id": "deepseek-v4.1-flash",
                "object": "model",
                "metadata": {
                    "context_limit": 1_048_000,
                    "context_budget": 328_000,
                    "output_budget": 128_000,
                    "architecture": {
                        "input_modalities": ["text", "image"],
                        "output_modalities": ["text"]
                    }
                }
            },
            {
                "id": "hy4-preview",
                "object": "model",
                "metadata": {
                    "context_limit": 1_000_000,
                    "output_budget": 64_000,
                    "architecture": {
                        "input_modalities": ["text"],
                        "output_modalities": ["text"]
                    }
                }
            },
            {
                "id": "top-level-metadata",
                "object": "model",
                "context_limit": 500_000,
                "output_budget": 64_000,
                "input_modalities": ["text", "image"]
            }
        ]
    });

    let models = parse_models(CatalogShape::OpenAi, &json);
    assert_eq!(models.len(), 3);
    let flash = models
        .iter()
        .find(|model| model.id == "deepseek-v4.1-flash")
        .unwrap();
    assert_eq!(flash.context_window, Some(1_048_000));
    assert_eq!(flash.max_output_tokens, Some(128_000));
    assert_eq!(flash.vision, Some(true));
    let hunyuan = models
        .iter()
        .find(|model| model.id == "hy4-preview")
        .unwrap();
    assert_eq!(hunyuan.vision, Some(false));
    let top_level = models
        .iter()
        .find(|model| model.id == "top-level-metadata")
        .unwrap();
    assert_eq!(top_level.context_window, Some(500_000));
    assert_eq!(top_level.max_output_tokens, Some(64_000));
    assert_eq!(top_level.vision, Some(true));
}

#[test]
fn thinking_type_field_wins_over_legacy_reasoning_bool() {
    // `supports_thinking_type` is the newer, authoritative field: "no"
    // must override a stray `supports_reasoning: true`.
    let json = serde_json::json!({
        "data": [
            { "id": "a", "supports_reasoning": true, "supports_thinking_type": "no" },
            { "id": "b", "supports_reasoning": false, "supports_thinking_type": "both" }
        ]
    });
    let models = parse_models(CatalogShape::OpenAi, &json);
    assert_eq!(models[0].reasoning, Some(false));
    assert_eq!(models[1].reasoning, Some(true));
}

#[test]
fn endpoint_advertising_both_responses_and_chat_resolves_to_responses_api() {
    // CommandCode's live /provider/v1/models advertises
    // `supported_endpoints: ["/chat/completions", "/responses"]` for DeepSeek
    // and modern GPT models. Nuo prefers the reasoning-native Responses API
    // over legacy Chat Completions (ADR-0083).
    let json = serde_json::json!({
        "data": [
            {
                "id": "deepseek/deepseek-v4.1-flash",
                "name": "DeepSeek V4.1 Flash (latest)",
                "context_length": 1000000,
                "supported_endpoints": ["/chat/completions", "/responses"]
            },
            {
                "id": "z-ai/glm-5.3-flash",
                "name": "GLM-5.3 Flash",
                "context_length": 1000000,
                "supported_endpoints": ["/chat/completions"]
            },
            {
                "id": "claude-sonnet-5-5",
                "name": "Claude Sonnet 5.5",
                "context_length": 1000000,
                "supported_endpoints": ["/messages"]
            }
        ]
    });
    let models = parse_models(CatalogShape::OpenAi, &json);
    assert_eq!(models[0].protocol, Some(WireProtocol::OpenAiResponses));
    assert_eq!(
        models[1].protocol,
        Some(WireProtocol::OpenAiChatCompletions)
    );
    assert_eq!(models[2].protocol, Some(WireProtocol::AnthropicMessages));
}
