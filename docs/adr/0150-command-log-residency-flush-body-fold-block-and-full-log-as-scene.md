---
id: ADR-0150
title: "Command Log Residency: the Flush Body, the Fold Block, and the Full Log as a Scene"
status: accepted
date: 2026-12-22
scope: presentation/nuo-view, presentation/nuo-ui-primitives, tui/disclosure, tui/overlays, tui/surfaces, interaction/affordance
superseded_by: null
negative_knowledge: true
---

# 0150. Command Log Residency: the Flush Body, the Fold Block, and the Full Log as a Scene

- Status: Accepted
- Date: 2026-12-22
- Deciders: Nuo Architecture Working Group
- Consulted: Presentation Substrate, Interface Architecture, Interaction/Affordance
- Amends:
  - [ADR-0115](0115-command-output-fold-geometry-closed-compaction-and-disclosure-ladder.md) — the fold stays a closed viewport compaction; what changes is the *chrome it spends* and *what the chrome says*
  - [ADR-0116](0116-content-residency-and-surface-layer-topology.md) — applies `[INV-LAYER-01]` to a command's log: content, therefore L0/scene, never L3/dialog
  - [ADR-0140](0140-command-effect-classes-readings-are-receipts-not-records.md) — the copy receipt must describe the exact slice that landed on the clipboard
- Supersedes: the `rail` draft of this ADR (a `│` track down every command row). Removed in full, with its tokens (`COMMAND_RAIL_GLYPH`, `command_rail_glyph`, `COMMAND_RAIL_COLS`, `Theme::rail_rule`, `Theme::rail_prompt`) and its renderer (`RailCtx`). The negative knowledge below is why.

---

## Context and Problem Statement

An expanded `execute_command` step paints a `$ invocation` line, the output rows, and — when the body is long — the chrome that stands in for the rows a fold hides. Three defects made that chrome the least trustworthy region in the transcript.

1. **A left track is not chrome, it is contraband.** A `│` in front of every row survives the renderer's own copy path (which can skip a `prefix_cols`-wide prefix) but **not** the terminal's. `Shift`-drag is the selection users actually reach for in iTerm2, Alacritty, Kitty, Windows Terminal and tmux, and it operates on cells, not on a layout map. Every line copied that way arrived with a border glued to its front: pasting a build log into an issue carried `│`, and pasting a command into a shell produced `bash: │: command not found`. A decoration that a user must hand-strip from `n ×` pasted lines is not decoration.
2. **The track also mis-signalled.** `theme.dim()` — the track's original tone — is the rung `[INV-TUI-RUNNING-01]` reserves for cancelled/inert rows, so a healthy log looked faded. Repainting it as a surface rule fixed the colour and left the deeper problem: a 30-row column is a **frame**, and the block does not have one. It competed with the content's own indentation and spent a column of every row on an edge that says nothing the `$` does not already say.
3. **The fold chrome was one crowded line.** `... 246 lines hidden (7.1 KB) — error: …      [o] view log   [y] copy` put three unrelated things — a count, a truncated diagnostic, and two verbs — on one full-width row. On a wide terminal the count and the verbs sat tens of columns apart with nothing between them; on a narrow one the count truncated to `2 li…` while the verbs held their columns. The brackets were a second keycap vocabulary competing with the `key label` form every footer uses. And the two verbs shared one row-wide hit region, so clicking `copy` opened the log.
4. **`view log` opened a dialog, and a log is not a dialog.** A thousand rows inside a centered floating panel fabricates a second reading position for text that already has one home, spends the terminal's width on a border, and — because the modal stack is not the tab's scene stack — cannot be walked back with `Alt+[`, cannot appear in breadcrumbs, and does not participate in scroll anchoring or resize.
5. **`o` was not the verb's letter.** The row printed `view log` and bound `o`. A reader has to translate "I want to *view*" into "press *o*", and the alias cost a neighbour chord (`Alt-o` notices). `y`/`c` and `Y`/`C` were duplicated pairs for the same reason: legacy spellings nobody could learn from anything on screen.

