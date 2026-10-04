---
id: ADR-0010
title: "Harness Decomposition, Capability Tool Decoupling, and Cognitive Runtime Unification"
status: accepted
date: 2026-10-03
scope: architecture/layering, runtime/agent, harness/governance, capability/tools
superseded_by: null
negative_knowledge: true
---

# 0010. Harness Decomposition, Capability Tool Decoupling, and Cognitive Runtime Unification

- Status: Accepted
- Date: 2026-10-03
- Deciders: Nuo Architecture Working Group
- Consulted: Runtime, Capability, and Interface Teams
- Informed: System Architects, Release Engineering
- Amends: [ADR-0001](0001-flat-workspace-and-microkernel-capability-topology.md) §`[INV-TOOL-01]` (strict decentralized tool ownership)
- Amends: [ADR-0005](0005-unified-binary-and-concentric-runtime-architecture.md) §"Cognitive & Tool Lifecycle"
- Supersedes: [ADR-0009](0009-single-agent-runtime.md) §4 (retires dual-runtime split in favor of unified `nuo-agent` cognitive engine)

---

## Context and Problem Statement

Following the tool consolidation of ADR-0008 and the single-runtime diagnosis of ADR-0009, an architectural evaluation of `nuo-harness` and `nuo-agent` revealed that the system carries two severe structural liabilities:

1. **`nuo-harness` has become a 50,000-line Monolithic "God Crate".**  
   The harness was originally conceived as the governance and policy layer ("傩面法器与执行管束" — identity projection, permission brokering, sandbox containment, and human-in-the-loop approvals). In practice, it has become an undisciplined catch-all sink containing:
   - **Duplicated host tools**: Seven core filesystem and process tools (`read_text`, `write_file`, `edit_text`, `list_dir`, `find_files`, `search_text`, `execute_command`) implemented inside `nuo-harness` that shadow and duplicate `nuo-host/src/tools.rs`.
   - **External web search & scraping**: 5 web search engines (Bocha, DuckDuckGo, Exa, SearXNG, Tavily), Jina reader, HTTP fetching, and HTML parsing directly bundled into the harness.
   - **Heavy AST parsing**: Multi-language Tree-sitter parsers (`tree-sitter-rust`, `python`, `go`, `c`, `cpp`, `typescript`) compiled directly into `nuo-harness` for `code_query.rs` (1,177 LOC).
   - **Model catalog discovery & network sync**: Fetching and caching provider metadata from OpenRouter/DeepSeek (`catalog/sync.rs`, 894 LOC).
   - **The cognitive ReAct streaming loop**: `agent/rounds.rs`, `agent/execution.rs`, `agent/steering.rs` (6,169 LOC).

   This conflates runtime governance with tool capabilities, language AST tooling, web networking, and model catalog services, causing massive compilation overhead and directly violating `[INV-TOOL-01]`.

2. **The "Dual-Brain" Inversion and Orphaned Cognitive Runtime.**  
   ADR-0009 documented the existence of two divergent agent runtimes: `nuo-agent` (6,107 LOC) and `nuo-harness::Agent` (6,169 LOC). ADR-0009 declared `nuo-harness::Agent` the shipped runtime because the daemon ran it, while leaving `nuo-agent` orphaned as an SDK with passing tests.  
   However, this created an architectural semantic inversion:
   - **Semantic inversion**: A *harness* is by definition the outer restraint/wrapper (the "reins") around an agent; it should not *be* the agent.
   - **Capability fragmentation**: `nuo-agent` houses critical multi-agent ACP coordination features (channel turns, peer delegation, zero-trust envelope verification, skill sandbox) that the production daemon completely lacks.
   - **Perpetual technical debt**: Carrying an orphaned 6,000-line SDK alongside a 50,000-line harness creates cognitive dissonance, confusing contributors and fragmenting maintenance.

To build a modern, uncompromising, zero-legacy architecture, we must decompose the harness into its true role and unify the cognitive agent core.

---

## Decision Drivers

