//! Shape dispatch, endpoint algebra, and Antigravity curated catalogs.

use super::*;

#[test]
fn parser_pipeline_dispatches_cleanly() {
    let openai_json: serde_json::Value = serde_json::json!({
        "data": [{"id": "model-1"}]
    });
    let parser = parser_for(CatalogShape::OpenAi);
    let models = parser.parse_json(&openai_json);
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].id, "model-1");

    let anthropic_parser = parser_for(CatalogShape::Anthropic);
    let anthropic_models = anthropic_parser.parse_json(&openai_json);
    assert_eq!(anthropic_models.len(), 1);
    assert_eq!(anthropic_models[0].id, "model-1");
}

#[test]
fn catalog_endpoints_append_to_explicit_roots() {
    for (format, root, expected) in [
        (
            CatalogShape::OpenAi,
            "https://relay.example/team/v1",
            "https://relay.example/team/v1/models",
        ),
        (
            CatalogShape::Anthropic,
            "https://relay.example/team/v1/",
            "https://relay.example/team/v1/models",
        ),
        (
            CatalogShape::Codex,
            "https://chatgpt.com/backend-api/codex",
            "https://chatgpt.com/backend-api/codex/models",
        ),
        (
            CatalogShape::Google,
            "https://relay.example/team/v1beta",
            "https://relay.example/team/v1beta/models",
        ),
        (
            CatalogShape::GoogleCloudCode,
            "https://relay.example/team",
            "https://relay.example/team/v1internal:fetchAvailableModels",
        ),
        // A path that happens to resemble an inference endpoint is still a root.
        (
            CatalogShape::OpenAi,
            "https://relay.example/chat/completions",
            "https://relay.example/chat/completions/models",
        ),
    ] {
        assert_eq!(models_endpoint_for(format, root).unwrap(), expected);
    }
    for invalid in [
        "",
        "relative/path",
        "https://relay.example/v1?key=secret",
        "https://user:pass@relay.example/v1",
        "file:///tmp/models",
    ] {
        assert!(models_endpoint_for(CatalogShape::OpenAi, invalid).is_err());
    }
}

#[test]
fn parses_antigravity_curated_agent_sorts_and_filters_lane_and_effort_variants() {
    let json = serde_json::json!({
        "defaultAgentModelId": "gemini-3.8-flash",
        "agentModelSorts": [
            {
                "displayName": "Recommended",
                "groups": [
                    {
                        "modelIds": [
                            "gemini-4-argon",
                            "gemini-3.8-flash",
                            "claude-sonnet-5-5",
                            "claude-opus-5-5",
                            "claude-sonnet-4-6",
                            "claude-opus-4-6-thinking"
                        ]
                    }
                ]
            }
        ],
        "commandModelIds": ["gemini-3-flash"],
        "commitMessageModelIds": ["gemini-3.5-flash-lite"],
        "imageGenerationModelIds": ["gemini-3.1-flash-image"],
        "audioTranscriptionModelIds": ["models/proactive-observer-v10"],
        "models": {
            "gemini-4-argon": { "maxTokens": 1000000, "supportsThinking": true },
            "gemini-3.8-flash": { "maxTokens": 1048576, "supportsThinking": true },
            "gemini-3.8-flash-high": { "maxTokens": 1048576, "supportsThinking": true },
            "gemini-3.8-flash-medium": { "maxTokens": 1048576, "supportsThinking": true },
            "gemini-3.8-flash-low": { "maxTokens": 1048576, "supportsThinking": true },
            "claude-sonnet-5-5": { "maxTokens": 250000, "supportsThinking": true },
            "claude-sonnet-5-5-high": { "maxTokens": 250000, "supportsThinking": true },
            "claude-sonnet-5-5-max": { "maxTokens": 250000, "supportsThinking": true },
            "claude-opus-5-5": { "maxTokens": 250000, "supportsThinking": true },
            "claude-opus-5-5-low": { "maxTokens": 250000, "supportsThinking": true },
            "claude-sonnet-4-6": { "maxTokens": 250000 },
            "claude-opus-4-6-thinking": { "maxTokens": 250000, "supportsThinking": true },
            "gemini-3-flash": { "maxTokens": 1000000 },
            "gemini-3.5-flash-lite": { "maxTokens": 1000000 },
            "gemini-3.1-flash-image": { "maxTokens": 1000000 },
            "models/proactive-observer-v10": { "maxTokens": 16000 },
            "chat_20706": { "maxTokens": 16000 }
        }
    });

    let discovered = parse_models(CatalogShape::GoogleCloudCode, &json);
    let ids: Vec<String> = discovered.iter().map(|m| m.id.clone()).collect();

    // Preserves curated ordering from agentModelSorts [INV-CATALOG-01]
    assert_eq!(
        ids,
        vec![
            "gemini-4-argon".to_string(),
            "gemini-3.8-flash".to_string(),
            "claude-sonnet-5-5".to_string(),
            "claude-opus-5-5".to_string(),
            "claude-sonnet-4-6".to_string(),
            "claude-opus-4-6".to_string(),
        ]
    );

    // Verify Gemini effort ladder [INV-CATALOG-02]
    let flash38 = discovered
        .iter()
        .find(|m| m.id == "gemini-3.8-flash")
        .unwrap();
    assert_eq!(
        flash38.effort_levels,
        Some(vec![
            "minimal".to_string(),
            "low".to_string(),
            "medium".to_string(),
            "high".to_string()
        ])
    );

    // Verify Claude 5.5 effort ladder [INV-CATALOG-02]
    let sonnet55 = discovered
        .iter()
        .find(|m| m.id == "claude-sonnet-5-5")
        .unwrap();
    assert_eq!(
        sonnet55.effort_levels,
        Some(vec![
            "low".to_string(),
            "medium".to_string(),
            "high".to_string(),
            "max".to_string()
        ])
    );
}

