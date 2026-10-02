---
id: ADR-0002
title: "Unify Tool Contracts and MCP Runtime on Nous Substrate"
status: accepted
date: 2026-10-01
scope: substrate/tools, app/mcp, app/agent
superseded_by: null
negative_knowledge: true
---

# 0002. Unify Tool Contracts and MCP Runtime on Nous Substrate

- Status: Accepted
- Date: 2026-10-01
- Deciders: Nuo Architecture Working Group
- Consulted: Substrate & Runtime Team
- Informed: Core Contributors

---

## Context and Problem Statement

The `nous` cognitive substrate provides two foundational crates for capabilities and extensible integrations:
- **`nous-tool`** (`../nous/crates/nous-tool`): Canonical agent-neutral tool specification, risk profiles (`RiskProfile`), operational scopes (`ToolScope`), cancellation context (`ToolContext`), and dynamic execution runtime.
- **`nous-mcp`** (`../nous/crates/nous-mcp`): Standard Model Context Protocol (MCP) client and server transport, supporting out-of-process tool discovery over JSON-RPC 2.0 and native bridging to `nous_tool::Tool`.

However, the application host layer in `nuo` inherited historical decoupled implementations:
1. **Dead Substrate Dependencies**: `crates/nuo-agent/Cargo.toml` declared dependencies on `nous-tool` and `nous-mcp`, but never invoked any symbols from them.
2. **Duplicated MCP Stack**: `crates/nuo-mcp` maintained an independent, redundant 5,000+ line implementation of stdio JSON-RPC transport, protocol handshakes, and tool adapters, completely bypassing `nous-mcp`.
3. **Heterogeneous Tool Contracts**: `nuo-contracts` defined a private, session-coupled `Tool` trait, `HazardLevel`, and `ScopeTarget` that were isomorphic to `nous-tool`'s contracts but completely disconnected in the type system.

We must unify the tool contracts and MCP runtime on top of the `nous` substrate, eliminating redundant code while preserving Nuo's interactive session ergonomics (e.g. TUI approval sheets, cooperative turn cancellation).

---

## Decision Drivers

- **Substrate First**: Standardize all canonical tool definitions and external MCP protocol communications on `nous-tool` and `nous-mcp`.
- **Zero Redundant Protocols**: Eliminate parallel JSON-RPC 2.0 protocol parsers and child process transports in `nuo-mcp`.
- **Clean Decorator / Adapter Architecture**: Keep `nous-tool::Tool` pure and agent-runtime-agnostic. Host-specific concerns (e.g., TUI permission dialogues, interactive prompts, subagent spawn gates) wrap the substrate tool as decorators rather than polluting the core tool interface.
- **Interoperability**: Enable any tool written against `nous-tool` (including native tools, MCP-discovered tools, and dynamic closures) to be executed by the `nuo` agent harness without modification.

---

## Considered Options

- **Option 1: Complete Fork / Parallel Maintenance**
  Continue maintaining `crates/nuo-mcp` and `nuo_contracts::Tool` entirely independently from `nous`.
- **Option 2: Destructive Replacement of Application Contracts**
  Immediately delete `nuo_contracts::Tool` and rewrite all 30+ built-in agent tools to implement `nous_tool::Tool` directly, removing host-specific metadata like `requires_vision` and `spawns_subagent`.
- **Option 3 (Chosen): Substrate Grounding with Bidirectional Bridge & Lean MCP Runtime**
  1. Ground all tool risk and execution semantics on `nous-tool`.
  2. Implement canonical bidirectional adapters (`NousToolBridge` and `RiskProfile` $\leftrightarrow$ `HazardLevel` mapping) in `nuo-contracts`.
  3. Rebase `nuo-mcp`'s transport and tool conversion directly onto `nous-mcp::McpClient` and `nous_mcp::McpNativeTool`.
  4. Retain `nuo-mcp` strictly as the application host glue responsible for `config.toml` binding, health monitoring, and feeding `DynamicToolSink`.

---

## Decision Outcome

