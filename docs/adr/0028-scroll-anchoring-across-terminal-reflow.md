---
id: ADR-0028
title: "Scroll Anchoring Across Terminal Reflow: Preserve the Reading Position on Resize"
status: accepted
date: 2026-10-09
scope: tui/nuo-tui, presentation/layout, interaction/input, rendering/scroll
superseded_by: null
negative_knowledge: true
---

# 0028. Scroll Anchoring Across Terminal Reflow: Preserve the Reading Position on Resize

- Status: Accepted
- Date: 2026-10-09
- Deciders: Nuo Architecture Working Group
- Consulted: TUI, Interaction, and Layout maintainers
- Informed: System Architects
- Amends: [ADR-0022](0022-minimum-terminal-geometry-and-frozen-input-contract.md) §"Resume" (the unfreeze repaint now also re-anchors a manual reading position)
- Related: [ADR-0003](0003-autonomous-terminal-canvas-substrate-nuotc.md), [ADR-0011](0011-nuo-tui-presentation-and-nuo-server-container.md), [ADR-0020](0020-interactive-component-registry-single-source-of-truth.md)

---

## Context and Problem Statement

The transcript viewport stores its scroll position as a **raw content-line
offset** (`App::scroll`, a `u16`). That offset is meaningful only against the
`content_lines` count produced by the *current* wrap width: a "line" is one
wrapped row, and how many rows a message occupies depends entirely on the
terminal's column count. The same offset therefore addresses completely
different content at 80 columns than at 160.

When the terminal is resized, the retained grids reflow at the new width
(`nuotc::Terminal::resize_to`), and the TUI's `TerminalResized` handler does two
things (`nuo-tui/src/event_loop/actions.rs:248-261`):

1. `app.layout_height_cache.clear()` — drop the width-dependent height cache
   (every cached row count is now wrong).
2. If `!follow_bottom`, set `app.scroll_settle_pending = true` — request a
   measured settle pass so the next frame's `max_scroll` can be honored.

What the handler does **not** do is re-derive `app.scroll`. The raw offset is
carried across the width change verbatim and merely clamped to the new
`max_scroll` (`nuo-tui/src/event_loop/mod.rs:373-383`). The result is the drift
this ADR removes: a user scrolled into history, resizing the window between 80
and 160 columns, watches the viewport jump to unrelated content, because line
offset *N* at the old width and line offset *N* at the new width are different
places in the transcript. The larger the width delta (and the more long,
wrapping paragraphs precede the reading position), the worse the drift.

`follow_bottom` is unaffected — it re-pins to `max_scroll` after the reflow — so
the defect is specific to a **manual** (non-following) reading position, which
is exactly the position a reader cares about preserving.

## Decision Drivers

- **Reading continuity.** A resize is a window-management action, not a
  navigation action. It must not move the user somewhere else in the document.
- **Width independence.** The scroll position must survive a reflow, which means
  it cannot be stored as (only) a width-dependent quantity.
- **Reuse the existing seams.** The layout already has exactly one
  "the width changed" transition (`HeightCache::prepare`) and one measured
  settle pass (armed by `scroll_settle_pending`); anchoring should ride them
  rather than add a parallel resize path.
- **No steady-state cost.** Anchoring must be invisible on ordinary frames: it
  must not add per-frame work, must not fight the user's own scrolling, and must
  not perturb the height-cache/virtualization fast paths.
- **Preserve the frame budget ([ADR-0003](0003-autonomous-terminal-canvas-substrate-nuotc.md)).** Any extra layout work must be bounded to the resize itself, not spread across frames.

## Considered Options

- **Option 1 (chosen):** Capture the semantic identity of the content at the
  viewport top (anchored message id + row offset) on every unpinned pass, and,
  on the pass that follows a width change, resolve that anchor against the new
  layout back into a content-line offset before the frame commits.
- **Option 2:** Scale the offset proportionally (`scroll * new_lines / old_lines`,
  or `scroll / max_scroll`).
