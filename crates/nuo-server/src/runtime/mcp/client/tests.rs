use super::*;

#[tokio::test]
async fn a_cancelled_tool_call_returns_without_waiting_for_the_server() {
    let script = r#"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  [ -z "$id" ] && continue
  case "$line" in
*'"method":"initialize"'*)
  printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"slow","version":"1"}}}\n' "$id" ;;
*'"method":"tools/call"'*)
  sleep 5
  printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"late"}],"isError":false}}\n' "$id" ;;
  esac
done
"#;
    let config = McpServerConfig {
        command: vec!["sh".to_string(), "-c".to_string(), script.to_string()],
        ..Default::default()
    };
    let client = McpClient::connect(&config).await.expect("mock client");
    let cancel = CancellationToken::new();
    let trigger = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        trigger.cancel();
    });
    let mut progress = |_tick: nuo_tool::ToolProgress| {};
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        client.request_with(
            "tools/call",
            json!({"name": "slow_tool", "arguments": {}}),
            Some(&cancel),
            Some(&mut progress),
            None,
        ),
    )
    .await
    .expect("cancel must release the call promptly");
    assert!(matches!(result, Err(McpError::Cancelled)), "{result:?}");
}

#[tokio::test]
async fn cancelling_one_stdio_request_does_not_kill_a_sibling() {
    let script = r#"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  [ -z "$id" ] && continue
  case "$line" in
    *'"method":"initialize"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"sibling","version":"1"}}}\n' "$id" ;;
    *'"method":"tools/call"'*)
      ( sleep 0.2; printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"ok"}],"isError":false}}\n' "$id" ) &
      ;;
  esac
done
"#;
    let config = McpServerConfig {
        command: vec!["sh".to_string(), "-c".to_string(), script.to_string()],
        ..Default::default()
    };
    let client = McpClient::connect(&config).await.expect("mock client");
    let cancel = CancellationToken::new();
    let trigger = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(30)).await;
        trigger.cancel();
    });
    let (left, right) = tokio::join!(
        client.request_with(
            "tools/call",
            json!({"name": "left", "arguments": {}}),
            Some(&cancel),
            None,
            None,
        ),
        client.request_with(
            "tools/call",
            json!({"name": "right", "arguments": {}}),
            None,
            None,
            None,
        ),
    );
    assert!(matches!(left, Err(McpError::Cancelled)), "{left:?}");
    assert!(
        right.is_ok(),
        "cancelling one request killed its sibling: {right:?}"
    );
}

#[tokio::test]
async fn two_stdio_requests_overlap_instead_of_serializing() {
    let script = r#"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  [ -z "$id" ] && continue
  case "$line" in
    *'"method":"initialize"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"overlap","version":"1"}}}\n' "$id" ;;
    *'"method":"tools/call"'*)
      ( sleep 0.2; printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"ok"}],"isError":false}}\n' "$id" ) &
      ;;
  esac
done
"#;
    let config = McpServerConfig {
        command: vec!["sh".to_string(), "-c".to_string(), script.to_string()],
        ..Default::default()
    };
    let client = McpClient::connect(&config).await.expect("mock client");
    let started = std::time::Instant::now();
    let (left, right) = tokio::join!(
        client.request_with(
            "tools/call",
            json!({"name": "left", "arguments": {}}),
            None,
            None,
            None,
        ),
        client.request_with(
            "tools/call",
            json!({"name": "right", "arguments": {}}),
            None,
            None,
            None,
        ),
    );
    let elapsed = started.elapsed();
    assert!(left.is_ok(), "{left:?}");
    assert!(right.is_ok(), "{right:?}");
    assert!(
        elapsed < Duration::from_millis(350),
        "requests serialized instead of overlapping: {elapsed:?}"
    );
}

#[tokio::test]
async fn a_tool_call_forwards_progress_notifications() {
    let script = r#"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  [ -z "$id" ] && continue
  case "$line" in
*'"method":"initialize"'*)
  printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"test","version":"1"}}}\n' "$id" ;;
*'"method":"tools/call"'*)
  token=$(printf '%s' "$line" | sed -n 's/.*"progressToken":"\([^"]*\)".*/\1/p')
  if [ -n "$token" ]; then
    printf '{"jsonrpc":"2.0","method":"notifications/progress","params":{"progressToken":"%s","progress":1,"total":2,"message":"half"}}\n' "$token"
  fi
  printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"done"}],"isError":false}}\n' "$id" ;;
  esac
