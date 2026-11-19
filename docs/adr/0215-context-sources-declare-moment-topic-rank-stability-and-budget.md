---
id: ADR-0215
title: "Context Sources Declare Their Moment, Topic, Rank, Stability, and Budget"
status: accepted
date: 2026-12-19
scope: protocol/context, engine/cognition, prompt-policy
superseded_by: null
supersedes:
  - "`InstructionSection::is_transient`"
  - "`InstructionContext` as an open struct of `pub` fields"
  - "the `cognition::budget` module"
negative_knowledge: true
---

# 0215. Context Sources Declare Their Moment, Topic, Rank, Stability, and Budget

- **Status**: Accepted
- **Date**: 2026-12-19
- **Amends**: [ADR-0172](0172-request-plan-containers-are-regions-and-blocks-are-boundaries.md)
  (the container is still the region; `Stability` adds what membership cannot
  carry), [ADR-0199](0199-the-default-agent-has-a-coding-baseline-and-skill-catalog.md)
  (the catalog reports its own elision instead of truncating),
  [ADR-0214](0214-context-thresholds-follow-the-model-window-and-are-not-user-configuration.md)
  (one more model-derived threshold: the instruction ceiling)
- **Supersedes**: `InstructionSection::is_transient`,
  `InstructionContext` as an open struct of `pub` fields, and the
  `nuo_engine::cognition::budget` module
- **Derives from**: [RFC-0004](../rfc/0004-context-sources-declaration-budget-and-ownership.md)

## Context and Problem Statement

The request assembler is a good pipeline with no declaration surface. A section
declares an id, an order, and a body; it does not declare what it is *about*,
who else may say the same thing, how much it may cost, or how long its bytes stay
valid. Every property the assembler would need in order to reason about the
prompt is therefore missing, and five defects followed:

1. **Semantic duplication is invisible.** `agent.coding_baseline` and
   `workspace.topology` both carry the native-tool preference and the
   parallelization rule, in near-identical wording. The registry checks `id`
   uniqueness only, so no test can see it.
2. **`fixed` is not actually stable.** It mixes compiled-in contracts with
   blocks derived from workspace files, and nothing declares which is which. A
   single `AGENTS.md` edit invalidates the provider's cache from the middle of
   the prefix onward, and nothing reports that it happened.
3. **Admission is unbounded while eviction is disciplined.** Tool-result pruning
   names what it dropped and keeps a handle
   (`protocol::pressure::cleared_placeholder_with_reason`). The skill catalog
   silently drops everything past 64 entries; project rules had no bound at all.
   The one bound that existed was denominated in **bytes**, in a codebase whose
   own pressure module states that every context budget is token-denominated.
4. **A layout error panics on the request path.** Anchors and cycles are
   validated inside `build_regions`, called from `assemble_prepared`, which turns
   the error into `panic!("invalid instruction layout")`. A third-party section
   with one mistyped id takes down the server from inside a request.
5. **Dead context substrate survives.** `cognition::budget` declared a second,
   unreferenced budget vocabulary with hardcoded 60%/75% watermarks while the
   live policy derived its thresholds from the model window. A dead `pub` type is
   invisible to `dead_code`.

Each is a missing proof obligation on one declaration. Adding a lint per symptom
does not generalize; the declaration is what was missing.

## Architectural Decisions

### 1. One declaration carries five facts

A context element names, in one place:

| Fact | Type | What it decides |
| :--- | :--- | :--- |
| Moment | documentation + module placement | which lifecycle hook may create it |
| Topic | `protocol::context::Topic` | ownership; one active owner |
| Rank | derived from `Topic` | placement: directive text, reference text, or untrusted data |
| Stability | derived from declared inputs | region, band order, cache claim |
| Budget | `protocol::context::Budget` | size bound and overflow behaviour |

A source that cannot state all five is not a context element.

### 2. `Stability` replaces `is_transient` and orders the prefix

```rust
pub enum Stability { Frozen, Route, Session, Workspace, Turn }
```

- **Region**: `Turn` is the `transient` region; everything else is `fixed`.
  `is_transient()` is deleted, so "transient but frozen" is unrepresentable.
- **Order**: within the body, blocks sort by ascending `Stability`, then by
  declared order. A workspace edit therefore invalidates only the `Workspace`
  suffix.
- **Cache claim**: the assembler can state what a re-render should cost, which is
  what makes prefix drift reportable ([ADR-0217](0217-context-projection-is-inspectable-and-prefix-drift-is-reported.md)).

`Stability` is *derived* from the source's declared inputs, never claimed.
A source cannot assert `Frozen` while reading a mutable workspace file.

### 3. `Position` separates the frame from the body

Identity and the conflict-resolution contract are not "the most stable blocks";
they are what the request *is*. They are emitted first, in topic order, and the
body follows in stability bands.

