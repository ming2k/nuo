---
id: ADR-0224
title: "Bounded Reachability Fast-Fail, Transport Keepalive, Pool Flush, and Owned Client WebSocket Liveness"
status: accepted
date: 2026-11-19
scope: protocol/error, provider/transport, engine/orchestration, engine/subagent, tool-web, nuo/transport, nuo/shell
superseded_by: null
negative_knowledge: true
---

# 0224. Bounded Reachability Fast-Fail, Transport Keepalive, Pool Flush, and Owned Client WebSocket Liveness

- Status: Accepted
- Date: 2026-11-19
- Date-Verified: 2026-11-19
- Amends: [ADR-0136](0136-wire-drift-is-a-frame-not-a-disconnect.md)

---

## Context and Problem Statement

Nuo's primary LLM streaming loop already separated silent failures from mid-stream restarts (`stream_restart_max_attempts`), bounded idle streams (`body_idle_timeout = 60s` + `STREAM_IDLE_TIMEOUT = 120s`), and parked durable `/retry` checkpoints (`RetryPoint`). Under real-world network fluctuation and offline conditions, five concrete failure modes remained:

1. **Offline hosts burned the full 30-attempt transient budget.** DNS resolution errors and OS routing unreachability (`ENETUNREACH`, `EHOSTUNREACH`, `ENETDOWN`) arrived as ordinary retryable `ProviderErrorKind::Transport`, causing an offline session (and background subagents) to stall for 3.5–9 minutes before giving up.
2. **Half-open sockets waited passively for 60 s.** Shared `netune` provider clients left TCP keepalive disabled (`keepalive: None`), and retry backoff never flushed stranded sockets from the pool across Wi-Fi/VPN switches.
3. **Client WebSocket transport duplicated a third-party runtime with zero heartbeat.** `crates/nuo` depended directly on `tokio-tungstenite` across four copy-pasted dial sites (`attach`, `remote`, `control`, `monitor`) with no connect timeout and no Ping/Pong liveness, even though `netune::WsSession` was already linked in the workspace.
4. **Remote TUI relink aborted on the first failed re-dial.** `runner::run_attached` treated any transport error from `target_server.connect` as immediately fatal even when `relinking` was active, and applied a ~2.5 s local-restart budget (`10 × 50–500 ms`) to remote WebSocket links.
5. **`tool-web` dropped `[web] timeout_secs` and left body reads unbounded by the request deadline.** `WebHttp::new` discarded `config.timeout_secs` into `_timeout`, `WebHttp::read` and `guarded_get` only wrapped header open (not `next_chunk()`), and idempotent `GET` failed without a single retry on transient 502/503/504 glitches.

## Architectural Decisions

Keep mechanism in `netune` and policy in `nuo`, adding no background OS network-watcher threads, no new wire envelopes, and no speculative state machines:

### Subsystem Boundaries & Structural Changes

1. **Typed reachability error across `protocol` → `provider` → `engine`**:
   - Add `ProviderErrorKind::Unreachable` (`protocol/src/error.rs`) and `TransportError::Unreachable` (`provider/src/transport/error.rs`), populated directly from `netune::NetError::is_unreachable()` without prose parsing.
   - Cap consecutive `Unreachable` attempts at `UNREACHABLE_MAX_ATTEMPTS = 3` in both the principal round loop (`engine/src/orchestration/execute.rs`) and the subagent retry loop (`engine/src/cognition/interop/subagent_tool/tool/run.rs`), parking the existing durable `RetryPoint` on failure.
   - Fix the 1-based attempt off-by-one in `execute.rs` so the stepped backoff schedule (`1s -> 2s -> 5s -> 10s...`) no longer repeats `1s` twice.
2. **Transport keepalive and selective pool flush**:
   - Configure `netune::SocketConfig::with_keepalive(30s)` on Unix in `provider/src/transport/network.rs`.
   - Expose `provider::transport::clear_shared_pools()` over `netune::Client::clear_pool()` and invoke it before retry backoff only for connection-level failures (`Transport | Unreachable | Timeout`, via `engine::runtime::retry::should_flush_pools`), preserving healthy TLS/HTTP2 sockets across HTTP 429/5xx retries.
3. **Break clean on client WebSocket transport (`crates/nuo`)**:
   - Remove `tokio-tungstenite` from `crates/nuo/Cargo.toml`.
   - Consolidate all client WebSocket dials (`attach.rs`, `remote.rs`, `control.rs`, `monitor.rs`) onto one `connect_ws_wire` helper in `crates/nuo/src/transport/mod.rs` backed by `netune::Client::connect_ws_with` (10 s connect & upgrade timeouts, 30 s TCP keepalive) and `netune::WsSession` (20 s Ping / 10 s Pong liveness on long-lived links; disabled on one-shot control verbs).
4. **Split local vs remote relink policy (`crates/nuo/src/shell/runner.rs`, `crates/nuo/src/transport/attach.rs`)**:
   - Keep `relinking` active across failed transport re-dials (`AttachError::is_transport_failure()`) so a multi-second network drop no longer terminates the TUI on attempt 1, while explicit server `Wire::Error` refusals and initial launch failures still fail loudly, and reset `relinking = false` immediately once re-attachment succeeds.
   - Apply distinct backoff budgets: local (`10` attempts, `0..500 ms`) for daemon replacement vs remote (`20` attempts, stepped `500 ms -> 10 s` with equal jitter), reset after 5 s of stable attachment, and cancellable immediately via `Esc` or `Ctrl+C` across dial, backoff sleep, and local daemon re-resolution.
