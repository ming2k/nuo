---
id: ADR-0069
title: "Execution Log Spilling, Epistemic Context Paging, and Anti-Recursion Topology"
status: accepted
date: 2026-11-08
scope: tool/exec, context/recall, harness/rounds, storage/offstream, protocol/session-ir
superseded_by: null
negative_knowledge: true
---

# 0069. Execution Log Spilling, Epistemic Context Paging, and Anti-Recursion Topology

- Status: Accepted
- Date: 2026-11-08
- Deciders: Nuo Architecture Working Group, Runtime Engineering
- Consulted: Tooling (`nuo-tool`), Harness (`nuo-harness`), Server (`nuo-server`), Protocol (`nuo-protocol`), TUI (`nuo-tui`)
- Informed: System Architects, Release Engineering
- Supersedes: [ADR-0057](0057-tool-output-claim-check-mechanism-and-context-stub-topology.md)
- Complements: [ADR-0008](0008-single-tool-contract.md), [ADR-0058](0058-granular-causal-session-forking-sovereign-tab-projection-and-aside-retirement.md), [ADR-0059](0059-canonical-thread-persistence-schema-and-zero-legacy-clean-break.md)

---

## Context and Problem Statement

ADR-0057 established the *Tool Output Claim-Check Mechanism* to protect LLM context windows by intercepting oversized command executions and replacing them with a context "stub" carrying an opaque voucher handle (`call:<tool_call_id>`). To retrieve truncated text, models were instructed to invoke an `inspect` meta-tool.

Operational experience in production and stress-testing uncovered four fatal architectural defects in this model:

1. **The Recursive Claim-Check ("Russian Doll / 套娃") Failure Mode**:
   The harness universal claim-check interceptor monitored all tool outputs exceeding `SHELL_MAX_OUTPUT_CHARS` (8,000 bytes / ~2,000 tokens). However, `inspect`'s default retrieval budget was 4,096 tokens (~16,000 bytes). Lacking tool immunity, when a model invoked `inspect`, the retrieved content exceeded the watermark, causing the interceptor to wrap the `inspect` output into *another* claim-check voucher (`call:<inspect_call_id>`). This trapped autonomous models in infinite recursive extraction loops.
2. **Domain Conflation (POSIX I/O vs. Epistemic Memory)**:
   ADR-0057 conflated transient execution streams (stdout/stderr of compiler runs, tests, shell commands) with persistent cognitive state (subagent trajectories, causal compaction nodes, multimodal image assets). External shell outputs are ephemeral stream artifacts best queried using native operating system utilities, whereas cognitive states are internal directed acyclic graph (DAG) nodes in SQLite.
3. **Ergonomic Degradation and ReAct Tax**:
   Opaque handles (`call:call_xyz`) hid data formats, size, and layout from the model. Rather than utilizing pre-trained, high-performance Unix primitives (`grep`, `ripgrep`, `head`, `tail`, `sed`), the model was forced to waste entire ReAct turns and tokens calling a primitive `inspect` tool with fragile `cursor` strings and rudimentary query filters.
4. **Cognitive Nomenclature Drift ("Stub")**:
   While "stub" originated from physical ticket/check stubs (consistent with claim checks), in modern software engineering it is heavily overloaded with "test stubs" and "dummy mocks", introducing cognitive friction to models and developers.

We require a modern, decoupled, zero-legacy architecture that eliminates recursion, restores native operating system tooling primacy for execution I/O, and refactors internal memory paging into a dedicated, bounded cognitive tool.

---

## Decision Drivers

- **Zero Recursion Invariant (`[INV-RECALL-02]`)**: Under no circumstances may an offstream inspection or retrieval tool trigger secondary truncation or generate nested vouchers.
- **Native OS Tooling Primacy (`[INV-LOG-01]`)**: Large execution outputs must be persisted to transparent, directly addressable physical files, allowing models to use native filesystem and command-line tools without intermediate RPC shims.
- **Separation of I/O Spilling from Cognitive Recall**: Maintain a strict firewall between transient external stream overflow and internal session DAG memory.
- **Deterministic Pagination Clamping (`[INV-RECALL-03]`)**: Context retrieval tools must enforce strict, safe budget bounds directly at the tool boundary, guaranteeing zero context blowup.
- **Clean Epistemic Vocabulary**: Eliminate confusing metaphors (`Claim-Check Stub`, `ticket vouchers`) in favor of direct, professional technical descriptors (`Execution Log Spilling`, `Folded Checkpoint`, `Subagent Trajectory`).

---

## Decision Outcome

We supersede ADR-0057 and establish the **Execution Log Spilling and Epistemic Context Paging Topology**:

### 1. Bifurcated Data Topology: POSIX Spilling vs. Epistemic Context

The system bifurcates offstream data into two strictly separated domains:

| Domain | Data Classes | Storage Medium | Retrieval Mechanism |
| :--- | :--- | :--- | :--- |
| **Execution I/O** | `execute_command` stdout/stderr, search hits | Transparent physical files (`.nuo/exec_logs/exec_<id>.log`) | Native OS tools (`grep`, `tail`, `read_text`) via semantic in-context guidance |
| **Epistemic Context** | Subagent traces (`sub:`), Compacted DAG history (`fold:`), Pruned visual media (`artifact:`) | Relational SQLite (`threads`, `causal_nodes`) & Content-Addressed BlobStore | Dedicated `recall_context` tool with deterministic line pagination |