```rust
pub enum Position { Frame, Body }
```

`Position` is derived from `Topic` (`Identity`, `Precedence` are `Frame`). A frame
block may declare only `Route`-or-more-stable inputs; anything else is
`FrameVolatility`, because a volatile frame block would invalidate the entire
prefix on every workspace edit.

This is why `agent.precedence` becomes **unconditional**. Making the arbitration
contract appear only when a policy file happens to exist is a false economy: the
model should always know how conflicting instructions resolve, and the block is
under 100 tokens.

### 4. `Topic` is a closed vocabulary with one active owner

Two active sources claiming one topic is `DuplicateTopic` unless one names the
other in `supersedes`. This is [ADR-0179](0179-one-capability-one-producer-and-no-ghost-builtins.md)
and [ADR-0180](0180-single-ownership-of-shared-vocabulary.md) applied to prompt
content, which was the last shared vocabulary in the repository without an owner.

`Topic` is closed and lives in `nuo-protocol` because embedders construct sources
through the public builder API. Adding a topic is a deliberate edit to the
contract crate, which is the review gate: new prompt text must either join an
existing topic or justify a new one.

### 5. `Budget` is mandatory, token-denominated, and always disclosed

Every source declares a token budget and an `Overflow` policy. `Elide` and
`Index` carry a `Resume` — the model-visible route back to the dropped bytes
(a file offset, a tool, an `@` mention, or `Operator` when no in-workspace route
exists and a human must shorten the source).

`ContextBudget` gains `instructions_ceiling_tokens`, derived from the model
window like every other threshold ([ADR-0214](0214-context-thresholds-follow-the-model-window-and-are-not-user-configuration.md)).
It is the ceiling for `fixed` + `transient` together. Without it, a large
`AGENTS.md` plus a large skill catalog can consume the window before the
conversation starts, and every downstream budget inherits the error. When the
region exceeds the ceiling, the assembler drops blocks in reverse stability order
(most volatile first), emitting one notice per drop.

### 6. Compile once, then be infallible

`Policy::compile` proves ordering, ownership, supersede resolution, frame
volatility, rank placement, and non-empty budgets. `Policy::plan` is total: no
`Result`, no `panic!`.

The two `panic!("invalid instruction layout")` sites become unreachable because
the only route to a `Policy` is `compile`. Validation moves from per-request to
per-agent-construction, and a broken policy fails agent construction loudly with
the offending ids instead of failing a user's request.

The render memo keys on `(inputs_hash, policy_generation)`; the generation
counter cannot be forgotten the way a manual `invalidate_memo` call can.

### 7. `ContextInputs` records what each field *is*

`InstructionContext` becomes `ContextInputs`, and each field declares a static
`InputKey` with a stability. `InputKey::stability()` is a property of the key, not
of the value: project rules are `Workspace`-stable whether or not they are
present.

`tool_names: Vec<String>` is replaced by `ToolSurface`, for the reason recorded in
[ADR-0216](0216-the-prompt-branches-on-declared-tool-capability-not-tool-names.md).

## Normative Invariants

- **`[INV-CTXSRC-01]` Single Topic Owner** — At most one active source per
  `Topic`; a same-topic pair must declare `supersedes` in at least one direction.
  Enforced by `ContextPolicyError::DuplicateTopic` and a unit test over the
  default policy.
- **`[INV-CTXSRC-02]` Declared Budget** — Every source declares a token budget and
  an overflow policy; the assembler enforces both, and the instruction region is
  bounded by `ContextBudget::instructions_ceiling_tokens`.
- **`[INV-CTXSRC-03]` Derived Stability** — A source's stability is the maximum
  stability of the inputs it declares. A source cannot claim more stability than
  its inputs have.
- **`[INV-CTXSRC-04]` Rank Is Placement** — `Rank::Untrusted` never enters
  `fixed`; `Rank::Directive` never becomes a conversation message.
- **`[INV-CTXSRC-05]` Infallible Request Path** — Policy validation happens in
  `compile`; no request-path function may panic on a layout condition.
- **`[INV-CTXSRC-06]` No Dead Context Declaration** — Every `ContextInputs`
  accessor has a production reader, and every `cognition::context` submodule is
  reachable from a production path. Extends and supersedes `[INV-CONTEXT-06]`.
- **`[INV-CTXSRC-07]` One Budget Vocabulary** — Context thresholds and watermark
  fractions exist only in `protocol::{pressure,tuning}`. A numeric watermark
  elsewhere is a governance failure.
- **`[INV-CTXSRC-08]` Purity** — A `render` or `is_active` implementation reads
  `ContextInputs` only: no clock, no filesystem, no network, no interior
  mutability. The memo key is the inputs hash, so an impure read silently serves a
  stale body.

## Consequences & Trade-offs

