---
id: ADR-0070
title: "Unified Content-Addressed Storage, Virtual File System Projection, and Epistemic Meta-Tool Abolition"
status: accepted
date: 2026-11-09
scope: storage/cas, vfs/projection, harness/rounds, agent/compactor, tool/nuo-tool-fs, tui/nuo-tui
superseded_by: null
negative_knowledge: true
---

# 0070. Unified Content-Addressed Storage, Virtual File System Projection, and Epistemic Meta-Tool Abolition

- Status: Accepted
- Date: 2026-11-09
- Deciders: Nuo Architecture Working Group, Runtime Engineering, TUI Presentation
- Consulted: Agent Engineering (`nuo-agent`), Harness (`nuo-harness`), Tool Infrastructure (`nuo-tool`, `nuo-tool-fs`), TUI (`nuo-tui`), Persistence (`nuo-persistence`)
- Supersedes: [ADR-0069](0069-execution-log-spilling-epistemic-context-paging-and-anti-recursion-topology.md)
- Amends: [ADR-0063](0063-bifurcated-shell-tool-presentation-out-of-band-status-and-clean-stream-topology.md)

---

## Context and Problem Statement

ADR-0069 established *Execution Log Spilling* and the `recall_context` meta-tool to prevent context blowup and eliminate Russian Doll recursion. However, production practice and system-level scrutiny revealed fundamental architectural tensions and epistemic bad smells:

1. **Tool Ambiguity and Cognitive Dilution**:
   The coexistence of `recall_context` and `recall_memory` created severe cognitive confusion for language models and operators. Both tools were framed as "cognitive memory retrieval", yet one operated as an episodic cross-session dialogue memory while the other operated as an ad-hoc line-pager over internal session traces. Models routinely called the wrong tool, passed text queries to the line-pager, or suffered tool selection paralysis.

2. **ReAct Tax and Blind Paging**:
   `recall_context` imposed a rigid line-budgeted window (`offset_lines`, `limit_lines`). To find a single failure or panic in a 10,000-line subagent execution log, an autonomous model was forced to execute dozens of sequential ReAct paging turns (`offset=0`, `offset=80`, `offset=160`), burning massive token budgets, wall-clock time, and cost without native filtering.

3. **Workspace-Free Role Incompatibility**:
   In Nuo, roles such as `ops` and `philosophist` are strictly workspace-free (`workspace_root: None`, ADR-0220). Relative virtual paths like `.nuo/virtual/...` presuppose an active project directory and fail or trigger path guessings in workspace-free sessions. Conversely, pseudo-absolute roots like `/virtual/...` suggest elevated system root permissions (`/`), creating permission confusion for non-root users.

4. **Token Estimation Drift vs. Deterministic Physical Metrics**:
   Local token estimation (e.g., character heuristics or mismatched BPE tokenizers) is inherently non-deterministic and drifts by up to 200% across provider model families (Claude, GPT-4o, DeepSeek). Crucially, tools like `read_text` accept line numbers (`offset`, `limit`), not tokens. Reporting token approximations forces the model into inaccurate mental arithmetic.

5. **Blob Truncation vs. Multi-line Reality**:
   Output truncation operates at the byte/character boundary of the entire text blob. In scenarios involving minified code (`bundle.min.js`), massive single-line JSON, or base64 streams, a single line can contain megabytes of data. Reporting "lines omitted" in such cases is mathematically absurd and misleads the model into executing dangerous whole-line reads.

---

## Decision Drivers

- **Zero Legacy Burden (`[INV-VFS-01]`)**: Abolish intermediate meta-tools (`recall_context`, `inspect`) entirely. Unify model inspection under standard, universally trained POSIX primitives (`read_text`, `execute_command (grep/tail/jq)`, `read_image`).
- **Single Source of Truth in CAS (`[INV-CAS-01]`)**: All offstream observations, folded causal checkpoints, execution overflow, and subagent traces must be durably backed by SHA-256 Content-Addressed Storage (`BlobStore`) in persistence.
- **Rootless Sovereign VFS Addressing (`[INV-VFS-02]`)**: Standardize all virtual resource paths under `/tmp/nuo/vfs/{sha256}.{ext}` (and `%TEMP%\nuo\vfs\...` on Windows). This is globally accessible without root privileges, immune to workspace absence, and clean.
- **In-Process VFS Zero-I/O Interception (`[INV-VFS-03]`)**: Virtual paths must resolve in-memory directly from CAS within tool handlers, ensuring zero disk writes, zero inode exhaustion, and zero workspace pollution.
- **Deterministic Physical Metrics (`[INV-METRIC-01]`)**: Eliminate local token estimation in omission banners. Use deterministic physical metrics (`lines` + `KB` for multi-line streams; `characters` + `KB` for single-line blobs).
- **Calm, High-Signal Industrial Presentation**: In `nuo-tui`, remove decorative glyph noise (`·`), elevate folded blocks into prominent structural dividers (`┈┈┈ N lines omitted ┈┈┈`), and segregate execution outcome metadata into an unaggressive, explained status footer with distinct surface elevation (`match_title_surface`).

---

## Architectural Decisions

### 1. The CAS + VFS Projection Architecture

All historical, folded, or spilled session artifacts are managed under a strict two-tier storage and projection model:

