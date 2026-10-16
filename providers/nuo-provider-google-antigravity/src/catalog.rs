//! Google Antigravity (CloudCode) live model catalog parser (`/v1internal:fetchAvailableModels`).
//!
//! Implements [INV-CATALOG-01] (Curated Agent Surface Primacy), [INV-CATALOG-02]
//! (Orthogonal Capability Lifting & Parameter Suffix Folding), [INV-CATALOG-03]
//! (Abolition of Legacy Version Blacklists), and [INV-CATALOG-04] (Provider-Owned
//! Dialect Ingestion) per ADR-0068.

use std::collections::{HashMap, HashSet};
use nuo_model_codec::catalog::DiscoveredModel;
use nuo_model_codec::reasoning::ReasoningSupport;
use nuo_model_codec::Availability;
use serde_json::Value;

const EFFORT_OR_TIER_SUFFIXES: &[&str] = &[
    "-high",
    "-medium",
    "-low",
    "-minimal",
    "-max",
    "-tiered",
];

const DISPLAY_NAME_PARAM_SUFFIXES: &[&str] = &[
    " (High)",
    " (Medium)",
    " (Low)",
    " (Minimal)",
    " (Max)",
    " (Tiered)",
    " (Disabled)",
    " - High",
    " - Medium",
    " - Low",
    " - Minimal",
    " - Max",
    " - Tiered",
    " - Disabled",
];

struct CanonicalizedModelId<'a> {
    base_id: &'a str,
    effort_suffix: Option<&'a str>,
    is_disabled: bool,
}

fn canonicalize_model_id(raw_id: &str) -> CanonicalizedModelId<'_> {
    let mut id = raw_id;
    let mut is_disabled = false;
    if let Some(stripped) = id.strip_suffix("-disabled") {
        is_disabled = true;
        id = stripped;
    }

    let mut effort_suffix = None;
    if id.starts_with("gemini-") || id.starts_with("claude-") || id.contains("argon") {
        for &suffix in EFFORT_OR_TIER_SUFFIXES {
            if let Some(stripped) = id.strip_suffix(suffix) {
                effort_suffix = Some(&suffix[1..]);
                id = stripped;
                break;
            }
        }
    }

    CanonicalizedModelId {
        base_id: id,
        effort_suffix,
        is_disabled,
    }
}

fn clean_display_name(raw_name: &str) -> String {
    let mut name = raw_name.trim();
    for &suffix in DISPLAY_NAME_PARAM_SUFFIXES {
        if let Some(stripped) = name.strip_suffix(suffix) {
            name = stripped.trim();
            break;
        }
    }
    name.to_string()
}

fn is_internal_helper(id: &str) -> bool {
    id.starts_with("chat_")
        || id.starts_with("tab_")
        || id.starts_with("models/")
        || id.contains("image")
}

struct ModelFoldAccumulator {
    canonical_id: String,
    preferred_display_name: Option<String>,
    fallback_display_name: Option<String>,
    context_window: Option<usize>,
    max_output_tokens: Option<u32>,
    reasoning: Option<bool>,
    vision: Option<bool>,
    has_usable_variant: bool,
    has_disabled_variant: bool,
    observed_effort_suffixes: Vec<String>,
}

impl ModelFoldAccumulator {
    fn new(canonical_id: String) -> Self {
        Self {
            canonical_id,
            preferred_display_name: None,
            fallback_display_name: None,
            context_window: None,
            max_output_tokens: None,
            reasoning: None,
            vision: None,
            has_usable_variant: false,
            has_disabled_variant: false,
            observed_effort_suffixes: Vec::new(),
        }
    }

    fn absorb(
        &mut self,
        raw_id: &str,
        mdata: &Value,
        effort_suffix: Option<&str>,
        is_disabled: bool,
    ) {
        if is_disabled {
            self.has_disabled_variant = true;
        } else {
            self.has_usable_variant = true;
        }

        if let Some(suffix) = effort_suffix {
            if !self.observed_effort_suffixes.iter().any(|s| s == suffix) {
                self.observed_effort_suffixes.push(suffix.to_string());
            }
        }

        let raw_name = mdata
            .get("displayName")
            .or_else(|| mdata.get("display_name"))
            .and_then(Value::as_str);

        if let Some(name) = raw_name {
            let cleaned = clean_display_name(name);
            if raw_id == self.canonical_id {
                self.preferred_display_name = Some(cleaned);
            } else if self.fallback_display_name.is_none() {
                self.fallback_display_name = Some(cleaned);
            }
        }

        let ctx = mdata
            .get("maxTokens")
            .and_then(Value::as_u64)
            .map(|v| v as usize);
        if let Some(v) = ctx {
            self.context_window = Some(self.context_window.map_or(v, |curr| curr.max(v)));
        }

        let out_toks = mdata
            .get("maxOutputTokens")
            .and_then(Value::as_u64)
            .map(|v| v as u32);
        if let Some(v) = out_toks {
            self.max_output_tokens = Some(self.max_output_tokens.map_or(v, |curr| curr.max(v)));
        }

        let thinking = mdata
            .get("supportsThinking")
            .and_then(Value::as_bool)
            .or(Some(true));
        if thinking == Some(true) {
            self.reasoning = Some(true);
        } else if self.reasoning.is_none() {
            self.reasoning = thinking;
        }

        let images = mdata.get("supportsImages").and_then(Value::as_bool);
        if images == Some(true) {
            self.vision = Some(true);
        } else if self.vision.is_none() {
            self.vision = images;
        }
    }

