//! Integration test: panicking on assertion failure is the desired
//! behaviour here, so the workspace `unwrap_used`/`expect_used` lints
//! are relaxed for this file. (Lib/bin code stays linted.)
#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Wire-level integration tests for the chat/streaming provider implementations.
//!
//! The in-module unit tests can only exercise the *pure* parsing helpers
//! (`parse_openai_stream_data`, `parse_anthropic_stream_data`, the echo filter)
//! because the `chat` / `stream_chat_events` methods build a live `reqwest`
//! request. These tests stand up a localhost mock HTTP server (mockito) and
//! drive the full request → HTTP → SSE-byte-reassembly → event-parse path, so
//! the integration behaviour — header attachment, error classification, and
//! echo suppression over a real stream — is covered.

use futures::StreamExt;
use mockito::{Matcher, Server};
use nuo_model_codec::{Message, Provider, ProviderStreamEvent, Role, SecretString};
use nuo_providers::{
    AnthropicMessagesProvider, OpenAiChatCompletionsProvider, OpenAiResponsesProvider,
};
use serde_json::{Value, json};

/// Join SSE `data:` events into a single response body. Each event becomes one
/// `data: <payload>\n\n` frame — the shape `sse::data_payloads` decodes.
fn sse_body(events: &[&str]) -> String {
    events
        .iter()
        .map(|event| format!("data: {event}\n\n"))
        .collect()
}

/// Collect a stream of provider events into a flat `Vec`, failing if any item
/// is itself an `Err`. Mirrors how the harness drains a turn's event stream.
async fn collect_events(
    stream: futures::stream::BoxStream<
        'static,
        Result<ProviderStreamEvent, nuo_model_codec::ProviderError>,
    >,
) -> Vec<ProviderStreamEvent> {
    let mut out = Vec::new();
    for item in stream.collect::<Vec<_>>().await {
        out.push(item.expect("stream item must be Ok"));
    }
    out
}

// ═════════════════════════════════════════════════════════════════════════════
// OpenAI-compatible provider
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn openai_chat_completions_parses_content_reasoning_tool_calls_and_headers() {
    let mut server = Server::new_async().await;
    let url = format!("{}/v1/chat/completions", server.url());
    let _mock = server
        .mock("POST", "/v1/chat/completions")
        // The bearer token and chosen user agent must reach the wire.
        .match_header("authorization", "Bearer test-key")
        .match_header("user-agent", "muta-test/1")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            r#"{"choices":[{"message":{"content":"Hello!","reasoning_content":"thinking","tool_calls":[{"id":"call_1","type":"function","function":{"name":"bash","arguments":"{\"command\":\"ls\"}"}}]}}]}"#,
        )
        .create_async()
        .await;

    let provider = OpenAiChatCompletionsProvider::with_base_url_and_user_agent(
        "test-key".to_string(),
        "gpt-test".to_string(),
        &url,
        "muta-test/1",
    );
    let message = provider
        .chat(vec![Message::new(Role::User, "hi")].into())
        .await
        .expect("chat should succeed")
        .message;

    assert_eq!(message.content, "Hello!");
    assert_eq!(
        message.reasoning_content.as_deref(),
        Some("thinking"),
        "reasoning_content must be parsed"
    );
    let calls = message
        .tool_calls
        .as_ref()
        .expect("tool_calls must be present");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].id, "call_1");
    assert_eq!(calls[0].name, "bash");
    assert_eq!(calls[0].arguments, r#"{"command":"ls"}"#);
}

#[derive(Debug)]
struct MockOAuthSource {
    auth: nuo_model_codec::ResolvedAuth,
}

impl nuo_model_codec::CredentialSource for MockOAuthSource {
    fn resolve_auth<'a>(
        &'a self,
    ) -> futures::future::BoxFuture<'a, Result<nuo_model_codec::ResolvedAuth, String>> {
        Box::pin(futures::future::ready(Ok(self.auth.clone())))
    }

    fn force_refresh<'a>(
        &'a self,
    ) -> futures::future::BoxFuture<'a, Result<nuo_model_codec::ResolvedAuth, String>> {
        Box::pin(futures::future::ready(Ok(self.auth.clone())))
    }

    fn is_oauth(&self) -> bool {
        true
    }
}

#[tokio::test]
async fn chatgpt_responses_chat_uses_streaming_transport_and_dynamic_credentials() {
    let mut server = Server::new_async().await;
    let url = format!("{}/backend-api/codex/responses", server.url());
    let _mock = server
        .mock("POST", "/backend-api/codex/responses")
        .match_header("authorization", "Bearer live-oauth-token")
        .match_header("chatgpt-account-id", "acct-test")
        .match_header("originator", "codex_cli_rs")
        .match_body(Matcher::PartialJson(json!({
            "model": "gpt-5.6-sol",
            "store": false,
            "stream": true,
            "include": ["reasoning.encrypted_content"]
        })))
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body(sse_body(&[
            r#"{"type":"response.output_text.delta","delta":"ok"}"#,
            r#"{"type":"response.output_item.done","output_index":0,"item":{"id":"msg_1","type":"message","role":"assistant","content":[{"type":"output_text","text":"ok"}],"status":"completed"}}"#,
            r#"{"type":"response.completed","response":{"id":"resp_1","output":[],"usage":{"input_tokens":3,"output_tokens":1,"total_tokens":4}}}"#,
        ]))
        .create_async()
        .await;

    let auth = nuo_model_codec::ResolvedAuth::new("live-oauth-token")
        .with_extension(nuo_model_codec::ChatGptAuthMetadata {
            account_id: "acct-test".to_string(),
        });
    let provider = OpenAiResponsesProvider::with_credentials(
        std::sync::Arc::new(MockOAuthSource { auth }),
        "gpt-5.6-sol".to_string(),
        &url,
    )
    .with_dialect(nuo_model_codec::OpenAiResponsesDialect::ChatGpt);
    let message = provider
        .chat(vec![Message::new(Role::User, "hi")].into())
        .await
        .expect("credential-source bearer should reach the ChatGPT backend")
        .message;

    assert_eq!(message.content, "ok");
}