done
"#;
    let config = McpServerConfig {
        command: vec!["sh".to_string(), "-c".to_string(), script.to_string()],
        ..Default::default()
    };
    let client = McpClient::connect(&config).await.expect("mock client");
    let mut ticks = Vec::new();
    let mut progress = |tick: nuo_tool::ToolProgress| ticks.push(tick);
    let result = client
        .request_with(
            "tools/call",
            json!({"name": "mock_tool", "arguments": {}}),
            Some(&CancellationToken::new()),
            Some(&mut progress),
            None,
        )
        .await
        .expect("mock call");
    assert_eq!(result["content"][0]["text"], "done");
    assert_eq!(ticks.len(), 1);
    assert_eq!(ticks[0].message, "half");
    assert_eq!(ticks[0].current, Some(1));
    assert_eq!(ticks[0].total, Some(2));
}

#[tokio::test]
async fn an_oversized_stdio_line_is_rejected_before_it_is_buffered() {
    let data = vec![b'x'; 1024];
    let mut reader = tokio::io::BufReader::new(std::io::Cursor::new(data));
    let error = read_bounded_line(&mut reader, 100)
        .await
        .expect_err("a line over the cap must fail");
    assert!(error.contains("exceeds"), "{error}");
}

#[test]
fn progress_notifications_match_only_their_request_token() {
    let params = json!({
        "progressToken": "nuo-7",
        "progress": 3,
        "total": 10,
        "message": "indexing"
    });
    let progress = progress_tick(Some(&params), "nuo-7").expect("matching token");
    assert_eq!(progress.message, "indexing");
    assert_eq!(progress.current, Some(3));
    assert_eq!(progress.total, Some(10));
    assert!(progress_tick(Some(&params), "nuo-8").is_none());
}

#[test]
fn sse_progress_is_forwarded_before_the_reply() {
    let body = concat!(
        "event: message\n",
        "data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\",\"params\":{\"progressToken\":\"nuo-1\",\"progress\":1,\"total\":2}}\n\n",
        "event: message\n",
        "data: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"ok\":true}}\n\n"
    );
    let mut ticks = Vec::new();
    let mut on_progress = |progress: nuo_tool::ToolProgress| ticks.push(progress);
    let reply = parse_http_message_with_progress(
        body,
        "text/event-stream",
        "tools/call",
        Some("nuo-1"),
        Some(&mut on_progress),
    )
    .expect("reply");
    assert_eq!(reply["id"], 1);
    assert_eq!(ticks.len(), 1);
    assert_eq!(ticks[0].current, Some(1));
}

#[test]
fn structured_content_is_preserved_as_a_typed_result() {
    let result = json!({
        "content": [{"type": "text", "text": "found 2 rows"}],
        "structuredContent": {"rows": 2}
    });
    match render_tool_result(&result, "test tool") {
        nuo_tool::ToolOutput::Structured {
            content,
            value,
            failed,
        } => {
            assert!(content.starts_with("<untrusted origin=\"mcp\""));
            assert!(content.contains("found 2 rows"));
            assert_eq!(value["rows"], 2);
            assert!(!failed);
        }
        other => panic!("expected Structured, got {other:?}"),
    }
}

#[test]
fn image_content_is_not_flattened_to_text() {
    let result = json!({
        "content": [
            {"type": "text", "text": "screenshot"},
            {"type": "image", "mimeType": "image/png", "data": "aGVsbG8="}
        ]
    });
    match render_tool_result(&result, "test tool") {
        nuo_tool::ToolOutput::Media { parts, .. } => {
            assert_eq!(parts.len(), 2);
            assert!(matches!(
                parts[1],
                nuo_tool::tool_output::MediaPart::Image { .. }
            ));
        }
        other => panic!("expected Media, got {other:?}"),
    }
}

