//! Catalog-shape validation, wire mapping, and label hygiene.

use super::*;

#[test]
fn catalog_shape_validation_distinguishes_empty_from_malformed() {
    assert!(
        validate_catalog_shape(CatalogShape::OpenAi, &serde_json::json!({ "data": [] })).is_ok()
    );
    assert!(matches!(
        validate_catalog_shape(
            CatalogShape::OpenAi,
            &serde_json::json!({ "error": "unauthorized" })
        ),
        Err(ModelListError::Parse(_))
    ));
    assert!(matches!(
        validate_catalog_shape(
            CatalogShape::Google,
            &serde_json::json!({ "error": "bad key" })
        ),
        Err(ModelListError::Parse(_))
    ));
}

#[test]
fn catalog_shape_maps_wire_protocols() {
    assert_eq!(
        CatalogShape::from_wire_protocol(WireProtocol::AnthropicMessages),
        CatalogShape::Anthropic
    );
    assert_eq!(
        CatalogShape::from_wire_protocol(WireProtocol::GoogleGenerateContent),
        CatalogShape::Google
    );
    assert_eq!(
        CatalogShape::from_wire_protocol(WireProtocol::GoogleInteractions),
        CatalogShape::Google
    );
    assert_eq!(
        CatalogShape::from_wire_protocol(WireProtocol::OpenAiChatCompletions),
        CatalogShape::OpenAi
    );
    assert_eq!(
        CatalogShape::from_wire_protocol(WireProtocol::OpenAiResponses),
        CatalogShape::OpenAi
    );
}

#[test]
fn clean_display_label_strips_thinking_suffixes() {
    assert_eq!(
        clean_display_label("Claude Opus 4.6 (Thinking)"),
        "Claude Opus 4.6"
    );
    assert_eq!(
        clean_display_label("Claude Sonnet 4.6 (thinking)"),
        "Claude Sonnet 4.6"
    );
    assert_eq!(
        clean_display_label("Claude Opus 4.6(Thinking)"),
        "Claude Opus 4.6"
    );
    assert_eq!(
        clean_display_label("Claude Opus 4.6 - Thinking"),
        "Claude Opus 4.6"
    );
    assert_eq!(
        clean_display_label("Claude Opus 4.6 [Thinking]"),
        "Claude Opus 4.6"
    );
}
