---
id: ADR-0172
title: "Request Plan: Containers Are Regions, Blocks Are Boundary Choices"
status: accepted
date: 2026-11-13
scope: protocol/instructions, engine/model-request, providers/request, persistence/session-ir
superseded_by: null
supersedes: [ADR-0052]
negative_knowledge: true
---

# 0172. Request Plan: Containers Are Regions, Blocks Are Boundary Choices

- Status: Accepted
- Date: 2026-11-13
- Deciders: Nuo Architecture Working Group
- Supersedes: [ADR-0052](0052-unified-instruction-pipeline-pure-role-topology-and-end-to-end-cache-fidelity.md) (the four-tier `Base / Session / Task / Ephemeral` taxonomy and the `system.*` section namespace)
- Related: [ADR-0064](0064-orthogonal-agent-tool-surface-meta-tool-pruning-and-mutation-topology.md), [ADR-0130](0130-model-guidance-dynamic-prompt-engineering-and-pure-cognitive-demarcation.md), [ADR-0165](0165-user-instructions-absorb-model-guidance-as-the-user-source.md), [RFC-0002](../rfc/0002-request-plan-and-wire-projection.md)

## Context

The four-tier instruction model (`Base / Session / Task / Ephemeral`) promised
cache segmentation that the providers never received:

- `InstructionBundle::render_system_instructions()` concatenated `Base`,
  `Session`, and `Task` into one static string; only `Ephemeral` was emitted
  separately.
- `Task` had exactly three producers, all one-shot cognitive calls. Its
  documented consumer — subagent task framing — used the child agent's `Base`
  identity instead. It was a label, not a lifetime.
- A flat `ContentKind` proposal mixed two axes: `Tool` / `Message` describe wire
  shape, while `Policy` / `Reference` / `Context` describe text semantics. It
  made invalid combinations representable and turned every new source into an
  enum change.
- Sections embedded their own `\n# Heading` and leading newlines, so ordering
  and byte stability were distributed across the section table.
- `system.*` was a transport-era namespace: the domain abstraction is a
  request, and providers place instructions in system blocks, top-level
  `instructions`, developer items, or a system-role message.

The product problem is concrete: cache hits depend on a byte-stable prefix, but
the old model could not say which part of the request was fixed, which part grew
with the conversation, and which part was request-local.

## Decision Drivers

- **One lifetime axis, one content axis.** Region answers “how long does it
  live”; shape answers “how is it serialized”.
- **No duplicated information.** Tools and messages are protocol fields and must
  never also appear as prompt text.
- **Deterministic bytes.** The fixed region must be byte-identical for a route.
- **Simple blocks.** A block is a fragment of text plus a boundary choice, not a
  taxonomy.
- **No historical baggage.** Delete the four tiers, `Task`, `ToneGuidance`, and
  the `system.*` IDs rather than aliasing them.

## Considered Options

1. **Keep the four tiers and document the collapse.** Rejected: it preserves a
   false promise and keeps `Task` as dead architecture.
2. **Enumerate concrete origins as `ContentKind` variants.** Rejected: origins
   are open-ended and orthogonal to serialization; every new source would change
   the enum and every match.
3. **Keep `WireShape` / `TextRole` / `Trust` enums.** Rejected: once the plan
   uses typed containers, the container is the shape and region. The enums
   duplicate that information.
4. **Use a linked list for sections.** Rejected: plans are built once per
   request; Rust ownership and traversal costs outweigh insertion flexibility.
5. **Keep Markdown as a render mode.** Rejected: body syntax and boundary choice
   are orthogonal. AGENTS.md is Markdown content, not a Markdown rendering mode.
6. **`RequestPlan` with typed containers and boundary-only blocks (chosen).**

## Decision

### 1. The plan is four typed containers

```rust
pub struct RequestPlan {
    pub tools: Vec<ToolSpec>,
    pub fixed: Vec<Block>,
    pub messages: Vec<Message>,
    pub transient: Vec<Block>,
}
```

`RequestPlan` lives in `nuo-protocol`: both the live instruction registry and
the `SessionIR` compiler produce the same plan, so neither path owns a private
request shape. The container is the region and the shape:

| Container | Region | Shape | Cache |
| :--- | :--- | :--- | :--- |
| `tools` | Fixed | Tool | prefix |
| `fixed` | Fixed | Text | prefix |
| `messages` | Cumulative | Message | rolling prefix |
| `transient` | Transient | Text | no cache |

There is no `RequestRegion`, `WireShape`, `TextRole`, or `Trust` field. Trust is
conveyed by placement (`transient` or a tool-result message) and by the block’s
boundary choice.

### 2. A block is text plus a boundary choice

```rust
pub struct Block {
    pub id: String,
    pub title: Option<String>,
    pub source: Option<String>,
    pub body: String,
    pub render: BlockRender,
}

pub enum BlockRender {
    Xml(&'static str),
    Plain,
}
```

- `id` is a stable machine identity (`agent.identity`, `project.rules`, …).
- `title` is a plain label line, not a Markdown heading.
- `body` is semantic content only; its syntax (Markdown, prose, code) is
  preserved.
- `render` chooses an XML envelope or no envelope. It does not classify the
  content.

`render_blocks` is the single formatter. It owns title lines, XML tags,
attribute escaping, and deterministic spacing. Sections never emit headings or
leading newlines.

### 3. Tools and messages are written once

`RequestPlan::into_request` renders `fixed` and `transient` into
`InstructionText` and moves `tools` and `messages` into the prepared request.
Tools never enter text; messages never enter text. A provider without a separate
instruction field projects `fixed` into a system/developer message; that is a
projection, not a second copy.

### 4. Section IDs are origin-based

