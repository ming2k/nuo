---
id: ADR-0048
title: "CLI Architecture Modernization, Resource Symmetry, and Application Observability Topology"
status: accepted
date: 2026-10-25
scope: interface/cli, architecture/observability, server/status, runtime/concentric
superseded_by: null
negative_knowledge: true
---

# 0048. CLI Architecture Modernization, Resource Symmetry, and Application Observability Topology

- Status: Accepted
- Date: 2026-10-25
- Deciders: Nuo Architecture Working Group
- Consulted: Interface, Runtime, Protocol, and TUI Teams
- Informed: System Architects, Developer Experience
- Amends: [ADR-0005](0005-unified-binary-and-concentric-runtime-architecture.md), [ADR-0033](0033-canonical-server-entity-dual-hosting-postures-and-endpoint-orthogonality.md), [ADR-0038](0038-thread-entity-server-ssot-fanout-and-orthogonal-action-lifecycle.md), [ADR-0043](0043-thread-domain-nomenclature-unification-compact-tabbar-affordance-and-c-x-namespace-reorganization.md), [ADR-0045](0045-dynamic-port-sharing-palette-orthogonal-control-plane-and-server-lifecycle-handoff.md)

---

## Context and Problem Statement

Nuo unified its binary footprint under ADR-0005 and streamlined its domain nomenclature under ADR-0038 and ADR-0043. However, progressive feature accumulation across rapid release cycles exposed deep ergonomic inconsistencies, architectural redundancies, and observability gaps across the Command Line Interface (CLI):

1. **Dual-Parser Drift and Redundant Argument Validation**:
   Command-line invocation historically traversed uncoordinated parsing layers between root launcher and presentation runtime.

2. **CRUD Asymmetry in Thread Resource Management**:
   Under ADR-0038 and ADR-0043, the persistent multi-turn conversational entity was formally ratified as `Thread`. The CLI provided administrative deletion via `nuo thread rm <id>`, but lacked symmetrical `nuo thread ls`.

3. **Nomenclature Inflation and Leaked Scene Commands**:
   Internal TUI scenes (`attach`, `dashboard`, `settings`) leaked into the top-level CLI surface as pseudo-commands. Concurrently, redundant verbs (`serve` vs `server start`) and internal utilities (`doctor`, `auth`, `quota`, `context`) cluttered `--help`.

4. **Observability Conflation in `nuo status`**:
   `nuo status` originally dumped raw session tables, obscuring high-level system health, provider readiness, and active workflows.

5. **Foreground vs. Background Lifecycle Semantic Inversion**:
   In standard CLI ergonomics, `start` denotes background service orchestration, whereas foreground container execution requires `--fg`.

We require a modernized, uncompromising, forward-facing CLI architecture that eradicates dual-parser drift, enforces full CRUD resource symmetry, canonicalizes `thread` as the primary resource noun, consolidates server lifecycle under `nuo server`, and purges all legacy command baggage.

---

## Decision Drivers

- **Single CLI Parse Authority (`[INV-PARSE-01]`)**: All command-line arguments across both interactive TUI and headless/server modes must be parsed once within the root binary crate (`nuo`) into a strongly typed execution descriptor.
- **Resource CRUD Symmetry (`[INV-CLI-03]`)**: Every first-class resource that exposes mutation verbs (`rm`, `set`) must provide a corresponding listing verb (`ls`).
- **Singular Domain Resource Noun (`[INV-CLI-04]`)**: `thread` is the authoritative public noun for multi-turn agent workloads; `session` is demoted to a silent backwards-compatibility alias.
- **Hierarchical Application Observability (`[INV-STATUS-02]`)**: `nuo status` serves as the top-level application dashboard, presenting server health, provider readiness, MCP/tool state, and active workflows. Thread enumeration is delegated to `nuo thread ls`, and low-level transport debugging is delegated to `nuo server status` / `--diagnostic`.
- **Pure Resource-Verb Lifecycle Topology (`[INV-CLI-05]`)**: Server lifecycle is strictly unified under `nuo server <start|stop|reload|restart|status|token>`. `nuo server start` launches background instances by default, with `--fg` for foreground container execution. Leaked scene commands (`attach`, `dashboard`, `settings`) and legacy verbs (`serve`, `doctor`, `auth`, `quota`, `context`) are purged without legacy shims.

