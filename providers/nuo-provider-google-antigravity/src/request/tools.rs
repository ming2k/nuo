//! Tool declaration conversion and JSON-schema sanitation.

use serde_json::{Value, json};

pub(super) fn google_tools(
    tool_specs: Option<&[nuo_provider::capability::ToolSpec]>,
) -> Option<Value> {
    let specs = tool_specs?;
    if specs.is_empty() {
        return None;
    }
    let declarations = specs
        .iter()
        .map(|spec| {
            json!({
                "name": spec.name,
                "description": spec.description,
                "parameters": sanitize_schema(&spec.parameters),
            })
        })
        .collect::<Vec<_>>();
    Some(json!([{ "functionDeclarations": declarations }]))
}

/// Sanitize and normalize a JSON Schema into Google Gemini's OpenAPI Schema subset.
///
/// Google Gemini's REST API (`generateContent` / `streamGenerateContent`) maps
/// function declaration parameter schemas directly to the Protobuf `Schema` definition
/// (`google.ai.generativelanguage.v1beta.Schema`).
///
/// Standard JSON Schema (Draft 7 / 2020-12, TypeScript/Pydantic/MCP schemas) allows
/// keywords like `const`, `oneOf`, `allOf`, `$schema`, `additionalProperties`, `title`,
/// `default`, and array-based `type: ["string", "null"]` which Google's Protobuf JSON parser
/// strictly rejects with HTTP 400 (`Invalid JSON payload received. Unknown name "..."`).
///
/// This sanitizer recursively converts standard JSON Schema constructs into Gemini-compatible
/// Schema representations:
/// - `const: "v"` -> `enum: ["v"]`, `type: "string"`
/// - `oneOf` -> merged `enum` (if string literals) or converted to `anyOf`
/// - `allOf` -> flattened and merged into parent object
/// - `type: [T, "null"]` -> `type: T, nullable: true`
/// - `$schema`, `additionalProperties`, `title`, `default`, etc. -> stripped / migrated to description
/// - Missing `type` inferred from `properties`, `items`, or `enum`
/// - Recursive sanitation of nested `properties`, `items`, and `anyOf`.
pub(super) fn tool_response_payload(text: &str) -> Value {
    match serde_json::from_str::<Value>(text) {
        Ok(value) if value.is_object() => value,
        Ok(value) => json!({ "result": value }),
        Err(_) => json!({ "content": text }),
    }
}

