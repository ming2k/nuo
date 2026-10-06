---
id: ADR-0022
title: "Minimum Terminal Geometry and the Frozen-Input Contract"
status: accepted
date: 2026-10-05
scope: tui/nuo-tui, presentation/layout, interaction/input, empty-state
superseded_by: null
negative_knowledge: true
---

# 0022. Minimum Terminal Geometry and the Frozen-Input Contract

- Status: Accepted
- Date: 2026-10-05
- Deciders: Nuo TUI Working Group
- Consulted: Presentation, Interaction, and Empty-State maintainers
- Informed: System Architects
- Amends: the terminal-size guard of `draw_transcript` (the "terminal too small" notice) and the empty-state hero of the empty session

---

## Context and Problem Statement

The TUI already refuses to lay itself out below a minimum geometry: `draw_transcript` short-circuits and paints a centered "Terminal too small" notice instead of the chrome. But the guard was **render-only**, and it was sized against a width that could not actually present the product's own welcome screen:

1. **`MIN_TERMINAL_COLS` was 40, smaller than the 42-column built-in wordmark.** At the minimum width the empty-state hero would wrap the logo, which both deforms the ASCII art and violates the wrap-independent height accounting the app loop relies on for scroll.
2. **Nothing froze.** Below the minimum the event loop kept running: keystrokes were applied to an invisible composer, modals opened behind the notice, the spinner/carousel kept animating, and daemon mutations kept repainting state the user could not see. Resizing back up revealed a burst of invisible edits and navigation — the terminal was hidden, but *state was not frozen*.
3. **The logo had only a ceiling.** `MAX_LOGO_COLS` / `MAX_LOGO_ROWS` clamped a giant paste, but there was no floor and no fit test: a cramped or oversized mark could wrap, clip, or shrink the first impression instead of gracefully yielding to the carousel.

The requirement is a real minimum window plus a real post-minimum behavior: below it, the UI must be *frozen* — no input reaches application state, no animation advances, and only the notice is painted — and the empty-state hero must drop the wordmark when there is not enough room for it rather than deform it.

---

## Decision Drivers

- **A single geometric authority.** The renderer and the event loop must agree on "below minimum" from one predicate, or one will freeze what the other still paints.
- **No invisible mutation.** A user must never be able to change state they cannot see; every key that lands below the minimum is dropped, not buffered or applied.
- **Never lose daemon work.** Freezing the UI must not drop a streaming round or a pending permission — daemon-originated mutations keep applying while frozen.
- **An escape hatch.** Ctrl-C must still quit while frozen; a user must never be trapped by a terminal they cannot resize.
- **No deformation of the wordmark.** The logo is shown only when it fits; otherwise the carousel carries the welcome.
- **Durable, accurate carousel copy.** Every rotating hint must describe a capability that exists and a chord that fires in the shipped build.

---

## Considered Options

- **Option 1 (Chosen)**: A single `below_minimum(w, h)` predicate; minimum `44 × 12`; a loop-level freeze branch that applies daemon mutations, blocks user events (except resize and Ctrl-C), pauses animation, and paints only the notice; a `plan_hero` fit test plus `MIN_LOGO_COLS` × `MIN_LOGO_ROWS` floor and the `MAX_LOGO_COLS` × `MAX_LOGO_ROWS` ceiling; refreshed carousel pages.
- **Option 2**: Keep the render-only guard; extract the notice into its own widget and rely on it alone. No event blocking.
- **Option 3**: Buffer input while frozen and replay it on restore.
- **Option 4**: Freeze everything, including daemon mutation application.

---

## Decision Outcome

Chosen option: **"Option 1"**.

### Mechanism

