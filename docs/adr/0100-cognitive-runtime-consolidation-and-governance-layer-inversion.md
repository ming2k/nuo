---
id: ADR-0100
title: "Cognitive Runtime Consolidation: Orphaned Twin Retirement and Governance-Layer Inversion"
status: accepted
date: 2026-11-29
scope: architecture/workspace-topology, harness/nuo-harness, agent/nuo-agent, harness/governance
superseded_by: null
negative_knowledge: true
---

# 0100. Cognitive Runtime Consolidation: Orphaned Twin Retirement and Governance-Layer Inversion

- Status: Accepted
- Date: 2026-11-29
- Deciders: Nuo Architecture Working Group
- Supersedes: [ADR-0010](0010-harness-decomposition-and-agent-unification.md) §2–§3 (cognitive-engine unification *into* `nuo-agent`)
- Resolves: [ADR-0009](0009-single-agent-runtime.md) §4 (the deferred "retire the orphansed SDK" product decision)
- Amends: [ADR-0005](0005-unified-binary-and-concentric-runtime-architecture.md) §"Cognitive & Tool Lifecycle"

---

## Context and Problem Statement

Three accepted ADRs diagnosed the same structural defect and none of them, in the end,
removed it:

- **ADR-0008** flagged a duplicate agent runtime.
- **ADR-0009** ("Single Agent Runtime") proved `nuo-agent::Agent`
  (`nuo-agent/src/agent/runtime.rs`) had **zero callers outside its own tests**, and that
  the shipped daemon runs `nuo-harness::Agent`. It then **deferred** the real question —
  *"does the project still ship a standalone embedding agent SDK?"* — to a product
  decision, leaving the crate in the workspace as "feature-bearing but orphaned."
- **ADR-0010** ("Harness Decomposition") ratified the *opposite* remedy: port the harness's
  ReAct loop and compaction **into** `nuo-agent`, and reduce `nuo-harness` to a pure
  governance wrapper (`nuo → nuo-harness → nuo-agent`).

Reality after all three: **the migration never executed.** `nuo-harness` still owns the
streaming ReAct loop (`nuo-harness/src/agent/`, ≈6.9k LOC) and the causal compactor
(`nuo-harness/src/compaction/`), while `nuo-agent` remains a second, unreachable cognitive
engine. The repository therefore carries the *worst* state named in ADR-0010's own
Rejected-Alternatives section: two engines, one dead, with no code path selecting the dead
one. The only inbound edge to `nuo-agent` is a bridge (`agent/cognitive_bridge.rs`) whose
entry point `run_cognitive_prompt` has no callers.

A fourth liability compounds it: **`nuo-harness` is a mixed crate.** The cognition
concerns (loop, prompt assembly, compaction) and the governance concerns (permission
brokering, sandbox containment, approvals, trajectory supervision) live as sibling modules
with **mutual dependencies**, so the boundary ADR-0010 wanted is not actually reflected in
the module graph.

This ADR finishes the job — not by executing ADR-0010's crate migration, but by the
decision the evidence actually supports.

## Decision Drivers

- **One runtime, no dead weight.** A cognitive engine nothing executes is not
  architecture; it is CI cost and reader confusion.
- **The boundary must be a mechanism, not prose.** A responsibility split that exists only
  in a diagram decays; it must be enforced by the compiler or a regression test.
- **Respect the repository's own crate-admission rule.** `docs/dev/standards/architecture.md`
  §3.2: a new crate is admitted only if the responsibility "cannot be cleanly modeled as
  internal modules of existing domain crates."
- **No capability regression.** ACP multi-agent channel/delegation capability must survive.

## Architectural Decisions

### 1. Retire `nuo-agent` (the deferred product decision, now made)

The product **does not** ship a standalone embedding agent SDK. `nuo-agent` is deleted
(runtime, session, provider adapter, memory, skills, collaboration wrapper, and tests).

This is **capability-safe**, on the evidence of ADR-0009 §3 itself: the ACP collaboration
tools already live in the independent `nuo-acp` crate and are wired into the **production**
assembly path (`nuo/src/registry.rs::assemble_hosted` →
`nuo_acp::create_acp_tools` → `nuo_harness::bridge_substrate_tools`), gated by the `nuo`
crate's `collaboration` feature. The `nuo-agent::collaboration` module was only a re-export
wrapper over `nuo-acp`; no protocol-level coverage is lost (it already lives in
`nuo-acp/tests/`).

Consequences of the deletion: the harness bridge (`cognitive_bridge.rs`), the
`Agent::to_cognitive_agent` constructor, and the `nuo-agent` dependency edge are all
removed.

### 2. `nuo-harness` is the single cognitive runtime — a physical split is rejected

With `nuo-agent` gone, `nuo-harness` is the sole implementation of the ReAct loop. The
question is whether to *further* split `nuo-harness` into `nuo-cognition` + `nuo-governance`
crates (the "responsibility-split" remedy).

**A physical two-crate split is rejected.** The audit proves the cognition and governance
concerns are **mutually coupled** — a partition of the harness's module graph contains
cycles in both directions:

```
agent      -> governance, hooks, host, orchestration, model_request,
              conversation_context, cognitive, aspects, extension
governance -> agent, hooks
orchestration -> agent            agent -> orchestration
model_request -> agent            agent -> model_request
dispatch -> agent, conversation_context
interop  -> agent, ...            execution -> tools, ...
```

