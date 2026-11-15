//! Anthropic, OpenAI label, and Google native catalogs.

use super::*;

#[test]
fn parses_anthropic_data_ids() {
    // Anthropic's /v1/models returns the same {data:[{id}]} shape; no
    // capability fields are advertised. The `display_name` it rides along
    // is ingested as the presentation-only label (never the identity).
    let json = serde_json::json!({
        "data": [
            { "id": "claude-opus-4-8", "display_name": "Claude Opus 4.8" },
            { "id": "claude-sonnet-5", "display_name": "Claude Sonnet 5" }
        ]
    });
    let models = parse_models(CatalogShape::Anthropic, &json);
    let mut got: Vec<String> = models.iter().map(|model| model.id.clone()).collect();
    got.sort();
    assert_eq!(got, vec!["claude-opus-4-8", "claude-sonnet-5"]);
    assert_eq!(models[0].context_window, None);
    assert_eq!(models[0].reasoning, None);
    assert_eq!(models[0].name.as_deref(), Some("Claude Opus 4.8"));
}

#[test]
fn openai_data_models_carry_no_label_when_the_endpoint_publishes_none() {
    // The stock OpenAI-compatible `/models` shape is `{id, object,
    // created, owned_by}` — no label. Absence must stay `None` (the
    // surfaces then render the bare id), not an empty string.
    let json = serde_json::json!({
        "data": [
            { "id": "glm-5.2", "object": "model", "owned_by": "zhipu" }
        ]
    });
    let models = parse_models(CatalogShape::OpenAi, &json);
    assert_eq!(models[0].id, "glm-5.2");
    assert_eq!(models[0].name, None);
    assert_eq!(models[0].remote_metadata().name, None);
}

#[test]
fn a_label_repeating_the_id_collapses_to_none() {
    // Kimi advertises `display_name` equal to the id (`"k3"`), and a
    // models.dev entry may repeat its id as `name`. Storing that would
    // render the same string twice on the row, so it collapses to "no
    // label" and the surfaces show the bare id.
    let json = serde_json::json!({
        "data": [
            { "id": "k3", "display_name": "k3" },
            { "id": "glm-5.2", "display_name": "  " }
        ]
    });
    let models = parse_models(CatalogShape::OpenAi, &json);
    assert_eq!(models[0].name.as_deref(), Some("k3"));
    assert_eq!(models[0].remote_metadata().name, None);
    assert_eq!(models[1].remote_metadata().name, None);
}

#[test]
fn google_display_name_is_read_from_display_name_not_the_resource_path() {
    // Gemini's `name` is the resource path (`models/gemini-2.5-pro`) that
    // becomes the id; the human label is a separate `displayName` field.
    // Reading the label from `name` would echo the id back.
    let json = serde_json::json!({
        "models": [{
            "name": "models/gemini-2.5-pro",
            "displayName": "Gemini 2.5 Pro",
            "supportedGenerationMethods": ["generateContent"]
        }]
    });
    let models = parse_models(CatalogShape::Google, &json);
    assert_eq!(models[0].id, "gemini-2.5-pro");
    assert_eq!(models[0].name.as_deref(), Some("Gemini 2.5 Pro"));
}

#[test]
fn google_models_parse_token_limits_and_reasoning() {
    let json = serde_json::json!({
        "models": [{
            "name": "models/gemini-2.5-flash",
            "displayName": "Gemini 2.5 Flash",
            "inputTokenLimit": 1048576,
            "outputTokenLimit": 65536,
            "supportedGenerationMethods": ["generateContent"]
        }]
    });
    let models = parse_models(CatalogShape::Google, &json);
    assert_eq!(models[0].id, "gemini-2.5-flash");
    assert_eq!(models[0].context_window, Some(1048576));
    assert_eq!(models[0].max_output_tokens, Some(65536));
    assert_eq!(models[0].reasoning, Some(true));
    assert_eq!(models[0].family.as_deref(), Some("google"));
}

#[test]
fn google_models_parse_gemini_4_argon_with_thinking_bool() {
    let json = serde_json::json!({
        "models": [{
            "name": "models/gemini-4-argon",
            "displayName": "Gemini 4 Argon",
            "inputTokenLimit": 1048576,
            "outputTokenLimit": 32768,
            "thinking": true,
            "supportedGenerationMethods": ["generateContent"]
        }]
    });
    let models = parse_models(CatalogShape::Google, &json);
    assert_eq!(models[0].id, "gemini-4-argon");
    assert_eq!(models[0].name.as_deref(), Some("Gemini 4 Argon"));
    assert_eq!(models[0].context_window, Some(1048576));
    assert_eq!(models[0].max_output_tokens, Some(32768));
    assert_eq!(models[0].reasoning, Some(true));
    assert_eq!(models[0].family.as_deref(), Some("google"));
}

#[test]
fn parses_google_models_stripping_prefix_and_filtering_non_text() {
    let json = serde_json::json!({
        "models": [
            {
                "name": "models/gemini-2.5-flash",
                "supportedGenerationMethods": ["generateContent", "countTokens"]
            },
            {
                "name": "models/gemini-2.5-pro",
                "supportedGenerationMethods": ["generateContent"]
            },
            // An embedding-only model must be excluded.
            {
                "name": "models/text-embedding-004",
                "supportedGenerationMethods": ["embedContent"]
            },
            // A model with no methods array is kept (best-effort).
            { "name": "models/gemini-3-pro-preview" }
        ]
    });
    let mut got: Vec<String> = parse_models(CatalogShape::Google, &json)
        .into_iter()
        .map(|model| model.id)
        .collect();
    got.sort();
    assert_eq!(
        got,
        vec!["gemini-2.5-flash", "gemini-2.5-pro", "gemini-3-pro-preview"]
    );
}
