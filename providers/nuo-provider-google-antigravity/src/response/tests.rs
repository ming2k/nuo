use super::*;

#[test]
fn stream_text_concatenates_parts_and_preserves_multibyte() {
    let payload = r#"{"candidates":[{"content":{"parts":[{"text":"😀😁"},{"text":"😃😄"}]}}]}"#;
    assert_eq!(stream_text(payload), "😀😁😃😄");
}

#[test]
fn stream_text_returns_empty_for_non_text_payload() {
    assert_eq!(
        stream_text(r#"{"candidates":[{"finishReason":"STOP"}]}"#),
        ""
    );
    assert_eq!(stream_text("not json"), "");
}

#[test]
fn message_extracts_function_calls() {
    let value: Value = serde_json::from_str(
        r#"{
          "candidates": [{
            "content": {
              "parts": [
                {
                  "thoughtSignature": "sig-1",
                  "functionCall": {"id": "abc", "name": "find_files", "args": {"path": "."}}
                },
                {"thoughtSignature": "text-sig", "text": "done"}
              ]
            }
          }]
        }"#,
    )
    .unwrap();

    let message = super::message(&value).unwrap();
    assert_eq!(message.content, "done");
    let calls = message.tool_calls.unwrap();
    assert_eq!(calls[0].id, "abc");
    assert_eq!(calls[0].name, "find_files");
    assert_eq!(calls[0].arguments, r#"{"path":"."}"#);
    assert_eq!(
        message.provider_meta.unwrap()[THOUGHT_SIGNATURES_META_KEY]["abc"],
        "sig-1"
    );
    let value: Value = serde_json::from_str(
        r#"{"candidates":[{"content":{"parts":[{"thoughtSignature":"text-sig","text":"done"}]}}]}"#,
    )
    .unwrap();
    let message = super::message(&value).unwrap();
    assert_eq!(
        message.provider_meta.unwrap()[TEXT_THOUGHT_SIGNATURE_META_KEY],
        "text-sig"
    );
}

#[test]
fn stream_events_extract_function_calls_and_usage() {
    let events = stream_events(
        r#"{
          "usageMetadata": {
            "promptTokenCount": 10,
            "candidatesTokenCount": 2,
            "totalTokenCount": 12
          },
          "candidates": [{
            "content": {
              "parts": [
                {"text": "thinking"},
                {
                  "thoughtSignature": "sig-2",
                  "functionCall": {"id": "abc", "name": "grep", "args": {"query": "x"}}
                }
              ]
            }
          }]
        }"#,
    );

    assert!(matches!(events[0], ProviderStreamEvent::Usage(_)));
    assert_eq!(
        events[1],
        ProviderStreamEvent::TextDelta("thinking".to_string())
    );
    assert_eq!(
        events[2],
        ProviderStreamEvent::ToolCallDelta {
            index: 0,
            id: Some("abc".to_string()),
            name: Some("grep".to_string()),
            arguments: r#"{"query":"x"}"#.to_string(),
        }
    );

    let payload = stream_payload(
        r#"{"candidates":[{"content":{"parts":[{"thought_signature":"sig-3","functionCall":{"id":"xyz","name":"find_files","args":{}}}]}}]}"#,
    );
    assert_eq!(
        payload.thought_signatures,
        vec![("xyz".to_string(), "sig-3".to_string())]
    );
}

#[test]
fn stream_routes_thought_parts_to_reasoning_delta() {
    // A response with a thought summary followed by the answer. Only the
    // `"thought": true` part must become ReasoningDelta; the plain text
    // part stays a TextDelta.
    let events = stream_events(
        r#"{
          "candidates": [{
            "content": {
              "parts": [
                {"text": "let me consider the angles...", "thought": true},
                {"text": "The answer is 42."}
              ]
            }
          }]
        }"#,
    );
    assert_eq!(
        events[0],
        ProviderStreamEvent::ReasoningDelta("let me consider the angles...".to_string())
    );
    assert_eq!(
        events[1],
        ProviderStreamEvent::TextDelta("The answer is 42.".to_string())
    );
}

