#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use nuo_model_codec::protocol::{anthropic, google, openai};
use nuo_model_codec::{
    ContentBlock, Endpoint, SseDecoder, StreamAccumulator, WireChunk, WireMessage, WireRequest,
    WireRole, WireTool, WireToolCallChunk,
};
use serde_json::json;

#[test]
fn test_openai_wire_request_and_response() {
    let endpoint = Endpoint::openai("sk-test", "gpt-4o");
    let request = WireRequest {
        messages: vec![
            WireMessage::system("You are a helpful assistant."),
            WireMessage::user("Hello!"),
        ],
        tools: vec![WireTool::new(
            "get_weather",
            "Get weather",
            json!({"type": "object"}),
        )],
        temperature: Some(0.7),
        max_tokens: Some(1000),
        thinking_budget: None,
    };

    let (url, headers, body) = openai::build_request(&endpoint, "sk-test", &request, false);
    assert_eq!(url, "https://api.openai.com/v1/chat/completions");
    assert!(headers.contains_key("authorization"));
    assert_eq!(body["model"], "gpt-4o");
    assert_eq!(body["messages"].as_array().unwrap().len(), 2);
    assert_eq!(body["tools"].as_array().unwrap().len(), 1);

    // Test parsing
    let mock_resp = json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": "Sunny in Seattle",
                "tool_calls": [{
                    "id": "call_123",
                    "type": "function",
                    "function": {
                        "name": "report_weather",
                        "arguments": "{\"temp\": 72}"
                    }
                }]
            }
        }],
        "usage": {
            "prompt_tokens": 15,
            "completion_tokens": 25,
            "total_tokens": 40
        }
    });

    let parsed = openai::parse_response(&mock_resp).unwrap();
    assert_eq!(parsed.content.as_deref(), Some("Sunny in Seattle"));
    assert_eq!(parsed.tool_calls.len(), 1);
    assert_eq!(parsed.tool_calls[0].name, "report_weather");
    assert_eq!(parsed.usage.total_tokens, 40);
}

#[test]
fn test_anthropic_wire_request_and_thinking_response() {
    let endpoint = Endpoint::anthropic("sk-ant-test", "claude-3-7-sonnet-20250219");
    let request = WireRequest {
        messages: vec![
            WireMessage::system("System instructions"),
            WireMessage::user("Complex reasoning question"),
        ],
        tools: Vec::new(),
        temperature: None,
        max_tokens: Some(2048),
        thinking_budget: Some(1024),
    };

    let (url, headers, body) = anthropic::build_request(&endpoint, "sk-ant-test", &request, false);
    assert_eq!(url, "https://api.anthropic.com/v1/messages");
    assert_eq!(headers.get("x-api-key").unwrap(), "sk-ant-test");
    assert_eq!(body["system"][0]["text"], "System instructions");
    assert_eq!(body["thinking"]["type"], "enabled");
    assert_eq!(body["thinking"]["budget_tokens"], 1024);

    let mock_resp = json!({
        "content": [
            {
                "type": "thinking",
                "thinking": "Step 1: analyze premises. Step 2: formulate deduction."
            },
            {
                "type": "text",
                "text": "The final conclusion is 42."
            }
        ],
        "usage": {
            "input_tokens": 30,
            "output_tokens": 80
        }
    });

    let parsed = anthropic::parse_response(&mock_resp).unwrap();
    assert_eq!(
        parsed.content.as_deref(),
        Some("The final conclusion is 42.")
    );
    assert_eq!(
        parsed.thinking.as_deref(),
        Some("Step 1: analyze premises. Step 2: formulate deduction.")
    );
    assert_eq!(parsed.usage.prompt_tokens, 30);
    assert_eq!(parsed.usage.completion_tokens, 80);
}

#[test]
fn test_gemini_wire_request_and_response() {
    let endpoint = Endpoint::google("goog-test", "gemini-2.0-flash");
    let request = WireRequest {
        messages: vec![WireMessage::user("What time is it in Tokyo?")],
        tools: vec![WireTool::new(
            "get_time",
            "Get current time",
            json!({"type": "object"}),
        )],
        temperature: Some(0.2),
        max_tokens: Some(512),
        thinking_budget: None,
    };

    // Non-streaming URL
    let (url, _headers, body) = google::build_request(&endpoint, "goog-test", &request, false);
    assert!(url.contains(":generateContent?key=goog-test"));
    assert_eq!(body["contents"][0]["role"], "user");
    assert_eq!(body["contents"][0]["parts"][0]["text"], "What time is it in Tokyo?");
    assert_eq!(body["tools"][0]["functionDeclarations"][0]["name"], "get_time");
    assert!(body.get("stream").is_none());

    // Streaming URL
    let (stream_url, _, stream_body) = google::build_request(&endpoint, "goog-test", &request, true);
    assert!(stream_url.contains(":streamGenerateContent?alt=sse&key=goog-test"));
    assert!(stream_body.get("stream").is_none());

    let mock_resp = json!({
        "candidates": [{
            "content": {
                "parts": [
                    {
                        "thought": "The user wants current time in Tokyo. Call get_time with city=Tokyo."
                    },
                    {
                        "functionCall": {
                            "name": "get_time",
                            "args": { "city": "Tokyo" }
                        }
                    }
                ],
                "role": "model"
            },
            "finishReason": "STOP"
        }],
        "usageMetadata": {
            "promptTokenCount": 20,
            "candidatesTokenCount": 35,
            "totalTokenCount": 55
        }
    });

    let parsed = google::parse_response(&mock_resp).unwrap();
    assert_eq!(
        parsed.thinking.as_deref(),
        Some("The user wants current time in Tokyo. Call get_time with city=Tokyo.")
    );
    assert_eq!(parsed.tool_calls.len(), 1);
    assert_eq!(parsed.tool_calls[0].name, "get_time");
    assert_eq!(parsed.tool_calls[0].arguments["city"], "Tokyo");
    assert_eq!(parsed.usage.total_tokens, 55);
}

