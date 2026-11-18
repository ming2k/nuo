//! ChatGPT Plan (Codex subscription) remote catalog discovery and parsing.

use inference_wire::WireProtocol;
use protocol::{ApiRoot, Availability, ReasoningSupport};
use provider::catalog::{
    DiscoveredModel, ModelListError, RemoteCatalogOptions, RemoteCatalogRequest,
    RemoteCatalogUpdate, append_query, finalize_catalog_response,
};
use serde_json::Value;

/// Emulated Codex CLI version sent on the `/models` query string.
pub const CODEX_VERSION: &str = "0.153.4";

/// Fetch and parse the ChatGPT Codex `/models` catalog.
pub async fn fetch_remote_catalog(
    req: RemoteCatalogRequest<'_>,
    options: RemoteCatalogOptions<'_>,
) -> Result<RemoteCatalogUpdate, ModelListError> {
    let root = ApiRoot::parse(req.base_url).map_err(ModelListError::BadEndpoint)?;
    let endpoint = append_query(&root.append("models"), &[("client_version", CODEX_VERSION)]);
    let user_agent = req.user_agent.unwrap_or(provider::NUO_USER_AGENT);

    let mut request = provider::transport::Request::new(netune::Method::GET, &endpoint)
        .header("user-agent", user_agent)
        .header("originator", "codex_cli_rs");
    if !req.api_key.expose_secret().trim().is_empty() {
        request = request.header(
            "authorization",
            format!("Bearer {}", req.api_key.expose_secret()),
        );
    }
    if let Some(account_id) = req.account_id {
        request = request.header("chatgpt-account-id", account_id);
    }
    if let Some(etag) = options.etag {
        request = request.header("if-none-match", etag);
    }
    for (name, value) in req.extra_headers {
        request = request.header(name, *value);
    }

    let response = provider::transport::Http::control_plane()
        .map_err(ModelListError::Http)?
        .send(request)
        .await
        .map_err(ModelListError::Http)?;

    // Codex has semantic curation order (priority), so preserve order while deduping.
    finalize_catalog_response(response, options, false, |json| {
        if !json.get("models").is_some_and(Value::is_array) {
            return Err(ModelListError::Parse(
                "response is missing the required models array".to_string(),
            ));
        }
        Ok(parse_codex_models(json))
    })
}

pub fn parse_codex_models(json: &Value) -> Vec<DiscoveredModel> {
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
                        .map(protocol::clean_model_display_name),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_catalog_parses_visibility_api_support_and_priority() {
        let json = serde_json::json!({
            "models": [
                {
                    "slug": "second-listed",
                    "priority": 2,
                    "visibility": "list",
                    "supported_in_api": true,
                    "supported_reasoning_levels": [{"effort": "low"}, {"effort": "high"}],
                    "max_context_window": 272000
                },
                {
                    "slug": "hidden-helper",
                    "priority": 0,
                    "visibility": "hide",
                    "supported_in_api": true,
                    "supported_reasoning_levels": []
                },
                {
                    "slug": "first-listed",
                    "priority": 1,
                    "visibility": "list",
                    "supported_in_api": false,
                    "input_modalities": ["text"]
                }
            ]
        });

        let models = parse_codex_models(&json);
        let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["hidden-helper", "first-listed", "second-listed"]);
        assert_eq!(models[0].advertised, Some(false));
        assert_eq!(
            models[0].availability.as_ref().map(|a| a.usable),
            Some(true)
        );
        assert_eq!(models[1].advertised, Some(true));
        assert_eq!(
            models[1].availability.as_ref().map(|a| a.usable),
            Some(false)
        );
        assert_eq!(models[1].vision, Some(false));
        assert_eq!(models[2].context_window, Some(272_000));
        assert_eq!(
            models[2].effort_levels.as_deref(),
            Some(&["low".to_string(), "high".to_string()][..])
        );
    }
}
