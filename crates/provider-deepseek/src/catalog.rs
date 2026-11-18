//! DeepSeek live catalog discovery and effort-ladder expansion (ADR-0083).

use protocol::{ReasoningSupport, clean_model_display_name};
use provider::catalog::{
    DiscoveredModel, ModelListError, RemoteCatalogOptions, RemoteCatalogRequest,
    RemoteCatalogUpdate, fetch_bearer_models_response, finalize_catalog_response,
};
use serde_json::Value;

/// Fetch and parse DeepSeek's `/models` catalog, expanding advertised effort
/// rungs with DeepSeek's documented compatibility aliases.
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
        Ok(parse_deepseek_models(json))
    })
}

pub fn parse_deepseek_models(json: &Value) -> Vec<DiscoveredModel> {
    let Some(entries) = json.get("data").and_then(Value::as_array) else {
        return Vec::new();
    };
    entries
        .iter()
        .filter_map(|entry| {
            let id = entry.get("id").and_then(Value::as_str)?.to_string();
            let deepseek_effort = entry.get("effort").filter(|value| value.is_object());
            let effort_levels = deepseek_effort
                .and_then(|value| value.get("supported_levels"))
                .and_then(Value::as_array)
                .map(|levels| {
                    let raw: Vec<String> = levels
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect();
                    deepseek_accepted_efforts(&raw)
                });
            let reasoning = deepseek_effort.map(|_| true);
            let vision = entry
                .get("input_modalities")
                .and_then(Value::as_array)
                .map(|modalities| modalities.iter().any(|v| v.as_str() == Some("image")));
            Some(DiscoveredModel {
                id,
                name: entry
                    .get("display_name")
                    .or_else(|| entry.get("name"))
                    .and_then(Value::as_str)
                    .map(clean_model_display_name),
                context_window: entry
                    .get("context_window")
                    .or_else(|| entry.get("context_length"))
                    .and_then(Value::as_u64)
                    .map(|l| l as usize),
                max_output_tokens: entry
                    .get("max_output_tokens")
                    .and_then(Value::as_u64)
                    .and_then(|l| u32::try_from(l).ok()),
                reasoning,
                thinking: deepseek_effort.map(|_| ReasoningSupport::ReasoningContent),
                vision,
                effort_levels,
                ..DiscoveredModel::default()
            })
        })
        .collect()
}

pub fn deepseek_accepted_efforts(advertised: &[String]) -> Vec<String> {
    /// Documented compatibility aliases: `(alias rung, rung the vendor serves)`.
    const ALIASES: &[(&str, &str)] = &[
        ("minimal", "low"),
        ("medium", "high"),
        ("xhigh", "high"),
        ("ultra", "max"),
    ];
    let mut accepted: Vec<String> = Vec::with_capacity(advertised.len() + ALIASES.len() + 1);
    accepted.push("none".to_string());
    accepted.extend(advertised.iter().filter(|level| *level != "none").cloned());
    for (alias, target) in ALIASES {
        if advertised.iter().any(|level| level == target) {
            accepted.push((*alias).to_string());
        }
    }
    accepted.sort_by_key(|level| {
        protocol::Effort::ORDER
            .iter()
            .position(|rung| rung.as_str() == level)
            .unwrap_or(usize::MAX)
    });
    accepted.dedup();
    accepted
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let models = parse_deepseek_models(&json);
        assert_eq!(models.len(), 2);

        let flash = &models[0];
        assert_eq!(flash.id, "deepseek-flash");
        assert_eq!(flash.context_window, Some(1_048_576));
        assert_eq!(flash.max_output_tokens, Some(393_216));
        assert_eq!(flash.reasoning, Some(true));
        assert_eq!(flash.thinking, Some(ReasoningSupport::ReasoningContent));
        assert_eq!(flash.vision, Some(true));
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
        assert_eq!(pro.vision, Some(false));
    }

    #[test]
    fn deepseek_discovery_expands_exactly_to_the_compiled_ladder() {
        let baseline: Vec<String> = provider::effort_ladders::DEEPSEEK_NATIVE
            .iter()
            .map(|rung| rung.as_str().to_string())
            .collect();
        assert_eq!(
            deepseek_accepted_efforts(&["low".to_string(), "high".to_string(), "max".to_string()]),
            baseline
        );
        assert_eq!(
            deepseek_accepted_efforts(&["low".to_string()]),
            vec!["none".to_string(), "minimal".to_string(), "low".to_string()]
        );
        assert_eq!(
            deepseek_accepted_efforts(&["max".to_string()]),
            vec!["none".to_string(), "max".to_string(), "ultra".to_string()]
        );
    }
}
