---
id: ADR-0229
title: "Every Context Element Has a Production Owner"
status: accepted
date: 2026-12-20
scope: protocol/context, engine/cognition/context
superseded_by: null
supersedes:
  - "`Topic::Delegation`"
  - "`Topic::Memory`"
  - "`Topic::Retrieval`"
  - "`ToolSurface::spawns_subagent`"
  - "`ToolSurface::deferred_active`"
  - "`ToolActivation::len`"
negative_knowledge: true
---

# 0229. Every Context Element Has a Production Owner

- **Status:** Accepted
- **Date:** 2026-12-20
- **Amends:** [ADR-0215](0215-context-sources-declare-moment-topic-rank-stability-and-budget.md)
  (the declaration surface shrinks and gains an exhaustiveness proof; the
  five-fact rule stands)
- **Related:** [ADR-0216](0216-the-prompt-branches-on-declared-tool-capability-not-tool-names.md),
  [ADR-0208](0208-tool-contract-validation-snapshots-and-deferred-discovery.md),
  [ADR-0231](0231-one-instruction-region-and-no-ghost-extension-subsystem.md)
- **Amended by:** [ADR-0231](0231-one-instruction-region-and-no-ghost-extension-subsystem.md)
  — `Topic::TurnDirective` is deleted, not reserved; the "no reserved band" rule is now `[INV-STABILITY-01]`.

## Context and Problem Statement

ADR-0215 §4 made `Topic` a *closed* vocabulary so that two sources could not own
one concern. The closure was enforced; the membership was not. Three topics were
declared, ranked, and never claimed:

| Topic | Rank | Owner |
| :--- | :--- | :--- |
| `Delegation` | Directive | none |
| `Memory` | Reference | none |
| `Retrieval` | Untrusted | none |

`ToolSurface` carried two more unread facts, and `ToolActivation` a third:

- `spawns_subagent` — assigned from `Tool::spawns_subagent()` and read only by a
  test. The framework's real recursion guard reads the tool itself
  (`protocol::subagent`), never the surface.
- `deferred_active` — a counter threaded through `PartialEq`/`Hash` *exclusions*
  so that activation could not move the prefix. It was a field whose entire
  implementation existed to negate itself, and `ToolActivation::len()` was
  `pub(crate)` with zero callers.

The consequences were architectural, not cosmetic:

- The `[INV-CTXSRC-01]` test could not see it.
  `no_two_default_sources_share_a_topic` asserts *no topic has two owners*.
  It says nothing about a topic with zero, so three dead topics passed it for the
  life of the design.
- **A field existed only to be ignored.** `deferred_active`'s `PartialEq`/`Hash`
  impls were prose explaining why the field does not count. Deleting the field
  deletes the explanation.

A fourth, larger case — `Topic::TurnDirective` and the whole `transient` region —
turned out to have no producer either, and is resolved by deletion in ADR-0231
rather than by reservation.

ADR-0231 also split one tag that this ADR had shared: the "no reserved band" rule
is `[INV-STABILITY-01]`, and `[INV-CTXTOPIC-02]` here remains the input-key rule.
Two rules, two tags.

## Decision

### 1. Delete the unreachable members

`Topic::{Delegation, Memory, Retrieval}`, `ToolSurface::spawns_subagent`,
`ToolSurface::deferred_active`, `ToolSurface::new`'s third parameter, and
`ToolActivation::len` are removed. `ToolSurface` is now exactly what its name
says: a declared capability set plus the channel's parallel support, with derived
`PartialEq`/`Eq`/`Hash` and no exclusion comment.

### 2. The ownership test becomes exhaustive

`no_two_default_sources_share_a_topic` (renamed: the old name asserted something
weaker than its claim) is joined by two scans:

- every `Topic::ALL` member has a production source;
- every `InputKey::ALL` member is declared by one.

The second scan proves **declaration**, not that a `ContextInputs` field reads
the key. That half is review-enforced; the invariant below is named for what the
gate actually proves.

There is **no exception list**. A zero-owner topic or key is a test failure. If a
future design wants reserved vocabulary, it must arrive with a producer — ADR-0231
records why the last reservation was withdrawn.

### 3. A field that exists to be ignored is deleted

The rule this ADR encodes: if a struct field's only implementation is an argument
that it should not count, the field is the defect. Identity and diagnostics are
different values; a capability set does not carry a counter.

## Invariants

- **`[INV-CTXTOPIC-01]` No Unclaimed Topic** — every `Topic::ALL` member is claimed
  by exactly one production source.
- **`[INV-CTXTOPIC-02]` No Undeclared Input Key** — every `InputKey::ALL` member
  is declared by at least one production source. A key with no `ContextInputs`
  field that reads it is a review defect, not a gate failure.
- **`[INV-CTXTOPIC-03]` Identity Is Not a Counter** — a value used as a memo key
  carries no diagnostic field and no "excluded from equality" comment.

## Consequences & Trade-offs

- **Positive**: `/context` no longer lists concepts nothing can produce; the
  ceiling and purity checks cover only reachable code; `ToolSurface` has no field
  whose purpose is to be ignored.
- **Negative & incurred**: an embedder that constructed a source with
  `Delegation`/`Memory`/`Retrieval`, or passed the third `ToolSurface::new`
  argument, must update. Those names never had a working producer, so the affected
  code was test scaffolding and one embedding test that now reuses a topic freed by
  disabling its default owner.
- **Neutral**: `Rank`, `Position`, `Stability`, and the five-fact rule are
  unchanged.

## Discarded Alternatives

### A — Mark them `Reserved` and keep them

- **Why considered**: no break for embedders.
- **Why rejected**: a reserved topic is a promise the framework does not keep. The
  one reservation that survived this ADR (`TurnDirective`) was withdrawn in
  ADR-0231 for exactly this reason: three call sites had to thread a value that
  could only be empty.

### B — Implement a source for each topic now

- **Why considered**: makes the declarations true.
- **Why rejected**: there is no producer with a real payload. Inventing a source
  to make a topic non-empty is the same defect one layer over.

## Validation

- `crates/protocol/src/context/mod.rs` — `Topic::ALL`; label and position tests
  iterate every member. `rank()` and `stability()` are total matches, so a new
  member without an arm does not compile.
- `crates/engine/src/cognition/context/tests.rs` —
  `no_two_default_sources_share_a_topic`, `stability_is_derived_from_the_declared_inputs`.
- `crates/engine/src/cognition/context/inputs.rs::the_surface_identity_is_a_capability_set`.
- `tests/integration/governance/context_engineering.rs` —
  `inv_ctxtopic_01_every_declared_topic_and_key_has_a_producer`.