### 2. Transparent Execution Log Spilling (`[INV-LOG-01]`)

When a shell or tool execution exceeds `SHELL_MAX_OUTPUT_CHARS`:
1. The full, unpruned raw stdout and stderr are written directly to `$XDG_STATE_HOME/nuo/exec_logs/exec_<tool_call_id>.log`.
2. The in-context tool result projects a structured preview (Head lines + Tail lines) dividing omitted content with a clear textual omission banner (`--- [Middle output omitted to protect context budget] ---`).
3. The result provides explicit, model-actionable semantic guidance referencing the absolute log path:
   ```text
   [Output truncated: 4500 tokens total]
   <Head preview>
   --- [Middle output omitted to protect context budget] ---
   <Tail preview>

   [Full raw output saved to: /path/to/.nuo/exec_logs/exec_123.log]
   [Guidance: Use `execute_command` with grep/tail or `read_text` with line limits to inspect specific sections.]
   ```
4. **Voucher Retirement**: Generating CAS BlobStore vouchers (`call:<tool_call_id>`) for external tool execution outputs is permanently abolished.

### 3. Epistemic Context Paging Tool (`recall_context`)

The legacy `inspect` tool is refactored and specialized into `recall_context` (`[INV-RECALL-01]`):
- **Exclusive Scope**: Handles only internal session state schemes:
  - `sub:<session_id>`: Subagent execution transcripts.
  - `fold:<compaction_node_id>`: Compaction checkpoints and folded causal ancestor nodes.
  - `artifact:<blob_hash>`: Pruned multimodal visual media (rehydrating `ToolOutput::Image`).
- **Deterministic Schema**: Eliminates opaque `cursor` strings and useless `action="list"` enums. Standardizes on intuitive line-based offsets:
  ```json
  {
    "target": "sub:... | fold:... | artifact:...",
    "query": "optional substring/regex line filter",
    "offset_lines": 0,
    "limit_lines": 80
  }
  ```

### 4. Universal Immunity and Hard Clamping (`[INV-RECALL-02]`, `[INV-RECALL-03]`)

- **Tool Immunity (`[INV-RECALL-02]`)**: In `nuo-harness/src/agent/rounds.rs`, tool results produced by `recall_context` are strictly exempt from Claim-Check or Log Spilling interception. Their output passes directly into the message window.
- **Budget Clamping (`[INV-RECALL-03]`)**: `recall_context` enforces a hard line limit ($20 \le \text{limit} \le 200$ lines, capped at $\le 2,000$ tokens). If content exceeds the requested limit, the tool internally emits a clean continuation notice (`[... Truncated to fit limit (N total lines). Re-invoke recall_context with offset_lines=M ...]`), eliminating the possibility of runaway recursion.

---

## Invariants & Behavioral Boundaries

- **`[INV-LOG-01] Transparent Execution Log Spilling`**: Whenever shell execution or command output exceeds the context threshold, the complete unpruned stream MUST be written to an accessible file in the execution logs directory (`exec_logs/exec_<id>.log`), accompanied by head/tail preview markers and OS filter guidance. CAS Blob vouchers (`call:<id>`) for external execution I/O are strictly prohibited.
- **`[INV-RECALL-01] Epistemic Context Specialization`**: The `recall_context` tool is dedicated exclusively to offstream cognitive and session states (`sub:`, `fold:`, `artifact:`). It MUST NOT be used to page local command execution logs.
- **`[INV-RECALL-02] Universal Immunity and Non-Recursion`**: Outputs emitted by `recall_context` MUST be immune from truncation and log-spilling interception. No execution turn may ever generate a nested handle pointing to a previous retrieval call.
- **`[INV-RECALL-03] Deterministic Pagination Clamping`**: The `recall_context` tool MUST enforce hard line and token boundaries ($20 \le \text{limit} \le 200$ lines, $\le 2000$ tokens) with 0-based `offset_lines`, preventing context window overflow at the source.

---

## Rejected Alternatives & Negative Knowledge

1. **Retain Universal Claim-Check Vouchers for All Outputs (ADR-0057)**:
   - *Why rejected*: Produced severe recursive "Russian doll" loops, hid data behind artificial RPC abstractions, and crippled the model's native ability to run `grep`/`tail` on log files.
2. **Pure Passive Context Pre-Injection (RAG) for Epistemic Memory**:
   - *Why rejected*: Context needs emerge dynamically mid-turn during multi-step reasoning, which static pre-turn injection cannot anticipate. Furthermore, RAG is probabilistic and heuristic, whereas forensic history lookup requires deterministic, loss-free addressing.
3. **Conversation Forking as a Memory Lookup Primitive**:
   - *Why rejected*: Forking is an ontological mutation that clones session trees and mounts new tabs. Using `/fork` merely to inspect past parameters violates separation of concerns.
