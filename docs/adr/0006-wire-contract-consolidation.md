---
id: ADR-0006
title: "Wire Contract Consolidation and the Amendment of [INV-WIRE-01]"
status: accepted
date: 2026-10-03
scope: workspace/topology, protocol/wire, architecture/layering
superseded_by: null
negative_knowledge: true
---

# 0006. Wire Contract Consolidation and the Amendment of [INV-WIRE-01]

- Status: Accepted
- Date: 2026-10-03
- Deciders: Nuo Architecture Working Group
- Consulted: Runtime & Interface Teams, Release Engineering
- Informed: System Architects

---

## Context and Problem Statement

ADR-0001 mandated the complete dismantling of the catch-all `nuo-contracts`
crate, and ADR-0005 established `nuo-wire` as the dedicated home for wire
envelopes and session communication entities. Executing that dismantling
revealed a structural fact that the original ADRs did not anticipate:

1. **The shared contracts are not independently movable.** The former
   `nuo-contracts` modules are deeply entangled along the wire/domain axis —
   `events` references `capability`, `monitor`, `command`, `cognitive`,
   `token_ledger`, and `usage_stats`; `capability` references `subagent`,
   `tool_access`, and `tool_output`; `subagent` references `model`. They form a
   single strongly-connected cluster, not a set of separable leaf substrates.
2. **Only `nuo-wire` sits below every consumer.** Every other candidate home
   (`nuo-agent`, `nuo-tool`, `nuo-host`, `nuo-mcp`, `nuo-model-codec`) either
   already depends on the cluster or would create a dependency cycle the moment
   the cluster referenced it back. `nuo-wire` is the sole cycle-free destination
   for the whole cluster.
3. **The cluster names leaf-substrate types.** The contracts reference
   `nuo_mcp::McpConnectionStatus`, `nuo_host::{JobId, WorkspaceSecuritySnapshot}`,
   `nuo_tool::{TodoList, HazardLevel}`, and `nuo_model_codec` model types. Hosting
   the cluster in `nuo-wire` therefore forces `nuo-wire` to depend on those
   substrates — and those substrates pull an async runtime (`tokio` via `netune`).

The result is that `nuo-wire` can no longer satisfy ADR-0005's
`[INV-WIRE-01]` verbatim ("zero asynchronous runtime dependencies (no `tokio`)…
must compile in under 1 second"). A decision is required: how to complete the
dismantling without either resurrecting `nuo-contracts` or leaving a binding
invariant silently violated.

---

## Decision Drivers

- **Complete the dismantling (ADR-0001)**: `nuo-contracts` must not exist.
- **No dependency cycles**: the shared contracts must live below all consumers.
- **Single source of truth**: no lossy per-crate mirrors of the same DTO.
- **Honest invariants**: a binding invariant must never be silently violated;
  it must be amended through governance.

---

## Decision Outcome

The former `nuo-contracts` domain and wire contracts are **consolidated into
`nuo-wire`** as a single source of truth, and `nuo-contracts` is deleted.

`nuo-wire` now owns two coherent responsibilities:
1. **Wire framing** — byte-level envelopes (`Wire`), the length-delimited
   `NativeWireCodec`, and protocol-version invariants.
2. **Shared zero-I/O domain contracts** — capability traits (`Provider`,
   `Tool`, `Hook`), conversation/tool-output types, the context-pressure model,
   token ledger, subagent profiles, and the event vocabulary.

`[INV-WIRE-01]` is **amended**: `nuo-wire` must contain **zero I/O** (no
filesystem, no network access) and must not **use** an async runtime directly;
async-runtime crates (`tokio`) may appear only transitively through the leaf
substrates whose types the contracts name. The "compile in under 1 second"
aspiration is retained as a soft goal, not a hard invariant.

---

## Invariants & Behavioral Boundaries

- **`[INV-WIRE-02] Zero-I/O Contract Layer`**: `nuo-wire` must not mutate the
  filesystem, open network connections, or spawn tasks. It is a pure data and
  trait definition crate. Read-only path resolution (e.g.
  `std::fs::canonicalize` for temp/skill root helpers) is permitted.
- **`[INV-WIRE-03] No Direct Async Runtime`**: `nuo-wire` must not declare
  `tokio` (or any async runtime) as a *direct* dependency. Async runtimes may
  appear only transitively via leaf substrates. This supersedes the literal
  "no `tokio`" clause of ADR-0005 `[INV-WIRE-01]`.
- **`[INV-WIRE-04] Single Contract Source of Truth`**: A shared wire/domain DTO
  must be defined exactly once. Per-crate "mirror" copies that drop fields are
  prohibited — they silently diverge and break type identity across crate
  boundaries.
- **`[INV-ARCH-01]` (ADR-0001) remains binding**: no crate named `contracts`,
  `common`, `shared`, or `core` may exist.

---

## Positive Consequences

- `nuo-contracts` is permanently gone; `[INV-ARCH-01]` is satisfied.
- One canonical definition per contract type — no lossy mirrors, no cross-crate
  type-identity breakage.
- The dependency graph is acyclic and flat; `nuo-wire` is the single contract
  layer beneath every consumer.

## Negative Consequences & Trade-offs

- `nuo-wire` is now a larger, higher-fan-out crate: a change to any shared
  contract recompiles every consumer. This is the "Rebuild Amplification"
  concern ADR-0001 raised — accepted here because the alternative (scattered
  duplicates) is strictly worse for correctness.
- `nuo-wire` transitively links `tokio`, so its clean "compile in <1s" property
  is lost. Mitigation: the runtime is never *used* by `nuo-wire` itself, so the
  link is inert.

---

## Rejected Alternatives & Negative Knowledge

### 1. Scatter each contract module to its "natural" leaf crate
- **Why considered**: Matches ADR-0001's original sketch (DTOs → `nuo-client`,
  model wire formats → `nuo-model-codec`, session state → `nuo-persistence`).