#[tokio::test]
async fn openai_chat_completions_strips_tool_call_echo_when_native_calls_present() {
    // GLM/Qwen leak: the same tool call arrives both as `content` text and as a
    // native `tool_calls` entry. The native call wins and the textual mirror is
    // suppressed so raw JSON never reaches the UI.
    let mut server = Server::new_async().await;
    let url = format!("{}/v1/chat/completions", server.url());
    let _mock = server
        .mock("POST", "/v1/chat/completions")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            r#"{"choices":[{"message":{"content":"{\"tool\":\"bash\",\"arguments\":{\"command\":\"ls\"}}","tool_calls":[{"id":"call_1","type":"function","function":{"name":"bash","arguments":"{\"command\":\"ls\"}"}}]}}]}"#,
        )
        .create_async()
        .await;

    let provider =
        OpenAiChatCompletionsProvider::with_base_url("k".to_string(), "m".to_string(), &url);
    let message = provider
        .chat(vec![Message::new(Role::User, "hi")].into())
        .await
        .expect("chat should succeed")
        .message;

    assert!(
        message.content.is_empty(),
        "mirrored echo must be stripped when native tool calls are present: got {:?}",
        message.content
    );
    assert_eq!(
        message.tool_calls.as_ref().expect("native call")[0].name,
        "bash"
    );
}

#[tokio::test]
async fn openai_chat_completions_classifies_server_error_as_retryable() {
    let mut server = Server::new_async().await;
    let url = format!("{}/v1/chat/completions", server.url());
    let _mock = server
        .mock("POST", "/v1/chat/completions")
        .with_status(500)
        .with_body("upstream boom")
        .create_async()
        .await;

    let provider =
        OpenAiChatCompletionsProvider::with_base_url("k".to_string(), "m".to_string(), &url);
    let error = provider
        .chat(vec![Message::new(Role::User, "hi")].into())
        .await
        .expect_err("5xx must surface as an error");

    // ensure_success tags 5xx as retryable so the harness backs off and retries.
    assert!(
        matches!(
            error.retry_disposition(),
            nuo_model_codec::RetryDisposition::Retry { .. }
        ),
        "5xx must be classified retryable: {error}"
    );
    assert!(error.message().contains("HTTP 500"));
}

#[tokio::test]
async fn openai_chat_completions_omits_auth_header_when_api_key_is_empty() {
    // Keyless servers (a local `llama-server` started without `--api-key`) must
    // not receive an empty `Authorization: Bearer ` header, which some servers
    // reject even when they would otherwise ignore the key.
    let mut server = Server::new_async().await;
    let url = format!("{}/v1/chat/completions", server.url());
    let _mock = server
        .mock("POST", "/v1/chat/completions")
        .match_header("authorization", Matcher::Missing)
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"choices":[{"message":{"content":"ok"}}]}"#)
        .create_async()
        .await;

    let provider = OpenAiChatCompletionsProvider::with_base_url_and_user_agent(
        String::new(),
        "m".to_string(),
        &url,
        "ua",
    );
    let message = provider
        .chat(vec![Message::new(Role::User, "hi")].into())
        .await
        .expect("keyless chat should succeed")
        .message;
    assert_eq!(message.content, "ok");
}

#[tokio::test]
async fn openai_chat_completions_decode_failure_embeds_raw_body() {
    // A gateway/CDN interstitial returns 200 with an HTML body instead of JSON.
    // reqwest's own `.json()` would surface only "error decoding response body"
    // with no hint of the cause; the decode helper must embed the raw text.
    let mut server = Server::new_async().await;
    let url = format!("{}/v1/chat/completions", server.url());
    let _mock = server
        .mock("POST", "/v1/chat/completions")
        .with_status(200)
        .with_header("content-type", "text/html")
        .with_body("<html><body>502 Bad Gateway</body></html>")
        .create_async()
        .await;

    let provider =
        OpenAiChatCompletionsProvider::with_base_url("k".to_string(), "m".to_string(), &url);
    let error = provider
        .chat(vec![Message::new(Role::User, "hi")].into())
        .await
        .expect_err("non-JSON 200 must surface as a decode error");

    assert!(
        error.message().contains("error decoding response body"),
        "should name the decode failure: {error}"
    );
    assert!(
        error.message().contains("502 Bad Gateway"),
        "should embed the raw body preview so the cause is diagnosable: {error}"
    );
}

#[tokio::test]
async fn openai_stream_parses_text_reasoning_and_tool_call_deltas() {
    let mut server = Server::new_async().await;
    let url = format!("{}/v1/chat/completions", server.url());
    let body = sse_body(&[
        r#"{"choices":[{"delta":{"content":"Hel"}}]}"#,
        r#"{"choices":[{"delta":{"content":"lo"}}]}"#,
        r#"{"choices":[{"delta":{"reasoning_content":"hm"}}]}"#,
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"bash","arguments":"{\"command\":\"pwd\"}"}}]}}]}"#,
        "[DONE]",
    ]);
    let _mock = server
        .mock("POST", "/v1/chat/completions")
        // The streaming path must request a stream.
        .match_body(Matcher::PartialJson(json!({"stream": true})))
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body(body)
        .create_async()
        .await;

    let provider =
        OpenAiChatCompletionsProvider::with_base_url("k".to_string(), "m".to_string(), &url);
    let stream = provider
        .stream_chat_events(vec![Message::new(Role::User, "hi")].into())
        .await
        .expect("stream should open");
    let events = collect_events(stream).await;

    let text: String = events
        .iter()
        .filter_map(|event| match event {
            ProviderStreamEvent::TextDelta(delta) => Some(delta.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "Hello");

    assert!(events.iter().any(|event| matches!(
        event,
        ProviderStreamEvent::ReasoningDelta(reasoning) if reasoning == "hm"
    )));
    let tool_calls: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            ProviderStreamEvent::ToolCallDelta { name, .. } => name.clone(),
            _ => None,
        })
        .collect();
    assert_eq!(tool_calls, vec!["bash".to_string()]);
}

