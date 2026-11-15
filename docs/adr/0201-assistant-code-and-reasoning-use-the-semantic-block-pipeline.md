---
id: ADR-0201
title: "Assistant Code and Reasoning Use the Semantic Block Pipeline"
status: accepted
date: 2026-11-15
scope: tui/presentation, nuo-view, syntax
superseded_by: null
supersedes: []
negative_knowledge: true
---

# 0201. Assistant Code and Reasoning Use the Semantic Block Pipeline

- Status: Accepted
- Date: 2026-11-15

## Context and Problem Statement

Two presentation paths bypassed the semantic block model. Assistant Markdown
code fences were painted as uniformly colored text with a line-number gutter,
even though tool-result code and diffs were syntax-highlighted. The most
common place a user reads code was the least informative. Separately, an
expanded reasoning trace rendered only `Block::Text`: a fenced code block,
list, heading, table, or quote in the thinking stream was parsed and then
silently dropped.

Both defects came from local renderers re-implementing a subset of the block
model instead of projecting through the shared one.

## Architectural Decisions

### 1. Syntax projection is a `nuo-ui-primitives` pure function

`project_syntax_slice` and `project_syntax_segments` move from the shell's
diff renderer into `nuo_ui_primitives::syntax`. They are pure projections over
token spans and byte ranges; keeping them in the primitive crate lets
`nuo-view` and the shell share one implementation. The shell's diff renderer
re-exports the primitive function instead of owning a copy.

### 2. `code_gutter_line_syntax` lives in `nuo-view::text_layout`

The syntax-aware gutter row is a presentation primitive: it owns the gutter,
token colors, selection splitting, and padded tail. The assistant Markdown
code block and the disclosure code renderer both call it. A wrapped
continuation row keeps the token color that started on the previous row.

### 3. Assistant fences map their language tag explicitly

`draw_code_block` maps the fence tag (`rust`, `ts`, `python`, `bash`, …) to the
line tokenizer's `Language`. Unknown tags stay `Plain`; the renderer never
guesses a language from content. The mapping is local to the code block
because the tokenizer's `from_path` is extension-based.

### 4. Reasoning flattens non-prose blocks instead of dropping them

The expanded reasoning body still uses the muted prose style for text, but
every other block is flattened to its raw text with a marker (`> ` for quotes,
`- `/`1.` for list items, `[x]`/`[ ]` for tasks, `∑` for math, the rendered
grid for tables, a rule glyph for `Rule`). `Block::Break` is the only skipped
variant because it carries no content. This is deliberately simpler than a
full second Markdown renderer; the contract is "nothing disappears."

## Consequences

- **Positive**: assistant code is highlighted; a thinking trace no longer
  loses code, lists, or tables; one syntax projection is shared by every code
  surface.
- **Negative & Incurred Costs**: the tokenizer is line-local, so multi-line
  comments/strings still do not highlight across lines. Reasoning code is
  flattened to prose rather than a code band; a future richer renderer can
  replace the flattening without changing the block model.
- **Neutral**: selection/copy still resolves source bytes; the syntax spans do
  not alter the document.

## Discarded Alternatives (Negative Knowledge)

- **Duplicate the shell token projection in `nuo-view`**: two copies drift,
  and the shell already had a tested implementation.
- **Re-tokenize each wrapped row independently**: loses the token context of a
  token that starts on an earlier row.
- **Leave reasoning non-text blocks unrendered**: the exact silent data loss
  this ADR retires.

## Links

- [ADR-0176](0176-read-is-one-capability-with-bounded-content-dispatch.md)
- [ADR-0185](0185-storybook-retirement-and-single-presentation-path.md)