- **Zero Monolithic Bloat**: `nuo-harness` must be stripped of concrete capability tools, AST parsers, and catalog sync logic.
- **Strict Concentric Layering**: The dependency and orchestration flow must be strictly concentric:  
  `Host / Daemon (nuo)` $\rightarrow$ `Governance Harness (nuo-harness)` $\rightarrow$ `Cognitive Engine (nuo-agent)` $\rightarrow$ `Capability Standards (nuo-tool, nuo-wire)`.
- **Single Canonical Cognitive Engine**: Eliminate the dual-runtime split permanently. Consolidate the streaming ReAct loop, context compaction, and multi-agent ACP collaboration into `nuo-agent`.
- **True Capability Decentralization (`[INV-TOOL-01]` Fulfillment)**: Every tool belongs exclusively to its capability domain crate. No tools reside in the harness.
- **Uncompromising Modernization**: Zero backward-compatibility hacks, zero bridges, and zero shadowing of duplicate implementations.

---

## Considered Options

### Option 1: Status Quo (Keep `nuo-harness::Agent` as Monolith, Deprecate and Delete `nuo-agent`)
- Keep all tools, parsers, and the cognitive loop in `nuo-harness`.
- Delete `nuo-agent` entirely, discarding its multi-agent ACP channel cognition.

### Option 2: Extract Tools Only, Retain Cognitive Loop in `nuo-harness`
- Move web tools, code tools, and host tools out of `nuo-harness`.
- Retain the ReAct streaming loop inside `nuo-harness`. Keep `nuo-agent` as a separate detached SDK.

### Option 3: Full Decoupling & Concentric Convergence (Chosen)
- **Decompose `nuo-harness`**: Strip all concrete tools, AST parsers, and catalog sync. `nuo-harness` becomes a pure governance and policy harness (permissions, bash safety, trajectory guards, human broker, dispatch pipeline).
- **Decentralize capability tools**: Host tools reside solely in `nuo-host`; web and code tools move to dedicated capability crates (`nuo-tools-web`, `nuo-tools-code`); catalog moves to `nuo-providers`.
- **Converge cognitive engine into `nuo-agent`**: Migrate the production streaming ReAct loop into `nuo-agent`, merging it with ACP channel/delegation capabilities. `nuo-harness` instruments and wraps `nuo-agent`.

---

## Decision Outcome

Chosen option: **Option 3: Full Decoupling & Concentric Convergence**.

```text
┌────────────────────────────────────────────────────────────────────────┐
│                        Applications & Frontends                        │
│             nuo (Daemon Host & CLI)       nuox (Semantic TUI)          │
└───────────────────────────────────┬────────────────────────────────────┘
                                    │
                                    ▼
┌────────────────────────────────────────────────────────────────────────┐
│                              nuo-harness                               │
│              【Pure Governance, Safety & Execution Harness】            │
│  • Permission Broker & Grants (PermissionStore, PermissionPolicy)      │
│  • Command Safety & AST AST-Safety Checks (BashPolicy)                 │
│  • Trajectory Loop Guard & Stream Stalls (TrajectoryGuard)              │
│  • Human-in-the-Loop Decision Gateways (HumanRequestBroker, AskUser)   │
│  • Execution Audit & In-Flight Tracing (DispatchPipeline, Record)      │
└───────────────────────────────────┬────────────────────────────────────┘
                                    │ wraps & instruments
                                    ▼
┌────────────────────────────────────────────────────────────────────────┐
│                               nuo-agent                                │
│                   【Canonical Cognitive Engine Core】                   │
│  • ReAct Streaming Cognitive Loop (Think -> Act -> Observe)            │
│  • Model Request & Prompt Assembly (SystemPrompt, Spec Projection)     │
│  • Causal Token Compaction & Observation Pruning                       │
│  • Round Lifecycle, Full-Duplex Steering & Turn Queues                 │
│  • Multi-Agent Collaboration (ACP Channels, Peer Delegation, Handshake)│
└───────────────────────┬────────────────────────┬───────────────────────┘
                        │                        │ dispatches
                        │ depends on             ▼
                        │             ┌──────────────────────────────────┐
                        │             │   Decentralized Capability Tools │
                        │             │ • nuo-host (Filesystem / Shell)  │
                        │             │ • nuo-tools-web (Search / Reader)│
                        │             │ • nuo-tools-code (Tree-sitter)   │
                        │             │ • nuo-mcp (MCP External Tools)   │
                        │             │ • nuo-acp (Collaboration Tools)  │
                        ▼             └─────────────────┬────────────────┘
┌────────────────────────────────┐                      │
│            nuo-tool            │                      │ implements
│    【Zero-Runtime Standard】   │◄─────────────────────┘
│  • Tool trait & Descriptor     │
│  • ToolContext & ToolOutput    │
└────────────────────────────────┘
```