The `system.*` namespace is deleted. The default sections are:

| ID | Region | Content |
| :--- | :--- | :--- |
| `agent.identity` | fixed | identity preamble, when supplied |
| `agent.precedence` | fixed | short conflict-resolution contract |
| `user.instructions` | fixed | user-scoped tuning |
| `project.rules` | fixed | AGENTS.md |
| `project.instructions` | fixed | scoped repository instructions |
| `workspace.roots` | fixed | additional workspace roots |
| `workspace.topology` | fixed | workspace architecture summary |
| `security.web_untrusted` | fixed | web prompt-injection boundary |
| `provider.guidance` | fixed | provider dialect notes |

`ToneGuidance` is deleted. The default `developer` role emits no identity
preamble; a role is a policy (tools, workspace, permissions), not a persona.

### 5. Ordering is a validated DAG

Sections declare `Head`, `Before(id)`, `After(id)`, or `Tail`. The registry
compiles the active set with a stable topological sort. A missing registered
anchor is a `MissingAnchor` error; a cycle is a `Cycle` error. A registered but
inactive target contributes no edge. There is no silent tail fallback and no
integer rank.

### 6. One pipeline

```text
Live turn:
  Sections
    -> InstructionRegistry::build_regions
    -> RequestPlan { tools, fixed, messages, transient }

Replay / analysis:
  SessionIR
    -> compile_session_request (4 passes)
    -> RequestPlan { tools, fixed, messages, transient }

Both:
  RequestPlan
  -> RequestPlan::into_request
  -> ModelRequest (provider-neutral prepared request)
  -> provider projector (body_with_capabilities / build_body)
  -> WireRequest { protocol, body }
  -> WireRequest::encode
  -> EncodedRequest { protocol, body: Vec<u8>, content_type }
  -> transport (RequestBuilder::encoded)
```

`WireRequest` and `EncodedRequest` are concrete types in `nuo-provider::wire`,
not conceptual labels. `ProviderPipeline::plan_request` takes `&WireRequest` and
returns an `OutboundPlan` whose body is an `EncodedRequest`; every provider send
path encodes once and hands the bytes to `RequestBuilder::encoded`.
`ModelRequest` keeps its name: it is the provider-neutral prepared request,
never a provider-specific wire body.

## Invariants

- **INV-PLAN-01** — Every request element appears in exactly one of `tools`,
  `fixed`, `messages`, or `transient`.
- **INV-PLAN-02** — `fixed` contains no conversation-dependent content.
- **INV-PLAN-03** — `messages` is the only region that grows across turns.
- **INV-PLAN-04** — `transient` is emitted after the final cache breakpoint.
- **INV-DUP-01** — A tool schema is emitted only in the protocol tool field.
- **INV-DUP-02** — A conversation message is emitted only in the protocol message
  list.
- **INV-FORMAT-01** — A section returns a body without a renderer-owned heading,
  leading newline, or trailing whitespace.
- **INV-FORMAT-02** — `fixed` bytes are deterministic; golden tests pin them.
- **INV-RENDER-01** — `BlockRender` chooses a boundary only; it never changes a
  block’s region or body syntax.
- **INV-LAYOUT-01** — Layout order is compiled and validated; missing anchors and
  cycles are errors.

## Negative Knowledge

- **Do not enumerate concrete origins as kinds.** `identity`, `project`,
  `security`, and `provider` are origins, not wire shapes. They belong in the
  block id.
- **Do not flatten tools or messages into prompt text.** Protocol fields are the
  single source of truth for their content.
- **Do not use Markdown as a render mode.** A Markdown document is a body; the
  boundary is XML or plain.
- **Do not add a `Trust` enum for a two-value attribute.** Placement and the
  boundary tag carry the distinction; add an explicit boolean only if a trusted
  context block becomes a real requirement.
- **Do not use a linked list for layout.** Declarative anchors compile to a
  `Vec`; the runtime layout is read-only.
- **Do not keep `system.*` aliases.** A migration error names the old and new
  IDs; it does not serve both.
- **Do not keep `Task` or `ToneGuidance` as reserved slots.** Add them when a
  consumer exists.

## Consequences

### Positive

- Cache layout is explicit: fixed prefix, growing conversation, request-local
  tail.
- Tools and messages cannot be duplicated into text.
- The default prompt is smaller: no tone slot, no default developer persona, no
  tier bookkeeping.
- Section ordering is validated; missing anchors and cycles fail fast.
- AGENTS.md is embedded with a real boundary instead of heading collision.

### Negative / cost

- `InstructionTier`, `InstructionSlice`, and `InstructionBundle` are deleted;
  `InstructionText { fixed, transient }` is the serialized instruction shape.
  `PROTOCOL_VERSION` and `MIN_PROTOCOL_VERSION` move to 28.
- Embedders using `disable_instruction_section("system.project_rules")` must
  migrate to `project.rules`.
- `InstructionSection::tier` and `rank` are gone; custom sections implement
  `render -> Option<Block>` and optionally `is_transient`.
- The `developer` role no longer emits a persona directive. A custom role that
  wants prose must supply it explicitly.
- No default section is transient today. The plan and formatter support
  transient blocks; the first production transient producer must land the
  provider-specific trailing placement described in RFC-0002 §5.12.

## Compliance and Verification

- `cargo check --workspace` and `cargo test --workspace` pass.
- `RequestPlan::into_request` has a regression test proving tools and messages
  never appear in the rendered text.
- `render_blocks` has deterministic-output tests.
- The instruction registry has tests for ordering, missing anchors, cycles,
  transient partition, memo invalidation, and duplicate ids.
- Provider request tests pin `InstructionText` projection.