#[test]
fn message_routes_thought_parts_to_reasoning_content() {
    // Non-streaming: thought text goes to reasoning_content, never content.
    let value: Value = serde_json::from_str(
        r#"{
          "candidates": [{
            "content": {
              "parts": [
                {"text": "reasoning step by step", "thought": true},
                {"text": "final answer"}
              ]
            }
          }]
        }"#,
    )
    .unwrap();
    let message = super::message(&value).unwrap();
    assert_eq!(message.content, "final answer");
    assert_eq!(
        message.reasoning_content.as_deref(),
        Some("reasoning step by step")
    );
}

#[test]
fn message_without_thought_flag_keeps_all_text_in_content() {
    // No `thought` flag → nothing is reasoning; all text is the answer.
    // Guards against accidentally treating ordinary text as thinking.
    let value: Value = serde_json::from_str(
        r#"{
          "candidates": [{
            "content": {
              "parts": [
                {"text": "part one "},
                {"text": "part two"}
              ]
            }
          }]
        }"#,
    )
    .unwrap();
    let message = super::message(&value).unwrap();
    assert_eq!(message.content, "part one part two");
    assert!(message.reasoning_content.is_none());
}

#[test]
fn usage_surfaces_google_cached_content_tokens_as_read() {
    let u = usage(&serde_json::json!({
        "promptTokenCount": 900,
        "candidatesTokenCount": 30,
        "totalTokenCount": 930,
        "cachedContentTokenCount": 600
    }))
    .unwrap();
    assert_eq!(u.prompt_tokens, 900);
    assert_eq!(u.cache_read_input_tokens, 600);
    assert_eq!(u.cache_creation_input_tokens, 0);
}

#[test]
fn parses_antigravity_wrapped_response_and_stream() {
    let wrapped_json = serde_json::json!({
        "response": {
            "candidates": [{
                "content": {
                    "parts": [
                        {"text": "thinking deep", "thought": true},
                        {"text": "Antigravity response"}
                    ]
                }
            }],
            "usageMetadata": {
                "promptTokenCount": 100,
                "candidatesTokenCount": 20,
                "totalTokenCount": 120
            }
        },
        "responseId": "resp-123"
    });

    let msg = super::message(&wrapped_json).unwrap();
    assert_eq!(msg.content, "Antigravity response");
    assert_eq!(msg.reasoning_content.as_deref(), Some("thinking deep"));

    let stream_str = wrapped_json.to_string();
    let payload = stream_payload(&stream_str);
    assert_eq!(payload.events.len(), 3); // Usage + ReasoningDelta + TextDelta
    assert_eq!(
        payload.events[0],
        ProviderStreamEvent::Usage(TokenUsage {
            prompt_tokens: 100,
            completion_tokens: 20,
            total_tokens: 120,
            ..Default::default()
        })
    );
    assert_eq!(
        payload.events[1],
        ProviderStreamEvent::ReasoningDelta("thinking deep".to_string())
    );
    assert_eq!(
        payload.events[2],
        ProviderStreamEvent::TextDelta("Antigravity response".to_string())
    );
}

