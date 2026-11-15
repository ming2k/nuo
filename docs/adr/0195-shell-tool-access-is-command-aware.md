---
id: ADR-0195
title: "Shell Tool Access Is Command-Aware"
status: accepted
date: 2026-11-15
scope: tools/exec, engine/dispatch, concurrency
superseded_by: null
supersedes: []
negative_knowledge: true
---

# 0195. Shell Tool Access Is Command-Aware

- Status: Accepted
- Date: 2026-11-15

## Context and Problem Statement

The tool scheduler arbitrates a batch from each call's declarative
`ToolAccesses`. `execute_command` did not override the default mapping for a
`ScopeTarget::Command`, so every shell command declared `ToolAccesses::all()`
and serialized the entire batch behind it. A model that ran `rg foo` and read
three files in one turn paid for four sequential calls even though the command
and the reads were independent. The conservative declaration is correct for an
opaque command, but it is not correct for the common inspection commands.

## Architectural Decisions

### 1. A simple read-only command declares a recursive read

`command_accesses` recognizes a deliberately small set of inspection commands
(`ls`, `cat`, `head`, `tail`, `wc`, `rg`, `grep`, `find`, `git status`,
`git diff`, `git log`, `cargo metadata`, version probes, …) and returns
`ToolAccesses::read_tree(".")`. Two such commands parallelize; they conflict
with a writer anywhere in the workspace.

### 2. Anything compound or unknown stays global

The classifier refuses to guess when the command contains a shell control
operator (`;`, `&`, `|`, `<`, `>`, backticks, newlines) or command substitution
`$(`, or when the executable/subcommand is not on the allowlist. Those calls
keep `ToolAccesses::all()`. A false negative costs parallelism; a false
positive would let a writer race a reader, so the safe direction is fixed.

### 3. The classification is a pure function

`command_accesses(command: &str) -> ToolAccesses` lives beside the tool and is
unit-tested without spawning a process. The tool's `accesses` implementation
parses the command argument and delegates to it; malformed JSON keeps the
global claim.

### 4. The default stays conservative

`Tool::accesses` still maps an unspecified command target to
`ToolAccesses::all()`. Only the concrete shell capability opts into the
command-aware refinement. New execution tools must make the same explicit
choice.

## Consequences

- **Positive**: independent reads and inspection commands run concurrently;
  a batch no longer stalls behind a harmless `git status`.
- **Negative & Incurred Costs**: the allowlist needs maintenance when a new
  read-only tool becomes common. An allowlisted command that mutates state
  through a side effect would be a bug; the list is intentionally narrow.
- **Neutral**: output volume and timeouts are unchanged. This decision is
  about scheduling conflicts, not about permission or confinement.

## Discarded Alternatives (Negative Knowledge)

- **Parse the shell grammar and derive paths**: high effort and still unable
  to see what a child process touches. The narrow allowlist is honest.
- **Parallelize every command**: races writers against readers.
- **Keep everything serial**: leaves a measured latency regression in place.

## Links

- [ADR-0093](0093-abolition-of-pseudo-vfs-spool-architecture-and-salience-folding.md)
- [ADR-0183](0183-composition-root-owns-capability-linkage.md)
