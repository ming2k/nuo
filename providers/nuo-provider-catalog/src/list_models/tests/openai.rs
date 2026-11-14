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
fn parses_codex_catalog_in_priority_order_with_capabilities() {
    let json = serde_json::json!({
        "models": [
            {
                "slug": "hidden-helper",
                "priority": 0,
                "visibility": "hide",
                "supported_in_api": true,
                "supported_reasoning_levels": [],
                "context_window": 64_000,
                "input_modalities": ["text"]
            },
            {
                "slug": "gpt-codex",
                "priority": 1,
                "visibility": "list",
                "supported_in_api": true,
                "supported_reasoning_levels": [
                    {"effort": "low"}, {"effort": "high"}
                ],
                "context_window": 272_000,
                "input_modalities": ["text", "image"]
            }
        ]
    });
    let models = parse_models(CatalogShape::Codex, &json);
    assert_eq!(models[0].id, "hidden-helper");
    // `visibility:"hide"` is a *listing* declaration, not an availability
    // one. `hidden-helper` is `supported_in_api:true`, so it stays usable —
    // exactly the distinction the old single bit erased.
    assert_eq!(models[0].advertised, Some(false));
    assert_eq!(
        models[0].availability,
        Some(Availability::usable()),
        "an API-supported model must not be marked unusable for being unlisted"
    );
    assert_eq!(models[1].id, "gpt-codex");
    assert_eq!(models[1].advertised, Some(true));
    assert_eq!(models[1].availability, Some(Availability::usable()));
    assert_eq!(models[1].protocol, Some(WireProtocol::OpenAiResponses));
    assert_eq!(models[1].context_window, Some(272_000));
    assert_eq!(models[1].thinking, Some(ReasoningSupport::ReasoningSummary));
    assert_eq!(models[1].vision, Some(true));
    assert_eq!(
        models[1].effort_levels,
        Some(vec!["low".to_string(), "high".to_string()])
    );
}

