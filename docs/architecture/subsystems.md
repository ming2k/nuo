# Subsystem Architecture: Nuo System Blueprint

- Status: Living Blueprint
- Last Updated: 2026-10-02
- Scope: workspace/topology, architecture/subsystems, substrate/tools, microkernel/topology, terminal/nuotc
- Maintainers: Nuo Architecture Working Group

---

## 1. System Overview & Monorepo Topology

Nuo (傩) is a self-contained AI session daemon, semantic terminal, and web host system. The project name derives from the Chinese Pinyin for 傩 (*Nuó*)—traditionally a ritual for expelling perils and disasters, later regarded as a ceremony for communing with the divine. According to traditional Nuo customs, the priest dons a Nuo mask (傩面) to convey intent according to the image and persona embodied by the mask. Nuo draws its core inspiration from this: using an execution harness to employ different identities to communicate with intelligence.

It is organized as a flat Cargo workspace containing 14 specialized crates arranged across three distinct tiers:

```text
┌────────────────────────────────────────────────────────────────────────┐
│                        Applications & Frontends                        │
│   nuo (Daemon Host & CLI)    nuox (Semantic TUI)    web (SvelteKit)   │
└───────────────────┬────────────────────────────────┬───────────────────┘
                    │                                │
                    ▼                                ▼
┌────────────────────────────────────────────────────────────────────────┐
│                      Subsystems & Host Harness                         │
│   nuo-client (SDK & Wire)  nuo-harness (Execution & Policy Harness)    │
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
- **`nuo-harness`**: Host execution harness: manages agent identity/role projection, permission brokering, human confirmation checkpoints, execution sandboxing, tool scheduling, and causal context compaction.
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
| **ADR-0001** | Flat Workspace Topology and Microkernel Capability Architecture | Abolish nested dirs for flat workspace, enforce microkernel harness role, decentralize capability tools, and eliminate catch-all contracts | Workspace topology, `nuo-harness`, `nuo-host`, `nuo-model-codec` |
| **ADR-0002** | Canonical Agent Coordination Protocol (ACP) Standard | Establish ACP specification with URI addressing, HMAC envelopes, fabric channels, and native collaboration tools | `acp`, inter-agent collaboration |
| **ADR-0003** | Autonomous Retained-Mode Terminal Canvas Engine (Nuotc) | Retained-mode 2D terminal canvas with differential double-buffered rendering strictly decoupled from AI domain concepts | `nuotc`, `nuox` |
| **ADR-0004** | Federated Cluster SemVer and Release Topology | Partition workspace into 4 SemVer clusters with dual-resolving version and path dependencies | Release engineering, manifests, versioning |
