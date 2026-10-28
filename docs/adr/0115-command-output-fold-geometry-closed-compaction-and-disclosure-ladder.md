---
id: ADR-0115
title: "Command Output Fold Geometry: Closed Compaction, Tail-Heavy Window, and a Disclosure Ladder Derived From the Rendered Body"
status: accepted
date: 2026-12-09
scope: presentation/nuo-view, presentation/nuo-ui-primitives, tool/nuo-tool, tui/disclosure, interaction/affordance
superseded_by: null
negative_knowledge: true
---

# 0115. Command Output Fold Geometry: Closed Compaction, Tail-Heavy Window, and a Disclosure Ladder Derived From the Rendered Body

- Status: Accepted
- Date: 2026-12-09
- Deciders: Nuo Architecture Working Group
- Consulted: Presentation Substrate, Tool Surface, Interface Architecture
- Amends: [ADR-0093](0093-abolition-of-pseudo-vfs-spool-architecture-and-salience-folding.md) — extends `[INV-FOLD-01]`'s tail-heavy salience bias from the *model-facing text projection* to the *human-facing viewport*, where the same bias now has a closed geometry

---

## Context and Problem Statement

An expanded command step can emit hundreds of stdout/stderr rows. The rows that explain the outcome —
the `exit N` status line, the termination footer (timeout / blocked / cancelled), the panic or error
signature — sit at the **tail**; the rows that restate the invocation sit at the **head**; and the
middle is usually volume. Folding the middle keeps the outcome visible without hiding the setup.

The first implementation was scattered and open:

1. **The threshold was open.** Folding kicked in at `HEAD + TAIL + 1` *lines* while the folded body
   paints `HEAD + TAIL + 3` *rows* (a blank/label/blank negative-space gap). An 8-row body therefore
   "folded" into 9 rows: the control reported a compaction, the renderer expanded it. The control and the
   chrome each held their own arithmetic, so the defect was invisible to both.
2. **The activation ladder was invented at the call site.** A command step has three states — collapsed,
   expanded-and-folded, expanded-and-revealed — but "can this step be revealed?" was answered by
   guessing from the tool name or the row count at the key handler, not by asking the body's own
   derivation. A press could promise a reveal that revealed nothing.
3. **The fold lived in the wrong crate.** Row derivation, exit-status parsing, termination recovery and
   coverage classification were interleaved with painting, so the renderer, the copy path, and the model
   projection each re-derived "what rows exist" from the same payload.

## Decision Drivers

- **A fold is a compaction, so it must be *closed* over row count** — the folded body may never be
  taller than the verbatim body, for any input.
- **The bias is the feature** — a symmetric or head-heavy window would hide the outcome it exists to
  preserve, so the tail-heavy relationship must be an enforced property, not a tuned constant.
- **No dead key presses** — a disclosure rung exists exactly when there is something hidden to reveal,
  and that is a property of the *painted body*, not of the tool's name.
- **One derivation, three audiences** — the renderer (what to paint), the clipboard (what was copied),
  and the model projection (what was sent) must agree on which rows exist.

## Architectural Decisions

### 1. Closed fold geometry in the design substrate

The geometry is three constants plus one predicate, and the predicate is the only authority:

```rust
// nuo-ui-primitives::design
pub const BASH_FOLD_HEAD_ROWS: usize = 2;          // leading context kept
pub const BASH_FOLD_TAIL_ROWS: usize = 6;          // trailing context + every event footer
pub const BASH_FOLD_CHROME_ROWS: usize = 3;        // blank / label / blank
pub const BASH_FOLD_MIN_VERBATIM_ROWS: usize =
    BASH_FOLD_HEAD_ROWS + BASH_FOLD_CHROME_ROWS + BASH_FOLD_TAIL_ROWS;

/// Folding must always save rows: at the minimum the folded body is exactly as
/// tall as the verbatim one, so it stays verbatim (nothing hidden for no gain).
pub const fn bash_fold_applies(logical_rows: usize) -> bool {
    logical_rows > BASH_FOLD_MIN_VERBATIM_ROWS
}
```

`[INV-FOLD-02]` — **closed compaction.** `MIN_VERBATIM_ROWS` is *derived* from the three geometry
constants, so the threshold cannot drift from the chrome it accounts for. A compile-time assertion
enforces the bias (`TAIL > HEAD`).

### 2. The derivation is a pure function in the presentation substrate

```rust
// nuo-view::tools::shell_output
pub struct ShellOutput { rows: Vec<String>, exit: Option<i32>, termination: ShellTermination, coverage: OutputCoverage }
pub struct BashFold    { head_end: usize, tail_start: usize, hidden: usize, omitted_bytes: usize }

impl ShellOutput { pub fn derive(output: &str, structured: Option<&ToolOutput>) -> Self; pub fn fold(&self) -> Option<BashFold>; }
pub fn command_step_fold(name: &str, output: Option<&str>, structured: Option<&ToolOutput>) -> Option<BashFold>;
```

The module owns no styling and no drawing. It answers three questions: which logical rows the body has,
what the outcome was, and what each audience holds. Row materialisation prefers the arrival-ordered
`lines` (interleaved stdout/stderr, the UI-authoritative order) and falls back to the flat band strings
for live seeds and restored pre-structured sessions.

### 3. Coverage is a tri-partition, not a boolean

