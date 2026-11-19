---
id: ADR-0221
title: "The Workspace Trust Gate Is Composed From Columns and Bands, Not Glyphs and Prose"
status: accepted
date: 2026-12-21
scope: presentation/nuo-view, presentation/nuo-ui-primitives, tui/pre-attach, tui/question-model
superseded_by: null
amends:
  - "[ADR-0127](0127-workspace-trust-review-obligation-request-lifecycle-and-consistent-refusal.md) §2 (the gate's question wording is reduced to one line per reason class; the assets it used to name in prose are named on their rows)"
  - "[ADR-0127](0127-workspace-trust-review-obligation-request-lifecycle-and-consistent-refusal.md) §7 (`[INV-TRUST-09]`'s pixel assertions are re-anchored on columns and tones rather than on the `|  [Changed]` label syntax)"
negative_knowledge: true
---

# 0221. The Workspace Trust Gate Is Composed From Columns and Bands, Not Glyphs and Prose

- **Status**: Accepted
- **Date**: 2026-12-21
- **Amends**: [ADR-0127](0127-workspace-trust-review-obligation-request-lifecycle-and-consistent-refusal.md)
  §2 and §7
- **Related**: [ADR-0127](0127-workspace-trust-review-obligation-request-lifecycle-and-consistent-refusal.md)
  (the review obligation, the refusal path, and the original pixel tests),
  [ADR-0152](0152-universal-text-overflow-column-budget-and-focus-reveal-topology.md)
  (one budget, one overflow implementation),
  [ADR-0139](0139-severity-band-language-color-only-severity-on-notice-cards.md)
  (severity rides tone, not a decorative marker)

---

## Context and Problem Statement

The pre-attach gate is the one surface that owns the entire terminal, and it
was the least designed thing in the product. A user meeting it saw:

```
[workspace trust]

Previously trusted content changed on disk: Project instructions.
The earlier approval was for different content, so it does not carry over.
Review the changes before authorizing them again.
Unselected assets stay inactive for this session and will not be asked about
again unless their content changes.

[x] Project instructions  |  [Changed]
    Project rules & instructions

↑/↓ move   Space toggle   Enter confirm   Esc decline
```

Every one of those lines is a decision that was never made:

1. **A display string was load-bearing.** The status lived *inside* the option
   label (`"{}  |  [{badge}]"`), so it could not be toned, aligned, or dropped
   under width pressure — and it had to be *parsed back out* to answer the
   question (`answer_to_domains` split on `"  |  "` and matched path strings
   against a hardcoded table). Two failures followed directly: a label that
   spelled one of those paths resolved whether or not that domain was on screen,
   and no renderer could change the row's design without changing the protocol
   that read it.

2. **The label form violated the project's own visual language.** `docs/reference/tui/visual-language.md`
   §3 reserves brackets for the `[ROLE]` badge and the `[N queued]` count, and
   requires a state token to be one lowercase unbracketed word. `[Changed]` is
   neither: it is misread as a badge and it capitalizes a state.

3. **Five columns of punctuation said what a column boundary says.** `  |  [`
   is a separator glyph plus two brackets, in a product whose selectable-row
   rule (`components/row.rs` rule 3) is *separate columns by whitespace, never by
   a glyph*. The eye had to parse a delimiter to reach the word that justified
   the whole interruption.

4. **The prompt was four sentences about one fact,** and the only sentence that
   carried new information (what happens to an unselected asset) came last.
   The asset names were also repeated in prose after already being listed as
   options — a second, weaker copy of the same list, and the copy that had to
   break words to fit.

5. **The footer advertised four affordances regardless of the surface.** A
   one-row gate offered `↑/↓ move` and `Space toggle` — keys that do nothing —
   and spent half the row's width on them, which left the two keys that actually
   end the decision as the quietest things on screen.

6. **A one-row option carried a second line that said nothing.** `Project rules &
   instructions` under `Project instructions` is a gloss of a name the user can
   already read.

7. **The panel was a fixed 12–28 row slab, top-anchored,** so a short prompt
   painted in the upper third of the screen above a field of black.

8. **The surface reused a question shape it did not have.** It is a
   *closed-set* decision — grant a subset of a known list — but it was modelled
   by the `ask_user` question, which appends a synthetic free-text "Other" row.
   That row was not painted, yet navigation still reached it, so pressing `↓`
   often enough left *no* highlighted row on screen while `Enter` there
   submitted a decision the user could not see.

## Decision Drivers

- **A painted fact must be a value, not a substring.** Anything the surface must
  tone, align, or drop is data; anything it must parse back out is a design
  defect that has already shipped.
- **The visual language is normative, not advisory.** Brackets, separator
  glyphs, and capitalization have assigned meanings; a new surface does not get
  to redefine them because it is important.
- **Separation is carried by area.** A background step says "these rows are one
  object" without spending a row, and without implying a table.
