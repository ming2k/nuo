---
id: ADR-0231
title: "One Instruction Region, One Conversation, and No Ghost Extension Subsystem"
status: accepted
date: 2026-12-20
scope: protocol/instructions, protocol/capability, protocol/extension, engine/runtime, providers, tool/events
superseded_by: null
supersedes:
  - "`InstructionText::transient`"
  - "`InstructionText::render_fixed`"
  - "`InstructionText::render_transient`"
  - "`ContextPlan::transient`"
  - "`ContextPlanView::transient_tokens`"
  - "`Stability::Turn`"
  - "`Topic::TurnDirective`"
  - "`InputKey::TurnDirective`"
  - "`ModelRequest::temporary_context`"
  - "`ModelRequest::with_temporary_context`"
  - "`ModelRequest::wire_messages`"
  - "`CachePlan::temporary_context_messages`"
  - "`RequestTokenEstimate::temporary_context_tokens`"
  - "`RequestProjection::temporary_context`"
  - "`protocol::Extension`"
  - "`protocol::HookPhase`"
  - "`protocol::extension::HookOutcome`"
  - "`AgentRoleProfile::extensions`"
negative_knowledge: true
---

# 0231. One Instruction Region, One Conversation, and No Ghost Extension Subsystem

- **Status:** Accepted
- **Date:** 2026-12-20
- **Amends:** [ADR-0172](0172-request-plan-containers-are-regions-and-blocks-are-boundaries.md)
  (its four containers become two text regions plus two protocol fields),
  [ADR-0215](0215-context-sources-declare-moment-topic-rank-stability-and-budget.md)
  (`Stability` loses its unused band),
  [ADR-0229](0229-every-context-element-has-a-production-owner.md)
  (the declared reserve is now deleted, not reserved)
- **Related:** [ADR-0226](0226-one-renderer-and-one-reader-for-message-envelopes.md),
  [ADR-0216](0216-the-prompt-branches-on-declared-tool-capability-not-tool-names.md),
  [ADR-0093](0093-abolition-of-pseudo-vfs-spool-architecture-and-salience-folding.md)

## Context and Problem Statement

This is **one decision** — delete every extension point that has no producer —
with three instances. They are recorded together because they share one root
cause and one replacement map; they are not three independent decisions. Each
instance has its own vocabulary and its own break, so each is documented in its
own subsection below.

Three separate mechanisms claimed to carry "extra context for this request". None
of them had a producer.

### 1. `transient`: a text region nothing could write

`InstructionText` carried `fixed` and `transient`; `ContextPlan` carried `fixed`
and `transient`; `RequestBuilder` carried both; every provider branched:

```rust
let (static_text, ephemeral_text) = match (…) {
    (false, false) => format!("{static_text}\n{ephemeral_text}"),
    (false, true) => static_text, (true, false) => ephemeral_text, (true, true) => String::new(),
};
```

Five dialects carried that match. Both branches were always taken on an empty
string, because `transient` was reachable only from `Stability::Turn`, which was
reachable only from `InputKey::TurnDirective`, which no production source
declared. The region had been "reserved" (ADR-0229) precisely because deleting it
alone would leave `Stability::Turn` with no topic that reaches it.

### 2. `temporary_context` (`E_n`): a message tail produced by a ghost

`ModelRequest::temporary_context` was a per-request message tail, excluded from
the cacheable prefix and from the durable window. It had a documented producer:
`HookPhase::ProjectTemporaryContext`, run over `Agent::extensions()`.

That producer could not produce:

- The only production `Extension` implementation, `CodeIntelligenceExtension`,
  declared `hooks() -> &[]` and `run(..) -> HookOutcome::None`. Its "compatibility
  constructors" `new()` and `read_only()` returned the same unit struct.
- `HookPhase::InterceptFileMutation` was never fired: no production call site
  constructed `HookContext::mutation`.
- `Extension::tool()` had zero readers. A tool extension's tool was never
  installed; tools entered through `ToolSet`, a different subsystem.
- `ToolExtension` had no constructor call outside the protocol crate's own test.

So `temporary_context` was always empty, and every provider still carried the
`conversation.iter().chain(temporary_context.iter())` branch, `pressure` still
summed its tokens, `CachePlan` still counted its messages, `RequestProjection`
still archived it, and two CLI overlays still rendered a "Temporary 0 tokens"
row.

### 3. `Extension`: a primitive with one degenerate implementation

`Extension` was documented as "one primitive unifying tools and ambient harness
facets... the sole capability unit". In practice it was a third registry beside
`ToolSet` (which owns tools) and `HookRegistry` (`[[hooks]]`, which owns lifecycle
events and is configured from disk). Its only instance did nothing, and its
extensibility surface (`with_extension`, `add_extension`) was wired into role
profiles, subagent construction, and a slash command for the purpose of adding
that nothing.

Three mechanisms, one zone. Each was a plausible extension point, each was
documented as supported, and each was empty in production. The aggregate cost was
not the dead code — it was that **the documentation described a system that did
not exist**, and three separate call sites had to decide what to pass through a
parameter that could only ever be empty.

## Decision

### 1. One instruction region

```rust
#[serde(transparent)]
pub struct InstructionText(pub String);
```

- `ContextPlan` carries `fixed: Vec<Block>` and `fixed_tokens` only.
- `RequestBuilder` / `RequestContext` carry `instructions: Vec<Block>`.
- `RequestTokenEstimate`, `ContextTokenSnapshot`, `CachePlan`, and
  `RequestProjection` lose every `temporary_*` field.
- `/context` and the telemetry panel lose their "Transient tail" and "Temporary"
  rows.

### 2. One conversation

