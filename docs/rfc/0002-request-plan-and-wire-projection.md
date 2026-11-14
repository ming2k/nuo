# RFC-0002: Request Plan and Wire Projection

- **Status:** Migrated to ADR — implemented by [ADR-0172](../adr/0172-request-plan-containers-are-regions-and-blocks-are-boundaries.md)
- **Revision:** Final
- **Date:** 2026-11-13
- **Related:** ADR-0052 (instruction pipeline), ADR-0064 (tool surface pruning),
  ADR-0130 (model guidance), ADR-0165 (user instructions)
- **Supersedes in part:** the four-tier `Base / Session / Task / Ephemeral`
  taxonomy, the flat `ContentKind` model, and the `system.*` ID namespace
- **Implementation note:** `RequestPlan::into_request` produces the provider-neutral `ModelRequest`; provider projectors produce `WireRequest`, and `WireRequest::encode` produces `EncodedRequest` (`nuo-provider::wire`). The transport consumes `EncodedRequest` through `RequestBuilder::encoded`.


## 0. Recommendation (TL;DR)

Collapse prompt assembly to one logical structure and one projection step.

1. `RequestPlan` is the provider-neutral request content. It lives in
   `nuo-protocol` so the live registry and the `SessionIR` compiler produce the
   same shape:
   ```rust
   struct RequestPlan {
       tools: Vec<ToolSpec>,
       fixed: Vec<Block>,
       messages: Vec<Message>,
       transient: Vec<Block>,
   }
   ```
2. `Block` is a simple fragment of `fixed` or `transient`:
   ```rust
   struct Block {
       id: &'static str,
       title: Option<&'static str>,
       source: Option<&'static str>,
       body: String,
       render: BlockRender,
   }
   ```
3. No `RequestRegion`, `WireShape`, `TextRole`, or `Trust` enum. The plan
   collection is the region and the shape; trust is conveyed by placement and
   render choice.
4. `WireProjector` maps `RequestPlan` to a provider-shaped `WireRequest` exactly
   once: tools go to the protocol tool field, messages to the protocol message
   list, and only `fixed` / `transient` become text.
5. `Encoder` turns `WireRequest` into `EncodedRequest` (JSON or framed bytes).
6. Rendering is a per-block boundary choice: an XML envelope or plain text.
   Body syntax (for example Markdown inside AGENTS.md) is independent and is
   preserved as-is. `BlockRender` is not a semantic taxonomy.
7. Delete `Task`, `ToneGuidance`, and the `system.*` ID namespace. Rename
   sections to origin-based IDs (`agent.*`, `project.*`, `workspace.*`,
   `security.*`, `provider.*`).

The rule is: **containers carry region and shape; blocks carry text and render
style; the projector carries protocol knowledge.**

---

## 1. Motivation

### 1.1 The current taxonomy duplicates information

`InstructionTier` has four variants, but `Base`, `Session`, and `Task` are all
rendered into the same static instruction string. Only `Ephemeral` is treated
differently. The provider never sees four cache tiers.

`Task` has only three producers, all one-shot cognitive calls; subagents use
`Base` via their child identity. It is a label, not a lifetime.

### 1.2 A flat `ContentKind` mixes axes

`Tool` and `Message` describe wire shape; `Policy`, `Reference`, and `Context`
describe text meaning. A flat enum makes invalid combinations representable and
turns every new source into an enum change.

Enumerating concrete origins (`Identity`, `Project`, `Security`, …) is worse: it
conflates “who wrote it” with “how it serializes” and forces a match update for
every new source.

### 1.3 Sections own presentation

Default sections embed `\n# Heading` in their rendered bodies. Ordering,
spacing, and headings are distributed across sections, so reordering is
fragile and cache bytes are not deterministic.

### 1.4 Tools and messages are at risk of duplication

A prompt builder can accidentally serialize a tool schema into instruction text
or repeat a message as a “context” block. The protocol already has typed slots
for both. The plan must make duplication structurally impossible.

### 1.5 `system.*` is a transport-era name