#[test]
fn parses_visible_astra_from_codex_catalog() {
    let json = serde_json::json!({
        "models": [{
            "slug": "gpt-6-astra",
            "priority": 1,
            "visibility": "list",
            "supported_in_api": true,
            "supported_reasoning_levels": [
                {"effort": "low"},
                {"effort": "medium"},
                {"effort": "high"},
                {"effort": "xhigh"},
                {"effort": "max"},
                {"effort": "ultra"}
            ],
            "context_window": 272000,
            "max_context_window": 872000,
            "input_modalities": ["text", "image"]
        }]
    });

    let models = parse_models(CatalogShape::Codex, &json);
    let astra = models.first().unwrap();
    assert_eq!(astra.id, "gpt-6-astra");
    assert_eq!(astra.advertised, Some(true));
    assert_eq!(astra.context_window, Some(872_000));
    assert_eq!(
        astra
            .effort_levels
            .as_ref()
            .unwrap()
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        vec!["low", "medium", "high", "xhigh", "max", "ultra"]
    );
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
fn parses_deepseek_catalog_capability_fields() {
    let json = serde_json::json!({
        "object": "list",
        "data": [
            {
                "id": "deepseek-flash",
                "object": "model",
                "owned_by": "deepseek",
                "name": "DeepSeek-V4.1-Flash",
                "context_window": 1048576,
                "max_output_tokens": 393216,
                "input_modalities": ["text", "image"],
                "output_modalities": ["text"],
                "effort": {
                    "supported_levels": ["low", "high", "max"],
                    "default_level": "high"
                }
            },
            {
                "id": "deepseek-v4-pro",
                "object": "model",
                "owned_by": "deepseek",
                "name": "DeepSeek-V4-Pro",
                "context_window": 1048576,
                "max_output_tokens": 393216,
                "input_modalities": ["text"],
                "output_modalities": ["text"],
                "effort": {
                    "supported_levels": ["low", "high", "max"],
                    "default_level": "high"
                }
            }
        ]
    });
    let models = parse_models(CatalogShape::OpenAi, &json);
    assert_eq!(models.len(), 2);

    let flash = &models[0];
    assert_eq!(flash.id, "deepseek-flash");
    assert_eq!(flash.context_window, Some(1_048_576));
    assert_eq!(flash.max_output_tokens, Some(393_216));
    assert_eq!(flash.reasoning, Some(true));
    assert_eq!(flash.thinking, Some(ReasoningSupport::ReasoningContent));
    assert_eq!(flash.vision, Some(true));
    // The advertisement (`low`/`high`/`max`) is narrower than the accepted
    // set: `none` is the off switch and the vocabulary aliases are mapped
    // by the vendor, so the clamp range is the expanded set (ADR-0083).
    assert_eq!(
        flash.effort_levels,
        Some(
            [
                "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra"
            ]
            .map(str::to_string)
            .to_vec()
        )
    );

    let pro = &models[1];
    assert_eq!(pro.id, "deepseek-v4-pro");
    assert_eq!(pro.context_window, Some(1_048_576));
    assert_eq!(pro.max_output_tokens, Some(393_216));
    assert_eq!(pro.reasoning, Some(true));
    assert_eq!(pro.thinking, Some(ReasoningSupport::ReasoningContent));
    assert_eq!(pro.vision, Some(false));
}

#[test]
fn deepseek_discovery_expands_exactly_to_the_compiled_ladder() {
    // Anti-drift (ADR-0083): the compiled ladder a DeepSeek route starts
    // from and the ladder a live `/models` refresh lands on must be the
    // same set — otherwise the picker's menu changes shape on first sync,
    // and a persisted `medium`/`none` silently clamps to a different rung.
    let baseline: Vec<String> = nuo_provider::effort_ladders::DEEPSEEK_NATIVE
        .iter()
        .map(|rung| rung.as_str().to_string())
        .collect();
    assert_eq!(
        deepseek_accepted_efforts(&["low".to_string(), "high".to_string(), "max".to_string()]),
        baseline
    );
    // The off switch is reachable on every reasoning model…
    assert_eq!(
        deepseek_accepted_efforts(&["low".to_string()]),
        vec!["none".to_string(), "minimal".to_string(), "low".to_string()]
    );
    // …and no rung above the model's own ceiling is ever invented.
    assert_eq!(
        deepseek_accepted_efforts(&["max".to_string()]),
        vec!["none".to_string(), "max".to_string(), "ultra".to_string()]
    );
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
fn parses_copilot_nested_capability_fields() {
    // GitHub Copilot's live `/models` shape (per `@vscode/copilot-api`'s
    // `CCAModel`): capability data is nested under `capabilities`, unlike
    // Kimi's flat fields. A reasoning model advertises a non-empty
    // `reasoning_effort` tier list; a non-reasoning chat model has none.
    let json = serde_json::json!({
        "data": [
            {
                "id": "gpt-5",
                "name": "GPT-5",
                "object": "model",
                "model_picker_enabled": true,
                "capabilities": {
                    "type": "chat",
                    "family": "gpt-5",
                    "limits": {
                        "max_context_window_tokens": 272_000,
                        "max_output_tokens": 128_000,
                        "max_prompt_tokens": 200_000
                    },
                    "supports": {
                        "adaptive_thinking": false,
                        "streaming": true,
                        "tool_calls": true,
                        "vision": true,
                        "reasoning_effort": ["low", "medium", "high"]
                    }
                }
            },
            {
                "id": "gpt-4o",
                "name": "GPT-4o",
                "object": "model",
                "model_picker_enabled": true,
                "capabilities": {
                    "type": "chat",
                    "family": "gpt-4o",
                    "limits": {
                        "max_context_window_tokens": 128_000,
                        "max_output_tokens": 16_384,
                        "max_prompt_tokens": 96_000
                    },
                    "supports": {
                        "adaptive_thinking": true,
                        "streaming": true,
                        "tool_calls": true,
                        "vision": true
                    }
                }
            },
            {
                "id": "text-embedding-3-small",
                "name": "Embedding V3 small",
                "object": "model",
                "capabilities": {
                    "type": "embeddings"
                }
            }
        ]
    });
    let models = parse_models(CatalogShape::OpenAi, &json);
    // The embeddings entry is filtered out — only chat models surface.
    assert_eq!(models.len(), 2);
    let gpt5 = models.iter().find(|m| m.id == "gpt-5").unwrap();
    assert_eq!(gpt5.context_window, Some(272_000));
    assert_eq!(gpt5.max_output_tokens, Some(128_000));
    assert_eq!(gpt5.reasoning, Some(true));
    assert_eq!(gpt5.thinking, Some(ReasoningSupport::ReasoningContent));
    assert_eq!(gpt5.tool_call, Some(true));
    assert_eq!(gpt5.vision, Some(true));
    assert_eq!(
        gpt5.effort_levels,
        Some(vec![
            "low".to_string(),
            "medium".to_string(),
            "high".to_string()
        ])
    );
    let gpt4o = models.iter().find(|m| m.id == "gpt-4o").unwrap();
    assert_eq!(gpt4o.context_window, Some(128_000));
    // `adaptive_thinking` explicitly declares reasoning despite the absent
    // `reasoning_effort` vocabulary.
    assert_eq!(gpt4o.reasoning, Some(true));
    assert_eq!(gpt4o.effort_levels, None);
    assert_eq!(gpt4o.thinking, Some(ReasoningSupport::Adaptive));
}

#[test]
fn copilot_metadata_preserves_picker_and_endpoint_facts() {
    let json = serde_json::json!({
        "data": [
            {
                "id": "claude-opus-4.7",
                "name": "Claude Opus 4.7",
                "model_picker_enabled": true,
                "supported_endpoints": ["/v1/messages"],
                "capabilities": {
                    "type": "chat",
                    "family": "claude-opus",
                    "limits": {
                        "max_context_window_tokens": 144_000,
                        "max_output_tokens": 64_000
                    },
                    "supports": {
                        "adaptive_thinking": true,
                        "tool_calls": true,
                        "vision": true,
                        "reasoning_effort": ["low", "medium", "high"]
                    }
                }
            },
            {
                "id": "internal-title-model",
                "name": "Internal title model",
                "model_picker_enabled": false,
                "policy": { "state": "disabled", "terms": "requires Copilot Business" },
                "supported_endpoints": ["/responses"],
                "capabilities": {
                    "type": "chat",
                    "family": "internal",
                    "limits": { "max_output_tokens": 1024 },
                    "supports": { "tool_calls": false }
                }
            }
        ]
    });

    let models = parse_models(CatalogShape::OpenAi, &json);
    let claude = models
        .iter()
        .find(|model| model.id == "claude-opus-4.7")
        .unwrap();
    // `model_picker_enabled` is a *listing* declaration, so it lands on
    // `advertised`; availability comes from `policy.state` only.
    assert_eq!(claude.advertised, Some(true));
    assert_eq!(claude.availability, None);
    assert_eq!(claude.protocol, Some(WireProtocol::AnthropicMessages));
    assert_eq!(claude.family.as_deref(), Some("claude-opus"));
    assert_eq!(claude.thinking, Some(ReasoningSupport::Adaptive));
    assert_eq!(claude.tool_call, Some(true));
    assert_eq!(claude.max_output_tokens, Some(64_000));

    let remote = claude.remote_metadata();
    assert_eq!(remote.protocol, Some(WireProtocol::AnthropicMessages));
    assert_eq!(
        remote.effort_levels,
        Some(vec![
            nuo_protocol::EffortLevel::Known(nuo_protocol::Effort::Low),
            nuo_protocol::EffortLevel::Known(nuo_protocol::Effort::Medium),
            nuo_protocol::EffortLevel::Known(nuo_protocol::Effort::High)
        ])
    );
    assert_eq!(remote.thinking, Some(ReasoningSupport::Adaptive));

    let internal = models
        .iter()
        .find(|model| model.id == "internal-title-model")
        .unwrap();
    // Both axes independently: the vendor's picker flag is a listing
    // declaration, and `policy.state:"disabled"` is the availability one,
    // carrying the vendor's own `terms` as the verbatim reason.
    assert_eq!(internal.advertised, Some(false));
    assert_eq!(
        internal.availability,
        Some(Availability::locked(Some(
            "requires Copilot Business".to_string()
        )))
    );
    assert_eq!(internal.protocol, Some(WireProtocol::OpenAiResponses));
}

#[test]
fn copilot_unconfigured_policy_is_undeclared_not_disabled() {
    // `policy.state` is a closed vocabulary. Only an explicit `disabled`
    // is a declaration; `unconfigured` means "no policy set" and must
    // deserialize to undeclared — never to unavailable.
    let json = serde_json::json!({
        "data": [{
            "id": "o5-mini",
            "name": "o5-mini",
            "model_picker_enabled": true,
            "policy": { "state": "unconfigured", "terms": "" },
            "supported_endpoints": ["/chat/completions"],
            "capabilities": {
                "type": "chat",
                "family": "o5",
                "limits": { "max_output_tokens": 2048 },
                "supports": { "tool_calls": true }
            }
        }]
    });
    let models = parse_models(CatalogShape::OpenAi, &json);
    assert_eq!(models[0].availability, None);
    assert_eq!(models[0].advertised, Some(true));
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