#[test]
fn request_shape_reads_back_what_the_envelope_carried() {
    // The Antigravity envelope nests the GenerateContentRequest under
    // `request`; the shape must read the inner object, not the wrapper.
    let body = serde_json::json!({
        "project": "aicode-consumers",
        "model": "gemini-3.8-flash-tiered",
        "request": {
            "contents": [
                {"role": "user", "parts": [{"text": "hi"}]},
                {"role": "model", "parts": [{"text": "hello"}]},
            ],
            "generationConfig": {
                "thinkingConfig": {"includeThoughts": true, "thinkingLevel": "HIGH"}
            },
            "tools": [
                {"functionDeclarations": [{"name": "a"}, {"name": "b"}]},
            ],
        }
    });
    let shape = super::RequestShape::of("gemini-3.8-flash-tiered", &body);
    assert_eq!(shape.wire_model, "gemini-3.8-flash-tiered");
    assert_eq!(shape.content_count, 2);
    assert_eq!(shape.tool_count, 2);
    assert_eq!(
        shape.thinking_config.as_deref(),
        Some(r#"{"includeThoughts":true,"thinkingLevel":"HIGH"}"#)
    );
    let described = shape.describe();
    assert!(
        described.contains("model=gemini-3.8-flash-tiered"),
        "{described}"
    );
    assert!(described.contains("thinkingLevel=HIGH"), "{described}");
    assert!(described.contains("tools=2"), "{described}");
    assert!(described.contains("contents=2"), "{described}");
    // The rendering must be brace-free: the client's notice normalizer
    // treats the first `{` as the start of a JSON body and would re-parse
    // this diagnostic as if it were the upstream's payload.
    assert!(!described.contains('{'), "{described}");
    assert!(!described.contains('}'), "{described}");
}

#[test]
fn request_shape_reports_an_omitted_thinking_surface_explicitly() {
    // A downgraded / non-reasoning request must say "omitted" rather than
    // silently dropping the fact — the absence IS the diagnostic when a
    // previous attempt was refused for that surface.
    let body = serde_json::json!({
        "contents": [{"role": "user", "parts": [{"text": "hi"}]}],
    });
    let shape = super::RequestShape::of("gemini-2.5-flash", &body);
    assert!(shape.thinking_config.is_none());
    assert_eq!(shape.tool_count, 0);
    assert!(shape.describe().contains("thinkingConfig=omitted"));
}

#[test]
fn a_content_free_400_gains_the_sent_request_shape() {
    // The reported failure: Google's canonical refusal names no field, so
    // the notice said only "Request contains an invalid argument." with
    // nothing to act on. The sent shape must now ride along.
    let raw = nuo_provider::ProviderError::new(
        "Google",
        nuo_provider::ProviderErrorKind::InvalidRequest,
        "Google HTTP 400 Bad Request: Request contains an invalid argument.",
    )
    .with_status(400);
    let shape = super::RequestShape::of(
        "gemini-3.8-flash-tiered",
        &serde_json::json!({
            "contents": [{"role": "user", "parts": [{"text": "hi"}]}],
            "generationConfig": {
                "thinkingConfig": {"includeThoughts": true, "thinkingLevel": "HIGH"}
            },
        }),
    );
    let clarified = super::clarify_error_with_shape(
        raw,
        "gemini-3.8-flash",
        "https://cloudcode-pa.googleapis.com",
        Some(&shape),
    );
    let msg = clarified.message();
    assert!(msg.starts_with("Google HTTP 400 Bad Request"), "{msg}");
    assert!(
        msg.contains("Request contains an invalid argument."),
        "{msg}"
    );
    // The evidence travels with the failure.
    assert!(msg.contains("model=gemini-3.8-flash-tiered"), "{msg}");
    assert!(msg.contains("thinkingLevel=HIGH"), "{msg}");
    // ...and it is framed as a report, never as a diagnosis.
    assert!(msg.contains("names no field"), "{msg}");
    assert_eq!(clarified.status(), Some(400));
    // The whole message stays brace-free so the client's notice normalizer
    // does not mistake our diagnostic for a JSON error body.
    assert!(!msg.contains('{'), "{msg}");
}

#[test]
fn a_non_refusal_keeps_its_own_guidance_and_gains_no_shape() {
    // A 429's body is rich; bolting the request shape onto it would bury
    // the quota facts under irrelevant request anatomy.
    let raw = nuo_provider::ProviderError::new(
        "Google",
        nuo_provider::ProviderErrorKind::RateLimited,
        "Google HTTP 429 Too Many Requests: {\"error\":{\"status\":\"RESOURCE_EXHAUSTED\"}}",
    )
    .with_status(429)
    .retryable(None);
    let shape = super::RequestShape::of("gemini-3.8-flash-tiered", &serde_json::json!({}));
    let clarified =
        super::clarify_error_with_shape(raw, "gemini-3.8-flash", "https://x", Some(&shape));
    assert!(clarified.message().contains("RESOURCE_EXHAUSTED"));
    assert!(!clarified.message().contains("names no field"));
    assert_eq!(
        clarified.retry_disposition(),
        nuo_provider::RetryDisposition::Retry {
            retry_after_ms: None
        }
    );
}

#[test]
fn clarify_error_appends_guidance_to_retryable_error() {
    let raw = nuo_provider::ProviderError::new(
        "Google",
        nuo_provider::ProviderErrorKind::RateLimited,
        "Google HTTP 429 Too Many Requests: {\"error\":{\"code\":429,\"status\":\"RESOURCE_EXHAUSTED\"}}",
    )
    .with_status(429)
    .retryable(None);
    let clarified = super::clarify_error_with_shape(
        raw,
        "gemini-3.7-flash",
        "https://cloudcode-pa.googleapis.com",
        None,
    );

    assert_eq!(clarified.status(), Some(429));
    assert_eq!(
        clarified.retry_disposition(),
        nuo_provider::RetryDisposition::Retry {
            retry_after_ms: None
        }
    );
    assert!(
        clarified.message().contains("RESOURCE_EXHAUSTED"),
        "the provider body survives: {}",
        clarified.message()
    );
    assert!(
        clarified
            .message()
            .contains("Google rate limit / quota exhausted"),
        "the guidance is attached: {}",
        clarified.message()
    );
}

#[test]
fn clarify_error_promotes_google_retryinfo_delay_into_retry_after_ms() {
    let body = r#"{"error":{"code":429,"message":"Resource has been exhausted (e.g. check quota).","status":"RESOURCE_EXHAUSTED","details":[{"@type":"type.googleapis.com/google.rpc.RetryInfo","retryDelay":"3620s"}]}}"#;
    let raw = nuo_provider::ProviderError::new(
        "Google",
        nuo_provider::ProviderErrorKind::RateLimited,
        format!("Google HTTP 429 Too Many Requests: {body}"),
    )
    .with_status(429)
    .retryable(None);
    let clarified = super::clarify_error_with_shape(raw, "gemini-3.7-flash", "https://x", None);
    assert_eq!(
        clarified.retry_disposition(),
        nuo_provider::RetryDisposition::Retry {
            retry_after_ms: Some(3_620_000)
        }
    );
    assert!(
        clarified.message().contains("~1h 00m"),
        "the reset hint is humanized from Google's own delay: {}",
        clarified.message()
    );
}

#[test]
fn clarify_error_preserves_an_existing_retry_after() {
    let raw = nuo_provider::ProviderError::new(
        "Google",
        nuo_provider::ProviderErrorKind::RateLimited,
        "Google HTTP 429: {\"error\":{\"status\":\"RESOURCE_EXHAUSTED\",\"details\":[{\"retryDelay\":\"45s\"}]}}",
    )
    .with_status(429)
    .retryable(Some(120_000));
    let clarified = super::clarify_error_with_shape(raw, "m", "https://x", None);
    assert_eq!(
        clarified.retry_disposition(),
        nuo_provider::RetryDisposition::Retry {
            retry_after_ms: Some(120_000)
        }
    );
}

#[test]
fn clarify_error_passes_non_retryable_errors_through_with_guidance() {
    let raw = nuo_provider::ProviderError::new(
        "Google",
        nuo_provider::ProviderErrorKind::InvalidRequest,
        "Google HTTP 404 Not Found",
    )
    .with_status(404);
    let clarified = super::clarify_error_with_shape(raw, "m", "https://x", None);
    assert!(clarified.message().starts_with("Google HTTP 404"));
    assert!(
        clarified
            .message()
            .contains("endpoint and protocol dialect")
    );
    assert!(!clarified.message().contains("Switch to a model"));
    assert_eq!(
        clarified.retry_disposition(),
        nuo_provider::RetryDisposition::Never
    );
}

#[test]
fn parse_google_duration_accepts_protobuf_and_bare_forms() {
    use super::parse_google_duration;
    assert_eq!(parse_google_duration("45s"), Some(45_000));
    assert_eq!(parse_google_duration("1.500s"), Some(1_500));
    assert_eq!(parse_google_duration("2m30s"), Some(150_000));
    assert_eq!(parse_google_duration("1h"), Some(3_600_000));
    assert_eq!(parse_google_duration("0.5s"), Some(500));
    assert_eq!(parse_google_duration("30"), Some(30_000));
    assert_eq!(parse_google_duration("nonsense"), None);
}

#[test]
fn rejects_thinking_config_matches_google_invalid_argument() {
    use super::rejects_thinking_config;
    // The canonical Google shape: INVALID_ARGUMENT naming the field.
    assert!(rejects_thinking_config(
        "Google HTTP 400: {\"error\":{\"code\":400,\"status\":\"INVALID_ARGUMENT\",\
         \"message\":\"Invalid JSON payload received. Unknown name \\\"thinkingConfig\\\" \
         at 'generation_config': Cannot find field.\"}}"
    ));
    // Relay re-casings and the snake_case spelling both match.
    assert!(rejects_thinking_config(
        "Google HTTP 400: unknown name include_thoughts"
    ));
    assert!(rejects_thinking_config(
        "Google HTTP 400: thinking_level is not supported on this model"
    ));
}

#[test]
fn rejects_thinking_config_ignores_other_400s() {
    use super::rejects_thinking_config;
    // A 400 that does not name the thinking surface is some other
    // contract violation — it must keep failing loudly.
    assert!(!rejects_thinking_config(
        "Google HTTP 400: {\"error\":{\"status\":\"INVALID_ARGUMENT\",\
         \"message\":\"Request payload size exceeds the limit.\"}}"
    ));
    // A thinking-related message without an invalid-argument signal
    // (e.g. quoted inside unrelated prose) does not downgrade either.
    assert!(!rejects_thinking_config("thinkingConfig is great"));
    // Retryable exhaustion naming nothing about thinking stays itself.
    assert!(!rejects_thinking_config(
        "Google HTTP 429: RESOURCE_EXHAUSTED thinking about quota"
    ));
    // A corrupted/foreign thought signature is a *replay* fault, not a
    // refused thinking surface: it names no thinking field, so it must keep
    // failing loudly. Downgrading here would silently drop the disclosure
    // surface and mask the real cause, and the sticky latch would then strip
    // `thinkingConfig` from every later turn on the channel.
    assert!(!rejects_thinking_config(
        "Google HTTP 400: {\"error\":{\"status\":\"INVALID_ARGUMENT\",\
         \"message\":\"Corrupted thought signature.\"}}"
    ));
    assert!(!rejects_thinking_config(
        "Google HTTP 400: {\"error\":{\"status\":\"INVALID_ARGUMENT\",\
         \"message\":\"Invalid thought signature.\"}}"
    ));
}

#[test]
fn the_augmented_message_must_never_be_fed_to_the_downgrade_predicate() {
    // Regression lock. `clarify_error` appends guidance that necessarily
    // mentions `thinkingConfig`, `thinkingLevel` and `HTTP 400` — that is
    // what makes a content-free refusal actionable. So the *augmented*
    // message matches `rejects_thinking_config` for ANY 400, and using it
    // for the downgrade decision would latch the channel into permanent
    // chain-withholding on an unrelated fault (a bad tool schema, a refused
    // image).
    //
    // The typed entry point reads the upstream text, so it stays false.
    let upstream = nuo_provider::ProviderError::new(
        "Google",
        nuo_provider::ProviderErrorKind::InvalidRequest,
        "Google HTTP 400 Bad Request: Request contains an invalid argument.",
    )
    .with_status(400);
    // Unrelated fault: the upstream says nothing about the thinking surface.
    assert!(
        !super::rejects_thinking_config_error(&upstream),
        "an unrelated 400 must not be classified as a thinking rejection"
    );

    let shape = super::RequestShape::of(
        "gemini-3.8-flash-tiered",
        &serde_json::json!({
            "generationConfig": {
                "thinkingConfig": {"includeThoughts": true, "thinkingLevel": "HIGH"}
            },
        }),
    );
    let augmented =
        super::clarify_error_with_shape(upstream, "gemini-3.8-flash", "https://x", Some(&shape));
    // The augmented text DOES match the raw predicate — this is exactly the
    // trap, and asserting it keeps the reason for the typed entry point
    // visible and tested rather than a comment someone can delete.
    assert!(
        super::rejects_thinking_config(augmented.message()),
        "the augmented message is expected to match the raw predicate; \
         that is precisely why callers must classify before augmenting"
    );
    // ...and a genuine thinking rejection is still recognized upstream.
    let genuine = nuo_provider::ProviderError::new(
        "Google",
        nuo_provider::ProviderErrorKind::InvalidRequest,
        "Google HTTP 400: {\"error\":{\"status\":\"INVALID_ARGUMENT\",\
         \"message\":\"Invalid JSON payload received. Unknown name \
         \\\"thinkingConfig\\\" at 'request': Cannot find field.\"}}",
    )
    .with_status(400);
    assert!(super::rejects_thinking_config_error(&genuine));
}

/// A signature minted on a **reasoning** (`thought: true`) part must never be
/// recorded as replayable: the request builder emits no `thought` parts, so
/// there is no byte-identical part to stamp it on. Re-homing it — which is
/// what the old code did by treating every text part as a signature site —
/// is exactly what Google answers `400 Corruption thought signature` to.
#[test]
fn signature_on_a_thought_part_is_never_replayable() {
    let value: Value = serde_json::from_str(
        r#"{
          "candidates": [{
            "content": {
              "parts": [
                {"text": "deliberating", "thought": true, "thoughtSignature": "sig-thought"},
                {"text": "the answer"}
              ]
            }
          }]
        }"#,
    )
    .unwrap();
    let message = super::message(&value).unwrap();
    assert_eq!(message.reasoning_content.as_deref(), Some("deliberating"));
    // Nothing replayable was minted, so no sidecar is emitted at all.
    assert!(
        message.provider_meta.is_none(),
        "a signature with no replayable home must not be recorded: {:?}",
        message.provider_meta
    );
}

