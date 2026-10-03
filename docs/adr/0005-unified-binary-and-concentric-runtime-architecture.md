---
id: ADR-0005
title: "Unified Binary, Concentric Runtime, and Wire Entity Architecture"
status: accepted
date: 2026-10-02
scope: workspace/topology, architecture/cli, protocol/wire, runtime/concentric
superseded_by: null
negative_knowledge: true
---

# 0005. Unified Binary, Concentric Runtime, and Wire Entity Architecture

- Status: Accepted
- Date: 2026-10-02
- Deciders: Nuo Architecture Working Group
- Consulted: Core Contributors, Runtime & Interface Teams
- Informed: System Architects, Release Engineering

---

## Context and Problem Statement

As Nuo expanded to accommodate rich terminal interactions and browser-based control planes, three architectural anti-patterns and cognitive burdens emerged:

1. **The Multi-Binary & Dual-Cognition Tax (`nuo` vs `nuox`)**: Users and developers were forced to maintain dual mental models across two distinct executables: running `nuo start` to manage background daemons and `nuox` to interact within a full-screen terminal. This violated modern CLI expectations where a single, unified command (`nuo`) provides interactive sessions by default, headless pipelines via flags, and service execution via subcommands.
2. **Polyglot Monorepo Drift (`web/`)**: The workspace maintained a Svelte 5 / Vite / Node.js web frontend (`web/`) in the same repository as the high-performance Rust core. Because everyday workflows overwhelmingly center on semantic terminal interactions (`nuox`), the web client introduced substantial CI/build overhead, diverged from rapid wire protocol changes, and contaminated the daemon with browser-specific defenses (CORS headers, Origin validation).
3. **Semantic Inversion and Protocol Conflation**: The wire framing and DTO types were historically housed within `nuo-client`, forcing the daemon server to depend on client packaging (`nuo` -> `nuo-client`), which inverted standard dependency semantics. Furthermore, attempting to name this contract `api` or `protocol` introduced ambiguity against the Agent Coordination Protocol (`nuo-acp`) and leaked RPC calling assumptions over what is fundamentally a bidirectional, streaming wire envelope.
4. **Conflation of Process Supervision and Session Registry**: Operating-system-level process lifecycle concerns (`detach_daemon`, double-forking, PID probing) were tightly coupled with in-memory domain session state machines (`SessionRegistry`), obscuring the boundary between host orchestration and service execution.

We require a modern, uncompromising architecture that eliminates legacy burdens, consolidates entrypoints, and strictly stratifies system lifecycles.

---

## Decision Drivers

- **Zero-Friction Single CLI Surface**: A unified top-level binary `nuo` serving interactive TUI (default), headless scripts (`nuo -p`), and persistent service (`nuo serve`).
- **100% Rust Monorepo Purity**: Complete decoupling and relocation of the web frontend to an independent repository (`nuo-web`), restoring workspace purity.
- **Pure Wire Entity Separation (`nuo-wire`)**: A zero-I/O, zero-runtime crate (`nuo-wire`) defining byte-level envelopes, streaming schemas, and barrier contracts without dependency inversion or transport-mechanism conflation.
- **Concentric Layering & Clear Lifecycle Boundaries**: Strict separation between:
  - **OS Process Lifecycle**: Supervised by `nuo` CLI launcher and `nuo-host`.
  - **Session Lifecycle**: Hosted and routed by `nuo-server`.
  - **Cognitive & Tool Lifecycle**: Orchestrated by `nuo-harness` over `nuo-agent`.
- **Feature-Gated Multi-Agent Collaboration**: Aligning protocol naming to `nuo-acp` and establishing it as an optional compile-time feature (`features = ["acp"]`).

---

## Decision Outcome

We establish the unified binary, concentric runtime, and wire entity architecture:

```text
┌───────────────────────────────────────────────────────────────────────────────────────────────────┐
│                                            nuo (CLI 入口)                                         │
│                                 【单一顶级二进制 · 用户心智唯一入口】                             │
└───────────────┬───────────────────────────────────┬───────────────────────────────────┬───────────┘
                │ (默认: 交互模式)                  │ (无头/脚本: -p / run)             │ (服务模式: serve)
                ▼                                   │                                   │
       ┌─────────────────┐                          │                                   │
       │     nuo-tui     │ (终端交互视窗)           │                                   │
       │ (nuotc渲染/按键)│                          │                                   │
       └────────┬────────┘                          │                                   │
                │ (消费流式事件)                    │                                   │
                ▼                                   ▼                                   │
       ┌─────────────────────────────────────────────────┐                              │
       │                   nuo-client                    │                              │
       │           (客户端驱动 SDK / 本地 IPC 通道)      │                              │
       └────────────────────────┬────────────────────────┘                              │
                                │                                                       │
                                │ (通过 UDS / WebSocket 传输)                           │
                                │                                                       │
                                │     【约束边界: nuo-wire (纯线缆帧与 Session 实体)】   │
                                │                                                       │
                                ▼                                                       ▼
┌───────────────────────────────────────────────────────────────────────────────────────────────────┐
│                                            nuo-server                                             │
│                                 【后台服务容器与多会话调度宿主】                                  │
│                                                                                                   │
│   • UDS / WebSocket 通信网关 (Connection Gateway)    • 多会话生命周期注册表 (Session Registry)   │
│   • 优雅停机信号处理 (Graceful Drainer)              • 状态持久化与数据库 (nuo-persistence)       │
└─────────────────────────────────────────────────┬─────────────────────────────────────────────────┘
                                                  │
                                                  │ 实例化并驱动 Session
                                                  ▼
┌───────────────────────────────────────────────────────────────────────────────────────────────────┐
│                                           nuo-harness                                             │
│                                 【认知编排中枢 · 傩面法器与执行管束】                             │
│                                                                                                   │
│   • 角色投影与身份装配 (Persona / Identity)          • 危险操作管控 (RiskProfile / Policy)        │
│   • 人类在环确认断点 (Approval / AskUser)            • 上下文因果修剪压缩 (Causal Compaction)     │
│   • 动态工具与技能调度 (Dynamic Tools / MCP)         • 轮次生命周期编排 (Round Lifecycle)         │
└───────┬─────────────────────────────────────────┬─────────────────────────────────────────┬───────┘
        │                                         │                                         │
        │ 驱动认知思考                            │ [可选] 智能体间协同 (Feature: "acp")    │ 调度宿主沙箱工具
        ▼                                         ▼                                         ▼
┌─────────────────┐                      ┌─────────────────┐                       ┌─────────────────┐
│    nuo-agent    │                      │     nuo-acp     │                       │    nuo-host     │
│                 │                      │                 │                       │                 │
│  纯认知思考回路 │                      │  多智能体协作网 │                       │  OS进程沙箱隔离 │
│ (Think-Act-Obs) │                      │  (agent:// 信箱)│                       │  本地 7 核心工具│
└────────┬────────┘                      └─────────────────┘                       └─────────────────┘
         │
         │ 请求多厂商模型适配
         ▼
┌──────────────────────────────────────────────────────────┐
│              nuo-providers & nuo-model-codec             │
│         (多厂商模型目录 · OpenAI/Anthropic/DeepSeek)     │
└──────────────────────────────────────────────────────────┘
```

### 1. Unified Human CLI Surface (`nuo`)
- Running `nuo` with no arguments launches the interactive semantic terminal, silently detecting or spawning a local background server via `nuo-host`.
- Running `nuo -p "<prompt>"` or `nuo run "<prompt>"` executes a headless turn through `nuo-client`, streaming markdown/plain text directly to `stdout`.
- Running `nuo serve` launches `nuo-server` in the foreground or as a dedicated container payload.
- Management verbs (`nuo status`, `nuo session ls`, `nuo mcp ls`, `nuo doctor`) route over `nuo-client` IPC.

### 2. Physical Decoupling of Presentation (`nuo-tui`)
The rich terminal UI engine (previously the `nuox` application) is reorganized as `nuo-tui`, a dedicated view library built on `nuotc`. It owns no standalone binary targets; it is invoked directly by `nuo` during interactive sessions.

### 3. Pure Wire Entities Specification (`nuo-wire`)
`nuo-wire` is established as an independent substrate crate. It contains:
- Zero asynchronous runtimes and zero network I/O dependencies.
- Wire envelopes (`WireEnvelope`), control frames (`ControlRequest`, `ControlReply`), streaming events (`AgentEvent`, `StreamDelta`), and barrier definitions (`ApprovalRequest`, `PromptAnswer`).
- Protocol version invariants (`PROTOCOL_VERSION`, `MIN_PROTOCOL_VERSION`).
Both `nuo-server` and `nuo-client` depend strictly downward on `nuo-wire`.

