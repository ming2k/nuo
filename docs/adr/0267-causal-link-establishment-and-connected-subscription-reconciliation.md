---
id: ADR-0267
title: "Causal Link Establishment and Connected Subscription Reconciliation"
status: accepted
date: 2026-11-30
scope: host/nuo
superseded_by: null
amends:
  - "[ADR-0260](0260-server-lifecycle-contract-session-free-handshake-and-unified-readiness-projection.md) (enforces causal ordering of LinkEstablished before translator spawns, gates subscription reconciliation on an active connected link, and eliminates the unread placeholder channel)"
negative_knowledge: true
---

# 0267. Causal Link Establishment and Connected Subscription Reconciliation

- **Status**: Accepted
- **Date**: 2026-11-30
- **Deciders**: Nuo Architecture Working Group
- **Amends**: [ADR-0260](0260-server-lifecycle-contract-session-free-handshake-and-unified-readiness-projection.md)
- **Related**: [ADR-0251](0251-multiplexed-thread-sessions-and-resident-per-thread-workspaces.md), [ADR-0266](0266-observable-migration-progress-watch-only-lifecycle-and-honest-readiness.md)

---

## Context and Problem Statement

After [ADR-0260](0260-server-lifecycle-contract-session-free-handshake-and-unified-readiness-projection.md) decoupled `Wire::Hello -> Wire::ServerHello` from `Wire::Subscribe`, launching `nuo` against a warm server (already in `ServerPhase::Ready`) frequently stalled indefinitely on `preparing · thread store…` until the user manually created or opened a thread.

Tracing the startup sequence exposed three compounding defects:

1. **Translator-Before-Link Race (`crates/nuo/src/shell/supervisor.rs`)**:
   `finish_server_hello` pre-buffered `MonitorEvent::Snapshot(snap)` into `link.monitor_rx` before returning `ServerLink`. `spawn_connection_supervisor` then spawned `spawn_monitor_translator`, `spawn_link_event_translator`, and `spawn_response_translator` *before* emitting `AppMutation::LinkEstablished { req_tx }`. On a multi-threaded Tokio runtime, the monitor translator popped the pre-buffered `Snapshot` (`ServerPhase::Ready`) and enqueued `AppMutation::ServerPhase(Ready)` into `runtime.mutations` **ahead of** `AppMutation::LinkEstablished`.
2. **Unconnected Reconciliation (`crates/nuo/src/shell/event_loop/apply_link.rs`)**:
   `apply_server_phase` called `reconcile_subscriptions(app)`, which checked only `app.session.server_phase.admits_work()` without checking whether `app.link.phase` was `Connected` (or `!app.link.link_down`). When the early `ServerPhase(Ready)` arrived while `App` was still `Connecting`, `reconcile_subscriptions` consumed `pending_initial_subscribe.take()` and called `app.subscribe_thread(...)` on the uninstalled placeholder channel.
3. **Silent Black-Hole Placeholder Channel (`crates/nuo/src/shell/mod.rs`)**:
   `run_supervised_tui` created `let (placeholder_tx, _placeholder_rx) = mpsc::unbounded_channel();`. Because `_placeholder_rx` stayed alive for the entire event loop, `placeholder_tx.send(ClientOutbound::Subscribe { .. })` returned `Ok(())` and silently discarded the initial subscription into an unread queue. When `LinkEstablished` arrived milliseconds later and installed the real `req_tx`, `pending_initial_subscribe` was already `None` and the tab strip was empty — leaving `session_phase` permanently stuck in `ThreadPhase::Preparing { stage: PrepareStage::Thread }`.

---

## Architectural Decisions

We eliminate the race at all three layers so the defect is structurally impossible:

### 1. Causal FIFO Ordering in `ConnectionSupervisor`