/// A single text part carrying a signature IS byte-identical to the merged
/// answer, so that signature is replayable on the same part.
#[test]
fn signature_on_the_only_text_part_is_replayable() {
    let value: Value = serde_json::from_str(
        r#"{
          "candidates": [{
            "content": {
              "parts": [{"text": "whole answer", "thoughtSignature": "sig-text"}]
            }
          }]
        }"#,
    )
    .unwrap();
    let message = super::message(&value).unwrap();
    let meta = message.provider_meta.unwrap();
    assert_eq!(
        meta[TEXT_THOUGHT_SIGNATURE_META_KEY],
        Value::String("sig-text".into())
    );
    assert!(meta.get(TERMINAL_THOUGHT_SIGNATURE_META_KEY).is_none());
}

/// A zero-length text part is the documented terminal frame: its signature
/// signs the *turn*, so it is recorded as a turn-level signature — valid on
/// the turn's `functionCall`, never on the answer text.
#[test]
fn terminal_frame_signature_becomes_a_turn_signature() {
    let value: Value = serde_json::from_str(
        r#"{
          "candidates": [{
            "content": {
              "parts": [
                {"functionCall": {"name": "find_files", "args": {"path": "."}}},
                {"text": "", "thoughtSignature": "sig-terminal"}
              ]
            },
            "finishReason": "STOP"
          }]
        }"#,
    )
    .unwrap();
    let message = super::message(&value).unwrap();
    let meta = message.provider_meta.unwrap();
    assert!(meta.get(TEXT_THOUGHT_SIGNATURE_META_KEY).is_none());
    assert_eq!(
        meta[TERMINAL_THOUGHT_SIGNATURE_META_KEY],
        Value::String("sig-terminal".into())
    );
}