- **One geometry authority.** `design::below_minimum(width, height)` is the single predicate. `MIN_TERMINAL_COLS = 44` is the width at which the 42-column built-in wordmark fits with one column of margin each side; `MIN_TERMINAL_ROWS = 12` keeps the existing "footer + a couple of transcript rows" rationale. `draw_transcript` uses the predicate to swap the chrome for the notice; the event loop uses it to freeze.
- **Loop-level freeze.** Every iteration re-reads the live geometry. Below the minimum, `run_app_loop` enters a dedicated branch that (a) drains and applies `AppMutation` (daemon state stays current), (b) discards user-originated clipboard results, (c) paints the frozen notice (`render::draw_too_small`) on entry and on each geometry change, and (d) blocks all input except a `Resize` and Ctrl-C (`frozen_event_passthrough`). No spinner, carousel, elapsed timer, scroll clamp, or staging pass runs, so no animation advances and no state moves.
- **Mid-batch re-check.** If a `Resize` inside an already-draining input batch drops the terminal below the minimum, the batch re-evaluates and blocks the remaining events, so a single tick cannot mutate state before the notice is painted.
- **Resume.** Leaving the freeze clears the paint latch and forces one full repaint; the resize had already invalidated the retained grid, so the normal chrome reappears cleanly.
- **Logo fit, not scale.** `plan_hero(logo, area, guidance)` shows the wordmark only when it respects the `MIN_LOGO_COLS` × `MIN_LOGO_ROWS` floor, sits within `MAX_LOGO_COLS` × `MAX_LOGO_ROWS`, and fits the viewport without wrapping or clipping (width ≤ `area.width`; height + `HERO_LOGO_GAP_ROWS` + guidance ≤ `area.height`). Otherwise the hero drops the logo and centers the guidance line alone. `draw_empty_state` returns the rows it actually painted, so `content_lines` accounting can no longer drift from what is on screen.
- **Carousel refresh.** The rotating pages were rewritten to the shipped capabilities and bindings: `/` commands, mid-round `Enter` steer / `Tab` queue, `/btw`, `Ctrl-r` history, `/models`, `@` mentions, `Ctrl-l` command palette, `/sessions`. The stale `Ctrl-m` "switch models" page (no binding; collides with Enter without the Kitty protocol) and the "Enter queues it" default-mode claim were removed.

### Invariants & Behavioral Boundaries

- **[INV-TUI-MIN-01] One predicate owns "below minimum".** The frozen notice and the input freeze MUST derive from the same `below_minimum(width, height)` check; neither the renderer nor the loop may carry a private threshold.
- **[INV-TUI-MIN-02] Frozen means no user-state mutation.** While below the minimum, every key, mouse, and paste event MUST be dropped except a resize and Ctrl-C. Dropping is silent: blocked input is neither applied nor replayed later ([INV-AGENT-02] negative knowledge on buffering below).
- **[INV-TUI-MIN-03] Daemon state stays current while frozen.** Translator mutations MUST continue to be applied (and drained) while frozen, so a streaming round, permission request, or session switch is never lost; only the paint is suppressed.
- **[INV-TUI-MIN-04] The logo never deforms.** The hero MUST NOT wrap, clip, or truncate a logo to fit. If it does not fit, or is below the minimum floor, the wordmark is dropped and the guidance line is shown alone.
- **[INV-TUI-MIN-05] Painted height equals reported height.** `draw_empty_state` MUST report the rows it painted; scroll accounting may not use a separately derived height.
- **[INV-TUI-MIN-06] Advertised carousel chords are live.** Every keycap in the tour MUST correspond to a binding that fires in the shipped build, and every claim MUST match the default mode.

### Positive Consequences

