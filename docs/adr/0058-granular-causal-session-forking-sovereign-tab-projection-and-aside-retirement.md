---
id: ADR-0058
title: "Granular Causal Session Forking, Sovereign Tab Projection, and Aside Retirement"
status: accepted
date: 2026-10-29
scope: persistence/session, wire/protocol, server/nuo-server, tui/nuo-tui, harness/routing
superseded_by: null
negative_knowledge: true
---

# 0058. Granular Causal Session Forking, Sovereign Tab Projection, and Aside Retirement

- Status: Accepted
- Date: 2026-10-29
- Deciders: Nuo Architecture Working Group
- Consulted: Runtime, Persistence, Server, Protocol, and TUI maintainers
- Informed: System Architects, Release Engineering
- Supersedes: [ADR-0017](0017-concurrent-aside-execution-substrate-and-isolated-round-routing.md)
- Complements: [ADR-0039](0039-client-tab-workspace-scene-unification-and-reactive-surface-sync.md), [ADR-0041](0041-thread-command-purity-viewport-workspace-separation-and-tab-management-topology.md), [ADR-0042](0042-client-viewport-routing-ownership-tab-history-and-scene-self-projection.md), [ADR-0050](0050-authentic-thread-title-lifecycle-clean-scene-head-and-semantic-tab-projection.md)
- Amended by: [ADR-0104](0104-fork-client-projection-reconciliation-and-aside-retirement-completion.md) — client projection reconciled to the single-attach client; `/btw` redirect completed

---

## Context and Problem Statement

In early single-viewport terminal architectures (ADR-0017), long-running primary agent executions prevented operators from conducting side inquiries or quick exploratory checks without destroying active turn state. To address this, ADR-0017 established the `/btw` (Aside) subsystem—introducing an isolated side session, an independent `SideRegistry`, specialized wire events (`SideViewOpened`, `ExitSideView`, `CloseSide`, `BtwList`), an F5 modal overlay, and dedicated TUI scene heads.

With the subsequent ratification and implementation of the **Three-Tier Header Topology** (ADR-0040), **Thread Command Purity** (ADR-0041), and **Tab-Autonomous Scene Stacks** (ADR-0042), the foundational assumptions that necessitated `/btw` no longer hold:

1. **Subsumption under Causal History Forking**:
   A side query or non-polluting exploratory aside is conceptually nothing more than an ephemeral branch forked from a specific historical point in the dialogue causality graph.
2. **Accidental Architectural Complexity**:
   Maintaining a parallel aside registry (`SideRegistry`), bifurcated event streams, and aside-specific presentation modes introduces substantial code duplication and operational friction across `nuo-server`, `nuo-wire`, and `nuo-tui`.
3. **Inadequacy of Coarse Head-Only Forking**:
   The legacy `/fork` implementation only supported cloning at the latest session tip and mutated the current store in place, making it impossible to diverge from earlier decision points (e.g. Round 2 or Round 3) to explore alternative implementation paths or maintain side-by-side comparative tabs.

To eliminate legacy debt and build an uncompromised, modern foundation for tree-structured agent interactions, we must formally retire the `/btw` aside substrate and replace it with **Granular Causal Session Forking** projected into sovereign `ClientTab::Thread` viewports.

---

## Decision Drivers

- **Unification of Branching Semantics (`[INV-FORK-02]`)**: All divergent conversation paths—whether quick exploratory questions, scratchpads, or multi-directional development spikes—MUST be represented as first-class session forks derived from the causal history DAG.
- **Granular Temporal Addressing (`[INV-FORK-03]`)**: The `/fork` command MUST support fine-grained temporal targeting:
  - `/fork`: Branch from the latest settled round (Head).
  - `/fork <round>`: Branch from the conclusion of the specified round (e.g. `/fork 2`).
  - `/fork <round>.<turn>`: Branch from an exact intra-round turn boundary.
