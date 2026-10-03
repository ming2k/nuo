//! Tool argument pre-validation.
//!
//! Minimal structural validation of a tool call's `arguments` (a JSON string)
//! against the top-level shape declared in the tool's `parameters` schema,
//! performed by the dispatch layer *before* the [`Tool`](crate::Tool) impl
//! runs. This is **not** a full JSON Schema implementation — it covers the
//! common "did the model send the right top-level shape" checks, which catch
//! the vast majority of bad-arguments bugs without pulling in a
//! schema-validation dependency. Ported from praxion's `tool/validation.rs`
//! (including its integer/number matching fix) and extended with top-level
//! `required` / per-property primitive checks.
//!
//! Anything beyond (nested object shapes, `additionalProperties`, `format`,
//! `enum`, semantic rules) stays the Tool impl's responsibility — every tool
//! still parses and validates `arguments` itself; this layer never bypasses
//! that, it only rejects calls it is sure violate the declared shape.

use serde_json::Value;

/// Validate the top-level shape of `arguments` against `schema`
/// ([`Tool::parameters`](crate::Tool::parameters)). Returns `Err` with a
/// model-actionable message when the arguments clearly violate the schema;
/// the dispatch layer turns that into the same error shape a failing Tool
/// impl would produce, and the tool never runs.
///
/// Rules checked, in order:
///
/// - An **unparseable** argument string passes: the Tool impl's own parse
///   error already reports that case in the tool's own words.
/// - If the schema declares a top-level `"type"`, the arguments' JSON type
///   must match. An integer value matches an `"integer"` schema and is also
///   accepted for a `"number"` schema; a fractional number never matches
///   `"integer"`. Top-level `null` is tolerated for `"object"` schemas (some
///   providers send `null` instead of `{}` for no-arg tools).
/// - If the arguments are an object, every key in the schema's top-level
///   `"required"` must be present (JSON Schema `required` is key presence).
/// - If the arguments are an object, every present property named in the
///   schema's top-level `"properties"` must match that property's declared
///   primitive `"type"` (`string`/`number`/`integer`/`boolean`/`array`/
///   `object`), with the same integer/number rule as the top level. Explicit
///   `null` property values are skipped (providers send `null` for unset
///   optional fields, mirroring the top-level `null`-for-`object` rule).
///
/// Checks are **not** recursive: a property typed `"object"` is checked to be
/// an object, but its nested contents are left to the Tool impl. A schema
/// without a top-level `"type"`/`"required"`/`"properties"` admits anything.
pub fn validate_tool_arguments(schema: &Value, arguments: &str) -> Result<(), String> {
    let Ok(args) = serde_json::from_str::<Value>(arguments) else {
        return Ok(());
    };

    // Top-level type check.
    if let Some(expected) = schema.get("type").and_then(Value::as_str) {
        let actual = json_type_of(&args);
        if !(expected == "object" && actual == "null") {
            let matches = expected == actual || (expected == "number" && actual == "integer");
            if !matches {
                return Err(format!(
                    "invalid arguments: expected type `{expected}`, got `{actual}`"
                ));
            }
        }
    }

    // `required` and per-property checks only apply to object arguments.
    let Some(object) = args.as_object() else {
        return Ok(());
    };

    if let Some(required) = schema.get("required").and_then(Value::as_array) {
        let missing: Vec<&str> = required
            .iter()
            .filter_map(Value::as_str)
            .filter(|name| !object.contains_key(*name))
            .collect();
        if !missing.is_empty() {
            let names = missing
                .iter()
                .map(|name| format!("`{name}`"))
                .collect::<Vec<_>>()
                .join(", ");
            return Err(format!(
                "invalid arguments: missing required field(s): {names}"
            ));
        }
    }

    if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
        for (key, value) in object {
            if value.is_null() {
                continue;
            }
            let Some(expected) = properties
                .get(key)
                .and_then(|sub| sub.get("type"))
                .and_then(Value::as_str)
            else {
                continue;
            };
            let actual = json_type_of(value);
            let matches = expected == actual || (expected == "number" && actual == "integer");
            if !matches {
                return Err(format!(
                    "invalid argument `{key}`: expected type `{expected}`, got `{actual}`"
                ));
            }
        }
    }

    Ok(())
}

