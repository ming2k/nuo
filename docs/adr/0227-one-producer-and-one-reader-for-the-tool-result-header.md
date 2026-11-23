---
id: ADR-0227
title: "The Tool-Result Header Has One Producer and One Reader"
status: accepted
date: 2026-12-20
scope: tool/tool_result, engine/cognition/agent, provider-google-antigravity, nuo/shell, server/product
superseded_by: null
negative_knowledge: true
---

# 0227. The Tool-Result Header Has One Producer and One Reader

- **Status:** Accepted
- **Date:** 2026-12-20
- **Related:** [ADR-0172](0172-request-plan-containers-are-regions-and-blocks-are-boundaries.md),
  [ADR-0226](0226-one-renderer-and-one-reader-for-message-envelopes.md),
  [ADR-0093](0093-abolition-of-pseudo-vfs-spool-architecture-and-salience-folding.md)

## Context and Problem Statement

Every model-facing tool result begins with `[<name> result]:`. The format had five
producers and four independent readers:

- producers: `rounds/tools.rs` (three branches), `spool_and_fold_generic_output`,
  and `subagent_result_text` in `agent/mod.rs`;
- readers: `nuo::shell::transcript::parse_tool_result`,
  `server::product::export::parse_tool_result`, a `ends_with("result]:")` check in
  the code payload renderer, and a sixth spelling invented by
  `provider-google-antigravity` on its degraded path:
  `[Tool result for "{name}"]:` / `[Tool result for call "{id}"]:` /
  `[Tool result]:`.

None of those readers could see the others. The provider's variant was invisible
to every one of them, so a degraded Google turn produced a transcript the export
and the TUI could not name.

There is a prior question this ADR also answers: **why does the header exist at
all**, given that a tool result is already a protocol field with a `Tool` role and
a `tool_call_id`? Two reasons survive:

1. A **degraded projection**: a provider that cannot express a tool role for this
   turn falls back to a user-role text message, and the header is the only thing
   that still names the call.
2. **Human readers**: the transcript renderer and the Markdown export recover the
   tool name from it.

So the header is presentation. It is not state, and no reader may use it to
recover something the protocol already carries.

## Decision

### 1. `tool::tool_result` owns the format

```rust
pub fn render(tool: &str, body: &str) -> String
pub fn render_with_note(tool: &str, body: &str, note: &str) -> String
pub fn parse(content: &str) -> Option<(&str, &str)>
pub fn is_tool_result(content: &str) -> bool
```

The suffix is a private constant. No caller spells it.

### 2. Every producer formats through it

All five producer branches call `render` / `render_with_note`. A branch that needs
a trailing spool pointer or a subagent re-anchor passes it as `note`; the header
still appears exactly once.

### 3. Every reader parses through it

`parse` returns `(tool, body)` with exactly one leading newline removed, which is
what `render` inserted; everything else is preserved byte for byte. The TUI
transcript, the Markdown export, and the code payload renderer call it (the last
via `is_tool_result`).

### 4. The provider's sixth spelling is deleted

On its degraded path, Google Antigravity now reuses the caller's header when one
is already present and otherwise renders one with `render`. A provider may *drop*
a header it cannot use; it may not invent a format the rest of the system cannot
read.

### 5. The header is not a parse surface for state

A future reader that needs the tool name must read `tool_call_id`, not the header.
`parse` exists for the two human-facing readers above and is documented as such.

## Invariants

- **`[INV-TOOLHEADER-01]` One Producer** — no file outside `tool::tool_result`
  formats a tool-result header. A governance scan rejects the literal.
- **`[INV-TOOLHEADER-02]` One Reader** — every reader calls `parse` or
  `is_tool_result`.
- **`[INV-TOOLHEADER-03]` No Provider-Invented Spelling** — a provider projection
  either keeps the caller's header or renders one through the shared function.
- **`[INV-TOOLHEADER-04]` The Header Is Presentation** — nothing parses it to
  recover state the protocol carries.

## Consequences & Trade-offs

- **Positive**: five producers and four readers collapse onto one format; the
  Google degraded path becomes readable by the transcript and the export; a future
  format change is one edit.
- **Negative & incurred**: the Google degraded projection's bytes change. That
  path is only reached for an unsigned cross-provider call, and the old bytes were
  unparseable, so the new spelling is strictly more legible.
- **Neutral**: the header's presence and shape are unchanged for every other
  producer.

## Discarded Alternatives

### A — Keep five producers and add a lint over the literal

- **Why considered**: no shared crate edge and no cross-crate API.
- **Why rejected**: the defect was writer/reader drift, and a lint over a
  literal cannot see a new spelling. The Google provider invented a sixth format
  that no lint or reader could name; only a shared parser sees that class.

### B — Delete the header and rely on the protocol `Tool` role and `tool_call_id`

- **Why considered**: the header is presentation, and the protocol already
  carries the call identity.
- **Why rejected**: a provider that cannot express a tool role for this turn
  falls back to a user-role text message; the header is the only thing that
  still names the call. The transcript and the Markdown export also need a
  human-readable name.

### C — Keep the Google provider's spelling and teach the readers

- **Why considered**: no provider change, and the degraded path is rare.
- **Why rejected**: four readers would need a fifth branch, and the spelling is
  not parseable by the shared parser. A provider may drop a header it cannot
  use; it may not invent a format the rest of the system cannot read.

## Validation

- `crates/tool/src/tool_result.rs` — round trip, note handling, one-newline rule,
  rejection of non-headers.
- `crates/engine/src/cognition/agent/rounds/tests.rs` — the existing header
  assertions now exercise the shared producer.
- `tests/integration/governance/` — `[INV-TOOLHEADER-01]`/`02`.