- **Sovereign ClientTab Projection (`[INV-FORK-04]`)**: Forking MUST NOT replace or destroy the parent session in place. The client viewport MUST mount the newly created child thread as a sovereign `ClientTab::Thread` on Row 1, preserving the parent tab and permitting seamless `Alt+1..9` switching between divergent branches.
- **Zero Legacy Burden (`[INV-DEPR-01]`)**: The `/btw` harness command, `SideRegistry`, aside-specific modal overlays, and associated wire request/response messages are retired. Educational guidance MUST non-destructively redirect legacy `/btw` habits to `/fork`.

---

## Considered Options

### Option 1: Retain `/btw` alongside Granular `/fork`
- Keep `/btw` for "lightweight" temporary questions and use `/fork` only for "heavy" permanent sessions.
- *Assessment*: Rejected per **[INV-AGENT-01]**. Having two distinct primitives for branching introduces cognitive ambiguity ("should I use /btw or /fork?"), doubles the wire contract surface, and maintains redundant session registries.

### Option 2: In-Place Timeline Switch within a Single Tab
- When `/fork <round>` is executed, swap the active session pointer within the existing tab instead of opening a new tab.
- *Assessment*: Rejected. In-place timeline swapping obscures the parent branch, prevents parallel multi-tab observation, and breaks the spatial clarity established by ADR-0042.

### Option 3: Granular Causal Forking with Sovereign Tab Projection (Chosen)
- Enhance `nuo-persistence` to slice transcripts and branch `SessionIR` DAGs at specified round/turn coordinates.
- Extend `/fork` to accept optional round/turn targets.
- Project forks directly into the `ClientTab` workspace, automatically mounting and activating the child tab.
- Formally deprecate and remove `/btw` and its supporting aside infrastructure.

---

## Rejected Alternatives & Negative Knowledge

Per **[INV-AGENT-01]**, the following alternative approaches were evaluated and discarded:

1. **Rejected: Blind In-Memory Transcript Truncation Without IR Delta Forking**
   - *Failure Mode*: Simply taking a subslice of serialized transcript entries while leaving the SQLite `SessionIR` or delta sequences unchanged produces dangling causal references and breaks future compaction passes.
2. **Rejected: Shared Filesystem Mutability Across Concurrent Coding Branches Without Safeguards**
   - *Failure Mode*: If two divergent threads simultaneously invoke file-writing tools (`edit_text`, `write_file`) on the identical workspace root, they can overwrite each other's edits. Granular branching isolates LLM dialogue context; physical filesystem divergence requires distinct commit checkpoints or operator consciousness.

---

## Decision Outcome

Chosen option: **Option 3**.

### 1. Granular Fork Addressing Specification

The `/fork` slash grammar is extended:
```text
/fork                  → Fork from current settled head
/fork <round>          → Fork at the settlement boundary of Round <round> (e.g. /fork 2)
/fork <round>.<turn>   → Fork after Turn <turn> within Round <round> (e.g. /fork 2.1)
```

### 2. Lineage and Persistence Topology

When forking at target boundary $(R, T)$:
1. `SessionStore::fork_at(...)` creates a new child session record with:
   - `id`: Fresh UUIDv4.
   - `parent_id`: ID of the source session.
   - `fork_kind`: `SessionForkKind::Fork`.
   - `round_counter`: Truncated to target round $R$.
   - `transcript`: Sliced to include only entries up to the specified boundary.
   - `SessionIR`: Forked via `ir.fork_at(...)` retaining the causal history path up to the target node.
2. The parent session store is untouched and remains fully operational.
3. The server emits `AgentResponse::SessionForked { child_id, parent_id, round, title }`.

### 3. Viewport & Tab Orchestration

Upon receipt of `AgentResponse::SessionForked`:
1. The TUI appends a new `ClientTab::thread(child_id, title)` to the tab workspace.
2. Focus transitions smoothly to the new tab, activating its composer for immediate branch interaction.
3. Operators switch back to parent or sibling branches via standard `Alt+1..9` chords or tab controls.

### 4. Retirement of Aside (`/btw`) Substrate

- `/btw` is removed from active slash command registries and command auto-completion.
- Submissions of `/btw <prompt>` are intercepted: the harness executes a granular fork at current head and dispatches `<prompt>` into the new branch tab, displaying educational guidance.
- The `SideRegistry` and aside-specific modals (`overlays/btw.rs`) are scheduled for comprehensive eradication.
