---
id: ADR-0183
title: "Composition Root Owns Capability Linkage; Engine Depends on Contracts"
status: accepted
date: 2026-11-14
scope: engine, server, tools, providers, workspace
superseded_by: null
supersedes: []
negative_knowledge: true
---

# 0183. Composition Root Owns Capability Linkage; Engine Depends on Contracts

- Status: Accepted
- Date: 2026-11-14
- Amends: [ADR-0012](0012-domain-aligned-capability-topology-and-tool-asymmetry-elimination.md), [ADR-0027](0027-provider-definition-single-source-and-adapters-retirement.md)

## Context and Problem Statement

Capability crates (`tools/nuo-tool-*`, `providers/nuo-provider-*`) are leaves.
The shipped binary links them through `inventory` registration, so exactly one
crate must own the link edges — the composition root.

Today the cognitive runtime is also a link root:

- `nuo-engine` depends on `nuo-tool-fs`, `nuo-tool-exec`, `nuo-tool-web`, and
  `nuo-tool-ast`, and re-exports all four with `pub use ...::*`.
- `nuo-server` also depends on `nuo-tool-fs`, `nuo-tool-exec`, and
  `nuo-tool-ast`; the actual tool instantiation lives in
  `nuo-server/src/bootstrap.rs`.
- `cargo tree -p nuo --no-default-features` still contains 14 tree-sitter
  dependency lines through `nuo-engine → nuo-tool-ast`, even though the "lean"
  build links no terminal stack.

The engine is not the composition root. Linking every capability into it
forces the cognition layer to compile the web and AST stacks and makes the
"decentralized capability tools" invariant nominal.

## Architectural Decisions

### 1. `nuo-server` is the only shipped capability link root

`nuo-server` (the composition root) depends on all four tool crates and all
provider crates. It assembles the toolset and the provider registry. The `nuo`
binary depends on `nuo-server`; it does not link a capability directly.

### 2. `nuo-engine` depends only on the tool contract

`nuo-engine` depends on `nuo-tool` (the trait/context contract), not on any
`nuo-tool-*` capability crate. Its `tools` module owns only the cognitive
meta-tools that require the agent runtime: `ask_user`, `recall_memory`, and
`todo`. The blanket `pub use nuo_tool_*::*` re-exports are deleted.

Engine unit tests that need a concrete tool take the capability crate as a
**dev-dependency**. Dev-dependencies do not enter the shipped graph.

### 3. Provider leaves have a declared allowlist

The blanket "providers are leaves" statement is refined:

- `nuo-provider-catalog` may depend on provider crates because it is the
  shape-keyed catalog dispatcher (ADR-0027 §2).
- `nuo-provider-codebuddy-{cn,intl}` may depend on
  `nuo-provider-codebuddy-auth` because they share one authentication base.

No other vendor-to-vendor edge is allowed. A test reads every
`providers/*/Cargo.toml` and fails on an edge outside the allowlist.

### 4. Invariants

- **`[INV-ARCH-TOOL-LINK-01]` Composition-Root Linkage**: A crate that is not
  the composition root may not declare a dependency on `nuo-tool-*` (except
  `nuo-tool` itself). The engine manifest is the canonical check.
- **`[INV-ARCH-TOOL-LINK-02]` Engine Capability Budget**: The normal
  `nuo-engine` dependency graph contains no `nuo-tool-*` crate and no
  `tree-sitter*` crate. The composition root may still link them for the
  shipped binary; the invariant is about the cognition layer.
- **`[INV-ARCH-PROV-LEAF-01]` Provider Edge Allowlist**: Provider-to-provider
  edges are exactly the catalog and codebuddy-auth exceptions.

## Consequences & Trade-offs

- **Positive**: The cognition layer stops compiling the web/AST stacks; the
  lean binary is genuinely lean; capability registration has one owner.
- **Negative & Incurred Costs**: Engine tests gain dev-dependencies; the
  server's manifest grows by one tool crate (`nuo-tool-web`). No behavior
  changes are permitted.

## Discarded Alternatives (Negative Knowledge)

### Alternative A — Keep tool linkage in the engine and feature-gate it
- **Why considered**: Avoids touching the server's composition root.
- **Why rejected**: A feature gate still leaves the engine as a link root and
  creates a second, invisible build matrix. The composition root already exists.

### Alternative B — Move tool instantiation into each tool crate
- **Why considered**: Keeps ownership local.
- **Why rejected**: Instantiation needs session context, workspace roots, and
  the service bag — all composition concerns. The leaf owns the tool, not the
  session wiring.

## Links

- [ADR-0012](0012-domain-aligned-capability-topology-and-tool-asymmetry-elimination.md)
- [ADR-0027](0027-provider-definition-single-source-and-adapters-retirement.md)
- [ADR-0180](0180-single-ownership-of-shared-vocabulary.md)
