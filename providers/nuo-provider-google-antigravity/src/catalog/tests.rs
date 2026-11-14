use super::*;

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

    let discovered = parse_antigravity_catalog(&json);
    let ids: Vec<String> = discovered.iter().map(|m| m.id.clone()).collect();

    // Preserves curated ordering from agentModelSorts [INV-CATALOG-01]
    // and folds parameter/thinking suffixes into canonical identities [INV-CATALOG-02]
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

    let opus46 = discovered
        .iter()
        .find(|m| m.id == "claude-opus-4-6")
        .unwrap();
    assert_eq!(
        opus46.wire_entities,
        Some(vec!["claude-opus-4-6-thinking".to_string()])
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
fn folds_parameter_variants_without_bare_model_and_locks_disabled_entitlements() {
    // Upstream sends ONLY parameterized variants (-high, -medium, etc.) and
    // entitlement-disabled variants (-disabled), with zero bare model entries.
    let json = serde_json::json!({
        "agentModelSorts": [
            {
                "displayName": "Recommended",
                "groups": [
                    {
                        "modelIds": [
                            "gemini-4-argon-disabled",
                            "gemini-3.8-flash-high",
                            "gemini-3.8-flash-medium"
                        ]
                    }
                ]
            }
        ],
        "models": {
            "gemini-4-argon-disabled": {
                "displayName": "Gemini 4 Argon (Disabled)",
                "maxTokens": 1000000,
                "maxOutputTokens": 64000,
                "supportsThinking": true
            },
            "gemini-3.8-flash-high": {
                "displayName": "Gemini 3.8 Flash (High)",
                "maxTokens": 1048576,
                "maxOutputTokens": 65536,
                "supportsThinking": true,
                "supportsImages": true
            },
            "gemini-3.8-flash-medium": {
                "displayName": "Gemini 3.8 Flash (Medium)",
                "maxTokens": 1048576,
                "maxOutputTokens": 65536,
                "supportsThinking": true,
                "supportsImages": true
            },
            "gemini-3.8-flash-low": {
                "displayName": "Gemini 3.8 Flash (Low)",
                "maxTokens": 1048576,
                "maxOutputTokens": 65536,
                "supportsThinking": true,
                "supportsImages": true
            },
            "gemini-3.8-flash-tiered": {
                "displayName": "Gemini 3.8 Flash (Tiered)",
                "maxTokens": 1048576,
                "maxOutputTokens": 65536,
                "supportsThinking": true
            }
        }
    });

    let discovered = parse_antigravity_catalog(&json);
    let ids: Vec<String> = discovered.iter().map(|m| m.id.clone()).collect();

    // Deduped and folded canonical IDs in curated sort order
    assert_eq!(
        ids,
        vec!["gemini-4-argon".to_string(), "gemini-3.8-flash".to_string()]
    );

    // Argon: entitlement locked because only -disabled variant was observed
    let argon = discovered
        .iter()
        .find(|m| m.id == "gemini-4-argon")
        .unwrap();
    assert_eq!(argon.name.as_deref(), Some("Gemini 4 Argon"));
    assert_eq!(
        argon.availability,
        Some(Availability::disabled(
            "disabled: tier",
            Some("Requires subscription or account entitlement".to_string()),
            None,
        ))
    );
    assert_eq!(
        argon.effort_levels,
        Some(vec![
            "minimal".to_string(),
            "low".to_string(),
            "medium".to_string(),
            "high".to_string()
        ])
    );

    // Flash 3.8: folded into canonical base ID, clean display name, usable availability
    let flash38 = discovered
        .iter()
        .find(|m| m.id == "gemini-3.8-flash")
        .unwrap();
    assert_eq!(flash38.name.as_deref(), Some("Gemini 3.8 Flash"));
    assert_eq!(flash38.availability, None);
    assert_eq!(flash38.context_window, Some(1048576));
    assert_eq!(flash38.max_output_tokens, Some(65536));
    assert_eq!(flash38.vision, Some(true));
    assert_eq!(
        flash38.effort_levels,
        Some(vec![
            "minimal".to_string(),
            "low".to_string(),
            "medium".to_string(),
            "high".to_string()
        ])
    );
}

#[test]
fn curated_agent_sorts_suppresses_uncurated_legacy_generations() {
    let json = serde_json::json!({
        "agentModelSorts": [
            {
                "displayName": "Recommended",
                "groups": [
                    {
                        "modelIds": [
                            "gemini-3.8-flash"
                        ]
                    }
                ]
            }
        ],
        "models": {
            "gemini-3.8-flash": { "maxTokens": 1048576, "supportsThinking": true },
            "gemini-2.5-flash": { "maxTokens": 1048576 },
            "gemini-2.5-pro": { "maxTokens": 1048576 }
        }
    });

    let discovered = parse_antigravity_catalog(&json);
    let ids: Vec<String> = discovered.iter().map(|m| m.id.clone()).collect();

    assert_eq!(ids, vec!["gemini-3.8-flash".to_string()]);
    assert!(!ids.contains(&"gemini-2.5-flash".to_string()));
    assert!(!ids.contains(&"gemini-2.5-pro".to_string()));
}

/// The parser records the wire-entity **evidence** it observed: suffixed
/// rows reconstruct to concrete registered entities ranked shallow→deep,
/// a bare-row registration leads the list (in-band depth), and absence
/// of variants means no entity list at all (verbatim routing). This is
/// the parser→`RemoteModelMetadata`→`ModelCapabilities` belt the request
/// builder's typed resolution consumes (ADR-0074).
#[test]
fn observes_wire_entities_from_live_rows() {
    let json = serde_json::json!({
        "agentModelSorts": [
            {"groups": [{"modelIds": [
                "gemini-4-argon-medium", "claude-opus-5-5-high", "gemini-3.8-flash",
                "claude-sonnet-4-6"
            ]}]}
        ],
        "models": {
            "gemini-4-argon-low":    {"maxTokens": 1048576, "supportsThinking": true},
            "gemini-4-argon-medium": {"maxTokens": 1048576, "supportsThinking": true},
            "gemini-4-argon-high":   {"maxTokens": 1048576, "supportsThinking": true},
            "claude-opus-5-5-low":    {"maxTokens": 1000000, "supportsThinking": true},
            "claude-opus-5-5-medium": {"maxTokens": 1000000, "supportsThinking": true},
            "claude-opus-5-5-high":   {"maxTokens": 1000000, "supportsThinking": true},
            "gemini-3.8-flash": {"maxTokens": 1048576, "supportsThinking": true},
            "gemini-3.8-flash-high": {"maxTokens": 1048576, "supportsThinking": true},
            "claude-sonnet-4-6": {"maxTokens": 250000}
        }
    });

    let discovered = parse_antigravity_catalog(&json);
    let pick = |id: &str| {
        discovered
            .iter()
            .find(|m| m.id == id)
            .cloned()
            .unwrap_or_else(|| panic!("missing {id}"))
    };

    // Suffixed-only surface: entities in shallow→deep register order.
    assert_eq!(
        pick("gemini-4-argon").wire_entities.as_deref(),
        Some(
            &[
                "gemini-4-argon-low".to_string(),
                "gemini-4-argon-medium".to_string(),
                "gemini-4-argon-high".to_string()
            ][..]
        )
    );
    // Bare row observed: the canonical id leads (in-band depth).
    assert_eq!(
        pick("gemini-3.8-flash")
            .wire_entities
            .as_deref()
            .and_then(|e| e.first()),
        Some(&"gemini-3.8-flash".to_string())
    );
    // Zero parameterized variants but a bare registered row: the
    // canonical id leads a single-entity list (verbatim-equivalent —
    // `wire_entity_resolution` sees the channel id IS a registered
    // entity and takes the Verbatim arm).
    let legacy = pick("claude-sonnet-4-6");
    assert_eq!(
        legacy.wire_entities.as_deref(),
        Some(&["claude-sonnet-4-6".to_string()][..])
    );
    let caps = nuo_provider::ModelCapabilities::for_channel(
        "claude-sonnet-4-6",
        Some(&legacy.remote_metadata()),
    );
    assert!(matches!(
        caps.wire_entity_resolution("claude-sonnet-4-6"),
        nuo_provider::WireEntityResolution::Verbatim
    ));

    // Merge path: remote metadata → caps, verbatim for the bare-lead case.
    let meta = pick("gemini-3.8-flash").remote_metadata();
    let caps = nuo_provider::ModelCapabilities::for_channel("gemini-3.8-flash", Some(&meta));
    assert!(caps.effort_entities.is_empty() || caps.effort_entities[0] == "gemini-3.8-flash");
    let argon_meta = pick("gemini-4-argon").remote_metadata();
    let argon_caps =
        nuo_provider::ModelCapabilities::for_channel("gemini-4-argon", Some(&argon_meta));
    assert_eq!(argon_caps.effort_entities.len(), 3);
    assert!(matches!(
        argon_caps.wire_entity_resolution("gemini-4-argon"),
        nuo_provider::WireEntityResolution::PerLevel { .. }
    ));
    assert!(matches!(
        argon_caps.wire_entity_resolution("gemini-4-argon-high"),
        nuo_provider::WireEntityResolution::Verbatim
    ));
}

#[test]
fn clean_display_name_strips_thinking_and_tier_suffixes() {
    assert_eq!(
        clean_display_name("Claude Opus 4.6 (Thinking)"),
        "Claude Opus 4.6"
    );
    assert_eq!(
        clean_display_name("Claude Sonnet 4.6 (Thinking)"),
        "Claude Sonnet 4.6"
    );
    assert_eq!(
        clean_display_name("Claude Opus 4.6 (thinking)"),
        "Claude Opus 4.6"
    );
    assert_eq!(
        clean_display_name("Claude Opus 4.6(Thinking)"),
        "Claude Opus 4.6"
    );
    assert_eq!(
        clean_display_name("Claude Opus 4.6 [Thinking]"),
        "Claude Opus 4.6"
    );
    assert_eq!(
        clean_display_name("Claude Opus 4.6 - Thinking"),
        "Claude Opus 4.6"
    );
    assert_eq!(
        clean_display_name("Claude Opus 4.6 (Thinking) (High)"),
        "Claude Opus 4.6"
    );
    assert_eq!(
        clean_display_name("Gemini 4 Argon (Medium)"),
        "Gemini 4 Argon"
    );
    assert_eq!(
        clean_display_name("Gemini 4 Argon (Disabled)"),
        "Gemini 4 Argon"
    );
}
