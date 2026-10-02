---
id: ADR-0003
title: "Autonomous Retained-Mode Terminal Canvas Engine (Nuotc)"
status: accepted
date: 2026-10-02
scope: workspace/substrate, terminal/nuotc, ui/engine
superseded_by: null
negative_knowledge: true
---

# 0003. Autonomous Retained-Mode Terminal Canvas Engine (Nuotc)

- Status: Accepted
- Date: 2026-10-02
- Deciders: Nuo Architecture Working Group
- Consulted: Core Contributors, Terminal & Systems Engineering Teams
- Informed: System Architects

---

## Context and Problem Statement

Building high-fidelity interactive terminal clients (`nuox`) for streaming LLM reasoning, multi-pane tool execution, and markdown tables exposed serious flaws in conventional terminal rendering approaches:
1. **Immediate-Mode Flicker and Bandwidth Bloat**: Naive terminal interfaces re-emit full screen buffers on every frame. Streaming token arrivals at 60+ Hz saturated terminal PTY bandwidth, caused severe visual tearing, and wasted host CPU cycles.
2. **Domain Coupling**: Previous terminal engines interwove ANSI escape handling and UI layouts with AI agent domain models (`Turn`, `ReasoningBlock`, `AgentState`). This prevented testing the rendering engine in isolation and made it impossible to reuse in non-AI terminal applications.
3. **Fragile Layout Engines**: Ad-hoc string slicing and coordinate arithmetic failed on dynamic terminal resizes, unicode full-width characters (CJK), and complex bordered containers.

We require a general-purpose, retained-mode terminal rendering engine that delivers zero-flicker differential output, declarative Flexbox layouts, and absolute isolation from AI domain concepts.

---

## Decision Drivers

- **Zero-Flicker Differential Rendering**: Compute optimal ANSI diff sequences between consecutive frame buffers to minimize escape codes and eliminate visual tearing.
- **Strict Domain Decoupling**: Complete absence of AI domain vocabulary (`agent`, `prompt`, `token`, `session`) within the engine.
- **Declarative Flexbox Layout Solver**: Robust container geometry calculation supporting Flexbox axes, alignment, and constraints across variable terminal dimensions.
- **Retained Scene Graph**: Persistent UI component hierarchies enabling efficient dirty-region invalidation and event dispatching.
- **Autonomous Substrate Reusability**: Standalone crate (`nuotc`) independently publishable to `crates.io`.

---

## Decision Outcome

We establish **`nuotc`** as an autonomous, domain-free terminal canvas and differential rendering engine:

```text
┌────────────────────────────────────────────────────────┐
│                      nuotc (Root)                      │
│   Unified Retained-Mode Terminal Canvas & Diff Engine  │
└───────────────────────────┬────────────────────────────┘
                            │
        ┌───────────────────┼───────────────────┐
        ▼                   ▼                   ▼
 ┌──────────────┐    ┌──────────────┐    ┌──────────────┐
 │      ui      │    │   widgets    │    │    layout    │
 │ Retained     │    │ Block, Para, │    │ Rect, Flex,  │
 │ Scene Graph  │    │ Clear, Spans │    │ Anchor       │
 └──────┬───────┘    └──────┬───────┘    └──────┬───────┘
        │                   │                   │
        └───────────────────┼───────────────────┘
                            │
        ┌───────────────────┴───────────────────┐
        ▼                                       ▼
 ┌──────────────┐                        ┌──────────────┐
 │   terminal   │                        │    render    │
 │ Backend,     │                        │ Run-length   │
 │ Crossterm    │                        │ Cell diff &  │
 │ abstraction  │                        │ Escape emit  │
 └──────────────┘                        └──────────────┘
```

### 1. Decoupled Modular Architecture
`nuotc` organizes its responsibilities into six acyclic modules:
- **`terminal`**: Low-level terminal abstractions and crossterm wrapper with raw mode management and event polling.
- **`layout`**: Rect geometry, Flexbox direction, wrap, and alignment solvers.
- **`render`**: Double-buffered cell grid (`Buffer`), run-length encoded diffing, and minimal ANSI escape sequence emitter.
- **`widgets`**: Fundamental UI building blocks (Paragraph, Block, Borders, Clear, Span, Line).
- **`ui`**: Retained-mode scene graph, component tree lifecycle, and event bubbling.

### 2. Differential Rendering Pipeline
Rendering operates via double buffering:
1. Widgets paint cells into a staging `Buffer`.
2. The differential engine compares the staging `Buffer` against the previously displayed `Buffer`.
3. Only mutated cells are grouped using run-length encoding and emitted to stdout with targeted cursor jump sequences (`\x1b[{row};{col}H`), reducing output bytes by over 90% during streaming.

---

## Invariants & Behavioral Boundaries

- **`[INV-NUOTC-01] Zero Domain Vocabulary`**: `nuotc` must never import, reference, or define AI agent concepts. It must remain a domain-free graphics and layout library.
- **`[INV-NUOTC-02] Double-Buffered Diff Invariant`**: All output emitted to the physical terminal must pass through the `render` module's diff engine. Bypassing the buffer to perform raw writes is prohibited.
- **`[INV-NUOTC-03] Autonomous Package Identity`**: `nuotc` manages its own independent version and package metadata in `nuotc/Cargo.toml`. It must not inherit `version.workspace = true`.

---

## Negative Knowledge & Rejected Alternatives

### 1. Immediate-Mode Full Redraws (Naive ANSI Clearing)
- **Why considered**: Simple implementation requiring no retained buffer state.
- **Why rejected**: Catastrophic screen flicker, cursor jitter, and high CPU usage at streaming token frequencies.

### 2. Inlining Terminal Rendering Directly into `nuox`
- **Why considered**: Avoided maintaining a separate crate boundary for the terminal client.
- **Why rejected**: Intertwines presentation logic with rendering algorithms, preventing unit testing of layout solvers and blocking external adoption of the canvas engine.