```rust
pub enum OutputCoverage {
    Full,            // the UI holds the whole log, and so did the model
    ModelTruncated,  // the model's copy was cut at the context watermark; the UI's is complete
    CutBeforeUi,     // the payload arrived already cut; the middle is genuinely gone
}
```

Collapsing these into one "truncated" flag is what allowed a footer to claim "partial kept" over a body
that nothing had cut. `[INV-FOLD-03]` — **the footer may only assert a loss that actually happened**, and
it names which audience lost it.

### 4. The disclosure ladder is a pure function of the rendered body

```rust
// Collapsed → expanded+folded → expanded+revealed → collapsed
pub fn bash_disclosure_ladder(expanded: bool, revealed: bool, has_fold: bool) -> (bool, bool) {
    match (expanded, revealed, has_fold) {
        (false, _, _)        => (true, false),
        (true, false, true)  => (true, true),
        (true, _, _)         => (false, false),
    }
}
```

`has_fold` comes from `tool_step_folded_rows()`, which calls the *same* `command_step_fold` the renderer
paints with. `[INV-FOLD-04]` — **the reveal rung exists iff the body folds**, so the ladder and the
painting cannot disagree and no key press is dead. Collapsing forgets the reveal (`output_revealed =
false`), so re-expanding always starts from the folded viewport; the flag is ephemeral view state and is
never persisted.

## Normative Invariants Established

- `[INV-FOLD-02]` **Closed compaction** — folding never renders more rows than the verbatim body.
  Enforced by: `BASH_FOLD_MIN_VERBATIM_ROWS` being derived from the geometry constants, plus
  `bash_fold_only_applies_when_it_saves_rows` sweeping all row counts.- `[INV-FOLD-03]` **Honest coverage** — a truncation claim states which audience lost content, and only
  when that loss occurred. Enforced by: `OutputCoverage` being a three-valued enum with no `bool`
  constructor.
- `[INV-FOLD-04]` **Ladder derived from the body** — the reveal rung is present exactly when the painted
  body folds. Enforced by: `bash_disclosure_ladder` taking `has_fold` from `command_step_fold`, the single
  derivation the renderer also calls.

## Consequences

- **Positive**: the fold can no longer invert (a "compaction" that grows the step), for any input, by
  construction rather than by tuning.
- **Positive**: the tail-heavy bias is enforced at compile time (`TAIL > HEAD` and `TAIL >= 2 × HEAD`
  are `const` assertions beside the constants), so a future layout tweak cannot silently start hiding
  the outcome.
- **Positive**: row existence is answered once, so the copy path and the model projection stop
  re-deriving it — and the clipboard can no longer disagree with the screen about what exists.
- **Negative & trade-offs**: a step must materialise its logical rows to know whether it folds, so the
  activation ladder pays a derivation per key press. Mitigation: the derivation is a pure function over
  the already-in-memory payload, and the answer is only needed when the user toggles.
- **Composition**: the painting path is now a consumer, not a peer — `draw_command_content` derives a
  `ShellOutput`, asks `.fold()`, and paints `head / chrome / tail`. The three helpers that used to hold
  the parallel arithmetic (`parse_unstructured_shell_content`, `command_structured_lines`,
  `emit_command_lines_folded`) are deleted, so no second threshold exists to drift. The fold label is
  built from the hidden rows themselves and carries the first salient failure signature via `nuo-tool`'s
  scanner (`[INV-FOLD-01]`), so the human and the model are pointed at the same line.

## Rejected Alternatives & Negative Knowledge

### Alternative A (Rejected): fold at `HEAD + TAIL + 1` lines and let the renderer add its own chrome

- **Why considered**: The control only needs to know "is this long?", and the renderer owns its gap.
- **Why rejected**: Disproved by construction — an 8-row body folds to 9 rows. Two crates holding
  halves of one arithmetic is the defect, not the tolerance.

### Alternative B (Rejected): a boolean `truncated` flag

- **Why considered**: Simpler; one flag covers "the model didn't see everything" and "the middle is
  gone".
- **Why rejected**: The two facts have different remedies (reveal vs. re-run with a wider budget) and
  different owners (the harness cut one; the payload arrived with the other). A boolean forced the footer
  to lie about one of them.

### Alternative C (Rejected): decide the ladder rung from the tool name at the key handler

- **Why considered**: Command steps are known by name, so the handler could branch on `execute_command`.
- **Why rejected**: Name is not body. A command step whose result is an `Error` or `PermissionDenied`
  renders as an error body and does not fold, so a name-based rung offers a reveal that reveals nothing —
  a dead key press by design.

## Links

- Related ADRs: [ADR-0093](0093-abolition-of-pseudo-vfs-spool-architecture-and-salience-folding.md) (`[INV-FOLD-01]` model-facing tail-heavy folding), [ADR-0070](0070-unified-cas-vfs-projection-and-epistemic-meta-tool-abolition.md) (content-addressed payload projection), [ADR-0026](0026-streamed-tool-input-progress.md) (tool-step lifecycle and disclosure)
- Code: `crates/nuo-ui-primitives/src/design.rs`, `crates/nuo-view/src/tools/shell_output.rs`, `crates/nuo-view/src/model/document.rs` (`tool_step_folded_rows`, `bash_disclosure_ladder`), `crates/nuo-view/src/model/interactive.rs` (`toggle_expanded`)
