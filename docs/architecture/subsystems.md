# Subsystem Architecture: Nuo System Blueprint

- Status: Living Blueprint
- Last Updated: 2026-10-02
- Scope: workspace/topology, architecture/subsystems, substrate/tools, microkernel/topology, terminal/nuotc
- Maintainers: Nuo Architecture Working Group

---

## 1. System Overview & Monorepo Topology

Nuo is a self-contained AI session daemon, semantic terminal, and web host system. It is organized as a flat Cargo workspace containing 15 specialized crates arranged across three distinct tiers:

```text
┌────────────────────────────────────────────────────────────────────────┐
│                        Applications & Frontends                        │
│   nuo (Daemon Host & CLI)    nuox (Semantic TUI)    web (SvelteKit)   │
└───────────────────┬────────────────────────────────┬───────────────────┘
                    │                                │
                    ▼                                ▼
┌────────────────────────────────────────────────────────────────────────┐
│                      Subsystems & Host Harness                         │
│   nuo-client (SDK)         nuo-harness (Policy & Orchestrator)         │
│   nuo-host (PAL & Sandbox) nuo-persistence (SQLite Store & Memory)     │
│   nuo-providers (Catalog)  nuo-contracts (Pure Domain Types)           │
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

---

## 2. Core Subsystems and Domain Responsibilities

### 2.1 Protocols & Substrates
- **`acp`** (*Agent Coordination Protocol*): Defines canonical inter-agent communication, addressing (`agent://`, `acp://`), tamper-evident HMAC signature envelopes, mailboxes, and multi-party channels. Natively exports standard peer-to-peer and channel collaboration tools.
- **`nuo-agent`**: Implements the autonomous cognitive loop (think → act → observe), session turns, channel notification policies, two-tier context hygiene, and token compaction.
- **`nuo-tool` & **`nuo-tool-derive`**: Zero-runtime tool contracts defining schema, hazard risk profiles (`RiskProfile`), execution scopes (`ToolScope`), and cancellation tokens. `nuo-tool-derive` provides compile-time derive macros for JSON schemas.
- **`nuo-model-codec`**: Multi-vendor wire protocol serialization (OpenAI, Anthropic, Google Gemini, DeepSeek) and SSE stream demuxing. Completely decoupled from agent cognitive loops.
- **`nuo-mcp`**: Native Model Context Protocol client and server transport over stdio and JSON-RPC 2.0.
- **`nuotc`**: Retained-mode 2D terminal canvas, differential rendering pipeline, and Flexbox layout solver. Free of AI domain vocabulary.

### 2.2 Applications & Frontends
- **`nuo`**: The unified daemon binary and service engine. Manages daemon detachment, session registries, SQLite persistence, and dual control planes (WebSocket and Unix Domain Socket).
- **`nuox`**: High-performance semantic terminal client and rich TUI harness built on `nuotc`. Also operates as a headless one-shot runner (`nuox run`).
- **`web`**: Decoupled SvelteKit browser interface providing live session monitoring, streaming chat, and inline tool approval workflows.

### 2.3 Subsystems & Host Infrastructure
- **`nuo-client`**: Standalone client SDK and wire DTOs providing discovery, connection management, event streaming, and command completion for frontends and external integrations.
- **`nuo-contracts`**: Pure zero-I/O domain types, events, and traits shared across multiple layers without filesystem or network dependencies.
- **`nuo-harness`**: Host execution harness: permission brokering, human confirmation checkpoints, execution sandboxing, tool scheduling, and causal context compaction.
- **`nuo-persistence`**: SQLite transactional store, database migrations, configuration parsing, role memory, and full-text search indexing.
- **`nuo-providers`**: Multi-vendor model catalog resolution, OAuth2/PKCE authentication flows, credential management, and concrete provider adapters.
- **`nuo-host`**: Host execution environment: process supervision (process groups / Windows Job Objects), workspace sandboxing, cross-platform paths, secure file operations, and native host tools.

---

## 3. Terminal Canvas Subsystem (`nuotc`)

`nuotc` provides retained-mode terminal rendering structured into six decoupled, acyclic modules:

```text
┌────────────────────────────────────────────────────────┐
│                      nuotc (Root)                      │
│   Unified Retained-Mode Terminal Canvas & Diff Engine  │
└───────────────────────────┬────────────────────────────┘
                            │
        ┌───────────────────┼───────────────────┐
        ▼                   ▼                   ▼
 ┌──────────────┐    ┌──────────────┐    ┌──────────────┐
 │      ui      │    │   widgets    │    │    layout    │
 │ Retained     │    │ Block, Para, │    │ Rect, Flex,  │
 │ Scene Graph  │    │ Clear, Spans │    │ Anchor       │
 └──────┬───────┘    └──────┬───────┘    └──────┬───────┘
        │                   │                   │
        └───────────────────┼───────────────────┘
                            │
        ┌───────────────────┴───────────────────┐
        ▼                                       ▼
 ┌──────────────┐                        ┌──────────────┐
 │   terminal   │                        │    render    │
 │ Backend,     │                        │ Run-length   │
 │ Frame, Loop  │                        │ Diff, Driver │
 └──────┬───────┘                        └──────┬───────┘
        │                                       │
        └───────────────────┬───────────────────┘
                            │
                            ▼
                     ┌──────────────┐
                     │    buffer    │
                     │ Cell, Grid,  │
                     │ Text, Glyph  │
                     └──────────────┘
```

