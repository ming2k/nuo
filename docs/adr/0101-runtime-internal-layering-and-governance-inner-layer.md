---
id: ADR-0101
title: "Runtime Internal Layering: Subsystem Grouping and Enforced Inner Layers"
status: accepted
date: 2026-11-29
scope: harness/nuo-harness, architecture/modules, harness/governance
superseded_by: null
negative_knowledge: true
---

# 0101. Runtime Internal Layering: Subsystem Grouping and Enforced Inner Layers

- Status: Accepted
- Date: 2026-11-29
- Deciders: Nuo Architecture Working Group
- Amends: [ADR-0100](0100-cognitive-runtime-consolidation-and-governance-layer-inversion.md) §Migration Roadmap (executes the internal-organization step it deferred)

---

## Context and Problem Statement

ADR-0100 consolidated the cognitive runtime onto a single crate (`nuo-harness`) and
**rejected** a physical `nuo-cognition`/`nuo-governance` crate split, on the evidence that
the module graph is cyclic in both directions. That left the internal organization
unresolved: `nuo-harness/src` was a flat list of **~30 sibling modules** (`agent/`,
`model_request/`, `conversation_context/`, `compaction/`, `skills/`, `interop/`,
`orchestration.rs`, `round_lifecycle.rs`, `hooks.rs`, `host.rs`, `execution/`,
`durability/`, …) with no visible layering. The crate boundary was correct; the *module*
boundary was not expressed.

The standard that governs this (`docs/dev/standards/architecture.md` §3.2) is explicit: a
new crate is admitted only when a responsibility "cannot be cleanly modeled as internal
modules of existing domain crates." Since ADR-0100 established the responsibilities are not
cleanly separable into crates, the correct instrument is **module structure** — which is a
design tool, cheap and reversible, unlike the compilation/publishing unit that a crate is.

## Decision Drivers

- **Honest names live in modules, not crates.** Renaming/re-parenting a module is a `git mv`
  + path rewrite; renaming a crate ripples across every consumer. Put the design in the
  cheap place.
