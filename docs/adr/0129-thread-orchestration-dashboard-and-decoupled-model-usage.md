---
id: ADR-0129
title: "Thread Orchestration Dashboard: Sovereign Fleet Management, Rich Thread Telemetry, and Decoupled Model Usage"
status: accepted
date: 2026-10-30
scope: host/nuo, presentation/nuo-view, presentation/nuo-ui-primitives, protocol/nuo-protocol, server/nuo-server, tui/overlays, tui/event-loop
superseded_by: null
negative_knowledge: true
---

# 0129. Thread Orchestration Dashboard: Sovereign Fleet Management, Rich Thread Telemetry, and Decoupled Model Usage

- Status: Accepted
- Date: 2026-10-30
- Deciders: Core Engineering, Architecture Working Group
- Amends: [ADR-0024](0024-two-row-head-band-session-identity-and-scene-row.md), [ADR-0038](0038-thread-entity-server-ssot-fanout-and-orthogonal-action-lifecycle.md), [ADR-0039](0039-client-tab-workspace-scene-unification-and-reactive-surface-sync.md), [ADR-0041](0041-thread-command-purity-viewport-workspace-separation-and-tab-management-topology.md), [ADR-0116](0116-content-residency-and-surface-layer-topology.md), [ADR-0120](0120-thread-lineage-graph-persisted-fork-provenance-and-browser-grade-history-topology.md), [ADR-0128](0128-thread-browser-order-newest-creation-first-with-cursor-stability.md)

---

## Context and Problem Statement

The session dashboard (`/dashboard`, `C-x d`) originally evolved from an ephemeral host control panel (`/host`) structured around two zones:
1. An oversized console pane occupying 60%+ of the viewport, intended for `@N` directives and receipt streaming, which is mostly empty during normal monitoring.
2. A compact dock of 36-column card tiles at the bottom or left strip, displaying only sequence `#N`, folder basename, uptime, and status.

This legacy topology suffered from three fundamental usability and structural deficiencies:

1. **Information Blindness**: The card tiles completely concealed the authentic semantic thread title (`MonitoredSession.title`), task digest (`SessionDigest`), and real-time execution activity (`current_tool`, progress, turns). When multiple threads operated within the same repository, every card rendered as `#N nuo 14m running`, making fleet supervision impossible without manually selecting and inspecting every single card.
2. **Constrained Fleet Operations**: The dashboard lacked real-time thread filtering, clear prioritization for threads requiring attention (`NeedsApproval`, `NeedsInput`, `Failed`), and an ergonomic table presentation suitable for multi-thread monitoring.
3. **Improper Coupling of Usage Accounting**: Token usage statistics (`/usage`) were treated as an in-thread slash command and invoked through the active thread's session driver (`AgentRequest::QueryUsageStats`). Usage is an installation-wide, cross-session aggregate over the day-partitioned store (`data/usage/`) that reflects provider and model consumption independently of any single thread. Coupling it to a thread session made it inaccessible prior to thread creation, unrefreshable without an active session, and architecturally inverted.

## Decision Drivers

- **Thread-First Fleet Management**: The primary purpose of the dashboard is to track, supervise, and control all threads across the server at a glance.
- **Immediate Thread Telemetry**: Thread identity must lead with authentic titles, mission intent, and active tool progress rather than opaque directory basenames.
- **Direct, Orthogonal Fleet Actions**: Single-stroke controls (`Enter`/`a` attach, `i` interrupt, `k` kill with confirmation, `s` suspend, `n` new, `p` prompt, `/` filter) must execute immediately against the selection.
- **Session-Independent Usage Observability**: Model usage is global telemetry; it must be queryable and viewable independently of any thread lifecycle or session driver.
- **Zero Legacy Burden**: Eliminate obsolete card abstractions and thread-bound usage routes cleanly without transitional deprecation layers.

## Architectural Decisions

### 1. Dashboard Dual-Pane Fleet Orchestrator

The dashboard is refactored into a high-visibility fleet management workstation:
- **Header Fleet Metrics Band**: Displays aggregate counts (Total Threads, Running count with live status indicator, Attention Needed count, Token sum, Uptime) and view toggles (`[1] Threads Fleet`, `[2] Model Usage`).
- **Thread Fleet Explorer Table**: A structured multi-column table displaying status indicators, sequence addresses (`#N`), authentic thread titles, workspace paths, active tools/activities, context token counts, and last updated timestamps.
- **Live Thread Dossier & Inspector**: Deep-dive side pane rendering the cognitive mission, intent, milestone progress, tool activity, context pressure, and lineage back-reference for the focused thread.
- **Integrated Cockpit Console Drawer**: Embedded directive receipt log retaining the `@N` command grammar and server acknowledgments without displacing the thread list.
- **Interactive Live Filter (`/`)**: Real-time interactive substring filtering across thread titles, IDs, statuses, and workspaces.

### 2. Session-Free Model Usage

Model usage statistics are fully decoupled from thread sessions:
- **Control Plane Invariant**: `ControlRequest::GetUsageStats { event_cap }` is added to the server control channel, executed asynchronously on the blocking threadpool and returned as a serialized report over the session-free wire.
- **Protocol Version Transition**: `PROTOCOL_VERSION` and `MIN_PROTOCOL_VERSION` advance to `17` to guarantee client/server compatibility for the new control verb.
- **Independent Observability**: Usage is accessible directly from the dashboard via `u` / tab toggle, via `C-x u` or the command palette, and functions seamlessly even when zero threads exist on the server.

### Normative Invariants Established

- `[INV-DASH-01] Thread-First Topology`: The dashboard primary viewport is dedicated to thread fleet tracking and management. Thread identity is anchored by its authentic title, live runtime progress, and attention state.
- `[INV-DASH-02] Orthogonal Fleet Controls`: Direct, single-stroke lifecycle operations (attach, interrupt, terminate, suspend, prompt, new, filter) act immediately upon the active fleet selection without requiring prior context switches.
- `[INV-USAGE-01] Thread-Decoupled Usage Accounting`: Model usage statistics (daily consumption, spend, provider-model breakdown) are pure application-level statistics that exist and refresh independently of any active thread or attached session driver.

## Consequences

- **Positive**:
  - Operators can identify, track, and manage dozens of concurrent threads without navigating in and out of individual sessions.
  - Authentic thread titles and live tool executions are immediately visible in the fleet table.
  - Model token usage and spend analytics can be inspected cold, before any thread is created or without interrupting an active conversation.
- **Trade-offs**:
  - The dashboard layout requires responsive column allocation to handle terminal widths from 80 columns to ultra-wide displays gracefully.

## Rejected Alternatives

### Alternative A: Retaining Dock Cards with Tooltips
- **Why rejected**: Cards in a grid cannot present structured multi-attribute columns (title, workspace, tool, tokens, status) legibly without constant modal drill-in.

### Alternative B: Streaming Usage Through MonitorEvents
- **Why rejected**: Folding a 400-day historical usage aggregate into every monitor tick creates unnecessary CPU overhead on the critical path of the server event loop. On-demand control-plane retrieval preserves both freshness and performance.