Chosen option: **Option 3: Substrate Grounding with Bidirectional Bridge & Lean MCP Runtime**, because it eliminates protocol duplication, adheres to clean hexagonal architecture, and achieves full alignment with the `nous` substrate with zero disruption to the interactive TUI.

### Architecture Topology

```text
┌─────────────────────────────────────────────────────────────┐
│ Application Host Layer (nuo)                                │
│                                                             │
│  crates/nuo-mcp (Application Glue & Lifecycle)              │
│   ├── config.toml server declarations                       │
│   ├── Attestation & project trust verifiers                 │
│   └── Feeds into nuo_contracts::DynamicToolSink             │
│                                                             │
│  crates/nuo-contracts                                       │
│   ├── HazardLevel <──> nous_tool::RiskProfile mapping       │
│   └── NousToolBridge: adapts nous_tool::Tool -> nuo::Tool   │
└──────────────────────────────┬──────────────────────────────┘
                               │ drives / adapts
                               ▼
┌─────────────────────────────────────────────────────────────┐
│ Cognitive Substrate Layer (nous)                            │
│                                                             │
│  nous-mcp                                                   │
│   ├── StdioTransport & JSON-RPC 2.0 protocol engine         │
│   ├── McpClient handshake, tool enumeration, & execution    │
│   └── McpNativeTool adapter                                 │
│                                                             │
│  nous-tool                                                  │
│   ├── Canonical Tool trait & ToolContext                    │
│   ├── RiskProfile & ToolScope semantic security model       │
│   └── ToolRegistry & Execution pipelines                    │
└─────────────────────────────────────────────────────────────┘
```

### Invariants & Behavioral Boundaries

- **`[INV-TOOL-01] Substrate Tool Compatibility`**:
  Every tool conforming to `nous_tool::Tool` must be runnable within `nuo` via the `NousToolBridge` adapter, preserving declared risk profiles and cancellation tokens.
- **`[INV-MCP-01] Single MCP Protocol Implementation`**:
  All MCP wire protocol serialization, JSON-RPC 2.0 framing, and stdio transport execution must be delegated to `nous-mcp`. `nuo-mcp` must not duplicate MCP protocol request/response serialization.
- **`[INV-TOOL-02] Risk Profile Coherence`**:
  `nuo_contracts::HazardLevel` must derive deterministically from `nous_tool::RiskProfile`. A tool marked `RiskProfile::Destructive` must never degrade to a safe or read-only hazard level in the application layer.

### Positive Consequences

- `nous-mcp` and `nous-tool` become active, foundational components of the `nuo` runtime rather than dead dependencies.
- Eliminates thousands of lines of duplicated JSON-RPC and process transport code in `nuo-mcp`.
- Tools developed anywhere in the `nous` ecosystem can be loaded into `nuo` sessions out of the box.

### Negative Consequences & Trade-offs

- Cross-crate type conversions between `serde_json::Value` arguments and raw JSON strings introduce minor serialization wrapping on tool dispatch.

---

## Rejected Alternatives & Negative Knowledge

### Option 1 (Rejected: Parallel Tool & MCP Implementations)
- **Why considered**: Avoided changing `nuo-mcp` or adding adapter layers.
- **Why rejected**: Creating duplicate implementations of the same MCP protocol and tool contracts defeats the fundamental purpose of the `nous` substrate. Bugs fixed in `nous-mcp` would not benefit `nuo-mcp`, creating divergent behavior.

### Option 2 (Rejected: Forcing `nous_tool::Tool` to Carry Application Concerns)
- **Why considered**: Direct replacement without an adapter layer.
- **Why rejected**: Contaminates the substrate. Forcing `nous_tool::Tool` to declare TUI-specific UI strings (`permission_label`), turn cancellation heuristics (`supports_cooperative_cancel`), and subagent dispatch flags would couple a generic cognitive library to the Nuo host terminal application.

---

## Links

- Substrate Architecture: Ingested under [ADR-0006](0006-absorb-nous-substrate-and-establish-acp-protocol-standard.md)
- Wire Alignment: [ADR-0001 (nuo)](0001-unify-model-wire-protocol-and-application-provider-boundary.md)
- Invariants Constitution: [Invariants Constitution](../governance/documentation/core/invariants.md)
