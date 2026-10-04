//! Agent compatibility path for text-emitted tool calls.
//!
//! Providers without native function calling (or ones that mirror a native
//! call as text) emit a JSON object such as `{"tool":"execute_command","arguments":{...}}`,
//! optionally wrapped in ChatML/Hermes sentinel tokens.
//! `parse_text_tool_call` recovers a [`ToolCall`] from such prose-embedded
//! JSON. Provider-native framing remains in the protocol SDKs.

use crate::{Message, Role, ToolCall};
use nuo_model_codec::find_balanced_object;

/// Parse a tool call from assistant response text.
///
/// Supports JSON tool calls emitted as plain text by providers without native
/// function calling. Robust to surrounding prose, markdown code fences, and
/// ChatML/Hermes-style special tokens (e.g. `<|tool_calls_section_end|>`,
/// `<tool_call>`): the first balanced `{ ... }` object carrying a recognised
/// tool identifier is used, so any text around the JSON is ignored. Both the
/// `"tool"` key and the OpenAI/MCP `"name"` key are accepted as the tool
/// identifier.
pub(crate) fn parse_text_tool_call(text: &str) -> Option<ToolCall> {
    let mut start = 0;
    while let Some(offset) = text[start..].find('{') {
        let brace_at = start + offset;
        if let Some(end) = find_balanced_object(text, brace_at) {
            let candidate = &text[brace_at..=end];
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(candidate) {
                let tool_name = json
                    .get("tool")
                    .or_else(|| json.get("name"))
                    .and_then(|value| value.as_str());
                if let Some(tool_name) = tool_name {
                    let args = match json.get("arguments") {
                        Some(serde_json::Value::String(string)) => string.clone(),
                        Some(value) => value.to_string(),
                        None => "{}".to_string(),
                    };
                    return Some(ToolCall {
                        id: format!("call_{}", uuid::Uuid::new_v4()),
                        name: tool_name.to_string(),
                        arguments: args,
                    });
                }
            }
            // Skip past this object and keep searching; a later object in the
            // text may carry the tool identifier.
            start = end + 1;
        } else {
            // A malformed opening brace does not rule out a later, independent
            // object. Advance one byte so that nested/later candidates are
            // still considered.
            start = brace_at + 1;
        }
    }
    parse_called_tool_fallback(text)
}

/// Parse fallback text formatted as `[Called tool <name> with arguments: <json>]`
/// or `[Called tool `name` with arguments: <json>]`.
fn parse_called_tool_fallback(text: &str) -> Option<ToolCall> {
    let prefix = "[Called tool ";
    let idx = text.find(prefix)?;
    let rest = &text[idx + prefix.len()..];
    let rest = rest.trim_start();
    let (tool_name, rest) = if let Some(stripped) = rest.strip_prefix('`') {
        let end_backtick = stripped.find('`')?;
        (&stripped[..end_backtick], &stripped[end_backtick + 1..])
    } else {
        let end_space = rest.find(' ')?;
        (&rest[..end_space], &rest[end_space..])
    };
    if tool_name.is_empty() {
        return None;
    }
    let args_marker = "with arguments:";
    let args_idx = rest.find(args_marker)?;
    let args_slice = rest[args_idx + args_marker.len()..].trim_start();
    let brace_start = args_slice.find('{')?;
    let balanced_end = find_balanced_object(args_slice, brace_start)?;
    let candidate = &args_slice[brace_start..=balanced_end];
    if serde_json::from_str::<serde_json::Value>(candidate).is_ok() {
        Some(ToolCall {
            id: format!("call_{}", uuid::Uuid::new_v4()),
            name: tool_name.to_string(),
            arguments: candidate.to_string(),
        })
    } else {
        None
    }
}

/// Promote a text-based (fallback) tool call onto the preceding assistant
/// message as a native `tool_calls` entry. This keeps the tool_call /
/// tool_call_id pairing valid for OpenAI-compatible providers (which require
/// every tool result to reference an assistant tool call), while non-native
/// providers simply ignore the `tool_calls` field and keep using the message
/// `content`.
pub(crate) fn attach_fallback_tool_call(messages: &mut [Message], call: &ToolCall) {
    if let Some(last) = messages.last_mut()
        && last.role == Role::Assistant
        && last.tool_calls.is_none()
    {
        last.tool_calls = Some(vec![call.clone()]);
    }
}

