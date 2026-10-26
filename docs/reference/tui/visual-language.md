# TUI Visual Language Reference

Factual reference for the text-join grammar `nuo-tui` uses to compose labels. The rule is stated in `crates/nuo-tui/src/design.rs`: keep labels free of punctuation soup.

This page is cited directly by `crates/nuo-tui/src/design.rs` (the `JOIN_*` token block).

---

## 1. The Rule

The design.rs comment block records the governing principle:

> Keep labels free of punctuation soup. Atomic values use ordinary spaces, peer metadata uses a two-column gap, secondary measures use parentheses, cause/reason uses an em dash, and hierarchy uses a breadcrumb.

Each semantic relationship has exactly one join form. There is no free mixing of separators.

---

## 2. Join Tokens

| Relationship | Form | Token / symbol | Example |
| :--- | :--- | :--- | :--- |
| Atomic values | ordinary spaces | — | `os_type=linux arch=amd64` |
| Peer enumeration (same rank) | two-column gap, no glyph | `JOIN_ENUMERATE_COLS = 2` | `› turn 3  glm-5.3 xhigh  14:22` (turn band) |
| Secondary → measure | parentheses | — | `4.2k (7%)` |
| Cause / reason | em dash | — | `locked — provider refuses this account` |
| Hierarchy (container › member) | breadcrumb | `JOIN_BREADCRUMB = " › "` | `round 3 › turn 2` |

`JOIN_BREADCRUMB` mirrors `primitives::BREADCRUMB_SEP`, which drives the head band's drill-in stack (`‹ thread › subagent #1 › diff`). The affordance gap in `components/keycap.rs` (`"  "`, width 2) is deliberately the same width as `JOIN_ENUMERATE_COLS`.

---

## 3. Rules by Context

### Peer enumeration

Same-rank peers are separated by pure whitespace (a two-column gap), never by a glyph like `·` or `|`. Consumers include the activity strip, turn band, and skills overlay.

### Hierarchy and drill-in

Nested navigation uses the breadcrumb separator ` › ` (U+203A). Front-truncation preserves the trailing context with a leading `... › `. This is the head band's SceneBar right side and the modal header's breadcrumb.

### Secondary measures

Numbers that qualify a primary value are parenthesized rather than dash-joined — e.g. the activity strip's `({clause})`.

### Cause and reason

A reason that explains a state follows an em dash, keeping the state word readable first (e.g. a locked model row leads with the provider's own verdict).

### State tokens

A persistent state is written as a **closed-vocabulary, lowercase, unbracketed token**, never as a sentence or a capitonym:

| Property | Rule | Counter-example |
| :--- | :--- | :--- |
| Form | One word, lowercase, no brackets (`auto`, `open`) | `UNATTENDED`, `[AUTO]` |
| Polarity | Say what *is* the case, not what is not (`open`, not `unconfined`) | `UNCONFINED` (double negative) |
| Presence | Drawn **only** on deviation from the safe default; a healthy default costs no columns | Always-on flags that spend space precisely when nothing is wrong |
| Priority | Safety state outranks identity: a long identity yields (elides) rather than crowding a state token into a misleading prefix | `open` clipped to `op`, still painted in the warning tone |
| Meaning | Each token names the state it asserts, so it stands alone — no tooltip layer required | A token needing an out-of-band gloss ("something is permissive") |

Caps and brackets are already spoken for elsewhere in the chrome: `[ROLE]` is the badge slot, and `[N queued]` means a *count* in the footer stack. A state token that borrows either form is misread as one of them. A well-chosen token needs no gloss at all — if it would require a tooltip to be understood, the token is wrong, not under-explained; the authoritative long-form wording belongs to the slash-command catalog, never to chrome that is always on screen (`[INV-TUI-12]`, ADR-0063). Worked example: the `ThreadContextBar`'s `auto` / `open` posture pair (ADR-0062 `[INV-HEAD-07]`).

### In-Flight Running State Demarcation (ADR-0110 `[INV-TUI-RUNNING-01..04]`)

Running and in-flight operations must strictly demarcate active vitality from inert cancellation:
- **Active Accent Hue**: Running states are styled in `theme.info()` (or `theme.warning`) with bold modifier, never `theme.dim()`.
- **Dimmed Tone Restriction**: `theme.dim()` is reserved exclusively for cancelled, aborted, or inert locked items (`[INV-TUI-RUNNING-01]`). An active task must never appear dead or faded.
- **Progress Grammar**: Running headers adhere to the canonical grammar `{Lead} {Target} · running ({elapsed})`, freezing into immutable duration badges upon completion (`[INV-TUI-RUNNING-03]`).
- **Single Breathing Anchor**: Only the global `ActivityBar` carries animated breathing sweeps; individual leaf components limit liveness indication to at most a single-character discrete spinner frame (`⠋`) and elapsed duration counts (`[INV-TUI-RUNNING-04]`).

---

## 4. Glyph Vocabulary

Standalone glyphs (from `design.rs` and `primitives.rs`):

| Glyph | Token | Use |
| :--- | :--- | :--- |
| `›` | `INPUT_PROMPT_GLYPH` / `USER_MESSAGE_GUTTER_GLYPH` / `AI_OUTPUT_LEAD_GLYPH` | The canonical lead — composer, command-palette query, input injection, question "Other" field, dashboard console, provider search rows, completion-menu suggestions, and the user-message / assistant-turn transcript headers |
| `▎` | `QUOTE_GUTTER_GLYPH` | Quoted content gutter |
| `↳` | `QUESTION_ANSWER_GLYPH` | Answer marker under a question |
| `│` / `┃` | — | Band borders (thin / thick identity bar) |
| `▾` / `▸` | — | Expanded / collapsed disclosure markers |

The ASCII `<` / `>` transcript leads (and their "Unix stdin/stdout redirection" rationale) are retired: one canonical single guillemet now fronts every lead, per the ADR-0092 single-guillemet typography standard.

---

## 5. Non-Goals

- **No emoji in chrome.** Per `[INV-TUI-12]` (ADR-0063), execution footers and diagnostic statuses must not contain decorative emoji; status is conveyed by glyphs, tone, and words (`exit 0`, `timed out after 60s`).
- **No mixed separators.** A label uses the join form matching its relationship and nothing else.
- **No punctuation soup.** If a label reads as an arbitrary run of `·`, `|`, `/`, and `—`, it is a defect.
