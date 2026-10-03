---
id: ADR-0001
title: "Flat Workspace Topology and Microkernel Capability Architecture"
status: accepted
date: 2026-10-02
scope: workspace/topology, architecture/microkernel, capability/tools
superseded_by: null
negative_knowledge: true
---

# 0001. Flat Workspace Topology and Microkernel Capability Architecture

- Status: Accepted
- Date: 2026-10-02
- Deciders: Nuo Architecture Working Group
- Consulted: Core Contributors, Runtime & Tooling Teams
- Informed: System Architects

---

## Context and Problem Statement

As Nuo evolved from a monolithic cognitive prototype into a multi-interface AI platform (daemon, terminal TUI, web frontend), its monorepo topology suffered from structural complexity and architectural drift:

1. **Deep Directory Nesting Anti-Pattern**: The repository previously divided code into arbitrary `apps/` (daemon, terminal) and `crates/` (libraries) directories. This artificial nesting introduced path complexity, confused contributor mental models, and obscured cross-crate relationships.
2. **The "God Harness" Problem**: All built-in tools (filesystem access, command execution, web retrieval, memory recall, and inter-agent collaboration) were historically concentrated in `nuo-harness/src/tools/`. The execution harness became a bloated, omniscient subsystem with direct dependencies on every disparate capability.
3. **The "Contracts Junk Drawer" Anti-Pattern**: The legacy `nuo-contracts` crate accumulated dead code (`mesh.rs`, obsolete `Tool` traits), misplaced domain substrates (LLM wire formats, MCP proxies), session models, and presentation types. Spanning over 70 files and 36,000 lines of code, any modification within it forced the entire workspace to recompile ("Rebuild Amplification").
4. **Substrate Naming & Responsibility Blur**:
   - Operating system and process management was vaguely named `nuo-system`.
   - LLM dialect compilation and SSE chunk demuxing was named `nuo-model-wire`, misrepresenting its role as a codec.

We require a clean, enduring architectural topology: a flat monorepo structure, a microkernel division of responsibilities, native capability-owned tools, and the complete elimination of catch-all junk drawers.

---

## Decision Drivers

- **Simplicity and Flat Topology**: Eliminate artificial nesting tiers (`apps/`, `crates/`) so all workspace crates reside directly at repository root.
- **Microkernel Harness Separation**: `nuo-harness` must act solely as an orchestrator, policy governor, sandbox supervisor, and human confirmation gate; it must implement only cognitive meta-tools (`ask_user`, `subagent`, `todo`).
- **Decentralized Capability Tool Ownership**: Substrate crates must natively own and expose tools conforming to the zero-agent-runtime `nuo-tool::Tool` trait.
- **Zero Junk-Drawer Tolerance**: Eliminate `nuo-contracts` entirely. Domain types must live in their respective leaf substrates; client/daemon protocol types must live in `nuo-client`.
- **Language-Agnostic Web Boundary**: The web interface communicates with the daemon via standard JSON/WebSocket contracts without brittle compile-time Rust-to-TypeScript code generation macros.

---

## Decision Outcome

We establish a flat, microkernel capability topology across the workspace:

```text
┌────────────────────────────────────────────────────────────────────────┐
│                        Applications & Frontends                        │
│   nuo (Daemon Host & CLI)    nuox (Semantic TUI)    web (SvelteKit)   │
└───────────────────┬────────────────────────────────┬───────────────────┘
                    │                                │
                    ▼                                ▼
┌────────────────────────────────────────────────────────────────────────┐
│                      Subsystems & Host Harness                         │
│   nuo-client (SDK & Wire)  nuo-harness (Ritual Vessel & Policy Engine) │
│   nuo-host (PAL & Sandbox) nuo-persistence (SQLite Store & Memory)     │
│   nuo-providers (Catalog)                                              │
└───────────────────┬────────────────────────────────┬───────────────────┘
                    │                                │
                    ▼                                ▼
┌────────────────────────────────────────────────────────────────────────┐
│                       Substrates & Protocols                           │
│   acp (Coordination Protocol)       nuo-agent (Cognitive Loop)         │
│   nuo-tool & nuo-tool-derive        nuo-model-codec (Dialects & SSE)   │
│   nuo-mcp (MCP Transport)           nuotc (Retained Terminal Canvas)   │
└────────────────────────────────────────────────────────────────────────┘
```

### 1. Flat Cargo Workspace
All workspace member crates reside directly at the repository root. Sibling path dependencies use straightforward relative references (e.g. `path = "nuo-host"`).

