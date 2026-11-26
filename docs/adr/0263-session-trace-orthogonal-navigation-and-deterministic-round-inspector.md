---
id: ADR-0263
title: "Session Trace Orthogonal Navigation and Deterministic Round Inspector"
status: accepted
date: 2026-11-29
scope: host/nuo, presentation/overlays/telemetry, presentation/surfaces
superseded_by: null
amends:
  - "[ADR-0037](0037-bifurcation-of-session-telemetry-into-session-stats-and-session-trace.md) (replaces overloaded `Enter` drill-down with orthogonal `Enter` container entry and `i` item inspection, introduces the L1 Round Inspector, and fixes multi-turn attempt identity)"
negative_knowledge: true
---

# 0263. Session Trace Orthogonal Navigation and Deterministic Round Inspector

- **Status**: Accepted
- **Date**: 2026-11-29
- **Deciders**: Nuo Architecture Working Group
- **Amends**: [ADR-0037](0037-bifurcation-of-session-telemetry-into-session-stats-and-session-trace.md)
- **Related**: [ADR-0233](0233-a-slash-command-is-never-a-conversation-message.md), [ADR-0253](0253-one-owner-per-field-thread-instance-and-viewport.md)

---

## Context and Problem Statement

Two related needs converged around post-round execution review and `/trace`:

1. **Overloaded Navigation & Missing Round-Level Summary in `/trace`**:
   Under [ADR-0037](0037-bifurcation-of-session-telemetry-into-session-stats-and-session-trace.md), `SessionTraceDialog` overloaded `Enter` with two conflicting meanings:
   - On L1 (`Rounds`), `Enter` opened the child list (`Turns`), leaving **no way to inspect a Round itself** (its total wall-clock timing breakdown, human wait pause, tool activity, or round-level token/cost totals).
   - On L2 (`Turns`), `Enter` abruptly switched meaning to open the read-only `Attempt Inspector`.
   Meanwhile, every sibling browse modal (`Threads`, `Models`, `Connections`) already used `i` to inspect the highlighted item's info sub-view.
2. **Multi-Turn Attempt Identity Collision**:
   `SessionTraceDialog` stored the L3 target as `Option<(u32, u32)>` (`(round, attempt)`) without `turn`. Whenever a round contained multiple ReAct turns where each turn succeeded on `attempt == 1`, selecting Turn #1 vs Turn #2 collided on `attempt == 1` in `build_attempt_inspector_body`.
3. **Discarded Authoritative Round Summaries**:
   `RoundEvent::RoundCompleted(RoundSummary)` already carried exact per-round `duration_ms`, `paused_ms`, and `generation_ms` from the server, yet the TUI response translator discarded `_summary` on arrival.

---

## Decision Drivers

