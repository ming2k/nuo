---
id: ADR-0210
title: "MCP Cancellation and Progress Cross the Tool Boundary"
status: accepted
date: 2026-11-17
scope: server/runtime/mcp, tool/stream, view
superseded_by: null
supersedes: []
negative_knowledge: true
---

# 0210. MCP Cancellation and Progress Cross the Tool Boundary

- Status: Accepted
- Date: 2026-11-17

## Context and Problem Statement

The MCP client treated a request as a blocking pipe operation:

- it held the transport mutex for the whole request;
- the read loop skipped every message whose id was not ours, including
  `notifications/progress`;
- no `progressToken` was sent, so a server had nothing to correlate;
- cancellation dropped the future without sending
  `notifications/cancelled`, so the server could keep running after the user
  stopped or the 120s timeout fired;
- the HTTP path read the full body before parsing SSE, so progress was never
  incremental.

The visible symptoms were a spinner for long tools, cancelled work continuing
on the server, and late responses/queued requests contending with the next call.

## Decision

### 1. One request primitive with cancel/progress/id observation

`McpClient::request_with` replaces the body of `request`:

- generates the JSON-RPC id and stores it in a caller-provided
  `Arc<AtomicU64>` as soon as it is allocated;
- merges `_meta` instead of overwriting it, and adds a `progressToken` when a
  progress callback is present;
- while reading stdio, routes `notifications/progress` to the callback and
  continues until the matching response id;
- selects against the caller's `CancellationToken`; on cancel, sends
  `notifications/cancelled { requestId, reason }` after releasing the transport
  lock and returns `McpError::Cancelled`;
- parses SSE progress frames for HTTP responses.

`request` remains a thin wrapper for callers (catalog discovery, handshake) that
do not need progress or cancellation.

### 2. Background reader and pending map (stdio)

The stdio transport now has one reader task that owns `stdout`; it routes
responses by JSON-RPC id to a `pending: HashMap<id, PendingRequest>` and progress
notifications by token. A request writes to `stdin` under a short lock, then
waits on a oneshot; it never holds the transport lock while waiting.

Consequences:

- two stdio requests to the same server overlap instead of serializing;
- a cancelled or timed-out request removes its pending entry and leaves the
  connection usable;
- a late response is dropped by id;
- a closed reader fails every pending request with a transport error.

### 3. Streamable HTTP also runs requests concurrently

`netune::Client` is `Clone` over a shared pool and `request` takes `&self`; the
bottleneck was nuo's single `Mutex<McpConnection>` held for the whole HTTP
exchange, not the transport. The client now snapshots a cloneable
`HttpTransport` (client + URL + session-id handle) under a short lock, then
runs the request, body stream, and cancellation notification without the
transport lock. Only the MCP session id needs a mutex, and it is never held
across an await. A server can therefore process parallel tool calls; the
session header is echoed and updated on every response.

If a request carrying a session id receives `404 Not Found`, the server has
discarded that session. One request takes `HttpTransport::reinit`, re-runs the
handshake without the stale id, updates the session, sends
`notifications/initialized`, and retries the rejected request once. Concurrent
requests that hit the same stale id observe the refreshed session and retry
without a second handshake. A 404 is rejected before the server applies the
call, so retrying even a non-idempotent tool is safe.

### 4. Non-progress notifications are routed

The stdio reader and the HTTP SSE parser now turn server notifications into
`McpNotification`:

- `notifications/message` maps to `tracing` at the server's declared level and
  never enters the model transcript;
- `tools/list_changed`, `resources/list_changed`, and `prompts/list_changed`
  send the server name to a coalescing channel; the runtime worker reconnects
  that server after a 50 ms quiet period;
- `resources/updated`, `roots/list_changed`, and unknown notifications are
  traced for observability. Resource tools query `resources/list` and
  `resources/read` live, so there is no resource cache to invalidate; the
  notification is diagnostic until a subscription cache exists.

Server-to-client requests (sampling, roots, elicitation) remain ignored because
the client advertises no corresponding capability.

### 5. Server-to-client requests are answered, not ignored

The client advertises no `sampling`, `roots`, or `elicitation` capability, so a
spec-compliant server will not send those requests. A defensive implementation
still answers rather than leaving a peer waiting:

- `ping` returns an empty result;
- every unsupported server request returns JSON-RPC `-32601` with a clear
  message.

Stdio writes the reply through the same shared stdin mutex used by requests;
an SSE stream POSTs the reply to the endpoint as a separate message. Sampling
and elicitation are host capabilities (model invocation and user interaction),
not transport behavior, so they remain explicit unsupported methods until a
host injects a handler.

### 6. A cancellation watcher survives future drop

The dispatcher may drop a tool future on user cancel or timeout. A watcher task
spawned by the MCP tool therefore owns the request-id slot and the cancellation
token independently:

```text
token cancelled ──▶ watcher loads request id ──▶ notifications/cancelled
```

The watcher is stopped by a `done` token on normal completion. This makes
cancel best-effort even when the request future is gone.

### 7. Progress is an ephemeral `ToolStream` frame

`ToolStream::Progress(ToolProgress { message, current, total })` carries a
structured tick. `ToolContext::emit_progress` emits it through the existing
stream sink; the TUI shows it on the running tool card, headless prints it to
stderr, and it is never written to the durable transcript.

### 8. Non-MCP tools can use the same surface

A long-running filesystem/index tool can emit `ToolProgress` without inventing a
second event channel. The transport is already the ephemeral tool stream.

## Consequences

- **Positive**: user cancel and timeout send a protocol-level cancellation;
  long MCP tools show real progress; progress is structured rather than
  text-parsed; the request-id slot makes late responses disposable.
- **Negative & Incurred Costs**: a server that ignores cancellation still runs,
  though the client removes its waiter and no longer blocks on it. Sampling,
  roots, and elicitation are not implemented; unsupported server requests get a
  protocol error because the client advertises no capability. A list_changed
  notification re-lists tools over the existing connection; only a failed live
  request falls back to a reset+reconnect, so the common catalog edit does not
  fail an unrelated in-flight call.
- **Neutral**: protocol errors remain non-retryable; only transport errors keep
  the existing one-shot reconnect.

## Discarded Alternatives

- **Drop the future and assume the server notices**: it does not; this is the
  bug being fixed.
- **Wait for a cancellation acknowledgement**: `notifications/cancelled` has no
  reply in the protocol; blocking would turn a best-effort signal into a hang.
- **Add a second progress event type**: `ToolStream` is already the ephemeral
  per-call channel; one variant is less surface than a parallel event pipeline.
- **Serialize HTTP requests for safety**: rejected because the MCP session id
  is the only shared mutable wire state; `netune::Client` is explicitly a
  cloneable shared-pool client, and requests have independent bodies. Holding
  one mutex for the whole exchange turns independent tool calls into a queue.
- **Advertise sampling/elicitation from the transport**: rejected because the
  transport has neither a model provider nor a UI. Promising a capability the
  host cannot service would turn a protocol error into a hang or a fabricated
  answer. These are injected host capabilities, not MCP client defaults.
- **Advertise an empty roots list**: rejected because an empty root set is not
  the same as no roots capability; a workspace-aware host should supply real
  roots and change notifications.
