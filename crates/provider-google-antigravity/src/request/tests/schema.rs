//! Google function-schema sanitation.

use super::*;

#[test]
fn sanitize_schema_converts_const_to_enum_and_type() {
    let input = json!({
        "const": "execute_task"
    });
    let sanitized = sanitize_schema(&input);
    assert_eq!(sanitized["type"], "string");
    assert_eq!(sanitized["enum"], json!(["execute_task"]));
    assert!(sanitized.get("const").is_none());
}

#[test]
fn sanitize_schema_converts_one_of_literals_to_flat_enum() {
    let input = json!({
        "oneOf": [
            { "const": "list" },
            { "const": "kill" },
            { "const": "kill_all" }
        ]
    });
    let sanitized = sanitize_schema(&input);
    assert_eq!(sanitized["type"], "string");
    assert_eq!(sanitized["enum"], json!(["list", "kill", "kill_all"]));
    assert!(sanitized.get("oneOf").is_none());
    assert!(sanitized.get("anyOf").is_none());
}

#[test]
fn sanitize_schema_converts_one_of_objects_to_any_of() {
    let input = json!({
        "one_of": [
            {
                "type": "object",
                "properties": {
                    "kind": { "const": "text" },
                    "content": { "type": "string" }
                },
                "required": ["kind", "content"]
            },
            {
                "type": "object",
                "properties": {
                    "kind": { "const": "image" },
                    "url": { "type": "string" }
                },
                "required": ["kind", "url"]
            }
        ]
    });
    let sanitized = sanitize_schema(&input);
    assert!(sanitized.get("one_of").is_none());
    assert!(sanitized.get("oneOf").is_none());
    let any_of = sanitized["anyOf"].as_array().expect("must be array");
    assert_eq!(any_of.len(), 2);
    assert_eq!(any_of[0]["type"], "object");
    assert_eq!(any_of[0]["properties"]["kind"]["enum"], json!(["text"]));
    assert!(any_of[0]["properties"]["kind"].get("const").is_none());
    assert_eq!(any_of[1]["properties"]["kind"]["enum"], json!(["image"]));
}

#[test]
fn sanitize_schema_handles_all_of_merging() {
    let input = json!({
        "allOf": [
            {
                "type": "object",
                "properties": { "id": { "type": "string" } },
                "required": ["id"]
            },
            {
                "type": "object",
                "properties": { "name": { "type": "string" } },
                "required": ["name"]
            }
        ]
    });
    let sanitized = sanitize_schema(&input);
    assert_eq!(sanitized["type"], "object");
    assert!(sanitized["properties"]["id"].is_object());
    assert!(sanitized["properties"]["name"].is_object());
    assert_eq!(sanitized["required"], json!(["id", "name"]));
    assert!(sanitized.get("allOf").is_none());
}

#[test]
fn sanitize_schema_handles_nullable_type_array() {
    let input = json!({
        "type": ["string", "null"],
        "description": "An optional string property",
        "title": "Ignored Title"
    });
    let sanitized = sanitize_schema(&input);
    assert_eq!(sanitized["type"], "string");
    assert_eq!(sanitized["nullable"], true);
    assert_eq!(sanitized["description"], "An optional string property");
    assert!(sanitized.get("title").is_none());
}

#[test]
fn sanitize_schema_strips_forbidden_meta_fields() {
    let input = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "properties": {
            "count": { "type": "integer", "default": 0, "minimum": 0 }
        },
        "additionalProperties": false,
        "patternProperties": { "^x-": { "type": "string" } }
    });
    let sanitized = sanitize_schema(&input);
    assert_eq!(sanitized["type"], "object");
    assert!(sanitized["properties"]["count"].is_object());
    assert!(sanitized.get("$schema").is_none());
    assert!(sanitized.get("additionalProperties").is_none());
    assert!(sanitized.get("patternProperties").is_none());
    assert!(sanitized["properties"]["count"].get("default").is_none());
    assert!(sanitized["properties"]["count"].get("minimum").is_none());
}

#[test]
fn sanitize_schema_handles_deeply_nested_subagent_items_with_const() {
    // Reproduce exact wire path:
    // parameters.properties[0].value.items.one_of[0].properties[0].value (const)
    let input = json!({
        "type": "object",
        "properties": {
            "Subagents": {
                "type": "array",
                "items": {
                    "one_of": [
                        {
                            "type": "object",
                            "properties": {
                                "TypeName": {
                                    "type": "string",
                                    "const": "researcher"
                                },
                                "Role": {
                                    "type": "string"
                                }
                            },
                            "required": ["TypeName", "Role"]
                        }
                    ]
                }
            }
        }
    });
    let sanitized = sanitize_schema(&input);
    assert_eq!(sanitized["type"], "object");
    let subagents = &sanitized["properties"]["Subagents"];
    assert_eq!(subagents["type"], "array");
    let items = &subagents["items"];
    assert!(items.get("one_of").is_none());
    let any_of = items["anyOf"].as_array().expect("anyOf array");
    assert_eq!(any_of.len(), 1);
    let type_name_prop = &any_of[0]["properties"]["TypeName"];
    assert_eq!(type_name_prop["type"], "string");
    assert_eq!(type_name_prop["enum"], json!(["researcher"]));
    assert!(type_name_prop.get("const").is_none());
}