---

## Considered Options

### Option 1: Incremental Patching of Dual Parsers and Synthetic Aliases (Status Quo)
- Retain `nuo-tui/src/cli.rs` and manually synchronize flag definitions with `nuo/src/cli.rs`. Route `nuo thread ls` as a synthetic shell wrapper around `nuo status`.
- *Assessment*: Rejected. Preserves the dual-parser maintenance tax, leaves `nuo status` cluttered with raw ASCII session dumps, and perpetuates terminological ambiguity between `thread` and `session`.

### Option 2: Radical Reorganization with Immediate Breaking Flag Removal
- Eliminate all aliases (`session`, `serve`, `start`, `stop`) instantly with zero compatibility period. Require nested subcommand invocations for all operations (`nuo server lifecycle start`).
- *Assessment*: Rejected. Introduces needless user friction and breaks operational scripts without architectural justification. Aliases should be retained as silent shims rather than prematurely deleted.

### Option 3: Modernized Single-Entrypoint CLI, Resource Symmetry, and Application Observability Topology (Chosen)
- Consolidate all CLI parsing into `nuo/src/cli.rs`, eliminating `nuo-tui/src/cli.rs` in favor of a strongly typed `TuiLaunchOptions` structure.
- Canonicalize `nuo thread ls` as the dedicated listing surface, establishing `ls` as the uniform listing verb across all subcommands (`mcp ls`, `skill ls`, `auth ls`, `config ls`).
- Re-architect `nuo status` into a curated, hierarchical Application Health Dashboard (Hero summary, Provider readiness, Active tasks, MCP health).
- Relocate low-level infrastructure diagnostics to `nuo server status` / `nuo status --diagnostic`.
- Realign service verbs: `nuo serve` = foreground execution; `nuo server start` / `nuo start` = background execution by default (with `--fg` escape hatch).

---

## Decision Outcome

Chosen option: **Option 3**.

### 1. Single CLI Entrypoint Architecture (`[INV-PARSE-01]`)

The redundant parser in `nuo-tui/src/cli.rs` is eradicated. The entrypoint in `nuo/src/cli.rs` acts as the sole command-line parser for the entire binary workspace.

```text
                                  User Invocations (CLI)
                                            │
                                            ▼
                              ┌───────────────────────────┐
                              │     nuo/src/cli.rs        │
                              │  (Sole Parse Authority)   │
                              └─────────────┬─────────────┘
                                            │
                ┌───────────────────────────┼───────────────────────────┐
                ▼                           ▼                           ▼
      Mode::Interactive             Mode::Thread                Mode::Server
     (TuiLaunchOptions)             (ThreadAction)             (ServerAction)
                │                           │                           │
                ▼                           ▼                           ▼
      nuo-tui::runner::run           commands::thread             supervisor
```

- When an interactive or headless invocation is parsed, `nuo/src/cli.rs` produces a fully resolved `TuiLaunchOptions` struct containing validated fields:
  ```rust
  pub struct TuiLaunchOptions {
      pub target_thread: Option<String>,
      pub initial_prompt: Option<String>,
      pub project_root: Option<PathBuf>,
      pub unattended: bool,
      pub role: Option<String>,
      pub resume: bool,
      pub no_confinement: bool,
      pub explicit_port: Option<u16>,
      pub initial_overlay: StartupOverlay,
  }
  ```
- `nuo-tui` accepts `TuiLaunchOptions` directly, operating strictly as a presentation and event loop crate without any dependency on command-line string tokenization.

