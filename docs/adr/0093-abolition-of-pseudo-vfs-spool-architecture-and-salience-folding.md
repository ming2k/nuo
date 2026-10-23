---
id: ADR-0093
title: "Abolition of Pseudo-VFS Projection, Ephemeral Session Spool Architecture, and Salience-Aware Zero-Injection Truncation"
status: accepted
date: 2026-11-26
scope: harness/rounds, tool/nuo-tool, tool/nuo-tool-fs, agent/compactor, protocol/pressure, tui/disclosure
superseded_by: null
negative_knowledge: true
---

# 0093. Abolition of Pseudo-VFS Projection, Ephemeral Session Spool Architecture, and Salience-Aware Zero-Injection Truncation

- Status: Accepted
- Date: 2026-11-26
- Deciders: Nuo Architecture Working Group, Runtime Engineering, Security & Agent Alignment
- Consulted: Agent Engineering (`nuo-agent`), Harness (`nuo-harness`), Tool Infrastructure (`nuo-tool`, `nuo-tool-fs`), Protocol (`nuo-protocol`), TUI Presentation (`nuo-tui`)
- Supersedes: [ADR-0070](0070-unified-cas-vfs-projection-and-epistemic-meta-tool-abolition.md) (specifically VFS projection & in-band guidance)
- Amends: [ADR-0063](0063-bifurcated-shell-tool-presentation-out-of-band-status-and-clean-stream-topology.md)

---

## Context and Problem Statement

ADR-0070 eliminated `recall_context` to mitigate meta-tool cognitive confusion and projected truncated tool outputs to virtual file paths (`/tmp/nuo/vfs/{hash}.log`), instructing agents via inline natural language guidance (`[Guidance: Use read_text(...) or execute_command with grep/tail to inspect.]`).

In production and sub-agent execution, this design suffered from fundamental architectural and security defects:

1. **In-Band Prompt Injection Collisions (`[INV-TRUNC-01]`)**:
   Modern LLMs with safety alignment and indirect prompt-injection defenses (such as `WebUntrustedContentGuidance`) are instructed to treat tool outputs as untrusted data and strictly reject imperative directives embedded in tool returns. When the harness injected `[Guidance: Use read_text(...) or execute_command with grep/tail to inspect.]` into the `ToolResult` payload, safety-aligned subagents correctly flagged this text as a malicious prompt injection attempting to induce command execution on `/tmp` paths, resulting in turn failure and operator warnings.

2. **The "Virtual" vs "Physical" Ontological Paradox**:
   ADR-0070 claimed an in-process memory VFS (*"without writing a physical file to disk"*), yet advised agents to use `execute_command with grep/tail`. OS subshell processes cannot read Rust in-memory resolvers; to prevent execution failures, the harness secretly performed triple synchronous writes to physical disk (`/tmp/nuo/vfs/{hash}.log`, and two copies in `~/.local/state/nuo/logs/exec/`).

3. **ReAct Tax and Blind Offset Paging**:
   Hardcoded guidance recommending `offset: 1, limit: 100` directed models to read initialization boilerplate rather than the error site (which typically resides in the tail). This introduced 3–5 redundant roundtrips per truncation.

4. **Privilege Inversion**:
   Read-only subagents (such as `SubAgentProfile::EXPLORE`) were advised to use `execute_command`, a tool forbidden by their security profile.

---

## Decision Drivers

- **Zero Prompt-Injection Ambiguity**: Strip all imperative natural language directives from tool result envelopes.
- **Physical Reality Decoupled from Virtual Pretense**: Transparently treat large execution outputs as ephemeral OS session spools rather than pretending they are in-process memory objects.
- **Zero ReAct Roundtrip Tax for Diagnoses**: Surface critical error and failure signatures directly in the truncation banner via tail-heavy biasing and salience scanning.
- **Role Autonomy**: The harness must never dictate tool invocation to autonomous agents.

---

## Architectural Decisions

### 1. Abolition of Imperative In-Band Guidance (`[INV-TRUNC-01]`)

`ToolResult` payloads and compaction placeholders MUST NOT contain imperative guidance (e.g. `[Guidance: Use ...]`). All truncation envelopes MUST be purely declarative, structured metadata blocks:

```text
--- [Middle 4,500 lines (180.2 KB) omitted to protect context budget. 2 salient signature(s) detected:] ---
>> [line +1204]: cargo:warning=linker `lld` not found
>> [line +3412]: error[E0425]: cannot find value `config` in this scope
--- [Full un-truncated output spooled to: /tmp/nuo/spool/e3b0c44298fc.log] ---
```

