use super::*;

#[test]
fn normalizes_json_error_into_header_and_pretty_detail() {
    let raw = r#"OpenAI HTTP 429 Too Many Requests: {"error":{"code":"1305","message":"该模型当前访问量过大，请您稍后再试"}}
Gave up after 6 attempt(s); the upstream service appears overloaded. Resend the message to try again, or raise `[resilience] retry_max_attempts` for more attempts."#;
    let parsed = normalize(raw);
    assert_eq!(
        parsed.title,
        "OpenAI HTTP 429 Too Many Requests: 该模型当前访问量过大，请您稍后再试 (type: 1305)"
    );
    assert!(parsed.detail.is_some());
    let detail = parsed.detail.unwrap();
    assert!(detail.contains("\"code\": \"1305\""));
    assert!(detail.contains("该模型当前访问量过大，请您稍后再试"));
    assert!(!detail.contains("Gave up after"));
}

#[test]
fn normalizes_retry_exhausted_json_error() {
    let raw = r#"Exhausted 30 retry attempts — Google HTTP 429 Too Many Requests: {"error":{"code":429,"message":"Resource has been exhausted","status":"RESOURCE_EXHAUSTED"}}"#;
    let parsed = normalize(raw);
    assert_eq!(
        parsed.title,
        "Exhausted 30 retry attempts — Google HTTP 429 Too Many Requests: Resource has been exhausted"
    );
    assert!(parsed.detail.is_some());
    let detail = parsed.detail.unwrap();
    assert!(detail.contains("\"code\": 429"));
    assert!(detail.contains("\"status\": \"RESOURCE_EXHAUSTED\""));
}

#[test]
fn normalizes_plain_http_error_without_json() {
    let raw = "OpenAI HTTP 503 Service Unavailable";
    let parsed = normalize(raw);
    assert_eq!(parsed.title, "OpenAI HTTP 503 Service Unavailable");
    assert_eq!(parsed.detail, None);
}

#[test]
fn normalizes_plain_message_without_detail() {
    let raw = "Failed to connect to host";
    let parsed = normalize(raw);
    assert_eq!(parsed.title, "Failed to connect to host");
    assert_eq!(parsed.detail, None);
}

#[test]
fn normalizes_multiline_text_into_first_line_and_rest() {
    let raw = "Claude HTTP 500 Server Error\nDetails:\nhost unreachable";
    let parsed = normalize(raw);
    assert_eq!(parsed.title, "Claude HTTP 500 Server Error");
    assert_eq!(
        parsed.detail,
        Some("Details:\nhost unreachable".to_string())
    );
}

#[test]
fn normalizes_plain_text_with_escaped_newlines() {
    let raw =
        "Provider HTTP 400 Bad Request: Invalid message format\\nLine 2 details\\nLine 3 guidance";
    let parsed = normalize(raw);
    assert_eq!(
        parsed.title,
        "Provider HTTP 400 Bad Request: Invalid message format"
    );
    assert_eq!(
        parsed.detail,
        Some("Line 2 details\nLine 3 guidance".to_string())
    );
}

#[test]
fn a_provider_diagnostics_lead_line_stays_the_title() {
    // A provider fault whose upstream body is content-free (Google's
    // canonical `400 INVALID_ARGUMENT`) is augmented with a report of the
    // request that was sent. The *first* line is still the fault itself, so
    // the entry's bold lead must remain the provider's own words — the
    // evidence block is detail, and must survive intact for the reader to
    // act on. This is the client half of that contract: the producer emits
    // brace-free prose (see `nuo_provider_google_antigravity::RequestShape`),
    // so the JSON heuristic below cannot re-parse it as a payload.
    let raw = "Google HTTP 400 Bad Request: Request contains an invalid argument.\n\n\
The request that was refused carried: model=gemini-3.8-flash-tiered, \
thinkingConfig=(includeThoughts=true, thinkingLevel=HIGH), tools=12, contents=43.\n\
Google's message names no field, so read this list against the upstream contract: \
a `thinkingLevel` outside `LOW`/`MEDIUM`/`HIGH` is a 400.";
    let parsed = normalize(raw);
    assert_eq!(
        parsed.title,
        "Google HTTP 400 Bad Request: Request contains an invalid argument."
    );
    let detail = parsed.detail.expect("the sent-shape evidence must survive");
    assert!(detail.contains("thinkingLevel=HIGH"), "{detail}");
    assert!(detail.contains("contents=43"), "{detail}");
    assert!(detail.contains("names no field"), "{detail}");
}

