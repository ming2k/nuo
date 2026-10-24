---
id: ADR-0099
title: "Decoupled Physical StreamGuard Flood Protection, Byte-Centric Safety Ceilings, and Cognitive Truncation Harmony"
status: accepted
date: 2026-11-28
scope: tool/nuo-tool-exec, tool/nuo-tool, harness/rounds, tui/disclosure
superseded_by: null
negative_knowledge: true
---

# 0099. Decoupled Physical StreamGuard Flood Protection, Byte-Centric Safety Ceilings, and Cognitive Truncation Harmony

- Status: Accepted
- Date: 2026-11-28
- Deciders: Nuo Architecture Working Group, Runtime Engineering
- Consulted: Tool Infrastructure (`nuo-tool`, `nuo-tool-exec`), Harness (`nuo-harness`), Presentation (`nuo-tui`)
- Amends: [ADR-0057](0057-tool-output-claim-check-mechanism-and-context-stub-topology.md) (specifically Section 3 StreamGuard Flood Protection)
- Complements: [ADR-0063](0063-bifurcated-shell-tool-presentation-out-of-band-status-and-clean-stream-topology.md), [ADR-0093](0093-abolition-of-pseudo-vfs-spool-architecture-and-salience-folding.md)

---

## Context and Problem Statement

In ADR-0057, Nuo introduced `StreamGuard` as an in-flight online streaming watchdog to prevent runaway continuous streaming processes (such as `tail -f`, `yes`, or `cat /dev/urandom`) from exhausting host memory. However, ADR-0057 symmetrically coupled the physical process termination limit to the cognitive token context budget:
```text
SHELL_STREAM_FLOOD_LINES = 200 lines (~16 KB)
SHELL_STREAM_FLOOD_BYTES = 16 KB (exactly 2× the 8 KB stub watermark)
```

In production and multi-test agent workflows, this design created a catastrophic architectural contradiction between upstream process execution and downstream cognitive truncation:

1. **Premature Process Termination ("False-Positive Murder")**:
   Modern engineering workloads (e.g. `cargo test -p nuo-tui --lib` with 1,500+ unit tests, `cargo clippy`, large `git diff`, `npm test`) legitimately produce 1,000–3,000 lines of standard output (~30 KB – 100 KB) in under 3 seconds. Under the 200-line / 16 KB threshold, StreamGuard intervened prematurely, sending `SIGKILL` to perfectly healthy processes and stamping `terminated — output limit reached` before tests could complete.
2. **Layering Violation (Physical Watchdog vs. Cognitive Window)**:
   The upstream watchdog conflated physical machine safety (RAM, CPU, disk exhaustion) with the LLM context window size. 16 KB of text consumes 0.0001% of host RAM. By setting the physical kill switch at 16 KB, the system prevented downstream ADR-0093 salience-aware folding and ephemeral session log spilling from ever executing on normal workloads.
3. **The Unbounded Single-Line Buffer Vulnerability**:
   `tokio::io::AsyncBufReadExt::read_until(b'\n', &mut buf)` buffered incoming bytes indefinitely until a newline character was encountered. In minified code (`bundle.min.js`), massive single-line JSON, or binary streams lacking newlines, the stream reader task accumulated gigabytes in memory without ever dispatching lines to the channel, completely evading line-based StreamGuard checks until an Out-Of-Memory (OOM) crash occurred.
4. **False Equivalence of Lines and Physical Load**:
   Lines are human and linguistic formatting conventions, not physical resource units. A single line can contain 50 MB (bypassing line bounds), whereas 2,000 lines of test output can contain only 40 KB (triggering line bounds).

A modern, future-facing architecture is required to decouple physical host protection from cognitive context management, establish byte-centric physical bounds, and harmonize upstream execution with downstream salience-aware truncation.

---

## Decision Drivers

- **Zero False-Positive Execution Murders (`[INV-STREAM-01]`)**: Finite, self-terminating engineering commands must never be killed simply because they emit structured multi-thousand-line logs.
- **Physical Safety Primacy (`[INV-STREAM-02]`)**: StreamGuard must protect the host against genuine runaway streams (`yes`, `cat /dev/urandom`, infinite loops) with strict, bounded physical memory ceilings (Bytes).
- **Bounded In-Flight Ingestion (`[INV-STREAM-03]`)**: Stream readers must chunk unbounded single-line payloads at 64 KB boundaries to prevent in-flight buffer inflation prior to channel dispatch.
- **Cognitive & Physical Decoupling**: Upstream StreamGuard guards host resources (RAM/CPU) at the megabyte level; downstream ADR-0093 guards model attention (tokens) at the kilobyte level.

---

## Architectural Decisions