/// A streamed answer arrives as many deltas, so the merged answer is not
/// byte-identical to the signed delta. The signature must survive as a
/// turn-level signature rather than being stamped onto the merged text.
#[test]
fn multi_delta_stream_survives_as_a_turn_signature() {
    let mut sites = super::TextSignatureSites::default();
    for payload in [
        r#"{"response":{"candidates":[{"content":{"parts":[
             {"text":"Hel","thoughtSignature":"sig-delta"}]}}]}}"#,
        r#"{"response":{"candidates":[{"content":{"parts":[{"text":"lo"}]}}]}}"#,
    ] {
        let value: Value = serde_json::from_str(payload).unwrap();
        sites.observe(&super::stream_payload_value(&value));
    }
    let resolved = sites.resolve();
    assert!(
        resolved.text_signature.is_none(),
        "the merged answer's bytes are not the bytes that were signed"
    );
    assert_eq!(resolved.turn_signature.as_deref(), Some("sig-delta"));
}

/// The counterpart: a single-delta answer IS byte-identical, so its
/// signature stays replayable on the text part and is not demoted.
#[test]
fn single_delta_stream_keeps_its_text_signature() {
    let mut sites = super::TextSignatureSites::default();
    let value: Value = serde_json::from_str(
        r#"{"response":{"candidates":[{"content":{"parts":[
             {"text":"whole answer","thoughtSignature":"sig-only"}]}}]}}"#,
    )
    .unwrap();
    sites.observe(&super::stream_payload_value(&value));
    let resolved = sites.resolve();
    assert_eq!(resolved.text_signature.as_deref(), Some("sig-only"));
    assert!(resolved.turn_signature.is_none());
}