### 1. Stripping `nuo-harness` down to Pure Governance
`nuo-harness` is relieved of all tool implementations and capability management:
- **Filesystem & Shell Tools**: Duplicate harness tools are deleted. `nuo-host` is the sole provider of filesystem and shell tools.
- **Web Intelligence**: Web search engines (Bocha, DDG, Exa, SearXNG, Tavily) and web scrapers/readers are migrated to a dedicated capability crate (`nuo-tools-web`).
- **Code Intelligence**: Tree-sitter query logic is relocated to `nuo-tools-code`. Heavy Tree-sitter C bindings are purged from `nuo-harness`.
- **Model Catalog Sync**: Remote catalog discovery (`catalog/sync.rs`) is relocated to `nuo-providers` / `nuo-host`.
- **Retained Harness Scope**: `nuo-harness` strictly retains:
  - `PermissionStore`, `PermissionPolicy`, `BashPolicy`
  - `HumanRequestBroker`, interactive input gating
  - `TrajectoryGuard`, `StreamLoopDetector`
  - `DispatchPipeline`, execution records, and sandbox containment adapters.

### 2. Unifying the Cognitive Engine into `nuo-agent`
- The streaming ReAct cognitive loop (`rounds.rs`, `execution.rs`, `steering.rs`, `state.rs`) is extracted from `nuo-harness` and consolidated into `nuo-agent`.
- `nuo-agent` merges the production streaming loop with its existing ACP collaboration engine (`run_channel_turn`, unread backlog, peer delegation, signature enforcement).
- `nuo-agent` becomes the **single source of truth** for cognitive execution across both single-agent and multi-agent topologies.
- `nuo-harness` wraps `nuo-agent` by providing the `ToolMiddleware` dispatch pipeline, permission gates, and human approval brokers.

### 3. Clear Inbound & Outbound Dependency Structure
The dependency direction is strictly top-down:
- `nuo` $\rightarrow$ `nuo-harness` $\rightarrow$ `nuo-agent` $\rightarrow$ `{nuo-tool, nuo-wire}`.
- Capability tool crates (`nuo-host`, `nuo-tools-web`, `nuo-tools-code`, `nuo-acp`) depend only on `nuo-tool` and `nuo-wire`, never on `nuo-harness` or `nuo-agent`.

---

## Invariants & Behavioral Boundaries

- **`[INV-HARNESS-01] Zero Capability Tools in Harness`**: `nuo-harness` MUST NOT define concrete filesystem, process execution, web browsing, or syntax query tools. All tools must reside in decentralized capability crates conforming to `nuo_tool::Tool`.
- **`[INV-HARNESS-02] Pure Governance and Policy Boundary`**: `nuo-harness` is restricted to execution containment, permission gating, trajectory supervision, human-in-the-loop coordination, and execution audit. It must not depend on AST parsing engines or network scraping libraries.
- **`[INV-AGENT-07] Single Cognitive Engine in nuo-agent`**: `nuo-agent` is the sole implementation of the ReAct cognitive loop, model prompt assembly, causal token compaction, and multi-agent ACP coordination. No secondary cognitive loop may exist in `nuo-harness` or `nuo`.
- **`[INV-DEP-03] Concentric Dependency Hierarchy`**: Dependency edges must flow from application to harness, harness to agent, and agent to contracts/capabilities. `nuo-agent` must never depend on `nuo-harness`. Capability crates must never depend on `nuo-agent` or `nuo-harness`.

---