`ModelRequest` carries `messages` and nothing else that is a message.
`wire_messages()` — a `messages.chain(temporary_context)` iterator with no
readers — is deleted with the field it existed for.

Every provider's degraded path keeps working unchanged: a caller that needs a
message the route cannot express uses the tool-result header (ADR-0227) inside
`messages`, which is a protocol field, not a second channel.

### 3. Delete the extension subsystem

`protocol::Extension`, `protocol::HookPhase`, `protocol::extension::HookOutcome`,
`extension::HookContext`, `ToolExtension`, `CodeIntelligenceExtension`,
`AgentRoleProfile::extensions`, `AgentBuilder::{with_extension, with_extensions}`,
`Agent::{extensions, add_extension}`, `AgentControlState::extensions`, the
temporary-context byte budget, its truncating wrapper, and the facet loop that
fed them are removed.

The surviving paths are unchanged. Three are **capability entrances**; two are
**data owners**:

| Concern | Owner | Kind |
| :--- | :--- | :--- |
| Model-callable capability | `ToolSet` / `ToolManager` (one registry) | entrance |
| Lifecycle observation and injection | `HookRegistry` (`[[hooks]]`, ADR-0025) | entrance |
| Standing prompt text | declared `ContextSource` (ADR-0215) | entrance |
| Durable conversation | `messages` / `ThreadFact` | data owner |
| Request-local data | a hidden message with its own `InjectionKind` (ADR-0226) | data owner |

A fourth **capability entrance** is a defect. A new **data owner** is a different
question: it must name the protocol field it replaces and the cache semantics it
preserves. The two are not interchangeable, and conflating them is how
`temporary_context` became a second instruction channel with no producer.

The deletion also leaves a principle: a trait whose only production
implementation is a unit struct with defaulted methods is deleted rather than
kept "for future producers". It is a design principle, not an invariant: no test
can prove that a future trait will not be degenerate. The evidence is the
deletion, and the rule is stated here so the next proposal argues with it.

### 4. Delete the `Turn` band and its topic

`Stability::Turn`, `Topic::TurnDirective`, `InputKey::TurnDirective`, and
`Stability::is_transient` are removed. `Stability` is now exactly the bands the
cache can distinguish: `Frozen`, `Route`, `Session`, `Workspace`. `Topic::ALL`
has no unowned member, so `[INV-CTXTOPIC-01]`'s exception list is empty.

## Invariants

- **`[INV-REGION-01]` One Instruction Region** — a model request has exactly one
  instruction text and exactly one message sequence. A third text or message
  channel is a design defect, not a feature.
- **`[INV-REGION-02]` No Empty Branch** — a provider may not branch on a region
  that no producer can fill. Enforced by the absence of the field: there is
  nothing to chain.
- **`[INV-CAPABILITY-01]` One Path Per Concern** — a model-callable capability
  enters through `ToolSet`; a lifecycle injection enters through `HookRegistry`; a
  standing rule enters through a `ContextSource`. There is no fourth capability
  entrance.
- **`[INV-STABILITY-01]` No Reserved Band** — a `Stability` variant exists only if
  a `Topic` reaches it.

## Consequences & Trade-offs

- **Positive**: five provider branches collapse to one field; ~60 `E_n` threading
  sites across protocol, providers, pressure, cache, projection, CLI, and
  telemetry become zero; the `Extension` trait, its two builders, its role-profile
  field, its subagent wiring, and its slash-command wiring disappear; the
  explanation doc no longer promises a transient region.
- **Negative & incurred**: `InstructionText` is a newtype, so `text.fixed` becomes
  `text.0` / `text.text()`. Every provider's `body_with_capabilities` loses its
  second argument. `ContextPlan`/`ContextPlanView` lose a field each, which is a
  hard break on the wire form (ADR-0220) — accepted because the field was always
  zero and a peer that reads a zero is reading a lie.
- **Negative & accepted**: an embedding that implemented `protocol::Extension` must
  move to `HookRegistry` (lifecycle) or `ToolSet` (capability). No such
  implementation exists in-tree, and the trait was not usable for either purpose:
  an extension's tool was never installed.
- **Neutral**: all fourteen default context sources, the instruction ceiling, the
  eviction ladder, `[INV-CTXSRC-04]` placement, and every rank/stability rule are
  unchanged.

## Discarded Alternatives

### A — Keep `transient` and implement a real producer

- **Why considered**: the region is protocol vocabulary; deleting it shrinks the
  contract.
- **Why rejected**: the only candidate payload — a facet's projection — is
  request-local, non-durable, and already correctly expressed as a hidden message
  with an `InjectionKind`. Putting it in a second instruction text would give the
  model two instruction channels and no rule for which wins.

### B — Keep `Extension` and delete only its plumbing

- **Why considered**: a smaller break.
- **Why rejected**: a trait with no producer and no reader is not made honest by
  keeping it unplumbed; it is made honest by deletion. The two real paths already
  exist and are named.

### C — Keep `temporary_context` for a future embedder

- **Why considered**: an embedder might want a request-scoped tail.
- **Why rejected**: `messages` already is that, with provenance, ordering, cache
  semantics, and a renderer. A second channel would need its own semantics for all
  four.

## Compliance and Verification

- `crates/protocol/src/instructions.rs::instruction_text_is_one_region`.
- `crates/protocol/src/request_builder.rs::tools_and_messages_are_moved_once_and_never_rendered_as_text`.
- `tests/integration/engine/it/context_prefix.rs` — the golden pins one
  `fixed_tokens` and no transient row.
- `tests/integration/governance/context_engineering.rs::inv_ctxtopic_01_*` — no
  unowned topic, with an empty exception list.
- `crates/provider-openai/src/responses/request/tests.rs::no_dialect_emits_a_second_instruction_item`.