There is **no 2-partition of the module graph without cross edges**, in either direction, so
a crate cut would require fabricating a new interface-port layer (traits injected at the
composition root) purely to satisfy the crate boundary — an "interface layer invented for
its own sake" that `architecture.md` §3.2's admission rule is designed to prevent. The
correct granularity is **internal modules with an enforced one-directional boundary.**

### 3. Governance becomes a strict inner layer (mechanically enforced)

Rather than relocate code across crates, this ADR makes the boundary *real* by eliminating
the cycles:

- The single `agent → governance` reference was a by-product of the retired bridge
  (`to_cognitive_agent` constructing `HarnessApprovalHandler`); it disappears with §1.
- The single `governance → agent` reference was one type, `ScopedToolDisable`. It is
  relocated out of `agent` into `governance` (its consumers — the permission policy and the
  dispatch tool manager — already sit at or below the governance layer).

After both, the `governance` module references **nothing** in `crate::agent`, and the
`agent` module references **nothing** in `crate::governance`: governance is a strict inner
layer that cognition may depend on, never the reverse.

### Normative Invariants Established

- **`[INV-RUNTIME-01] No Second Cognitive-Runtime Crate`**: the retired `nuo-agent`
  cognitive-runtime crate MUST NOT be re-created; `nuo-harness` is the sole cognitive
  runtime. Enforcement: the `nuo-harness` test
  `tests/layering.rs::single_cognitive_runtime_crate`. Supersedes `[INV-AGENT-05]`; voids
  `[INV-AGENT-07]`.
- **`[INV-HARNESS-03] Governance Is the Inner Layer`**: the `governance` module MUST NOT
  reference any module above it (`cognition`, `orchestration`, `dispatch`, `tools`).
  Governance is depended *upon* by cognition, never the reverse.
  Enforcement: the `nuo-harness` integration test `tests/layering.rs`
  (`governance_is_a_strict_inner_layer`), which parses `src/governance/**` with `syn` and
  fails on any `crate::<upper-group>` path. (Superseded in scope by
  [ADR-0101](0101-runtime-internal-layering-and-governance-inner-layer.md), which extends the
  same mechanism to the `runtime` and `cognition` layers.)

## Consequences

- **Positive**:
  - The "two cognitive runtimes" liability diagnosed across ADR-0008/0009/0010 is
    **eliminated at the root**, not deferred a fourth time.
  - The cognition/governance boundary becomes an enforced red-line (`[INV-HARNESS-03]`)
    instead of an aspiration, and — because it is now one-directional — is a prerequisite
    that makes a *future* physical `nuo-governance` extraction mechanical if ever wanted.
  - Workspace, CI, and release surface shrink by one crate.
- **Negative & Trade-offs**:
  - Public API break: `nuo-agent` is removed from the workspace and from the publish set. No
    in-tree consumer is affected; external embedding-SDK consumers (none are supported by
    this repository's product) would lose the crate. Recorded deliberately; this is the
    ADR-0009 §4 product decision.
  - `nuo-harness` remains a large crate. Its size is accepted on the evidence of §2, but if
    it grows further the inner-layer boundary established here is exactly the seam to
    extract along.

## Rejected Alternatives & Negative Knowledge

### Alternative A — Execute ADR-0010 literally (migrate the loop *into* `nuo-agent`) (Rejected)
- **Why considered**: ADR-0010 is accepted and its layering intent (`governance` over
  `cognition`) is sound.
- **Why rejected**: It would keep alive a crate with no product purpose purely to host code
  the harness already runs correctly, and it migrates ≈7k LOC of load-bearing execution path
  for no behavioural gain. ADR-0009 already established this was a product question, not a
  cleanup, and the product answer is "no".

### Alternative B — Merge `nuo-agent` + `nuo-harness` into one renamed crate (Rejected)
- **Why considered**: Would remove the duplicate `Agent` in one move.
- **Why rejected**: `nuo-agent` is dead — merging dead code into the live runtime imports
  ~6.6k LOC of unreachable surface into the shipped crate and *grows* the God-crate concern
  ADR-0010 raised, the opposite of the goal. Deletion, not fusion, is the clean break.

### Alternative C — Physical `nuo-cognition` / `nuo-governance` crate split (Rejected)
- **Why considered**: A crate boundary is a hard compiler firewall — the strongest possible
  enforcement of the responsibility split.
- **Why rejected**: The module graph is cyclic in both directions (§2). A crate cut would
  force a synthetic port layer whose only purpose is to satisfy the boundary, violating
  `architecture.md` §3.2. The boundary is delivered as an enforced inner-module relationship
  instead; physical extraction is deferred until it is a clean cut.

## Migration Roadmap

1. **Retire `nuo-agent`** — delete the crate, the `cognitive_bridge`, `to_cognitive_agent`,
   and the dependency edge. (Executed with this ADR.)
2. **Invert the governance layer** — relocate `ScopedToolDisable` into `governance`; the
   `agent → governance` edge already disappears with step 1. (Executed with this ADR.)
3. **Enforce the boundary** — land the `[INV-HARNESS-03]` regression test. (Executed with
   this ADR.)

## Links

- [ADR-0005: Unified Binary, Concentric Runtime, and Wire Entity Architecture](0005-unified-binary-and-concentric-runtime-architecture.md)
- [ADR-0008: Single Tool Contract](0008-single-tool-contract.md)
- [ADR-0009: Single Agent Runtime and Contract-Layer Ownership of Agent Identity](0009-single-agent-runtime.md)
- [ADR-0010: Harness Decomposition, Capability Tool Decoupling, and Cognitive Runtime Unification](0010-harness-decomposition-and-agent-unification.md)
- [System Architecture Governance](../dev/standards/architecture.md) §3.2