#[tokio::test]
async fn openrouter_stream_uses_gateway_dialect_and_returns_replay_artifacts() {
    let mut server = Server::new_async().await;
    let url = format!("{}/api/v1/chat/completions", server.url());
    let body = sse_body(&[
        r#"{"choices":[{"delta":{"reasoning":"inspect ","reasoning_details":[{"type":"reasoning.text","text":"inspect ","id":"r1","index":0}]}}]}"#,
        r#"{"choices":[{"delta":{"reasoning":"files","reasoning_details":[{"text":"files","signature":"sig","index":0}]}}]}"#,
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"read","arguments":"{}"}}]}}]}"#,
        "[DONE]",
    ]);
    let _mock = server
        .mock("POST", "/api/v1/chat/completions")
        .match_header("authorization", "Bearer sk-or-test")
        .match_header("x-openrouter-title", "Muta")
        .match_body(Matcher::PartialJson(json!({
            "model": "nex-agi/nex-n2.5-pro:free",
            "stream": true,
            "reasoning": {"effort": "high"}
        })))
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body(body)
        .create_async()
        .await;

    let capabilities = nuo_model_codec::ModelCapabilities {
        family: "nex".into(),
        context_window: 262_144,
        max_output_tokens: Some(235_929),
        thinking: nuo_model_codec::ReasoningSupport::ReasoningContent,
        tool_call: true,
        // `vision` is three-valued (ADR-0230); this fixture declares support
        // explicitly rather than leaving it undeclared.
        vision: Some(true),
        effort_levels: vec![
            nuo_model_codec::Effort::None.into(),
            nuo_model_codec::Effort::Medium.into(),
            nuo_model_codec::Effort::High.into(),
        ],
    };
    let provider = OpenAiChatCompletionsProvider::with_base_url(
        "sk-or-test".into(),
        "nex-agi/nex-n2.5-pro:free".into(),
        &url,
    )
    .with_dialect(nuo_model_codec::OpenAiChatDialect::OpenRouter)
    .with_reasoning_effort(Some(nuo_model_codec::Effort::High))
    .with_model_capabilities(capabilities);

    let events = collect_events(
        provider
            .stream_chat_events(vec![Message::new(Role::User, "inspect")].into())
            .await
            .expect("OpenRouter stream should open"),
    )
    .await;
    let reasoning: String = events
        .iter()
        .filter_map(|event| match event {
            ProviderStreamEvent::ReasoningDelta(delta) => Some(delta.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(reasoning, "inspect files");
    let artifacts = events
        .iter()
        .find_map(|event| match event {
            ProviderStreamEvent::Completed(meta) => meta.artifacts.as_ref(),
            _ => None,
        })
        .expect("terminal event must carry reasoning replay artifacts");
    assert_eq!(
        artifacts["openrouter_reasoning_details"][0]["text"],
        "inspect files"
    );
    assert_eq!(
        artifacts["openrouter_reasoning_details"][0]["signature"],
        "sig"
    );
}

#[tokio::test]
async fn openai_stream_strips_echo_text_when_native_tool_calls_stream_in() {
    // Over a real stream: the textual tool-call mirror and the native tool-call
    // delta both arrive. The echo filter must hold the mirror and drop it once
    // the native call is observed, so no raw JSON leaks as a TextDelta.
    let mut server = Server::new_async().await;
    let url = format!("{}/v1/chat/completions", server.url());
    let body = sse_body(&[
        r#"{"choices":[{"delta":{"content":"{\"tool\":\"bash\",\"arguments\":{\"command\":\"ls\"}}"}}]}"#,
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"bash","arguments":"{\"command\":\"ls\"}"}}]}}]}"#,
        "[DONE]",
    ]);
    let _mock = server
        .mock("POST", "/v1/chat/completions")
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body(body)
        .create_async()
        .await;

    let provider =
        OpenAiChatCompletionsProvider::with_base_url("k".to_string(), "m".to_string(), &url);
    let stream = provider
        .stream_chat_events(vec![Message::new(Role::User, "hi")].into())
        .await
        .expect("stream should open");
    let events = collect_events(stream).await;

    assert!(
        !events
            .iter()
            .any(|event| matches!(event, ProviderStreamEvent::TextDelta(_))),
        "no TextDelta should survive: the echo must be stripped, got {events:?}"
    );
    assert!(events.iter().any(|event| matches!(
        event,
        ProviderStreamEvent::ToolCallDelta { name, .. } if name.as_deref() == Some("bash")
    )));
}

// ═════════════════════════════════════════════════════════════════════════════
// Anthropic-compatible /messages provider
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn anthropic_chat_assembles_text_thinking_and_tool_use() {
    let mut server = Server::new_async().await;
    let url = format!("{}/v1/messages", server.url());
    let _mock = server
        .mock("POST", "/v1/messages")
        // The Messages surface identifies via x-api-key + anthropic-version.
        .match_header("x-api-key", "test-key")
        .match_header("anthropic-version", "2023-06-01")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            r#"{"content":[
                {"type":"thinking","thinking":"deliberating"},
                {"type":"text","text":"Done."},
                {"type":"tool_use","id":"toolu_1","name":"bash","input":{"command":"ls"}}
            ]}"#,
        )
        .create_async()
        .await;

    let provider = AnthropicMessagesProvider::with_base_url_and_user_agent(
        "test-key".to_string(),
        "minimax-m3".to_string(),
        &url,
        "ua",
    );
    let message = provider
        .chat(vec![Message::new(Role::User, "hi")].into())
        .await
        .expect("chat should succeed")
        .message;

    assert_eq!(message.content, "Done.");
    assert_eq!(message.reasoning_content.as_deref(), Some("deliberating"));
    let calls = message
        .tool_calls
        .as_ref()
        .expect("tool_use must map to tool_calls");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].id, "toolu_1");
    assert_eq!(calls[0].name, "bash");
    // The `input` object is serialized back to a JSON argument string.
    let input: Value = serde_json::from_str(&calls[0].arguments).expect("input is valid json");
    assert_eq!(input["command"], "ls");
}

#[tokio::test]
async fn anthropic_stream_parses_tool_use_block_and_argument_fragments() {
    // A tool_use block opens at index 1 (id + name up front), then its argument
    // JSON streams in as `input_json_delta` fragments the harness concatenates.
    let mut server = Server::new_async().await;
    let url = format!("{}/v1/messages", server.url());
    let body = sse_body(&[
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hi"}}"#,
        r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":"bash"}}"#,
        r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"comm"}}"#,
        r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"and\":\"ls\"}"}}"#,
        r#"{"type":"message_stop"}"#,
    ]);
    let _mock = server
        .mock("POST", "/v1/messages")
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body(body)
        .create_async()
        .await;

    let provider = AnthropicMessagesProvider::with_base_url_and_user_agent(
        "k".to_string(),
        "minimax-m3".to_string(),
        &url,
        "ua",
    );
    let stream = provider
        .stream_chat_events(vec![Message::new(Role::User, "hi")].into())
        .await
        .expect("stream should open");
    let events = collect_events(stream).await;

    assert!(events.iter().any(|event| matches!(
        event,
        ProviderStreamEvent::TextDelta(text) if text == "Hi"
    )));
    // The opening block carries id + name; the two argument fragments follow.
    let tool_events: Vec<&ProviderStreamEvent> = events
        .iter()
        .filter(|event| matches!(event, ProviderStreamEvent::ToolCallDelta { .. }))
        .collect();
    assert_eq!(tool_events.len(), 3, "open + 2 fragments");
    assert!(matches!(
        tool_events[0],
        ProviderStreamEvent::ToolCallDelta { id, name, .. }
            if id.as_deref() == Some("toolu_1") && name.as_deref() == Some("bash")
    ));
}