### 4. Pure Server Runtime (`nuo-server`)
`nuo-server` is decoupled from OS-level daemonization and client abstractions:
- It exposes a unified connection listener over local Unix Domain Sockets / Windows Named Pipes and optional WebSockets.
- It maintains the in-memory `SessionRegistry` and SQLite persistence coordination.
- It delegates agent cognitive orchestration entirely to `nuo-harness`.

### 5. Multi-Agent Protocol Alignment (`nuo-acp`)
The Agent Coordination Protocol implementation is renamed from `acp` to `nuo-acp` for workspace prefix consistency with `nuo-mcp`. In `nuo-server`, it is configured as an optional feature flag (`features = ["acp"]`), permitting lean single-agent distributions when multi-party mesh collaboration is not required.

---

## Invariants & Behavioral Boundaries

- **`[INV-CLI-01] Unified Binary Singleton`**: `nuo` is the sole public executable binary produced by the workspace. `nuox` must not exist as an independent binary target.
- **`[INV-WIRE-01] Zero-Runtime Wire Specification`**: `nuo-wire` must contain zero asynchronous runtime dependencies (no `tokio`), zero network I/O, and zero domain harness references. It must compile in under 1 second. **(Amended by [ADR-0006](0006-wire-contract-consolidation.md) `[INV-WIRE-03]`: async runtimes may appear transitively via leaf substrates whose types the contracts name; the zero-I/O and no-*direct*-runtime clauses remain binding.)**
- **`[INV-DEP-01] Server-Client Anti-Inversion`**: `nuo-server` must NEVER depend on `nuo-client`. Both subsystems must communicate strictly through `nuo-wire` data contracts.
- **`[INV-LIFECYCLE-01] Three-Tier Stratification`**:
  1. *OS Process Lifecycle* must be governed strictly by CLI launcher code (`nuo`) and `nuo-host`.
  2. *Session Lifecycle* must be governed strictly by `nuo-server` (`SessionRegistry`).
  3. *Cognitive & Barrier Lifecycle* must be governed strictly by `nuo-harness`.

---

## Rejected Alternatives

- **Retaining `nuox` as a Separate Binary (`nuo` + `nuox`)**:
  *Why rejected*: Imposes cognitive friction on users, requiring them to remember two commands and understand daemon detachment prematurely. Neovim, Ollama, and Claude Code demonstrate that single-binary UX with flag-driven modes is strictly superior.
- **Retaining `web/` Inside the Cargo Monorepo**:
  *Why rejected*: Mixing TypeScript/pnpm/Vite within a Rust engine monorepo created dual CI burdens and protocol divergence. Relocating to `../nuo-web` restores repository purity and lets the web dashboard evolve at its own cadence.
- **Naming the Contract Layer `nuo-api`, `nuo-protocol`, or `nuo-ipc`**:
  *Why rejected*: `api` implies callable functions and traditional request-response RPC. `protocol` clashed with `nuo-acp` and violated `[INV-ARCH-01]` against vague abstract naming. `ipc` confuses data entities with transport mechanisms (the entities are transport-agnostic, not bound to IPC). `nuo-wire` accurately and concisely denotes over-the-wire envelopes and session communication entities.
- **Inlining Wire Types Directly into `nuo-client` as a Module**:
  *Why rejected*: If wire types live inside `nuo-client`, then `nuo-server` must depend on `nuo-client`. This inverts dependency semantics (the server depends on its own client), violating the Dependency Inversion Principle.
- **Conflating Process Supervision with `nuo-server`**:
  *Why rejected*: An OS process cannot supervise or detach itself from within. Conflating process management (PID files, fork detachment) with domain session management violates single responsibility and complicates containerized deployments.
- **Mandatory Non-Gated ACP Integration**:
  *Why rejected*: Single-agent terminal workflows do not require HMAC signature verification, multi-agent mailbox routing, or distributed channels. Feature-gating `nuo-acp` ensures minimal binary size and instantaneous cold start for lightweight use cases.
