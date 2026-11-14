---
id: ADR-0177
title: "Edit Matching Is Exact First, Then a Unique Line-Window Normalization"
status: accepted
date: 2026-11-13
scope: tools/filesystem, cognition/tool-loop, protocol/tool-arguments
superseded_by: null
negative_knowledge: true
---

# 0177. Edit Matching Is Exact First, Then a Unique Line-Window Normalization

- Status: Accepted
- Date: 2026-11-13
- Deciders: Nuo Architecture Working Group
- Consulted: Filesystem Tools, Cognitive Runtime, Diff/Preview, Security
- Supersedes: the exact-byte-only `edit_text` matcher
- Complements: [ADR-0176](0176-read-is-one-capability-with-bounded-content-dispatch.md),
  [ADR-0125](0125-tool-effects-are-reversible-or-explicitly-opaque.md)

---

## Context and Problem Statement

`edit_text` required `old_string` to occur exactly once, byte for byte. That is
a good safety property and a poor model interface. The common failures were
mechanical, not semantic:

- the file uses CRLF and the model sent LF;
- a line has trailing spaces the model omitted;
- prose or comments contain typographic quotes/dashes and the model normalized
  them;
- the model copied a line from a rendered read and kept an invisible
  difference.

Each failure cost a model round trip, and the retry often failed for the same
reason. This is one of the largest avoidable turn taxes in a coding session,
and it is invisible in a tool list because it is a *matching* defect, not a
missing capability.

OpenCode's edit matcher already retries with line-ending, whitespace, and
typographic normalization. Nuo should adopt the same class of fallback, but
with a stricter rule: a normalized match may only replace whole lines it can
prove, and it must remain unique. A fuzzy match that rewrites unrelated bytes is
worse than a clean failure.

## Decision

### 1. Exact match is the first and preferred path

The matcher searches for the exact `old_string` first. If it occurs exactly
once, replace it. If it occurs multiple times and `replace_all` is false,
reject with the matching line numbers. If `replace_all` is true, replace every
exact occurrence. The same non-overlapping, left-to-right rule applies when the
fallback selects normalized windows.

`new_string` must differ from `old_string`; a no-op edit is rejected rather
than reported as a successful mutation.

### 2. A unique line-window fallback, never a fuzzy rewrite

If no exact occurrence exists, the matcher splits the file into lines while
preserving each line's ending. It compares the normalized text of consecutive
lines against the normalized `old_string` lines:

- trailing spaces/tabs are ignored;
- typographic single quotes, double quotes, dashes, and non-breaking/thin
  spaces are folded to their ASCII counterparts;
- leading indentation is *not* ignored (indentation is semantic in code);
- line endings are not part of the comparison.

A window is a candidate only if every line matches. The fallback is applied
only when the window is unique, or when `replace_all` is true. Multiple
candidate windows without `replace_all` are an ambiguity error, exactly like
multiple exact matches.

An `old_string` that ends in a newline is an explicit byte-span request; the
line-window fallback does not run for that shape, because replacing line text
would silently drop the boundary the caller asked to include.

The replacement span is the byte range from the first matched line's start to
the last matched line's end, excluding the final line ending. Bytes outside
that span are never rewritten. The replacement inherits the file's line-ending
style for new lines, so a CRLF file stays CRLF.

### 3. Normalized edits are disclosed

When the fallback is used, the tool result carries a warning: the edit matched
after normalizing line endings/whitespace/typographic characters. The mutation
is still deterministic and reviewable; the warning exists so a human reviewing
the diff knows why the match succeeded.

### 4. `replace_all` is explicit, not inferred

`replace_all` defaults to false. A caller that wants a rename or a repeated
literal must say so. The schema description states the uniqueness rule and the
fallback behavior, so the model can choose the right call instead of learning
it from a failure.

### 5. The matcher is pure and shared by preview and execution

`plan_edit` remains the single pure function used by both the preview and the
execution path. A preview that re-implemented matching would eventually show a
diff for an edit that execution rejects, or worse, apply a different one. The
fallback is inside `plan_edit`, so both callers see the same result.

## Invariants

1. **Exact first.** A normalized fallback never runs when an exact match
   exists.
2. **Unique or explicit.** A non-`replace_all` edit requires exactly one match,
   exact or normalized.
3. **No collateral bytes.** Only the matched span is replaced; normalization is
   a comparison device, not a rewrite of the file.
4. **Line endings preserved.** A CRLF file remains CRLF after an edit.
5. **Indentation is semantic.** The fallback does not trim leading whitespace.
6. **No no-op success.** `old_string == new_string` is rejected.
7. **Pure and shared.** Preview and execution call the same `plan_edit`.

## Consequences

- Fewer failed edits and fewer retry turns on CRLF/whitespace/typographic
  mismatches.
- `edit_text` gains `replace_all`, closing the "rename every occurrence" case
  without a whole-file rewrite.
- The matcher grows a line model and a small normalization table; the diff
  preview still comes from the finalized content, not from the match strategy.
- A normalized match is visibly disclosed, so the fallback cannot silently
  mask a wrong location.
- The exact-match fast path remains byte-identical to the old behavior, so
  existing exact edits are unaffected.

## Negative Knowledge

- **Do not** trim leading indentation to make a match succeed. Indentation is
  part of the code; a match that ignores it can land in the wrong block.
- **Do not** normalize the whole file and write it back. That rewrites bytes
  the caller never targeted.
- **Do not** use a fuzzy/levenshtein threshold. A near-match is not evidence of
  intent; either the lines are equal after a declared normalization or the edit
  fails.
- **Do not** infer `replace_all` from multiple matches. Multiple matches without
  an explicit flag are an error, not a permission slip.
- **Do not** add a second matcher in the preview path. One pure function, two
  callers.