#[tokio::test]
async fn anthropic_stream_surfaces_in_band_error_event() {
    // Anthropic can emit an `error` event mid-stream (e.g. overloaded); the
    // parser must surface it as an Err item rather than a silent empty stream.
    let mut server = Server::new_async().await;
    let url = format!("{}/v1/messages", server.url());
    let body = sse_body(&[
        r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#,
    ]);
    let _mock = server
        .mock("POST", "/v1/messages")
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body(body)
        .create_async()
        .await;

    let provider = AnthropicMessagesProvider::with_base_url_and_user_agent(
        "k".to_string(),
        "minimax-m3".to_string(),
        &url,
        "ua",
    );
    let stream = provider
        .stream_chat_events(vec![Message::new(Role::User, "hi")].into())
        .await
        .expect("stream should open");
    let items = stream.collect::<Vec<_>>().await;
    let errored = items.iter().any(|item| {
        item.as_ref()
            .is_err_and(|error| error.message().contains("Overloaded"))
    });
    assert!(
        errored,
        "in-band error must surface as an Err item: {items:?}"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// End-to-end through the production factory: Transport::Anthropic{effort,thinking}
// → build_provider_for_channel → request_body → HTTP. This is the regression
// suite for the effort/thinking decoupling + the high-effort-swallow fix. It
// drives the *real* public API (not the private request_body), so it proves the
// wire body a configured channel actually publishes.
// ═════════════════════════════════════════════════════════════════════════════

use nuo_model_codec::catalog::{Channel, Transport};
use nuo_model_codec::{Effort, ReasoningMode};
use nuo_providers::build_provider_for_channel;

/// Build a channel → factory provider, send one turn to a mockito server that
/// asserts the request body matches `expected` (partial JSON), and confirm the
/// call succeeds. The shared harness for the three decoupling regressions.
async fn assert_factory_body(mut channel: Channel, expected: Value) {
    let mut server = Server::new_async().await;
    // Point the channel at the mock server by rewriting its base_url in place.
    let url = format!("{}/v1/messages", server.url());
    if let Transport::Anthropic { base_url, .. } = &mut channel.transport {
        *base_url = url;
    }
    let _mock = server
        .mock("POST", "/v1/messages")
        .match_body(Matcher::PartialJson(expected))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"content":[{"type":"text","text":"ok"}]}"#)
        .create_async()
        .await;

    let provider = build_provider_for_channel(&channel, "anthropic", None);
    let msg = provider
        .chat(vec![Message::new(Role::User, "hi")].into())
        .await
        .expect("factory-built provider chat must succeed")
        .message;
    assert_eq!(msg.content, "ok");
}

/// Regression #1: an explicit `effort = "high"` MUST publish
/// `output_config.effort = "high"`. Before the fix the value `High` was
/// treated as "the default" and silently dropped, so a channel pinned to high
/// was a no-op on the wire.
#[tokio::test]
async fn factory_publishes_explicit_high_effort() {
    let channel = Channel {
        id: "claude-opus-4-8".into(),
        label: "Opus".into(),
        transport: Transport::Anthropic {
            base_url: String::new(), // rewritten by the harness
            client_profile: nuo_model_codec::ClientProfile::from("ua"),
            effort: Some(Effort::High),
            thinking: None,
            dialect: Default::default(),
        },
        credentials: nuo_model_codec::static_credential("k"),
        model: "claude-opus-4-8".into(),
        remote: None,
        user_overrides: None,
        prompt_cache_preference: nuo_model_codec::PromptCachePreference::default(),
        prompt_cache: nuo_model_codec::PromptCacheCapabilities::unsupported(),
    };
    assert_factory_body(channel, json!({ "output_config": { "effort": "high" } })).await;
}

/// Regression #2: effort and thinking stay DECOUPLED. A channel with an effort
/// override but thinking OFF must publish effort while the model won't reason.
/// (Previously setting effort forced `thinking:{adaptive}` on.) The pure-mode
/// contract (no `thinking` field) is asserted in the unit test
/// `effort_without_thinking_stays_decoupled`; this test proves the factory
/// honors an explicit `ReasoningMode::Off` together with an effort override end
/// to end — i.e. the two overrides reach the provider independently.
#[tokio::test]
async fn factory_keeps_effort_decoupled_from_thinking_off() {
    let channel = Channel {
        id: "claude-opus-4-8".into(),
        label: "Opus".into(),
        transport: Transport::Anthropic {
            base_url: String::new(),
            client_profile: nuo_model_codec::ClientProfile::from("ua"),
            effort: Some(Effort::Medium),
            thinking: Some(ReasoningMode::Off),
            dialect: Default::default(),
        },
        credentials: nuo_model_codec::static_credential("k"),
        model: "claude-opus-4-8".into(),
        remote: None,
        user_overrides: None,
        prompt_cache_preference: nuo_model_codec::PromptCachePreference::default(),
        prompt_cache: nuo_model_codec::PromptCacheCapabilities::unsupported(),
    };
    // The request publishes the effort override; the absence of a `thinking`
    // field is verified by the companion unit test.
    assert_factory_body(channel, json!({ "output_config": { "effort": "medium" } })).await;
}

/// Regression #3: a thinking ON override with no effort publishes
/// `thinking:{adaptive}` and omits `output_config` (no explicit effort).
#[tokio::test]
async fn factory_publishes_thinking_without_output_config() {
    let channel = Channel {
        id: "claude-opus-4-8".into(),
        label: "Opus".into(),
        transport: Transport::Anthropic {
            base_url: String::new(),
            client_profile: nuo_model_codec::ClientProfile::from("ua"),
            effort: None,
            thinking: Some(ReasoningMode::Adaptive),
            dialect: Default::default(),
        },
        credentials: nuo_model_codec::static_credential("k"),
        model: "claude-opus-4-8".into(),
        remote: None,
        user_overrides: None,
        prompt_cache_preference: nuo_model_codec::PromptCachePreference::default(),
        prompt_cache: nuo_model_codec::PromptCacheCapabilities::unsupported(),
    };
    assert_factory_body(
        channel,
        json!({ "thinking": { "type": "adaptive", "display": "summarized" } }),
    )
    .await;
}

/// Sonnet 5 thinking is ON by default when the `thinking` field is omitted, so
/// an explicit opt-OUT (`ReasoningMode::Off`) MUST publish
/// `thinking:{type:"disabled"}`. Omitting the field would leave the model
/// reasoning and billing against the user's ADR-0046 opt-out intent — the
/// regression this variant exists to catch.
#[tokio::test]
async fn sonnet5_opt_out_emits_explicit_disabled() {
    let channel = Channel {
        id: "claude-sonnet-5".into(),
        label: "Sonnet 5".into(),
        transport: Transport::Anthropic {
            base_url: String::new(),
            client_profile: nuo_model_codec::ClientProfile::from("ua"),
            effort: Some(Effort::High),
            thinking: Some(ReasoningMode::Off),
            dialect: Default::default(),
        },
        credentials: nuo_model_codec::static_credential("k"),
        model: "claude-sonnet-5".into(),
        remote: None,
        user_overrides: None,
        prompt_cache_preference: nuo_model_codec::PromptCachePreference::default(),
        prompt_cache: nuo_model_codec::PromptCacheCapabilities::unsupported(),
    };
    assert_factory_body(
        channel,
        json!({
            "thinking": { "type": "disabled" },
            "output_config": { "effort": "high" }
        }),
    )
    .await;
}

/// Sonnet 5 opt-IN publishes adaptive thinking (not `disabled`), and honors the
/// full effort range — here `xhigh`, which Sonnet 4.6 would reject.
#[tokio::test]
async fn sonnet5_opt_in_publishes_adaptive_and_full_effort_range() {
    let channel = Channel {
        id: "claude-sonnet-5".into(),
        label: "Sonnet 5".into(),
        transport: Transport::Anthropic {
            base_url: String::new(),
            client_profile: nuo_model_codec::ClientProfile::from("ua"),
            effort: Some(Effort::Xhigh),
            thinking: Some(ReasoningMode::Adaptive),
            dialect: Default::default(),
        },
        credentials: nuo_model_codec::static_credential("k"),
        model: "claude-sonnet-5".into(),
        remote: None,
        user_overrides: None,
        prompt_cache_preference: nuo_model_codec::PromptCachePreference::default(),
        prompt_cache: nuo_model_codec::PromptCacheCapabilities::unsupported(),
    };
    assert_factory_body(
        channel,
        json!({
            "thinking": { "type": "adaptive", "display": "summarized" },
            "output_config": { "effort": "xhigh" }
        }),
    )
    .await;
}

/// Fable 5 thinking is ALWAYS ON and cannot be disabled; even an explicit
/// `ReasoningMode::Off` override is a no-op on the wire, which still publishes
/// `thinking:{type:"adaptive"}`.
#[tokio::test]
async fn fable5_always_on_thinking_ignores_off_override() {
    let channel = Channel {
        id: "claude-fable-5".into(),
        label: "Fable 5".into(),
        transport: Transport::Anthropic {
            base_url: String::new(),
            client_profile: nuo_model_codec::ClientProfile::from("ua"),
            effort: None,
            thinking: Some(ReasoningMode::Off),
            dialect: Default::default(),
        },
        credentials: nuo_model_codec::static_credential("k"),
        model: "claude-fable-5".into(),
        remote: None,
        user_overrides: None,
        prompt_cache_preference: nuo_model_codec::PromptCachePreference::default(),
        prompt_cache: nuo_model_codec::PromptCacheCapabilities::unsupported(),
    };
    assert_factory_body(
        channel,
        json!({ "thinking": { "type": "adaptive", "display": "summarized" } }),
    )
    .await;
}

// ═════════════════════════════════════════════════════════════════════════════
// Live model-list discovery (list_models)
// ═════════════════════════════════════════════════════════════════════════════
//
// The in-module unit tests cover the pure parsers + endpoint derivation; these
// wire tests stand up a localhost mock and drive the full GET → JSON → parse
// path per protocol, asserting the exact headers/auth a chat request would send
// and that the returned ids are sorted + de-duplicated.

use nuo_providers::{
    CatalogShape, ModelListError, RemoteCatalogOptions, RemoteCatalogRequest, RemoteCatalogUpdate,
    fetch_remote_catalog, list_models,
};

#[tokio::test]
async fn opencode_console_list_models_sends_bearer_and_org_and_parses_config() {
    let mut server = Server::new_async().await;
    // The catalog root is `https://opencode.ai/console`-shaped; discovery
    // appends the shape path `api/config` (ADR-0269).
    let base_url = server.url();
    let _mock = server
        .mock("GET", "/api/config")
        .match_header("authorization", "Bearer st-live")
        .match_header("x-org-id", "wrk-org-1")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            r#"{
                "config": {
                    "provider": {
                        "opencode": {
                            "npm": "@ai-sdk/openai-compatible",
                            "api": "https://opencode.ai/inference/openai/v1",
                            "models": {
                                "glm-5.2": {
                                    "name": "GLM-5.2",
                                    "reasoning": true,
                                    "tool_call": true
                                },
                                "claude-opus-5": {
                                    "name": "Claude Opus 5",
                                    "reasoning": true,
                                    "tool_call": true,
                                    "provider": {
                                        "npm": "@ai-sdk/anthropic",
                                        "api": "https://opencode.ai/inference/anthropic/v1"
                                    }
                                }
                            }
                        }
                    }
                }
            }"#,
        )
        .create_async()
        .await;

    let key = SecretString::from("st-live");
    let req = RemoteCatalogRequest {
        protocol: CatalogShape::OpencodeConsole,
        base_url: &base_url,
        api_key: &key,
        account_id: None,
        org_id: Some("wrk-org-1"),
        user_agent: None,
        extra_headers: &[],
        catalog_signing: None,
        dimensions: &[],
    };
    let models = list_models(req).await.expect("discovery succeeds");
    let ids: Vec<&str> = models.iter().map(|model| model.id.as_str()).collect();
    assert_eq!(ids, vec!["claude-opus-5", "glm-5.2"]);
    // Per-model routing rides the catalog: npm → wire, api → root override.
    let glm = models.iter().find(|m| m.id == "glm-5.2").unwrap();
    assert_eq!(
        glm.protocol,
        Some(nuo_model_codec::WireProtocol::ChatCompletions)
    );
    assert_eq!(glm.endpoint, None);
    let claude = models.iter().find(|m| m.id == "claude-opus-5").unwrap();
    assert_eq!(
        claude.protocol,
        Some(nuo_model_codec::WireProtocol::AnthropicMessages)
    );
    assert_eq!(
        claude.endpoint.as_deref(),
        Some("https://opencode.ai/inference/anthropic/v1")
    );
}