#[test]
fn resource_text_and_blobs_are_preserved() {
    let result = json!({
        "contents": [
            {"uri": "file:///a.txt", "mimeType": "text/plain", "text": "hello"},
            {"uri": "file:///b.png", "mimeType": "image/png", "blob": "aGVsbG8="}
        ]
    });
    match render_resource_result(&result) {
        nuo_tool::ToolOutput::Media { parts, .. } => {
            assert_eq!(parts.len(), 2);
            assert!(matches!(
                parts[0],
                nuo_tool::tool_output::MediaPart::Text { .. }
            ));
            assert!(matches!(
                parts[1],
                nuo_tool::tool_output::MediaPart::Image { .. }
            ));
        }
        other => panic!("expected Media, got {other:?}"),
    }
}

#[test]
fn mcp_is_error_maps_to_a_failed_observation() {
    let result = json!({
        "isError": true,
        "content": [{"type": "text", "text": "boom"}]
    });
    assert!(render_tool_result(&result, "test tool").is_error());
}

#[test]
fn names_are_safe_for_provider_function_schemas() {
    assert_eq!(sanitize_name("git tools/read-file"), "git_tools_read_file");
}

#[test]
fn colliding_public_names_are_disambiguated_deterministically() {
    // `read-file` and `read.file` both sanitize to the same public name;
    // the second (and third) must get a distinct, stable suffix.
    let mut taken = HashSet::new();
    let first = unique_name("mcp__s__read_file".to_string(), "s\0read-file", &mut taken);
    let second = unique_name("mcp__s__read_file".to_string(), "s\0read.file", &mut taken);
    let third = unique_name("mcp__s__read_file".to_string(), "s\0read file", &mut taken);
    assert_eq!(first, "mcp__s__read_file");
    assert!(second.starts_with("mcp__s__read_file_"));
    assert!(third.starts_with("mcp__s__read_file_"));
    assert_ne!(second, third);
    assert!(second.len() <= MCP_MAX_TOOL_NAME);
    // A name that only differs after sanitization keeps its own identity.
    let other = unique_name(
        "mcp__s__write_file".to_string(),
        "s\0write-file",
        &mut taken,
    );
    assert_eq!(other, "mcp__s__write_file");
}

#[test]
fn text_content_is_framed_as_untrusted_data() {
    let result = json!({
        "content": [
            {"type": "text", "text": "first"},
            {"type": "text", "text": "second"}
        ]
    });
    let text = render_tool_result(&result, "test tool").to_text();
    assert_eq!(text.matches("<untrusted origin=\"mcp\"").count(), 2);
    assert!(text.contains("first"));
    assert!(text.contains("second"));
    assert!(!text.contains("jsonrpc"));
}

#[test]
fn compact_mcp_output_preserves_small_outputs() {
    let small = "hello world\nline 2";
    assert_eq!(compact_mcp_output(small), small);
}

#[test]
fn compact_mcp_output_compacts_and_spills_large_byte_payloads() {
    // Output exceeding 16KB
    let large = "a".repeat(20_000);
    let compacted = compact_mcp_output(&large);
    assert!(compacted.contains("[MCP Output truncated:"));
    assert!(compacted.contains("Exceeded 16KB/200-line safety boundary"));
    assert!(compacted.contains("[Full output saved to:"));
}

#[test]
fn compact_mcp_output_compacts_and_spills_large_line_count() {
    // Output exceeding 200 lines
    let lines: Vec<String> = (1..=300).map(|i| format!("line {i}")).collect();
    let large = lines.join("\n");
    let compacted = compact_mcp_output(&large);
    assert!(compacted.contains("[MCP Output truncated: 300 lines"));
    assert!(compacted.contains("--- Output preview (head) ---"));
    assert!(compacted.contains("line 1"));
    assert!(compacted.contains("--- Output preview (tail) ---"));
    assert!(compacted.contains("line 300"));
}

// ── Streamable HTTP ────────────────────────────────────────────────────────