- **A footer may only advertise keys that exist.** An affordance for an action
  the surface cannot perform is a lie that costs the width of a real one.
- **A security prompt is read once, under time pressure.** Every sentence must
  earn its row, and the most surprising fact must be the first one.

## Architectural Decisions

### 1. The row is a domain view, and the status is a column

`view::trust_gate::TrustDomainView` carries `domain`, `label`, and a
`TrustStatus`. `gate_request` builds its options from those views, so an option
label is the asset name and nothing else; the status is a value the surface
paints right-aligned in its own tone.

`TrustStatus` is a closed, lowercase, unbracketed vocabulary (`new`, `changed`,
`expired`) derived from `WorkspaceTrustState::of` — total over the state enum, so
a seventh state cannot compile until its gate status is chosen. The severity
ladder is a palette decision, not a per-status literal: `changed` takes
`theme.warn()` (the bytes you approved are not the bytes on disk), `new` takes
`theme.muted()`, `expired` takes `theme.dim()`.

### 2. Answers map through the offered set

`answer_to_domains(answers, offered)` resolves each returned label against
`TrustDomain::label` **and** against the domains the surface actually offered.
A label naming an unoffered asset maps to nothing, so an answer can never grant
or refuse authority the user was not shown. This is strictly stronger than the
previous table match, which resolved any label spelling a known path.

### 3. The question is one line per reason class

`gate_question` states *why* the gate is open — changed, expired, never reviewed
— one sentence per class present, and names no assets. The assets are the rows'
job. `GATE_CONSEQUENCE` is a separate constant so the surface paints it in a
secondary tone beneath the rows rather than sharing the question body's
full-strength tone.

This keeps `changed on disk` (which `[INV-TRUST-09]` and the pixel tests anchor
on) while dropping the three sentences that restated it.

### 4. The surface is composed of sections, separated by one blank row

`draw_pre_attach` assembles context → title → headline → decision band →
consequence → footer, joining sections with exactly one blank row. There is no
rule glyph: the decision block is a `theme.raised()` band with the pointed-at row
at `theme.selected()`, so grouping is carried by background steps. The band
spans the panel's full width — an inset band inside a bordered-looking panel
reads as a card floating in a card.

The panel sizes to its content and is centered, so a short decision sits in the
middle of the viewport. When the terminal is shorter than the composition, the
context line drops first, then the consequence: the decision, its reasons, and
the keys that answer it survive every geometry.

### 5. The workspace path is context, and it is conditional

Its own line, in `theme.dim()`. Never `Workspace trust · ~/projects/nuo`: a
`·` would invent a relationship between two fields the reader must then decode.

It is **rendered only when the offered set crosses the project boundary** — that
is, when a `UserAssets` domain is on the list. A project-scoped decision happens
in the directory the user just opened, so the path is zero information there; a
user-global asset makes "my project or my whole machine?" the actual question.

### 6. The footer is adaptive, and the model agrees with it

`Enter` and `Esc` always. `↑↓ move` and `Space toggle` only when there is more
than one row. `Esc`'s label is `skip` rather than a sentence, because what
skipping *leaves behind* is already stated by the consequence line directly
above — and a two-word-plus label pushed the navigation hints of a multi-row gate
into `F1 more`, hiding the keys the surface has.

The footer is not decoration, so the state machine has to match it. A one-row gate
is declared `multi_select = false` in `gate_request`: with the question left
multi-select, `Space` was a live toggle for a key the footer does not advertise,
so pressing it deselected the only asset and the next `Enter` dispatched a
refusal of the thing the user meant to grant. A one-row gate also paints no
checkbox — the pointed-at row is already the answer.

### 7. The panel width is a reading measure

`panel_width` claims `PANEL_MAX_WIDTH` (84 columns) whenever the screen can fund
it and gives back only the frame's `PANEL_H_MARGIN` when it cannot. A percentage
of the viewport is the wrong model twice over: it narrows the panel on a narrow
terminal, where the prompt most needs its columns, and widens it on a wide one,
where the row's two fields are pushed to opposite edges — a wider measure adds no
legibility there, it only opens a gulf between the name and its status.

### 8. A closed question has no free-text row — in the state machine either

`QuestionModel::open_closed` suppresses the synthetic "Other" row. Suppressing it
only at paint time was not enough: the row still existed for navigation, so the
highlight could land on an unpainted row and `Enter` would submit an invisible
decision. A row the surface does not paint must not exist in the model.

The policy travels with the model (`QuestionModel::free_text`), including through
`update_mut`'s take-and-replace, which would otherwise restore the free-text row
on the first keystroke.

### Normative Invariants Established

- `[INV-TRUST-10] A painted status is a value`: an option label carries no
  status syntax, and no consumer parses a label to recover a decision. Enforced
  by `view::trust_gate::tests::the_label_is_the_plain_asset_name` and
  `shell::snapshot_tests::pre_attach::the_painted_gate_carries_no_status_syntax_in_the_asset_name`.
