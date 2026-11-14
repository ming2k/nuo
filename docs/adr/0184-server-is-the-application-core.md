---
id: ADR-0184
title: "Server Is the Application Core: Module Topology and Product Boundary"
status: accepted
date: 2026-11-14
scope: server, cli, product, documentation
superseded_by: null
supersedes: []
negative_knowledge: true
---

# 0184. Server Is the Application Core: Module Topology and Product Boundary

- Status: Accepted
- Date: 2026-11-14
- Amends: [ADR-0033](0033-canonical-server-entity-dual-hosting-postures-and-endpoint-orthogonality.md), [ADR-0038](0038-thread-entity-server-ssot-fanout-and-orthogonal-action-lifecycle.md)

## Context and Problem Statement

`nuo-server` describes itself as "application-neutral: it holds no product
name, mission, or preset profile". In practice it is the application core:

- `project.rs` materializes a `.nuo/` configuration tree for `/init`.
- `startup.rs` owns the complete built-in slash-command vocabulary.
- `export.rs` writes a `# Nuo thread` document.
- `hypervisor.rs` and `archivist.rs` carry Nuo-specific prompts.
- The provider registry, skills host, credentials host, and provider baselines
  are all composed here.

The crate also has no internal topology: 43 `pub mod`s, 38 root-level source
files, `handlers_*` modules at the root, `slash_handler` beside
`handlers_slash/`, and `monitor` beside `registry/monitor`. A reader cannot
tell the session runtime from the product policy, and no server layering test
exists.

Denying the product role does not make it neutral; it only hides where product
policy lives.

## Architectural Decisions

### 1. `nuo-server` is explicitly the application core

The crate owns the session runtime **and** the product composition. Its
documentation states that. The frontend-neutral part remains: the session
runtime (`runtime/`) knows no binary name, preset, or slash-command vocabulary.

### 2. Module topology is grouped by responsibility

```text
nuo-server/src/
├── composition/   bootstrap, startup, agent_setup, provider_registry,
│                  provider_baselines, kernel_host, skills_host,
│                  credentials_host
├── runtime/       host, registry/, thread_driver/, shutdown, task_ledger,
│                  task_fault_tolerance, background_jobs/, mcp/, offstream,
│                  workspace_journal/, hooks, monitor, usage_cache,
│                  search_lexical, input_completion, archivist,
│                  archivist_service, hypervisor
├── handlers/      chat, history, permission, provider/, slash/, thread,
│                  websearch, slash_extension
├── transport/     serve/, serve_discovery, wire_channel, health_http,
│                  ui_bridge, thread_view
├── product/       project, export, notice/vocabulary policy
└── api.rs         the small facade consumed by the `nuo` binary
```

- `composition/` is the only place that constructs providers, tools, and
  stores.
- `runtime/` owns live session state and lifecycle.
- `handlers/` translates one request family into runtime calls.
- `transport/` owns sockets, wire codecs, discovery, and UI bridges.
- `product/` owns the Nuo-specific decisions that `lib.rs` used to deny.

### 3. The `nuo` binary consumes a facade, not 40 modules

`nuo-server/src/api.rs` re-exports the small surface the binary needs
(`HostOptions`, `ThreadStart`, `run_server`, `serve_discovery`, the provider
registry entry points, and the completion engine). Internal modules are
`pub(crate)` unless a test or the facade needs them.

### 4. Invariants

- **`[INV-SERVER-TOPOLOGY-01]` Grouped Responsibility**: No source file sits at
  `nuo-server/src/*.rs` except `lib.rs` and `api.rs`. Every other module lives
  in one of the five groups.
- **`[INV-SERVER-PRODUCT-01]` Product Boundary**: Product names, `.nuo`
  scaffolding, export branding, and the built-in slash vocabulary live under
  `product/` or `composition/`; `runtime/` must not depend on `product/`.
  A module-graph test enforces the edge.

## Consequences & Trade-offs

- **Positive**: The crate's real role is visible; handlers/transport/runtime
  stop sharing one flat namespace; the `nuo` binary depends on a stable facade;
  product policy has one home and can be reviewed as policy.
- **Negative & Incurred Costs**: A large directory move and path rewrite. No
  behavior changes are permitted.

## Discarded Alternatives (Negative Knowledge)

### Alternative A — Split the product policy into a new `nuo-app` crate
- **Why considered**: Cleaner crate-level separation.
- **Why rejected**: It would create a second application crate whose only
  consumer is `nuo`, and the session runtime still needs the composition root.
  The separation is a module boundary, not a crate boundary.

### Alternative B — Keep the flat layout and document it
- **Why considered**: Zero churn.
- **Why rejected**: A 38-file flat root with two slash-command modules and two
  monitor modules is exactly the ambiguity this ADR removes.

## Links

- [ADR-0033](0033-canonical-server-entity-dual-hosting-postures-and-endpoint-orthogonality.md)
- [ADR-0038](0038-thread-entity-server-ssot-fanout-and-orthogonal-action-lifecycle.md)
- [ADR-0183](0183-composition-root-owns-capability-linkage.md)