- **Option 3:** Anchor to the viewport **bottom** line instead of the top.
- **Option 4:** Do nothing / document it as expected ("resizing re-flows the
  transcript; scroll position may move").

## Decision Outcome

Chosen option: **Option 1.** It is the only option that restores the *same text*
under the viewport top after an arbitrary reflow (proportional scaling is a
per-frame approximation that is wrong wherever wrapping is uneven — which is
every real transcript), and it does so by reusing the two seams the layout
already owns rather than adding a resize-specific code path.

Concretely:

- **Anchor capture.** `Stream::dispatch` records, for the first message whose
  rows extend past the viewport top, a `ScrollAnchor { message_id, row_offset }`
  (`nuo-tui/src/layout/mod.rs`). `message_id` is
  `TranscriptMessage::id` — process-unique and stable across the per-frame
  clone — so it survives the transcript rebuild; `row_offset` is how many of that
  message's rows already sit above the viewport top. Capture runs on every
  unpinned pass, touches no pixels, and stops at the first match.
- **Anchor arming.** `HeightCache::prepare(width)` is the single width-change
  seam. On a width change it promotes the last captured anchor to a `pending`
  request and clears the now-invalid height entries. A resize with no
  intervening render cannot double-arm (`take`).
- **Anchor resolution.** A pass with a `pending` anchor **skips virtualization**
  and walks the full transcript, resolving the anchor's message to its new
  start line plus the row offset (clamped to the message's re-measured height).
  The full walk is required: a large reflow can move the anchored content far
  beyond the window the stale raw offset would select, and a windowed walk would
  never measure it, silently failing the resolve. Off-screen messages still
  advance via the height cache (no re-wrapping), so the cost is one O(messages)
  pass per resize; virtualization resumes on the next frame.
- **Anchor application.** The resolved offset is published on the cache; the
  event loop's settle branch (`nuo-tui/src/event_loop/mod.rs`) consumes it with
  `take_resolved()` and applies it to `App::scroll` **before** the `max_scroll`
  clamp and **before** committing the staged grid — so the terminal only ever
  sees the correctly anchored frame, never an intermediate drift.

### Invariants & Behavioral Boundaries

- `[INV-ANCHOR-01]` **Re-anchor only on a width change.** Anchoring runs exactly
  once per actual wrap-width change, armed by `HeightCache::prepare`. A frame at
  a steady width must never re-anchor — that would fight the user's own
  scrolling. `take_resolved()` is cleared on read so a single resize settles
  once.
- `[INV-ANCHOR-02]` **Anchor identity is widen-independent.** The stored position
  is `(message id, row offset)`, never a bare content-line offset. A raw offset
  may be a resolved *output*, never the stored *identity*.
- `[INV-ANCHOR-03]` **Resolve walks the whole transcript.** The pass armed by a
  width change MUST walk all messages, not just the virtual window the stale
  offset selects; the anchored content may be far outside that window after a
  large reflow.
- `[INV-ANCHOR-04]` **A vanished anchor degrades, never crashes.** If the
  anchored message is gone (compaction, a folded command echo), resolution
  yields nothing and the existing offset is left untouched; a missing anchor is
  never a panic or a jump to line 0.
- `[INV-ANCHOR-05]` **Follow-bottom is untouched.** `follow_bottom == true`
  re-pins to `max_scroll` after the reflow; anchoring applies only to a manual
  position.
- `[INV-ANCHOR-06]` **No steady-state cost.** Capture runs in `O(1)` after the
  first match on ordinary passes; no anchor work may run on a frame whose width
  is unchanged, and virtualization must remain in force on every non-resolve
  pass.

### Positive Consequences

- A manual reading position survives a column resize: the same text stays under
  the viewport top instead of jumping to unrelated content.
- The mechanism reuses the two seams the layout already owns (the width-change
  transition and the measured settle pass), so there is no parallel resize path
  to keep in step with the main one.
- Anchoring is captured from the position the user actually sees, so it is
  automatically correct for every message kind (prose, tool step, reasoning,
  subagent, notice, command) without per-kind handling.

### Negative Consequences & Trade-offs

- One extra `O(messages)` layout pass per resize (virtualization suspended for
  that single pass). Heights still resolve through the cache, so off-screen
  messages are advanced without re-wrapping; the next frame is fully
  virtualized again.
- A new pair of `HeightCache` fields (`anchor`, `pending`) and two small
  `Stream` outputs. The state lives on the cache rather than `App` because the
  cache already owns the one authoritative width-change transition; the cost is
  that anchor state is cleared whenever the cache is (a harmless, safe reset —
  an unknown anchor simply leaves the offset untouched).
- The anchor is exact at message granularity; a resize that reflows the *interior*
  of the single anchored message can still shift within that message by a row or
  two (the row offset is clamped to the new height). This is a deliberate
  precision/robustness trade: message-level identity never misplaces the view
  wholesale, which is the failure being fixed.

## Rejected Alternatives & Negative Knowledge

### Option 2 (Rejected): Proportional scaling of the offset
- Why considered: One line of arithmetic (`scroll * new_lines / old_lines`),
  no new state.
- Why rejected: Wrapping is unevenly distributed across a transcript, so the
  offset-to-content mapping is not linear in width. Proportional scaling is a
  per-frame approximation that produces a **different** wrong answer than the
  raw offset, not a correct one — it still moves the user to unrelated content
  whenever long and short messages are mixed, which is every real transcript.
  It buys nothing over doing nothing except the illusion of a fix.

### Option 3 (Rejected): Anchor to the viewport bottom line
- Why considered: When reading the tail of a long reply, the bottom edge is
  where attention rests, so pinning the bottom could feel more stable.
- Why rejected: The top edge is the fragment the user re-orients by — the first
  thing re-read after a scroll — so it is the correct anchor for continuity. A
  bottom anchor also interacts badly with `follow_bottom`: the two would fight
  over the same edge near the end of the stream. The top edge is stable, has no
  competing consumer, and matches the mental model ("I am reading from here").

### Option 4 (Rejected): Do nothing; document resize as shifting scroll
- Why considered: Zero code; the behavior is "only" a scroll jump, not data
  loss.
- Why rejected: It is the reported defect, and it is not rare — dragging a
  terminal's edge emits many resize events in quick succession, so the reading
  position is repeatedly knocked around exactly when the user is mid-task.
  Calling an ergonomic regression "expected" neither fixes it nor satisfies the
  reading-continuity driver.

### Re-capturing the anchor from the pre-resolve `viewport_top` (Rejected)
- Why considered: Simplest — the resolve pass could store whatever anchor its own
  (not-yet-settled) `viewport_top` implies, so the next resize has *something*.
- Why rejected: On the resolve pass `viewport_top` is the **stale** offset the
  whole mechanism exists to replace, so re-capturing from it would store a
  subtly wrong anchor for the next resize. The resolve path therefore derives the
  stored anchor from the *resolved* position, so the anchor always describes the
  settled layout (`[INV-ANCHOR-02]`).

## Compliance

- `HeightCache::prepare` remains the single width-change seam; any new
  width-dependent state must arm/reset there rather than introduce a second
  resize notification.
- A regression test must assert all of: a steady width never resolves
  (`[INV-ANCHOR-01]`); a reflow re-derives a different offset and restores the
  same content at the top; and the resolve finds an anchor that lies far outside
  the stale offset's virtual window (`[INV-ANCHOR-03]`).
- A missing anchor (compacted/folded message) must leave the offset untouched —
  covered by the degrade path in `Stream::resolve_anchor`, not an `unwrap`.

## Links

- Related ADRs: [ADR-0003](0003-autonomous-terminal-canvas-substrate-nuotc.md) (retained canvas, coalescing/frame budget),
  [ADR-0011](0011-nuo-tui-presentation-and-nuo-server-container.md) (TUI presentation split),
  [ADR-0020](0020-interactive-component-registry-single-source-of-truth.md) (presentation single-source discipline),
  [ADR-0022](0022-minimum-terminal-geometry-and-frozen-input-contract.md) (minimum geometry and the frozen-input contract).
- Related code: `nuo-tui/src/model/layout.rs` (`ScrollAnchor`),
  `nuo-tui/src/layout/mod.rs` (`Stream::capture_anchor` / `resolve_anchor`),
  `nuo-tui/src/render/mod.rs` (`HeightCache::prepare` / `pending_anchor` /
  `set_resolved` / `take_resolved`, the resolve-pass full walk),
  `nuo-tui/src/event_loop/mod.rs` (settle branch applies the resolved offset),
  `nuo-tui/src/event_loop/actions.rs:248-261` (`TerminalResized`).
- Tests: `nuo-tui/src/snapshot_tests.rs`
  (`resize_reanchors_the_manual_viewport_top`,
  `steady_width_captures_but_never_resolves`,
  `resize_resolves_an_anchor_far_outside_the_stale_window`).
