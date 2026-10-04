---
id: ADR-0013
title: "Decoupled Tool Architecture: Dedicated Tool Namespace, Semantic Nomenclature, and Infrastructure Purity"
status: accepted
date: 2026-10-03
scope: workspace/topology, architecture/layering, capability/tools, tools/namespace
superseded_by: null
negative_knowledge: true
---

# 0013. Decoupled Tool Architecture: Dedicated Tool Namespace, Semantic Nomenclature, and Infrastructure Purity

- Status: Accepted
- Date: 2026-10-03
- Deciders: Nuo Architecture Working Group
- Consulted: Runtime, Capability, and Interface Teams
- Informed: System Architects, Release Engineering
- Amends: [ADR-0001](0001-flat-workspace-and-microkernel-capability-topology.md) §`[INV-TOOL-01]`, [ADR-0012](0012-domain-aligned-capability-topology-and-tool-asymmetry-elimination.md)
- Complements: [ADR-0008](0008-single-tool-contract.md), [ADR-0010](0010-harness-decomposition-and-agent-unification.md)

---

## Context and Problem Statement

Following ADR-0010 (harness decomposition) and ADR-0012 (domain capability alignment), an architectural evaluation revealed a fundamental conflation of abstraction dimensions:

1. **Conflation of Infrastructure Primitives vs. Agent Tool Projections**:  
   - Operating system primitives (`nuo-host`) and AST parsing engines (`nuo-code`) were forced to embed Agent-facing `nuo_tool::Tool` implementations.
   - Low-level OS platform libraries were writing model-facing JSON Schemas, prompt descriptions (`"Replace an exact, unique block of text..."`), line-number formatting (`[Lines 1-50 of 200...]`), and model error diagnoses.
   - An OS Platform Abstraction Layer (PAL) and an AST parsing library should have zero awareness of LLMs or agent prompt engineering. Embedding tool implementations inside them represents severe **abstraction leakage**.

2. **The "Single Crate + Feature Flags" Anti-Pattern**:  
   Consolidating all tools into a single catch-all crate (e.g. `nuo-tools` with Cargo feature flags) was evaluated and discarded due to three fatal Rust compiler defects:
   - **Feature Unification Trap**: Cargo features are additive across the workspace dependency graph. If target A enables `fs` and target B enables `web`, Cargo compiles all unified dependencies, destroying compile-time isolation.
   - **Invalidation of the Atomic Compilation Unit**: `rustc` compiles at the crate level. A change to a single web search regex in a monolithic tools crate forces re-typechecking, re-borrowchecking, and monomorphization of all filesystem and sandbox code, destroying incremental build speed and multi-core parallelism (`cargo build -j`).
   - **Cross-Compilation & Target Pollution**: OS tools require Unix `libc` and Windows C APIs, whereas web and AST tools are platform-agnostic and should be able to compile for `wasm32` or edge sandboxes. Bundling them in a single crate contaminates platform-neutral tools with platform-specific system dependencies.

3. **Semantic Ambiguity in Nomenclature**:  
   Crate names like `nuo-web` or `nuo-search` created severe confusion (sounding like web servers, web frontends, or workspace code search). Tool crates should unambiguously state their functional domain: `fs`, `exec`, `web`, `ast`.

---

## Decision Drivers

- **Strict Separation of Abstraction Dimensions**:  
  - *Infrastructure Layer*: OS sandboxes (`nuo-host`), AST parsing (`nuo-code`). 100% deterministic, zero LLM awareness.
  - *Tool Projection Layer*: Adapts infrastructure primitives into `nuo_tool::Tool` plugins for LLM consumption (JSON Schemas, descriptions, prompt diagnostics, output formatting).
- **Physical Compiler Isolation**: Each tool family must be an independent crate (its own atomic compilation unit), preventing Cargo feature unification and preserving incremental build parallelism.
- **Dedicated Tool Namespace (`tools/`)**: Tool plugin crates reside within a dedicated `tools/` namespace (`tools/nuo-tool-*`), preventing root-level clutter as tool families expand.
- **Semantic Nomenclature**: Tool crates are named precisely by their functional domain: `nuo-tool-fs`, `nuo-tool-exec`, `nuo-tool-web`, `nuo-tool-ast`.

---

## Decision Outcome

