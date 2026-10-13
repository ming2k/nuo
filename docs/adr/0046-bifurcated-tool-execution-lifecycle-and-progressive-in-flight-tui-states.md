---
id: ADR-0046
title: "Autonomous Tool Execution Slot, Fault-Tolerant Streaming Projection, and Bifurcated Lifecycle Architecture"
status: draft
date: 2026-10-23
scope: tui/nuo-tui, presentation/disclosure, protocol/wire, harness/dispatch, interaction/affordance
superseded_by: null
negative_knowledge: true
---

# 0046. Autonomous Tool Execution Slot, Fault-Tolerant Streaming Projection, and Bifurcated Lifecycle Architecture

- Status: Draft
- Date: 2026-10-23
- Deciders: Nuo Architecture Working Group
- Consulted: TUI, Interaction, Harness Runtime, and Protocol Maintainers
- Informed: System Architects
- Complements: [ADR-0008](0008-single-tool-contract.md), [ADR-0020](0020-interactive-component-registry-single-source-of-truth.md), [ADR-0026](0026-streamed-tool-input-progress.md)
- Amends: ADR-0026 §Decision Outcome (surpasses count-only buffering with fault-tolerant streaming projection and bifurcated in-flight execution states)

---

## Context and Problem Statement

In modern Agent architectures, function calling is no longer a passive append-only log entry. It is an **autonomous, interactive execution slot** with distinct temporal phases, causal boundaries, and rendering modalities.

[ADR-0026](0026-streamed-tool-input-progress.md) introduced early announcement (`ToolCallStarted` + `ToolInputProgress`) to inform the user that a tool call was initiated before arguments finished streaming. However, bound by historical conservative constraints, ADR-0026 made a severe compromise: it treated partial streaming arguments purely as unparseable byte counts (`receiving input · X KB`), refusing to expose any semantic content until the entire JSON payload completed.

In practice, this compromise produces significant user-experience and architectural failures:

1. **Semantic Blindness During Ingestion (The "Edit text" Void)**:
   When an LLM streams a large `edit_text` or `write_file` invocation, the target `path` is emitted within the first ~50 bytes, while the `old_string` / `new_string` arguments stream for several seconds or minutes. Because the parser insisted on strict `serde_json::from_str` completion, the presenter fell back to a generic `Edit text · receiving input (4.2 KB)`. The user was kept blind to *which file was being edited* throughout the entire streaming duration.
2. **Monolithic In-Flight Conflation (`Running` Ambiguity)**:
   The lifecycle conflated two physically opposite bottlenecks into a single `ToolStepStatus::Running`:
   - **Phase A (Model Ingestion)**: Controlled by upstream LLM inference latency and token streaming.
   - **Phase B (Local Execution)**: Controlled by local disk I/O, process execution (`execute_command`), AST verification, or external network requests.
   When the JSON stream closed and dispatch occurred, the count indicator abruptly disappeared, leaving the step in a bare, silent `Running` state. If local execution took time (e.g. large file matching or heavy compilation), the UI appeared frozen, leaving the operator unable to distinguish between a hung process and ongoing work.
3. **Blank Expanded Bodies for In-Flight Edits**:
   Because `tools.rs` only drew results if `has_output || is_command || has_structured`, an expanded `edit_text` step remained completely empty until the tool finished writing to disk. Even after arguments were 100% available and verified, no diff preview was rendered during the dispatch-to-completion window.
4. **Semantic Summary Suffix Suppression**:
   `tool_step_semantic_summary` explicitly suppressed all trailing badges while `status == ToolStepStatus::Running`, completely swallowing the in-flight indicators and creating an inconsistent rendering contract between plain and semantic summaries.

---

## Decision Drivers

