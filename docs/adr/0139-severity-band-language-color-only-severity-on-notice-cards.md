---
id: ADR-0139
title: "Severity-Band Language: Color-Only Severity on Notice Cards, Abolishing the Triangle Glyph and Text Badges"
status: accepted
date: 2026-12-17
scope: presentation/nuo-ui-primitives, presentation/nuo-view, host/nuo (tui/overlays)
superseded_by: null
negative_knowledge: true
---

# 0139. Severity-Band Language: Color-Only Severity on Notice Cards

- Status: Accepted
- Date: 2026-12-17
- Deciders: Nuo Architecture Working Group
- Amends: [ADR-0134](0134-two-track-notice-architecture-sovereign-notice-center-dialog-and-footer-dock-eradication.md) (card header rendering vocabulary: severity conveyed by a color band, not `▲`/`ℹ` glyphs + `[ERROR]`/`[WARN]`/`[OK]` text badges)

---

## Context and Problem Statement

The notice surfaces inherited two overlapping severity vocabularies:

1. **The notice-center dialog** (`crates/nuo/src/shell/overlays/notices.rs`)
   renders each card header as `{glyph} [{ERROR|WARN|OK|INFO}] {topic}` — a
   glyph (`▲`/`✗`/`✓`/`ℹ`), a bracketed text badge, **and** a severity-colored
   foreground, all three saying the same thing. The `▲` is a raw string literal
   with **no** ASCII fallback in the `GlyphSet` (it is not in
   `ASCII_FALLBACKS`, so a VT100 terminal renders `?`).
2. **The transcript notice head** (`crates/nuo-view/src/components/notice.rs`)
   paints `! `, `▲ `, or `ℹ ` as its lead, again as raw literals.

The user's critique is precise: the triangle is a redundant glyph marks a
channel that terminal cards already own with **color**. Cards can convey
severity with a tinted band (background) plus a tone-foreground topic — exactly
the diff-band language the theme already uses (`diff_add_bg`/`diff_del_bg` +
highlight pair) — and can drop both the glyph and the bracketed text badge.

This is a **clean break** from the legacy badge vocabulary, not a gradual
replacement: the dialog and transcript heads stop painting glyphs/`[ERROR]`
text entirely. The **toast bubble** keeps its icon — a floating pill has no card
surface to tint its whole body, and the bubble is a different layer with a
different history (borderless already); the toast's warning icon moves from the
raw `▲` literal to the `GlyphSet` so VT100 gets a real fallback.

## Decision Drivers

- **One severity channel per surface.** A card conveys severity with color
  (band + tone-foreground); the transcript conveys it with tone-colored text and
  a thin left rail. Never two glyphs + a badge + a color for the same fact.
- **Clean break, zero legacy burden** ([ADR-0059](0059-canonical-thread-persistence-schema-and-zero-legacy-clean-break.md)): the legacy glyph/text
  badge vocabulary is deleted, not layered behind a config knob.
- **Terminal-proof**: every remaining literal must round-trip through
  `GlyphSet`/`ASCII_FALLBACKS`. The raw `▲` dies.
- **Design tokens, not magic colors**: the band is derived from the theme's own
  severity accents with the existing `mix()` helper, so it retunes with the
  palette and stays legible in ANSI-16/monochrome (where `app_bg` is `Reset`,
  the band falls back to the theme's raised surface rather than inventing a
  color).

## Architectural Decisions

### 1. `SeverityBand` — a derived tint pair in `nuo-ui-primitives`

A new small type in the theme module:

```rust
pub struct SeverityBand {
    pub bg: Color,
    pub fg: Color,
}
```

`Theme::severity_band(accent: Color)` derives both from the theme's own
tokens, taking the already severity-resolved accent tone (renderers resolve
severity → `theme.err()` / `theme.warn()` / `theme.ok()` / `theme.info()`
through the existing accessors, so no second severity enum enters the theme):