- `[INV-TRUST-11] An answer maps through the offered set`: a label naming an
  unoffered asset grants nothing. Enforced by
  `view::trust_gate::tests::an_answer_naming_an_unoffered_asset_grants_nothing`.
- `[INV-TRUST-12] Sections are separated by area`: no rule glyph on the gate, and
  statuses share a trailing column. Enforced by
  `the_painted_gate_separates_sections_by_area_not_by_a_rule` and
  `the_painted_gate_aligns_statuses_into_one_column`.
- `[INV-TRUST-13] A footer advertises only keys that exist`: a one-row gate does
  not mention navigation, and a multi-row gate's four keys all fit. Enforced by
  `the_painted_gate_advertises_only_keys_that_exist`.
- `[INV-TRUST-15] An unadvertised key cannot change the answer`: a one-row gate is
  not multi-select, so `Space` cannot deselect the only asset and turn the grant
  into a refusal. Enforced by
  `shell::pre_attach::tests::a_single_asset_gate_cannot_be_deselected_by_a_key_it_does_not_advertise`.
- `[INV-TRUST-16] The panel's width is a measure, not a fraction`: it claims its
  reading measure and yields only the frame's margin. Enforced by
  `shell::pre_attach::tests::the_panel_claims_its_measure_and_yields_only_the_frame_margin`.
- `[INV-TRUST-14] A closed question has a closed state machine`: navigation
  cannot reach an unpainted row. Enforced by
  `question_model::tests::a_closed_question_never_parks_the_highlight_on_an_unpainted_row`.

## Consequences

- **Positive**: The gate is now composed from the product's existing row and
  section grammar, so it reads like the rest of the TUI rather than like a
  debug print. The decision is one line each, its status is a column the eye can
  scan vertically, and the two keys that answer it are the two most prominent
  things on screen.
- **Positive**: The security answer no longer depends on a display string.
  Rows and options are generated together from one enumeration
  (`every_offered_row_lines_up_with_its_option`), and answers are filtered by the
  offered set, closing a grant path that a hand-built or stale answer could
  reach.
- **Negative & Trade-offs**: `answer_to_domains` grew a parameter, and
  `UserQuestionOption::description` is no longer populated by the gate. The
  description slot remains for `ask_user`; the gate states the collective
  consequence once instead of glossing each asset name.
- **Negative & Trade-offs**: The gate's footer says `skip` where it used to say
  `decline`/`continue untrusted`. The word is shorter and the meaning moved
  one line up, but a reader who lands directly on the footer loses the longer
  gloss — which is why `[INV-TRUST-13]`'s test also asserts the consequence line
  is painted.

## Rejected Alternatives & Negative Knowledge

### Alternative A: keep the label syntax and only restyle it

- **Why considered**: Smallest change — no new type, no signature change, no
  protocol-adjacent edits.
- **Why rejected**: It preserves the defect. A status inside a label cannot be
  right-aligned (the column would have to be padded inside the string), cannot be
  dropped under width pressure independently of the name, and still has to be
  parsed back out. Restyling a load-bearing display string is how the string
  stays load-bearing.

### Alternative B: a horizontal rule between sections

- **Why considered**: The obvious way to separate a list from its surrounding
  prose, and it costs one row.
- **Why rejected**: It spends a row to say what a background step says, and it
  reads as a table header — implying columns where there are none. The decision
  block is one object; a band says so with area alone.

### Alternative C: keep `Workspace trust · ~/projects/nuo` on one line with two tones

- **Why considered**: Saves a row, and tone does distinguish the two fields.
- **Why rejected**: The `·` is still a glyph asserting a relationship. Two fields
  on one line need a join; two fields on two lines need nothing. The path is also
  conditional, which a joined line cannot express without leaving a dangling
  separator.

### Alternative D: suppress the "Other" row only in the renderer

- **Why considered**: The gate has its own `draw_pre_attach`, so the painter
  could simply skip the trailing row.
- **Why rejected**: That is exactly the bug. Navigation, selection, and `Enter`
  read the model, not the painter, so a row the painter skips is a row the user
  can still land on and submit. The suppression has to be in the state machine.

### Alternative E: give the gate its own state machine instead of `QuestionModel`

- **Why considered**: The gate's semantics (a set decision over a closed list)
  are not `ask_user`'s.
- **Why rejected**: `QuestionModel`'s MVU purity is what makes the gate's input
  path testable without a terminal, and its multi-select semantics are exactly
  right. The mismatch was one optional row, so the fix is one policy flag — not a
  second state machine to keep in sync with the first.

## Links

- Amends: [ADR-0127](0127-workspace-trust-review-obligation-request-lifecycle-and-consistent-refusal.md)
- Related: [ADR-0152](0152-universal-text-overflow-column-budget-and-focus-reveal-topology.md)
  — the row builder and overflow implementation this surface now uses
- Reference: [`docs/reference/tui/trust-gate.md`](../reference/tui/trust-gate.md)