### 2. Microkernel Role Boundaries
- **`nuo`**: The root daemon executable and supervisor. Manages process lifecycles, HTTP/WebSocket/UDS server listeners, and daemon detachment.
- **`nuox`**: Autonomous semantic terminal client and rich TUI powered by `nuotc`.
- **`nuo-harness`**: The ritual execution vessel and policy governor. Orchestrates turns, enforces `RiskProfile` gating, injects persona masks, and compacts causal contexts. Harness implements strictly cognitive meta-tools (`ask_user`, `subagent`, `todo`).
- **`nuo-host`** (formerly `nuo-system`): Host execution platform. Manages process isolation (process groups, Windows Job Objects), containment, workspace paths, and exports 7 canonical host tools (`read_text`, `write_file`, `edit_text`, `list_dir`, `find_files`, `search_text`, `execute_command`).
- **`nuo-model-codec`** (formerly `nuo-model-wire`): Dedicated dialect translation and SSE stream codec projecting Session IR to vendor APIs (OpenAI, Anthropic, Google Gemini, DeepSeek).
- **`nuo-persistence`**: SQLite transactional store, database migrations, configuration, and native `recall_memory` tool.
- **`nuo-client`**: Standalone client SDK, wire protocol DTOs, and transport drivers for terminal and external programmatic consumers.

### 3. Complete Dismantling of `nuo-contracts`
The catch-all `nuo-contracts` crate is permanently eliminated:
- Client-daemon interaction DTOs (`AttachAction`, `ControlRequest`, `AgentEvent`) are relocated to `nuo-client`.
- Model parameters and wire formats are relocated to `nuo-model-codec`.
- Session state, history, and token metrics are relocated to `nuo-persistence`.
- Dead legacy code (`mesh.rs`, obsolete `Tool` traits) is discarded without replacement.

> **Implementation Note (2026).** Executed as a consolidation into `nuo-wire`
> rather than a per-subsystem scatter: the domain/wire modules were deeply
> entangled (`events` ↔ `capability` ↔ `subagent` ↔ `monitor` ↔ …), and only
> `nuo-wire` — the zero-I/O wire crate designated by ADR-0005 — sits below every
> consumer, so it is the sole cycle-free destination for the shared contracts.
> `nuo-wire` therefore now owns both the byte envelopes and the shared
> zero-I/O domain vocabulary. The `nuo-contracts` crate was deleted once all
> consumers (`nuo`, `nuox`, `nuo-harness`, `nuo-persistence`, `nuo-providers`,
> `nuo-client`) were flipped to `nuo_wire::`. See
> [ADR-0006](0006-wire-contract-consolidation.md) for the full rationale and the
> amendment of ADR-0005's `[INV-WIRE-01]` ("zero async runtime") to "zero I/O /
> no direct async runtime": `nuo-wire` retains only `tokio-util`'s codec traits
> for length-delimited framing and links `tokio` solely transitively through the
> leaf substrates whose types the contracts name.

---

## Invariants & Behavioral Boundaries

- **`[INV-WS-01] Flat Monorepo Topology`**: All Rust crates in the workspace must reside directly under the repository root. Nested subfolders (`crates/`, `apps/`, `libs/`) are prohibited.
- **`[INV-TOOL-01] Native Capability Tool Ownership`**: Capabilities own their tools. Every tool exposed to agents must implement `nuo-tool::Tool`. Harness must not host OS, network, or persistence tools.
- **`[INV-ARCH-01] Catch-All Junk Drawer Ban`**: No crate named `contracts`, `common`, `shared`, or `core` may be created. Types must belong to specific domain or capability crates.
- **`[INV-HOST-01] Sandboxed Execution Boundary`**: All local file modifications and command executions must execute through `nuo-host` security boundaries.

---

## Negative Knowledge & Rejected Alternatives

### 1. Hierarchical Monorepo with `apps/` and `crates/`
- **Why considered**: Common industry practice to separate executable binaries from shared libraries.
- **Why rejected**: Introduces artificial directory depth, fragile relative path configurations (`../../crates/...`), and circular dependency temptations across app boundaries. A flat Cargo workspace provides superior clarity.

### 2. Retaining the "God Harness" Tool Pattern
- **Why considered**: Keeps all tool implementations in one place for quick access by the agent runtime.
- **Why rejected**: Creates circular dependency hazards, inflates harness compilation time, and prevents subagents or third-party embeddings from reusing ACP or OS tools without pulling in the entire cognitive execution engine.

### 3. Renaming `nuo-contracts` to `nuo-protocol` or Retaining a Shared Crate
- **Why considered**: Desired an easy place to put cross-crate types and avoid duplicate struct definitions.
- **Why rejected**: "Protocol" is an equally ambiguous umbrella name. A shared types crate inevitably becomes a junk drawer where developers dump types to avoid thinking about domain boundaries, causing catastrophic rebuild cascades across all 14 crates.

### 4. Hard-Coupling Frontend Types via `ts-rs`
- **Why considered**: Automated TypeScript generation from Rust structs.
- **Why rejected**: SvelteKit web frontends should not require a Rust toolchain. Rust structs often contain internal domain invariants ill-suited for frontend state management. Standard JSON/WebSocket protocol contracts provide cleaner decoupled engineering hygiene.