5. **Bounded `tool-web` deadline, pool flush, keepalive, and single idempotent `GET` retry**:
   - Store and honor `WebConfig::timeout_secs` on `WebHttp` (with 30 s Unix TCP keepalive), span one `tokio::time::Instant` deadline across both `open_until_classified()` and body streaming (`next_chunk()`), flush the `WebHttp` pool before retry backoff, and retry `guarded_get` once after `500 ms` only on transient transport errors (`is_retryable() && !is_unreachable()`) or HTTP `502 / 503 / 504` (never retrying SSRF refusals, offline unreachability, or 4xx).
   - Configure the same 30 s Unix TCP keepalive on the Streamable HTTP MCP client (`server/src/runtime/mcp/client.rs`).

### Normative Invariants Established

- `[INV-RESILIENCE-01]` **Offline Reachability Fast-Fail**: Consecutive `ProviderErrorKind::Unreachable` failures are bounded by `UNREACHABLE_MAX_ATTEMPTS` (3) in both principal rounds and subagent loops, parking a durable `RetryPoint` instead of consuming the 30-attempt transient upstream budget. *Enforced by*: `engine::orchestration::tests::unreachable_errors_fast_fail_and_park_retry_point`, `engine::cognition::interop::subagent_tool::tool::tests::subagent_fast_fails_unreachable_at_short_budget`, and `engine::runtime::retry::tests::unreachable_budget_and_pool_flush_policy`.
- `[INV-RESILIENCE-02]` **Selective Pool Flush & Keepalive**: Shared provider pools flush before retry backoff on `Transport | Unreachable | Timeout` (never on healthy-socket HTTP `RateLimited | Upstream`), and Unix provider clients configure `TCP_KEEPALIVE_IDLE` (30 s). *Enforced by*: `provider::transport::network::tests::shared_client_is_a_singleton` and `engine::runtime::retry::tests::unreachable_budget_and_pool_flush_policy`.
- `[INV-RESILIENCE-03]` **Owned Client WebSocket Substrate & Server Pong Liveness**: `crates/nuo` links `netune` (never `tokio-tungstenite`) for all client WebSocket channels, with bounded connect/upgrade timeouts and `WsSession` Ping/Pong liveness on long-lived streams; server `run_monitor` polls `wire_source` so idle WebSocket monitor streams flush Pong replies promptly. *Enforced by*: `nuo::transport::tests::netune_ws_session_wire_roundtrip_and_liveness`, `nuo::transport::tests::connect_ws_wire_survives_idle_monitor_heartbeats_against_real_server`, and `crates/nuo/Cargo.toml`.
- `[INV-RESILIENCE-04]` **Remote Relink Tolerance**: Local and remote attach relink policies use distinct bounded backoff schedules, transport re-dial failures (`AttachError::is_transport_failure()`) during an active relink step through the budget instead of terminating on attempt 1 while explicit server `Wire::Error` refusals fail immediately, and both backoff waits and stalled re-dials remain cancellable via `Esc`/`Ctrl+C` with live terminal status feedback. *Enforced by*: `nuo::shell::runner::tests::relink_backoff_separates_local_and_remote_policies` and `nuo::transport::tests::attach_error_distinguishes_transport_failures_from_uncoded_server_refusals`.

## Consequences & Trade-offs

- **Positive**: Eliminates 3.5–9 minute offline hangs, cuts NAT silent-drop detection via OS TCP keepalive, removes `tokio-tungstenite` from `crates/nuo`, gives remote TUI sessions real Ping/Pong liveness and ~2.5 min reconnect tolerance, and closes `tool-web`'s unbounded body-read gap.
- **Negative & Incurred Costs**: Requires `netune v0.1.2` (`ADR-0056` in `netune`) for typed `NetError::is_unreachable()`, `Client::clear_pool()`, and cancel-safe `WsSession` liveness.

## Discarded Alternatives (Negative Knowledge)

### Alternative A: Cross-Platform OS Network-Interface Watchers (Netlink / SystemConfiguration / IP Helper) (Rejected)
- **Why considered**: Could detect Wi-Fi/VPN transitions proactively without waiting for a failed socket.
- **Why rejected**: Introduces three platform-specific native event-loop backends for marginal gain when `netune`'s 20 s idle bound, HTTP/1.1 checkout probe, 30 s TCP keepalive, and selective `clear_shared_pools()` on backoff already discard stranded sockets cleanly.

### Alternative B: Partial-Output Stream Continuation State Machine (Rejected)
- **Why considered**: Avoids discarding long assistant prose if a stream drops near completion.
- **Why rejected**: Synthesizing continuation prompts across non-deterministic models, reasoning blocks, and interleaved tool-call fragments risks duplicate or corrupted output and breaks prompt-cache prefixes. `StreamDiscard` + `stream_restart_max_attempts` + durable `RetryPoint` keeps transcript history deterministic.

### Alternative C: Automatic Multi-Provider Failover & Microservice Circuit Breakers (Rejected)
- **Why considered**: Could route around an unreachable upstream automatically.
- **Why rejected**: Silently switching providers or models violates operator control over billing, data residency, and model behavior in a single-user agent.

### Alternative D: Delta Replay Ring Buffer Over `watermark_seq` & Persistent Client Outbox (Rejected)
- **Why considered**: Avoids re-sending a full `ThreadSnapshot` on brief remote reconnects and buffers prompts typed while offline.
- **Why rejected**: Reconnects are infrequent; `ThreadSnapshot` hydration is already the single idempotent path for attach, lag, and reconnect. Buffering stale intents offline risks executing outdated commands unexpectedly upon reconnect.

## Links

- Related ADRs: [ADR-0136](0136-wire-drift-is-a-frame-not-a-disconnect.md), [ADR-0101](0101-runtime-internal-layering-and-governance-inner-layer.md)