- **Uncompromising Streaming Transparency**: In a streaming-first Agent, token flow must project immediate semantic meaning. If the model emits `"path": "src/main.rs"`, the UI must immediately reflect `Edit src/main.rs`, never hiding behind generic placeholders.
- **Physical Phase Honesty (Causality & Ownership)**: The interface must cleanly demarcate when the model is generating arguments (`Ingesting`) versus when the local machine is executing the tool (`Executing`), providing real-time elapsed duration for active execution.
- **Single Breathing Anchor Preservation ([ADR-0008], [ADR-0026] §INV-STREAM-TOOL-05)**: Never introduce chaotic per-step animated spinners. Visual liveness is conveyed through semantic progressive disclosure, static timers, and hue hierarchy. The Activity Bar remains the sole animated anchor.
- **Strict Execution Gating ([ADR-0026] §INV-STREAM-TOOL-01)**: While partial arguments may be projected for *display*, no tool may *execute* on partial or unverified arguments. Display projection and execution gating remain strictly decoupled.
- **Zero-Waste Terminal Layout Budget**: Partial streaming projections must be resilient and strictly bounded. Malformed or escaping strings must never cause layout crashes, panics, or unmetered terminal reflow thrashing.

---

## Considered Options

### Option 1 (Chosen): Autonomous Execution Slot with Fault-Tolerant Streaming Projection and Bifurcated Lifecycle
- Implement a fault-tolerant partial JSON streaming extractor for tool arguments.
- Instantly project semantic fields (such as `path`, `command`, `query`) the moment they close in the stream.
- Formally bifurcate the in-flight lifecycle into `Ingesting` and `Executing`.
- Render dynamic elapsed execution timers (`· applying (0.4s)`, `· running (2.1s)`) synchronized with the terminal heartbeat.
- Render pre-commit Diff previews in the expanded body as soon as arguments are established.

### Option 2: Retain Pure Count-Only Progress and Monolithic `Running`
- Keep the ADR-0026 model unchanged.
- *Assessment*: Rejected. Sustains the "frozen UI" illusion, suppresses semantic file identification during edits, and fails modern AI interface expectations.

### Option 3: Raw Character-by-Character JSON Stream Dumping
- Dump unparsed raw JSON deltas directly into the transcript body.
- *Assessment*: Rejected per ADR-0026 §Option 3. Unparsed JSON syntax noise (escaped quotes, unclosed braces, raw delimiters) produces terminal wrapping jitter and degrades readability.

---

## Decision Outcome

Chosen option: **Option 1**.

### 1. Fault-Tolerant Partial Argument Projection

Introduce resilient, zero-panic partial argument extraction:
- Even when the trailing JSON payload is incomplete (e.g. `{"path": "src/auth.rs", "new_string": "fn login() ...`), closed key-value string fields are extracted deterministically.
- `EditPresenter`, `ExecuteCommandPresenter`, and `SearchPresenter` receive early projected fields. As soon as the first argument field is closed, the header projects full domain identity (`Edit src/auth.rs` instead of `Edit text`).
- Once both `old_string` and `new_string` are extractable, diff hunk counts (`+12 -3`) project immediately onto the header and the expanded body.

### 2. Bifurcated In-Flight Lifecycle Model

The in-flight state is bifurcated across the model and presentation layers:

```text
                      ┌────────────────────────────────────────────────────────┐
                      │              Autonomous Tool Execution Slot            │
                      │                                                        │
[ToolCallStarted] ────► Ingesting (Model Generating Arguments)                 │
                      │   • Streaming Input Progress: `receiving input · X KB` │
                      │   • Fault-Tolerant Semantic Header (Path / Command)    │
                      │                                                        │
[ToolCall / Complete] ─► Executing (Local Host / Sandbox In-Flight)            │
                      │   • Live Execution Elapsed Timer: `running (1.2s)`     │
                      │   • Pre-Commit Diff Preview rendered in body           │
                      └──────────────────────────┬─────────────────────────────┘
                                                 │
                                [ToolResult / Terminal]
                                                 │
                                                 ▼
                       Terminal Outcome (Ok | Failed | Denied | Cancelled)
                         • Final duration frozen: `(280ms)`
```