- **Orthogonal Key Semantics**: `Enter` ALWAYS means "enter child container" (and is inert on leaves and inspectors); `i` ALWAYS means "inspect highlighted item"; `Esc` ALWAYS unwinds one layer.
- **Transcript Purity (Occam's Razor)**: Keep the conversation transcript (L0) strictly for real dialogue and tool execution. Do NOT inject an automatic post-round LLM "recap" entry into the transcript or create a redundant `/recap` command when `/trace` already owns per-round execution review.
- **Honest, Zero-LLM Round Recap**: Derive timing breakdown and round activity deterministically from durable telemetry (`TokenSourceReport`), live `RoundSummary` records on `ThreadInstance`, and transcript facts (`TranscriptMessage.round`), never fabricating zeros or burning per-round LLM calls.
- **Impossible-to-Be-Invalid State**: Replace loose `(detail: bool, turn: Option<(u32, u32)>)` flags with a closed `TraceView` enum and `TraceAttemptKey { round, turn, attempt }`.

---

## Architectural Decisions

### 1. Orthogonal Trace Navigation (`[INV-TRACE-02]`)

`SessionTraceDialog` models its active surface as a single closed enum `TraceView`:

```text
  L1 Rounds  ──Enter──▶  L2 Turns
      │                      │
      i                      i
      ▼                      ▼
  L1 RoundInfo           L2 AttemptInfo(TraceAttemptKey { round, turn, attempt })
```

- **`Enter` (`InputAction::TraceEnter`)**:
  - On `TraceView::Rounds`: enters `TraceView::Turns` for the highlighted round (when at least one round exists).
  - On `TraceView::Turns`, `TraceView::RoundInfo`, and `TraceView::AttemptInfo(_)`: inert (attempts are leaves with no child container; inspectors are read-only sheets).
- **`i` (`InputAction::TraceInspect`)**:
  - On `TraceView::Rounds`: opens `TraceView::RoundInfo` for the highlighted round.
  - On `TraceView::Turns`: opens `TraceView::AttemptInfo(TraceAttemptKey { round, turn, attempt })` for the highlighted attempt.
  - Inside either inspector: inert.
- **`Esc` (`pop_sublayer`)**:
  - `AttemptInfo(_)` -> `Turns` (preserving `turn_cursor`)
  - `Turns` -> `Rounds` (preserving `index`)
  - `RoundInfo` -> `Rounds` (preserving `index`)
  - `Rounds` -> dismisses `SessionTrace`.
- **Scroll Isolation**: Inspector views scroll `info_scroll`, leaving list scroll (`scroll`) and selection cursors intact across enter/exit transitions.

- **Multi-Actor Attempt Identity**: `TraceAttemptKey { round, actor_id, turn, attempt }` addresses root and subagent (`subagent:<call_id>`) attempts distinctly, L2 renders subagent rows as `sub #T` (`sub #T.A`), and the Attempt Inspector discloses non-root actors explicitly.
- **Context-Honest Palette & `?` Reference**: `PaletteContext::trace_view` drives `Requires::TraceRounds` (`trace.enter`) and `Requires::TraceList` (`trace.inspect`) so the command palette and `?` key reference advertise only actions valid for the active `TraceView`.

### 2. Durable Round Summaries & Deterministic L1 Round Inspector (`[INV-TRACE-03]`)

- **Durable Journal Persistence + `TokenSourceReport` Hydration**: Completed rounds upsert their `protocol::RoundSummary` into `ThreadData::round_summaries` inside the same atomic terminal `CommitTurn` transaction (`crates/persistence/src/thread/history.rs`), record it in `TokenSourceLedger::record_round_summary`, and restore it on open / session switch via `TokenSourceLedger::restore_round_summaries`. `TokenSourceReport::round_summaries` carries the durable summaries over the existing telemetry wire surface, merged with live `ThreadInstance::round_summaries` on the client so live and reopened/restarted threads render identical wall-clock and human-wait breakdowns.
- **L1 Rounds Table Duration**: Displays authoritative `RoundSummary.duration_ms` (from durable `TokenSourceReport` or live `ThreadInstance`) when available, falling back to observed attempt wall span (`last_finished_at_ms - first_started_at_ms`), then summed model `e2e_duration_ms`.
- **L1 Round Inspector (`overlays::telemetry::round`)**: Renders four objective sections from settled telemetry + `RoundSummary` + `extract_round_transcript_recap(&messages, round_number)`:
  1. **Overview**: prompt preview (collapsed single line), deduplicated model/provider attribution, root turns and subagent call counts (noting retries/non-completed attempts).
  2. **Timing Breakdown**: total duration with explicit basis (`wall-clock` / `observed span` / `model sum`), model request sum + avg TTFT + aggregate Stream TPS, tool execution duration, human wait (`paused_ms` when > 0), and reasoning trace duration.
  3. **Activity**: tool call total with outcome counts (`ok`, `failed`, `denied`, `cancelled`, `interrupted`) and per-tool invocation tallies, or `None (direct model response)`.
  4. **Tokens & Cost**: total, prompt, completion, cache read (+ hit rate), cache write, reasoning tokens, and cost when priced.

---

## Normative Invariants

- **`[INV-TRACE-02]` Orthogonal Session Trace Navigation**: In `DialogKind::SessionTrace`, `Enter` MUST only enter a child list container (`Rounds` -> `Turns`) and MUST remain inert on leaf lists (`Turns`) and read-only inspectors (`RoundInfo`, `AttemptInfo`). `i` MUST open the item inspector for the highlighted row at every list level (`Rounds` -> `RoundInfo`, `Turns` -> `AttemptInfo`). Palette and `?` key-reference availability MUST track `TraceView`, and attempt lookup MUST address `(round, actor_id, turn, attempt)` rather than `(round, attempt)` alone.
- **`[INV-TRACE-03]` Deterministic Round Recap Residency**: Per-round duration breakdowns and execution recaps MUST reside in `/trace`'s L1 Round Inspector, backed by durably persisted `RoundSummary` records hydrated through `TokenSourceReport` together with settled request telemetry and transcript facts — never by injecting an automatic post-round LLM summary entry into the dialogue transcript.

---

## Consequences & Trade-offs

- **Positive**:
  - Aligns `/trace` keybindings with `Threads` / `Models` / `Connections` (`Enter` navigates/activates, `i` inspects).
  - Delivers accurate per-round timing and activity recap with zero extra LLM latency, zero token spend, zero context-window pollution, and zero async transcript reflow.
  - Fixes a latent bug where multi-turn rounds collided on `attempt == 1` in the Attempt Inspector.
- **Negative & Incurred Costs**:
  - Operators accustomed to pressing `Enter` twice in `/trace` (L1 -> L2 -> L3) now press `Enter` to enter L2 and `i` to inspect an attempt (advertised in the L2 footer and dialog key reference).
  - Legacy historical threads recorded before ADR-0263 carry no persisted `RoundSummary` records and gracefully fall back to reconstructed attempt wall span (`observed span`) until new rounds complete.

---

## Rejected Alternatives & Negative Knowledge

### 1. Automatic Post-Round LLM Recap Entry Appended to Transcript
- **Why considered**: Mirroring thread titling by running a detached `HarnessTask` after every `RoundCompleted` to append a "what was done + elapsed time" entry to the transcript.
- **Why rejected**: Redundant with the assistant's own closing reply, risks async viewport jumping and out-of-order appends when the user sends a follow-up immediately, multiplies primary-model cost per round (until lightweight routing lands), and pollutes the L0 transcript with non-dialogue prose (counter to [ADR-0233](0233-a-slash-command-is-never-a-conversation-message.md)).

### 2. Standalone `/recap` Command Separate from `/trace`
- **Why considered**: Keeping transcript clean by offering an on-demand `/recap` command.
- **Why rejected**: Splits per-round timing and execution review across two overlapping surfaces (`/trace` and `/recap`). Consolidating round timing and activity into `/trace`'s L1 Round Inspector provides one authoritative observability surface.