## Decision Drivers

- **Content lives in the content layer.** `[INV-LAYER-01]` (ADR-0116) puts dialogue and its causal facts at L0, receipts at L2, spatial configuration at L3, transient feedback at L4. A command's output is `L0` content.
- **What a user selects is what the process printed.** Copy purity cannot depend on the user choosing our copy path. Chrome must therefore be either *absent* from the content's columns or *bounded* to rows that are not content — never a per-row prefix.
- **A keycap is a promise.** A row that prints `v view full log` must open the log; a row that prints `y copy hidden` must copy the hidden rows. One hit target for two verbs breaks that promise whichever verb the handler picks.
- **The clipboard is a receipt** (ADR-0140). What lands there must be exactly what the sentence describing it says landed there.
- **Plain beats ornate.** A terminal block earns its structure from surface, alignment, whitespace and typographic hierarchy. Glyph fences, brackets, tracks and dotted rules are the vocabulary of a UI that cannot afford backgrounds; Nuo can.

## Architectural Decisions

### 1. A command body is flush — there is no rail

`CommandBodyCtx` (formerly `RailCtx`) carries where the block starts and which tones its parts take. It carries **no glyph**. Every body row — the `$ invocation`, the output rows, the exit strip, the governance strip — begins at `indent`, and the block is identified by three things that cost no column:

- the `$` prompt on the invocation row, in `Theme::command_prompt()` (brand; content tone on a monochrome profile), because it names the *act* and is the one cell a reader scanning a long transcript needs to find;
- the `code_surface` the whole body sits on;
- the uniform indent the head, the body and the footer share.

`COMMAND_RAIL_GLYPH`, `command_rail_glyph`, `COMMAND_RAIL_COLS`, `Theme::rail_rule`, `Theme::rail_prompt` and `RailCtx` are **deleted**. A grep for them returns nothing; there is no deprecated alias, because an alias is a second way to draw a border.

### 2. Painted chrome is never registered text; registered text is never chrome

`BlockRegion::text` must be the string the block model stores at `start_byte..end_byte`, because a selection slices the block with the offsets the region recorded. The `$ ` prompt is therefore **painted but not registered**: `prefix_cols` is where the content begins, so a click on the prompt resolves to byte 0 of the command and a drag that starts there copies the command rather than the `$`.

This is the invariant that makes "the `$` line is copyable" a real fix rather than a regression: the tempting repair — registering the *painted* `$ cargo build` — would make every region in a command body index a string the block model does not hold, silently shifting every selection two cells to the right.

```text
painted row:      $ cargo build
registered text:  cargo build          (prefix_cols = 2)
copied:           cargo build
cursor_at(0..2)   → byte 0 of "cargo build"
```

### 3. The fold is a **block**, inset, with a gutter mark on its own two rows

The fold is the one region of a command body that is not process output. Its chrome is four rows and two columns of inset:

```text
  $ cd /data/projects/geodrb && ./scripts/build.sh
  Using daer compiler: /data/projects/daer/target/release/daer
  daer 0.0.1

    ▎ 246 lines hidden (7.1 KB)
    ▎ press v view full log, press y copy hidden

  For more information, try '--help'.
  warning: geoip:microsoft not found in upstream dataset, skipping.
```

- **The blank rows belong to `BASH_FOLD_CHROME_ROWS`.** They are budget, not decoration: they are what lets a shift-drag over the head or the tail end on a clean edge instead of catching the fold's own rows. Counting them keeps `bash_fold_applies`'s closed geometry honest — folding still only ever *saves* rows, and the compile-time assertions now check `HEAD + 4 + TAIL`.
- **The inset is `BASH_FOLD_INSET_COLS` (2).** The block steps in from the content's own left edge, which is what tells the reader "this is not stdout" before any word is read.
- **The block has no surface of its own.** It sits on the same code surface as the rows it interrupts; the separation is whitespace plus one mark. A second background tier was tried and removed — it was decoration competing with the transcript's own surface ladder, and it made the fold read as a card rather than as an absence.
- **The gutter mark (`▎`, `bash_fold_gutter_glyph` → `|` on the ASCII set) is bounded to the block's two text rows.** It is the same mark the transcript already uses for "structure, not content" (`Theme::pane_marker_glyph`), so the vocabulary does not grow. Because it stops where the block stops, nothing a user copies passes through it.
- **The fact row is chrome and registers nothing**: no selection can capture a sentence the process never printed.