### 2. Thread Resource Symmetry and Nomenclature Canonicalization (`[INV-CLI-03]`, `[INV-CLI-04]`)

The thread management surface is standardized under `nuo thread`:

```text
nuo thread ls [--watch] [--json] [--all]
nuo thread rm <id>
```

- **`nuo thread ls`**: Lists hosted threads in a clean, responsive terminal table:
  - Columns: `THREAD ID`, `ROLE`, `STATUS`, `ROUNDS`, `TOKENS`, `UPDATED`, `TITLE`.
  - Default view filters out reaped/suspended records; `--all` includes dormant and persisted history.
  - Supports `--json` for automation and `--watch` for streaming table updates.
- **Canonical Noun**: All help text, completion scripts, and reference documentation advertise `thread`.
- **Silent Backward Compatibility**: `session` remains supported at the parser level as an unadvertised, zero-cost alias for `thread` (`names: &["thread", "session"]`).

### 3. Application-Level Observability Surface (`[INV-STATUS-02]`)

`nuo status` is bifurcated into two distinct operational scopes:

#### A. Top-Level Application Dashboard (`nuo status`)
Displays high-level health and subsystem readiness for developers:

```text
● Nuo [Healthy] · Server PID 39401 (uptime 3h 12m)
  Transport: Local IPC (0-TCP secure) · Project: /workspace/acme-core
  Database:  nuo.db (WAL healthy, 4.2 MB)

Providers & Models
  ✓ Anthropic  claude-3-7-sonnet      [Ready · Latency 142ms]
  ✓ OpenAI     o3-mini                [Ready · Latency 98ms]
  ! DeepSeek   deepseek-chat          [Key Expired · Needs Attention]

Capabilities & Extensions
  MCP:    3 servers active (filesystem, git, github) · 24 tools registered
  Skills: 6 discovered (2 project, 4 user)

Active Workflows (1 running / 2 idle)
  ID         ROLE        STATUS    TOKENS   LAST ACTIVE   SUMMARY
  t-8f20b    developer   Running    14.2k   Just now      Refactor auth middleware

Tip: Run `nuo thread ls` for full thread history, or `nuo attach t-8f20b` to join.
```

#### B. Infrastructure Diagnostic Surface (`nuo server status` / `nuo status --diagnostic`)
Displays low-level systems information for debugging and deployment operations:
- Socket paths (`$XDG_STATE_HOME/nuo/server.sock`), process lock holders, TCP listener states.
- Client connection accounting and idle-exit countdown timers.
- Detailed transport error counters and memory usage footprints.

### 4. Uniform Listing Verb Standard

All resource subcommands standardize on **`ls`** as the primary canonical verb, with `list` retained as a universal silent alias:
- `nuo thread ls`
- `nuo mcp ls`
- `nuo skill ls`
- `nuo config ls`

### 5. Lifecycle Semantics Realignment (`[INV-CLI-05]`)

Server execution verbs are aligned with standard systems engineering expectations:
- **`nuo server start`**: Starts the server in the **background (detached)** by default. An explicit `--fg` or `--foreground` flag runs it in the foreground for container/supervision environments.
- **`nuo server stop`**: Drains and terminates the running server.
- **`nuo server reload`**: Triggers Level 1 soft configuration and MCP/skill re-sync without dropping connections.
- **`nuo server restart`**: Restarts or replaces the server instance.
- **`nuo server status` / `nuo server token`**: Infrastructure diagnostics and bearer credentials.
- **Eradicated redundant aliases**: `serve`, top-level `start`/`stop`/`reload`/`restart`/`token`, `attach`, `dashboard`, `settings`, `doctor`, `auth`, `quota`, and `context` are eliminated outright.

---

## Invariants & Behavioral Boundaries

