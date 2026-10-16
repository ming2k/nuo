//! Google Antigravity (CloudCode) live model catalog parser (`/v1internal:fetchAvailableModels`).
//!
//! Implements [INV-CATALOG-01] (Curated Agent Surface Primacy), [INV-CATALOG-02]
//! (Orthogonal Capability Lifting & Parameter Suffix Folding), [INV-CATALOG-03]
//! (Abolition of Legacy Version Blacklists), and [INV-CATALOG-04] (Provider-Owned
//! Dialect Ingestion) per ADR-0068.

use std::collections::HashSet;
use nuo_model_codec::catalog::DiscoveredModel;
use nuo_model_codec::reasoning::ReasoningSupport;
use serde_json::Value;

/// Parse the Google Antigravity `/v1internal:fetchAvailableModels` response.
pub fn parse_antigravity_catalog(root: &Value) -> Vec<DiscoveredModel> {
    let Some(models_map) = root.get("models").and_then(Value::as_object) else {
        return Vec::new();
    };

    let deprecated_map = root.get("deprecatedModelIds").and_then(Value::as_object);

    // 1. Delineate dedicated non-agent lane models from upstream response arrays [INV-CATALOG-01]
    let mut dedicated_lane_ids: HashSet<&str> = HashSet::new();
    for lane_key in [
        "commandModelIds",
        "commitMessageModelIds",
        "imageGenerationModelIds",
        "audioTranscriptionModelIds",
    ] {
        if let Some(arr) = root.get(lane_key).and_then(Value::as_array) {
            for id in arr.iter().filter_map(Value::as_str) {
                dedicated_lane_ids.insert(id);
            }
        }
    }

    // 2. Discover curated agent order from upstream `agentModelSorts` [INV-CATALOG-01]
    let mut curated_order: Vec<&str> = Vec::new();
    if let Some(sorts) = root.get("agentModelSorts").and_then(Value::as_array) {
        for sort in sorts {
            if let Some(groups) = sort.get("groups").and_then(Value::as_array) {
                for group in groups {
                    if let Some(ids) = group.get("modelIds").and_then(Value::as_array) {
                        for id in ids.iter().filter_map(Value::as_str) {
                            curated_order.push(id);
                        }
                    }
                }
            }
        }
    }

    // 3. Generic parameter variant filter [INV-CATALOG-02]:
    // Suppress reasoning effort suffixes and synthetic tiering duplicates.
    fn is_effort_or_tier_variant(model_id: &str) -> bool {
        const SUFFIXES: &[&str] = &["-high", "-medium", "-low", "-minimal", "-max", "-tiered"];
        SUFFIXES.iter().any(|suffix| model_id.ends_with(suffix))
    }

    // Helper: is this an internal lane or non-text helper?
    fn is_internal_helper(model_id: &str) -> bool {
        model_id.starts_with("chat_")
            || model_id.starts_with("tab_")
            || model_id.starts_with("models/")
            || model_id.contains("image")
            || model_id.ends_with("-disabled")
    }

    let mut out = Vec::new();
    let mut emitted: HashSet<String> = HashSet::new();

    let mut ingest_model = |model_id: &str, mdata: &Value| {
        // [INV-CATALOG-03] Structural admission filter: zero legacy version blacklists.
        if is_internal_helper(model_id)
            || is_effort_or_tier_variant(model_id)
            || dedicated_lane_ids.contains(model_id)
            || deprecated_map.is_some_and(|dep| dep.contains_key(model_id))
        {
            return;
        }

        let context_window = mdata
            .get("maxTokens")
            .and_then(Value::as_u64)
            .map(|v| v as usize);
        let max_output_tokens = mdata
            .get("maxOutputTokens")
            .and_then(Value::as_u64)
            .map(|v| v as u32);
        let reasoning = mdata
            .get("supportsThinking")
            .and_then(Value::as_bool)
            .or(Some(true));
        let vision = mdata.get("supportsImages").and_then(Value::as_bool);

        // [INV-CATALOG-02] Lift reasoning capability to orthogonal effort ladder.
        // Google Gemini/Argon models support [minimal, low, medium, high].
        // Claude 5.5 models on Antigravity Vertex support [low, medium, high, max].
        let effort_levels = if reasoning == Some(true) {
            if model_id.starts_with("gemini-") || model_id.contains("argon") {
                Some(vec![
                    "minimal".to_string(),
                    "low".to_string(),
                    "medium".to_string(),
                    "high".to_string(),
                ])
            } else if model_id.starts_with("claude-") {
                Some(vec![
                    "low".to_string(),
                    "medium".to_string(),
                    "high".to_string(),
                    "max".to_string(),
                ])
            } else {
                None
            }
        } else {
            None
        };

        let discovered = DiscoveredModel {
            id: model_id.to_string(),
            availability: None,
            advertised: None,
            protocol: Some(nuo_model_codec::WireProtocol::GoogleGemini),
            endpoint: None,
            family: if model_id.starts_with("claude-") {
                Some("claude".to_string())
            } else {
                Some("google".to_string())
            },
            name: mdata
                .get("displayName")
                .or_else(|| mdata.get("display_name"))
                .and_then(Value::as_str)
                .map(str::to_string),
            context_window,
            max_output_tokens,
            reasoning,
            thinking: Some(if model_id.starts_with("claude-") {
                ReasoningSupport::AnthropicAdaptive
            } else {
                ReasoningSupport::ReasoningContent
            }),
            tool_call: Some(true),
            vision,
            effort_levels,
            catalog_source: None,
        };
        if emitted.insert(discovered.id.clone()) {
            out.push(discovered);
        }
    };

    // Prioritize curated agent models preserving upstream order [INV-CATALOG-01]
    for id in curated_order {
        if let Some(mdata) = models_map.get(id) {
            ingest_model(id, mdata);
        }
    }

    // Ingest any remaining models from the technical definitions dictionary
    for (model_id, mdata) in models_map {
        ingest_model(model_id, mdata);
    }

    out
}

#[cfg(test)]
mod tests {
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
        assert_eq!(
            ids,
            vec![
                "gemini-4-argon".to_string(),
                "gemini-3.8-flash".to_string(),
                "claude-sonnet-5-5".to_string(),
                "claude-opus-5-5".to_string(),
                "claude-sonnet-4-6".to_string(),
                "claude-opus-4-6-thinking".to_string(),
            ]
        );

        // Verify Gemini effort ladder [INV-CATALOG-02]
        let flash38 = discovered.iter().find(|m| m.id == "gemini-3.8-flash").unwrap();
        assert_eq!(
            flash38.effort_levels,
            Some(vec!["minimal".to_string(), "low".to_string(), "medium".to_string(), "high".to_string()])
        );

        // Verify Claude 5.5 effort ladder [INV-CATALOG-02]
        let sonnet55 = discovered.iter().find(|m| m.id == "claude-sonnet-5-5").unwrap();
        assert_eq!(
            sonnet55.effort_levels,
            Some(vec!["low".to_string(), "medium".to_string(), "high".to_string(), "max".to_string()])
        );
    }
}
