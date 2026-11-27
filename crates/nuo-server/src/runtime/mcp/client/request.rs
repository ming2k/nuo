//! JSON-RPC request/notification plumbing for an MCP client.
//!
//! Kept beside the connection type so the stdio reader can route responses by
//! id and progress by token without holding the transport mutex for the whole
//! request.

use super::*;

impl McpClient {
    pub(super) async fn request(&self, method: &str, params: Value) -> Result<Value, McpError> {
        self.request_with(method, params, None, None, None).await
    }

    /// One request with optional cancellation, progress, and request-id
    /// observation.
    ///
    /// `request_id` is stored as soon as the id is allocated. A caller that
    /// spawns a cancellation watcher can therefore send
    /// `notifications/cancelled` even when this future is dropped by a timeout
    /// before the transport read returns.
    pub(super) async fn request_with(
        &self,
        method: &str,
        mut params: Value,
        cancel: Option<&CancellationToken>,
        on_progress: Option<&mut (dyn FnMut(nuo_tool::ToolProgress) + Send)>,
        request_id: Option<&Arc<AtomicU64>>,
    ) -> Result<Value, McpError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        if let Some(slot) = request_id {
            slot.store(id, Ordering::Relaxed);
        }
        let progress_token = on_progress.as_ref().map(|_| format!("nuo-{id}"));

        // The 2026-07-28 revision carries the protocol version, client
        // identity, and capabilities on every request via `params._meta`
        // under reserved `io.modelcontextprotocol/*` keys (SEP-2575); older
        // revisions send them once, in `initialize`.
        let mut meta = params
            .get("_meta")
            .cloned()
            .filter(Value::is_object)
            .unwrap_or_else(|| json!({}));
        if matches!(*self.dialect.read().await, McpDialect::Stateless) {
            meta["io.modelcontextprotocol/protocolVersion"] = json!(MCP_STATELESS_VERSION);
            meta["io.modelcontextprotocol/clientInfo"] = json!({
                "name": "nuo",
                "version": env!("CARGO_PKG_VERSION")
            });
            meta["io.modelcontextprotocol/clientCapabilities"] = json!({});
        }
        if let Some(token) = &progress_token {
            meta["progressToken"] = json!(token);
        }
        if !meta.as_object().is_some_and(|object| object.is_empty()) {
            if params.is_null() {
                params = json!({});
            }
            if let Some(object) = params.as_object_mut() {
                object.insert("_meta".to_string(), meta);
            }
        }