```text
[ Process Subshell stdout/stderr ]
               │
               ▼
┌─────────────────────────────────────────────────────────────┐
│ 1. Bounded In-Flight Ingestion (AsyncBufRead Chunking)      │
│    • Emits on '\n' boundary OR at MAX_LINE_CHUNK_BYTES (64 KB)│
│    • Zero unbounded buffering on minified / non-newline data│
└──────────────────────────────┬──────────────────────────────┘
                               │
                               ▼  (mpsc channel)
┌─────────────────────────────────────────────────────────────┐
│ 2. Upstream Watchdog: StreamGuard Physical Flood Protection │
│    • Default Bounds: 2 MB (2,097,152 B) / 20,000 lines      │
│    • Raw Mode Bounds: 10 MB (10,485,760 B) / 100,000 lines   │
│    • Primary metric: Bytes (physical memory truth)          │
│    • Secondary metric: Lines (prevents micro-allocation DOS)│
│    • Action on Breach: SIGKILL + ShellTermination::StreamGuard│
└──────────────────────────────┬──────────────────────────────┘
                               │
               (Within 2 MB physical safety bound)
                               │
                               ▼
┌─────────────────────────────────────────────────────────────┐
│ 3. Downstream Protection: ADR-0093 Cognitive Truncation     │
│    • Threshold: SHELL_MAX_OUTPUT_CHARS (8,000 characters)   │
│    • Action: Tail-heavy salience folding (25% head, 75% tail)│
│    • Raw Log Spilling: Ephemeral session spool (/tmp/nuo/...)│
│    • Model Context: Completely shielded from token blowup   │
└─────────────────────────────────────────────────────────────┘
```

### 1. Decoupled Byte-Centric Safety Ceilings (`[INV-STREAM-01]`)

The constants in `nuo-tool-exec` are recalibrated to reflect physical host safety margins rather than cognitive token limits:

```rust
/// Maximum lines a foreground command may produce before StreamGuard terminates it early.
/// Acts strictly as a secondary ceiling against micro-allocation heap fragmentation.
pub const SHELL_STREAM_FLOOD_LINES: usize = 20_000;

/// Maximum bytes a foreground command may produce before StreamGuard terminates it early.
/// Primary physical memory and runaway flood watchdog (2 MB).
pub const SHELL_STREAM_FLOOD_BYTES: usize = 2 * 1024 * 1024;

/// Relaxed bounds when raw mode is explicitly requested.
pub const RAW_STREAM_FLOOD_LINES: usize = 100_000;
pub const RAW_STREAM_FLOOD_BYTES: usize = 10 * 1024 * 1024; // 10 MB
```

- **Runaway Detection Uncompromised**: Commands such as `yes` or `cat /dev/urandom` generate >2 MB in under 20 milliseconds, triggering `child.kill()` instantly.
- **Normal Bursts Unhindered**: A 1,500-test `cargo test` run (~50 KB) or a large compiler diagnostic run completes unimpeded and exits with clean status codes.

### 2. Bounded Reader Stream Ingestion (`[INV-STREAM-02]`)

Reader tasks replace naive `read_until(b'\n', &mut buf)` with bounded buffer consumption:

```rust
pub const MAX_LINE_CHUNK_BYTES: usize = 64 * 1024; // 64 KB
```

- If `\n` is encountered, the line is emitted trimmed.
- If `buf.len() >= MAX_LINE_CHUNK_BYTES` without a newline, the chunk is emitted immediately to `tx`.
- This guarantees memory inside reader buffers never exceeds 64 KB, and byte accumulators in the supervisor loop promptly detect floods regardless of newline presence.

### 3. Harmonious Cognitive Handoff (`[INV-STREAM-03]`)

When a finite process emits between 8 KB (`SHELL_MAX_OUTPUT_CHARS`) and 2 MB (`SHELL_STREAM_FLOOD_BYTES`):
1. Upstream StreamGuard observes that physical limits are respected and lets the process terminate naturally with `ShellTermination::Exited` and its authentic exit code (`Some(0)`).
2. The executor flags `truncated = true` and attaches `truncated_note`.
3. Downstream `shell_body_to_text` (ADR-0093) processes the payload, preserving setup commands and exit summaries via 25/75 salience-aware folding, while spilling full logs to the session spool.
4. The model receives a compact, high-signal observation with zero risk of context window exhaustion.

---

## Invariants & Behavioral Boundaries

- **`[INV-STREAM-01] Byte-Centric Physical Guarding`**: StreamGuard enforces process termination based primarily on physical byte thresholds (`2 MB` default, `10 MB` raw), with line count (`20,000` / `100,000`) serving strictly as a secondary ceiling against micro-allocation heap fragmentation.
- **`[INV-STREAM-02] Bounded Reader Stream Ingestion`**: In-flight stream readers must bound single-line accumulations to 64 KB chunks (`MAX_LINE_CHUNK_BYTES = 64 KB`), ensuring un-delimited streams or huge single-line outputs cannot bypass StreamGuard or cause unbounded reader-side buffer inflation before dispatch.
- **`[INV-STREAM-03] Seamless Cognitive Handoff`**: Finite processes producing outputs within physical safety bounds (>8 KB and <2 MB) run to completion without early termination, transitioning seamlessly to ADR-0093 salience-aware semantic folding and ephemeral session log spilling.

---

## Negative Knowledge & Discarded Alternatives

### 1. Completely Abolishing StreamGuard (Pure Downstream Truncation)
- **Why considered**: If downstream already folds text and spills to disk, why terminate the process at all?
- **Why rejected**: A runaway loop (`yes`, `cat /dev/urandom`, infinite `while true; do echo; done`) generates gigabytes per minute. Without an upstream process kill switch, the host exhausts physical RAM (triggering the OS OOM killer on `nuo`), fills temporary filesystems, or locks build directories for the entire 30-minute default timeout.

### 2. Time-Rate Throttling (e.g. Tokens Per Second)
- **Why considered**: Attempting to measure stream velocity rather than volume.
- **Why rejected**: Adds complex timer state machines, high jitter under CPU scheduling pressure, and unrepeatable flakiness across varied hardware (CI runners vs high-end workstations). Byte and line volume bounds provide deterministic physical guarantees with zero timer overhead.