- **`[INV-PARSE-01]` Single Parser Authority**: Command-line arguments across the entire Nuo distribution MUST be parsed solely within `nuo/src/cli.rs`. The `nuo-tui` presentation crate MUST NOT contain an independent command-line parser.
- **`[INV-CLI-03]` Thread Resource Symmetry**: The CLI MUST provide `nuo thread ls` for querying threads whenever `nuo thread rm` is provided for removing threads. Thread enumeration MUST NOT require invocation of top-level system health commands.
- **`[INV-CLI-04]` Canonical Thread Domain**: The public user-facing nomenclature for agent workloads MUST be `thread`. The noun `session` MUST NOT appear in generated help text, shell completions, or public documentation.
- **`[INV-STATUS-02]` Layered Observability Separation**: `nuo status` MUST focus on top-level application and capability health. It MUST NOT dump raw unpaginated multi-project thread tables. Low-level socket diagnostics MUST be isolated under `nuo server status` or `--diagnostic`.
- **`[INV-CLI-05]` Pure Server Lifecycle Topology**: Server operations MUST be routed under `nuo server <start|stop|reload|restart|status|token>`. `nuo server start` MUST detach into the background by default unless `--fg` is explicitly declared. Redundant aliases (`serve`, top-level lifecycle verbs) and internal scene leakages (`attach`, `dashboard`, `settings`) MUST NOT exist as top-level CLI commands.

---

## Positive Consequences

- Eliminates duplicate argument definitions and resolves parser drift between `nuo` and `nuo-tui`.
- Delivers an intuitive, POSIX-aligned CRUD interface for thread inspection (`nuo thread ls`).
- Elevates `nuo status` into a modern, actionable application health dashboard.
- Standardizes listing verbs across the entire toolchain to `ls`.
- Resolves user confusion regarding foreground vs. background server execution.

---

## Negative Consequences & Trade-offs

- Automated shell scripts expecting `nuo status` to emit purely thread table columns must migrate to `nuo thread ls --json`.
- *Mitigation*: `nuo status --json` will include an actionable deprecation field pointing to `nuo thread ls --json`, with schema changes clearly noted in release notes.

---

## Rejected Alternatives & Negative Knowledge

Per **[INV-AGENT-01]**, the following alternative architectures were evaluated and rejected:

### 1. Rejected: Synthetic Shell Alias Mapping `nuo thread ls` to `nuo status`
- *Why considered*: Avoided writing a dedicated listing presentation handler.
- *Why rejected*: Violates Layered Observability Separation (`[INV-STATUS-02]`). An operator querying threads expects a compact, thread-focused tabular output, not provider health checks, MCP summaries, and system diagnostics.

### 2. Rejected: Dual Co-Equal Nouns (`nuo thread` AND `nuo session` in Help Menus)
- *Why considered*: Allowed users of legacy versions to continue seeing familiar terms in `--help`.
- *Why rejected*: Violates Canonical Thread Domain (`[INV-CLI-04]`). Advertising dual synonyms introduces cognitive friction, raises doubts about whether they represent distinct system concepts, and undermines the clean taxonomy established in ADR-0038.

### 3. Rejected: Defaulting `nuo start` to Foreground Execution
- *Why considered*: Preserved compatibility with ADR-0029 and Docker single-process execution.
- *Why rejected*: Violates Deterministic Server Lifecycle Verbs (`[INV-CLI-05]`). In interactive terminal operations, `start` implies initiating a background service. Foreground service execution is the canonical responsibility of `serve`.

---

## Links

- Amends: [ADR-0005](0005-unified-binary-and-concentric-runtime-architecture.md)
- Amends: [ADR-0033](0033-canonical-server-entity-dual-hosting-postures-and-endpoint-orthogonality.md)
- Amends: [ADR-0038](0038-thread-entity-server-ssot-fanout-and-orthogonal-action-lifecycle.md)
- Amends: [ADR-0043](0043-thread-domain-nomenclature-unification-compact-tabbar-affordance-and-c-x-namespace-reorganization.md)
- Amends: [ADR-0045](0045-dynamic-port-sharing-palette-orthogonal-control-plane-and-server-lifecycle-handoff.md)