#[tokio::test]
async fn openai_list_models_sends_bearer_and_returns_sorted_unique_ids() {
    let mut server = Server::new_async().await;
    let root_url = format!("{}/v1", server.url());
    // The mock must be on the derived /v1/models path.
    let _mock = server
        .mock("GET", "/v1/models")
        // Auth matches the chat path: a bearer when a key is set.
        .match_header("authorization", "Bearer sk-live")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            r#"{"data":[
                {"id":"zeta-last"},
                {"id":"alpha-first"},
                {"id":"alpha-first"},
                {"id":"mid-model"}
            ]}"#,
        )
        .create_async()
        .await;

    let key = SecretString::from("sk-live");
    let req = RemoteCatalogRequest {
        protocol: CatalogShape::OpenAi,
        base_url: &root_url,
        api_key: &key,
        account_id: None,
        org_id: None,
        user_agent: None,
        extra_headers: &[],
        catalog_signing: None,
        dimensions: &[],
    };
    let models = list_models(req).await.expect("discovery succeeds");
    // Sorted + de-duplicated, regardless of the API's ordering or duplicates.
    let ids: Vec<&str> = models.iter().map(|model| model.id.as_str()).collect();
    assert_eq!(ids, vec!["alpha-first", "mid-model", "zeta-last"]);
}

