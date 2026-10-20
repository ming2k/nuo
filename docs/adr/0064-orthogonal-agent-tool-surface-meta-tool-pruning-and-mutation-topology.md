---
id: ADR-0064
title: "Orthogonal Agent Tool Surface, Meta-Tool Pruning, and High-Fidelity Mutation Topology"
status: accepted
date: 2026-11-04
scope: tool/nuo-tool, protocol/nuo-wire, tools/nuo-tool-fs, harness/agent
superseded_by: null
negative_knowledge: true
---

# 0064. Orthogonal Agent Tool Surface, Meta-Tool Pruning, and High-Fidelity Mutation Topology

- Status: Accepted
- Date: 2026-11-04
- Deciders: Nuo Architecture Working Group
- Consulted: Agent Engineering, Tool Infrastructure, Cognitive Runtime
- Informed: Core Developers, AI Systems Integrators
- Amends: [ADR-0001](0001-flat-workspace-and-microkernel-capability-topology.md), [ADR-0007](0007-tool-ownership-and-plugin-boundaries.md), [ADR-0010](0010-harness-decomposition-and-agent-unification.md), [ADR-0013](0013-decoupled-tool-namespace-and-infrastructure-purity.md)

---

## Context and Problem Statement

As Nuo's agent runtime scales to handle larger codebases and multi-phase tasks, the cognitive efficiency of tool interactions dictates turn latency, token consumption, and model success rates. Historically, the agent capability surface inherited a legacy collection of meta-tools (`ask_user`, `todo`) and granular inspection tools (`list_dir`), alongside ambiguous file mutation boundaries.

In production execution traces, this legacy topology introduced four critical systemic failure modes:

1. **Stepwise Directory Wandering (`list_dir`)**:
   When exploring an unfamiliar repository, models equipped with `list_dir` fell into repetitive, single-level browsing loops (`list_dir(".")` $\rightarrow$ `list_dir("src")` $\rightarrow$ `list_dir("src/components")`). Each directory inspection consumes a full LLM turn and floods the context window with raw filenames, delaying actual problem-solving. This tool is structurally redundant because pattern globbing (`find_files`) and lexical indexing (`search_text`) form a strict superset of single-directory listing.

2. **Meta-Work Overhead and Turn Inflation (`todo`)**:
   The `todo` tool created an artificial state-machine tax: for simple 2-turn modifications, agents generated 4 to 6 tool invocations updating task statuses (`pending` $\rightarrow$ `in_progress` $\rightarrow$ `completed`). Modern language models maintain internal planning state in chain-of-thought scratchpads far more effectively than through round-trip external tool invocations.

3. **Autonomous Agency Abdication and Fragility (`ask_user`)**:
   Exposing a structured, blocking `ask_user` tool incentivized models to surrender investigative initiative on minor ambiguities (over-asking), pushing exploration burden back onto the human operator. Furthermore, blocking on structured tool requests stalled automated pipelines. Natural conversation already provides an unconstrained channel for human collaboration: an agent needing clarification can reply in natural prose and conclude its turn.

4. **Accidental Code Truncation and Weak Mutation Boundaries (`write_file` vs `edit_text`)**:
   `write_file` lacked defense-in-depth routing warnings. Faced with modifying a 500-line source file, models frequently invoked `write_file` with placeholder comments (e.g. `// ... existing code ...`), permanently erasing user implementations. Conversely, `edit_text` lacked explicit prompt guidance on multi-line context anchors, causing single-line replacements to fail ambiguity checks.

---

## Decision Drivers

- **Maximum Tool Orthogonality**: Eliminate functional overlap across tools to lower model decision entropy. Every tool in an agent's capability set must solve a unique cognitive problem.
- **Token Frugality & Zero Nanny Prompting**: Do not lecture the model with verbose system prompts. Shape behavior through minimal, hardened tool schemas and physical surface pruning.
- **Surgical Code Mutation**: Guarantee code integrity by physically isolating file creation (`write_file`) from surgical text patching (`edit_text`).
- **High-Velocity Exploration**: Force pattern-based and content-based workspace discovery (`find_files`, `search_text`) over myopic directory stepping.
- **Clean Dialogue Semantics**: Let natural language carry human interactions rather than synthetic tool-call rituals.

