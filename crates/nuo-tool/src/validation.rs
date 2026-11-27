//! Tool argument preparation: parse, coerce, validate.
//!
//! This is the single argument boundary for every tool call. It replaces the
//! former top-level-only shape check, which let malformed and mistyped model
//! output reach each tool's own `serde_json::from_value` and surface as a raw
//! deserializer error.
//!
//! The pipeline is deliberately ordered:
//!
//! 1. **Parse** the raw string. Unparseable arguments are a model error and
//!    are reported as one, not forwarded to the tool.
//! 2. **Coerce** only where the schema makes the intent unambiguous: numeric
//!    strings for numeric fields, `"true"`/`"false"` for booleans, a single
//!    value for an array field, a JSON-encoded object for an object field, and
//!    optional `null` placeholders removed. Every repair is recorded so the
//!    dispatcher can tell the model what was changed.
//! 3. **Validate** against a compiled JSON Schema. The validator is compiled
//!    once per tool snapshot; this module also offers a compile-free path for
//!    tests and one-off callers.
//!
//! Unknown JSON Schema keywords are ignored (JSON Schema semantics). Local
//! `$ref`s (`#/definitions/...`, `#/$defs/...`) are resolved by the validator;
//! external refs are rejected so a tool schema cannot cause a network or file
//! read at call time.

use serde_json::Value;
use std::sync::Arc;

/// Maximum coercion recursion depth. Deeply nested MCP schemas are still
/// served; pathological self-referential structures are refused.
const MAX_DEPTH: usize = 8;
/// Maximum number of nodes visited by the coercion pass.
const MAX_NODES: usize = 10_000;
/// Maximum number of validation issues reported to the model.
const MAX_ISSUES: usize = 5;
/// Maximum bytes of received arguments echoed back in an error.
const MAX_ECHO_BYTES: usize = 1024;

/// The result of preparing one tool call.
#[derive(Debug, Clone, PartialEq)]
pub struct PreparedArguments {
    /// The parsed, coerced, validated value.
    pub value: Value,
    /// Human-readable descriptions of every coercion applied, in order.
    pub repairs: Vec<String>,
}

impl PreparedArguments {
    /// Whether any coercion was applied.
    pub fn repaired(&self) -> bool {
        !self.repairs.is_empty()
    }
}

/// A compiled input or output schema.
///
/// Compilation is the expensive half of validation; a per-request tool snapshot
/// compiles each schema once and reuses it for every call in the request.
#[derive(Clone)]
pub struct CompiledSchema {
    schema: Value,
    validator: Arc<jsonschema::Validator>,
    /// Whether the schema expects a top-level object. Used to accept a `null`
    /// argument payload as `{}` for no-argument tools.
    wants_object: bool,
}

impl std::fmt::Debug for CompiledSchema {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompiledSchema")
            .field("wants_object", &self.wants_object)
            .finish_non_exhaustive()
    }
}

/// Compile `schema` once. External `$ref`s are rejected by the validator's
/// default resolver; local refs work.
pub fn compile_schema(schema: &Value) -> Result<CompiledSchema, String> {
    let validator = jsonschema::validator_for(schema)
        .map_err(|error| format!("invalid tool schema: {error}"))?;
    Ok(CompiledSchema {
        schema: schema.clone(),
        validator: Arc::new(validator),
        wants_object: schema_expects_object(schema),
    })
}

/// Compile through a process-wide cache keyed by the serialized schema.
///
/// Tool schemas are stable across requests (MCP registration changes are the
/// exception, and a changed schema produces a different key). Compiling the
/// same 30 schemas on every turn is pure latency; the cache keeps the compiled
/// validator and the parsed schema together.
pub fn compile_schema_cached(schema: &Value) -> Result<Arc<CompiledSchema>, String> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};

    static CACHE: OnceLock<Mutex<HashMap<String, Arc<CompiledSchema>>>> = OnceLock::new();
    let Ok(key) = serde_json::to_string(schema) else {
        return compile_schema(schema).map(Arc::new);
    };
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = cache.lock().unwrap_or_else(|error| error.into_inner());
    if let Some(compiled) = guard.get(&key) {
        return Ok(Arc::clone(compiled));
    }
    let compiled = Arc::new(compile_schema(schema)?);
    if guard.len() >= 512 {
        guard.clear();
    }
    guard.insert(key, Arc::clone(&compiled));
    Ok(compiled)
}