- **Positive**: duplicated prompt text becomes a compile error instead of a
  review finding; the largest stable blocks stop being invalidated by a workspace
  edit; every elision names its recovery path; a broken layout fails construction
  rather than a request; the prompt becomes inspectable per block
  ([ADR-0217](0217-context-projection-is-inspectable-and-prefix-drift-is-reported.md)).
- **Negative & Incurred Costs**: the default prompt order changes once (frame
  first, then stability bands), so the golden test moves in one reviewed diff.
  Every source must now declare a budget, which is friction on the first new
  source and the point of the decision. `cognition::context` is a new module tree
  with five files where one existed.
- **Neutral**: `Block`, `BlockRender`, `render_blocks`, `RequestBuilder`, and the
  two request regions are unchanged. Section ids are unchanged, so
  `disable_instruction_section("project.rules")` keeps working.

## Discarded Alternatives (Negative Knowledge)

### Alternative A — Keep `is_transient` and make it work

- **Why considered**: One boolean, no new vocabulary.
- **Why rejected**: It encodes one bit of what `Stability` encodes, cannot express
  a cache claim, and can contradict the band its inputs imply. Deriving the
  region removes an illegal state instead of testing for it.

### Alternative B — A `stability()` method the source declares

- **Why considered**: Simpler than deriving from inputs; each source knows
  itself.
- **Why rejected**: A source would be free to claim `Frozen` while reading a
  workspace file, which is exactly the defect being fixed. Derivation makes the
  claim unfalsifiable.

### Alternative C — Open `&'static str` topics

- **Why considered**: Cheaper to add a topic; no contract-crate edit.
- **Why rejected**: Recreates the duplication defect with different spellings
  (`tool.discipline` vs `tool_discipline`), which `[INV-VOCAB-02]` already
  rejects for types.

### Alternative D — A per-source `rank()` method

- **Why considered**: Some sources legitimately blend directive and reference
  content.
- **Why rejected**: It lets a `Directive` be relabelled `Reference` to reach a
  message channel, and vice versa. Rank is a property of the topic, so the
  mapping is reviewable in one place.

### Alternative E — A default `budget()` on the trait

- **Why considered**: Less friction for a new source.
- **Why rejected**: A default is inherited by every future source, and it is
  either unbounded (recreating the defect) or arbitrary (silently truncating a
  contract).

### Alternative F — Keep the request-path panic and catch it

- **Why considered**: Smallest diff.
- **Why rejected**: Turns a configuration error into a request-time failure with a
  worse diagnostic and no id.

### Alternative G — A separate `nuo-context` crate

- **Why considered**: Isolation.
- **Why rejected**: `is_active` reads live turn state, so the registry belongs
  below `Agent` but inside `engine`. A crate boundary would add a trait-object hop
  per block for no isolation gain, and `protocol` already owns the vocabulary.

### Alternative H — Order by declared anchors only, keeping `Stability` for cache accounting

- **Why considered**: Preserves the existing semantic order exactly.
- **Why rejected**: Then a workspace edit still invalidates from the middle of the
  prefix, and the cache claim is decorative. Stability ordering is the mechanism,
  not a side effect.

## Compliance and Verification

- `crates/engine/src/cognition/context/tests.rs` — duplicate topic, supersede
  cycle, frame volatility, rank placement, budget enforcement, derived stability.
- `tests/integration/engine/it/system_prompt_builder.rs` — embedding API and the
  frame-first default order.
- `tests/integration/governance/` — `[INV-CTXSRC-06]` (no dead input, no dead
  submodule) and `[INV-CTXSRC-07]` (one budget vocabulary).
- `nuo context --json` golden pins every default block's bytes and token cost.

## Links

- [RFC-0004](../rfc/0004-context-sources-declaration-budget-and-ownership.md)
- [ADR-0052](0052-unified-instruction-pipeline-pure-role-topology-and-end-to-end-cache-fidelity.md)
- [ADR-0153](0153-effective-input-ceiling-tool-output-retention-and-ingest-image-normalization.md)
- [ADR-0164](0164-instructions-are-one-pipeline-over-system-and-project-sources.md)
- [ADR-0172](0172-request-plan-containers-are-regions-and-blocks-are-boundaries.md)
- [ADR-0179](0179-one-capability-one-producer-and-no-ghost-builtins.md)
- [ADR-0180](0180-single-ownership-of-shared-vocabulary.md)
- [ADR-0199](0199-the-default-agent-has-a-coding-baseline-and-skill-catalog.md)
- [ADR-0214](0214-context-thresholds-follow-the-model-window-and-are-not-user-configuration.md)
- [ADR-0216](0216-the-prompt-branches-on-declared-tool-capability-not-tool-names.md)
- [ADR-0217](0217-context-projection-is-inspectable-and-prefix-drift-is-reported.md)