The domain abstraction is a request, not a system prompt. Providers place
instructions in system blocks, top-level `instructions`, developer items, or a
system-role message. `system.*` collides with `Role::System`, which ADR-0052
deliberately removes from the conversation vector.

---

## 2. Goals and Non-Goals

### Goals

- One logical request model: `tools / fixed / messages / transient`.
- No duplicated information between text and protocol fields.
- Deterministic text assembly and cache boundaries.
- Simple blocks; rendering is a per-block choice.
- Extensibility without editing concatenation code.
- Clear embedding of Markdown documents such as AGENTS.md.

### Non-Goals

- Rewriting the agent loop or provider transports.
- Solving prompt injection completely.
- Designing a general prompt schema.
- Preserving the four-tier taxonomy for compatibility.
- Adding semantic taxonomies that do not change behavior.

---

## 3. Principles

1. **The container is the region.** `fixed` is Fixed; `messages` is Cumulative;
   `transient` is Transient.
2. **The container is the shape.** `tools` is Tool; `messages` is Message;
   `fixed` / `transient` are Text.
3. **Protocol fields are written once.** Tools and messages never enter text.
4. **Blocks are dumb.** A block is id, title, source, body, and a render choice.
5. **Sections produce bodies; the formatter produces presentation.**
6. **XML is a boundary choice, not a taxonomy.** Use it where a text boundary
   helps; use plain text where it does not. The body's syntax (Markdown, prose,
   code) is independent of the boundary.
7. **Deterministic bytes.** `fixed` must be byte-identical for a given route.

---

## 4. Invariants

- **INV-PLAN-01** — Every request element appears in exactly one of `tools`,
  `fixed`, `messages`, or `transient`.
- **INV-PLAN-02** — `fixed` contains no conversation-dependent content.
- **INV-PLAN-03** — `messages` is the only region that grows across turns.
- **INV-PLAN-04** — `transient` is emitted after the final cache breakpoint.
- **INV-DUP-01** — A tool schema is emitted only in the protocol tool field, never
  in text.
- **INV-DUP-02** — A conversation message is emitted only in the protocol message
  list, never in `fixed` or `transient`.
- **INV-FORMAT-01** — A section returns a body without a renderer-owned heading,
  leading newlines, or trailing whitespace. Content-authored headings inside a
  Markdown document remain part of the body.
- **INV-FORMAT-02** — `fixed` bytes are deterministic; golden tests pin them.
- **INV-RENDER-01** — `BlockRender` chooses presentation only; it does not change
  the block's region.
- **INV-CACHE-01** — `transient` never enters the cached prefix.

---

## 5. Design

### 5.1 RequestPlan

```rust
pub struct RequestPlan {
    /// Protocol tool declarations. Projected to the provider tool field.
    pub tools: Vec<ToolSpec>,
    /// Provider-neutral text that does not depend on the conversation.
    pub fixed: Vec<Block>,
    /// Conversation turns. Projected to the provider message list.
    pub messages: Vec<Message>,
    /// Text that belongs only to this request.
    pub transient: Vec<Block>,
}
```

There is no separate region field. The vector is the region:

| Vector | Region | Shape | Cache |
| :--- | :--- | :--- | :--- |
| `tools` | Fixed | Tool | prefix |
| `fixed` | Fixed | Text | prefix |
| `messages` | Cumulative | Message | rolling prefix |
| `transient` | Transient | Text | no cache |

The order inside each vector is the wire order. `fixed` and `transient` are
ordered by the section registry; `messages` is chronological.

### 5.2 Block

```rust
pub struct Block {
    /// Stable identity for deduplication, override, and telemetry.
    pub id: &'static str,
    /// Optional human-readable label, rendered as a plain title line.
    pub title: Option<&'static str>,
    /// Optional provenance (e.g. "AGENTS.md").
    pub source: Option<&'static str>,
    /// Semantic content only. Body syntax (Markdown, prose, code) is preserved.
    pub body: String,
    /// Boundary choice.
    pub render: BlockRender,
}

pub enum BlockRender {
    /// Wrap the block in an XML envelope with the given tag,
    /// e.g. "policy", "reference", "untrusted".
    Xml(&'static str),
    /// Emit the title line and body without an envelope.
    Plain,
}
```

