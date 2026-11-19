# Trust Gate (Pre-Attach) Reference

Factual reference for the workspace trust gate: the one surface that owns the
entire terminal, what it is composed of, and the tokens and types that compose
it. The *decisions* behind it are recorded in
[ADR-0221](../../adr/0221-the-trust-gate-is-composed-from-columns-and-bands.md);
the review obligation, refusal semantics, and lifecycle in
[ADR-0127](../../adr/0127-workspace-trust-review-obligation-request-lifecycle-and-consistent-refusal.md).

This page is cited by `crates/nuo/src/shell/pre_attach.rs` and
`crates/view/src/trust_gate.rs`.

---

## 1. When It Mounts

`view::trust_gate::gate_request(snapshot)` returns `Some` when any domain reports
a `WorkspaceTrustState::needs_review()` value — `Quarantined`, `Changed`, or
`Expired` — and `None` for the three settled states (`Absent`, `Trusted`,
`Denied`). `PreAttachState::from_snapshot` wraps that; the event loop mounts the
surface, and the render loop early-returns after painting it, so no chat chrome
exists behind it.

---

## 2. The Composition

```text
  ┌ panel (84-col reading measure, centered, sized to content) ─┐
  │                                                             │
  │  ~/projects/nuo                        context, dim()       │
  │                                                             │
  │  Workspace trust                       title, brand() bold  │
  │                                                             │
  │  Content changed on disk since you approved it.             │
  │                                        headline, fg()       │
  │                                                             │
  │  [x] Project instructions                    changed        │
  │  [x] MCP servers                                 new        │
  │                                        decision band        │
  │                                                             │
  │  Unselected content stays off and is not asked about        │
  │  again until it changes.            consequence, muted()     │
  │                                                             │
  │  Enter trust  Esc skip  ↑↓ move  Space toggle               │
  │                                        footer, keycaps      │
  └─────────────────────────────────────────────────────────────┘
```

Sections are joined by exactly one blank row. **There is no rule glyph.** The
decision block is a `theme.raised()` band, and the pointed-at row is
`theme.selected()`, so grouping rides background steps.

### Panel width is a reading measure, not a percentage

`panel_width` claims `PANEL_MAX_WIDTH` (84) columns whenever the screen can fund
them, and gives back only what `PANEL_H_MARGIN` (2) needs when it cannot. A
percentage of the viewport is the wrong model: it narrows the panel on a narrow
terminal, where the prompt most needs its columns, and widens it on a wide one,
where the row's two fields (name at the left edge, status at the right) drift so
far apart that they stop reading as one row. The panel's height is its content's,
and the block is centred.

---

## 3. Which Sections Are Optional

| Section | Present when |
| :--- | :--- |
| Acceptance banner | `NUO_FORCE_PRE_ATTACH` mounted the fixture |
| Attestation failure | the last decision could not be persisted |
| Workspace path | the offered set contains `TrustDomain::UserAssets` |
| Headline | always (one line per reason class present) |
| Decision band | always, unless a decision is in flight |
| Consequence | unless the terminal is too short |
| Footer | always, unless a decision is in flight |

Degradation order on a short terminal: context line first, then the consequence.
The decision, its reasons, and the keys that answer it survive every geometry.

---

## 4. The Row

| Part | Source | Tone |
| :--- | :--- | :--- |
| selection marker | `ChoiceMarker::Checkbox` | `ok()` when ticked, `dim()` when not, band contrast when pointed at |
| asset name | `TrustDomainView::label` ← `TrustDomain::label` | `fg()`, bold while pointed at |
| status | `TrustStatus::token` | `TrustStatus::tone(&theme)` |

The status is **right-aligned to the band's trailing edge**, so statuses form one
column the eye can scan vertically. The token vocabulary is closed, lowercase, and
unbracketed:

| Status | Token | Tone | Meaning |
| :--- | :--- | :--- | :--- |
| `TrustStatus::Changed` | `changed` | `warn()` | the bytes you approved are not the bytes on disk |
| `TrustStatus::New` | `new` | `muted()` | never reviewed |
| `TrustStatus::Expired` | `expired` | `dim()` | lease lapsed; routine re-attestation |

Brackets are forbidden here: `[ROLE]` is the badge slot and `[N queued]` the count
slot (`docs/reference/tui/visual-language.md` §3), so a bracketed status is
misread as one of those.

---

## 5. The Footer

| Key | Label | Present when |
| :--- | :--- | :--- |
| `Enter` | `trust` | always (primary) |
| `Esc` | `skip` | always |
| `↑↓` | `move` | more than one row offered |
| `Space` | `toggle` | more than one row offered |

What `Esc` leaves behind is stated by the consequence line directly above it, so
the key names the act rather than re-describing its outcome — which is also what
keeps a four-affordance footer inside the panel instead of spilling into
`F1 more`.

A **single-asset gate is not multi-select**, and the footer is the reason it
matters: `Space` is not advertised for one row, so with the question still
declared multi-select the key would be a live toggle nobody was told about —
pressing it would deselect the only asset and the next `Enter` would dispatch a
*refusal* of the thing the user meant to grant. `gate_request` therefore sets
`multi_select = rows.len() > 1`, and the single row carries no checkbox.

---

## 6. The Model

The surface reuses `QuestionModel` for navigation and selection, opened with
`QuestionModel::open_closed`:

- a **closed** question has no synthetic "Other" free-text row, in the state
  machine as well as in the paint — so navigation cannot reach a row the surface
  does not draw;
- an **open** question (`QuestionModel::open`, the AI-authored `ask_user` shape)
  keeps it, because "none of these, here is my answer" has to be typeable.

`answer_to_domains(answers, offered)` resolves returned labels against the
offered set, so an answer can never grant or refuse authority the user was not
shown.

---

## 7. Invariants

| Invariant | Meaning | Enforced by |
| :--- | :--- | :--- |
| `[INV-TRUST-10]` | A painted status is a value; no consumer parses a label | `trust_gate::tests::the_label_is_the_plain_asset_name`, `snapshot_tests::pre_attach::the_painted_gate_carries_no_status_syntax_in_the_asset_name` |
| `[INV-TRUST-11]` | An answer maps through the offered set | `trust_gate::tests::an_answer_naming_an_unoffered_asset_grants_nothing` |
| `[INV-TRUST-12]` | Sections separated by area; statuses share a column | `the_painted_gate_separates_sections_by_area_not_by_a_rule`, `the_painted_gate_aligns_statuses_into_one_column` |
| `[INV-TRUST-13]` | A footer advertises only keys that exist | `the_painted_gate_advertises_only_keys_that_exist` |
| `[INV-TRUST-14]` | A closed question has a closed state machine | `question_model::tests::a_closed_question_never_parks_the_highlight_on_an_unpainted_row` |

---

## 8. Acceptance Fixture

`NUO_FORCE_PRE_ATTACH=1` (or `true`/`yes`/`on`) mounts a synthesized snapshot with
every domain awaiting review, so the full six-row composition can be inspected
without preparing a workspace. The surface paints an extra banner naming the env
var, because a decision there persists against the real workspace.