fn recorded_method(recorded: &netune::mock::Recorded) -> String {
    serde_json::from_slice::<Value>(&recorded.body)
        .ok()
        .and_then(|value| {
            value
                .get("method")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_default()
}

fn recorded_id(recorded: &netune::mock::Recorded) -> Value {
    serde_json::from_slice::<Value>(&recorded.body)
        .ok()
        .and_then(|value| value.get("id").cloned())
        .unwrap_or_else(|| json!(0))
}

fn recorded_tool(recorded: &netune::mock::Recorded) -> String {
    serde_json::from_slice::<Value>(&recorded.body)
        .ok()
        .and_then(|value| {
            value
                .get("params")
                .and_then(|params| params.get("name"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_default()
}

fn json_reply_with_session(
    recorded: &netune::mock::Recorded,
    result: Value,
    session: &str,
) -> netune::mock::MockResponse {
    let body = json!({
        "jsonrpc": "2.0",
        "id": recorded_id(recorded),
        "result": result,
    });
    netune::mock::MockResponse::ok(serde_json::to_vec(&body).unwrap())
        .header("content-type", "application/json")
        .header("mcp-session-id", session)
}

fn json_reply(recorded: &netune::mock::Recorded, result: Value) -> netune::mock::MockResponse {
    json_reply_with_session(recorded, result, "session-1")
}

fn static_initialize_reply(session: &str) -> netune::mock::MockResponse {
    netune::mock::MockResponse::ok(
        serde_json::to_vec(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "protocolVersion": "2024-11-05",
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "http-mock", "version": "1"}
            }
        }))
        .unwrap(),
    )
    .header("content-type", "application/json")
    .header("mcp-session-id", session)
}

fn initialize_reply(recorded: &netune::mock::Recorded) -> netune::mock::MockResponse {
    json_reply(
        recorded,
        json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "http-mock", "version": "1"}
        }),
    )
}

#[tokio::test]
async fn two_http_requests_overlap_and_share_the_session() {
    use http::StatusCode;
    use netune::mock::{Matcher, Mock, MockResponse};

    let mock = Mock::new();
    mock.on(Matcher::post("/mcp").when(|r| recorded_method(r) == "initialize"))
        .reply_with(initialize_reply);
    mock.on(Matcher::post("/mcp").when(|r| recorded_method(r) == "notifications/initialized"))
        .reply(MockResponse::new(StatusCode::ACCEPTED));
    mock.on(Matcher::post("/mcp")
        .when(|r| recorded_method(r) == "tools/call" && recorded_tool(r) == "slow"))
        .reply_with(|recorded| {
            json_reply(
                recorded,
                json!({"content": [{"type": "text", "text": "ok"}], "isError": false}),
            )
            .delay_head(Duration::from_millis(200))
        });

    let http = netune::Client::with_transport(mock.clone(), netune::ClientConfig::default());
    let config = McpServerConfig {
        url: Some("http://mock.invalid/mcp".to_string()),
        ..Default::default()
    };
    let client = McpClient::connect_with_http(&config, Some(http))
        .await
        .expect("mock HTTP client");

    let started = std::time::Instant::now();
    let (left, right) = tokio::join!(
        client.request_with(
            "tools/call",
            json!({"name": "slow", "arguments": {}}),
            None,
            None,
            None,
        ),
        client.request_with(
            "tools/call",
            json!({"name": "slow", "arguments": {}}),
            None,
            None,
            None,
        ),
    );
    let elapsed = started.elapsed();
    assert!(left.is_ok(), "{left:?}");
    assert!(right.is_ok(), "{right:?}");
    assert!(
        elapsed < Duration::from_millis(350),
        "HTTP requests serialized instead of overlapping: {elapsed:?}"
    );

    let calls = mock.calls();
    let tool_calls: Vec<_> = calls
        .iter()
        .filter(|call| recorded_method(call) == "tools/call")
        .collect();
    assert_eq!(tool_calls.len(), 2);
    for call in tool_calls {
        assert_eq!(
            call.headers
                .get("mcp-session-id")
                .and_then(|value| value.to_str().ok()),
            Some("session-1"),
            "the session id must be echoed on every request"
        );
    }
}

