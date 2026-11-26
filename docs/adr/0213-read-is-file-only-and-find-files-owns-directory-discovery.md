---
id: ADR-0213
title: "Read Is File-Only and FindFiles Owns Directory Discovery"
status: accepted
date: 2026-11-18
scope: tools/filesystem, protocol/tool-output, cognition/context-budget
superseded_by: null
supersedes:
  - the `read` directory-listing branch of ADR-0176
negative_knowledge: true
---

# 0213. Read Is File-Only and FindFiles Owns Directory Discovery

- **Status**: Accepted
- **Date**: 2026-11-18
- **Amends**: [ADR-0176](0176-read-is-one-capability-with-bounded-content-dispatch.md)
  (the directory branch of `read` is removed) and
  [ADR-0193](0193-pdf-reads-are-bounded-text-extraction-through-a-host-document-seam.md)
  (PDF extraction gains an explicit page range)
- **Complements**: [ADR-0064](0064-orthogonal-agent-tool-surface-meta-tool-pruning-and-mutation-topology.md),
  [ADR-0202](0202-filesystem-tools-search-off-thread-and-write-atomically.md),
  [ADR-0209](0209-read-pages-run-off-thread.md)
- **Supersedes**: `read` listing one directory level; `find_files` as recursive-only
- **Amended**: [ADR-0257](0257-read-boundary-hardening-virtual-path-confinement-and-modular-dispatch.md)
  (strictly rejects `pages` on non-PDF targets and cleans stale wording)
- **Deciders**: Nuo Architecture Working Group

---

## Context and Problem Statement

ADR-0176 merged text, image, and directory reads into one `read` capability and
gave the directory branch a one-level listing. Two listing implementations then
existed over the same tree:

1. `read` on a directory performed its own `fs::read_dir`, its own sort, its own
   100k-entry cap, and its own paging coordinates.
2. `find_files` performed a recursive `ignore::WalkBuilder` traversal with
   project ignore rules, glob matching, and a different cap.

The duplication leaked into the model-facing surface. A model that wanted
"what is in this directory?" could call `read(".")` and get a flat listing that
honored no ignore rules, or call `find_files` and get a different framing,
different ordering, and different truncation semantics. The listing branch also
made `read`'s argument contract ambiguous: `offset` and `limit` meant lines for
a file but entries for a directory, and a negative offset was valid for one and
rejected for the other. Every new discovery feature (depth, entry kind, ignore
visibility, exclusion) had to be implemented twice or silently diverged.

The cost was paid on the exploration path — the phase where an agent is most
sensitive to turn count — and it contradicted ADR-0064's premise that
pattern-based discovery is the orthogonal way to navigate a workspace.

## Decision

### 1. `read` accepts files, not directories

- `read.path` names a file. Text, images, and PDFs keep their bounded,
  kind-dispatched behavior from ADR-0176/0193.
- A directory path is a typed `ToolError` that names `find_files` (and
  `max_depth=1` for a shallow listing) instead of returning an inline listing.
- `ReadArgs` drops directory semantics: `offset` is 1-based lines, `limit` is
  lines, and `pages` is a PDF-only page selector.

### 2. `find_files` owns all directory discovery

- One `ignore::WalkBuilder` + `OverrideBuilder` engine serves shallow listing
  (`max_depth=1`, the former `ls` case), tree preview (`max_depth=2`), and
  recursive glob discovery.
- `FindFilesArgs` carries the complete discovery vocabulary: `pattern(s)`,
  `exclude`, `max_depth`, `kind` (`files`/`dirs`/`all`), `include_ignored`, and
  `sort` (`mtime`/`path`).
- `max_depth=1` without a pattern defaults to `kind=all` and `sort=path`, so a
  bare "list this directory" call is deterministic and ordered.
- The walk runs on the blocking pool with cancellation checks, a scan ceiling,
  and a result cap; truncation is explicit in the output.
- `ToolOutput::Listing` becomes a discovery result (`entries`, `total`,
  `truncated`, `hint`) rather than a paginated directory page. Directory
  paging coordinates (`offset`/`next`) are removed.

### 3. PDF page ranges are explicit and bounded

- `read.pages` accepts `N`, `N-M`, `N-`, and `-M` (1-based, inclusive).
- Extraction still honors the ADR-0193 byte, page, wall-clock, and text
  ceilings; a range only narrows the inspected set.
- A start page beyond the document is `DocumentError::PageOutOfRange`; a
  truncated result reports the next unread page so continuation is explicit.

## Consequences

- **Positive**: one discovery engine, one ordering, one ignore policy, one
  truncation contract. `read`'s arguments mean the same thing for every file
  kind. Directory exploration names its own capability, so prompts and
  permissions can describe it precisely.
- **Negative**: callers that relied on `read(".")` must switch to
  `find_files(max_depth=1)`. This is a clean break; no compatibility branch is
  kept.
- **Neutral**: image/PDF/text reads are unchanged in shape; the listing output
  envelope changes from paging coordinates to truncation/hint fields.

## Invariants

- `[INV-READ-FILE-01]` `read` never enumerates a directory.
- `[INV-FIND-01]` every directory listing and recursive discovery traverses
  the same ignore-aware walker; there is no second listing implementation.
- `[INV-FIND-02]` truncation is always observable (`truncated`/`hint`); a
  partial listing never looks complete.

## Alternatives Considered

- **Keep both listing paths and align their output**: rejected. Alignment is a
  permanent maintenance tax and still leaves two argument contracts on one
  tool.
- **Make `read` recursive when given a directory**: rejected. It would turn a
  bounded content read into an unbounded traversal and erase the distinction
  between reading content and discovering paths.
- **Add a `list_dir` tool again**: rejected by ADR-0064; a shallow
  `find_files` already covers it and does not reintroduce per-directory
  stepping.

## Negative Knowledge

- Do not add directory enumeration back to `read` for "convenience"; the two
  contracts diverge the moment either side gains a filter.
- Do not give `find_files` a separate shallow walker for `max_depth=1`; depth is
  a parameter of the one engine, not a second implementation.
- Do not make `read.pages` silently clamp an out-of-range start; report it as a
  typed failure so the model can correct the request.
