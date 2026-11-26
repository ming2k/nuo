---
id: ADR-0196
title: "Model Paths Are Confined at the Engine Gate"
status: accepted
date: 2026-11-15
scope: engine/tools, security, workspace
superseded_by: null
supersedes: []
negative_knowledge: true
---

# 0196. Model Paths Are Confined at the Engine Gate

- Status: Accepted
- Date: 2026-11-15
- Amends: [ADR-0125](0125-workspace-effect-journal-reversibility-as-a-declared-property.md)
- Amended: [ADR-0257](0257-read-boundary-hardening-virtual-path-confinement-and-modular-dispatch.md)
  (hardens `is_virtual_path` against `..` traversal and confines spool fallback)

## Context and Problem Statement

The workspace execution environment has a correct confinement policy
(`workspace_root`, additional roots, platform temp roots, skills roots,
lexical normalization, and a canonical symlink check). The filesystem tools
never used it. They resolved a relative path against the workspace root and
returned an absolute path unchanged, so `read /etc/passwd` was admitted
without a prompt. `WorkspaceJailMiddleware` existed but no execution pipeline
ran it. Confinement was a policy object with no enforcement point.

The engine gate is the only place that sees every tool call, knows the live
confinement flag (which `/unconfined` can change), and can attach the denial
to the model's observation stream. Enforcing there means a new path-bearing
tool cannot silently opt out by forgetting to call a helper.

## Architectural Decisions

### 1. The engine owns one path-boundary gate

`Agent::check_model_path_boundary` runs before the permission chain for the
path-bearing built-ins (`read`, `write_file`, `edit_text`, `find_files`,
`search_text`). It parses the `path` argument, skips virtual paths
(`skill://`, `offstream://`) that their own resolver owns, and calls
`admit_model_path`.

### 2. Admission is lexical first, canonical second

`admit_model_path` resolves relative paths against the primary root,
lexically normalizes `.`/`..`, and requires the result to sit under the
primary root, an additional root, a temp root, or a skills root. It then
canonicalizes the longest existing prefix and repeats the containment check.
The canonical pass closes the symlink escape that a lexical check cannot see;
the lexical pass still works for paths that do not exist yet (a write).

### 3. A boundary violation is an error observation, not a permission prompt

An out-of-root path returns a `ToolOutput::Error` that names the tool and
explains how to use an admitted root. It never parks a human; there is nothing
to approve outside the session's declared boundary. The model gets the
observation and the next turn, exactly like any other tool error.

### 4. `--unconfined` is the only bypass

The gate consults `Agent::is_confined()`. A session that explicitly started
unconfined keeps its host-wide authority. There is no per-call "allow once"
that silently widens the boundary.

### 5. `code_query` stays on its FsProvider

`code_query` already resolves through the confined `FsProvider`; a second
lexical gate could disagree with canonicalization. It is deliberately not in
`PATH_BOUND_TOOLS`.

## Consequences

- **Positive**: a model cannot read or write outside the admitted roots by
  supplying an absolute path or `..`. The check is centralized, testable, and
  runs before any tool body.
- **Negative & Incurred Costs**: a legitimate absolute path outside the roots
  now requires `[workspace].additional_roots` or an unconfined session. The
  canonicalization pass costs one or two `stat`/`readlink` calls per
  path-bearing tool call.
- **Neutral**: temp and skills roots remain admitted, so scratch files and
  skill assets keep working.

## Discarded Alternatives (Negative Knowledge)

- **Wire `WorkspaceJailMiddleware` around every tool**: the middleware trait
  has no executor and its path list is a second, smaller copy of the tool
  inventory. It would drift.
- **Make each tool canonicalize its own paths**: five implementations, five
  chances to forget. The engine gate is one.
- **Prompt the user for outside paths**: a permission prompt turns a policy
  boundary into a social-engineering surface. Additional roots are explicit
  configuration.

## Links

- [ADR-0125](0125-workspace-effect-journal-reversibility-as-a-declared-property.md)
- [ADR-0183](0183-composition-root-owns-capability-linkage.md)
- [ADR-0195](0195-shell-tool-access-is-command-aware.md)