impl CompiledSchema {
    /// Parse, coerce, and validate one raw argument string.
    pub fn prepare(&self, raw: &str) -> Result<PreparedArguments, String> {
        let mut value: Value = if raw.trim().is_empty() {
            Value::Object(serde_json::Map::new())
        } else {
            serde_json::from_str(raw).map_err(|error| {
                format!("invalid arguments: not valid JSON: {error}. Send a JSON object.")
            })?
        };

        if value.is_null() && self.wants_object {
            value = Value::Object(serde_json::Map::new());
        }

        let mut repairs = Vec::new();
        let mut nodes = 0usize;
        coerce(
            &self.schema,
            &self.schema,
            &mut value,
            "",
            0,
            &mut nodes,
            &mut repairs,
        )?;

        self.validate(&value)?;
        Ok(PreparedArguments { value, repairs })
    }

    /// Validate an already-parsed value against this schema.
    pub fn validate(&self, value: &Value) -> Result<(), String> {
        let errors: Vec<String> = self
            .validator
            .iter_errors(value)
            .take(MAX_ISSUES)
            .map(|error| format_issue(&error))
            .collect();
        if errors.is_empty() {
            return Ok(());
        }
        let mut message = String::from("invalid arguments:");
        for error in &errors {
            message.push_str("\n  - ");
            message.push_str(error);
        }
        if self.validator.iter_errors(value).nth(MAX_ISSUES).is_some() {
            message.push_str("\n  - …and more issues");
        }
        message.push_str("\nReceived: ");
        message.push_str(&compact_json(value));
        message.push_str("\nUpdate the arguments and call the tool again.");
        Err(message)
    }

    /// The original schema.
    pub fn schema(&self) -> &Value {
        &self.schema
    }
}

/// One-shot preparation for callers that do not hold a compiled schema.
///
/// Prefer [`compile_schema`] + [`CompiledSchema::prepare`] in hot paths.
pub fn prepare_tool_arguments(schema: &Value, raw: &str) -> Result<PreparedArguments, String> {
    compile_schema_cached(schema)?.prepare(raw)
}

/// One-shot output validation. Used when a tool declares `output_schema`.
pub fn validate_tool_output(schema: &Value, value: &Value) -> Result<(), String> {
    compile_schema_cached(schema)?
        .validate(value)
        .map_err(|message| {
            message
                .replace("invalid arguments:", "invalid structured output:")
                .replace("Update the arguments and call the tool again.", "")
        })
}

fn format_issue(error: &jsonschema::ValidationError<'_>) -> String {
    let path = error.instance_path().to_string();
    let location = if path.is_empty() {
        "/".to_string()
    } else {
        path
    };
    let mut detail = error.to_string();
    if detail.len() > 300 {
        let mut end = 300;
        while !detail.is_char_boundary(end) {
            end -= 1;
        }
        detail.truncate(end);
        detail.push('…');
    }
    format!("at `{location}`: {detail}")
}

fn compact_json(value: &Value) -> String {
    let mut text = serde_json::to_string(value).unwrap_or_else(|_| "<unserializable>".to_string());
    if text.len() > MAX_ECHO_BYTES {
        let mut end = MAX_ECHO_BYTES;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push_str("… (truncated)");
    }
    text
}

fn schema_expects_object(schema: &Value) -> bool {
    matches!(
        schema.get("type"),
        Some(Value::String(kind)) if kind == "object"
    ) || schema.get("properties").is_some()
}

/// Resolve a local `$ref` against the root schema. Returns `None` for external
/// or unresolvable refs, in which case coercion leaves the value as-is and the
/// compiled validator reports the final verdict.
fn resolve_ref<'a>(root: &'a Value, reference: &str) -> Option<&'a Value> {
    let pointer = reference.strip_prefix('#')?;
    if pointer.is_empty() {
        return Some(root);
    }
    let pointer = pointer.strip_prefix('/')?;
    let mut current = root;
    for segment in pointer.split('/') {
        let segment = segment.replace("~1", "/").replace("~0", "~");
        current = current.get(&segment)?;
    }
    Some(current)
}