1. **Ingesting Phase**:
   - `input_slot.is_some()` indicates active model streaming.
   - Summary displays projected semantic target + input progress: `Edit src/auth.rs · receiving input (2.4 KB)`.
2. **Executing Phase**:
   - `input_slot` cleared upon `collapse_tool_call`; `execution_started_at` captured.
   - Summary shifts from byte progress to active execution duration:
     - `edit_text` / `write_file`: `Edit src/auth.rs +12 -3 · applying (0.3s)`
     - `execute_command`: `Run cargo test · running (4.8s)`
     - Generic: `<summary> · running (1.1s)`
   - Diff preview is rendered in the expanded body immediately without waiting for disk commit.
3. **Terminal Phase**:
   - Live elapsed timers freeze into immutable duration badges `(310ms)` or failure indicators.

### 3. Summary & Semantic Line Parity

`tool_step_semantic_summary` is aligned with `tool_step_summary`. Both methods now project identical trailing lifecycle clauses across all in-flight and terminal states.

---

## Invariants & Behavioral Boundaries

- **`[INV-SLOT-01]` Strict Execution Gate**:
  Fault-tolerant partial parsing is strictly confined to presentation projection. Under no circumstances may an incomplete, streaming argument payload be dispatched for execution or disk modification. Execution requires an authenticated, closed `serde_json` object.
- **`[INV-SLOT-02]` Immediate Identity Projection**:
  A tool presenter MUST project the semantic target (e.g. file path, command name) as soon as the corresponding field is terminated in the argument stream. Generic fallbacks (e.g. `Edit text`) are permitted only while the specific field has not yet arrived.
- **`[INV-SLOT-03]` Non-Animated In-Flight Liveness**:
  In-flight liveness in the transcript MUST be represented via semantic text clauses, elapsed time readouts, and theme hues. Per-step animated spinners or pulsing glyphs remain prohibited. The Activity Bar is the sole animated anchor ([ADR-0008]).
- **`[INV-SLOT-04]` Heartbeat-Bound Elapsed Timing**:
  Live execution timers (`· running (Xs)`, `· applying (Xs)`) MUST derive from monotonic wall-clock timestamps at paint time, refreshed via the canvas heartbeat (~100ms). They MUST NOT generate per-tick async event loop wake-ups.
- **`[INV-SLOT-05]` In-Flight Pre-Commit Diff Projection**:
  An expanded diff-capable step with complete arguments MUST render its diff preview during the `Executing` phase prior to receiving `ToolResult`.

---

## Operational Consequences & Migration Path

- Operators watching long-running edits or code generation immediately see the target file path and live diff projection while tokens stream.
- Heavy execution steps provide unambiguous feedback on elapsed local execution time, eradicating the illusion of a frozen terminal.
- Subagent and command flows retain single-anchor visual tranquility while delivering maximum transparency.

---

## Rejected Alternatives & Negative Knowledge

### Option 2 (Rejected): Retain Pure Count-Only Progress (ADR-0026 Status Quo)
- *Why considered*: Minimal code change.
- *Why rejected*: Severely degrades user experience during large file edits; forces artificial blindness on the operator when the model has already specified the target file.

### Option 3 (Rejected): Unchecked Partial JSON Stream Body Dumping
- *Why considered*: Maximum raw transparency.
- *Why rejected*: Unparsed syntax debris produces layout jitter, escaping bugs, and terminal frame budget exhaustion. Structured, fault-tolerant semantic extraction provides full clarity with zero noise.

---

## Links

- Complements: [ADR-0008: Single Tool Contract](0008-single-tool-contract.md)
- Complements: [ADR-0020: Interactive Component Registry](0020-interactive-component-registry-single-source-of-truth.md)
- Amends & Extends: [ADR-0026: Streamed Tool-Input Progress](0026-streamed-tool-input-progress.md)