- **The boundary must be a mechanism, not prose.** Whatever layering is ratified must be
  enforced by a regression test (the repository's "mechanisms over exhortations" rule).
- **Behavior-preserving.** This is a pure organizational change: no interface inversions, no
  logic change, no new crate.

## Architectural Decisions

### 1. Five subsystem groups replace the flat module list

`crates/nuo-harness/src/` is organized into grouped subsystems. The grouping is
navigational — Rust does not enforce directory structure — and the group names are honest:

| Group | Responsibility | Members |
| :--- | :--- | :--- |
| **`cognition/`** | the single agent's mind | `agent`, `model_request`, `conversation_context`, `context_lifecycle`, `context_projection`, `compaction`, `budget`, `skills`, `interop` |
| **`governance/`** | how the agent is restrained | `permission_policy`, `permission_store`, `bash_policy`, `guard`, `trajectory_guard`, `stream_loop_detector`, `interaction`, `scoped_tools` |
| **`orchestration/`** | per-round policy that joins cognition and governance | `mod` (round driver), `round_lifecycle`, `session_title`, `aspects`, `pipeline` |
| **`dispatch/`** | tool execution pipeline & scheduling | `pipeline`, `tool_manager`, `tool_scheduler`, `dynamic_tools`, `tool_call`, `tool_integration` |
| **`runtime/`** | substrate: environment, durability, hooks, ports | `execution`, `durability`, `host`, `hooks`, `hook_runner`, `inflight`, `offstream`, `no_provider`, `sync`, `extension` |
| **`tools/`** | orchestration-native model tools | `ask_user`, `recall_memory`, `todo` |

`orchestration.rs` becomes `orchestration/mod.rs`, so the `crate::orchestration::*` path is
preserved and the round driver keeps its identity.

### 2. `runtime` and `governance` are strict inner layers; `cognition` is wrapped

The enforced partial order is:

```
runtime ⊳ governance ⊳ cognition ⊳ { dispatch, orchestration }
```

Three boundaries are enforced by the integration test `tests/layering.rs`. The test parses
each group's sources with `syn` and rejects any real `crate::<group>` path that inverts the
order. Being AST-based it is immune to comment/string mentions and to
`use crate::{group::…}` brace-grouping that a substring scan would miss; it skips
`#[cfg(test)]` items (test code is not a production edge) and `tests.rs` submodules
(whose `#[cfg(test)]` gate lives on the `mod` declaration in the parent file). Negative-control
unit tests in the same file prove the detector catches real violations *and* ignores the
exempt cases — a boundary test that cannot fail is not a mechanism.

- `governance` never references `cognition`, `orchestration`, `dispatch`, or `tools`
  (`[INV-HARNESS-03]`).
- `runtime` never references `cognition`, `orchestration`, `dispatch`, `governance`, or
  `tools` — it is the innermost layer (`[INV-HARNESS-04]`).
- `cognition` never references `orchestration` or `dispatch` — it is wrapped by them
  (`[INV-HARNESS-04]`).

### 3. The layering cycles were broken, not merely recorded

Grouping alone left two cycles. They were resolved by **sink** and **move**
(behavior-preserving), not by fabricating an interface layer:

- `runtime ↔ cognition`: the sole `runtime → cognition` edge was `runtime::hooks`
  building a context message via `cognition::conversation_context::hidden_user`. The
  message constructors moved to the leaf `runtime::message` (`hidden_user`,
  `hidden_user_with_reason`, `tool_image`, `visible_user`).
- `cognition ↔ orchestration`: the `cognition → orchestration` edges were a clock helper
  (`unix_epoch_ms`), three backoff helpers (`retry_delay_ms`, `apply_jitter_ms`,
  `public_retry_reason`), and two cognitive engines (`pipeline`, `aspects`). The helpers
  moved to the leaves `runtime::time` and `runtime::retry`; `pipeline` and `aspects` moved
  into `cognition`, where they belong.

After the change every enforced boundary is clean, and the observed cross-group edges all
point inward along the enforced order: `dispatch`/`orchestration` → `cognition` →
`governance` → `runtime`, with `tools` depending only on `runtime` and `dispatch` depending on
`tools`.

### Normative Invariants

- **`[INV-HARNESS-03]` (reaffirmed, ADR-0100)**: `governance` is a strict inner layer — it
  never references `cognition`/`orchestration`/`dispatch`/`tools`.
- **`[INV-HARNESS-04]` (established here)**: `runtime` is the innermost group (references no
  other group), and `cognition` is wrapped — it never references `orchestration` or
  `dispatch`. Both enforced by `tests/layering.rs`.
- **`[INV-RUNTIME-01]` (reaffirmed, ADR-0100)**: `nuo-harness` is the sole cognitive-runtime
  crate; the retired `nuo-agent` twin must not be re-created. Enforced by
  `tests/layering.rs::single_cognitive_runtime_crate`.

## Consequences

- **Positive**: the crate is navigable by responsibility; the layering intent is visible in
  the tree; the enforced boundary lives beside the module it governs. Zero behavior change —
  the full workspace test suite is unchanged.
- **Negative & Trade-offs**: a large, cross-crate mechanical diff (re-parented modules +
  rewritten `crate::` and `nuo_harness::` paths in `nuo-server` and `nuo`). The
  crate-root `use super::*` idiom required the re-parented module roots to switch to
  `use crate::*;`. Mitigation: the compiler is the correctness oracle for every moved path,
  and the change is behavior-preserving, so the existing test suite is the regression net.

## Rejected Alternatives & Negative Knowledge

### Alternative A — Physical `nuo-cognition`/`nuo-governance` crates (Rejected)
- **Why considered**: a crate is a compiler-enforced firewall — the strongest possible
  boundary, and the only way in Rust to forbid an intra-crate edge at compile time.
- **Why rejected**: ADR-0100 §2 proved the responsibilities are mutually coupled. Even after
  §3's cycle-breaking the edges are dense and cross-cutting — `orchestration`/`dispatch` →
  `cognition`, `dispatch` → `governance`, `cognition` → `runtime` — so a crate cut would draw
  its seam through many live interfaces and force a synthetic port layer. The two-crate split
  remains unjustified; the enforced inner-layer boundaries deliver the needed guarantee.

### Alternative B — Keep the flat top-level module list (Rejected)
- **Why considered**: zero churn; the crate already compiles and tests green.
- **Why rejected**: the layering boundary ADR-0100 ratified would remain invisible in the
  tree, inviting the exact drift ADR-0010's lineage spent four ADRs trying to prevent.

### Alternative C — Re-export shims so old module paths keep resolving (Rejected)
- **Why considered**: would delete most of the consumer churn.
- **Why rejected**: `pub use cognition::skills;` recreates `crate::skills`, defeating the
  rename and re-accumulating the flat namespace this ADR removes. Consumers are updated
  instead.

## Migration Roadmap

1. **Re-parent modules into the five groups** and rewrite `crate::`/`nuo_harness::` paths.
   (Executed; workspace green.)
2. **Enforce the inner layers** with the source-scan integration test `tests/layering.rs`.
   (Executed.)
3. **Break the residual cycles** by sinking the shared leaf helpers (`runtime::time`,
   `runtime::retry`, `runtime::message`) and moving `pipeline`/`aspects` into `cognition`,
   then enforcing the `runtime` and `cognition` boundaries. (Executed with this ADR; all three
   enforcement rules green.)

## Links

- [ADR-0010: Harness Decomposition, Capability Tool Decoupling, and Cognitive Runtime Unification](0010-harness-decomposition-and-agent-unification.md) (superseded)
- [ADR-0100: Cognitive Runtime Consolidation, Orphaned Twin Retirement, and Governance-Layer Inversion](0100-cognitive-runtime-consolidation-and-governance-layer-inversion.md)
- [Subsystem Architecture Blueprint](../dev/subsystems.md) §1