fn effective_schema<'a>(root: &'a Value, schema: &'a Value, depth: usize) -> &'a Value {
    if depth > MAX_DEPTH {
        return schema;
    }
    match schema.get("$ref").and_then(Value::as_str) {
        Some(reference) => resolve_ref(root, reference).unwrap_or(schema),
        None => schema,
    }
}

/// The non-null type declared by a schema, if it declares exactly one.
fn primary_type(schema: &Value) -> Option<&str> {
    match schema.get("type") {
        Some(Value::String(kind)) => Some(kind),
        Some(Value::Array(kinds)) => kinds
            .iter()
            .filter_map(Value::as_str)
            .find(|kind| *kind != "null"),
        _ => {
            if schema.get("properties").is_some() {
                Some("object")
            } else if schema.get("items").is_some() {
                Some("array")
            } else {
                None
            }
        }
    }
}

fn allows_null(schema: &Value) -> bool {
    match schema.get("type") {
        Some(Value::String(kind)) => kind == "null",
        Some(Value::Array(kinds)) => kinds.iter().any(|kind| kind == "null"),
        _ => false,
    }
}

#[allow(clippy::too_many_arguments)]
fn coerce(
    root: &Value,
    schema: &Value,
    value: &mut Value,
    path: &str,
    depth: usize,
    nodes: &mut usize,
    repairs: &mut Vec<String>,
) -> Result<(), String> {
    if depth > MAX_DEPTH {
        return Ok(());
    }
    *nodes = nodes.saturating_add(1);
    if *nodes > MAX_NODES {
        return Err("invalid arguments: argument structure is too large".to_string());
    }

    let schema = effective_schema(root, schema, depth);

    // All branches of an `allOf` describe the same value; coerce through each.
    if let Some(all_of) = schema.get("allOf").and_then(Value::as_array) {
        for sub in all_of {
            coerce(root, sub, value, path, depth + 1, nodes, repairs)?;
        }
    }

    // Combinators: coerce through the first branch whose declared type matches
    // the value. The compiled validator remains the authority on which branch
    // actually accepts it.
    for keyword in ["oneOf", "anyOf"] {
        let Some(branches) = schema.get(keyword).and_then(Value::as_array) else {
            continue;
        };
        let Some(branch) = branches
            .iter()
            .find(|branch| type_matches(root, branch, value, depth))
        else {
            continue;
        };
        coerce(root, branch, value, path, depth + 1, nodes, repairs)?;
        break;
    }

    let Some(kind) = primary_type(schema) else {
        return Ok(());
    };

    match kind {
        "object" => coerce_object(root, schema, value, path, depth, nodes, repairs),
        "array" => coerce_array(root, schema, value, path, depth, nodes, repairs),
        "integer" | "number" => {
            coerce_number(schema, value, path, kind, repairs);
            Ok(())
        }
        "boolean" => {
            coerce_boolean(value, path, repairs);
            Ok(())
        }
        "string" => Ok(()),
        _ => Ok(()),
    }
}

