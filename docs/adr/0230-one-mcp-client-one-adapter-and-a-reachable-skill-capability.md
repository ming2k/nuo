---
id: ADR-0230
title: "One MCP Client, One MCP Adapter, and a Skill Capability the Model Can Actually Reach"
status: accepted
date: 2026-12-20
scope: protocol/mcp, server/runtime/mcp, engine/cognition/skills, tool/builtin
superseded_by: null
supersedes:
  - "`server::runtime::mcp::substrate_bridge`"
  - "`mcp::McpNativeTool`"
  - "`mcp::McpClient::into_tools`"
  - "`mcp::McpServer`"
negative_knowledge: true
---

# 0230. One MCP Client, One MCP Adapter, and a Skill Capability the Model Can Actually Reach

- **Status:** Accepted
- **Date:** 2026-12-20
- **Amends:** [ADR-0208](0208-tool-contract-validation-snapshots-and-deferred-discovery.md)
  (deferred exposure now has a second declared producer),
  [ADR-0179](0179-one-capability-one-producer-and-no-ghost-builtins.md)
  (applied to MCP and to the skill tools),
  [ADR-0199](0199-the-default-agent-has-a-coding-baseline-and-skill-catalog.md)
  (the catalog's `Resume` now names a real tool)
- **Related:** [ADR-0210](0210-mcp-cancellation-and-progress.md),
  [ADR-0226](0226-one-renderer-and-one-reader-for-message-envelopes.md),
  [ADR-0228](0228-untrusted-content-has-one-framing.md),
  [ADR-0229](0229-every-context-element-has-a-production-owner.md),
  [ADR-0234](0234-nuo-is-an-mcp-client-and-has-three-capability-entrances.md)

## Context and Problem Statement

Four distinct defects shared one shape: **a capability declared twice, or
declared once and never installed.**

1. **Two MCP clients.** `mcp::McpClient` (a `&'static str` stdio client with
   `into_tools`) and `server::runtime::mcp::McpClient` (reconnect-capable, HTTP
   aware, cancellable, progress-emitting, with an error model that separates
   transport from protocol). The former existed only to serve
   `substrate_bridge`, whose only caller was its own test.
2. **Two MCP tool adapters.** `mcp::McpNativeTool` reported every tool as
   `RiskProfile::NetworkAccess`, declared no `exposure()`, no `timeout()`, no
   `retry_safe`, and no access scope. The production adapter
   (`server::runtime::mcp::McpTool`) declares all of them. A tool routed through
   the substrate bridge therefore bypassed permission scoping and cancellation.
3. **An MCP export server nobody serves.** `mcp::McpServer` handled
   `initialize` / `tools/list` / `tools/call`; the only consumer was
   `tests/integration/mcp.rs`. The `nuo` binary has no serve path, and the CLI
   documents `mcp` as removed. The README advertised bidirectional export.
4. **A resume route to a tool that is not installed.** `skills.catalog`'s budget
   is `Budget::index_to_tool(1_500, "list_skills")`, but `UseSkillTool` /
   `ListSkillsTool` were constructed by no production path, were absent from
   `BuiltinTool`, and carried a comment explaining that "progressive disclosure
   is not yet implemented". When the catalog was elided, the model was told to
   call a tool it could not call.

## Decision

### 1. Delete the substrate bridge and the second adapter

`server::runtime::mcp::substrate_bridge` is removed, along with
`mcp::McpNativeTool`, `mcp::McpClient::into_tools`, `mcp::McpServer`,
`mcp::client`, `mcp::server`, `mcp::adapter`, and `mcp::transport`.

`crates/mcp` is deleted. What survives is split by who reads it:

- **`protocol::mcp`** keeps the client-facing configuration and status DTOs
  (`McpServerConfig`, `McpConnectionStatus`), because they cross the wire to
  every client. It does not keep a JSON-RPC grammar: that grammar has exactly one
  consumer.
- **`server::runtime::mcp`** keeps the JSON-RPC and MCP wire grammar, the
  connection lifecycle, cancellation, progress, reconnection, the error model,
  and the tool adapter. It is the only client, so its wire grammar is private to
  it. A public copy in `protocol` would be a second vocabulary with no second
  reader — the defect this ADR removes, one layer down.

`McpResourceTool`'s text output is framed as untrusted data
(`<untrusted origin="mcp">`, ADR-0228) — the one external door that had no
boundary.

### 2. The skill tools become real, deferred built-ins

- `BuiltinTool::UseSkill` / `BuiltinTool::ListSkills` are added. They are
  `ToolExposure::Deferred`: reachable by `tool_search` and by direct dispatch,
  never declared in the prompt, because the catalog already occupies that space.
- `list_skills` / `use_skill` are installed by the composition root, so the
  catalog's `Overflow::Index` resume names a tool that exists.
- `use_skill` renders through the same `<skill>` envelope as an `@name` mention:
  one format, two doors (ADR-0226).
- Aliases are empty on purpose: `@name` is the user-facing mention grammar, not a
  tool alias, and claiming `use_skill` for a foreign spelling would be a second
  name for one capability.

### 3. No prompt guidance for MCP, on purpose

MCP tools describe themselves in their `description`, which is the protocol field
designed for it; the prompt may not branch on a tool name and a third-party tool
may not contribute prompt text (ADR-0216). Deferred MCP tools are already outside
the prompt by default (`defer_tools = true`), so the guidance question does not
arise: activation changes the tool *schemas*, never the instruction prefix.

## Invariants

- **`[INV-MCP-01]` One Client** — exactly one MCP connection implementation
  exists, and it is the reconnect-capable one in `server`.
- **`[INV-MCP-02]` One Adapter Module** — exactly one module owns every
  MCP-to-`Tool` implementation, and every tool type it exposes declares risk,
  exposure, timeout, retry safety, and access scope. `McpTool` (`tools/call`)
  and `McpResourceTool` (`resources/list` / `resources/read`) are two
  projections of the same client and the same protocol grammar, not two
  implementations. Enforced by
  `tests/integration/governance/context_engineering.rs::inv_mcp_01_one_client_and_one_adapter`
  (one file) plus the adapter-level declarations.
- **`[INV-MCP-03]` No Unserved Export** — a crate may not ship a server whose only
  consumer is its own test. Enforced by
  `tests/integration/governance/architecture_convergence.rs::inv_mcp_03_no_unserved_mcp_export_crate`
  (the retired `crates/mcp` must stay absent).
- **`[INV-SKILLTOOL-01]` Every Resume Names a Reachable Tool** — an
  `Overflow::Index` / `Elide` resume tool must be installed by the composition
  root and carry a `BuiltinTool` variant.
- **`[INV-SKILLTOOL-02]` One Skill Envelope** — a mention-loaded and a
  tool-loaded skill produce the same envelope shape.
- **`[INV-SKILLTOOL-03]` Deferred, Not Declared** — the skill tools are
  `Deferred`; the catalog is the only skill surface in the prompt.

## Consequences & Trade-offs

- **Positive**: one MCP path, so permission scoping, cancellation, progress, and
  retry safety apply to every MCP tool; the deferred catalog stops naming a tool
  the model cannot call; two adapters and a dead export server are gone.
- **Negative & incurred**: `crates/mcp` is deleted. An embedder that used
  `McpClient` / `into_tools` must use `server`'s runtime — which is the only one
  that was ever correct — or build its own client against `protocol::mcp`'s DTOs.
  That is a real break, accepted because the deleted client bypassed permission
  scoping, cancellation, and retry safety.
- **Neutral**: `McpServerConfig`, `McpConnectionStatus`, and the `/mcp` modal are
  unchanged.

## Discarded Alternatives

### A — Fix `McpNativeTool` instead of deleting it

- **Why considered**: keep the protocol crate self-sufficient.
- **Why rejected**: it would be a second adapter for the same trait, and the
  reconnect/cancellation/progress behaviour lives in `server`. Two adapters means
  two places to forget `retry_safe`.

### B — Wire `McpServer` to a `nuo mcp serve` subcommand

- **Why considered**: makes the README true.
- **Why rejected**: no consumer asked for it, and serving one's own tools over MCP
  needs a trust story (which tools, which scopes, which session) that does not
  exist. Deleting an unserved surface is the honest move; a future ADR can add it
  with its consumers.

### C — Leave the skill tools as library-only utilities

- **Why considered**: they already existed and their absence was documented.
- **Why rejected**: `Budget::index_to_tool(…, "list_skills")` tells the model to
  call it. A resume route to an uninstalled tool is a false statement in the
  prompt (`[INV-CTXSRC-02]`).

### D — Keep `crates/mcp` as a protocol-only crate

- **Why considered**: a crate boundary looks like an ownership boundary, and a
  protocol crate with no transport is a common shape.
- **Why rejected**: the JSON-RPC grammar, the connection state machine, and the
  error model have exactly one consumer (`server`). Moving the grammar into a
  public crate would create a second vocabulary that only one reader uses, and
  the next client would copy it rather than depend on the one that is correct.
  The DTOs that genuinely cross the wire already live in `protocol::mcp`.

## Compliance and Verification

- `crates/tool/src/builtin.rs` — `BuiltinTool::{UseSkill, ListSkills}` with unique
  canonical names (`tests/integration/tool/tool_contracts_test.rs`).
- `crates/engine/src/cognition/skills/tools.rs` — both tools declare `Deferred`
  exposure and a `BuiltinTool` capability.
- `crates/server/src/composition/bootstrap.rs` — both are installed.
- `crates/server/src/runtime/mcp/client.rs::render_resource_result` — the MCP
  resource text is an `<untrusted origin="mcp">` region.
- `tests/integration/governance/` — `[INV-MCP-01]`/`02`/`03`,
  `[INV-SKILLTOOL-01]`.
