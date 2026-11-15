---
id: ADR-0203
title: "Nested Project Rules Load on First Read"
status: accepted
date: 2026-11-15
scope: engine/tools, project-instructions, prompt-cache
superseded_by: null
supersedes: []
negative_knowledge: true
---

# 0203. Nested Project Rules Load on First Read

- Status: Accepted
- Date: 2026-11-15
- Amends: [ADR-0199](0199-the-default-agent-has-a-coding-baseline-and-skill-catalog.md)

## Context and Problem Statement

The root `AGENTS.md` is part of the fixed project-rules section, but a
monorepo can place additional `AGENTS.md` files at subtree roots. Loading every
one at startup costs tokens for subtrees the session never enters and makes the
fixed prompt change whenever any file is edited. Ignoring them means a session
that reads `crates/foo/` never sees the conventions that directory owns.

## Architectural Decisions

### 1. Discovery is a read side effect, not a startup scan

After a successful `read`, `Agent::inject_nested_project_rules` walks from the
read path's directory up to (but excluding) the workspace root, loading each
`AGENTS.md` it finds. The root file is excluded because it is already in the
fixed project-rules section.

### 2. Each path is injected at most once per session

`ContextState::loaded_project_rules` is a session-scoped `HashSet<PathBuf>`.
The first read below a subtree inserts the path and emits one hidden user
message; later reads do not repeat the prompt cost. A failed read (or a
virtual path) never triggers discovery.

### 3. The injection is bounded and provenance-bearing

A nested file is capped at 64 KiB at a char boundary; the message uses
`InjectionKind::ProjectRulesDelta` with the workspace-relative path as its
reason. The wrapper tells the model the rules apply to files under that
directory for the rest of the session. At most eight ancestors are injected in
one read.

### 4. The root rules remain fixed

`[project.rules]` stays the cacheable root composition. Nested files are
transient history entries: they follow the read that needed them, survive a
session resume as ordinary injected messages, and are not re-read on every
request.

## Consequences

- **Positive**: subtree conventions reach the model exactly when relevant;
  starting a session in a large monorepo does not pay for every rule file;
  the root prompt prefix stays stable.
- **Negative & Incurred Costs**: an injected rule file consumes context from
  the read onward. A later edit to that file is not observed until a new
  session (the set is per-session). The 64 KiB/8-ancestor caps bound the cost.
- **Neutral**: the `.nuo/instructions` scoped loader is unaffected; nested
  `AGENTS.md` is a separate rule source.

## Discarded Alternatives (Negative Knowledge)

- **Scan the whole tree at startup**: token cost and staleness scale with
  repository size.
- **Inject the root file's parent chain**: the workspace root is the trust
  boundary; parent rules are not part of the workspace.
- **Use a tool call to fetch rules**: the model does not know the file exists
  and would have to guess. The read path already knows.

## Links

- [ADR-0164](0164-instructions-are-one-pipeline-over-system-and-project-sources.md)
- [ADR-0199](0199-the-default-agent-has-a-coding-baseline-and-skill-catalog.md)