---

## Architectural Decision

1. **Prune `list_dir` from Agent Role Tool Surfaces**:
   - Remove `list_dir` from `DEVELOPER_TOOLS`, `OPS_TOOLS`, `READ_ONLY_TOOLS`, `SKILL_TOOLS`, and `DEBUG_TOOLS`.
   - Remove `ListDirTool` from the default suite created by `nuo_tool_fs::create_fs_tools`.
   - Agents navigate workspace paths exclusively via `find_files` (glob matching, including directory-scoped globs like `src/*`) and `search_text` (ripgrep/regex).

2. **Prune `todo` and `ask_user` from Primary Agent Capabilities**:
   - Remove `todo` and `ask_user` from `DEVELOPER_TOOLS`, `OPS_TOOLS`, and `PHILOSOPHIST_TOOLS`.
   - Task planning is conducted natively within model reasoning chains and summarized in textual turn responses.
   - Human clarification and critical confirmations occur directly via conversational text turns rather than blocking tool-call suspensions.

3. **Enforce Strict Mutation Bifurcation with Hardened Schema Descriptions**:
   - Retain `edit_text` and `write_file` as separate, orthogonal tools; strictly reject merging them into a monolithic schema.
   - **`edit_text` Schema Hardening**: Explicitly describe exact string replacement in existing files, mandate multi-line context anchors to ensure uniqueness, and explicitly forbid using it for file creation.
   - **`write_file` Schema Hardening**: Restrict its usage to new file creation or full rewrites, explicitly warning against modifying existing files where `edit_text` is required, and prohibiting truncation placeholders (`// ... existing code`).

---

## Invariants Established

- **`[INV-TOOL-09]` Workspace Discovery Orthogonality**:
  Agent role profiles MUST NOT admit single-directory traversal tools (`list_dir`). Workspace discovery MUST be conducted via glob pattern resolution (`find_files`) or content indexing (`search_text`).
- **`[INV-TOOL-10]` Surgical Mutation Preservation**:
  Modifications to existing workspace files MUST utilize `edit_text` with context anchor lines. Full-file overwriting via `write_file` is strictly prohibited for localized edits.
- **`[INV-TOOL-11]` Unobstructed Communication Channel**:
  User interaction MUST NOT depend on synchronous blocking meta-tools. Inquiries and confirmations are expressed through natural language responses.

---

## Rejected Alternatives & Negative Knowledge

### 1. Merging `edit_text` and `write_file` into a Single Polymorphic Tool
- **Concept**: Provide a single `modify_file` tool with optional arguments or a `mode` field (`overwrite` vs `replace`).
- **Failure Mode**: Multi-mode schemas with mutually exclusive parameters (`content` vs `old_string`/`new_string`) significantly increase JSON validation errors in language models. More critically, allowing full content overwrites in a general editing tool causes models to frequently rewrite entire files instead of localized patching, triggering catastrophic code loss via placeholder omissions (`/* rest of code unchanged */`).

### 2. Retaining `list_dir` While Instructing Models to Prefer `find_files`
- **Concept**: Keep `list_dir` in the role manifest but add system prompt instructions asking agents to prefer `find_files`.
- **Failure Mode**: Violates (Mechanisms over Exhortation). Models routinely ignore soft prompt advisories when simple tools like `list_dir` are available, lapsing back into directory wandering loops. Physical omission from the tool surface is the only fail-safe guarantee.

### 3. Preserving `todo` Tool as an Optional Meta-Tool
- **Concept**: Retain `todo` for agents to record checkpoints on complex multi-step workflows.
- **Failure Mode**: Generates severe meta-work friction. In benchmarks, agents spent up to 30% of their turn budget synchronizing external task lists rather than progressing the underlying software task. Modern reasoning models perform higher-fidelity task tracking internally without tool round-trips.