fn type_matches(root: &Value, schema: &Value, value: &Value, depth: usize) -> bool {
    let schema = effective_schema(root, schema, depth);
    let Some(kind) = primary_type(schema) else {
        return true;
    };
    match kind {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "number" => value.is_number(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        _ => true,
    }
}

fn coerce_object(
    root: &Value,
    schema: &Value,
    value: &mut Value,
    path: &str,
    depth: usize,
    nodes: &mut usize,
    repairs: &mut Vec<String>,
) -> Result<(), String> {
    if let Value::String(text) = value
        && let Ok(parsed @ Value::Object(_)) = serde_json::from_str::<Value>(text)
    {
        repairs.push(format!("{path}/: parsed a JSON-encoded object"));
        *value = parsed;
    }
    let Value::Object(object) = value else {
        return Ok(());
    };

    let required: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();

    // Optional `null` placeholders are providers' way of saying "unset".
    if !required.is_empty() || schema.get("properties").is_some() {
        object.retain(|key, item| !(item.is_null() && !required.contains(&key.as_str())));
    }

    let properties = schema.get("properties").and_then(Value::as_object);
    if let Some(properties) = properties {
        for (key, property_schema) in properties {
            let property_path = format!("{path}/{key}");
            let Some(item) = object.get_mut(key) else {
                continue;
            };
            if item.is_null() {
                if required.contains(&key.as_str()) && !allows_null(property_schema) {
                    continue; // let the validator produce the precise error
                }
                if !required.contains(&key.as_str()) {
                    object.remove(key);
                    continue;
                }
            }
            coerce(
                root,
                property_schema,
                item,
                &property_path,
                depth + 1,
                nodes,
                repairs,
            )?;
        }
    }

    match schema.get("additionalProperties") {
        Some(Value::Bool(false)) => {
            let known: Vec<String> = properties
                .map(|properties| properties.keys().cloned().collect())
                .unwrap_or_default();
            let before = object.len();
            object.retain(|key, _| known.iter().any(|known| known == key));
            if object.len() != before {
                repairs.push(format!(
                    "{path}: removed {} unknown field(s)",
                    before - object.len()
                ));
            }
        }
        Some(additional @ Value::Object(_)) => {
            let known: Vec<String> = properties
                .map(|properties| properties.keys().cloned().collect())
                .unwrap_or_default();
            for (key, item) in object.iter_mut() {
                if known.iter().any(|known| known == key) {
                    continue;
                }
                coerce(
                    root,
                    additional,
                    item,
                    &format!("{path}/{key}"),
                    depth + 1,
                    nodes,
                    repairs,
                )?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn coerce_array(
    root: &Value,
    schema: &Value,
    value: &mut Value,
    path: &str,
    depth: usize,
    nodes: &mut usize,
    repairs: &mut Vec<String>,
) -> Result<(), String> {
    let items = schema.get("items");
    if !value.is_array() {
        let mut single = value.clone();
        let coerced = match items {
            Some(items) => {
                coerce(root, items, &mut single, path, depth + 1, nodes, repairs).is_ok()
                    && type_matches(root, items, &single, depth + 1)
            }
            None => true,
        };
        if coerced {
            repairs.push(format!("{path}: wrapped a single value in an array"));
            *value = Value::Array(vec![single]);
        }
    }
    if let (Value::Array(values), Some(items)) = (value, items) {
        for (index, item) in values.iter_mut().enumerate() {
            coerce(
                root,
                items,
                item,
                &format!("{path}/{index}"),
                depth + 1,
                nodes,
                repairs,
            )?;
        }
    }
    Ok(())
}

fn coerce_number(
    schema: &Value,
    value: &mut Value,
    path: &str,
    kind: &str,
    repairs: &mut Vec<String>,
) {
    let Some(text) = value.as_str() else {
        return;
    };
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return;
    }
    let Ok(number) = trimmed.parse::<f64>() else {
        return;
    };
    if !number.is_finite() {
        return;
    }
    if kind == "integer" {
        if number.fract() != 0.0 {
            return;
        }
        if number < i64::MIN as f64 || number > i64::MAX as f64 {
            return;
        }
        *value = Value::from(number as i64);
    } else {
        let Some(number) = serde_json::Number::from_f64(number) else {
            return;
        };
        *value = Value::Number(number);
    }
    // A string that was already an exact integer/float is still a repair; the
    // model should learn the declared type.
    if schema.get("enum").is_none() {
        repairs.push(format!("{path}: coerced a numeric string to a {kind}"));
    }
}

fn coerce_boolean(value: &mut Value, path: &str, repairs: &mut Vec<String>) {
    let Some(text) = value.as_str() else {
        return;
    };
    match text.trim().to_ascii_lowercase().as_str() {
        "true" => {
            *value = Value::Bool(true);
            repairs.push(format!("{path}: coerced a string to a boolean"));
        }
        "false" => {
            *value = Value::Bool(false);
            repairs.push(format!("{path}: coerced a string to a boolean"));
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn prepare(schema: Value, raw: &str) -> Result<PreparedArguments, String> {
        prepare_tool_arguments(&schema, raw)
    }

    #[test]
    fn parses_and_validates_a_nested_object() {
        let schema = json!({
            "type": "object",
            "required": ["path"],
            "properties": {
                "path": {"type": "string"},
                "opts": {
                    "type": "object",
                    "required": ["n"],
                    "properties": {"n": {"type": "integer"}}
                }
            }
        });
        assert!(prepare(schema.clone(), r#"{"path":"a.rs","opts":{"n":1}}"#).is_ok());
        let error = prepare(schema, r#"{"path":"a.rs","opts":{}}"#).unwrap_err();
        assert!(error.contains("required"), "{error}");
    }

    #[test]
    fn unparseable_arguments_are_an_error_not_a_pass_through() {
        let error = prepare(json!({"type": "object"}), "{not json").unwrap_err();
        assert!(error.contains("not valid JSON"), "{error}");
    }

    #[test]
    fn null_root_becomes_an_empty_object() {
        let prepared = prepare(json!({"type": "object"}), "null").unwrap();
        assert_eq!(prepared.value, json!({}));
    }

    #[test]
    fn numeric_strings_and_single_values_are_coerced() {
        let schema = json!({
            "type": "object",
            "properties": {
                "limit": {"type": "integer"},
                "recursive": {"type": "boolean"},
                "paths": {"type": "array", "items": {"type": "string"}}
            }
        });
        let prepared = prepare(
            schema,
            r#"{"limit":"20","recursive":"true","paths":"src/lib.rs"}"#,
        )
        .unwrap();
        assert_eq!(prepared.value["limit"], json!(20));
        assert_eq!(prepared.value["recursive"], json!(true));
        assert_eq!(prepared.value["paths"], json!(["src/lib.rs"]));
        assert_eq!(prepared.repairs.len(), 3);
    }

    #[test]
    fn optional_null_is_removed_but_required_null_is_not() {
        let schema = json!({
            "type": "object",
            "required": ["path"],
            "properties": {"path": {"type": "string"}, "limit": {"type": "integer"}}
        });
        let prepared = prepare(schema, r#"{"path":"a.rs","limit":null}"#).unwrap();
        assert!(prepared.value.get("limit").is_none());
        assert!(prepare(
            json!({"type": "object", "required": ["path"], "properties": {"path": {"type": "string"}}}),
            r#"{"path":null}"#
        )
        .is_err());
    }

    #[test]
    fn additional_properties_false_drops_unknown_keys() {
        let schema = json!({
            "type": "object",
            "properties": {"path": {"type": "string"}},
            "additionalProperties": false
        });
        let prepared = prepare(schema, r#"{"path":"a","extra":1}"#).unwrap();
        assert_eq!(prepared.value, json!({"path":"a"}));
    }

    #[test]
    fn local_refs_are_resolved() {
        let schema = json!({
            "type": "object",
            "properties": {"limit": {"$ref": "#/$defs/limit"}},
            "$defs": {"limit": {"type": "integer", "minimum": 1}}
        });
        let prepared = prepare(schema.clone(), r#"{"limit":"3"}"#).unwrap();
        assert_eq!(prepared.value["limit"], json!(3));
        assert!(prepare(schema, r#"{"limit":0}"#).is_err());
    }

    #[test]
    fn one_of_selects_a_matching_branch() {
        let schema = json!({
            "type": "object",
            "properties": {
                "value": {"oneOf": [{"type": "string"}, {"type": "integer"}]}
            }
        });
        let prepared = prepare(schema, r#"{"value":"12"}"#).unwrap();
        // A string is a valid member of the `oneOf`; coercion must not invent
        // a numeric interpretation when the schema does not force one.
        assert_eq!(prepared.value["value"], json!("12"));
    }

    #[test]
    fn a_number_is_not_silently_turned_into_a_string() {
        let schema = json!({"type": "object", "properties": {"x": {"type": "string"}}});
        assert!(prepare(schema, r#"{"x":1}"#).is_err());
    }

    #[test]
    fn error_echo_is_bounded() {
        let schema = json!({"type": "object", "properties": {"x": {"type": "string"}}});
        let raw = format!(r#"{{"x":{{"a":"{}"}}}}"#, "a".repeat(8 * 1024));
        let error = prepare(schema, &raw).unwrap_err();
        assert!(error.contains("truncated"), "{}", error.len());
        assert!(error.len() < 4 * 1024);
    }
}