- **bg**: `mix(accent, app_bg, 0.86)` — a subtle tint of the severity accent
  over the app background (dark schemes get a faint colored wash; light schemes
  get a pastel). When `app_bg` is `Reset` (ANSI-16/monochrome) the band falls
  back to `theme.raised()` with the accent preserved as the foreground, so a
  palette whose accents are real ANSI colors (ANSI-16) still distinguishes
  severities by color.
- **fg**: `mix(accent, text, 0.30)` — the accent pulled toward the text tone,
  readable at small sizes.
- **marker**: `""` when the theme can distinguish severities by color
  (`Theme::severity_is_color_distinguishable`), otherwise the color-independent
  severity token (`"error"`/`"warn"`/`"ok"`/`"info"`). This is what keeps the
  glyph removal from regressing the **monochrome/VT100** profile, where every
  accent collapses to `Color::Reset`: color there carries *nothing*, so the
  deleted `!`/`▲`/`ℹ` was genuinely the only working signal. The surfaces append
  the marker to the topic (`topic error`) so severity survives as text where it
  cannot survive as color.

The dialog resolves `LineStatus → (accent, marker)` (Failed→`err()`/`"error"`,
Warn→`warn()`/`"warn"`, Ok→`ok()`/`"ok"`, Info→`info()`/`"info"`) and the
transcript resolves `NoticeSeverity → (accent, marker)` the same way, so one
band type serves both surfaces.

### 2. Dialog card header: band pill, no glyph, no badge

The header row becomes:

```text
▌ [ topic ] — band-pill: bg = band.bg, fg = band.fg (+ BOLD)     at HH:MM
```

- The topic pill keeps its `raised()` background as the *chip* surface, but the
  **band** is the severity signal: `bg = band.bg, fg = band.fg`, bold.
- The `[ERROR]`/`[WARN]`/`[OK]`/`[INFO]` text and the leading glyph (`▲`, `✓`,
  `✗`, `ℹ`) are **deleted** from the dialog.
- The selected-row bar (`▌`) and the right-aligned `at HH:MM` stay unchanged;
  the empty-state text stays unchanged.

### 3. Transcript notice head: tone-colored left rail, no glyph

The transcript head currently paints `! trust` / `▲ trust` / `ℹ trust`. It
becomes:

```text
▎ trust   (rail + topic, in severity tone, BOLD)          at HH:MM
```

- The lead glyph is replaced by a **thin left rail** (`▎` — in the glyph set's
  border/block family) painted in the severity tone, followed by the topic in
  the same tone. Severity is carried by color alone, matching the card.
- The sentinel/layout-map contract (head is chrome, body is content) is
  unchanged; only the head's text changes.