```text
┌────────────────────────────────────────────────────────────────────────┐
│                        Layer 0: Zero-Runtime Contract                  │
│                   nuo-tool (Tool trait, ToolDescriptor)                │
└───────────────────────────────────┬────────────────────────────────────┘
                                    │ implemented by
                                    ▼
┌────────────────────────────────────────────────────────────────────────┐
│                   Layer 2: Agent Tool Namespace (tools/)               │
│                                                                        │
│  • tools/nuo-tool-fs    -> read_text, write_file, edit_text, list_dir  │
│  • tools/nuo-tool-exec  -> execute_command (run_command)               │
│  • tools/nuo-tool-web   -> web_search, web_reader                      │
│  • tools/nuo-tool-ast   -> code_query                                  │
│                                                                        │
│  (专注面向 LLM 的 JSON Schema、Prompt Description、输出格式化与报错诊断)│
└──────────────┬────────────────────┬────────────────────┬───────────────┘
               │ 调用系统能力       │ 调用语法解析       │ 发起网络出站
               ▼                    ▼                    ▼
┌────────────────────────┐┌──────────────────┐┌─────────────────────────┐
│        nuo-host        ││     nuo-code     ││         netune          │
│   【纯 OS 基础设施】   ││ 【纯 AST 语法库】││      (HTTP Client)      │
│ • 零 LLM 概念          ││ • 零 LLM 概念    ││                         │
│ • 沙箱、进程、原子文件 ││ • 节点遍历、符号 ││                         │
└────────────────────────┘└──────────────────┘└─────────────────────────┘
```

### 1. Dedicated `tools/` Namespace
A dedicated workspace directory `tools/` is established for all official Agent tool plugins:
- `tools/nuo-tool-fs`: Filesystem inspection and mutation tools (`read_text`, `write_file`, `edit_text`, `list_dir`, `find_files`, `search_text`).
- `tools/nuo-tool-exec`: Sandboxed shell execution tool (`execute_command`, alias `run_command`).
- `tools/nuo-tool-web`: Outbound web search and page reader tools (`web_search`, `web_reader`, SSRF firewall).
- `tools/nuo-tool-ast`: AST structural syntax query tool (`code_query`).

### 2. Infrastructure Purity
- `nuo-host` is cleansed of all `nuo_tool::Tool` implementations and prompt strings. It becomes a pure OS platform abstraction layer (PAL).
- `nuo-code` is cleansed of `CodeQueryTool` and becomes a pure Tree-sitter parsing engine.
- `nuo-web` is deleted and replaced by `tools/nuo-tool-web`.

---

## Invariants & Behavioral Boundaries

- **`[INV-TOOL-14] Dedicated Tool Namespace`**: Official native Agent tool plugin crates MUST reside in `tools/nuo-tool-*`. Root directory crates are reserved for core subsystems, protocols, and infrastructure libraries.
- **`[INV-INFRA-01] Infrastructure Purity`**: Low-level infrastructure crates (`nuo-host`, `nuo-code`) MUST NOT depend on `nuo-tool` or implement `nuo_tool::Tool`. They must contain zero model-facing JSON schemas, prompt descriptions, or LLM output formatters.
- **`[INV-TOOL-15] Semantic Tool Nomenclature`**: Tool crates must be named according to their functional capability: `nuo-tool-fs`, `nuo-tool-exec`, `nuo-tool-web`, and `nuo-tool-ast`. Generic names such as `nuo-tool-host` or ambiguous names like `nuo-search` are prohibited.

---

## Positive Consequences

- **Architectural Symmetry & Clarity**: A clear dividing line between system infrastructure (how to safely execute a process) and agent tool plugins (how to present that process to an LLM).
- **Optimal Parallel Compilation**: Each tool family compiles in parallel with zero feature pollution or transitive rebuild amplification.
- **Platform Portability**: `nuo-tool-web` and `nuo-tool-ast` remain free of Unix/Windows system call dependencies, ensuring future WASM and edge compatibility.

---

## Rejected Alternatives & Negative Knowledge

### Monolithic Tools Crate with Feature Flags (Rejected)
- **Why considered**: Fewer workspace crates.
- **Why rejected**: Suffers from Cargo feature unification (features are additive across the workspace), destroys incremental build parallelism, and forces platform-specific dependencies onto platform-agnostic tools.

### Inlining Tool Projections Directly into Infrastructure Crates (Rejected)
- **Why considered**: Keeps the number of crates lower.
- **Why rejected**: Violates single responsibility and causes abstraction leakage. OS platform abstraction libraries should not write prompt engineering text or format markdown diffs for language models.

---

## Links

- [ADR-0001: Flat Workspace Topology and Microkernel Capability Topology](0001-flat-workspace-and-microkernel-capability-topology.md)
- [ADR-0008: Single Tool Contract: Metadata-as-Data, Capabilities-via-Context](0008-single-tool-contract.md)
- [ADR-0010: Harness Decomposition, Capability Tool Decoupling, and Cognitive Runtime Unification](0010-harness-decomposition-and-agent-unification.md)
- [ADR-0012: Domain-Aligned Capability Topology and Tool Asymmetry Elimination](0012-domain-aligned-capability-topology-and-tool-asymmetry-elimination.md)