### 4. Two rows, two jobs: the fact, then the sentence

The old chrome spent one row on three jobs and lost all three. The block spends two rows on two jobs.

| Row | Content | Tone |
| :--- | :--- | :--- |
| fact | `246 lines hidden (7.1 KB)`, then ` — <first salient failure signature>` when one exists | count in `code_text`; the join in `command_fold_gutter()`; the signature in `warn` |
| verbs | `press v view full log, press y copy hidden` | connectives in `muted`; **`v` and `y` in `keycap_accent` + BOLD** |

- **The count never truncates — it picks a shorter whole form.** It is the one fact the row exists to carry. The renderer chooses the most specific of `246 lines hidden (7.1 KB)` → `246 lines hidden` → `246 hidden` that fits, and paints the gutter mark alone when none does. The previous chrome shipped a *cut* count (`2 li…`) at narrow widths, which is a wrong number: an assertion about the log that the log does not support.
- **The signature truncates at token boundaries, never at columns.** `truncate_to_width` cuts at grapheme boundaries, so a tight budget turns `error[E0425]: cannot find …` into `error[E042…` — an identifier for an error that does not exist, at exactly the widths where a reader most needs the code. `fold_signature_for_budget` therefore keeps the leading token whole and cuts only between tokens (`error[E0425]: cannot find…`), and prints nothing at all when even the token will not fit. `BASH_FOLD_SIGNATURE_MIN_COLS` is that floor.
- **The verbs are a sentence that degrades in whole renditions.** `press v view full log, press y copy hidden` → `v view full log, y copy hidden` → `press v view full log` → `v view full log` → `v, y` → `v`. The ladder is a list of complete renderings, not a width budget, so a clause is never painted as `press v view f` — a partial verb promises a key and then fails to name it. `v` leads because it is the reason the block exists; `y` is the shortcut and yields first. The sentence also states what `y` copies, which the old `y copy` left the reader to guess between "the entry", "the log" and "the hidden rows".

### 5. `v` for view, `y` for yank — one spelling per verb

`VIEW_LOG_KEY = "v"`, `COPY_LOG_KEY = "y"`, `LOG_SCOPE_KEY = "h"`, all in `design.rs`, all read by the fold block, the focused-entry hint row, the command-log scene's footer and the scene switcher. The resolver honours **only** these:

- `o`/`O`/`V` (view aliases) and `c`/`C` (copy aliases) are gone. A key that acts but is printed nowhere cannot be learned from the UI, and each alias cost a neighbour chord — `o` sits one `Shift` from `Alt-o` notices, `c` one modifier from `Ctrl-c` quit. `session::resolve_chat_surface_key` now has one arm per advertised letter, and `only_the_advertised_letters_reach_the_entry_verbs` pins it.
- The hint row, the Help dialog's entry-verb rows, and `docs/reference/tui/interaction.md` all print `v`.

### 6. One hit target per verb, clause-wide

The verb row registers two regions, one per clause, each spanning the words around its key: `TOOL_FOLD_BLOCK_IDX` (`press v view full log`) and `TOOL_FOLD_COPY_BLOCK_IDX` (`press y copy hidden`). `classify_click` resolves each to its own `ClickTarget` before the generic cursor walk — both are zero-text chrome rows, so the walk would otherwise resolve them into an empty content hit.