```text
┌────────────────────────────────────────────────────────────────────────┐
│                        Authoritative Storage (CAS)                     │
│                        - SQLite causal DAG & thread records            │
│                        - SHA-256 BlobStore (compressed payload storage)│
└────────────────────────────────────┬───────────────────────────────────┘
                                     │
                                     ▼
┌────────────────────────────────────────────────────────────────────────┐
│                      In-Process VFS Resolver Layer                     │
│         - Intercepts paths matching `/tmp/nuo/vfs/...` or `/virtual/...`│
│         - Resolves slices from CAS / ObservationStore in memory        │
└────────────────────────────────────┬───────────────────────────────────┘
                                     │
           ┌─────────────────────────┴─────────────────────────┐
           ▼                                                   ▼
┌──────────────────────────────────────┐    ┌──────────────────────────────────────┐
│           `read_text` Tool           │    │          `read_image` Tool           │
│   Reads code, folded history, sub-   │    │   Rehydrates pruned multimodal media │
│   agent traces with line pagination  │    │   directly from CAS image bytes      │
└──────────────────────────────────────┘    └──────────────────────────────────────┘
```

1. **Rootless Sovereign Address Standard**:
   All virtual resources conform to:
   ```text
   /tmp/nuo/vfs/{sha256_short}.{ext}
   ```
   - Standard prefix: `/tmp/nuo/vfs/`
   - Content hash: 12-char SHA-256 content hash
   - Semantic extension: `.log` (shell output), `.txt` (text context), `.md` (agent traces), `.png` (images)

2. **In-Process VFS Resolution**:
   When `read_text` receives a path prefixed with `/tmp/nuo/vfs/`, it delegates to the in-process `VirtualFileResolver`, extracting lines from memory without writing a physical file to disk.

### 2. Adaptive Blob Omission Banners

When command output or context is truncated, the banner dynamically reflects the underlying physical structure:

- **Multi-line Stream (`lines > 1`)**:
  ```text
  --- [Middle 380 lines (16.2 KB) omitted to protect context budget] ---
  - Virtual Path: "/tmp/nuo/vfs/e3b0c44298fc.log"
  - Guidance: Use `read_text(path: "/tmp/nuo/vfs/e3b0c44298fc.log", offset: 51, limit: 100)` or `execute_command` with grep/tail to inspect.
  ```

- **Giant Single-Line Blob (`lines <= 1`, e.g. minified JS, giant JSON)**:
  ```text
  --- [Middle 85,200 characters (85.2 KB) omitted to protect context budget] ---
  - Virtual Path: "/tmp/nuo/vfs/7f8a9b0c1d2e.log"
  - Guidance: Large single-line content detected. Use `execute_command` with cut/jq/head on the log or `read_text` to inspect.
  ```

### 3. Epistemic Tool Surface Simplification

- **Abolition of `recall_context`**: `RecallContextTool` is permanently removed from `nuo-agent` and `nuo-harness`.
- **Single Memory Tool**: `recall_memory` remains the only memory tool, reserved exclusively for cross-session episodic dialogue memory.

### 4. TUI `execute_command` Presentation Modernization

1. **Middle Dot Elimination**: Replace ornamental `·` in step headers with structured, stable typography (e.g. `Run cargo (running)`, `Run cargo (120ms)`).
2. **Prominent Folding Divider Band**: Replace the dim single-line `⋯ N lines hidden` with a visible divider band (`┈┈┈ N lines omitted ┈┈┈┈┈┈┈┈┈`).
3. **Out-of-Band Status Footer**: Move `exit <code>` out of raw stdout into a subtly elevated status footer (`theme.match_title_surface()`), explaining canonical POSIX exit codes (e.g. `exit 1 (general error)`, `exit 127 (command not found)`, `exit 137 (killed / out of memory)`) without aggressive yellow-bold styling.

---

## Invariants Established

- **`[INV-VFS-01] Zero Legacy Meta-Tools`**: The active agent capability catalog shall not register specialized meta-tools for internal context pagination. Inspection occurs exclusively through standard file inspection tools.
- **`[INV-VFS-02] Rootless Sovereign Addressing`**: VFS paths must resolve under `/tmp/nuo/vfs/`, operating without root permissions and remaining independent of workspace configuration.
- **`[INV-CAS-01] Single Source of Truth`**: Offstream data references must resolve against durable content-addressed storage or in-memory observation stores, surviving session serializations.
- **`[INV-METRIC-01] Deterministic Physical Metrics`**: Omission notices must report deterministic physical metrics (`lines`, `characters`, `KB`) rather than local token estimations.

---

## Rejected Alternatives & Negative Knowledge

1. **Retaining Local Token Estimation in Truncation Banners**:
   *Why rejected*: Token counts derived from local estimators drift significantly across model vocabularies (Claude, GPT-4o, DeepSeek) and provide zero actionable alignment with line-based parameters (`offset`, `limit`) in `read_text`.
2. **Workspace-Relative VFS Paths (`.nuo/vfs/...`)**:
   *Why rejected*: Fails in workspace-free roles (`ops`, `philosophist`) and risks polluting project git working trees.
3. **Pseudo-Absolute Root Paths (`/virtual/...`)**:
   *Why rejected*: Suggests system root mount points (`/`), failing when external non-root subprocesses attempt to open them directly without root privileges.