- **`buffer`**: Compact cell memory (`Cell`, `Color`, `Modifier`, `Style`), 2D `Grid` with write-marks-dirty (`dirty_col`) line tracking, Unicode grapheme clustering, and standard glyph tables.
- **`layout`**: Spatial geometry (`Rect`), Flexbox layout solver (`Flex`, `FlexItem`, `SolvedFlex`), and collision-free anchored popups (`AnchorPlacement`, `compute_anchored_rect`).
- **`render`**: Pure run-length packed differential rendering (`diff`), display list recording (`Canvas`, `DisplayList`, `RenderNode`), escape code drivers, and standards-based terminal profiles (`TerminalProfile`).
- **`terminal`**: Low-level terminal mode management (`Backend`, `Bce`), alternate screen lifecycles, and double-buffered `Frame` loop.
- **`widgets`**: Declarative UI primitives (`Block`, `Paragraph`, `Clear`) and structural composition containers (`Column`, `Row`, `Stack`, `Container`, `Spacer`, `Divider`).
- **`ui`**: Retained scene graph (`Scene`) and component event dispatch runtime (`UiRuntime`).

---

## 4. Architectural Invariants Constitution

- **`[INV-WS-01]` Self-Contained Workspace**: All workspace crates and substrates must resolve within the local repository without relying on external sibling path dependencies (`../`).
- **`[INV-ARCH-FLAT-01]` Flat Workspace Topology**: No intermediate directory grouping (`apps/` or `crates/`). Every crate lives directly at the workspace root to eliminate path indirection.
- **`[INV-HOST-01]` Sandbox & Containment**: All local command execution and file mutations must pass through `nuo-host` sandbox and containment boundaries.
- **`[INV-TOOL-01]` Decentralized Capability Tools**: Tools are natively owned and implemented by their capability crates (`acp`, `nuo-host`, `nuo-persistence`) using the zero-runtime `nuo-tool` standard, rather than concentrated in a monolithic harness.
- **`[INV-VER-01]` Federated Cluster SemVer**: Autonomous substrates (`nuotc`, `acp`) maintain independent versions; the host suite (`nuo`, `nuox`, `nuo-*`) evolves in unified lockstep.
- **`[INV-MOD-01]` Zero Domain Leakage**: Terminal graphics substrates (`nuotc`) and wire codecs (`nuo-model-codec`) must remain completely free of application-level agent concepts.
- **`[INV-MOD-02]` Ghost-Free CJK Cells**: Wide-character trailing cells are populated by the writer with matching background attributes; diff never emits unowned padding spaces.
- **`[INV-MOD-03]` Authoritative Retained Grid**: The retained `Grid` is the single source of truth for desired visual state; dirty line tracking eliminates redundant full-frame rasterization.

---

## 5. Architectural Lineage & ADR Registry

| ADR | Title | Decision Summary | Primary Impact |
| :--- | :--- | :--- | :--- |
| **ADR-0001** | Unify Model Wire Protocol and Provider Boundary | Decouple model wire transport into pure codec; isolate application provider logic | `nuo-model-codec`, `nuo-providers` |
| **ADR-0002** | Unify Tool Contracts and MCP Runtime | Standardize tool definitions and MCP transport on lightweight substrate | `nuo-tool`, `nuo-mcp` |
| **ADR-0003** | Establish Apps Boundary (*Superseded*) | Proposed `apps/` hierarchy; subsequently superseded by flat workspace | Workspace structure |
| **ADR-0004** | Pragmatic Crate Granularity | Consolidate single application crates; isolate provider dependencies | `nuo`, `nuox`, `nuo-providers` |
| **ADR-0005** | Flattened Workspace Architecture | Abolish `apps/` and `crates/` directories; flatten all crates to workspace root | Workspace topology |
| **ADR-0006** | Absorb Substrates and Establish ACP | Ingest agent communication protocols and substrates into self-contained monorepo | `acp`, `nuo-agent`, `nuo-tool` |
| **ADR-0007** | Ingest Terminal Canvas Engine (`nuotc`) | Ingest nuotc into workspace root, restoring hermetic workspace invariant `[INV-WS-01]` | `nuotc`, workspace |
| **ADR-0008** | Federated Cluster SemVer Topology | Partition crates into 3 versioning clusters with dual-resolving dependencies | Release engineering, manifests |
| **ADR-0009** | Tool Substrate Consolidation | Establish flat sibling derive macro topology; eliminate redundant crates | `nuo-tool`, `nuo-tool-derive` |
| **ADR-0010** | Decentralized Capability Tools & Microkernel | Decentralize tools into capability substrates; rename `nuo-host` & `nuo-model-codec` | `nuo-host`, `nuo-model-codec`, `acp` |