#[test]
fn parses_antigravity_models_filtering_3_6_and_deprecated() {
    let json = serde_json::json!({
        "models": {
            "gemini-3.8-flash": { "maxTokens": 1048576, "supportsThinking": true },
            "gemini-3.8-flash-high": { "maxTokens": 1048576, "supportsThinking": true },
            "gemini-3.8-flash-medium": { "maxTokens": 1048576, "supportsThinking": true },
            "gemini-3.8-flash-low": { "maxTokens": 1048576, "supportsThinking": true },
            "gemini-3.8-flash-tiered": { "maxTokens": 1048576, "supportsThinking": true },
            "gemini-4-argon": { "maxTokens": 1000000, "supportsThinking": true },
            "gemini-3.6-flash-high": { "maxTokens": 1000000, "supportsThinking": true },
            "gemini-pro-agent": { "maxTokens": 1000000, "supportsThinking": true },
            "gemini-3.1-pro-high": { "maxTokens": 1000000, "supportsThinking": true },
            "chat_20706": { "maxTokens": 16000 }
        },
        "deprecatedModelIds": {
            "gemini-3.1-pro-high": { "newModelId": "gemini-pro-agent" }
        }
    });
    let discovered = parse_models(CatalogShape::GoogleCloudCode, &json);
    let got: Vec<String> = discovered.iter().map(|model| model.id.clone()).collect();
    assert!(got.contains(&"gemini-3.8-flash".to_string()));
    assert!(got.contains(&"gemini-4-argon".to_string()));
    assert!(got.contains(&"gemini-pro-agent".to_string()));
    assert!(
        !got.contains(&"gemini-3.8-flash-high".to_string()),
        "3.8 flash effort variants must be normalized/suppressed"
    );
    assert!(
        !got.contains(&"gemini-3.8-flash-medium".to_string()),
        "3.8 flash effort variants must be normalized/suppressed"
    );
    assert!(
        !got.contains(&"gemini-3.8-flash-low".to_string()),
        "3.8 flash effort variants must be normalized/suppressed"
    );
    assert!(
        !got.contains(&"gemini-3.8-flash-tiered".to_string()),
        "3.8 flash tiered duplicate must be suppressed"
    );
    assert!(
        !got.contains(&"gemini-3.6-flash-high".to_string()),
        "3.6 flash must be suppressed"
    );
    assert!(
        !got.contains(&"gemini-3.1-pro-high".to_string()),
        "deprecated model must be suppressed"
    );
    assert!(
        !got.contains(&"chat_20706".to_string()),
        "internal helper model must be suppressed"
    );

    let flash38 = discovered
        .iter()
        .find(|m| m.id == "gemini-3.8-flash")
        .unwrap();
    assert_eq!(
        flash38.effort_levels,
        Some(vec![
            "minimal".to_string(),
            "low".to_string(),
            "medium".to_string(),
            "high".to_string(),
        ])
    );
}

#[test]
fn rejects_empty_base_url() {
    assert!(matches!(
        models_endpoint_for(CatalogShape::OpenAi, ""),
        Err(ModelListError::BadEndpoint(_))
    ));
    assert!(matches!(
        models_endpoint_for(CatalogShape::OpenAi, "   "),
        Err(ModelListError::BadEndpoint(_))
    ));
}
