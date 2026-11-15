---
id: ADR-0202
title: "Filesystem Tools Search Off-Thread and Write Atomically"
status: accepted
date: 2026-11-15
scope: tools/filesystem, performance, durability
superseded_by: null
supersedes: []
negative_knowledge: true
---

# 0202. Filesystem Tools Search Off-Thread and Write Atomically

- Status: Accepted
- Date: 2026-11-15

## Context and Problem Statement

`search_text` ran a synchronous `read_to_string` + line scan inside an async
`execute`. On a large workspace that blocks a Tokio worker for the duration of
the scan, and the tool had no file filters, case folding, or context lines.
`write_file` and `edit_text` called `fs::write`, which truncates the target
before the new bytes are durable: a crash or full disk can leave a partial
file. Both are tool-boundary defects, not model behavior.

## Architectural Decisions

### 1. The scan runs on the blocking pool

`SearchTextTool::execute` validates arguments, resolves the root, then hands
the walk to `spawn_blocking`. A panicking or slow scan cannot stall the async
runtime. The worker returns a `Result<Vec<String>, String>`; the tool maps it
onto the normal typed error.

### 2. Search is a real query surface

`SearchTextArgs` gains `include` (glob filters), `case_insensitive`, and
`context` (0–5 lines). Literal queries are regex-escaped; regex queries are
compiled once. Context rows use `path-line- text` while match rows keep
`path:line: text`, and overlapping context is emitted once. The match limit
counts hits, not context rows.

### 3. Writes are same-directory temp + rename

`atomic_write` creates a hidden temp file in the target directory, writes and
`sync_all`s it, preserves the existing file's permissions on Unix, then
renames over the target. On Windows, where rename cannot replace an existing
file, the target is removed only after the complete temp file exists. A failed
write removes the temp file; the original is never truncated.

### 4. The tool contract is unchanged

`atomic_write` is a helper, not a new tool. `write_file` and `edit_text` keep
their names, schemas, previews, and risk profiles. The durability change is
invisible to the model.

## Consequences

- **Positive**: a large search no longer blocks the runtime; the model can
  filter and read context; a crash cannot leave a half-written source file.
- **Negative & Incurred Costs**: the search now pays one blocking-pool
  handoff and a temp-file create/rename per write (a few hundred microseconds
  on a local filesystem). The temp file is briefly visible to a directory
  watcher; it is hidden and removed on failure.
- **Neutral**: `.gitignore`/ignore rules and the existing directory pruning
  still own traversal.

## Discarded Alternatives (Negative Knowledge)

- **Keep the synchronous scan and add `spawn_blocking` only around the whole
  tool**: the schema/validation path is cheap; wrapping it too adds no value.
- **Link a full ripgrep binary**: a subprocess adds a platform dependency and
  a second error surface. The `ignore` walker + blocking scan is enough for
  the bounded result set.
- **Write to a temp directory and copy**: a cross-filesystem copy is not
  atomic and doubles the I/O. Same-directory rename is the atomic primitive.

## Links

- [ADR-0177](0177-edit-matching-is-exact-first-with-a-unique-line-window-normalization.md)
- [ADR-0196](0196-model-paths-are-confined-at-the-engine-gate.md)