#[tokio::test]
async fn http_sse_progress_is_delivered_before_the_result() {
    use http::StatusCode;
    use netune::mock::{Matcher, Mock, MockResponse};

    let mock = Mock::new();
    mock.on(Matcher::post("/mcp").when(|r| recorded_method(r) == "initialize"))
        .reply_with(initialize_reply);
    mock.on(Matcher::post("/mcp").when(|r| recorded_method(r) == "notifications/initialized"))
        .reply(MockResponse::new(StatusCode::ACCEPTED));
    mock.on(Matcher::post("/mcp").when(|r| {
        serde_json::from_slice::<Value>(&r.body)
            .ok()
            .is_some_and(|value| {
                value.get("id") == Some(&json!(99)) && value.get("result").is_some()
            })
    }))
    .reply(MockResponse::new(StatusCode::ACCEPTED));
    mock.on(Matcher::post("/mcp")
        .when(|r| recorded_method(r) == "tools/call" && recorded_tool(r) == "stream"))
        .reply_with(|recorded| {
            let id = recorded_id(recorded);
            let token = format!("nuo-{}", id.as_u64().unwrap_or_default());
            let progress = json!({
                "jsonrpc": "2.0",
                "method": "notifications/progress",
                "params": {"progressToken": token, "progress": 1, "total": 2, "message": "half"}
            });
            let result = json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {"content": [{"type": "text", "text": "done"}], "isError": false}
            });
            let ping = json!({"jsonrpc": "2.0", "id": 99, "method": "ping"});
            MockResponse::new(StatusCode::OK)
                .header("content-type", "text/event-stream")
                .header("mcp-session-id", "session-1")
                .chunks([
                    format!("data: {}\n\n", serde_json::to_string(&ping).unwrap()),
                    format!("data: {}\n\n", serde_json::to_string(&progress).unwrap()),
                    format!("data: {}\n\n", serde_json::to_string(&result).unwrap()),
                ])
                .delay_chunks(Duration::from_millis(100))
        });

    let mock_assertions = mock.clone();
    let http = netune::Client::with_transport(mock, netune::ClientConfig::default());
    let config = McpServerConfig {
        url: Some("http://mock.invalid/mcp".to_string()),
        ..Default::default()
    };
    let client = McpClient::connect_with_http(&config, Some(http))
        .await
        .expect("mock HTTP client");

    let mut progress_at = None;
    let mut progress = |tick: nuo_tool::ToolProgress| {
        assert_eq!(tick.message, "half");
        assert_eq!(tick.current, Some(1));
        progress_at = Some(std::time::Instant::now());
    };
    let result = client
        .request_with(
            "tools/call",
            json!({"name": "stream", "arguments": {}}),
            None,
            Some(&mut progress),
            None,
        )
        .await
        .expect("SSE call");
    let finished_at = std::time::Instant::now();
    assert_eq!(result["content"][0]["text"], "done");
    let progress_at = progress_at.expect("progress must arrive");
    assert!(
        progress_at < finished_at,
        "progress must be delivered while the body is still streaming"
    );
    assert!(
        mock_assertions.calls().iter().any(|call| {
            serde_json::from_slice::<Value>(&call.body)
                .ok()
                .is_some_and(|value| {
                    value.get("id") == Some(&json!(99)) && value.get("result").is_some()
                })
        }),
        "the client must answer a server ping with a JSON-RPC result"
    );
}

#[tokio::test]
async fn stdio_notifications_reach_the_sink() {
    let script = r#"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  case "$line" in
*'"method":"initialize"'*)
  printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"notify","version":"1"}}}\n' "$id" ;;
*'"method":"notifications/initialized"'*)
  ( sleep 0.1; printf '{"jsonrpc":"2.0","method":"notifications/message","params":{"level":"info","logger":"notify-test","data":{"hello":"world"}}}\n'; printf '{"jsonrpc":"2.0","method":"notifications/tools/list_changed","params":{}}\n' ) &
  ;;
  esac
done
"#;
    let config = McpServerConfig {
        command: vec!["sh".to_string(), "-c".to_string(), script.to_string()],
        ..Default::default()
    };
    let client = McpClient::connect(&config).await.expect("mock client");
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    client.set_notification_sink(std::sync::Arc::new(move |notification| {
        let _ = tx.send(notification);
    }));

    let mut kinds = Vec::new();
    for _ in 0..2 {
        let notification = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("notification timeout")
            .expect("notification channel closed");
        kinds.push(notification);
    }
    assert!(kinds.iter().any(|notification| matches!(
        notification,
        McpNotification::Logging { level, logger: Some(logger), data }
            if level == "info" && logger == "notify-test" && data["hello"] == "world"
    )));
    assert!(
        kinds
            .iter()
            .any(|notification| matches!(notification, McpNotification::ToolsListChanged))
    );
}