pub fn sanitize_schema(value: &Value) -> Value {
    let Value::Object(map) = value else {
        return json!({ "type": "object" });
    };

    let mut map = map.clone();

    // 1. Flatten allOf / all_of
    if let Some(all_of) = map.remove("allOf").or_else(|| map.remove("all_of"))
        && let Value::Array(schemas) = all_of
    {
        for sub_schema in schemas {
            if let Value::Object(sub_map) = sanitize_schema(&sub_schema) {
                for (k, v) in sub_map {
                    match k.as_str() {
                        "properties" => {
                            if let Value::Object(sub_p) = v {
                                if let Some(Value::Object(p)) = map.get_mut("properties") {
                                    p.extend(sub_p);
                                } else {
                                    map.insert("properties".to_string(), Value::Object(sub_p));
                                }
                            }
                        }
                        "required" => {
                            if let Value::Array(sub_r) = v {
                                if let Some(Value::Array(r)) = map.get_mut("required") {
                                    r.extend(sub_r);
                                } else {
                                    map.insert("required".to_string(), Value::Array(sub_r));
                                }
                            }
                        }
                        "description" => {
                            map.entry("description".to_string()).or_insert(v);
                        }
                        "type" => {
                            map.entry("type".to_string()).or_insert(v);
                        }
                        _ => {
                            map.entry(k).or_insert(v);
                        }
                    }
                }
            }
        }
    }

    // 2. Handle oneOf / one_of
    if let Some(one_of) = map.remove("oneOf").or_else(|| map.remove("one_of"))
        && let Value::Array(variants) = one_of
    {
        let mut string_enums = Vec::new();
        let mut all_simple_strings = !variants.is_empty();

        for variant in &variants {
            if let Value::Object(vmap) = variant {
                if let Some(Value::String(c)) = vmap.get("const") {
                    string_enums.push(c.clone());
                } else if let Some(Value::Array(e)) = vmap.get("enum") {
                    if e.iter().all(|val| val.is_string()) {
                        for val in e {
                            if let Some(s) = val.as_str() {
                                string_enums.push(s.to_string());
                            }
                        }
                    } else {
                        all_simple_strings = false;
                        break;
                    }
                } else {
                    all_simple_strings = false;
                    break;
                }
            } else {
                all_simple_strings = false;
                break;
            }
        }

        if all_simple_strings && !string_enums.is_empty() {
            map.insert("type".to_string(), json!("string"));
            map.insert("enum".to_string(), json!(string_enums));
        } else {
            let sanitized_variants: Vec<Value> = variants.iter().map(sanitize_schema).collect();
            map.insert("anyOf".to_string(), json!(sanitized_variants));
        }
    }

    // 3. Normalize any_of -> anyOf
    if let Some(any_of) = map.remove("any_of") {
        map.entry("anyOf".to_string()).or_insert(any_of);
    }
    if let Some(Value::Array(variants)) = map.get_mut("anyOf") {
        for variant in variants.iter_mut() {
            *variant = sanitize_schema(variant);
        }
    }

    // 4. Handle const -> enum & type
    if let Some(c) = map.remove("const") {
        match c {
            Value::String(s) => {
                if !map.contains_key("enum") {
                    map.insert("enum".to_string(), json!([s]));
                }
                map.entry("type".to_string()).or_insert(json!("string"));
            }
            Value::Bool(_) => {
                map.entry("type".to_string()).or_insert(json!("boolean"));
            }
            Value::Number(n) => {
                let type_name = if n.is_i64() || n.is_u64() {
                    "integer"
                } else {
                    "number"
                };
                map.entry("type".to_string()).or_insert(json!(type_name));
            }
            Value::Null => {
                map.insert("nullable".to_string(), json!(true));
            }
            _ => {}
        }
    }

    // 5. Handle type (array of types vs string vs missing)
    if let Some(t) = map.remove("type") {
        match t {
            Value::Array(types) => {
                let mut non_null_types = Vec::new();
                let mut is_nullable = false;
                for item in types {
                    if let Some(type_str) = item.as_str() {
                        if type_str.eq_ignore_ascii_case("null") {
                            is_nullable = true;
                        } else {
                            non_null_types.push(type_str.to_lowercase());
                        }
                    }
                }
                if is_nullable {
                    map.insert("nullable".to_string(), json!(true));
                }
                if non_null_types.len() == 1 {
                    map.insert("type".to_string(), json!(non_null_types[0]));
                } else if non_null_types.len() > 1 {
                    let any_of_variants: Vec<Value> = non_null_types
                        .into_iter()
                        .map(|t_name| json!({ "type": t_name }))
                        .collect();
                    map.insert("anyOf".to_string(), json!(any_of_variants));
                }
            }
            Value::String(s) => {
                let lower = s.to_lowercase();
                if lower == "null" {
                    map.insert("nullable".to_string(), json!(true));
                } else {
                    map.insert("type".to_string(), json!(lower));
                }
            }
            _ => {}
        }
    }

    // Infer type if missing
    if !map.contains_key("type") && !map.contains_key("anyOf") {
        if map.contains_key("properties") {
            map.insert("type".to_string(), json!("object"));
        } else if map.contains_key("items") {
            map.insert("type".to_string(), json!("array"));
        } else if let Some(Value::Array(e)) = map.get("enum") {
            if e.iter().all(|val| val.is_string()) {
                map.insert("type".to_string(), json!("string"));
            }
        } else {
            map.insert("type".to_string(), json!("object"));
        }
    }

    // 6. Handle properties
    if let Some(Value::Object(props_map)) = map.get_mut("properties") {
        for (_, prop_schema) in props_map.iter_mut() {
            *prop_schema = sanitize_schema(prop_schema);
        }
    }

    // 7. Handle required
    if let Some(req) = map.get_mut("required") {
        if let Value::Array(req_arr) = req {
            let mut valid_req = Vec::new();
            for item in req_arr.iter() {
                if let Some(s) = item.as_str()
                    && !valid_req.contains(&s.to_string())
                {
                    valid_req.push(s.to_string());
                }
            }
            if valid_req.is_empty() {
                map.remove("required");
            } else {
                *req = json!(valid_req);
            }
        } else {
            map.remove("required");
        }
    }

    // 8. Handle items
    if let Some(items) = map.get_mut("items") {
        match items {
            Value::Object(_) => {
                *items = sanitize_schema(items);
            }
            Value::Array(item_arr) => {
                let sanitized_arr: Vec<Value> = item_arr.iter().map(sanitize_schema).collect();
                *items = json!({ "anyOf": sanitized_arr });
            }
            _ => {
                map.remove("items");
            }
        }
    }

    // 9. Handle enum (must be string array)
    if let Some(e) = map.get_mut("enum") {
        if let Value::Array(arr) = e {
            let mut string_enums = Vec::new();
            for item in arr.iter() {
                let s = match item {
                    Value::String(s) => s.clone(),
                    Value::Number(n) => n.to_string(),
                    Value::Bool(b) => b.to_string(),
                    _ => continue,
                };
                if !string_enums.contains(&s) {
                    string_enums.push(s);
                }
            }
            if string_enums.is_empty() {
                map.remove("enum");
            } else {
                *e = json!(string_enums);
            }
        } else {
            map.remove("enum");
        }
    }

    // 10. Handle title and description
    if let Some(title) = map.remove("title")
        && !map.contains_key("description")
        && let Value::String(t_str) = title
        && !t_str.is_empty()
    {
        map.insert("description".to_string(), json!(t_str));
    }

    // 11. Retain ONLY supported Gemini Schema fields
    let allowed_keys = [
        "type",
        "format",
        "description",
        "nullable",
        "enum",
        "properties",
        "required",
        "items",
        "minItems",
        "maxItems",
        "anyOf",
        "example",
    ];

    map.retain(|k, _| allowed_keys.contains(&k.as_str()));

    Value::Object(map)
}
