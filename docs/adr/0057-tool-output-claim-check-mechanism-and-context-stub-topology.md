---
id: ADR-0057
title: "Tool Output Claim-Check Mechanism and Context Stub Topology"
status: accepted
date: 2026-10-28
scope: tool/exec, context/claim-check, harness/rounds, wire/pressure, storage/offstream
superseded_by: null
negative_knowledge: true
---

# 0057. Tool Output Claim-Check Mechanism and Context Stub Topology

- Status: Accepted
- Date: 2026-10-28
- Deciders: Nuo Architecture Working Group, Runtime Engineering
- Consulted: Tooling (`nuo-tool`, `nuo-tool-exec`), Harness (`nuo-harness`), Wire (`nuo-wire`), Server (`nuo-server`)
- Informed: System Architects, Documentation Governance
- Complements: [ADR-0008](0008-single-tool-contract.md), [ADR-0019](0019-token-pressure-measurement-and-budget-accounting.md), [ADR-0056](0056-zero-dependency-class-based-token-estimator.md)

---

## Context and Problem Statement

Autonomous agents frequently execute diagnostics, profiling, stress tests, and build commands (e.g. `perf record/stat`, `strace`, `gdb/lldb`, `cargo test`, `ninja`) that produce massive streams of standard output and error—ranging from tens of thousands of lines to dozens of megabytes.

Historically, this capability suffered from severe architectural vulnerabilities:
1. **Context Window Blowup**: Unbounded shell outputs were formatted as monolithic `ToolOutput::Text` or un-truncated strings, directly entering the live conversational context and exceeding remote provider context limits (`context_length_exceeded`).
2. **Disconnected Offstream Storage**: While an offstream reader infrastructure (`inspect` tool and `call:<id>` handles) existed in theory, command execution did not reliably preserve full raw outputs in content-addressable storage (CAS BlobStore) prior to truncation, leading to dangling or unresolvable handles.
3. **Missing StreamGuard Flood Protection**: Commands with infinite or high-throughput emission (e.g. `perf top`, `tail -f`) ran without in-flight cadence or line ceilings, buffering gigabytes in memory until timeouts occurred.
4. **Terminological Drift**: The pattern was colloquially described using informal or ambiguous terms ("发票机制", "receipt", "paging", "truncation"), lacking a precise and unified architectural vocabulary across code, comments, and documentation.

We require a unified **Claim-Check Mechanism** and formal **Context Stub** architecture that guarantees context safety, bounded memory execution, and complete on-demand inspection fidelity.

---

## Decision Drivers

- **Context Safety Invariant**: No single tool execution may ever overflow the model's active context window.
- **Data Loss Prevention**: Truncating in-context representations must never erase the raw execution artifact; the full unpruned stream must be durable and retrievable.
- **Architectural Terminology Unification**: Standardize on **Claim-Check Mechanism** (the macro pattern of offloading large payloads to cold store) and **Context Stub** (the in-context lightweight surrogate carrying head/tail previews and redemption handles).
- **In-Flight StreamGuard Defense**: Terminate runaway or unbounded continuous streaming tools before they exhaust system RAM or time budgets.
- **Zero Ambiguity in Model Dialogue**: Context stubs must clearly indicate omitted token counts, structural head/tail anchors, and explicit `inspect(handle="call:<id>")` invocation instructions.

---

## Decision Outcome

We establish the **Tool Output Claim-Check Mechanism** and **Context Stub Topology** across all capability crates:

### 1. Conceptual Distinction: Claim-Check vs. Stub
- **Claim-Check Mechanism**: The overarching architectural pattern. Large execution outputs ($> \text{threshold}$) are offloaded out-of-band to content-addressable storage (CAS BlobStore), returning a lightweight claim-check voucher into the message stream.
- **Context Stub (`Stub`)**: The concrete data object and in-context projection. A stub encapsulates:
  - Bounded structural preview: Head lines (command prelude, configuration) and Tail lines (exit status, panics, summary).
  - Elided metric notice: Human- and model-readable token/line count of omitted content.
  - Claim-Check Handle: Uniform locator URI (e.g. `call:<tool_call_id>`) enabling offstream demand-paging via the `inspect` tool.