A block does not know whether it is “policy” or “reference”. That distinction is
a property of its title and placement. The block only knows how it should be
rendered.

Examples:

```rust
Block {
    id: "project.rules",
    title: Some("Project Rules (AGENTS.md)"),
    source: Some("AGENTS.md"),
    body: agents_md,
    render: BlockRender::Xml("policy"),
}
```

```rust
Block {
    id: "workspace.topology",
    title: Some("Workspace"),
    source: None,
    body: topology,
    render: BlockRender::Xml("reference"),
}
```

```rust
Block {
    id: "retrieval.1",
    title: Some("Retrieved context"),
    source: Some("https://example.com"),
    body: page_text,
    render: BlockRender::Xml("untrusted"),
}
```

### 5.3 Sections and ordering

Sections are declarations that produce blocks:

```rust
pub trait Section {
    fn id(&self) -> &'static str;
    fn order(&self) -> SectionOrder;
    fn active(&self, ctx: &Context) -> bool;
    fn render(&self, ctx: &Context) -> Option<Block>;
}

pub enum SectionOrder {
    Head,
    Before(&'static str),
    After(&'static str),
    Tail,
}
```

`PlanBuilder`:

1. collects active sections;
2. rejects duplicate IDs;
3. validates anchors and cycles;
4. sorts by `Head` → anchors → `Tail`, with stable ID tie-break;
5. calls `render` and collects blocks into `fixed` or `transient`.

Missing anchors and cycles are hard errors, never a silent tail fallback.

The plan vectors are already ordered. `Block` carries no order field.

### 5.4 WireProjector

`WireProjector` maps `RequestPlan` to a provider-shaped `WireRequest`:

```rust
pub enum WireRequest {
    Anthropic(AnthropicRequest),
    OpenAiResponses(OpenAiRequest),
    ChatCompletions(ChatRequest),
    Google(GoogleRequest),
}
```

Projection rules:

- `tools` → the provider's tool declaration field;
- `messages` → the provider's message/input field;
- `fixed` → the provider's instruction channel:
  - Anthropic system blocks;
  - OpenAI Responses top-level `instructions`;
  - Chat Completions system/developer message;
  - Google `systemInstruction`;
- `transient` → the provider's trailing channel:
  - Anthropic trailing user content; an uncached system block only when
    message-prefix caching is not required;
  - OpenAI Responses developer item at the tail;
  - Chat Completions trailing user message (a late system message breaks the
    cached prefix);
  - Google appended to the last user turn.

The projector never moves a tool or a message into text.

### 5.5 EncodedRequest

`Encoder` turns `WireRequest` into bytes or a frame:

```rust
pub struct EncodedRequest {
    pub body: Vec<u8>,
    pub content_type: &'static str,
}
```

This is the “ready to send” structure. It contains no domain concepts, only the
provider payload.

Pipeline:

```text
Sections
  -> PlanBuilder
  -> RequestPlan { tools, fixed, messages, transient }
  -> WireProjector
  -> WireRequest (provider-specific typed payload)
  -> Encoder
  -> EncodedRequest (bytes)
```

`RequestRaw` is rejected as a name: “raw” usually means unprocessed input, not
a prepared output. Use `WireRequest` for the typed payload and `EncodedRequest`
for bytes.

### 5.6 No-duplication rule

The plan makes duplication a structural error:

- a tool is only in `tools`;
- a message is only in `messages`;
- text is only in `fixed` or `transient`.

If a provider has no separate instruction field, `fixed` becomes a
system/developer message. That is a projection, not a second copy.

If a provider has no transient channel, `transient` becomes a trailing user
message. It is not merged into `fixed`.

Tests must assert that no tool name or message body appears in the rendered
`fixed` / `transient` text.

### 5.7 User injection