/// The terminal zero-length signature frame arrives on essentially every
/// answer on this surface, so it must NOT disqualify the answer text's own
/// signature. Denying it would demote a valid text token to turn level, and
/// replaying a turn token on text is the defect this fix exists to prevent.
#[test]
fn a_terminal_frame_does_not_disqualify_the_text_signature() {
    let mut sites = super::TextSignatureSites::default();
    let value: Value = serde_json::from_str(
        r#"{"response":{"candidates":[{"content":{"parts":[
             {"text":"whole answer","thoughtSignature":"sig-text"},
             {"text":"","thoughtSignature":"sig-terminal"}]},
             "finishReason":"STOP"}]}}"#,
    )
    .unwrap();
    sites.observe(&super::stream_payload_value(&value));
    let resolved = sites.resolve();
    assert_eq!(
        resolved.text_signature.as_deref(),
        Some("sig-text"),
        "the answer text is byte-identical to the part that signed it"
    );
    assert_eq!(
        resolved.turn_signature.as_deref(),
        Some("sig-terminal"),
        "the terminal token signs the turn and is kept separately"
    );
}

/// A zero-length visible text part contributes no bytes to the answer, so it
/// must not count as a second text part when deciding byte-identity. Counting
/// it would disqualify a perfectly replayable signature.
#[test]
fn an_empty_text_part_does_not_look_like_a_second_text_part() {
    let value: Value = serde_json::from_str(
        r#"{"candidates":[{"content":{"parts":[
             {"text":"whole answer","thoughtSignature":"sig-text"},
             {"text":""}]},
             "finishReason":"STOP"}]}"#,
    )
    .unwrap();
    let message = super::message(&value).unwrap();
    let meta = message.provider_meta.expect("signed turn keeps a sidecar");
    assert_eq!(
        meta[TEXT_THOUGHT_SIGNATURE_META_KEY],
        Value::String("sig-text".into()),
        "an empty part adds no bytes, so the signed part is still the whole answer"
    );
}