#[tokio::test]
async fn an_expired_http_session_reinitializes_and_retries_once() {
    use http::StatusCode;
    use netune::mock::{Matcher, Mock, MockResponse};

    let mock = Mock::new();
    mock.on(Matcher::post("/mcp").when(|r| recorded_method(r) == "initialize"))
        .reply_once(static_initialize_reply("session-1"))
        .reply_once(static_initialize_reply("session-2"));
    mock.on(Matcher::post("/mcp").when(|r| recorded_method(r) == "notifications/initialized"))
        .reply(MockResponse::new(StatusCode::ACCEPTED));
    mock.on(Matcher::post("/mcp")
        .when(|r| recorded_method(r) == "tools/call" && recorded_tool(r) == "expiring"))
        .reply_once(MockResponse::new(StatusCode::NOT_FOUND))
        .reply_with(|recorded| {
            json_reply(
                recorded,
                json!({"content": [{"type": "text", "text": "after-refresh"}], "isError": false}),
            )
        });

    let http = netune::Client::with_transport(mock.clone(), netune::ClientConfig::default());
    let config = McpServerConfig {
        url: Some("http://mock.invalid/mcp".to_string()),
        ..Default::default()
    };
    let client = McpClient::connect_with_http(&config, Some(http))
        .await
        .expect("mock HTTP client");

    let result = client
        .request_with(
            "tools/call",
            json!({"name": "expiring", "arguments": {}}),
            None,
            None,
            None,
        )
        .await
        .expect("the request must be retried after re-initialization");
    assert_eq!(result["content"][0]["text"], "after-refresh");

    let calls = mock.calls();
    let initializes = calls
        .iter()
        .filter(|call| recorded_method(call) == "initialize")
        .count();
    let tool_calls: Vec<_> = calls
        .iter()
        .filter(|call| recorded_method(call) == "tools/call")
        .collect();
    assert_eq!(initializes, 2, "one handshake plus one session refresh");
    assert_eq!(tool_calls.len(), 2, "the rejected call is retried once");
    assert_eq!(
        tool_calls[0]
            .headers
            .get("mcp-session-id")
            .and_then(|value| value.to_str().ok()),
        Some("session-1")
    );
    assert_eq!(
        tool_calls[1]
            .headers
            .get("mcp-session-id")
            .and_then(|value| value.to_str().ok()),
        Some("session-2")
    );
}

#[test]
fn server_requests_get_a_ping_result_or_a_method_not_found_error() {
    let ping = server_request_response(&json!(7), "ping");
    assert_eq!(ping["id"], 7);
    assert_eq!(ping["result"], json!({}));
    assert!(ping.get("error").is_none());

    let sampling = server_request_response(&json!("req-1"), "sampling/createMessage");
    assert_eq!(sampling["id"], "req-1");
    assert_eq!(sampling["error"]["code"], -32601);
    assert!(sampling.get("result").is_none());
}

#[tokio::test]
async fn stdio_server_requests_receive_a_jsonrpc_reply() {
    let dir = tempfile::tempdir().expect("tempdir");
    let output = dir.path().join("ping-reply.json");
    let script = r#"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  case "$line" in
*'"method":"initialize"'*)
  printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"ping","version":"1"}}}\n' "$id" ;;
*'"method":"notifications/initialized"'*)
  printf '{"jsonrpc":"2.0","id":99,"method":"ping"}\n'
  IFS= read -r reply
  printf '%s' "$reply" > "$NUO_TEST_MCP_OUT"
  ;;
  esac
done
"#;
    let config = McpServerConfig {
        command: vec!["sh".to_string(), "-c".to_string(), script.to_string()],
        environment: std::collections::HashMap::from([(
            "NUO_TEST_MCP_OUT".to_string(),
            output.to_string_lossy().to_string(),
        )]),
        ..Default::default()
    };
    let client = McpClient::connect(&config).await.expect("mock client");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while !output.exists() && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let reply: Value = serde_json::from_str(
        &std::fs::read_to_string(&output).expect("the server must receive a reply"),
    )
    .expect("valid JSON reply");
    assert_eq!(reply["id"], 99);
    assert_eq!(reply["result"], json!({}));
    drop(client);
}

#[test]
fn resource_updated_notifications_carry_the_uri() {
    let notification = parse_notification(
        "notifications/resources/updated",
        Some(&json!({"uri": "file:///workspace/a.rs"})),
    );
    assert!(matches!(
        notification,
        McpNotification::ResourceUpdated { uri } if uri == "file:///workspace/a.rs"
    ));
}