#[tokio::test]
async fn openai_list_models_keyless_relay_sends_no_bearer_header() {
    // A keyless relay sends NO Authorization header at all (mirrors the chat
    // path); the mock rejects any request carrying one.
    let mut server = Server::new_async().await;
    let _mock = server
        .mock("GET", "/v1/models")
        .match_header("authorization", Matcher::Missing)
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"data":[{"id":"relay-only"}]}"#)
        .create_async()
        .await;

    let key = SecretString::default();
    let req = RemoteCatalogRequest {
        protocol: CatalogShape::OpenAi,
        base_url: &format!("{}/v1", server.url()),
        api_key: &key,
        account_id: None,
        org_id: None,
        user_agent: None,
        extra_headers: &[],
        catalog_signing: None,
        dimensions: &[],
    };
    let models = list_models(req).await.expect("keyless discovery succeeds");
    let ids: Vec<&str> = models.iter().map(|model| model.id.as_str()).collect();
    assert_eq!(ids, vec!["relay-only"]);
}

#[tokio::test]
async fn codex_list_models_sends_subscription_headers_and_preserves_priority() {
    let mut server = Server::new_async().await;
    let _mock = server
        .mock("GET", "/backend-api/codex/models")
        .match_query(Matcher::UrlEncoded(
            "client_version".to_string(),
            nuo_model_codec::client_identity::CODEX_VERSION.to_string(),
        ))
        .match_header("authorization", "Bearer chatgpt-access")
        .match_header("originator", "codex_cli_rs")
        .match_header("chatgpt-account-id", "acct-test")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_header("etag", "\"catalog-v2\"")
        .with_body(
            r#"{"models":[
                {"slug":"second","priority":2,"visibility":"list","supported_in_api":true,"supported_reasoning_levels":[]},
                {"slug":"first","priority":1,"visibility":"list","supported_in_api":true,"supported_reasoning_levels":[{"effort":"high"}]}
            ]}"#,
        )
        .create_async()
        .await;

    let key = SecretString::from("chatgpt-access");
    let req = RemoteCatalogRequest {
        protocol: CatalogShape::Codex,
        base_url: &format!("{}/backend-api/codex", server.url()),
        api_key: &key,
        account_id: Some("acct-test"),
        org_id: None,
        user_agent: None,
        extra_headers: &[],
        catalog_signing: None,
        dimensions: &[],
    };
    let update = fetch_remote_catalog(req, RemoteCatalogOptions { etag: None })
        .await
        .expect("Codex discovery succeeds");
    let RemoteCatalogUpdate::Modified { models, etag } = update else {
        panic!("expected a modified catalog");
    };
    assert_eq!(etag.as_deref(), Some("\"catalog-v2\""));
    assert_eq!(
        models
            .iter()
            .map(|model| model.id.as_str())
            .collect::<Vec<_>>(),
        vec!["first", "second"]
    );
}

#[tokio::test]
async fn codex_list_models_supports_etag_revalidation() {
    let mut server = Server::new_async().await;
    let _mock = server
        .mock("GET", "/backend-api/codex/models")
        .match_query(Matcher::UrlEncoded(
            "client_version".to_string(),
            nuo_model_codec::client_identity::CODEX_VERSION.to_string(),
        ))
        .match_header("if-none-match", "\"catalog-v2\"")
        .with_status(304)
        .create_async()
        .await;

    let key = SecretString::from("chatgpt-access");
    let req = RemoteCatalogRequest {
        protocol: CatalogShape::Codex,
        base_url: &format!("{}/backend-api/codex", server.url()),
        api_key: &key,
        account_id: None,
        org_id: None,
        user_agent: None,
        extra_headers: &[],
        catalog_signing: None,
        dimensions: &[],
    };
    let update = fetch_remote_catalog(
        req,
        RemoteCatalogOptions {
            etag: Some("\"catalog-v2\""),
        },
    )
    .await
    .expect("Codex catalog revalidation succeeds");
    assert_eq!(
        update,
        RemoteCatalogUpdate::NotModified {
            etag: Some("\"catalog-v2\"".to_string())
        }
    );
}

#[tokio::test]
async fn anthropic_list_models_sends_api_key_and_version_headers() {
    let mut server = Server::new_async().await;
    let _mock = server
        .mock("GET", "/v1/models")
        // Anthropic auth: x-api-key + the pinned anthropic-version header.
        .match_header("x-api-key", "sk-ant")
        .match_header(
            "anthropic-version",
            nuo_providers::protocol::anthropic::request::ANTHROPIC_VERSION,
        )
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            r#"{"data":[
                {"id":"claude-sonnet-5","display_name":"Sonnet"},
                {"id":"claude-opus-4-8","display_name":"Opus"}
            ]}"#,
        )
        .create_async()
        .await;

    let key = SecretString::from("sk-ant");
    let req = RemoteCatalogRequest {
        protocol: CatalogShape::Anthropic,
        base_url: &format!("{}/v1", server.url()),
        api_key: &key,
        account_id: None,
        org_id: None,
        user_agent: None,
        extra_headers: &[],
        catalog_signing: None,
        dimensions: &[],
    };
    let models = list_models(req)
        .await
        .expect("anthropic discovery succeeds");
    let ids: Vec<&str> = models.iter().map(|model| model.id.as_str()).collect();
    assert_eq!(ids, vec!["claude-opus-4-8", "claude-sonnet-5"]);
    // Capability fields stay None on this shape. The `display_name` the
    // endpoint advertises is captured verbatim as the presentation-only label.
    assert_eq!(models[0].context_window, None);
    assert_eq!(models[0].name.as_deref(), Some("Opus"));
    assert_eq!(models[1].name.as_deref(), Some("Sonnet"));
}