    fn into_discovered(self) -> DiscoveredModel {
        let model_id = self.canonical_id;
        let reasoning = self.reasoning;

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

        let availability = if !self.has_usable_variant && self.has_disabled_variant {
            Some(Availability::locked(Some(
                "Requires subscription or account entitlement".to_string(),
            )))
        } else {
            None
        };

        let name = self.preferred_display_name.or(self.fallback_display_name);

        DiscoveredModel {
            id: model_id.clone(),
            availability,
            advertised: None,
            protocol: Some(nuo_model_codec::WireProtocol::GoogleGemini),
            endpoint: None,
            family: if model_id.starts_with("claude-") {
                Some("claude".to_string())
            } else {
                Some("google".to_string())
            },
            name,
            context_window: self.context_window,
            max_output_tokens: self.max_output_tokens,
            reasoning,
            thinking: Some(if model_id.starts_with("claude-") {
                ReasoningSupport::AnthropicAdaptive
            } else {
                ReasoningSupport::ReasoningContent
            }),
            tool_call: Some(true),
            vision: self.vision,
            effort_levels,
            catalog_source: None,
        }
    }
}

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

    // 2. Discover curated agent order from upstream `agentModelSorts` and `defaultAgentModelId` [INV-CATALOG-01]
    let mut curated_canonical_ids: Vec<String> = Vec::new();
    let mut curated_seen: HashSet<String> = HashSet::new();

    if let Some(sorts) = root.get("agentModelSorts").and_then(Value::as_array) {
        for sort in sorts {
            if let Some(groups) = sort.get("groups").and_then(Value::as_array) {
                for group in groups {
                    if let Some(ids) = group.get("modelIds").and_then(Value::as_array) {
                        for id in ids.iter().filter_map(Value::as_str) {
                            let canon = canonicalize_model_id(id).base_id.to_string();
                            if !is_internal_helper(&canon)
                                && !dedicated_lane_ids.contains(id)
                                && !dedicated_lane_ids.contains(canon.as_str())
                                && curated_seen.insert(canon.clone())
                            {
                                curated_canonical_ids.push(canon);
                            }
                        }
                    }
                }
            }
        }
    }

    if let Some(default_id) = root.get("defaultAgentModelId").and_then(Value::as_str) {
        let canon = canonicalize_model_id(default_id).base_id.to_string();
        if !is_internal_helper(&canon)
            && !dedicated_lane_ids.contains(default_id)
            && !dedicated_lane_ids.contains(canon.as_str())
            && curated_seen.insert(canon.clone())
        {
            curated_canonical_ids.push(canon);
        }
    }

    // 3. Fold all raw models from upstream definitions into canonical accumulators [INV-CATALOG-02, INV-CATALOG-03]
    let mut accumulators: HashMap<String, ModelFoldAccumulator> = HashMap::new();
    let mut fallback_order: Vec<String> = Vec::new();

    for (raw_id, mdata) in models_map {
        // [INV-CATALOG-03] Structural admission filter: zero legacy version blacklists.
        if is_internal_helper(raw_id)
            || dedicated_lane_ids.contains(raw_id.as_str())
            || deprecated_map.is_some_and(|dep| dep.contains_key(raw_id.as_str()))
        {
            continue;
        }

        let canon = canonicalize_model_id(raw_id);
        if is_internal_helper(canon.base_id)
            || dedicated_lane_ids.contains(canon.base_id)
            || deprecated_map.is_some_and(|dep| dep.contains_key(canon.base_id))
        {
            continue;
        }

        let base_id_str = canon.base_id.to_string();
        if !accumulators.contains_key(&base_id_str) {
            fallback_order.push(base_id_str.clone());
        }
        let acc = accumulators
            .entry(base_id_str)
            .or_insert_with_key(|id| ModelFoldAccumulator::new(id.clone()));
        acc.absorb(raw_id, mdata, canon.effort_suffix, canon.is_disabled);
    }

    // 4. Emit models adhering to [INV-CATALOG-01] (Curated Agent Surface Primacy)
    let mut out = Vec::new();
    let mut emitted: HashSet<String> = HashSet::new();

    if !curated_canonical_ids.is_empty() {
        for canon_id in curated_canonical_ids {
            if let Some(acc) = accumulators.remove(&canon_id) {
                if emitted.insert(acc.canonical_id.clone()) {
                    out.push(acc.into_discovered());
                }
            }
        }
    } else {
        for base_id in fallback_order {
            if let Some(acc) = accumulators.remove(&base_id) {
                if emitted.insert(acc.canonical_id.clone()) {
                    out.push(acc.into_discovered());
                }
            }
        }
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
        assert_eq!(ids, vec!["gemini-4-argon".to_string(), "gemini-3.8-flash".to_string()]);

        // Argon: entitlement locked because only -disabled variant was observed
        let argon = discovered.iter().find(|m| m.id == "gemini-4-argon").unwrap();
        assert_eq!(argon.name.as_deref(), Some("Gemini 4 Argon"));
        assert_eq!(
            argon.availability,
            Some(Availability::locked(Some(
                "Requires subscription or account entitlement".to_string()
            )))
        );
        assert_eq!(
            argon.effort_levels,
            Some(vec!["minimal".to_string(), "low".to_string(), "medium".to_string(), "high".to_string()])
        );

        // Flash 3.8: folded into canonical base ID, clean display name, usable availability
        let flash38 = discovered.iter().find(|m| m.id == "gemini-3.8-flash").unwrap();
        assert_eq!(flash38.name.as_deref(), Some("Gemini 3.8 Flash"));
        assert_eq!(flash38.availability, None);
        assert_eq!(flash38.context_window, Some(1048576));
        assert_eq!(flash38.max_output_tokens, Some(65536));
        assert_eq!(flash38.vision, Some(true));
        assert_eq!(
            flash38.effort_levels,
            Some(vec!["minimal".to_string(), "low".to_string(), "medium".to_string(), "high".to_string()])
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
}