### 2. Dual-Threshold Bounded Truncation
- **`SHELL_MAX_OUTPUT_CHARS` (8,000 bytes / ~2,000 tokens)**: When total combined output exceeds this watermark, Claim-Check processing activates.
- **`SHELL_TRUNCATED_CHARS` (4,000 bytes / ~1,000 tokens)**: The stub retains the initial 2,000 bytes (Head) and the trailing 2,000 bytes (Tail), with an explanatory omission banner dividing them.

### 3. In-Flight StreamGuard Flood Protection
Foreground shell processes executed via `ExecuteCommandTool` enforce real-time online streaming constraints calibrated symmetrically to standard 80-column terminal densities:
- **Line Limit (`SHELL_STREAM_FLOOD_LINES` = 200 lines)**: Symmetrically calibrated to ~80 bytes per terminal line ($200 \times 80\text{ B} = 16\text{ KB}$).
- **Byte Limit (`SHELL_STREAM_FLOOD_BYTES` = 16 KB)**: Exactly 2× the base `SHELL_MAX_OUTPUT_CHARS` (8 KB) stub watermark, preventing runaway logging loops from consuming excessive memory.
- **Relaxed Raw Bounds (`RAW_STREAM_FLOOD_LINES` = 1,000 lines, `RAW_STREAM_FLOOD_BYTES` = 80 KB)**: Applied when `raw: true` is explicitly passed by the caller.
Processes crossing these bounds without self-terminating are halted early under `ShellTermination::StreamGuard`, preventing runaway buffering.

### 4. Canonical Claim-Check Stub Format
When an execution output is stubbed, the in-context representation conforms to the standard specification:
```text
[Exit status: 0]
--- stdout ---
<Head preview>
⋯ [Claim-Check Stub: {dropped_tokens} tokens / {dropped_lines} lines omitted to protect context budget] ⋯
<Tail preview>

[Claim-Check Handle: inspect full raw output with `inspect(action="read", handle="call:{call_id}")`]
```

---

## Invariants & Behavioral Boundaries

- **`[INV-CLAIM-01] Zero Context Blowup Guarantee`**: Every tool output emitted into the live ReAct message window MUST be bounded by context budgets before provider dispatch.
- **`[INV-CLAIM-02] Verbatim Offstream Fidelity`**: Whenever a tool output is converted to a Context Stub, the complete un-truncated raw stream MUST be preserved in persistent CAS storage before the turn completes.
- **`[INV-CLAIM-03] Universal Inspectability`**: Every generated Claim-Check handle (`call:<tool_call_id>`) MUST be resolvable by the `inspect` tool for keyword filtering and paginated retrieval.
- **`[INV-CLAIM-04] Terminology Consistency`**: Code declarations, docstrings, user-facing notices, and architecture documentation MUST use **Claim-Check** for the offloading mechanism and **Stub** for the in-context surrogate.

---

## Rejected Alternatives

### 1. Hard Drop / Blind Truncation Without Offstream Store
- **Why considered**: Simple to implement via string slicing (`text[..4000]`).
- **Why rejected**: Destroys critical forensic information. If a compiler or profiler emits a diagnostic error in the middle of 10,000 lines, the agent loses the ability to diagnose the failure without re-running the entire expensive command.

### 2. Automatic Full-Text Injection on Next Turn
- **Why considered**: Attempts to transparently provide full logs without explicit tool calls.
- **Why rejected**: Defeats the purpose of context preservation. The context window would still be blown up on subsequent turns, deteriorating attention and model reasoning.

### 3. Retain Ambiguous Nomenclature ("发票机制" / "Receipt / Paging")
- **Why considered**: Informal terms already used colloquially in dialogue.
- **Why rejected**: "发票" is a financial accounting invoice, while "Receipt" conflates protocol confirmations with data storage. "Claim-Check" and "Stub" are established, unambiguous computing terms that map directly to standard system patterns.