#[tokio::test]
async fn google_list_models_sends_key_query_param_and_filters_non_text() {
    let mut server = Server::new_async().await;
    let _mock = server
        .mock("GET", "/v1beta/models")
        // Google auth: the key is a query param, never a header.
        .match_query(Matcher::UrlEncoded(
            "key".to_string(),
            "gem-key".to_string(),
        ))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            r#"{"models":[
                {"name":"models/gemini-2.5-pro","supportedGenerationMethods":["generateContent"]},
                {"name":"models/text-embedding-004","supportedGenerationMethods":["embedContent"]}
            ]}"#,
        )
        .create_async()
        .await;

    let key = SecretString::from("gem-key");
    let req = RemoteCatalogRequest {
        protocol: CatalogShape::Google,
        base_url: &format!("{}/v1beta", server.url()),
        api_key: &key,
        account_id: None,
        org_id: None,
        user_agent: None,
        extra_headers: &[],
        catalog_signing: None,
        dimensions: &[],
    };
    let models = list_models(req).await.expect("google discovery succeeds");
    // The embedding-only model is filtered out.
    let ids: Vec<&str> = models.iter().map(|model| model.id.as_str()).collect();
    assert_eq!(ids, vec!["gemini-2.5-pro"]);
}

#[tokio::test]
async fn list_models_returns_status_error_on_non_2xx() {
    let mut server = Server::new_async().await;
    let _mock = server
        .mock("GET", "/v1/models")
        .with_status(401)
        .with_body(r#"{"error":"invalid_api_key"}"#)
        .create_async()
        .await;

    let key = SecretString::from("bad");
    let req = RemoteCatalogRequest {
        protocol: CatalogShape::OpenAi,
        base_url: &format!("{}/v1", server.url()),
        api_key: &key,
        account_id: None,
        org_id: None,
        user_agent: None,
        extra_headers: &[],
        catalog_signing: None,
        dimensions: &[],
    };
    match list_models(req).await {
        Err(ModelListError::Status(401, body)) => {
            assert!(
                body.contains("invalid_api_key"),
                "body surfaces in error: {body}"
            );
        }
        other => panic!("expected Status(401), got {other:?}"),
    }
}

#[tokio::test]
async fn list_models_accepts_authoritative_empty_data_array() {
    let mut server = Server::new_async().await;
    let _mock = server
        .mock("GET", "/v1/models")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"data":[]}"#)
        .create_async()
        .await;

    let key = SecretString::from("k");
    let req = RemoteCatalogRequest {
        protocol: CatalogShape::OpenAi,
        base_url: &format!("{}/v1", server.url()),
        api_key: &key,
        account_id: None,
        org_id: None,
        user_agent: None,
        extra_headers: &[],
        catalog_signing: None,
        dimensions: &[],
    };
    assert!(list_models(req).await.unwrap().is_empty());
}

// ═════════════════════════════════════════════════════════════════════════════
// OAuth wire & concurrency validation
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn oauth_token_endpoint_with_empty_access_token_fails() {
    let mut server = Server::new_async().await;
    let _mock = server
        .mock("POST", "/token")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"access_token":"","token_type":"Bearer"}"#)
        .create_async()
        .await;

    let cfg = nuo_providers::oauth::OAuthConfig::builder("test_empty")
        .token_url(format!("{}/token", server.url()))
        .build();

    // The owned transport, matching what the OAuth flows now use.
    let client = nuo_providers::http::Http::control_plane().expect("http client");
    let pkce = nuo_providers::oauth::PkceCodes::generate();
    let res = nuo_providers::oauth::token::exchange_code(
        &client,
        &cfg,
        "test_code",
        &pkce,
        "http://localhost:1234/callback",
    )
    .await;

    assert!(
        matches!(res, Err(nuo_providers::oauth::AuthError::Decode(msg)) if msg.contains("empty access_token")),
        "empty access token must fail decode validation"
    );
}

#[tokio::test]
async fn oauth_browser_login_validates_oidc_nonce() {
    use base64::Engine;
    let mut server = Server::new_async().await;
    // The owned transport, matching what the OAuth flows now use.
    let client = nuo_providers::http::Http::control_plane().expect("http client");

    // 1. Correct nonce succeeds
    let cfg_good = nuo_providers::oauth::OAuthConfig::builder("test_oidc_good")
        .token_url(format!("{}/token_good", server.url()))
        .send_nonce(true)
        .build();
    let login_good = nuo_providers::oauth::OAuth::new(cfg_good, nuo_providers::CredentialHost::none())
        .begin_browser_login()
        .await
        .unwrap();

    let good_claims = serde_json::json!({
        "sub": "user_123",
        "nonce": login_good.nonce,
        "exp": 2_000_000_000
    });
    let good_payload =
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(good_claims.to_string().as_bytes());
    let good_id_token = format!("eyJhbGciOiJub25lIn0.{good_payload}.sig");

    let _mock_good = server
        .mock("POST", "/token_good")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(format!(
            r#"{{"access_token":"valid_tok","id_token":"{good_id_token}","token_type":"Bearer"}}"#
        ))
        .create_async()
        .await;

    let expected_state = login_good.state.clone();
    login_good
        .inject_manual_input(&format!("code=mycode&state={expected_state}"))
        .unwrap();
    let tok_good = login_good.complete(&client).await;
    assert!(tok_good.is_ok());

    // 2. Mismatched nonce fails
    let cfg_bad = nuo_providers::oauth::OAuthConfig::builder("test_oidc_bad")
        .token_url(format!("{}/token_bad", server.url()))
        .send_nonce(true)
        .build();
    let login_bad = nuo_providers::oauth::OAuth::new(cfg_bad, nuo_providers::CredentialHost::none())
        .begin_browser_login()
        .await
        .unwrap();

    let bad_claims = serde_json::json!({
        "sub": "user_123",
        "nonce": "mismatched_nonce_from_attacker",
        "exp": 2_000_000_000
    });
    let bad_payload =
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bad_claims.to_string().as_bytes());
    let bad_id_token = format!("eyJhbGciOiJub25lIn0.{bad_payload}.sig");

    let _mock_bad = server
        .mock("POST", "/token_bad")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(format!(
            r#"{{"access_token":"valid_tok","id_token":"{bad_id_token}","token_type":"Bearer"}}"#
        ))
        .create_async()
        .await;

    let expected_state = login_bad.state.clone();
    login_bad
        .inject_manual_input(&format!("code=mycode&state={expected_state}"))
        .unwrap();
    let tok_bad = login_bad.complete(&client).await;
    assert!(
        matches!(tok_bad, Err(nuo_providers::oauth::AuthError::Authorization(msg)) if msg.contains("OIDC nonce mismatch"))
    );
}

