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
            CatalogShape::Google,
            "https://relay.example/team/v1beta",
            "https://relay.example/team/v1beta/models",
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
