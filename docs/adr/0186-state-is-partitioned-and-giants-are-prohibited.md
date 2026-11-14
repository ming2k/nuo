---
id: ADR-0186
title: "State Is Partitioned; God Objects and Giant Functions Are Prohibited"
status: accepted
date: 2026-11-14
scope: tui, server, engine, workspace/governance
superseded_by: null
supersedes: []
negative_knowledge: true
---

# 0186. State Is Partitioned; God Objects and Giant Functions Are Prohibited

- Status: Accepted
- Date: 2026-11-14
- Amends: [ADR-0072](0072-pure-declarative-component-paradigm-and-tea-single-direction-dataflow-topology.md)

## Context and Problem Statement

The codebase has repeatedly paid for centralised mutable state and giant
functions:

- `nuo::shell::App` has **212 public fields** in a struct spanning ~830 lines,
  with 12 `impl App` files. ADR-0072 diagnosed exactly this object as the
  original monolith; production still uses it.
- `nuo-server::ThreadDriver` exposes 24 public fields and `ThreadDriver::run`
  is **1,406 lines**.
- `nuo-engine::Agent` has 48 fields behind `Arc<Mutex<…>>` and 10 `impl Agent`
  files.
- `spawn_response_translator` is **1,767 lines**; `apply` is **706**;
  `apply_transcript` is **532**; `resume_streaming_inner` is **1,163**;
  `assemble_with_store` is **884**; `draw_message_body` is **1,278**.

Large functions cannot be unit-tested in isolation, make review diffs
unreviewable, and hide stale branches. A 200-field state bag makes every
borrow a global reasoning problem and lets unrelated features mutate each
other's state.

## Architectural Decisions

### 1. `App` is partitioned by ownership

`App` is a composition of sub-states, not a field bag:

```rust
pub struct App {
    pub session: SessionState,
    pub transcript: TranscriptState,
    pub composer: ComposerState,
    pub surfaces: SurfaceState,
    pub telemetry: TelemetryState,
    pub link: LinkState,
    pub config: ConfigState,
    pub input: InputState,
}
```

Each sub-state is declared in `shell/app/state/`, is `Default`, and owns the
fields its domain uses. Code that touches the transcript does not see composer
or surface fields.

### 2. `ThreadDriver` exposes a session context, not 24 fields

The driver's channels, agent, store, config, registries, lifecycle, and
workspace handles move into `SessionContext`. `ThreadDriver` keeps the run
loop and a `ctx: SessionContext`; handlers take `&SessionContext` or a narrow
borrow. `ThreadDriver::run` dispatches by request family into named methods.

### 3. `Agent` groups its state by concern

`Agent` keeps identity/provider/host and composes:

- `ToolState` — toolset, resolved tools, dynamic tools, disabled masks,
  tool manager.
- `ContextState` — todos, round counter, workspace roots/security,
  confinement, project rules, thread id, prune/projection settings.
- `ControlState` — human broker, hooks, bash policy, execution policy.
- `QueueState` — inbox, session queues, steering/follow-up modes.

The fields become private; accessors are the only way in.

### 4. Size ceilings are mechanical

- A function body must be **≤ 400 physical lines**.
- A struct must have **≤ 80 fields**.
- A non-test source file must be **≤ 2,000 physical lines**.
- A module that exceeds a ceiling must be split by responsibility, not by
  adding `#[allow]`.

A governance test parses the workspace with `syn` and fails on a violation.
Test modules are exempt from the function/file ceiling but not from the
"no god object" rule when they construct production types.

### 5. Invariants

- **`[INV-STATE-01]` Partitioned Ownership**: State types above the documented
  field ceiling must be decomposed; no new `App`-sized bag is admitted.
- **`[INV-SIZE-01]` Function/Struct/File Ceilings**: The size gate above is
  enforced by `governance::source_size`.

## Consequences & Trade-offs

- **Positive**: Review diffs become local; sub-states can be unit-tested with
  `Default` fixtures; the borrow checker stops seeing one global object;
  giant match towers become dispatchers.
- **Negative & Incurred Costs**: A large mechanical `self.field` →
  `self.group.field` sweep and several function splits. Behavior must remain
  identical; snapshot tests are the guard.

## Discarded Alternatives (Negative Knowledge)

### Alternative A — Keep `App` and add accessor methods
- **Why considered**: Less churn.
- **Why rejected**: Accessors do not partition ownership; every method still
  borrows the whole object, and the field count keeps growing.

### Alternative B — Rewrite the TUI as a full TEA application
- **Why considered**: ADR-0072's original direction.
- **Why rejected**: A full rewrite is not required to get local ownership. The
  sub-state split preserves the working renderer and snapshot coverage.

## Links

- [ADR-0072](0072-pure-declarative-component-paradigm-and-tea-single-direction-dataflow-topology.md)
- [ADR-0185](0185-storybook-retirement-and-single-presentation-path.md)