        let payload = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });

        if let Some(http) = self.http_transport().await {
            return self
                .request_http(method, id, &payload, &http, cancel, on_progress)
                .await;
        }
        self.request_stdio(method, id, &payload, cancel, on_progress)
            .await
    }

    /// Send one Streamable-HTTP request without holding the transport lock.
    ///
    /// `netune::Client` is a cheap clone over a shared pool, so every request
    /// owns an independent client handle; only the MCP session id needs a short
    /// shared lock. The request and its body stream are cancellable and
    /// deadline-bounded just like the stdio path.
    async fn request_http(
        &self,
        method: &str,
        id: u64,
        payload: &Value,
        http: &HttpTransport,
        cancel: Option<&CancellationToken>,
        mut on_progress: Option<&mut (dyn FnMut(nuo_tool::ToolProgress) + Send)>,
    ) -> Result<Value, McpError> {
        let stale_session = http.session();
        match self
            .request_http_once(method, id, payload, http, cancel, on_progress.take())
            .await
        {
            Err(McpError::SessionExpired) => {
                self.reinitialize_http(http, stale_session.as_deref())
                    .await?;
                // The first attempt consumed the progress callback. A stale
                // session is rare and the retried result is authoritative, so
                // the retry deliberately runs without progress rather than
                // inventing a second callback with an incompatible lifetime.
                match self
                    .request_http_once(method, id, payload, http, cancel, None)
                    .await
                {
                    Err(McpError::SessionExpired) => Err(McpError::Protocol(
                        "MCP HTTP session expired again after re-initialization".to_string(),
                    )),
                    other => other,
                }
            }
            other => other,
        }
    }

    /// One HTTP attempt. The caller owns the stale-session retry and the
    /// cross-request initialization lock lives in
    /// [`Self::reinitialize_http`].
    async fn request_http_once(
        &self,
        method: &str,
        id: u64,
        payload: &Value,
        http: &HttpTransport,
        cancel: Option<&CancellationToken>,
        mut on_progress: Option<&mut (dyn FnMut(nuo_tool::ToolProgress) + Send)>,
    ) -> Result<Value, McpError> {
        let session_at_start = http.session();
        let progress_token = on_progress.as_ref().map(|_| format!("nuo-{id}"));
        let (target, path) = netune::Target::from_url(&http.url).map_err(|error| {
            McpError::Transport(format!("invalid MCP url '{}': {error}", http.url))
        })?;
        let mut head = netune::RequestHead::new(netune::Method::POST, path)
            .with_header("accept", "application/json, text/event-stream")
            .with_header("content-type", "application/json");
        if let Some(session) = http.session() {
            head = head.with_header("mcp-session-id", session);
        }
        let body = serde_json::to_vec(payload)
            .map_err(|error| McpError::Protocol(format!("MCP {method} encode: {error}")))?;
        let exchange = http.client.request(&target, head, body);
        let exchange = if let Some(cancel) = cancel {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    self.cancel_http(method, id, "cancelled").await;
                    return Err(McpError::Cancelled);
                }
                result = tokio::time::timeout(MCP_REQUEST_TIMEOUT, exchange) => {
                    match result {
                        Ok(response) => response,
                        Err(_) => {
                            self.cancel_http(method, id, "timeout").await;
                            return Err(McpError::Timeout(format!(
                                "MCP {method} timed out after {}s",
                                MCP_REQUEST_TIMEOUT.as_secs()
                            )));
                        }
                    }
                }
            }
        } else {
            match tokio::time::timeout(MCP_REQUEST_TIMEOUT, exchange).await {
                Ok(response) => response,
                Err(_) => {
                    self.cancel_http(method, id, "timeout").await;
                    return Err(McpError::Timeout(format!(
                        "MCP {method} timed out after {}s",
                        MCP_REQUEST_TIMEOUT.as_secs()
                    )));
                }
            }
        };
        let mut exchange = exchange.map_err(|error| {
            McpError::Transport(format!(
                "MCP {method} HTTP failed: {error} [{} trace {}]",
                error.diagnostic().code,
                error.trace().meta.id
            ))
        })?;
        let status = exchange.head.status;
        if status.as_u16() == 404 && session_at_start.is_some() {
            // MCP Streamable HTTP: a 404 for a session-bearing request means
            // the session is gone. The request was rejected before the server
            // could apply its effect, so one re-initialize + retry is safe.
            return Err(McpError::SessionExpired);
        }
        http.remember_session(
            exchange
                .head
                .headers
                .get("mcp-session-id")
                .and_then(|value| value.to_str().ok()),
        );
        if !status.is_success() {
            let detail = tokio::time::timeout(MCP_REQUEST_TIMEOUT, exchange.body.read_to_end())
                .await
                .ok()
                .and_then(Result::ok)
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                .unwrap_or_default();
            return Err(McpError::Protocol(format!(
                "MCP {method} HTTP {status}: {detail}"
            )));
        }
        let content_type = exchange
            .head
            .headers
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let stream = async {
            if content_type.starts_with("text/event-stream") {
                self.stream_sse(
                    &mut exchange.body,
                    method,
                    progress_token.as_deref(),
                    &mut on_progress,
                    &self.inflight,
                    http,
                )
                .await
            } else {
                let bytes = exchange.body.read_to_end().await.map_err(|error| {
                    McpError::Transport(format!(
                        "MCP {method} body: {error} [{}]",
                        error.diagnostic().code
                    ))
                })?;
                let body = String::from_utf8_lossy(&bytes);
                parse_http_message_with_progress(
                    &body,
                    &content_type,
                    method,
                    progress_token.as_deref(),
                    on_progress.take(),
                )
            }
        };
        tokio::pin!(stream);
        let deadline = tokio::time::sleep(MCP_REQUEST_TIMEOUT);
        tokio::pin!(deadline);
        let response = if let Some(cancel) = cancel {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    self.cancel_http(method, id, "cancelled").await;
                    return Err(McpError::Cancelled);
                }
                _ = &mut deadline => {
                    self.cancel_http(method, id, "timeout").await;
                    return Err(McpError::Timeout(format!(
                        "MCP {method} body timed out after {}s",
                        MCP_REQUEST_TIMEOUT.as_secs()
                    )));
                }
                response = &mut stream => response?,
            }
        } else {
            tokio::select! {
                biased;
                _ = &mut deadline => {
                    self.cancel_http(method, id, "timeout").await;
                    return Err(McpError::Timeout(format!(
                        "MCP {method} body timed out after {}s",
                        MCP_REQUEST_TIMEOUT.as_secs()
                    )));
                }
                response = &mut stream => response?,
            }
        };
        extract_response(method, response)
    }

    /// Re-run the HTTP handshake after a stale-session response, once per
    /// observed session. Concurrent requests that hit the same stale id share
    /// the handshake via `HttpTransport::reinit` and then retry with the new id.
    async fn reinitialize_http(
        &self,
        http: &HttpTransport,
        stale_session: Option<&str>,
    ) -> Result<(), McpError> {
        let _guard = http.reinit.lock().await;
        if http.session().as_deref() != stale_session {
            // Another request already refreshed the session.
            return Ok(());
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let payload = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "initialize",
            "params": {
                "protocolVersion": MCP_PROTOCOL_VERSIONS[0],
                "capabilities": {},
                "clientInfo": {
                    "name": "nuo",
                    "version": env!("CARGO_PKG_VERSION")
                }
            }
        });
        let (target, path) = netune::Target::from_url(&http.url).map_err(|error| {
            McpError::Transport(format!("invalid MCP url '{}': {error}", http.url))
        })?;
        let head = netune::RequestHead::new(netune::Method::POST, path)
            .with_header("accept", "application/json, text/event-stream")
            .with_header("content-type", "application/json");
        let body = serde_json::to_vec(&payload)
            .map_err(|error| McpError::Protocol(format!("MCP initialize encode: {error}")))?;
        let exchange = tokio::time::timeout(
            MCP_REQUEST_TIMEOUT,
            http.client.request(&target, head, body),
        )
        .await
        .map_err(|_| {
            McpError::Timeout(format!(
                "MCP initialize timed out after {}s",
                MCP_REQUEST_TIMEOUT.as_secs()
            ))
        })?
        .map_err(|error| {
            McpError::Transport(format!(
                "MCP initialize HTTP failed: {error} [{} trace {}]",
                error.diagnostic().code,
                error.trace().meta.id
            ))
        })?;
        let mut exchange = exchange;
        let status = exchange.head.status;
        let session = exchange
            .head
            .headers
            .get("mcp-session-id")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        http.set_session(session.as_deref());
        if !status.is_success() {
            let detail = tokio::time::timeout(MCP_REQUEST_TIMEOUT, exchange.body.read_to_end())
                .await
                .ok()
                .and_then(Result::ok)
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                .unwrap_or_default();
            return Err(McpError::Protocol(format!(
                "MCP initialize HTTP {status}: {detail}"
            )));
        }
        let bytes = tokio::time::timeout(MCP_REQUEST_TIMEOUT, exchange.body.read_to_end())
            .await
            .map_err(|_| {
                McpError::Timeout(format!(
                    "MCP initialize body timed out after {}s",
                    MCP_REQUEST_TIMEOUT.as_secs()
                ))
            })?
            .map_err(|error| {
                McpError::Transport(format!(
                    "MCP initialize body: {error} [{}]",
                    error.diagnostic().code
                ))
            })?;
        let response: Value = serde_json::from_slice(&bytes).map_err(|error| {
            McpError::Protocol(format!("MCP initialize invalid JSON body: {error}"))
        })?;
        extract_response("initialize", response)?;
        self.notify_http(
            "notifications/initialized",
            &json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized",
                "params": {}
            }),
            http,
        )
        .await?;
        Ok(())
    }

    /// Best-effort protocol cancellation for an HTTP request. The request body
    /// is a separate HTTP call, so this does not need the transport lock and can
    /// run alongside another request from the same server.
    async fn cancel_http(&self, method: &str, id: u64, reason: &str) {
        let _ = self
            .notify(
                "notifications/cancelled",
                json!({"requestId": id, "reason": reason}),
            )
            .await;
        tracing::debug!(method, id, reason, "sent MCP HTTP cancellation");
    }

    /// Stream an SSE response, forwarding progress frames and returning the
    /// first JSON-RPC reply. Unlike the buffered parser, this does not wait for
    /// the whole body, so a long-running HTTP tool reports progress live.
    async fn stream_sse(
        &self,
        body: &mut netune::Body,
        method: &str,
        progress_token: Option<&str>,
        on_progress: &mut Option<&mut (dyn FnMut(nuo_tool::ToolProgress) + Send)>,
        inflight: &Inflight,
        http: &HttpTransport,
    ) -> Result<Value, McpError> {
        let mut buffer = Vec::new();
        loop {
            let chunk = body.next_chunk().await.map_err(|error| {
                McpError::Transport(format!(
                    "MCP {method} SSE body: {error} [{}]",
                    error.diagnostic().code
                ))
            })?;
            let Some(chunk) = chunk else {
                break;
            };
            buffer.extend_from_slice(&chunk);
            if buffer.len() > super::MAX_MCP_MESSAGE_BYTES {
                return Err(McpError::Protocol(format!(
                    "MCP {method} SSE frame exceeds {} MiB",
                    super::MAX_MCP_MESSAGE_BYTES / (1024 * 1024)
                )));
            }
            while let Some(newline) = buffer.iter().position(|byte| *byte == b'\n') {
                let line = String::from_utf8_lossy(&buffer[..newline])
                    .trim()
                    .to_string();
                buffer.drain(..=newline);
                let Some(data) = line.strip_prefix("data:") else {
                    continue;
                };
                let data = data.trim();
                if data.is_empty() {
                    continue;
                }
                let Ok(value) = serde_json::from_str::<Value>(data) else {
                    continue;
                };
                if let (Some(id), Some(request_method)) =
                    (value.get("id"), value.get("method").and_then(Value::as_str))
                {
                    // A server-to-client request in the SSE stream. Return a
                    // JSON-RPC reply over a separate POST; the tool result
                    // stream itself must keep flowing.
                    let response = server_request_response(id, request_method);
                    let _ = self.post_http_response(http, &response).await;
                    continue;
                }
                if let Some(notification_method) = value.get("method").and_then(Value::as_str) {
                    if notification_method == "notifications/progress" {
                        if let (Some(token), Some(callback)) =
                            (progress_token, on_progress.as_mut())
                            && let Some(tick) = progress_tick(value.get("params"), token)
                        {
                            (**callback)(tick);
                        }
                    } else {
                        inflight
                            .notify(parse_notification(notification_method, value.get("params")));
                    }
                    continue;
                }
                if value.get("result").is_some() || value.get("error").is_some() {
                    return Ok(value);
                }
            }
        }
        Err(McpError::Protocol(format!(
            "MCP {method} SSE ended without a JSON-RPC response"
        )))
    }

    pub(super) async fn notify(&self, method: &str, params: Value) -> Result<(), McpError> {
        let payload = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        if let Some(http) = self.http_transport().await {
            return self.notify_http(method, &payload, &http).await;
        }
        let mut connection = self.transport.lock().await;
        match &mut *connection {
            McpConnection::Stdio(transport) => {
                let mut stdin = transport.stdin.lock().await;
                write_message(&mut stdin, &payload).await.map_err(|msg| {
                    McpError::Transport(format!("MCP {method} notify failed: {msg}"))
                })
            }
            McpConnection::Http(_) => Err(McpError::Transport(
                "MCP notify routed to the wrong transport".to_string(),
            )),
        }
    }

    /// POST a JSON-RPC response for a server-to-client request received on an
    /// SSE stream. Best-effort: a failed reply must not abort the tool call
    /// whose stream delivered the request.
    async fn post_http_response(
        &self,
        http: &HttpTransport,
        response: &Value,
    ) -> Result<(), McpError> {
        let (target, path) = netune::Target::from_url(&http.url).map_err(|error| {
            McpError::Transport(format!("invalid MCP url '{}': {error}", http.url))
        })?;
        let mut head = netune::RequestHead::new(netune::Method::POST, path)
            .with_header("accept", "application/json, text/event-stream")
            .with_header("content-type", "application/json");
        if let Some(session) = http.session() {
            head = head.with_header("mcp-session-id", session);
        }
        let body = serde_json::to_vec(response)
            .map_err(|error| McpError::Protocol(format!("MCP reply encode: {error}")))?;
        let exchange = tokio::time::timeout(
            MCP_REQUEST_TIMEOUT,
            http.client.request(&target, head, body),
        )
        .await
        .map_err(|_| {
            McpError::Timeout(format!(
                "MCP server-request reply timed out after {}s",
                MCP_REQUEST_TIMEOUT.as_secs()
            ))
        })?
        .map_err(|error| {
            McpError::Transport(format!(
                "MCP server-request reply failed: {error} [{}]",
                error.diagnostic().code
            ))
        })?;
        let mut exchange = exchange;
        http.remember_session(
            exchange
                .head
                .headers
                .get("mcp-session-id")
                .and_then(|value| value.to_str().ok()),
        );
        let _ = tokio::time::timeout(MCP_REQUEST_TIMEOUT, exchange.body.read_to_end()).await;
        Ok(())
    }

    /// Send an HTTP notification against a cloned client, without the
    /// transport lock. Notifications have no id and thus no reply to correlate:
    /// spec-compliant servers answer 202 with a possibly empty body.
    async fn notify_http(
        &self,
        method: &str,
        payload: &Value,
        http: &HttpTransport,
    ) -> Result<(), McpError> {
        let (target, path) = netune::Target::from_url(&http.url).map_err(|error| {
            McpError::Transport(format!("invalid MCP url '{}': {error}", http.url))
        })?;
        let mut head = netune::RequestHead::new(netune::Method::POST, path)
            .with_header("accept", "application/json, text/event-stream")
            .with_header("content-type", "application/json");
        if let Some(session) = http.session() {
            head = head.with_header("mcp-session-id", session);
        }
        let body = serde_json::to_vec(payload)
            .map_err(|error| McpError::Protocol(format!("MCP {method} encode: {error}")))?;
        let exchange = tokio::time::timeout(
            MCP_REQUEST_TIMEOUT,
            http.client.request(&target, head, body),
        )
        .await
        .map_err(|_| {
            McpError::Transport(format!(
                "MCP {method} notify timed out after {}s",
                MCP_REQUEST_TIMEOUT.as_secs()
            ))
        })?
        .map_err(|error| {
            McpError::Transport(format!(
                "MCP {method} notify failed: {error} [{} trace {}]",
                error.diagnostic().code,
                error.trace().meta.id
            ))
        })?;
        let mut exchange = exchange;
        http.remember_session(
            exchange
                .head
                .headers
                .get("mcp-session-id")
                .and_then(|value| value.to_str().ok()),
        );
        let _ = tokio::time::timeout(MCP_REQUEST_TIMEOUT, exchange.body.read_to_end()).await;
        Ok(())
    }

    /// Multiplexed stdio request: register a waiter, write, then wait on the
    /// response/progress channels. The reader task owns stdout, so a slow or
    /// cancelled request never holds the transport lock.
    async fn request_stdio(
        &self,
        method: &str,
        id: u64,
        payload: &Value,
        cancel: Option<&CancellationToken>,
        mut on_progress: Option<&mut (dyn FnMut(nuo_tool::ToolProgress) + Send)>,
    ) -> Result<Value, McpError> {
        if !self.inflight.is_alive() {
            return Err(McpError::Transport(
                "MCP stdio reader is closed".to_string(),
            ));
        }
        let (response_tx, mut response_rx) = oneshot::channel();
        let (progress_tx, mut progress_rx) = if on_progress.is_some() {
            let (tx, rx) = mpsc::unbounded_channel();
            (Some(tx), Some(rx))
        } else {
            (None, None)
        };
        if !self.inflight.insert(
            id,
            PendingRequest {
                response: response_tx,
                progress: progress_tx,
            },
        ) {
            return Err(McpError::Transport(
                "MCP stdio reader is closed".to_string(),
            ));
        }

        {
            let mut connection = self.transport.lock().await;
            match &mut *connection {
                McpConnection::Stdio(transport) => {
                    let mut stdin = transport.stdin.lock().await;
                    if let Err(msg) = write_message(&mut stdin, payload).await {
                        self.inflight.remove(id);
                        return Err(McpError::Transport(format!(
                            "MCP {method} write failed: {msg}"
                        )));
                    }
                }
                McpConnection::Http(_) => {
                    self.inflight.remove(id);
                    return Err(McpError::Transport(
                        "MCP request routed to a non-stdio connection".to_string(),
                    ));
                }
            }
        }

        let cancel = cancel.cloned();
        let cancel_fut = async {
            match cancel {
                Some(token) => token.cancelled().await,
                None => std::future::pending().await,
            }
        };
        tokio::pin!(cancel_fut);
        let deadline = tokio::time::sleep(MCP_REQUEST_TIMEOUT);
        tokio::pin!(deadline);
        let mut timed_out = false;
        let result = loop {
            tokio::select! {
                biased;
                _ = &mut cancel_fut => break None,
                _ = &mut deadline => {
                    timed_out = true;
                    break None;
                }
                Some(tick) = async {
                    match progress_rx.as_mut() {
                        Some(receiver) => receiver.recv().await,
                        None => std::future::pending().await,
                    }
                } => {
                    if let Some(callback) = on_progress.as_mut() {
                        (**callback)(tick);
                    }
                }
                response = &mut response_rx => break Some(response),
            }
        };
        // A response and a final progress notification can be readable in the
        // same poll. Drain the progress queue before retiring the waiter so a
        // terminal tick is not lost to select ordering.
        if let Some(receiver) = progress_rx.as_mut() {
            while let Ok(tick) = receiver.try_recv() {
                if let Some(callback) = on_progress.as_mut() {
                    (**callback)(tick);
                }
            }
        }
        self.inflight.remove(id);

        let Some(result) = result else {
            let _ = self
                .notify(
                    "notifications/cancelled",
                    json!({
                        "requestId": id,
                        "reason": if timed_out { "timeout" } else { "cancelled" }
                    }),
                )
                .await;
            return Err(if timed_out {
                McpError::Timeout(format!(
                    "MCP {method} timed out after {}s",
                    MCP_REQUEST_TIMEOUT.as_secs()
                ))
            } else {
                McpError::Cancelled
            });
        };
        let response = result
            .map_err(|_| McpError::Transport("MCP response waiter dropped".to_string()))??;
        extract_response(method, response)
    }
}

/// Pull the JSON-RPC `result` out of a well-formed response. An `error` object
/// is a protocol result, never a reconnect-and-retry trigger.
fn extract_response(method: &str, response: Value) -> Result<Value, McpError> {
    if let Some(error) = response.get("error") {
        return Err(McpError::Protocol(format!("MCP {method} error: {error}")));
    }
    response
        .get("result")
        .cloned()
        .ok_or_else(|| McpError::Protocol(format!("MCP {method} response has no result")))
}
