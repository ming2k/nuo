# The Terminal UI: A Rendering Philosophy

This article explains why the `nuo-tui` terminal interface is built the way it is. It is an orientation, not a specification. For the authoritative layout tokens, modal presets, and input pipeline, consult the [Terminal UI Reference](../reference/tui/index.md).

---

## A Frame Is a Pure Function of State

The design begins with one commitment: **the frame is a pure function of state**. Anything that changes state — a streamed token, a permission request, a mouse drag — shows up on the very next frame with no manual invalidation. There is no "tell the widget to redraw" step, because there are no retained widgets to tell.

This is what makes the renderer cheap to reason about. A bug in what is on screen is a bug in either the state or the pure function, never in a hidden cache that failed to invalidate.

---

## The Engine Diffs; the UI Does Not

The retained-mode canvas engine (`nuotc`) holds the authority over the desired visual state (see [ADR-0003](../adr/0003-autonomous-terminal-canvas-substrate-nuotc.md)). It is a retained cell grid with dirty tracking and a **back/front diff**. Each frame, the UI produces a fresh grid; the engine compares the back grid against the front grid and walks only the dirty rows, repainting from each row's dirty column onward.

Because diffing is centralized in the engine, the UI never tracks "what did I draw last time". Per-frame rebuild keeps invalidation trivial: there is nothing to invalidate.

---

## Zones, Not Widgets

The viewport is divided into a small number of sovereign vertical zones rather than a tree of nested widgets:

- a **head band** of up to three tiers (TabBar / SceneBar / ThreadContextBar),
- a polymorphic **content body**,
- a declarative **footer stack** of rows.

Each zone owns a fixed slot and a declared height. A hidden row collapses to height 0 and the rows below it move up by construction — there is no separate "relayout" path, because the layout is recomputed whole each frame.

This is why the footer is a *list of rows* rather than a set of conditionally-positioned bars: the row order *is* the layout, and a new bar is one more entry.

---

## The Chrome Conveys Posture, Not Decoration

The chrome is deliberately restrained. Status is conveyed by glyphs, tone, and terse words (`exit 0`, `timed out after 60s`), never decorative emoji (`[INV-TUI-12]`, [ADR-0063](../adr/0063-bifurcated-shell-tool-presentation-out-of-band-status-and-clean-stream-topology.md)). The head band carries identity (which tab, which scene, what role and safety posture); the footer carries interaction (activity, input, model). Every pixel is load-bearing.

Two consequences follow for state that is *persistent* rather than transient. First, a healthy default is silent: the `ThreadContextBar` draws no safety token at all until the session actually deviates from interactive confirmation or from workspace-confined file tools (`[INV-HEAD-07]`, [ADR-0062](../adr/0062-polymorphic-scenebar-projection-sovereign-thread-context-and-consolidated-activity-strip.md)). Second, the element that may shrink is the one anchored to the edge — the workspace path — so state changes and narrow terminals degrade the *path*, never the governance cluster beside `[ROLE]`. A path that can no longer be shortened honestly is omitted rather than drawn as an ambiguous fragment.

---

## Degrade, Don't Panic

When the terminal is smaller than the usable minimum, the renderer does not draw a broken layout or risk an integer-underflow panic. A single authority (`below_minimum`) swaps the whole UI for a centered notice and freezes input behind it, so nothing changes out of sight. Honesty about the constraint beats a garbage frame.

---

## Where to Go Next

- [Terminal UI Reference](../reference/tui/index.md): the layouts, tokens, modals, and pipeline in full.
- [Lifecycle and Action Semantics Reference](../reference/lifecycle-and-action-semantics.md): the cross-domain action matrix.
