---
id: ADR-0037
title: "Bifurcation of Session Telemetry into Dedicated Session Stats and Session Trace Surfaces"
status: accepted
date: 2026-10-16
scope: tui/nuo-tui, presentation/modals, interaction/input, architecture/surfaces
superseded_by: null
negative_knowledge: true
---

# 0037. Bifurcation of Session Telemetry into Dedicated Session Stats and Session Trace Surfaces

- Status: Accepted
- Date: 2026-10-16
- Deciders: Nuo Architecture Working Group
- Consulted: Interface, TUI, Observability, and Interaction Architecture Teams
- Informed: Core Engineering, Developer Experience
- Amends: [ADR-0035](0035-domain-scoped-surface-architecture-and-encapsulated-dialog-lifecycle.md)

---

## Context and Problem Statement

The session telemetry surface was originally conceived to provide both context accounting overview and round/turn diagnostic details. Over iterations, this unified modal accumulated a two-tab structure (`Overview` and `Activity` tabs) with nested hierarchical drill-downs in the Activity tab (L1 Rounds table $\to$ L2 Turns table $\to$ L3 Attempt latency timeline/waterfall).

However, cohabitating these two distinct capabilities within a single tabbed dialog induced severe interaction and architectural anomalies:

1. **Interaction Model Schism**:
   - Tab 1 (`Overview`) is a read-only metric card displaying context window utilization, cumulative prompt/completion tokens, cache hit rates, and aggregate throughput. Its interaction model is strictly vertical document scrolling (`Up`/`Down`).
   - Tab 2 (`Activity`) is an interactive profiler and execution tree. Its interaction model is item selection (`Up`/`Down`), multi-level hierarchical drill-down (`Enter`), and step-back unwinding (`Esc`).
2. **Ambiguous and Conflicting Key Semantics**:
   - Switching tabs was bound to `Left`/`Right` arrow keys and `[`/`]`. In terminal tabular views, horizontal arrow keys conventionally navigate columns or cursor positions; using them for top-level lateral tab swaps caused accidental, disorienting view replacements.
   - In `Overview`, pressing `Enter` arbitrarily switched the active tab to `Activity` (because `Overview` had no selectable items), creating an asymmetric and inconsistent key contract where `Enter` behaved as a tab switcher in one view and a hierarchical drill-down activator in another.
   - When drilled into L2/L3 in Activity, lateral keys could silently mutate background tab states, causing unexpected jumps upon unwinding.
3. **Disparate User Intent and Frequency**:
   - Checking session token burn and context window capacity (`Ctrl+O` from the status bar meter) is a frequent, ambient operational inquiry.
   - Inspecting execution waterfalls, attempt retries, TTFT, and stream TPS is a diagnostic and troubleshooting task conducted when latency anomalies or tool execution issues occur.
4. **Architectural Deviation from ADR-0035**:
   - `ADR-0035` mandates atomic, encapsulated dialog entities with single-purpose ownership. Telemetry stood as the sole dialog violating this principle by embedding an internal multi-tab state engine.

We require an uncompromising, forward-looking architectural refactoring that eliminates legacy tab baggage, establishes distinct surfaces for metrics vs execution tracing, and delivers predictable, idiomatic TUI ergonomics.

---

## Decision Drivers

- **Single Responsibility Surface Contract (`[INV-STATS-01]`, `[INV-TRACE-01]`)**: A surface MUST embody a single coherent interaction model. Metric accounting dashboards MUST NOT be coupled with multi-level drill-down execution profilers.
- **Strict Key Orthogonality**: Eliminate all lateral `Left`/`Right` tab chording within modal dialogs. Arrow keys MUST exclusively serve viewport scrolling or row selection.
- **Semantic Fidelity (`Trace` vs `Telemetry`)**: Align user-facing nomenclature with modern observability standards. The step-by-step waterfall inspection of rounds, turns, attempts, and latency spans is an **Execution Trace**, not an abstract telemetry stream.
- **Frictionless Cross-Surface Affordance (`[INV-AFFORDANCE-01]`)**: Users inspecting `SessionStats` MUST have an instant, zero-friction path to launch `SessionTrace` without leaving their exploratory flow.

---

## Considered Options

### Option 1: Ad-Hoc Key Rebinding (Retain Monolithic Tabbed Modal)
- Keep `Overview` and `Activity` inside `DialogKind::Telemetry`, but remove `Left`/`Right` and restrict tab switching strictly to `Tab` / `Shift-Tab` or `1`/`2`.
- *Assessment*: Rejected. Does not resolve the fundamental cognitive mismatch between a static summary sheet and a 3-level deep execution tree. Keeps the awkward `Enter`-switches-tab quirk in Overview and perpetuates the only tabbed modal in the entire application.