/// End-to-end lock for the whole reported defect: the provider builds the
/// error from a real refusal, the client normalizes it, and the transcript
/// entry must actually carry the evidence into its rendered body.
///
/// This crosses the crate boundary on purpose (the Google provider is a
/// dev-dependency) because the defect spanned it: the provider owned the
/// `thinkingLevel` value *and* the diagnostic, while the notice model owned
/// the rendering. A unit test on either side alone would have passed while
/// the user still saw one bare line.
#[test]
fn a_content_free_400_reaches_the_transcript_with_its_evidence() {
    use crate::model::document::{NoticeSeverity, TranscriptMessage};
    use nuo_provider::ProviderErrorKind;

    // 1. The upstream refusal, verbatim — this is all Google says.
    let upstream = nuo_provider::ProviderError::new(
        "Google",
        ProviderErrorKind::InvalidRequest,
        "Google HTTP 400 Bad Request: Request contains an invalid argument.",
    )
    .with_status(400);

    // 2. The provider folds in what it actually sent.
    let shape = nuo_provider_google_antigravity::response::RequestShape::of(
        "gemini-3.8-flash-tiered",
        &serde_json::json!({
            "contents": [{"role": "user", "parts": [{"text": "hi"}]}],
            "generationConfig": {
                "thinkingConfig": {"includeThoughts": true, "thinkingLevel": "HIGH"}
            },
        }),
    );
    let clarified = nuo_provider_google_antigravity::response::clarify_error_with_shape(
        upstream,
        "gemini-3.8-flash",
        "https://cloudcode-pa.googleapis.com",
        Some(&shape),
    );

    // 3. The client builds the transcript entry from that message.
    let msg = TranscriptMessage::notice(NoticeSeverity::Error, clarified.message().to_string());

    // The painted body must lead with the fault and carry the evidence —
    // not collapse to the single unattributable line that was reported.
    assert!(
        msg.raw
            .starts_with("Google HTTP 400 Bad Request: Request contains an invalid argument."),
        "the fault must stay the lead line: {}",
        msg.raw
    );
    assert!(
        msg.raw.contains("model=gemini-3.8-flash-tiered"),
        "the routed wire model must be visible: {}",
        msg.raw
    );
    assert!(
        msg.raw.contains("thinkingLevel=HIGH"),
        "the stamped depth must be visible: {}",
        msg.raw
    );
    assert!(
        msg.raw.lines().count() > 1,
        "the entry must be multi-line, not a single template line: {}",
        msg.raw
    );
    // Every evidence line survives into a painted block (no line is lost
    // between normalization and the block list the renderer walks).
    let painted = msg
        .blocks
        .iter()
        .map(|block| block.raw_text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(painted.contains("thinkingLevel=HIGH"), "{painted}");
    assert!(painted.contains("names no field"), "{painted}");
}

/// The reported failure was `400 Corrupted thought signature`, which names no
/// field either — and which the old evidence block could not even suggest,
/// because it listed everything *except* the tokens actually sent. This locks
/// the missing fact end to end: a body carrying replayed signatures must make
/// the notice report them, so a refusal of this class is attributable without
/// a `/debug trace` capture.
#[test]
fn a_replayed_signature_becomes_visible_evidence() {
    use crate::model::document::{NoticeSeverity, TranscriptMessage};

    let upstream = nuo_provider::ProviderError::new(
        "Google",
        nuo_provider::ProviderErrorKind::InvalidRequest,
        "Google HTTP 400 Bad Request: Corrupted thought signature.",
    )
    .with_status(400);

    // Two replayed tokens, one of them the documented bypass sentinel.
    let shape = nuo_provider_google_antigravity::response::RequestShape::of(
        "gemini-3.8-flash-high",
        &serde_json::json!({
            "contents": [
                {"role": "user", "parts": [{"text": "hi"}]},
                {"role": "model", "parts": [{
                    "functionCall": {"name": "find_files", "args": {}},
                    "thoughtSignature": "sig-replayed"
                }]},
                {"role": "model", "parts": [{
                    "functionCall": {"name": "read", "args": {}},
                    "thoughtSignature": "skip_thought_signature_validator"
                }]},
            ],
        }),
    );
    let clarified = nuo_provider_google_antigravity::response::clarify_error_with_shape(
        upstream,
        "gemini-3.8-flash-high",
        "https://cloudcode-pa.googleapis.com",
        Some(&shape),
    );
    let msg = TranscriptMessage::notice(NoticeSeverity::Error, clarified.message().to_string());

    assert!(
        msg.raw.contains("replayedThoughtSignatures=1"),
        "the real token count must be reported (the sentinel is not a real token): {}",
        msg.raw
    );
    assert!(
        msg.raw.contains("bypass sentinels"),
        "the sentinel count must be reported too: {}",
        msg.raw
    );
    // The cause the user was never told to suspect is now named first.
    assert!(
        msg.raw.contains("thoughtSignature"),
        "the refusal class must be attributable from the notice: {}",
        msg.raw
    );
    assert!(msg.raw.contains("different wire identity"), "{}", msg.raw);
}

/// The counterpart: a body with no replayed token says so explicitly, so the
/// reader is not left wondering whether the evidence block silently omitted it.
#[test]
fn an_unsigned_body_reports_no_replayed_signature() {
    let shape = nuo_provider_google_antigravity::response::RequestShape::of(
        "gemini-3.8-flash-high",
        &serde_json::json!({
            "contents": [{"role": "user", "parts": [{"text": "hi"}]}],
        }),
    );
    assert_eq!(shape.replayed_signatures, 0);
    assert!(shape.describe().contains("replayedThoughtSignatures=none"));
}

#[test]
fn normalizes_nested_gateway_json_error_digesting_escapes_and_newlines() {
    let raw = r#"OpenAI Responses HTTP 400 Bad Request: {"error":{"message":"{\"error\":{\"message\":\"The messages parameter is illegal. Please check the documentation.\",\"type\":\"AI_APICallError\",\"param\":{\"error\":\"The messages parameter is illegal. Please check the documentation.\",\"statusCode\":400,\"name\":\"AI_APICallError\",\"message\":\"The messages parameter is illegal. Please check the documentation.\",\"isRetryable\":false,\"type\":\"AI_APICallError\"}},\"providerMetadata\":{\"gateway\":{\"routing\":{\"originalModelId\":\"zai/glm-5.3-flashx\",\"resolvedProvider\":\"zai\",\"fallbacksAvailable\":[],\"canonicalSlug\":\"zai/glm-5.3-flashx\",\"modelAttemptCount\":1,\"modelAttempts\":[{\"canonicalSlug\":\"zai/glm-5.3-flashx\",\"success\":false,\"providerAttemptCount\":1,\"providerAttempts\":[{\"provider\":\"zai\",\"credentialType\":\"system\",\"success\":false,\"error\":\"The messages parameter is illegal. Please check the documentation.\",\"startTime\":1792727628864,\"endTime\":1792727630532,\"providerRequestId\":\"20261023115350796e7fbd69c94610\",\"statusCode\":400}]}],\"totalProviderAttemptCount\":1,\"affinity\":{\"outcome\":\"no_pin\"},\"cli…(truncated)","type":"invalid_request_error"}}"#;
    let parsed = normalize(raw);
    assert_eq!(
        parsed.title,
        "OpenAI Responses HTTP 400 Bad Request: The messages parameter is illegal. Please check the documentation. (type: invalid_request_error)"
    );
    assert!(parsed.detail.is_some());
    let detail = parsed.detail.unwrap();
    // The nested JSON string must be unpacked into a structured object,
    // so `\"` escapes must not appear and the inner message must be formatted cleanly.
    assert!(!detail.contains(r#"{\"error\""#));
    assert!(detail.contains("The messages parameter is illegal. Please check the documentation."));
    assert!(detail.contains("\"canonicalSlug\": \"zai/glm-5.3-flashx\""));
}

#[test]
fn display_text_is_lead_then_detail_verbatim() {
    // The lead/detail seam is one newline: the detail keeps its own
    // paragraph breaks, and the entry head is what separates the lead
    // visually — an injected blank row would show up as a phantom gap.
    let text = normalize("Claude HTTP 500 Server Error\nDetails:\nhost unreachable").display_text();
    assert_eq!(
        text,
        "Claude HTTP 500 Server Error\nDetails:\nhost unreachable"
    );
}

#[test]
fn display_text_of_a_lead_only_notice_is_the_lead() {
    assert_eq!(
        display_text("Cancelled via [Esc Esc]"),
        "Cancelled via [Esc Esc]"
    );
}

#[test]
fn blank_detail_never_mints_an_empty_paragraph() {
    let text = NoticeText {
        title: "lead".into(),
        detail: Some("   \n".into()),
    }
    .display_text();
    assert_eq!(text, "lead");
}
