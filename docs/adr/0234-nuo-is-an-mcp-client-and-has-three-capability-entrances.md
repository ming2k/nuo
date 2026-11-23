---
id: ADR-0234
title: "Nuo Is an MCP Client and Has Three Capability Entrances"
status: accepted
date: 2026-12-20
scope: product/capability, mcp, extension, prompt-policy
superseded_by: null
negative_knowledge: true
---

# 0234. Nuo Is an MCP Client and Has Three Capability Entrances

- **Status:** Accepted
- **Date:** 2026-12-20
- **Related:** [ADR-0230](0230-one-mcp-client-one-adapter-and-a-reachable-skill-capability.md),
  [ADR-0231](0231-one-instruction-region-and-no-ghost-extension-subsystem.md),
  [ADR-0216](0216-the-prompt-branches-on-declared-tool-capability-not-tool-names.md),
  [ADR-0183](0183-composition-root-owns-capability-linkage.md)

## Context and Problem Statement

Two clean-up decisions removed capability surfaces:

- ADR-0230 deleted `mcp::McpServer`, the export half of "MCP client/server".
- ADR-0231 deleted `protocol::Extension`, the in-process extension trait.

Both ADRs are unambiguous about the code. Neither is a **product** decision: a
reader cannot tell whether Nuo will re-add an MCP export server or a plugin ABI
when a consumer appears, or whether those surfaces are now outside the product.
That ambiguity is expensive:

- The README still advertised "MCP client/server integration", so the first
  claim a new user read was one the code no longer supported.
- An embedder cannot tell whether to build against `protocol::Extension` (gone),
  `HookRegistry` (lifecycle), `ToolSet` (model-callable), or a declared
  `ContextSource` (prompt text).
- A future contributor can read ADR-0231's "there is no fourth capability
  entrance" as a local observation rather than a boundary, and add the fourth
  path the next time a feature needs request-local data.

The architectural decisions are already ratified. This ADR states the product
boundary they imply, so the next proposal starts from a declared route instead
of re-litigating a deletion.

## Decision

### 1. Nuo is an MCP client

Nuo connects to MCP servers configured in `config.toml` (or the role/project
overlays), discovers their tools and resources, and exposes them to the model.
It does **not** serve its own tools over MCP.

An MCP export server may be added only when all three exist:

1. a named consumer that is not Nuo itself;
2. a trust story that answers which tools, which scopes, and which session are
   exported; and
3. an ADR that supersedes this one and names the consumer.

Deleting the unserved `McpServer` is not a temporary simplification. It is the
current product boundary.

### 2. Three capability entrances, two data owners

A capability enters through exactly one of these doors:

| Concern | Owner |
| :--- | :--- |
| Model-callable capability | `ToolSet` / `ToolManager` |
| Lifecycle observation and injection | `HookRegistry` (`[[hooks]]`, ADR-0025) |
| Standing prompt text | a declared `ContextSource` (ADR-0215) |

Two data owners are not capability entrances:

| Concern | Owner |
| :--- | :--- |
| Durable conversation | `messages` / `ThreadFact` |
| Request-local data | a hidden `Message` with its own `InjectionKind` |

A fourth **capability** entrance is a design defect, not a feature. A new data
owner is a different question: it must name the protocol field it replaces and
the cache semantics it preserves.

### 3. There is no in-process plugin ABI

A third-party integration is one of two things:

- an MCP server, consumed through the client; or
- a compiled tool crate, linked by the composition root (ADR-0183).

There is no `Extension` trait, no dynamic library ABI, and no hook that installs
a tool outside `ToolSet`. A tool that enters through a second path bypasses
permission scoping, cancellation, and the per-request snapshot — the exact
failure ADR-0230 found in the substrate bridge.

## Invariants

- **`[INV-PRODUCT-01]` No Export Surface Without a Consumer** — a server or
  export surface ships only with a named non-self consumer, a trust story, and
  an ADR. A test that is the only consumer is not a consumer.
- **`[INV-PRODUCT-02]` The README States the Product Boundary** — the product
  overview may not advertise an MCP export server or an in-process plugin ABI.
  Enforced by a governance scan over the living product documents.

## Consequences & Trade-offs

- **Positive**: the route question has an answer; the README matches the code;
  an embedder knows which door to use; a future export server must arrive with
  its consumer and trust story instead of being re-added as a crate.
- **Negative & incurred**: Nuo cannot be used as an MCP server. An embedder that
  wants to expose Nuo's tools must build its own host and supply the trust story
  this ADR declines to invent.
- **Negative & accepted**: a third-party integration that is not an MCP server
  must be compiled into the workspace. There is no dynamic plugin lane.
- **Neutral**: MCP client behavior, the `/mcp` modal, `McpServerConfig`, and
  `McpConnectionStatus` are unchanged.

## Discarded Alternatives

### A — Keep `McpServer` and wire a `nuo mcp serve` subcommand

- **Why considered**: it makes the old README true, and the protocol code
  already existed.
- **Why rejected**: no consumer asked for it, and serving one's own tools needs
  a trust story (which tools, which scopes, which session) that does not exist.
  A surface with no consumer is a test-only export, which ADR-0230 already
  rejected.

### B — Keep `Extension` as a plugin ABI for future producers

- **Why considered**: platform extensibility is a plausible product direction.
- **Why rejected**: the trait had no production implementation beyond a unit
  struct, its `tool()` had no reader, and its only instance did nothing. The two
  real paths (`ToolSet` and `HookRegistry`) already exist and are named. Keeping
  the trait would preserve a promise the framework does not keep.

### C — Add a fourth "request-local" capability entrance

- **Why considered**: per-turn context needs a home, and a fourth door looks
  cheaper than reusing `messages`.
- **Why rejected**: `messages` is already that home, with provenance, ordering,
  cache semantics, and a renderer. A second channel would need its own rules for
  all four, and the model would have two instruction surfaces with no rule for
  which wins (ADR-0231, Alternative C).

## Compliance and Verification

- `README.md` / `README-zh.md` — the feature list says MCP **client**, not
  client/server.
- `tests/integration/governance/architecture_convergence.rs` —
  `[INV-PRODUCT-02]` scans the living product documents for a re-introduced
  export/plugin claim.
- `crates/protocol/src/dynamic.rs` — the `DynamicCatalog` doc names the MCP
  catalog as owned by the server runtime, not by a deleted `mcp` crate.
- `crates/server/src/composition/bootstrap.rs` — tools are installed by the
  composition root through `ToolSet`, not by an extension.