- `translucent ▎` has an ASCII fallback (the `▎` maps to `#` per
  `ASCII_FALLBACKS`'s block range), so VT100 renders a real marker, not `?`.

### 4. Toast keeps its icon; the triangle dies everywhere

The toast is a borderless pill with a neutral elevated surface; its icon is its
identity, and the severity accent is already carried by the icon color through
`ToastKind::color()`. The toast therefore keeps its icon — but the **triangle
dies here too**: the `▲` literal (a raw mark with an ASCII-`?` footgun, and the
exact glyph the operator flagged) is replaced by `!`, a universal character that
needs no fallback and stays severity-colored through the same branch. `✓`/`✗`
for ok/error keep their `GlyphSet` round-trip; `ℹ` keeps its existing `[i]`
ASCII branch.

## Subsystem Boundaries & Structural Changes

- **`nuo-ui-primitives`**: add `SeverityBand` + `Theme::severity_band()` +
  tests (dark, light, ANSI-16/monochrome fallback).
- **`nuo-view`**: `components/notice.rs` head rendering → rail + tone topic.
- **`nuo` (host)**: `overlays/notices.rs` card header → band pill; delete the
  `{glyph} {badge}` match; `overlays/toast.rs` warning icon → glyph-set.
- **Tests**: rewrite only the assertions that test the *legacy glyph/badge
  vocabulary*; every semantic assertion (topic present, body indent, selection
  geometry, fault counting, dwell, unread badge) stays green unchanged.

## Normative Invariants Established

- `[INV-NOTICE-09]` **Color-Only Card Severity**: Notice-card surfaces (dialog,
  transcript head) MUST NOT paint a legacy severity glyph (`▲`/`ℹ`/`✗`/`✓`) or a
  bracketed `[ERROR]`/`[WARN]`/`[OK]`/`[INFO]` text badge; severity is conveyed
  by the `SeverityBand` tint (dialog) / tone-rail (transcript). Where the theme
  cannot distinguish severities by color
  (`Theme::severity_is_color_distinguishable` is `false`, i.e. monochrome), the
  band's ASCII `marker` word (`error`/`warn`/`ok`/`info`) MUST appear instead, so
  removing the glyph never makes severity invisible on a color-blind profile.
  Enforcement: the `▲`/`ℹ`/`[ERROR]`/`[WARN]` literals are deleted from those
  renderers; one test asserts the band background is actually painted and the
  legacy markers are absent, and a second asserts a monochrome theme still names
  `error`/`warn` in the header text.

## Consequences

- **Positive**:
  - One severity channel per surface; the redundancy the user flagged is gone.
  - The raw `▲` literal (the ASCII-`?` footgun) is eradicated from the cards.
  - The band retunes with the palette: it is derived from the theme's own
    accents, never a magic `Color::Rgb` literal. ANSI-16 keeps color (its
    accents are real ANSI hues); monochrome/VT100 — where color carries nothing
    — falls back to an explicit textual severity marker, so the profile the
    deleted glyph used to serve is not silently regressed.
  - The transcript head reads cleaner and stays a one-line sentinel.
- **Negative & Trade-offs**:
  - Color-only severity is weaker for colorblind users than glyph+badge. The
    mitigation is structural: (a) the topic pill still carries the *word* of the
    topic, (b) the toast keeps its icon, and (c) the band's luminance step is
    kept wide enough that severity is still visible as a surface difference, not
    only a hue. A user-configurable "add glyphs back" knob is rejected (below).
  - The transcript head, being one row, cannot carry a full background band; it
    uses the rail + tone, which is the same color channel, so the two surfaces
    stay consistent.

## Rejected Alternatives & Negative Knowledge

### Alternative A: Keep the glyph and badge, add background color too (Rejected)

- **Why considered**: Maximum redundancy is maximum legibility.
- **Why rejected**: It is exactly the clutter the user rejected. Three channels
  for one fact (glyph + text badge + color) is not "more legible", it is noise;
  and the card header budget is fixed, so every extra channel costs topic width
  or truncates earlier.

### Alternative B: A config knob `notices.severity = band | badge | both` (Rejected)

- **Why considered**: Lets each user choose their legibility tradeoff.
- **Why rejected**: Per ADR-0121 Alternative F and the repository's clean-break
  standard ([ADR-0059]), a knob preserves the known-inferior path, doubles the
  render matrix, and lets two users describe the same surface differently. One
  right answer, no knob.

### Alternative C: Full-width background band on every dialog card row (Rejected)

- **Why considered**: Maximum color surface.
- **Why rejected**: A full card body wash is heavy: it stains the whole detail
  area and makes the expanded JSON detail harder to read. The band lives on the
  header pill and the thin left rail — a strong signal without a heavy wash.
  This matches the theme's own diff-band language (highlight pair on a band,
  not a full-row paint).

## Links

- Amends: [ADR-0134](0134-two-track-notice-architecture-sovereign-notice-center-dialog-and-footer-dock-eradication.md)
- Related: [ADR-0121](0121-notice-log-residency-retained-disclosure-record-and-bubble-as-pointer.md),
  [ADR-0116](0116-content-residency-and-surface-layer-topology.md) (layer routing),
  [ADR-0059](0059-canonical-thread-persistence-schema-and-zero-legacy-clean-break.md) (zero legacy burden)