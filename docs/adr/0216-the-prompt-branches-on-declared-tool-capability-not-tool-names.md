---
id: ADR-0216
title: "The Prompt Branches on Declared Tool Capability, Not Tool Names"
status: accepted
date: 2026-12-19
scope: protocol/context, engine/cognition, tool/exposure
superseded_by: null
supersedes:
  - "`InstructionContext::tool_names` as the prompt policy's tool input"
negative_knowledge: true
---

# 0216. The Prompt Branches on Declared Tool Capability, Not Tool Names

- **Status**: Accepted
- **Date**: 2026-12-19
- **Amends**: [ADR-0126](0126-provider-owned-model-tunables-closed-shape-vocabulary-over-open-knob-namespace.md)
  (the firewall against core branches on vendor strings now covers the prompt),
  [ADR-0164](0164-instructions-are-one-pipeline-over-system-and-project-sources.md)
- **Derives from**: [RFC-0004](../rfc/0004-context-sources-declaration-budget-and-ownership.md)

## Context and Problem Statement

The prompt policy conditioned its guidance on literal tool names:

```rust
if ctx.tool_names.iter().any(|name| name == "find_files") { /* … */ }
if ctx.tool_names.iter().any(|name| name == "read_url" || name == "search_web") { /* … */ }
```

Six such branches existed across two sections. Three failures follow, and all
three are silent:

1. **A rename deletes guidance.** `Tool` exposes `aliases()` and
   `BuiltinTool::matches`, but the prompt compared raw strings. Renaming
   `search_text`, or shipping a tool under an alias, silently drops the paragraph
   that told the model to prefer it — and no test notices.
2. **A prompt branch is an undeclared provider of framework behaviour.**
   `[INV-TUNE-01]` exists precisely to ban framework-core branches on a vendor or
   family string, because they force the core to change whenever a plugin does.
   The same shape was present, ungated, in the file that writes the prompt.
3. **The cacheable prefix became volatile.** `tool_names` included tools activated
   mid-session by `tool_search`. Activating one deferred MCP tool therefore
   changed the rendered `fixed` prefix and invalidated the provider's cache for a
   reason unrelated to policy.

The prompt needs to know *capability* ("a native directory lister is available"),
not *identity* ("a tool called `find_files` exists").

## Architectural Decision

### 1. The prompt reads a declared `ToolSurface`, never a name list

`ContextInputs` carries `ToolSurface`:

```rust
pub struct ToolSurface {
    /// Declared native capabilities, from `Tool::builtin()`. The ONLY tool input
    /// a context source may read.
    native: BTreeSet<BuiltinTool>,
    /// Whether the active channel supports parallel tool calls.
    parallel: bool,
    /// Whether any admitted tool spawns a subagent.
    spawns_subagent: bool,
    /// Number of deferred tools currently activated. Diagnostic only; never read
    /// by prompt policy.
    deferred_active: usize,
}
```

`ToolSurface` is built from `Tool::builtin()`, which is a typed
`Option<BuiltinTool>` already implemented by every native tool
(`crates/tool/src/lib.rs`). It is not built from `Tool::name()`.

### 2. Prompt sources branch on `BuiltinTool` variants

```rust
if surface.has_native(BuiltinTool::FindFiles) || surface.has_native(BuiltinTool::SearchText) {
    // native inspection protocol
}
```

A `BuiltinTool` variant is a compiled-in capability, so a new MCP tool with an
arbitrary name cannot enter the branch, and renaming a native tool's wire name
does not change it.

### 3. The raw name list is not reachable from a context source

`ContextInputs` has no `tool_names` field. There is no accessor that returns
tool-name strings, so a future source cannot reintroduce the defect without
editing the declaration vocabulary and the `[INV-CTXSRC-04]` test that inspects
it. `deferred_active` exists for `/context` and telemetry only; a governance test
asserts no source declares it in `inputs()`.

### 4. Activating a deferred tool does not change the instruction prefix

Because `deferred_active` is not a declared input, `tool_surface` stability is
`Route` and a `tool_search` activation cannot move the `fixed` bytes. The tool
*schemas* change (they are protocol fields, not text), which is the correct and
already-supported behaviour.

## Normative Invariants

- **`[INV-TOOLSURFACE-01]` No Name Branch** — A context source must not compare a
  tool name. Enforced structurally: `cognition::context` has no access to a name
  list, and the governance test fails if `Tool::name` or `BuiltinTool::as_str`
  appears in a context source.
- **`[INV-TOOLSURFACE-02]` Capability, Not Presence** — Guidance is gated on a
  declared native capability. A tool that declares no `BuiltinTool` cannot
  condition prompt text.
- **`[INV-TOOLSURFACE-03]` Prefix Immutability Under Activation** — Activating or
  deactivating a deferred tool must leave `fixed` byte-identical.

## Consequences & Trade-offs

- **Positive**: renaming or aliasing a native tool can no longer delete guidance;
  the prompt no longer changes when a deferred tool is activated; the framework
  core no longer carries a name branch, matching `[INV-TUNE-01]`.
- **Negative & Incurred Costs**: a *third-party* tool cannot contribute prompt
  guidance. That is intentional — such a tool must describe itself in its own
  `description`, which is the protocol field designed for it. If a future
  requirement is genuine, it arrives as a `BuiltinTool` variant with an owner.
- **Neutral**: `Tool::name()` remains the dispatch identity. Nothing about
  dispatch, aliasing, or `tool_search` changes.

## Discarded Alternatives (Negative Knowledge)

### Alternative A — Match names case-insensitively with the alias table

- **Why considered**: Smallest change; keeps the existing sections.
- **Why rejected**: Alias matching still hardcodes a capability in the prompt,
  still fails for a genuinely new tool, and still lets an MCP tool with a
  colliding name trigger a native branch.

### Alternative B — A `capabilities() -> &[&'static str]` method on `Tool`

- **Why considered**: Extensible without touching the enum.
- **Why rejected**: Reintroduces stringly-typed capability, which is the defect.
  `BuiltinTool` already exists as the typed vocabulary and is already implemented
  by every native tool.

### Alternative C — Keep `tool_names` but stop counting deferred tools

- **Why considered**: Fixes only the cache volatility.
- **Why rejected**: Leaves the rename defect and the framework-branch defect in
  place.

### Alternative D — Pass the whole `Tool` trait object to `render`

- **Why considered**: Maximum information.
- **Why rejected**: Destroys purity (`[INV-CTXSRC-08]`), makes the memo key
  uncomputable, and hands every source the ability to call `execute`.

## Compliance and Verification

- `crates/engine/src/cognition/context/tests.rs` — a renamed/aliased native tool
  still activates the same guidance; activating a deferred tool leaves `fixed`
  byte-identical.
- `tests/integration/governance/` — `[INV-TOOLSURFACE-01]` scans
  `crates/engine/src/cognition/context/` for `name()` / `as_str()` comparisons
  against a tool string.

## Links

- [RFC-0004](../rfc/0004-context-sources-declaration-budget-and-ownership.md)
- [ADR-0008](0008-single-tool-contract.md)
- [ADR-0126](0126-provider-owned-model-tunables-closed-shape-vocabulary-over-open-knob-namespace.md)
- [ADR-0179](0179-one-capability-one-producer-and-no-ghost-builtins.md)
- [ADR-0208](0208-tool-contract-validation-snapshots-and-deferred-discovery.md)
- [ADR-0215](0215-context-sources-declare-moment-topic-rank-stability-and-budget.md)
