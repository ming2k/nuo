---
id: ADR-0063
title: "Bifurcated Shell Tool Presentation, Out-of-Band Status Segregation, and Clean Stream Topology"
status: accepted
date: 2026-11-04
scope: tool/nuo-tool, presentation/nuo-tui, protocol/nuo-wire, harness/agent
superseded_by: null
negative_knowledge: true
---

# 0063. Bifurcated Shell Tool Presentation, Out-of-Band Status Segregation, and Clean Stream Topology

- Status: Accepted
- Date: 2026-11-04
- Deciders: Nuo Architecture Working Group
- Consulted: Tool Infrastructure, TUI Presentation, Agent Runtime
- Informed: System Architects, Core Developers
- Amends: [ADR-0057](0057-tool-output-claim-check-mechanism-and-context-stub-topology.md)

---

## Context and Problem Statement

When command execution tools (`ExecuteCommandTool`) run shell commands (such as `cargo test -p nuo-tui`), the execution output was unified into a legacy monolithic string representation (`Exit {}\nSTDOUT:\n{}\nSTDERR:\n{}`) with prose notes appended for harness interruptions (`[killed by harness: wall-clock timeout reached ...]`).

This legacy layout suffered from four fundamental design defects:

1. **Cognitive Inversion via Premature Exit Status**:
   `Exit N` was printed at the very first line of the output block before any stdout or stderr was displayed. For human operators reading terminal transcript flows sequentially, displaying the future outcome ahead of chronological logs is jarring. When a command was terminated by harness timeout or signals, formatting `exit.unwrap_or(-1)` falsely reported `Exit -1`, misleading the operator into believing the child process itself crashed with code `-1`.

2. **Artificial Stream Segregation and Phantom Label Noise**:
   Splitting captured output into rigid `STDOUT:` and `STDERR:` sections broke the natural chronological interleaving of terminal output. For standard compilers like `cargo`, `rustc`, and `gcc`, progress reports (`Compiling ...`) and warnings are written to `stderr` by standard POSIX convention to avoid contaminating data pipelines on `stdout`. Displaying an empty `STDOUT:` section followed by normal compilation logs under a loud `STDERR:` header falsely alarmed users into believing normal progress was an execution error.

3. **Audience Mismatch and Verbose Harness Prose**:
   Explanatory recovery instructions appended to shell outputs (e.g., `[killed by harness: wall-clock timeout reached — the command was still producing output. Retry with a larger timeout, or split the work into smaller steps.]`) were authored exclusively for language model prompt conditioning. Presenting this verbose instructional prose directly on the human operator's TUI canvas cluttered the interface with machine-directed instructions.

4. **In-Band Signaling Vulnerability and String Sniffing**:
   Conflating system metadata (termination reason, exit code, truncation indicators) with the raw payload inside a single text string forced presentation layers and export parsers to rely on heuristic string sniffing (`starts_with("Exit ")`, `starts_with("[killed by harness")`). If a user command printed matching substrings, the parser misidentified user payload as system control directives.

---

## Decision Drivers

- **Bifurcated Presentation Topology**: Strictly decouple human presentation (TUI / CLI) from language model prompting (LLM Tool Result context). Human views require chronological purity and concise state indicators; model contexts require structured causal facts and actionable recovery hints.
- **Strict Out-of-Band Control Segregation**: Treat process output (in-band stream) and lifecycle termination (out-of-band metadata) as strictly isolated types. Never rely on string-sniffing heuristics.
- **Terminal-Native Interleaving**: Preserve authentic temporal order of `stdout` and `stderr` without artificial section headers or empty placeholder labels.
- **Dignified Industrial Restraint**: Prohibit frivolous visual emojis and pseudo-dialogues. Use clean ANSI color grading and terse, professional status tokens (`exit 0`, `exit 101`, `timed out after 60s`, `stream cap reached`).

---

## Considered Options

- **Option 1 (Rejected): Superficial Text Reformatting with In-Band Markers**
  Rearrange the monolithic string to put `Exit` at the bottom and suppress empty `STDOUT:`.
  *Failure Mode*: Retains in-band signaling vulnerability. Parsing remains brittle to user payloads containing format markers, and model instructions still leak into operator views.

- **Option 2 (Rejected): Emotive Emoji-Augmented Status Badges**
  Decorate tool step footers with status emojis (e.g. `⏱ Timed out`, `✕ exit 1`).
  *Failure Mode*: Degrades the professional aesthetic of system-level developer tooling, introduces alignment inconsistencies across varying terminal font glyph widths, and clutters high-density log views.

- **Option 3 (Accepted): Bifurcated Out-of-Band Presentation and Clean Stream Topology**
  Split execution state into a pure in-band stream (`lines: Vec<ShellLine>`) and out-of-band control (`exit: Option<i32>`, `termination: ShellTermination`). The TUI renders an authentic interleaved output body closed by an industrial, emoji-free status footer. Text fallbacks and model payloads cleanly isolate diagnostics without polluting the primary log stream.

---

## Decision Outcome

Adopt **Option 3**:

1. **Clean Stream Formatting (`nuo-tool`)**:
   - `shell_inner_text` and `shell_to_text` format authentic terminal output: chronological content first, followed strictly at the tail by status metadata when required.
   - Suppress empty `STDOUT:` or `STDERR:` labels entirely. If stdout or stderr is empty, omit its section header.
   - When a process is terminated by the harness without a valid exit code (`exit == None`), do not synthesize `Exit -1`. Render authentic lifecycle status (`timed out`, `stream cap reached`, `cancelled`).

2. **Concise, Professional Footers (`nuo-tui`)**:
   - Eliminate verbose multi-space placeholders and emojis.
   - Standardize termination footers to crisp, lower-case industrial diagnostics:
     - `ShellTermination::Timeout` $\rightarrow$ `timed out`
     - `ShellTermination::StreamGuard` $\rightarrow$ `stream cap reached`
     - `ShellTermination::Cancelled` $\rightarrow$ `cancelled`
     - `ShellTermination::IdleBlocked` $\rightarrow$ `idle timeout detected`
     - `ShellTermination::InteractiveBlocked` $\rightarrow$ `interactive prompt detected`
     - `ShellTermination::InputUnanswered` $\rightarrow$ `unanswered input prompt`
   - Non-zero process exit codes render as `exit <code>` in warning/error tone; clean exits (`exit 0`) render dimmed.

3. **Audience-Aware Model Note Isolation**:
   - Model-facing recovery hints (`termination_model_note`) remain available for agent prompt generation but are decoupled from human display rendering.

---

## Invariants

- `[INV-TOOL-08] In-Band Stream Purity`: The child process output stream (`stdout`, `stderr`, `lines`) MUST NEVER contain synthesized harness state banners or artificial exit markers.
- `[INV-TUI-12] Emoji-Free Footer`: Execution footers and diagnostic statuses in TUI components MUST NOT contain decorative emoji glyphs.
- `[INV-EXEC-03] Authentic Exit Code Semantics`: `exit: Option<i32>` MUST be `None` when a command is interrupted by a harness signal or timeout. Synthesizing `Exit -1` is strictly forbidden.
