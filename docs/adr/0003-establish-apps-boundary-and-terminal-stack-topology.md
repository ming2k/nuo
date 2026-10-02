---
id: ADR-0003
title: "Establish Apps Boundary and Terminal Stack Topology"
status: accepted
date: 2026-10-01
scope: architecture/apps, terminal/nuox, client/facade
superseded_by: null
negative_knowledge: true
---

# 0003. Establish Apps Boundary and Terminal Stack Topology

- Status: Accepted
- Date: 2026-10-01
- Deciders: Nuo Architecture Working Group
- Consulted: Frontend & Runtime Team
- Informed: Core Contributors

---

## Context and Problem Statement

As Nuo evolved from a monolithic single-process agent prototype into a decoupled, multi-client daemon architecture, several structural anti-patterns and naming ambiguities accumulated across the crate topology:

1. **Hyper-Generic & Misleading Naming (`nuo-engine`)**:
   `crates/nuo-engine` resided at the top-level crate tier. In an autonomous AI agent framework, an "engine" crate is intuitively understood to house workflow orchestration, cognitive loops, or inference engines. In reality, `nuo-engine` was strictly an in-house terminal rendering engine (retained-mode cell grid, dirty line tracking, diff algorithms, and crossterm escape emitters) consumed exclusively by the `nuox` TUI client.
2. **Missing Frontend Application Boundary (`apps/`)**:
   Terminal application crates (`nuox`, `nuo-engine`) were placed directly in `crates/` alongside foundational system services (`nuo-agent`, `nuo-runtime`, `nuo-contracts`, `nuo-persistence`), while the web frontend lived loosely at root (`web/`). There was no structural boundary separating deployable/interactive user applications from core host libraries.
3. **Leaky God-Crate Dependencies (`nuo-runtime`)**:
   `nuox` required client-side transport utilities (`discover`, `monitor_stream`, `complete_slash_items`, `UiBridge`). Because these functions were co-located in the monolithic `nuo-runtime` crate alongside daemon servers, WebSocket listeners, background job schedulers, and SQLite stores, `nuox` was forced to pull in the entire server runtime dependency graph directly.

We need a long-term, uncompromised architecture establishing clean application boundaries, accurate domain semantics, and strict client/server decoupling.

---

## Decision Drivers

- **Orthogonal System Layering**: Enforce a strict distinction between end-user frontends (`apps/*`) and core domain/runtime infrastructure (`crates/*`).
- **Semantic Precision**: Eliminate misleading "junk drawer" or hyper-generic names. A terminal layout/diff engine must declare its true domain.
- **Client/Daemon Decoupling**: Isolate client-side connection and completion mechanics behind a dedicated `nuo-client` facade, cutting direct frontend dependencies on daemon internals.
- **Zero Legacy Burden**: Eliminate transitional shims, dead aliases, and redundant compatibility layers.

---

## Considered Options

- **Option 1: Status Quo (Flat Crate Structure with Ambiguous Naming)**
  Keep `crates/nuo-engine` and `crates/nuox` in `crates/` and continue having `nuox` import `nuo-runtime::client`.
- **Option 2: Rename `nuo-engine` to `nuo-tui` in `crates/` without Application Hierarchy**
  Change the crate name but keep the flat `crates/` structure without establishing an `apps/` boundary.
- **Option 3 (Chosen): First-Class `apps/` Hierarchy with Dedicated `nuox-render` and `nuo-client` Facade**
  1. Establish `apps/nuox/` containing the terminal application (`apps/nuox/app`) and its dedicated rendering engine (`apps/nuox/render`, renamed to `nuox-render`).
  2. Restore and enforce the lightweight `crates/nuo-client` facade for daemon discovery, streaming, and command completion.
  3. Enforce an automated architectural lint asserting that frontends never depend directly on `nuo-runtime`.

---

## Decision Outcome

Chosen Option: **Option 3**.

### 1. Structural Realignment
- Relocated terminal rendering subsystem from `crates/nuo-engine` to `apps/nuox/render`.
- Renamed the package from `nuo-engine` to `nuox-render` and updated all internal symbols, diff utilities, and tests.
- Relocated terminal client application from `crates/nuox` to `apps/nuox/app`.
- Relocated daemon host CLI binary from `crates/nuo` to `apps/nuo`.
- Relocated web frontend application from `web/` to `apps/web/` and updated `nuo-contracts` TypeScript typegen targets.

### 2. Client Decoupling & Architectural Firewall
- Re-established `crates/nuo-client` as the canonical client surface for the Nuo daemon.
- Migrated all `nuox` references from `nuo_runtime::client` to `nuo_client`.
- Added an invariant boundary unit test in `nuo-client` verifying that `apps/nuox/app/Cargo.toml` contains no direct dependency on `nuo-runtime`.

### 3. Cargo Workspace Integration
Updated root `Cargo.toml` workspace members and dependencies:
```toml
[workspace]
members = [
    "apps/nuo",
    "apps/nuox/app",
    "apps/nuox/render",
    "crates/nuo-agent",
    "crates/nuo-client",
    "crates/nuo-contracts",
    "crates/nuo-persistence",
    "crates/nuo-platform",
    "crates/nuo-providers",
    "crates/nuo-runtime",
]
default-members = ["apps/nuo", "apps/nuox/app"]
```

---

## Pros and Cons of the Options

### Option 3 (Chosen)
- **Positive**: Complete clarity in project topology: `apps/` contains frontends; `crates/` contains core infrastructure.
- **Positive**: `nuox-render` explicitly communicates its scope and cannot be confused with the cognitive agent engine.
- **Positive**: `nuox` compiles against a decoupled client facade, preventing daemon server entanglement.
- **Positive**: Enforces structural invariants with automated unit tests.
- **Negative**: Requires migrating import paths from `nuo_engine` to `nuox_render`.

### Option 1 (Status Quo)
- **Positive**: Zero short-term migration diffs.
- **Negative**: Perpetuates misleading cognitive overhead where new contributors mistake `nuo-engine` for an agent engine.
- **Negative**: Couples client frontends directly to daemon runtime implementation details.

---

## Rejected Alternatives

### 1. Naming the Crate `nuo-tui` in Global `crates/`
- **Why Rejected**: Retaining the rendering engine under `crates/` falsely implies it is a general-purpose, reusable public TUI library for arbitrary external applications. In reality, it is tailored specifically with retained dirty-line tracking and widget abstractions for `nuox`. Positioning it under `apps/nuox/render` preserves ownership cohesion.

### 2. Retaining Backward-Compatible Re-export Shims (`nuo-engine -> nuox-render`)
- **Why Rejected**: In accordance with the "no legacy burden, uncompromised" directive, retaining compatibility alias crates adds dead maintenance weight and prevents clean compiler-enforced boundary verification.

---

## Invariants & Validation Contracts

1. **`[INV-ARCH-TUI-01] Frontend Decoupling`**:
   The `nuox` application manifest must never depend directly on `nuo-runtime`. All daemon communication must pass through `nuo-client`.
2. **`[INV-ARCH-APPS-01] Render Scope Invariance`**:
   `nuox-render` must remain zero-agent-knowledge: it must never import `nuo-agent`, `nuo-runtime`, or `nuo-providers`.
