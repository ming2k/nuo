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
| Peer enumeration (same rank) | two-column gap, no glyph | `JOIN_ENUMERATE_COLS = 2` | `> turn 3  glm-5.3 xhigh  14:22` (turn band) |
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

---

## 4. Glyph Vocabulary

Standalone glyphs (from `design.rs` and `primitives.rs`):

| Glyph | Token | Use |
| :--- | :--- | :--- |
| `<` | `USER_MESSAGE_GUTTER_GLYPH` | User-message gutter |
| `>` | `AI_OUTPUT_LEAD_GLYPH` | Assistant-output lead |
| `▎` | `QUOTE_GUTTER_GLYPH` | Quoted content gutter |
| `↳` | `QUESTION_ANSWER_GLYPH` | Answer marker under a question |
| `│` / `┃` | — | Band borders (thin / thick identity bar) |
| `▾` / `▸` | — | Expanded / collapsed disclosure markers |

---

## 5. Non-Goals

- **No emoji in chrome.** Per `[INV-TUI-12]` (ADR-0063), execution footers and diagnostic statuses must not contain decorative emoji; status is conveyed by glyphs, tone, and words (`exit 0`, `timed out after 60s`).
- **No mixed separators.** A label uses the join form matching its relationship and nothing else.
- **No punctuation soup.** If a label reads as an arbitrary run of `·`, `|`, `/`, and `—`, it is a defect.
