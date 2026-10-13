//! Resilient, zero-panic streaming JSON argument parser (ADR-0046).
//!
//! Models emit JSON arguments incrementally over chunked provider streams.
//! This parser extracts closed or extractable key-value fields from incomplete
//! JSON prefixes so presenters can project semantic identities (e.g. file paths,
//! command lines, diff hunks) before the JSON object finishes streaming.

use serde_json::Value;

/// Parse partial or complete tool arguments into a JSON map.
///
/// Guaranteed to never panic on malformed, truncated, or unclosed streaming input.
pub fn parse_streaming_arguments(input: &str) -> serde_json::Map<String, Value> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return serde_json::Map::new();
    }
    // Fast path: if the JSON is already complete and valid.
    if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(trimmed) {
        return map;
    }

    // Heuristic 1: repair unclosed delimiters and parse via serde_json.
    if let Some(map) = repair_and_parse_json(trimmed) {
        if !map.is_empty() {
            return map;
        }
    }

    // Heuristic 2: direct scanner fallback extracting completed or partial key-values.
    scan_extract_key_values(trimmed)
}

fn repair_and_parse_json(input: &str) -> Option<serde_json::Map<String, Value>> {
    let trimmed = input.trim_start();
    if !trimmed.starts_with('{') {
        return None;
    }

    let mut in_string = false;
    let mut escaped = false;
    let mut delim_stack = Vec::new();

    for ch in trimmed.chars() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
        } else {
            match ch {
                '"' => in_string = true,
                '{' => delim_stack.push('}'),
                '[' => delim_stack.push(']'),
                '}' if delim_stack.last() == Some(&'}') => {
                    delim_stack.pop();
                }
                ']' if delim_stack.last() == Some(&']') => {
                    delim_stack.pop();
                }
                _ => {}
            }
        }
    }

    let mut repaired = trimmed.to_string();
    if escaped {
        repaired.pop();
    }
    if in_string {
        repaired.push('"');
    }
    let r_trimmed = repaired.trim_end();
    if r_trimmed.ends_with(':') {
        repaired.push_str("\"\"");
    } else if r_trimmed.ends_with(',') {
        let mut chars = r_trimmed.chars().collect::<Vec<_>>();
        chars.pop();
        repaired = chars.into_iter().collect();
    }
    while let Some(closing) = delim_stack.pop() {
        repaired.push(closing);
    }

    if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(&repaired) {
        Some(map)
    } else {
        None
    }
}

fn scan_extract_key_values(input: &str) -> serde_json::Map<String, Value> {
    let mut map = serde_json::Map::new();
    let bytes = input.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        if bytes[i] != b'"' {
            i += 1;
            continue;
        }
        // Extract key
        let key_start = i + 1;
        let mut key_end = None;
        i += 1;
        let mut escaped = false;
        while i < len {
            if escaped {
                escaped = false;
            } else if bytes[i] == b'\\' {
                escaped = true;
            } else if bytes[i] == b'"' {
                key_end = Some(i);
                i += 1;
                break;
            }
            i += 1;
        }
        let Some(k_end) = key_end else { break };
        let raw_key = &input[key_start..k_end];
        let key: String =
            serde_json::from_str(&format!("\"{}\"", raw_key)).unwrap_or_else(|_| raw_key.to_string());

        // Skip whitespace to colon
        while i < len && matches!(bytes[i], b' ' | b'\t' | b'\r' | b'\n') {
            i += 1;
        }
        if i >= len || bytes[i] != b':' {
            continue;
        }
        i += 1; // skip ':'
        while i < len && matches!(bytes[i], b' ' | b'\t' | b'\r' | b'\n') {
            i += 1;
        }
        if i >= len {
            break;
        }

        // Parse value
        if bytes[i] == b'"' {
            let val_start = i + 1;
            let mut val_end = None;
            i += 1;
            let mut esc = false;
            while i < len {
                if esc {
                    esc = false;
                } else if bytes[i] == b'\\' {
                    esc = true;
                } else if bytes[i] == b'"' {
                    val_end = Some(i);
                    i += 1;
                    break;
                }
                i += 1;
            }
            if let Some(v_end) = val_end {
                let raw_val = &input[val_start..v_end];
                let val_str: String = serde_json::from_str(&format!("\"{}\"", raw_val))
                    .unwrap_or_else(|_| raw_val.to_string());
                map.insert(key, Value::String(val_str));
            } else {
                let raw_val = &input[val_start..];
                let raw_val = raw_val.strip_suffix('\\').unwrap_or(raw_val);
                let val_str: String = serde_json::from_str(&format!("\"{}\"", raw_val))
                    .unwrap_or_else(|_| raw_val.to_string());
                map.insert(key, Value::String(val_str));
            }
        } else if bytes[i] == b't' || bytes[i] == b'f' {
            let start = i;
            while i < len && bytes[i].is_ascii_alphabetic() {
                i += 1;
            }
            let word = &input[start..i];
            if word == "true" {
                map.insert(key, Value::Bool(true));
            } else if word == "false" {
                map.insert(key, Value::Bool(false));
            }
        } else if bytes[i].is_ascii_digit() || bytes[i] == b'-' {
            let start = i;
            while i < len
                && (bytes[i].is_ascii_digit()
                    || matches!(bytes[i], b'.' | b'e' | b'E' | b'+' | b'-'))
            {
                i += 1;
            }
            if let Ok(num) = input[start..i].parse::<i64>() {
                map.insert(key, Value::Number(num.into()));
            }
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_complete_json() {
        let input = r#"{"path": "src/main.rs", "offset": 10}"#;
        let map = parse_streaming_arguments(input);
        assert_eq!(map.get("path").and_then(Value::as_str), Some("src/main.rs"));
        assert_eq!(map.get("offset").and_then(Value::as_i64), Some(10));
    }

    #[test]
    fn parses_unclosed_string_value() {
        let input = r#"{"path": "src/lib.rs", "old_string": "fn foo() {"#;
        let map = parse_streaming_arguments(input);
        assert_eq!(map.get("path").and_then(Value::as_str), Some("src/lib.rs"));
        assert_eq!(map.get("old_string").and_then(Value::as_str), Some("fn foo() {"));
    }

    #[test]
    fn parses_early_path_field() {
        let input = r#"{"path": "nuo-tui/src/app.rs""#;
        let map = parse_streaming_arguments(input);
        assert_eq!(
            map.get("path").and_then(Value::as_str),
            Some("nuo-tui/src/app.rs")
        );
    }

    #[test]
    fn parses_with_trailing_comma() {
        let input = r#"{"command": "cargo build", "#;
        let map = parse_streaming_arguments(input);
        assert_eq!(
            map.get("command").and_then(Value::as_str),
            Some("cargo build")
        );
    }
}