/// Extract a partial string field value from an in-flight streaming JSON arguments string.
///
/// For example, given an in-flight stream `{"path": "crates/nuo-harness/src/`
/// this extracts `"crates/nuo-harness/src/"` without waiting for the closing quote or brace.
/// This enables speculative pre-warming (e.g. disk page-cache read, path validation, permission check)
/// while the LLM is still streaming the rest of the arguments.
pub fn extract_partial_string_field(partial_json: &str, field_name: &str) -> Option<String> {
    let key_pattern = format!("\"{}\"", field_name);
    let key_pos = partial_json.find(&key_pattern)?;
    let after_key = &partial_json[key_pos + key_pattern.len()..];
    let colon_pos = after_key.find(':')?;
    let after_colon = after_key[colon_pos + 1..].trim_start();
    if !after_colon.starts_with('"') {
        return None;
    }
    let value_str = &after_colon[1..];
    let mut out = String::new();
    let mut escaped = false;
    for ch in value_str.chars() {
        if escaped {
            match ch {
                'n' => out.push('\n'),
                'r' => out.push('\r'),
                't' => out.push('\t'),
                '\\' => out.push('\\'),
                '"' => out.push('"'),
                other => {
                    out.push('\\');
                    out.push(other);
                }
            }
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == '"' {
            break;
        } else {
            out.push(ch);
        }
    }
    (!out.is_empty()).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_text_tool_call_accepts_bare_json() {
        let call = parse_text_tool_call("{\"tool\":\"alpha\",\"arguments\":{\"k\":1}}")
            .expect("bare json");
        assert_eq!(call.name, "alpha");
        assert_eq!(call.arguments, "{\"k\":1}");
    }

    #[test]
    fn parse_text_tool_call_ignores_trailing_special_tokens() {
        let call = parse_text_tool_call(
            "{\"tool\":\"read_text\",\"arguments\":{\"path\":\"x\"}}<|tool_calls_section_end|>",
        )
        .expect("trailing special token must not break parsing");
        assert_eq!(call.name, "read_text");
        assert_eq!(call.arguments, "{\"path\":\"x\"}");
    }

    #[test]
    fn parse_text_tool_call_ignores_prose_and_code_fences() {
        let call = parse_text_tool_call(
            "I'll read it now.\n```json\n{\"name\":\"read\",\"arguments\":{}}\n```",
        )
        .expect("prose + fence should still be found");
        assert_eq!(call.name, "read");
        assert_eq!(call.arguments, "{}");
    }

    #[test]
    fn parse_text_tool_call_accepts_name_key() {
        let call = parse_text_tool_call("{\"name\":\"alpha\"}").expect("name key is accepted");
        assert_eq!(call.name, "alpha");
        assert_eq!(call.arguments, "{}");
    }

    #[test]
    fn parse_text_tool_call_passes_through_string_arguments() {
        // Pre-serialised string arguments are forwarded verbatim, not
        // double-encoded by Value::to_string().
        let call = parse_text_tool_call("{\"tool\":\"alpha\",\"arguments\":\"{\\\"k\\\":1}\"}")
            .expect("string arguments");
        assert_eq!(call.arguments, "{\"k\":1}");
    }

    #[test]
    fn parse_text_tool_call_returns_none_for_plain_prose() {
        assert!(parse_text_tool_call("just some text, no tool call here").is_none());
    }

    #[test]
    fn parse_text_tool_call_skips_non_tool_json_objects() {
        // A JSON object without a tool/name key is skipped; a later object
        // carrying the identifier is still recognised.
        let call =
            parse_text_tool_call("{\"note\":\"thinking\"}{\"tool\":\"alpha\",\"arguments\":{}}")
                .expect("later object has the tool key");
        assert_eq!(call.name, "alpha");
    }

    #[test]
    fn parse_text_tool_call_skips_an_unbalanced_prefix() {
        let call =
            parse_text_tool_call("broken { prose before {\"tool\":\"alpha\",\"arguments\":{}}")
                .expect("later balanced object has the tool key");
        assert_eq!(call.name, "alpha");
    }

    #[test]
    fn extract_partial_string_field_extracts_incomplete_stream() {
        let stream = r#"{"path": "crates/nuo-harness/src/lib.rs", "content": "in progress"#;
        assert_eq!(
            extract_partial_string_field(stream, "path"),
            Some("crates/nuo-harness/src/lib.rs".to_string())
        );

        let incomplete_field = r#"{"command": "cargo build --"#;
        assert_eq!(
            extract_partial_string_field(incomplete_field, "command"),
            Some("cargo build --".to_string())
        );

        let empty = r#"{"other": 123}"#;
        assert_eq!(extract_partial_string_field(empty, "path"), None);
    }

    #[test]
    fn parse_called_tool_fallback_parses_unquoted_and_quoted_tool_calls() {
        let text1 = r#"[Called tool read_text with arguments: {"limit": 90, "offset": 100, "path": "/path/to/sync.rs"}]"#;
        let call1 = parse_text_tool_call(text1).expect("should parse unquoted called tool");
        assert_eq!(call1.name, "read_text");
        assert_eq!(
            call1.arguments,
            r#"{"limit": 90, "offset": 100, "path": "/path/to/sync.rs"}"#
        );

        let text2 = r#"[Called tool `todo` with arguments: {"items": []}]"#;
        let call2 = parse_text_tool_call(text2).expect("should parse backtick quoted called tool");
        assert_eq!(call2.name, "todo");
        assert_eq!(call2.arguments, r#"{"items": []}"#);
    }
}