- **Why rejected**: The modules are a strongly-connected cluster. Moving
  `events` to one crate while `capability` and `monitor` stay elsewhere
  produces cycles (`events` ↔ `capability` ↔ `subagent`). Disentangling the
  cluster into acyclic sub-modules is a multi-week refactor of ~12k lines and
  every call site, for no behavioral gain.

### 2. Retain `nuo-contracts` as-is
- **Why considered**: Zero migration cost; the crate compiles.
- **Why rejected**: Directly violates ADR-0001 `[INV-ARCH-01]`, which bans
  catch-all `contracts`/`common`/`shared`/`core` crates. Retaining it would
  require superseding an accepted ADR to keep a junk drawer.

### 3. Per-crate reduced "mirror" DTOs (the abandoned in-flight approach)
- **Why considered**: Let each leaf crate own a trimmed version of the wire
  DTO it needs, avoiding the heavy `nuo-wire` fan-in.
- **Why rejected**: Proven catastrophic in practice. The mirrors dropped fields
  (e.g. `TokenSourceReport` reduced to three scalars), so a value produced by
  the harness and a value expected by the client were *different types* with the
  same name — dozens of `E0308` type-mismatch errors across `nuo-harness`,
  `nuox`, and `nuo`. Lossy mirrors break type identity and are banned by
  `[INV-WIRE-04]`.

### 4. Keep `nuo-wire` literally "no tokio" by inlining the leaf types
- **Why considered**: Preserves `[INV-WIRE-01]` verbatim.
- **Why rejected**: The contracts must *name* `JobId`, `McpConnectionStatus`,
  `HazardLevel`, `TodoList`, etc. Re-declaring those in `nuo-wire` would either
  duplicate them (rejected above) or require moving them out of their owning
  capability crates — inverting `[INV-TOOL-01]`/`[INV-HOST-01]` native ownership.

---

## Links

- Related ADRs: [ADR-0001](0001-flat-workspace-and-microkernel-capability-topology.md) (junk-drawer ban), [ADR-0005](0005-unified-binary-and-concentric-runtime-architecture.md) (wire entity architecture; `[INV-WIRE-01]` amended here).