### Option 2: Vertical Stack Layout (Header Summary + Body Rounds Table)
- Merge both views into a single scrollable dialog where the Overview metrics form a sticky header card and the Rounds table sits beneath it.
- *Assessment*: Rejected. Crams excessive vertical density into the terminal viewport, degrading readability on compact displays ($< 35$ rows) and forcing unnecessary visual overhead on users who merely want a quick glance at context token headroom.

### Option 3: Clean Bifurcation into Dedicated `SessionStats` and `SessionTrace` Dialogs (Chosen)
- Formally decouple the monolithic modal into two independent, domain-scoped dialog entities:
  1. `SessionStatsDialog` (`DialogKind::SessionStats`, `/stats`, `Ctrl+O`): Lightweight, scrollable accounting dashboard for context usage and session tokens.
  2. `SessionTraceDialog` (`DialogKind::SessionTrace`, `/trace`): Focused execution profiler with structured L1 $\to$ L2 $\to$ L3 drill-down and latency waterfall inspector.
- Provide a direct affordance chord (`t`) in `SessionStats` that seamlessly transitions the user to `SessionTrace`.

---

## Rejected Alternatives

Per **[INV-AGENT-01]**, the following alternative approaches were evaluated and discarded:

1. **Rejected: Retaining the Name "Telemetry" for the Execution Profiler**
   - *Reason*: In developer tooling and cloud platforms, "telemetry" carries connotations of background analytics, metrics shipping, and privacy tracking. The chronological waterfall of requests, attempts, TTFT, and spans is unequivocally an **Execution Trace** (`Trace`). Retaining "Telemetry" imposes cognitive drag and violates semantic accuracy.
2. **Rejected: Shared Modal State Entity**
   - *Reason*: Storing trace cursor states and stats scroll states in a unified struct violates `[INV-SURFACE-01]` (ADR-0035). Each dialog must encapsulate its own private state struct.
3. **Rejected: Modal-in-Modal Stack Nesting for Transition**
   - *Reason*: Opening `SessionTrace` from `SessionStats` by pushing on top of the overlay stack creates stacking clutter. Transitioning MUST replace the active dialog directly via `open_dialog(SessionTrace)`, cleanly disposing the stats surface.

---

## Decision Outcome

Chosen option: **Option 3**.

### 1. Invariants Defined

- **`[INV-STATS-01] Pure Resource Accounting`**:
  `DialogKind::SessionStats` is dedicated solely to session context window pressure and token accounting (`/stats`, `Ctrl+O`). It is strictly read-only and vertically scrollable (`Up`/`Down`). It possesses zero tab states, zero lateral arrow handlers, and zero drill-down levels.
- **`[INV-TRACE-01] Hierarchical Execution Tracing`**:
  `DialogKind::SessionTrace` is dedicated solely to execution profiling and latency waterfall inspection (`/trace`). It owns a deterministic 3-tier hierarchical drill-down (`L1 Rounds` $\leftrightarrow$ `L2 Turns` $\leftrightarrow$ `L3 Attempt Timeline`). Navigation is strictly vertical (`Up`/`Down` row selection, `Enter` drill-in, `Esc` step-back unwinding). It possesses zero tabs and zero horizontal arrow paging.
- **`[INV-AFFORDANCE-01] Cross-Surface Transition Chord`**:
  `SessionStats` advertises and binds `t` (`SessionStatsOpenTrace`) in its footer legend, directly transitioning the view to `SessionTrace`.

### 2. Dialog Entity Specifications

```rust
dialog_entity!(SessionStatsDialog, SessionStats, {});

dialog_entity!(SessionTraceDialog, SessionTrace, {
    /// `true` when drilled into one round's turns (L2).
    detail: bool = false,
    /// `Some((round, attempt))` when drilled into an attempt inspector (L3).
    turn: Option<(u32, u32)> = None,
    /// Selected turn index in the L2 turns table.
    turn_cursor: usize = 0,
});
```

### 3. Command and Palette Topology

| Dialog | Kind | Title | Slash Command | Shortcut | Scope |
| :--- | :--- | :--- | :--- | :--- | :--- |
| **Session Stats** | `DialogKind::SessionStats` | `Session Stats` | `/stats` | `Ctrl+O` | `DialogScope::Session` |
| **Session Trace** | `DialogKind::SessionTrace` | `Session Trace` | `/trace` | (via stats `t` or switcher) | `DialogScope::Session` |

---

## Consequences

### Positive
- Eradicates all confusing `Left`/`Right` tab transitions and the asymmetric `Enter` jump quirk.
- Aligns terminal interaction ergonomics with native TUI best practices (`Up`/`Down` scroll or select, `Enter` activate, `Esc` dismiss/back).
- Clarifies observability vocabulary: `/stats` for metrics, `/trace` for call execution waterfall.
- Adheres cleanly to ADR-0035 encapsulated surface architecture without residual technical debt.

### Negative
- Deprecates `/telemetry` slash verb in favor of `/trace` (mitigated by retaining `/telemetry` as an alias redirecting to `/trace` if necessary).