| Input | Destination |
| :--- | :--- |
| identity, role, precedence | `fixed` |
| user instructions | `fixed` |
| AGENTS.md, project rules | `fixed` |
| workspace roots, topology | `fixed` |
| provider guidance | `fixed` |
| per-turn retrieval, attachments, mode nudges | `transient` |
| user chat message | `messages` |
| steering / inter-agent message | `messages` |
| tool call and tool result | `messages` |
| compaction summary | `messages` (summary block) |

Most user-authored configuration lands in `fixed`; per-turn injected context
lands in `transient`. Conversation stays in `messages`.

### 5.8 Cache boundaries

`CachePlanner` reads the provider capabilities and marks:

- end of the tool block (if the provider caches tools separately);
- end of `fixed`;
- end of the stable prefix of `messages`.

`transient` is always after the final breakpoint. Providers with fewer
breakpoints merge from the front; they never move `transient` into the cached
prefix.

### 5.9 Rendering

`BlockFormatter` renders one block:

- `BlockRender::Xml(tag)`:
  ```xml
  <tag source="...">
  Project Rules (AGENTS.md)

  ...body...
  </tag>
  ```
- `BlockRender::Plain`:
  ```text
  Project Rules (AGENTS.md)

  ...body...
  ```

The title is a plain label line, not a Markdown heading. There is no
`BlockRender::Markdown`: the body's syntax is independent of the boundary. A
Markdown document (AGENTS.md) is still a body; the renderer does not treat
Markdown as a boundary mode.

`RegionAssembler` joins blocks deterministically:

- one blank line between blocks;
- no leading newline;
- no more than two consecutive newlines;
- no trailing spaces;
- a title line only when `title` is present;
- internal IDs stay in attributes, never in title lines.

AGENTS.md remains Markdown content inside `BlockRender::Xml("policy")`. The
wrapper prevents its own headings from colliding with the outer layout.
Heading demotion, if used, is a body transformation, not a render mode.

### 5.10 Naming

| Current | Proposed |
| :--- | :--- |
| `InstructionTier` | deleted |
| `Base` / `Session` / `Task` | `fixed` |
| `Ephemeral` | `transient` |
| `ContentKind` | deleted |
| `WireShape` | deleted (container is shape) |
| `TextRole` | deleted (render choice is enough) |
| `Trust` | deleted (placement + render choice) |
| `system.identity_preamble` | `agent.identity` |
| `system.tone` | deleted |
| `system.user_instructions` | `user.instructions` |
| `system.provider_guidance` | `provider.guidance` |
| `system.project_rules` | `project.rules` |
| `system.project_instructions` | `project.instructions` |
| `system.workspace_roots` | `workspace.roots` |
| `system.workspace_topology` | `workspace.topology` |
| `system.web_untrusted_content` | `security.web_untrusted` |
| “renderer” | `BlockFormatter` / `RegionAssembler` / `WireProjector` / `Encoder` |

---

## 6. Security, Reliability, and Failure Modes

- **Untrusted content** is `transient` or a tool-result message; it never enters
  `fixed`. `BlockRender::Xml("untrusted")` is a signal, not a hard boundary.
- **Closing-tag injection** can end a fixed XML tag early. For trusted documents,
  escape or use a nonce. For untrusted data, structural placement is the primary
  defense.
- **Duplicate content** is prevented structurally (containers) and checked by
  golden tests.
- **Cache drift** is prevented by deterministic formatting and byte-level
  golden tests for `fixed`.
- **Missing anchors / cycles** are hard errors at plan build time.
- **Provider mismatch** must fail loudly if `transient` cannot be placed after
  the final cache breakpoint.
- **Oversized documents** need a size bound and a truncation notice that does
  not cut a code fence.

---

## 7. Migration and Compatibility

1. **Phase 0 — dead code and naming.**
   - delete `ToneGuidance`;
   - make default `developer` emit no directive;
   - fix the `identity.rs` doc/behavior mismatch;
   - re-anchor dependent orders.
2. **Phase 1 — introduce the plan.**
   - add `RequestPlan`, `Block`, `BlockRender`, `Section`;
   - map existing sections to blocks;
   - keep the current rendered output as a golden baseline.