Clause-wide rather than key-wide, because the target should match what the user perceives as the button: someone who points at the words `view full log` means that verb, and requiring them to hit the one letter is a puzzle, not an affordance. The two clauses never overlap, which is what `the_fold_block_registers_one_hit_target_per_verb` asserts at the click layer.

### 7. Copy semantics: the label decides the slice, and the receipt repeats it

| Verb | What lands on the clipboard | Receipt |
| :--- | :--- | :--- |
| fold block `y copy hidden`, or `y` while a folded command step is focused | `$ invocation` + **the rows the fold hides** | `hidden rows copied — N rows` |
| log scene `y` (scope `full`) | `$ invocation` + **every row** (+ `exit N` when non-zero) | `log copied — $ command + N rows` |
| log scene `y` (scope `hidden`) | `$ invocation` + the hidden middle | `hidden rows copied — N rows` |

A verb that cannot honour its own label reports that (`this step has nothing hidden to copy`) instead of writing a different slice under a confident sentence. The `$ invocation` leads every copy: a pasted log without its command is a page of output with no question attached.

### 8. The full log is a scene, pushed onto the thread tab's own stack

`SceneKind::CommandLog` is a peer of `Subagent`: it is a *reading* of one transcript step, so it belongs to the tab that owns that step and pushes onto that tab's history (`ClientTab::push_scene` accepts `Thread | Subagent | CommandLog` for a thread container). It is not a workspace container (`Dashboard`, `Settings`), which would let a thread's log outlive the thread.

The scene:

- derives its rows once, at open time, into `App::command_log` ([`CommandLog`](../../crates/nuo-view/src/model/command_log.rs)) — a reading of the step, not a second copy of it;
- scopes between `full` and `hidden` (`h`), refusing the toggle when the body did not fold, because a key that reveals an identical page is a dead key press. The footer verb names the scope the key *reaches*, so it reads `h hidden only` while the full log is on screen and `h full log` while the hidden rows are — a band that kept offering the scope the reader was already in would point at the place they are standing;
- paints the shared head band (TabBar + SceneBar named `log`), and a body of numbered rows on the code surface. The line number is the scene's only structure column — the same flush rule as the transcript body applies, so the row a reader shift-drags is the row the process printed;
- registers every body row as a real `BlockRegion`, so drag-and-copy works through the one selection path instead of a scene-specific implementation;
- clamps its own scroll (`App::command_log_scroll`), because routing it through the transcript's scroll would move a transcript the user cannot see.

`Esc` at this depth is `SceneBack` — "back to the transcript" — because the scene has no sub-layer to spend the key on.

## Consequences

- **Every copied line is a line the process printed.** No track, no brackets, no gutter inside the content's columns. A terminal-native shift-drag and Nuo's own selection now agree, which they could not while chrome occupied a per-row column.
- **The fold is legible as a distinct object** at any width: it is inset, padded above and below, and marked on exactly the rows it owns. Wide terminals no longer open a desert between a count and a verb, and narrow ones no longer truncate the count. It carries no background tier of its own — the separation is done with whitespace and one bounded mark, not with a second surface.
- **`BASH_FOLD_CHROME_ROWS` grew from 1 to 4**, so the verbatim threshold moved from 9 rows to 12. This is the cost and it is deliberate: a fold that renders fewer rows while reading as a stray output line saves a row and loses the block.
- **`BlockRegion` gains no chrome field and no new prefix arithmetic.** The rule stays "registered text is the block's text". The alternative — a `RegionStyle` on the region carrying `copy_skip_cols` — was implemented once, found to require maintaining *two* texts per row (`BlockRegion::text` for hit-testing, the block's for slicing), and removed. That is negative knowledge this ADR records.
- **Negative knowledge: a per-row rail cannot be part of a copyable block.** Both attempts at keeping one (a `dim()` track, then a surface rule) fixed the tone and left the copy defect, because the defect is the *column*, not the colour. The lesson is not "use a better rail colour" but "a block whose rows a user copies may not spend a column of every row on an edge".