`spawn_connection_supervisor` MUST `emit(AppMutation::LinkEstablished { server_id, server_phase, persistence_health, req_tx: link.req_tx }).await` into the FIFO `runtime.mutations` channel **before** spawning `spawn_monitor_translator`, `spawn_link_event_translator`, or `spawn_response_translator`. Because `runtime.mutations` is FIFO, no `MonitorEvent`, `LinkEvent`, or `AgentResponse` from a connection can ever reach `App` before its `req_tx` and `ClientLinkPhase::Connected` state are installed.

### 2. Connected-Link Gate and Confirmation-Gated Reconciliation

`reconcile_subscriptions(app)` MUST require:

1. `!app.link.link_down`,
2. `matches!(app.link.phase, ClientLinkPhase::Connected { .. })`, and
3. `app.session.server_phase.admits_work()`.

Rather than destructively `.take()`-ing `pending_initial_subscribe` on send, `SessionState` tracks `initial_subscribe_in_flight: bool`:

- `reconcile_subscriptions` dispatches `pending_initial_subscribe` only when `!initial_subscribe_in_flight`, setting `initial_subscribe_in_flight = true` only when `app.subscribe_thread` succeeds.
- `pending_initial_subscribe` is cleared only when the server confirms `ThreadSubscribed` / `PickRequired` or issues a terminal refusal.
- `initial_subscribe_in_flight` resets to `false` on `LinkEstablished` (recovering automatically if a server dies or restarts between `Subscribe` and `Subscribed`) and on retryable `ERR_SERVER_NOT_READY` refusals (recovering automatically on the next `ServerPhase(Ready)`).
- Background tab IDs are inserted into `subscribed_threads` only when their subscribe send succeeds.

### 3. Closed Placeholder Channel and Supervised Pre-Connect Guard

`run_supervised_tui` drops the placeholder receiver immediately (`let (placeholder_tx, _) = mpsc::unbounded_channel();`), ensuring no pre-connect send can ever vanish into an unread buffer or stall a `oneshot::Receiver` waiting on a control reply. In `App::send_outbound`, when `self.link.supervised` is true and `self.link.phase` is not `Connected`, outbound frames return `false` cleanly without falsely latching `link_down` or emitting a spurious `LinkSendFailed` notice.

### 4. Invariant

- **`[INV-SRVPHASE-09]` Causal Link Establishment and Connected Subscription Reconciliation**: `ConnectionSupervisor` MUST enqueue `AppMutation::LinkEstablished` before spawning any connection translator; `reconcile_subscriptions` MUST execute only while the transport link is `Connected` and `!link_down` (restoring `pending_initial_subscribe` on send failure); and supervised boots MUST NOT retain a live receiver on a placeholder outbox channel.

---

## Consequences & Trade-offs

- **Positive**: Completely eliminates the warm-server startup deadlock (`preparing · thread store…`), guarantees pre-connect control/intent sends never silently disappear into an unread channel, and preserves `pending_initial_subscribe` across mid-flush transport drops.
- **Negative & Incurred Costs**: None; zero wire changes (`PROTOCOL_VERSION` unchanged) and zero runtime overhead.

---

## Negative Knowledge & Rejected Alternatives

### Rejected Alternative A: Reordering Only `supervisor.rs` Without Guarding `reconcile_subscriptions`
Fixing only the spawn order in `supervisor.rs` would still allow a future caller or out-of-band `ServerPhase` mutation during `Reconnecting` to consume `pending_initial_subscribe` against a dead channel. Enforcing the invariant simultaneously at the producer (FIFO ordering), state machine (`Connected && !link_down` gate + lossless restore), and channel (dropped placeholder receiver) makes the bug class unrepresentable.

### Rejected Alternative B: Keeping a Live Placeholder Channel and Draining It on `LinkEstablished`
Buffering arbitrary pre-connect frames in a placeholder channel and replaying them after `LinkEstablished` risks replaying stale or duplicate `Subscribe` / `Control` requests across server restarts and violates [ADR-0260](0260-server-lifecycle-contract-session-free-handshake-and-unified-readiness-projection.md)'s declarative reconciliation model. `pending_initial_subscribe` is already the single declarative carrier for deferred startup attachment.
