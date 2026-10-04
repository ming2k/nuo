//! OpenAI prompt-cache request controls shared by Chat Completions and
//! Responses encoders.

use nuo_model_codec::{CacheRetention, PromptCacheMode, ResolvedCachePolicy};
use serde_json::{Value, json};

/// Stamp top-level OpenAI controls and, for explicit mode, one stable content
/// boundary in `items_field` (`messages` or `input`).
pub fn apply(body: &mut Value, plan: &ResolvedCachePolicy, items_field: &str) {
    let ResolvedCachePolicy::Enabled {
        mode,
        retention,
        routing_key,
        max_breakpoints,
    } = plan
    else {
        return;
    };

    if let Some(key) = routing_key.as_deref().filter(|key| !key.is_empty()) {
        body["prompt_cache_key"] = json!(key);
    }

    match retention {
        Some(CacheRetention::InMemory) => {
            body["prompt_cache_retention"] = json!("in_memory");
        }
        Some(CacheRetention::TwentyFourHours) => {
            body["prompt_cache_retention"] = json!("24h");
        }
        Some(CacheRetention::ThirtyMinutes) | None => {}
        Some(CacheRetention::FiveMinutes | CacheRetention::OneHour) => {
            unreachable!("non-OpenAI retention reached the OpenAI encoder")
        }
    }

    match mode {
        PromptCacheMode::Implicit => {
            if *retention == Some(CacheRetention::ThirtyMinutes) {
                body["prompt_cache_options"] = json!({"mode": "implicit", "ttl": "30m"});
            }
        }
        PromptCacheMode::Explicit => {
            let mut options = json!({"mode": "explicit"});
            if *retention == Some(CacheRetention::ThirtyMinutes) {
                options["ttl"] = json!("30m");
            }
            body["prompt_cache_options"] = options;
            stamp_stable_boundary(&mut body[items_field], max_breakpoints.unwrap_or(0));
        }
        PromptCacheMode::Automatic => {
            unreachable!("automatic cache mode reached the OpenAI encoder")
        }
    }
}

/// Move top-level Responses instructions into a developer input message so an
/// explicit breakpoint can include them. OpenAI does not permit breakpoints on
/// the top-level `instructions` field.
pub fn project_responses_instructions_for_explicit_mode(
    body: &mut Value,
    plan: &ResolvedCachePolicy,
) {
    if !matches!(
        plan,
        ResolvedCachePolicy::Enabled {
            mode: PromptCacheMode::Explicit,
            ..
        }
    ) {
        return;
    }
    let Some(instructions) = body
        .as_object_mut()
        .and_then(|object| object.remove("instructions"))
        .and_then(|value| value.as_str().map(str::to_string))
    else {
        return;
    };
    let Some(input) = body["input"].as_array_mut() else {
        return;
    };
    input.insert(
        0,
        json!({
            "type": "message",
            "role": "developer",
            "content": [{"type": "input_text", "text": instructions}]
        }),
    );
}

/// Mark the newest stable message boundary. The request tail is never cached:
/// it is the part most likely to be unique to this invocation.
fn stamp_stable_boundary(items: &mut Value, max_breakpoints: u8) {
    if max_breakpoints == 0 {
        return;
    }
    let Some(items) = items.as_array_mut() else {
        return;
    };
    let Some(boundary) = items.iter_mut().rev().skip(1).find(|item| {
        item.get("role").and_then(Value::as_str).is_some()
            && item.get("role").and_then(Value::as_str) != Some("tool")
    }) else {
        return;
    };
    stamp_content(&mut boundary["content"]);
}

fn stamp_content(content: &mut Value) {
    if let Some(text) = content.as_str().map(str::to_string) {
        *content = json!([{
            "type": "text",
            "text": text,
            "prompt_cache_breakpoint": {"mode": "explicit"}
        }]);
        return;
    }
    if let Some(parts) = content.as_array_mut()
        && let Some(part) = parts.iter_mut().rev().find(|part| {
            matches!(
                part.get("type").and_then(Value::as_str),
                Some("text" | "input_text" | "output_text")
            )
        })
        && let Some(object) = part.as_object_mut()
    {
        object.insert(
            "prompt_cache_breakpoint".to_string(),
            json!({"mode": "explicit"}),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn implicit_mode_without_retention_emits_only_affinity_key() {
        let mut body = json!({"input": []});
        apply(
            &mut body,
            &ResolvedCachePolicy::Enabled {
                mode: PromptCacheMode::Implicit,
                retention: None,
                routing_key: Some("session-42".into()),
                max_breakpoints: None,
            },
            "input",
        );

        assert_eq!(body["prompt_cache_key"], "session-42");
        assert!(body.get("prompt_cache_options").is_none());
        assert!(body.get("prompt_cache_retention").is_none());
    }

    #[test]
    fn explicit_mode_marks_the_stable_message_not_the_request_tail() {
        let mut body = json!({
            "messages": [
                {"role": "system", "content": "policy"},
                {"role": "user", "content": "old"},
                {"role": "assistant", "content": "answer"},
                {"role": "user", "content": "new"}
            ]
        });
        apply(
            &mut body,
            &ResolvedCachePolicy::Enabled {
                mode: PromptCacheMode::Explicit,
                retention: Some(CacheRetention::ThirtyMinutes),
                routing_key: Some("session".into()),
                max_breakpoints: Some(4),
            },
            "messages",
        );
        assert_eq!(
            body["messages"][2]["content"][0]["prompt_cache_breakpoint"]["mode"],
            "explicit"
        );
        assert_eq!(body["messages"][3]["content"], "new");
    }
}
