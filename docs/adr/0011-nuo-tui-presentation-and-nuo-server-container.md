---
id: ADR-0011
title: "Nuo-Tui Presentation Decoupling, Nuo-Server Extraction, and Unified Host Architecture"
status: accepted
date: 2026-10-03
scope: workspace/topology, architecture/cli, tui/nuo-tui, server/nuo-server
superseded_by: null
negative_knowledge: true
---

# 0011. Nuo-Tui Presentation Decoupling, Nuo-Server Extraction, and Unified Host Architecture

- Status: Accepted
- Date: 2026-10-03
- Deciders: Nuo Architecture Working Group
- Consulted: Interface, Runtime, and Protocol Teams
- Informed: System Architects, Release Engineering
- Fulfills & Finalizes: [ADR-0005](0005-unified-binary-and-concentric-runtime-architecture.md) §1–§4 (`[INV-CLI-01]`)
- Complements: [ADR-0010](0010-harness-decomposition-and-agent-unification.md)

---

## Context and Problem Statement

ADR-0005 codified the vision of a **Unified Binary Singleton** (`[INV-CLI-01]`: `nuo` as the single executable entry point) and a concentric lifecycle separation. Following the harness decomposition and agent unification of ADR-0010, two structural anomalies remained in the workspace topology:

1. **`nuox` Crate Nomenclature and Binary Residue**:  
   The workspace retained `nuox` as a crate name (109,000+ lines) and kept `nuox/src/main.rs`. While `nuo` was updated to delegate interactive sessions to `nuox::runner`, the presence of `nuox` as an independent crate name and binary target perpetuated dual-mindshare confusion and broke the canonical `nuo-*` crate naming convention (`nuo-wire`, `nuo-host`, `nuo-client`, etc.).

2. **Conflation of CLI Launcher and Server Engine in `nuo`**:  
   The `nuo` application crate (38,000+ lines) simultaneously housed the CLI command-line launcher (`cli.rs`, `main.rs`, `supervisor.rs`) and the entire background server runtime:
   - WebSocket and Unix Domain Socket (UDS) connection gateways (`serve.rs`)
   - The multi-session registry and actor router (`registry.rs`)
   - Active session loop driving and IPC protocol dispatch (`session_driver.rs`)
   - Graceful shutdown and draining (`shutdown.rs`)
   - Background jobs, hypervisors, and archivist services.

   This monolithic mixing prevented lean, headless, or containerized server deployments (e.g. running a dedicated server container without linking CLI and TUI machinery) and violated the separation of concerns between client orchestration and service hosting.

3. **Dataflow Ambiguity**:  
   Clarification was needed on the exact boundary between the interactive presentation layer, the client SDK, the wire contracts, and the server runtime: `nuo-tui` must consume `nuo-client` rather than reimplementing daemon discovery or transport connections, and `nuo-client` must speak to `nuo-server` strictly via `nuo-wire` frames.

We must eliminate all legacy naming and binary residue, finalize the transition specified in ADR-0005, and establish clean, uncompromising workspace boundaries.

---

## Decision Drivers

- **Canonical Workspace Naming**: All workspace crates must conform to the unified prefix: `nuo-*` for libraries, and `nuo` for the single public executable.
- **Pure Presentation View Library**: `nuo-tui` (formerly `nuox`) owns terminal graphics, layout, markdown rendering, and user keymaps. It owns zero binary targets and delegates all transport and session discovery to `nuo-client`.
- **Autonomous Headless Server Container (`nuo-server`)**: The daemon hosting engine (sockets, session registry, IPC, database coordination) is extracted into `nuo-server`. It has zero dependencies on terminal rendering (`nuotc`, `crossterm`).
- **Lean CLI Coordinator (`nuo`)**: The `nuo` binary becomes a lean coordinator that dispatches into `nuo-tui`, `nuo-client`, and `nuo-server`.
- **Zero Legacy Burden**: Eliminate `nuox/src/main.rs` and eradicate dual-binary artifacts.

---

## Considered Options

### Option 1: Keep Server Inside `nuo`, Only Rename `nuox` to `nuo-tui`
- Rename `nuox` to `nuo-tui` to fix naming consistency.
- Leave server hosting logic inside `nuo/src/`.
- *Assessment*: Incomplete. Leaves `nuo` as a monolithic 40k LOC crate mixing client, server, and CLI launcher, preventing clean containerization.