## Positive Consequences

- **Compilation Speed & Dependency Hygiene**: Evicting Tree-sitter (6 languages) and HTTP web scraper dependencies from `nuo-harness` drastically reduces build times and cleanses the core harness dependency graph.
- **Elimination of Duplication and Shadowing**: Eliminates the 7 duplicate host tools and resolves silent runtime tool shadowing in `nuo/src/bootstrap.rs`.
- **Unified Mental Model**: Resolves the "two runtimes" paradox permanently. `nuo-agent` is the cognitive core, `nuo-harness` is the governance wrapper.
- **First-Class Multi-Agent Capabilities**: By unifying the cognitive engine into `nuo-agent`, the production daemon naturally inherits multi-party channel cognition and zero-trust peer delegation.

---

## Negative Consequences & Trade-offs

- **Cross-Crate Migration Effort**: Moving the ReAct loop and context lifecycle from `nuo-harness` to `nuo-agent` touches critical execution paths and requires updating session orchestration call sites in `nuo`.
- **Workspace Member Realignment**: Adding `nuo-tools-web` and `nuo-tools-code` introduces new workspace members (mitigated by flat workspace topology and feature-gating).

---

## Rejected Alternatives & Negative Knowledge

### Retaining the Monolithic Harness and Deleting `nuo-agent` (Rejected)
- **Why considered**: Deleting `nuo-agent` would be mechanically easier than refactoring the harness, since the daemon currently runs `nuo-harness::Agent`.
- **Why rejected**: Perpetuates the 50,000-line God Crate anti-pattern. Cementing the cognitive loop inside `nuo-harness` makes `nuo-harness` impossible to decouple, leaves Tree-sitter and web scraper baggage in the core governance layer, and permanently destroys the tested ACP multi-party channel and delegation capabilities present in `nuo-agent`.

### Keeping Two Parallel Agent Runtimes via Trait Abstraction (Rejected)
- **Why considered**: Defining an abstract `AgentEngine` trait and allowing both `nuo-agent` and `nuo-harness::Agent` to implement it.
- **Why rejected**: Violated the core project directive: *no legacy burden, uncompromising modernization*. Carrying two parallel cognitive engines doubles maintenance cost, splits behavioral test coverage, and confuses downstream consumers without any architectural benefit.

---

## Migration Roadmap & Milestones

1. **Milestone 1 (Tool Eviction)**:
   - Deprecate duplicate tools in `nuo-harness/src/tools/{read_text, write_file, edit_text, list_dir, find_files, search_text, execute_command}` in favor of `nuo-host/src/tools.rs`.
   - Extract web search and reader tools into `nuo-tools-web`.
   - Extract Tree-sitter code query into `nuo-tools-code`.
2. **Milestone 2 (Cognitive Loop Unification)**:
   - Migrate `nuo-harness/src/agent/` ReAct streaming loop, steering, and compaction into `nuo-agent`.
   - Reconcile `nuo-agent`'s ACP multi-agent features with the streaming engine.
3. **Milestone 3 (Harness Pure Governance Refactoring)**:
   - Refactor `nuo-harness` to wrap `nuo-agent::Agent` with `PermissionStore`, `BashPolicy`, `TrajectoryGuard`, and `HumanRequestBroker`.
   - Wire `nuo` bootstrap to assemble `nuo-harness(nuo-agent)`.

---

## Links

- [ADR-0001: Flat Workspace Topology and Microkernel Capability Architecture](0001-flat-workspace-and-microkernel-capability-topology.md)
- [ADR-0005: Unified Binary, Concentric Runtime, and Wire Entity Architecture](0005-unified-binary-and-concentric-runtime-architecture.md)
- [ADR-0007: Tool Ownership, Contract Layering, and Compile-Time Plugin Mechanism](0007-tool-ownership-and-plugin-boundaries.md)
- [ADR-0008: Single Tool Contract: Metadata-as-Data, Capabilities-via-Context](0008-single-tool-contract.md)
- [ADR-0009: Single Agent Runtime and Contract-Layer Ownership of Agent Identity](0009-single-agent-runtime.md)
