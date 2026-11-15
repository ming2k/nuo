//! Google native model-list parsing.

use serde_json::Value;

use super::super::DiscoveredModel;
use super::clean_display_label;

pub(super) fn parse_google_models(json: &Value) -> Vec<DiscoveredModel> {
    if let Some(models) = json.get("models").and_then(Value::as_array) {
        return models
            .iter()
            .filter_map(|entry| {
                // Only keep text-generation models. A Google model entry advertises
                // its capabilities via `supportedGenerationMethods`; entries that
                // list `generateContent` are the chat/text models an agent uses.
                // Embeddings/embedding-only and image/video models are excluded.
                let methods = entry
                    .get("supportedGenerationMethods")
                    .and_then(Value::as_array);
                let is_text = methods
                    .is_none_or(|arr| arr.iter().any(|m| m.as_str() == Some("generateContent")));
                if !is_text {
                    return None;
                }
                let context_window = entry
                    .get("inputTokenLimit")
                    .and_then(Value::as_u64)
                    .map(|v| v as usize);
                let max_output_tokens = entry
                    .get("outputTokenLimit")
                    .and_then(Value::as_u64)
                    .map(|v| v as u32);
                entry.get("name").and_then(Value::as_str).map(|name| {
                    let id = name.strip_prefix("models/").unwrap_or(name).to_string();
                    let reasoning = entry
                        .get("thinking")
                        .and_then(Value::as_bool)
                        .unwrap_or_else(|| {
                            id.contains("gemini-2.5")
                                || id.contains("gemini-3")
                                || id.contains("gemini-4")
                                || id.contains("argon")
                        });
                    DiscoveredModel {
                        id,
                        // Gemini's model resource carries the label in
                        // `displayName` (`name` is the `models/<id>` path), so
                        // the display label must not be read from `name` here.
                        name: entry
                            .get("displayName")
                            .and_then(Value::as_str)
                            .map(clean_display_label),
                        context_window,
                        max_output_tokens,
                        reasoning: reasoning.then_some(true),
                        family: Some("google".to_string()),
                        ..DiscoveredModel::default()
                    }
                })
            })
            .collect();
    }

    Vec::new()
}