#[tokio::test]
async fn opencode_go_wire_request_carries_session_and_client_headers() {
    let mut server = Server::new_async().await;
    let url = format!("{}/inference/openai/v1/chat/completions", server.url());

    let _mock = server
        .mock("POST", "/inference/openai/v1/chat/completions")
        .match_header("x-opencode-session", "ses_wire_affinity_999")
        .match_header("x-opencode-client", "cli")
        .match_header(
            "user-agent",
            nuo_model_codec::client_identity::OPENCODE_USER_AGENT,
        )
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"choices":[{"message":{"content":"response from opencode-go"}}]}"#)
        .create_async()
        .await;

    let channel = Channel {
        id: "glm-5.2".into(),
        label: "GLM-5.2".into(),
        transport: Transport::OpenAi {
            base_url: url,
            client_profile: nuo_model_codec::ClientProfile::OpenCode,
            effort: None,
            dialect: Default::default(),
        },
        credentials: nuo_model_codec::static_credential("opencode-token"),
        model: "glm-5.2".into(),
        remote: None,
        user_overrides: None,
        prompt_cache: nuo_model_codec::PromptCacheCapabilities::unsupported(),
        prompt_cache_preference: Default::default(),
    };

    let provider =
        build_provider_for_channel(&channel, "opencode-go", Some("ses_wire_affinity_999"));
    let msg = provider
        .chat(vec![Message::new(Role::User, "hello")].into())
        .await
        .expect("opencode-go provider chat must succeed")
        .message;
    assert_eq!(msg.content, "response from opencode-go");
}

#[tokio::test]
async fn opencode_go_anthropic_wire_request_carries_session_headers() {
    let mut server = Server::new_async().await;
    let url = format!("{}/inference/anthropic/v1/messages", server.url());

    let _mock = server
        .mock("POST", "/inference/anthropic/v1/messages")
        .match_header("x-opencode-session", "ses_anthropic_wire_777")
        .match_header("x-opencode-client", "cli")
        .match_header(
            "user-agent",
            nuo_model_codec::client_identity::OPENCODE_USER_AGENT,
        )
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"content":[{"type":"text","text":"qwen3.6-plus response"}]}"#)
        .create_async()
        .await;

    let channel = Channel {
        id: "qwen3.6-plus".into(),
        label: "Qwen3.6 Plus".into(),
        transport: Transport::Anthropic {
            base_url: url,
            client_profile: nuo_model_codec::ClientProfile::OpenCode,
            effort: None,
            thinking: None,
            dialect: Default::default(),
        },
        credentials: nuo_model_codec::static_credential("opencode-token"),
        model: "qwen3.6-plus".into(),
        remote: None,
        user_overrides: None,
        prompt_cache: nuo_model_codec::PromptCacheCapabilities::unsupported(),
        prompt_cache_preference: Default::default(),
    };

    let provider =
        build_provider_for_channel(&channel, "opencode-go", Some("ses_anthropic_wire_777"));
    let msg = provider
        .chat(vec![Message::new(Role::User, "hello")].into())
        .await
        .expect("opencode-go anthropic-wire provider chat must succeed")
        .message;
    assert_eq!(msg.content, "qwen3.6-plus response");
}

#[tokio::test]
async fn opencode_device_flow_posts_json_and_returns_tokens() {
    let mut server = Server::new_async().await;
    let client = nuo_providers::http::Http::control_plane().expect("http client");

    let mut cfg = nuo_providers::oauth::opencode_preset();
    cfg.device_authorization_url = format!("{}/auth/device/code", server.url()).into();
    cfg.device_token_url = format!("{}/auth/device/token", server.url()).into();
    cfg.token_url = cfg.device_token_url.clone();

    let code_mock = server
        .mock("POST", "/auth/device/code")
        .match_header("content-type", "application/json")
        .match_body(Matcher::Json(serde_json::json!({
            "client_id": "opencode-cli",
        })))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            r#"{"device_code":"dev-1","user_code":"ABCD-1234","verification_uri_complete":"https://opencode.ai/console/device?code=ABCD-1234","expires_in":900,"interval":1}"#,
        )
        .create_async()
        .await;

    let device = nuo_providers::oauth::request_opencode_device_code(&client, &cfg)
        .await
        .expect("device code");
    assert_eq!(device.user_code, "ABCD-1234");
    assert_eq!(
        device.user_url(&cfg),
        "https://opencode.ai/console/device?code=ABCD-1234"
    );
    code_mock.assert_async().await;

    let token_mock = server
        .mock("POST", "/auth/device/token")
        .match_header("content-type", "application/json")
        .match_body(Matcher::Json(serde_json::json!({
            "grant_type": "urn:ietf:params:oauth:grant-type:device_code",
            "device_code": "dev-1",
            "client_id": "opencode-cli",
        })))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            r#"{"access_token":"console-token","refresh_token":"console-refresh","token_type":"Bearer","expires_in":3600}"#,
        )
        .create_async()
        .await;

    let tokens = nuo_providers::oauth::poll_opencode_device_code_with(
        &client,
        &cfg,
        &device,
        |_ms| async {},
        || 0,
    )
    .await
    .expect("polled tokens");
    assert_eq!(tokens.access_token.expose_secret(), "console-token");
    assert_eq!(
        tokens.refresh_token.as_ref().map(|t| t.expose_secret()),
        Some("console-refresh")
    );
    token_mock.assert_async().await;
}

#[tokio::test]
async fn opencode_refresh_posts_json_refresh_token() {
    let mut server = Server::new_async().await;
    let client = nuo_providers::http::Http::control_plane().expect("http client");

    let mut cfg = nuo_providers::oauth::opencode_preset();
    cfg.token_url = format!("{}/auth/device/token", server.url()).into();
    cfg.device_token_url = cfg.token_url.clone();

    let refresh_mock = server
        .mock("POST", "/auth/device/token")
        .match_header("content-type", "application/json")
        .match_body(Matcher::Json(serde_json::json!({
            "grant_type": "refresh_token",
            "refresh_token": "console-refresh",
            "client_id": "opencode-cli",
        })))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            r#"{"access_token":"rotated","refresh_token":"console-refresh-2","token_type":"Bearer","expires_in":3600}"#,
        )
        .create_async()
        .await;

    let refreshed = nuo_providers::oauth::refresh_access_token(&client, &cfg, "console-refresh")
        .await
        .expect("refreshed token");
    assert_eq!(refreshed.access_token.expose_secret(), "rotated");
    refresh_mock.assert_async().await;
}