/// Return the JSON Schema type name for `value`, or `"null"` for
/// `Value::Null`. Integers (any non-fractional number `serde_json` can
/// represent exactly) report as `"integer"`, floats as `"number"`.
fn json_type_of(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(n) => {
            if n.is_i64() {
                "integer"
            } else {
                "number"
            }
        }
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Table-driven coverage of the validation contract. Each case is
    /// `(schema, arguments, expected)`: `Ok(())` admits the call,
    /// `Err(substring)` rejects it with a message containing the substring.
    #[test]
    fn validation_table() {
        let cases: Vec<(Value, &str, Result<(), &str>)> = vec![
            // Top-level type matching
            (json!({"type": "object"}), "{}", Ok(())),
            (
                json!({"type": "object"}),
                "[1, 2]",
                Err("expected type `object`, got `array`"),
            ),
            (
                json!({"type": "object"}),
                "\"hi\"",
                Err("expected type `object`, got `string`"),
            ),
            // Some providers send `null` instead of `{}` for no-arg tools.
            (json!({"type": "object"}), "null", Ok(())),
            (json!({"type": "string"}), "\"hi\"", Ok(())),
            (
                json!({"type": "string"}),
                "42",
                Err("expected type `string`, got `integer`"),
            ),
            (json!({"type": "array"}), "[1]", Ok(())),
            (json!({"type": "boolean"}), "true", Ok(())),
            // integer vs number (praxion fix #11 semantics)
            // An integer value matches an `integer` schema...
            (json!({"type": "integer"}), "42", Ok(())),
            // ...and is also accepted for a `number` schema.
            (json!({"type": "number"}), "42", Ok(())),
            // A fractional value never matches an `integer` schema.
            (
                json!({"type": "integer"}),
                "4.2",
                Err("expected type `integer`, got `number`"),
            ),
            (json!({"type": "number"}), "4.2", Ok(())),
            // Empty / typeless schema admits anything
            (json!({}), "123", Ok(())),
            (json!({}), "[1, 2]", Ok(())),
            // required: key presence, top level only
            (
                json!({"type": "object", "required": ["path"]}),
                "{}",
                Err("missing required field(s): `path`"),
            ),
            (
                json!({"type": "object", "required": ["path", "content"]}),
                "{\"path\": \"f.rs\"}",
                Err("missing required field(s): `content`"),
            ),
            (
                json!({"type": "object", "required": ["path"]}),
                "{\"path\": \"f.rs\"}",
                Ok(()),
            ),
            // Empty required list admits anything.
            (json!({"type": "object", "required": []}), "{}", Ok(())),
            // Per-property primitive type checks
            (
                json!({"type": "object", "properties": {"command": {"type": "string"}}}),
                "{\"command\": 123}",
                Err("invalid argument `command`: expected type `string`, got `integer`"),
            ),
            (
                json!({"type": "object", "properties": {"command": {"type": "string"}}}),
                "{\"command\": \"ls\"}",
                Ok(()),
            ),
            (
                json!({"type": "object", "properties": {"limit": {"type": "integer"}}}),
                "{\"limit\": 3}",
                Ok(()),
            ),
            (
                json!({"type": "object", "properties": {"limit": {"type": "integer"}}}),
                "{\"limit\": 3.5}",
                Err("invalid argument `limit`: expected type `integer`, got `number`"),
            ),
            (
                json!({"type": "object", "properties": {"limit": {"type": "number"}}}),
                "{\"limit\": 3}",
                Ok(()),
            ),
            (
                json!({"type": "object", "properties": {"recursive": {"type": "boolean"}}}),
                "{\"recursive\": \"yes\"}",
                Err("invalid argument `recursive`: expected type `boolean`, got `string`"),
            ),
            (
                json!({"type": "object", "properties": {"items": {"type": "array"}}}),
                "{\"items\": {}}",
                Err("invalid argument `items`: expected type `array`, got `object`"),
            ),
            // Explicit null is tolerated for property values (unset optional).
            (
                json!({"type": "object", "properties": {"ext": {"type": "string"}}}),
                "{\"ext\": null}",
                Ok(()),
            ),
            // Properties absent from the schema are not checked
            // (`additionalProperties` stays the Tool impl's business).
            (
                json!({"type": "object", "properties": {"a": {"type": "string"}}}),
                "{\"a\": \"x\", \"b\": 123}",
                Ok(()),
            ),
            // No recursion into nested objects
            // `opts` must be an object, but its nested `required`/`properties`
            // are NOT validated by this layer.
            (
                json!({
                    "type": "object",
                    "properties": {
                        "opts": {
                            "type": "object",
                            "properties": {"n": {"type": "integer"}},
                            "required": ["n"]
                        }
                    }
                }),
                "{\"opts\": {}}",
                Ok(()),
            ),
            (
                json!({
                    "type": "object",
                    "properties": {
                        "opts": {
                            "type": "object",
                            "properties": {"n": {"type": "integer"}},
                            "required": ["n"]
                        }
                    }
                }),
                "{\"opts\": {\"n\": \"wrong\"}}",
                Ok(()),
            ),
            (
                json!({"type": "object", "properties": {"opts": {"type": "object"}}}),
                "{\"opts\": []}",
                Err("invalid argument `opts`: expected type `object`, got `array`"),
            ),
            // required/properties with no top-level "type" still apply
            (
                json!({"required": ["a"]}),
                "{}",
                Err("missing required field(s): `a`"),
            ),
            // Unparseable argument strings pass through to the Tool impl
            (json!({"type": "object", "required": ["a"]}), "", Ok(())),
            (json!({"type": "object"}), "{not json", Ok(())),
        ];

        for (schema, arguments, expected) in cases {
            let result = validate_tool_arguments(&schema, arguments);
            match (&result, expected) {
                (Ok(()), Ok(())) => {}
                (Err(message), Err(substring)) => assert!(
                    message.contains(substring),
                    "schema {schema}, args {arguments:?}: error {message:?} must contain {substring:?}"
                ),
                _ => panic!(
                    "schema {schema}, args {arguments:?}: got {result:?}, expected {expected:?}"
                ),
            }
        }
    }
}