#[test]
fn test_anthropic_prompt_caching_breakpoints() {
    let endpoint = Endpoint::anthropic("sk-test", "claude-3-5-sonnet");
    let mut sys_msg = WireMessage::system("Large cached documentation body");
    sys_msg = sys_msg.with_cache_control(nuo_model_codec::CacheControl::Ephemeral);

    let request = WireRequest {
        messages: vec![sys_msg, WireMessage::user("Query based on docs")],
        tools: Vec::new(),
        temperature: None,
        max_tokens: None,
        thinking_budget: None,
    };

    let (_, _, body) = anthropic::build_request(&endpoint, "sk-test", &request, false);
    assert_eq!(
        body["system"][0]["cache_control"]["type"],
        "ephemeral"
    );
}

#[test]
fn test_canonical_block_multimodal_ir() {
    let msg = WireMessage::new(
        WireRole::User,
        vec![
            ContentBlock::text("Describe this architecture diagram:"),
            ContentBlock::image("image/png", "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg=="),
        ],
    );

    let endpoint = Endpoint::openai("sk-test", "gpt-4o");
    let request = WireRequest {
        messages: vec![msg],
        tools: Vec::new(),
        temperature: None,
        max_tokens: None,
        thinking_budget: None,
    };

    let (_, _, body) = openai::build_request(&endpoint, "sk-test", &request, false);
    let parts = body["messages"][0]["content"].as_array().unwrap();
    assert_eq!(parts.len(), 2);
    assert_eq!(parts[0]["type"], "text");
    assert_eq!(parts[1]["type"], "image_url");
}

#[test]
fn test_stream_accumulator_stitches_parallel_tool_calls_and_partial_json() {
    let mut accumulator = StreamAccumulator::new();

    // Chunk 1: Thinking delta + Content delta + Tool 0 start (name only)
    accumulator.feed(&WireChunk {
        delta_content: Some("I will call two tools in parallel.".into()),
        delta_thinking: Some("Reasoning step 1...".into()),
        delta_tool_calls: vec![WireToolCallChunk {
            index: 0,
            id: Some("call_a".into()),
            name: Some("fetch_user".into()),
            arguments_delta: None,
        }],
        usage: None,
        is_done: false,
    });

    // Chunk 2: Tool 1 start (name only) + Tool 0 arguments delta fragment 1
    accumulator.feed(&WireChunk {
        delta_content: None,
        delta_thinking: None,
        delta_tool_calls: vec![
            WireToolCallChunk {
                index: 0,
                id: None,
                name: None,
                arguments_delta: Some("{\"user_id\":".into()),
            },
            WireToolCallChunk {
                index: 1,
                id: Some("call_b".into()),
                name: Some("fetch_orders".into()),
                arguments_delta: None,
            },
        ],
        usage: None,
        is_done: false,
    });

    // Chunk 3: Tool 0 arguments fragment 2 + Tool 1 arguments fragment
    accumulator.feed(&WireChunk {
        delta_content: None,
        delta_thinking: None,
        delta_tool_calls: vec![
            WireToolCallChunk {
                index: 0,
                id: None,
                name: None,
                arguments_delta: Some(" 42}".into()),
            },
            WireToolCallChunk {
                index: 1,
                id: None,
                name: None,
                arguments_delta: Some("{\"limit\": 10}".into()),
            },
        ],
        usage: Some(nuo_model_codec::WireUsage::new(50, 100)),
        is_done: true,
    });

    let finalized = accumulator.finish();

    assert_eq!(
        finalized.content.as_deref(),
        Some("I will call two tools in parallel.")
    );
    assert_eq!(finalized.thinking.as_deref(), Some("Reasoning step 1..."));
    assert_eq!(finalized.tool_calls.len(), 2);

    assert_eq!(finalized.tool_calls[0].id, "call_a");
    assert_eq!(finalized.tool_calls[0].name, "fetch_user");
    assert_eq!(finalized.tool_calls[0].arguments["user_id"], 42);

    assert_eq!(finalized.tool_calls[1].id, "call_b");
    assert_eq!(finalized.tool_calls[1].name, "fetch_orders");
    assert_eq!(finalized.tool_calls[1].arguments["limit"], 10);

    assert_eq!(finalized.usage.prompt_tokens, 50);
    assert_eq!(finalized.usage.completion_tokens, 100);
}

#[test]
fn test_sse_decoder_conforms_to_comments_and_multiline_data() {
    let mut decoder = SseDecoder::new();
    let endpoint = Endpoint::openai("sk-test", "gpt-4o");

    let sse_stream = b": ping heartbeat\r\n\
data: {\"choices\": [{\"delta\": {\"content\": \"Hello \"}}]}\r\n\r\n\
: keepalive\r\n\
data: {\"choices\": [{\"delta\": {\"content\": \"world!\"}}]}\r\n\r\n\
data: [DONE]\r\n\r\n";

    let chunks = decoder.feed(&endpoint, sse_stream).unwrap();
    assert_eq!(chunks.len(), 3);
    assert_eq!(chunks[0].delta_content.as_deref(), Some("Hello "));
    assert_eq!(chunks[1].delta_content.as_deref(), Some("world!"));
    assert!(chunks[2].is_done);
}