### Option 2: Full Decoupling & Alignment with Concentric Topology (Chosen)
- Rename `nuox` to `nuo-tui` as a pure library crate; remove `main.rs`.
- Extract server hosting and session registry into `nuo-server`.
- Keep `nuo` as the lightweight top-level executable coordinator.

---

## Decision Outcome

Chosen option: **Option 2: Full Decoupling & Alignment with Concentric Topology**.

```text
┌────────────────────────────────────────────────────────────────────────────────────────┐
│                                       nuo (Binary)                                     │
│                            【单一顶层可执行程序 · 调度协调器】                          │
└───────────────┬───────────────────────────────────┬────────────────────────────────────┘
                │ 1. 默认/交互模式                  │ 2. 无头模式 (nuo -p / run)
                ▼                                   │
       ┌─────────────────┐                          │
       │     nuo-tui     │ (终端交互视窗)           │
       │   (前身: nuox)  │ • 键盘映射/Markdown排版  │
       │   【纯呈现库】  │ • 模态弹窗/状态折叠渲染  │
       └────────┬────────┘                          │
                │                                   │
                │ 驱动会话交互                      │
                ▼                                   ▼
       ┌─────────────────────────────────────────────────┐
       │                   nuo-client                    │
       │               【客户端连接驱动 SDK】            │
       │ • 本地探活与静默拉起 (ensure_daemon)            │
       │ • UDS / WebSocket 传输通道管理                  │
       └────────────────────────┬────────────────────────┘
                                │
                                │  【线缆通信边界: nuo-wire】
                                │  (WireEnvelope / AgentRequest / StreamDelta)
                                │  通过 Unix Domain Socket 或 WebSocket 传输
                                ▼
       ┌─────────────────────────────────────────────────┐
       │                   nuo-server                    │
       │             【后台服务容器与多会话宿主】        │
       │ • UDS / WebSocket 监听网关                      │
       │ • 多会话生命周期与路由注册表 (SessionRegistry)  │
       │ • 状态持久化 (nuo-persistence)                  │
       └────────────────────────┬────────────────────────┘
                                │
                                │ 实例化并管束驱动 Session
                                ▼
       ┌─────────────────────────────────────────────────┐
       │                   nuo-harness                   │
       │              【执行管束与治理底盘】             │
       │ • 权限策略 (PermissionPolicy, BashPolicy)       │
       │ • 人机确认断点 (HumanBroker)                    │
       │ • 轨迹防死循环 (TrajectoryGuard)                │
       └────────────────────────┬────────────────────────┘
                                │
                                │ 驱动认知思考 (Think-Act-Observe)
                                ▼
       ┌─────────────────────────────────────────────────┐
       │                    nuo-agent                    │
       │                 【纯粹认知内核】                │
       │ • ReAct 流式推进循环                            │
       │ • ACP 多智能体协同协议 (Channels, Delegation)   │
       └────────────────────────┬────────────────────────┘
                                │
                                │ 调度能力工具
                                ▼
       ┌─────────────────────────────────────────────────┐
       │          去中心化工具库 (nuo-tool 标准)         │
       │ • nuo-host (文件/命令)  • nuo-tools-web (搜索)  │
       │ • nuo-tools-code (AST)  • nuo-mcp (外部扩展)    │
       └─────────────────────────────────────────────────┘
```

### 1. `nuox` $\rightarrow$ `nuo-tui` (Pure Presentation View Library)
- Crate directory `nuox/` is renamed to `nuo-tui/`.
- Package name in `Cargo.toml` is `nuo-tui`.
- Standalone binary `main.rs` is eliminated.
- `nuo-tui` exports `start_tui`, `run_tui`, and `runner` functions for consumption by `nuo`.
- `nuo-tui` depends strictly on `nuo-client`, `nuo-wire`, `nuo-host`, `nuotc`. It has zero server or daemonization code.

### 2. Extraction of `nuo-server`
- An autonomous crate `nuo-server` is created.
- Owns daemon service hosting:
  - Connection listeners (Unix Domain Sockets, Windows Named Pipes, WebSockets)
  - `SessionRegistry` (session state machines, routing, lifetimes)
  - `SessionDriver` (IPC event streaming, message translation)
  - Background task supervision, archivist, and shutdown draining.
