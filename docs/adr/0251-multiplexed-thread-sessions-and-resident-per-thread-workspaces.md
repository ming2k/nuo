---
id: ADR-0251
title: "Multiplexed Thread Sessions and Resident Per-Thread Workspaces"
status: accepted
date: 2026-11-24
scope: protocol/wire, server/transport, tui/nuo, architecture/tabs
superseded_by: null
negative_knowledge: true
---

# 0251. Multiplexed Thread Sessions and Resident Per-Thread Workspaces

- **Status**: Accepted
- **Date**: 2026-11-24
- **Amends**: [ADR-0091](0091-sovereign-tab-context-isolation-and-tabbar-boundary-purity.md) (supersedes the teardown/reconnect tab-switch lifecycle with resident per-thread workspaces over one multiplexed connection)

---

## Context and Problem Statement

Previously, each client connection was 1:1 bound to a single `HostedThread` on the server, and `App` held a single `TranscriptState` and `SessionState`. Switching between two open thread tabs (`Tab A -> Tab B`) set `should_quit = true`, exited the entire TUI event loop, tore down `App` and the socket, re-dialed the server for `Tab B`, constructed an empty `App`, waited for a full `ThreadSnapshot`, and rebuilt the entire transcript from scratch.

This design caused three systemic defects:

1. **Visual flicker & latency**: Switching tabs blanked the viewport until a full network round-trip delivered `ThreadSnapshot` and re-parsed Markdown.
2. **State destruction**: `HeightCache`, `BlockWrapCache`, `DiffCache`, per-entry disclosure expand/collapse states, `follow_bottom`, subagent zoom stacks, and in-transcript search state were destroyed on every switch.
3. **O(N) rebuild cost on every switch**: Long threads (100+ turns) re-deserialized, re-merged, re-parsed Markdown, and re-laid out the entire conversation every time the user toggled tabs.

---

## Architectural Decisions

### 1. `[INV-MUX-01]` One connection hosts N subscribed threads (Wire Protocol v34)

The wire envelope (`crates/protocol/src/lib.rs`) supports full-duplex thread multiplexing on one persistent connection:

- `Wire::Request { thread_id: Option<String>, request: AgentRequest }` — explicitly addresses the target thread (`None` targets the connection's primary thread for single-thread clients).
- `Wire::Subscribe { action: AttachAction, project: Option<PathBuf> }` — attaches to an existing thread or creates a new thread on the same connection without disturbing existing subscriptions.
- `Wire::Unsubscribe { thread_id: String }` — detaches `thread_id`, stops forwarding its broadcast stream, and releases its `HumanChannel` hold.
- `Wire::Subscribed { thread_id: String, phase: ThreadPhase }` — confirms a `Subscribe` request with the resolved thread ID before hydration (`ThreadSnapshot` + `ThreadPhase::Ready`) follows on the same stream.

`PROTOCOL_VERSION` and `MIN_PROTOCOL_VERSION` advance cleanly to `34`.

### 2. `[INV-MUX-02]` Resident Per-Thread Workspaces (`ThreadWorkspace`)

`App` never exits or tears down on tab switch:

- Every open thread tab has a resident `ThreadWorkspace` owning its live `messages`, `layout_height_cache` (`HeightCache`, `BlockWrapCache`, `DiffCache`), `scroll`, `follow_bottom`, `focus`, `focus_stack`, `selection`, `search`, `command_log`, `diff_viewer`, and per-thread session projection (`provider`, `model`, `round_count`, `phase`, `harness_retry_pending`, `session_phase`, etc.).
- The active thread's workspace lives in `App`'s live slots (`app.transcript` / `app.session`); inactive subscribed threads reside in `app.workspaces: HashMap<String, ThreadWorkspace>`.
- Switching between resident tabs is an **O(1) pointer swap** (`park_active_workspace` + `activate_workspace`) with zero cloning, zero Markdown re-parsing, zero cache eviction, and zero network round-trip.
- Composer drafts continue to be owned by the per-thread parked-draft ledger (ADR-0247 / ADR-0250) and handed off seamlessly on switch.

### 3. `[INV-MUX-03]` Background live updates without foreground interference

The response translator maintains per-thread translation state (`HashMap<String, ThreadTranslatorState>`) and tags every thread-scoped `AppMutation` with `thread_id`. The mutation applier routes foreground mutations to `app.transcript` / `app.session` and background mutations directly to `app.workspaces[thread_id]`. Background streaming rounds therefore advance live in memory and are immediately up to date when the user switches back.

### 4. `[INV-MUX-04]` Single FIFO outbound channel

The client transport exposes a single ordered channel (`ClientOutbound`: `Request`, `Subscribe`, `Unsubscribe`) feeding the wire sink. Because all control and domain frames share one FIFO queue, a `Request` stamped with `thread_id` can never race or reorder against a `Subscribe` or `Unsubscribe`.

---

## Discarded Alternatives (Negative Knowledge)

- **Client-side cache + reconnect on switch.** Still pays socket teardown/handshake latency, drops live background streaming while away, and requires reconciliation against stale snapshots.
- **Dual outbound channels (`req_tx` + `mux_tx`).** Introduces cross-channel reordering between thread subscription changes and thread-scoped requests.
- **Keeping `TabContextState` as a partial shadow copy of scroll/focus.** Duplicated a tiny subset of transcript state while discarding `follow_bottom`, caches, and disclosure state; `ThreadWorkspace` is now the single resident owner for live threads.