3. **Phase 2 — centralize formatting.**
   - move title lines and newlines out of sections;
   - add `BlockFormatter` and `RegionAssembler`;
   - add AGENTS.md XML embedding.
4. **Phase 3 — delete the old taxonomy.**
   - delete `InstructionTier`, `ContentKind`, `Task`, `system.*` IDs;
   - update tests, docs, and embedding APIs;
   - bump `PROTOCOL_VERSION` if serialized IR shapes change.
5. **Phase 4 — projection and encoding.**
   - add `WireProjector` and `Encoder`;
   - add provider capability/cache planning;
   - verify breakpoints per provider.

No compatibility aliases for old section IDs. A migration error should name the
old ID and the new ID.

`[[instructions.user]]` and `.nuo/instructions` config surfaces remain
unchanged; only their internal IDs and rendering change.

---

## 8. Work Plan

1. RFC review and agreement on the minimal plan.
2. Land Phase 0 (dead code + role default).
3. Land `RequestPlan` / `Block` behind the existing assembler; keep output
   byte-identical.
4. Move one section family at a time (identity → project → workspace →
   security → provider).
5. Add XML rendering and AGENTS.md embedding.
6. Add `WireProjector` and `Encoder`.
7. Delete old types and update docs/ADRs.
8. Run provider golden tests and model evals.

---

## 9. Test Plan

**Unit**

- Plan build: duplicate ID, missing anchor, cross-section cycle.
- Block: body trimming, title handling, render selection.
- Formatter: XML/plain output, deterministic whitespace.
- AGENTS.md: wrapper, code fences, optional heading demotion.
- XML: closing-tag injection, nonce behavior.

**Golden**

- Per-provider `WireRequest` snapshots: tools, fixed text, messages, transient
  placement.
- Byte stability: same input → identical `fixed` bytes across runs.
- No tool schema in text.
- No message body in text.
- No internal IDs in model-visible title lines.

**Behavioral evals**

- Untrusted content cannot redirect the agent.
- AGENTS.md headings are not treated as global authority.
- Transient content is not treated as durable.
- Same behavior across Claude / GPT / DeepSeek where supported.

---

## 10. Alternatives Rejected

- **Keep four tiers.** They imply cache boundaries the providers do not
  receive, and `Task` has no consumer.
- **A flat `ContentKind` enum.** It mixes wire shape and text role.
- **Enumerate concrete origins as kinds.** Origins are open-ended and orthogonal
  to serialization.
- **`WireShape` / `TextRole` / `Trust` enums.** Once the plan uses typed
  containers, the container is the shape and region; the enums duplicate that
  information.
- **A linked list for sections.** Rust ownership and traversal costs outweigh
  insertion flexibility; plans are built once per request.
- **Markdown-only boundaries.** AGENTS.md headings collide with outer headings;
  XML gives a clearer container.
- **Markdown as a `BlockRender` variant.** Body syntax and boundary choice are
  orthogonal; a Markdown render mode conflates them. Use `Xml` or `Plain` and
  preserve the body as-is.
- **XML for everything.** Tools and messages already have protocol structure.
- **Free-form string concatenation.** No validation, no byte stability, no
  cache planning.
- **`RequestRaw` as the output name.** “Raw” is ambiguous; use `WireRequest`
  and `EncodedRequest`.

---

## 11. Open Questions

1. Should `Block` carry a `cache` flag for sub-breakpoints inside `fixed`, or is
   cache planning entirely per region?
2. Should AGENTS.md heading demotion be the default, or opt-in?
3. Should untrusted `transient` blocks always use a nonce, or only when a
   closing-tag collision is detected?
4. How should remote-stored continuation (`ContinuationMode::RemoteStored`)
   represent `messages` when the conversation is not replayed?
5. Do compaction summaries need a dedicated message subtype, or is an XML
   envelope sufficient?
6. What is the size bound and truncation policy for AGENTS.md and scoped
   instructions?
7. Do we keep `SectionOrder::Before/After` anchors, or use a simple priority
   number for a small section set?
8. Should the plan expose a stable debug rendering for `nuo status` and
   diagnostics?