- Depends on `nuo-harness`, `nuo-persistence`, `nuo-providers`, `nuo-host`, `nuo-wire`.
- Does NOT depend on `nuo-client`, `nuo-tui`, or `nuotc`.

### 3. Lean Unified Binary `nuo`
- The `nuo` executable links `nuo-tui`, `nuo-client`, and `nuo-server`.
- Dispatches user intent with zero cognitive friction:
  - `nuo`: default interactive session (via `nuo-tui`, auto-spawning local daemon through `nuo-client` if needed).
  - `nuo -p "<prompt>"` / `nuo run "<prompt>"`: headless turn (via `nuo-client`).
  - `nuo serve` / `nuo start`: daemon hosting (via `nuo-server`).
  - `nuo stop` / `status` / `config` / `mcp` / `skill` / `doctor`: management actions (via `nuo-client`).

---

## Invariants & Behavioral Boundaries

- **`[INV-TUI-01] Pure Presentation Library`**: `nuo-tui` must NOT declare binary executables (`[[bin]]`). It is a pure library crate. It must depend downward on `nuo-client` for all daemon communication and must contain zero server listener logic.
- **`[INV-SERVER-01] Headless Service Container`**: `nuo-server` must NOT depend on terminal rendering libraries (`nuotc`, `crossterm`) or `nuo-tui`. It must be cleanly compilable and runnable in pure headless and container environments.
- **`[INV-SERVER-02] Server-Client Anti-Inversion`**: Reaffirming `[INV-DEP-01]` (ADR-0005): `nuo-server` must NEVER depend on `nuo-client`. Client and server communicate strictly via `nuo-wire` envelopes.
- **`[INV-CLI-02] Workspace Single Binary Invariant`**: `nuo` is the sole public executable binary target produced by the workspace. No secondary binary targets (such as `nuox`) may be declared in default workspace members.

---

## Positive Consequences

- **100% Naming Consistency**: Every crate in the workspace follows the canonical `nuo-*` prefix, with `nuo` as the single executable.
- **Clean Containerization**: `nuo-server` can be packaged into minimal headless Docker containers without dragging in crossterm, keyboard layout engines, or terminal UI code.
- **Strict Separation of Concerns**: Clear, testable contracts between UI (`nuo-tui`), Client SDK (`nuo-client`), Server Host (`nuo-server`), Governance (`nuo-harness`), and Cognitive Core (`nuo-agent`).

---

## Negative Consequences & Trade-offs

- **Directory and Reference Renaming**: Renaming `nuox` to `nuo-tui` touches import paths across tests and dependencies (mitigated by automated refactoring and compatibility aliases).

---

## Rejected Alternatives & Negative Knowledge

### Retaining `nuox` as a Permanent Independent Executable (Rejected)
- **Why considered**: Avoids refactoring `nuox` imports.
- **Why rejected**: Violated the core premise of modern developer experience. Two binaries (`nuo` and `nuox`) force users to understand daemon internals prematurely and constantly choose between commands.

### Merging Server and TUI Directly into a Single Flat Crate (Rejected)
- **Why considered**: Fewer workspace crates.
- **Why rejected**: Creates a massive 150,000-line monolithic God crate where terminal graphics, WebSockets, SQLite persistence, and process supervisors are entangled. Makes headless server deployment impossible.

---

## Migration Roadmap & Milestones

1. **Milestone 1**: Rename `nuox` to `nuo-tui`, delete `main.rs`, and update workspace dependency graph.
2. **Milestone 2**: Extract `nuo-server` crate containing `serve.rs`, `registry.rs`, `session_driver.rs`, and service handlers.
3. **Milestone 3**: Refactor `nuo` into the lean top-level CLI coordinator invoking `nuo-tui`, `nuo-client`, and `nuo-server`.

---

## Links

- [ADR-0001: Flat Workspace Topology and Microkernel Capability Architecture](0001-flat-workspace-and-microkernel-capability-topology.md)
- [ADR-0005: Unified Binary, Concentric Runtime, and Wire Entity Architecture](0005-unified-binary-and-concentric-runtime-architecture.md)
- [ADR-0010: Harness Decomposition, Capability Tool Decoupling, and Cognitive Runtime Unification](0010-harness-decomposition-and-agent-unification.md)
