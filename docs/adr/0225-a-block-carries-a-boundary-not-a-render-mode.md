---
id: ADR-0225
title: "A Block Carries a Boundary, Not a Render Mode"
status: accepted
date: 2026-12-20
scope: protocol/instructions, engine/cognition/context, prompt-policy
superseded_by: null
supersedes:
  - "`BlockRender`"
  - "`Block::render`"
  - "`Block::with_render`"
negative_knowledge: true
---

# 0225. A Block Carries a Boundary, Not a Render Mode

- **Status:** Accepted
- **Date:** 2026-12-20
- **Amends:** [ADR-0172](0172-request-plan-containers-are-regions-and-blocks-are-boundaries.md)
  (the decision is unchanged; the code now spells it)
- **Related:** [ADR-0215](0215-context-sources-declare-moment-topic-rank-stability-and-budget.md)

## Context and Problem Statement

ADR-0172 decided that *"a block is text plus a **boundary choice**"* and named the
section that way. The code then shipped:

```rust
pub struct Block { …, pub render: BlockRender }
pub enum BlockRender { Xml(&'static str), Plain }
```

Three defects, all of them properties of those two names:

1. **The field was named for its output, not its role.** `render` is a verb the
   *renderer* performs. A field that tells the renderer what to do is an
   instruction, and calling it `render` made the instruction indistinguishable
   from the result. `Block` does not even derive `Serialize`, so the field never
   appears in any output: `/context --json` carries the bytes `render_blocks`
   produced, never this flag. The name described something the type does not do.
2. **`Xml` claimed a format the renderer does not produce.** `render_blocks`
   writes no declaration, escapes no body, supports no nesting, and handles no
   CDATA. It emits a **tag pair**. A maintainer who read `Xml` reasonably assumed
   the bytes were XML and therefore safe, which is how the unescaped-body hole in
   the skill and file envelopes (ADR-0226) survived review.
3. **`Plain` contradicted the orthogonality ADR-0172 established.** `Plain` reads
   as "this body is not Markdown", but RFC-0002 §6 rejects exactly that reading:
   body syntax and boundary choice are orthogonal, and a Markdown document inside
   a tagged boundary is the normal case.

The vocabulary in the ADR and the vocabulary in the code had diverged, and the
divergence hid a real defect. This ADR removes the divergence.

## Why This Is an ADR

The change is not a style-only edit. It meets two of the three qualification
tests in [`docs/dev/standards/adr.md`](../dev/standards/adr.md) §1:

- **Public protocol surface**: `Block` is the instruction-region type every
  provider projects. Renaming its field, enum, and constructor is a breaking API
  change for every embedder that builds a request plan.
- **Binding red-line**: the old `Xml` variant claimed escaping the renderer never
  performed, and that false claim hid a real unescaped-body defect. The
  replacement name is part of the fix, not a preference.

It does not meet the high-reversal-cost test: the rename itself is mechanical.
The decision is recorded because the vocabulary is a contract, not because the
edit is large.

## Decision

### 1. `Block::boundary: Boundary`

```rust
pub enum Boundary {
    /// `<tag source="…">body</tag>`
    Tagged(&'static str),
    /// The title line and body alone.
    Bare,
}
```

- `Tagged` replaces `Xml`: it names what is actually emitted.
- `Bare` replaces `Plain`: it names the absence of a boundary and says nothing
  about the body's syntax.
- `with_render` becomes `with_boundary`.

The naming rule itself is a style rule, not an invariant:
[`docs/dev/standards/style.md`](../dev/standards/style.md) §4 states it. No
compiler or test can prove a name. The testable properties are the two
invariants below: a boundary is not serialized (`[INV-BOUNDARY-02]`), and a
body is emitted verbatim (`[INV-BOUNDARY-03]`).

### 2. `render` is reserved for the step that produces the bytes

`render_blocks` keeps its name: it does produce the final text. A type that
merely *chooses* a presentation may not be called `render`.

### 3. A boundary is not a security control

`Tagged` makes a region legible to the model and machine-readable to the harness.
It cannot make hostile bytes safe: `render_blocks` emits bodies verbatim, because
escaping a body would corrupt the asset. Untrusted content therefore never travels
in the instruction region at all (`[INV-CTXSRC-04]`); it travels in a message with
the untrusted framing (ADR-0228). This is now stated at the renderer, where the
next reader will look.

### 4. The tag vocabulary stays static and curated

`Boundary::Tagged(&'static str)` keeps the tag set **static**: the tag is a
source-level literal, never a runtime value. A dynamic identity (a skill name, an
MCP server) can only travel in an **attribute**. A tag chosen by untrusted input
would let that input choose which region of the prompt it lands in.

This is not an enum, and the type does not prove the vocabulary is closed. The
vocabulary is curated at the renderer, where a new tag is a source edit reviewed
with the block it opens. An enum in `protocol` would move that review into the
wire crate and couple it to every engine source; see Alternative C.

## Invariants

- **`[INV-BOUNDARY-02]` A Boundary Is Not Serialized** — `Block` carries no
  boundary on the wire. `/context --json` can only show rendered bytes, so the
  inspector cannot disagree with the prompt ([`INV-CTXPLAN-01]`).
- **`[INV-BOUNDARY-03]` Verbatim Body** — `render_blocks` does not escape a body.
  Its doc comment states the resulting constraint, and no untrusted body may be
  placed in a tagged instruction block.

## Consequences & Trade-offs

- **Positive**: code, ADR title, and behaviour now use one word for one concept;
  `Tagged` no longer implies escaping was performed; the escape hole that ADR-0226
  closes is discoverable from the renderer's own doc comment.
- **Negative & incurred**: a mechanical rename across the fourteen default sources
  and their tests. No wire break — `Block` is not serialized and the `/context
  --json` golden pins bodies, not field names.
- **Neutral**: `InstanceText`/`InstructionText` and the provider projections are
  untouched.

## Discarded Alternatives

### A — Keep `render` / `BlockRender` and document the distinction

- **Why considered**: smallest diff, no public break.
- **Why rejected**: the name still describes the renderer's action, and `Xml`
  still claims escaping the renderer does not perform. The false claim is what
  hid the unescaped-body defect in the first place.

### B — Rename only `Xml` → `Tagged`, keep `render`

- **Why considered**: fixes the format claim with one edit.
- **Why rejected**: `render` remains a verb the renderer performs; the field
  still reads as an instruction/result confusion, and the next reader still has
  to learn which of the two `render`s is the producer.

### C — Make the tag a closed enum in `protocol`

- **Why considered**: a true compile-time closed set, and a typo becomes a
  compile error.
- **Why rejected**: the tag vocabulary is a source-level convention. An enum in
  `protocol` would couple the wire type to every engine source and force a
  protocol edit for a new source. `&'static str` already excludes runtime input,
  which is the security property that matters.

## Validation

- `protocol::instructions::tests::render_blocks_is_deterministic_and_owns_presentation`
  (uses `Boundary::Tagged` / `Boundary::Bare`).
- `tests/integration/engine/it/context_prefix::the_plan_and_the_request_are_the_same_bytes`
  — the golden's bodies still equal the cacheable prefix byte for byte.
- `crates/tool/src/envelope.rs` unit suite — the message side of the same question.
