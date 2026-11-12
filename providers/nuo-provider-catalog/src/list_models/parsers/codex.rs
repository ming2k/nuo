//! ChatGPT Codex catalog parsing.

use nuo_inference_wire::WireProtocol;
use nuo_provider::{Availability, ReasoningSupport};
use serde_json::Value;

use super::super::DiscoveredModel;
use super::clean_display_label;

pub(super) fn parse_codex_models(json: &Value) -> Vec<DiscoveredModel> {
    let Some(entries) = json.get("models").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut models: Vec<(i64, usize, DiscoveredModel)> = entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            let id = entry.get("slug").and_then(Value::as_str)?.to_string();
            let effort_levels: Vec<String> = entry
                .get("supported_reasoning_levels")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|level| level.get("effort").and_then(Value::as_str))
                .map(str::to_string)
                .collect();
            let reasoning = !effort_levels.is_empty();
            // Two independent declarations, never ANDed into one bit
            // `visibility` says whether the model is *listed*, and
            // `supported_in_api` says whether the API will *run* it. The
            // `hidden-helper` case — listed nowhere, yet API-supported — is
            // exactly why they must not be conflated.
            let advertised = entry
                .get("visibility")
                .and_then(Value::as_str)
                .map(|visibility| visibility == "list");
            let availability =
                entry
                    .get("supported_in_api")
                    .and_then(Value::as_bool)
                    .map(|usable| Availability {
                        usable,
                        status_label: None,
                        reason: None,
                        resets_at_ms: None,
                    });
            let vision = entry
                .get("input_modalities")
                .and_then(Value::as_array)
                // Codex treats an omitted legacy field as text + image.
                .is_none_or(|modalities| {
                    modalities
                        .iter()
                        .any(|modality| modality.as_str() == Some("image"))
                });
            let context_window = entry
                .get("max_context_window")
                .or_else(|| entry.get("context_window"))
                .and_then(Value::as_i64)
                .and_then(|window| usize::try_from(window).ok());
            Some((
                entry
                    .get("priority")
                    .and_then(Value::as_i64)
                    .unwrap_or(i64::MAX),
                index,
                DiscoveredModel {
                    id,
                    availability,
                    advertised,
                    protocol: Some(WireProtocol::OpenAiResponses),
                    endpoint: None,
                    family: None,
                    name: entry
                        .get("display_name")
                        .or_else(|| entry.get("name"))
                        .and_then(Value::as_str)
                        .map(clean_display_label),
                    context_window,
                    max_output_tokens: None,
                    reasoning: Some(reasoning),
                    thinking: Some(if reasoning {
                        ReasoningSupport::ReasoningSummary
                    } else {
                        ReasoningSupport::None
                    }),
                    tool_call: Some(true),
                    vision: Some(vision),
                    effort_levels: Some(effort_levels),
                    catalog_source: None,
                    wire_entities: None,
                },
            ))
        })
        .collect();
    models.sort_by_key(|(priority, index, _)| (*priority, *index));
    models.into_iter().map(|(_, _, model)| model).collect()
}