- Below the minimum the process is genuinely frozen: no invisible edits, no background modal churn, no animation CPU. Resizing back shows exactly the state the session was in.
- The welcome screen is never broken by its own terminal guard: 44 columns guarantees the built-in wordmark fits, and short terminals get a clean centered carousel instead of a clipped logo.
- The empty-state height accounting is now honest (the renderer's return value), so an empty session cannot mis-pin scroll.
- The carousel no longer teaches dead chords (`Ctrl-m`) or a wrong default (`Enter` steers, it does not queue), reducing support confusion.
- One predicate and one notice make "below minimum" cheap to reason about and cheap to test.

### Negative Consequences & Trade-offs

- **44 columns raises the floor by four.** A 40–43 column terminal that previously rendered a (wrapped, broken) UI now shows the frozen notice. **Mitigation**: 40–43 columns could not present the built-in wordmark without deformation, and a user logo can still be supplied; the trade is deliberate — a clean notice beats a broken hero.
- **The freeze branch duplicates a small part of the loop** (channel draining, event dispatch for Ctrl-C). **Mitigation**: it reuses `process_one_event`, `frozen_event_passthrough`, and `draw_too_small`, so the only duplicated lines are the three `try_recv` drains; the tests pin the predicate and the geometry, not the branch shape.
- **`terminal.size()` is queried every iteration** (one `ioctl`). **Mitigation**: negligible against a frame; it is what keeps the freeze exact rather than one resize-event stale.
- **Ctrl-C still clears the composer if it is armed that way.** **Mitigation**: accepted as the quit escape hatch; the alternative (trapping the user) is worse.

---

## Rejected Alternatives & Negative Knowledge

### Option 3 (Rejected): Buffer input while frozen and replay on restore
- **Why considered**: it appears to satisfy "block events" without losing keystrokes.
- **Why rejected**: it produces *worse* invisible mutation, just deferred. A user who typed while the notice was up would watch a burst of text and navigation land the instant the window grew — state changing in response to input composed against a screen that no longer exists. It also invites an unbounded buffer on a pathological terminal. Dropping the input is the honest expression of "this surface is frozen".

### Option 4 (Rejected): Freeze everything, including daemon mutation application
- **Why considered**: the strongest reading of "state freeze" — literally nothing in `App` may change.
- **Why rejected**: it would stall the agent. A streaming round delivers deltas that must be folded (or the round's final snapshot can race a later one), and a pending permission that arrives while frozen would be invisible *and* unqueued. The daemon is not the user; keeping its state current while suppressing the paint preserves work and remains invisible. This is the distinction now recorded as [INV-TUI-MIN-03].

### Option 2 (Rejected): Render-only guard without event blocking
- **Why considered**: the smallest change; the notice already exists.
- **Why rejected**: it is precisely the bug this ADR fixes. Hiding the terminal does not freeze the state; the loop keeps mutating `App` behind the notice, and resizing back reveals the drift. A guard that only paints cannot make "frozen" true.

### Scaling the wordmark (Rejected during implementation)
- **Why considered**: shrinking or truncating the logo would let smaller terminals keep a brand mark.
- **Why rejected**: ASCII art does not scale without losing its shape, and truncation is the deformation the requirement forbids. Dropping the mark and letting the carousel carry the welcome is legible; a squashed logo is not.

### A private minimum in the event loop (Rejected during implementation)
- **Why considered**: the loop could track the size it last rendered and avoid an extra `terminal.size()` call.
- **Why rejected**: two thresholds drift. The renderer and the loop now share `below_minimum`, which is also the seam the tests exercise ([INV-TUI-MIN-01]).

---

## Links

- Implementation: `nuo-tui/src/design.rs` (`MIN_TERMINAL_COLS`, `MIN_TERMINAL_ROWS`, `below_minimum`), `nuo-tui/src/event_loop/mod.rs` (freeze branch, `frozen_event_passthrough`), `nuo-tui/src/render/mod.rs` (`draw_too_small`, too-small guard), `nuo-tui/src/empty_state.rs` (`plan_hero`, `HeroLayout`, `MIN_LOGO_*`, carousel pages).
- Related ADRs: [ADR-0003](0003-autonomous-terminal-canvas-substrate-nuotc.md) (retained terminal canvas and differential rendering), [ADR-0011](0011-nuo-tui-presentation-and-nuo-server-container.md) (TUI presentation decoupling).
- Related docs: `AGENTS.md` machine invariants ([INV-AGENT-01] negative knowledge, [INV-AGENT-02] context routing).
