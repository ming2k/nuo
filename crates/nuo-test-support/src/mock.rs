//! A deterministic OpenAI-compatible HTTP/SSE provider.
//!
//! The server understands the two request shapes the runtime sends for a
//! `--headless` turn: the streaming chat-completions call and the non-streaming
//! title-generation call. It records every request so E2E tests can assert on
//! the wire body and headers without reaching a live vendor.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::Result;
use crate::eventually;

/// One request received by the mock provider.
#[derive(Debug, Clone)]
pub struct RecordedRequest {
    pub method: String,
    pub path: String,
    /// Header names are lowercased.
    pub headers: BTreeMap<String, String>,
    pub body: String,
}

impl RecordedRequest {
    /// Parse the body as JSON.
    pub fn json(&self) -> Option<serde_json::Value> {
        serde_json::from_str(&self.body).ok()
    }
}

/// A local, hermetic OpenAI-compatible provider.
pub struct MockProvider {
    address: SocketAddr,
    state: Arc<State>,
    thread: Option<JoinHandle<()>>,
    response_text: String,
}

struct State {
    requests: Mutex<Vec<RecordedRequest>>,
    stop: AtomicBool,
}

impl MockProvider {
    /// Start a provider whose assistant response is `pong`.
    pub fn start() -> Result<Self> {
        Self::with_response("pong")
    }

    /// Start a provider with a custom assistant response.
    pub fn with_response(response_text: impl Into<String>) -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        let response_text = response_text.into();
        let state = Arc::new(State {
            requests: Mutex::new(Vec::new()),
            stop: AtomicBool::new(false),
        });
        let thread_state = state.clone();
        let thread_text = response_text.clone();
        let thread = thread::Builder::new()
            .name("nuo-mock-provider".to_string())
            .spawn(move || serve(listener, thread_state, thread_text))?;
        Ok(Self {
            address,
            state,
            thread: Some(thread),
            response_text,
        })
    }

    /// `http://127.0.0.1:<port>`.
    pub fn base_url(&self) -> String {
        format!("http://{}", self.address)
    }

    /// A copy of every request received so far, in arrival order.
    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.state
            .requests
            .lock()
            .map(|requests| requests.clone())
            .unwrap_or_default()
    }

    /// Number of requests received so far.
    pub fn request_count(&self) -> usize {
        self.state
            .requests
            .lock()
            .map(|requests| requests.len())
            .unwrap_or(0)
    }

    /// Wait until at least `count` requests have arrived.
    pub fn wait_for_requests(
        &self,
        count: usize,
        timeout: Duration,
    ) -> Result<Vec<RecordedRequest>> {
        eventually(timeout, || {
            (self.request_count() >= count).then(|| self.requests())
        })
    }

    /// The configured assistant text.
    pub fn response_text(&self) -> &str {
        &self.response_text
    }
}

impl Drop for MockProvider {
    fn drop(&mut self) {
        self.state.stop.store(true, Ordering::SeqCst);
        // Wake the blocking `accept` so the thread observes `stop`.
        let _ = TcpStream::connect(self.address);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn serve(listener: TcpListener, state: Arc<State>, response_text: String) {
    for stream in listener.incoming() {
        if state.stop.load(Ordering::SeqCst) {
            break;
        }
        let Ok(stream) = stream else {
            continue;
        };
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        let _ = handle_connection(stream, &state, &response_text);
    }
}

fn handle_connection(stream: TcpStream, state: &State, response_text: &str) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);

    let mut request_line = String::new();
    if reader.read_line(&mut request_line)? == 0 {
        return Ok(());
    }
    let mut request_parts = request_line.split_whitespace();
    let method = request_parts.next().unwrap_or_default().to_string();
    let path = request_parts.next().unwrap_or_default().to_string();

    let mut headers = BTreeMap::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }

    let content_length = headers
        .get("content-length")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = vec![0_u8; content_length];
    if content_length > 0 {
        reader.read_exact(&mut body)?;
    }
    let body = String::from_utf8_lossy(&body).into_owned();

    if let Ok(mut requests) = state.requests.lock() {
        requests.push(RecordedRequest {
            method: method.clone(),
            path: path.clone(),
            headers: headers.clone(),
            body: body.clone(),
        });
    }

    let (status, content_type, payload) = response_for(&method, &path, &body, response_text);
    let mut response = format!(
        "HTTP/1.1 {status}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-store\r\n\
         Connection: close\r\n\
         \r\n",
        payload.len()
    )
    .into_bytes();
    response.extend_from_slice(payload.as_bytes());

    let mut stream = stream;
    stream.write_all(&response)?;
    stream.flush()
}

fn response_for(
    method: &str,
    path: &str,
    body: &str,
    response_text: &str,
) -> (&'static str, &'static str, String) {
    if method == "GET" && path.ends_with("/models") {
        let payload = serde_json::json!({
            "object": "list",
            "data": [{"id": "mock-1", "object": "model"}],
        })
        .to_string();
        return ("200 OK", "application/json", payload);
    }

    if method != "POST" {
        return ("404 Not Found", "application/json", "{}".to_string());
    }

    if !path.ends_with("/chat/completions") {
        return ("404 Not Found", "application/json", "{}".to_string());
    }

    let streaming = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|value| value.get("stream").and_then(serde_json::Value::as_bool))
        .unwrap_or(false);

    if streaming {
        let first = serde_json::json!({
            "id": "chatcmpl-nuo-test",
            "object": "chat.completion.chunk",
            "choices": [{
                "index": 0,
                "delta": {"role": "assistant", "content": response_text},
            }],
        })
        .to_string();
        let last = serde_json::json!({
            "id": "chatcmpl-nuo-test",
            "object": "chat.completion.chunk",
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": "stop",
            }],
            "usage": {"prompt_tokens": 5, "completion_tokens": 1, "total_tokens": 6},
        })
        .to_string();
        let payload = format!("data: {first}\n\ndata: {last}\n\ndata: [DONE]\n\n");
        ("200 OK", "text/event-stream", payload)
    } else {
        let payload = serde_json::json!({
            "id": "chatcmpl-nuo-test",
            "object": "chat.completion",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": response_text},
                "finish_reason": "stop",
            }],
            "usage": {"prompt_tokens": 5, "completion_tokens": 1, "total_tokens": 6},
        })
        .to_string();
        ("200 OK", "application/json", payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streaming_request_is_served_and_recorded() {
        let provider = MockProvider::with_response("pong").unwrap();
        let address = provider.base_url().replace("http://", "");
        let mut stream = TcpStream::connect(address).unwrap();
        let body = r#"{"model":"mock-1","stream":true}"#;
        write!(
            stream,
            "POST /v1/chat/completions HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();

        assert!(response.contains("data: [DONE]"));
        assert!(response.contains("pong"));
        let requests = provider.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].path, "/v1/chat/completions");
        assert_eq!(
            requests[0].json().and_then(|json| json["stream"].as_bool()),
            Some(true)
        );
    }

    #[test]
    fn non_streaming_request_gets_one_json_body() {
        let provider = MockProvider::with_response("title").unwrap();
        let address = provider.base_url().replace("http://", "");
        let mut stream = TcpStream::connect(address).unwrap();
        let body = r#"{"model":"mock-1","stream":false}"#;
        write!(
            stream,
            "POST /v1/chat/completions HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();

        assert!(response.starts_with("HTTP/1.1 200 OK"));
        assert!(response.contains("\"content\":\"title\""));
        assert!(!response.contains("[DONE]"));
    }
}