### 2. Ephemeral Session Execution Spool (`[INV-SPOOL-01]`, `[INV-SPOOL-02]`)

1. **Standard POSIX Pathing**:
   Spool directories reside under OS-standard temporary paths (`/tmp/nuo/spool/` or `/dev/shm/nuo/spool/` on Linux for zero-disk-write RAM caching, `%TEMP%\nuo\spool\` on Windows).
2. **Single Atomic Write**:
   Eliminate redundant triplication. Write once to the canonical spool file.
3. **Transparent POSIX Accessibility (`[INV-SPOOL-02]`)**:
   External shell tools (`grep`, `tail`, `jq`) and filesystem inspection tools (`read_text`) access spool files directly without pseudo-VFS resolver interception. `read_text` falls back to disk inspection if no in-memory resolver claims the path.

### 3. Salience-Aware Tail-Heavy Semantic Folding (`[INV-FOLD-01]`)

1. **Tail-Biased Character Distribution**:
   Replace 50/50 head/tail splitting with a 25/75 (or 20/80) distribution favoring the tail, where crash traces and exit summaries reside.
2. **Salience Pattern Extraction**:
   The middle omitted slice is scanned for error signatures (`error:`, `FAILED`, `panic:`, `exception:`, `fatal:`, `SyntaxError`). Up to 5 matching lines are surfaced directly inside the fold banner with relative line offsets.

### 4. Sovereign Role Autonomy (`[INV-ROLE-01]`)

The harness never instructs agents on what tool to run. An agent autonomously determines whether to:
- Act directly on the surfaced error signature;
- Re-run the command with narrowed flags (e.g. `cargo test -p subcrate`);
- Run `execute_command` with `grep/tail` (if permitted by its `ToolPolicy`);
- Call `read_text` with targeted line ranges.

---

## Invariants Established

- **`[INV-TRUNC-01] Zero Imperative Guidance`**: Tool result messages and offload invoices MUST NOT contain imperative commands, pseudo-guidance, or instructions to invoke specific tools (`Guidance: Use ...`). All truncation envelopes MUST be purely declarative, objective, structured fact statements.
- **`[INV-SPOOL-01] Ephemeral Session Spool`**: Tool outputs exceeding context budgets are spooled to single, OS-native temporary session files under `$TMPDIR/nuo/spool/{hash}.log`. Zero redundant disk triplication.
- **`[INV-SPOOL-02] Transparent POSIX Accessibility`**: Spool paths are standard POSIX paths directly readable by child processes (`grep`, `tail`, `jq`) and standard file inspection tools (`read_text`) without pseudo-VFS resolver interception.
- **`[INV-FOLD-01] Salience-Aware Semantic Folding`**: Output truncation uses tail-heavy biasing and automatically extracts key error and failure signatures from the omitted middle section into the fold banner.
- **`[INV-ROLE-01] Sovereign Role Autonomy`**: The harness never instructs an agent to call tools outside its permission policy.

---

## Consequences

- **Positive**:
  - Completely eliminates false positive prompt-injection detections by LLM safety filters.
  - Resolves 90%+ of failure diagnostic tasks in a single turn through error signature surfacing.
  - Eliminates the cognitive and technical contradiction of pretending physical files are in-memory VFS.
  - Fully cross-platform across Linux (RAM-backed `tmpfs`), macOS, and Windows.
- **Negative & Trade-offs**:
  - Legacy tests checking for `[Guidance:` or `/tmp/nuo/vfs/` require modernization to the spool envelope specification.

---

## Rejected Alternatives & Negative Knowledge

### 1. In-Process FUSE Driver
- *Why considered*: To provide true virtual memory files readable by external shell processes without writing to disk.
- *Why rejected*: Requires kernel modules, breaks in unprivileged Docker/Kubernetes containers, unsupported on Windows without third-party drivers.

### 2. Linux `memfd_create` Anonymous Descriptors
- *Why considered*: Zero disk writes on Linux.
- *Why rejected*: Unportable to Windows and macOS; child process `/proc` traversal is blocked by container seccomp profiles.

---

## Links

- Supersedes: [ADR-0070](0070-unified-cas-vfs-projection-and-epistemic-meta-tool-abolition.md)
- Amends: [ADR-0063](0063-bifurcated-shell-tool-presentation-out-of-band-status-and-clean-stream-topology.md)
