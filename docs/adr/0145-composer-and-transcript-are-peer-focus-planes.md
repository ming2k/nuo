---
id: ADR-0145
title: "Composer and Transcript Are Peer Focus Planes: One Symmetric Crossing Chord, and Esc Never Navigates"
status: accepted
date: 2026-02-14
scope: tui/nuo, interaction/keyboard, tui/event-loop, tui/input, presentation/nuo-view, docs/reference/tui
superseded_by: null
negative_knowledge: true
---

# 0145. Composer and Transcript Are Peer Focus Planes: One Symmetric Crossing Chord, and Esc Never Navigates

- Status: Accepted
- Date: 2026-02-14
- Deciders: Nuo Architecture Working Group
- Consulted: Interaction Design, TUI Runtime, Presentation Substrate
- Amends: [ADR-0025](0025-scene-scoped-interrupt-chord.md) (Esc keeps its interrupt and loses its focus-clearing arm), [ADR-0040](0040-cross-domain-action-matrix-and-polymorphic-tui-spatial-topology.md) `[INV-KEY-02]` (Esc leaves the bidirectional-navigation set), [ADR-0042](0042-client-viewport-routing-ownership-tab-history-and-scene-self-projection.md) `[INV-NAV-02]` (Esc's navigation role narrows to overlays and scene steps inside a tab), [ADR-0051](0051-composer-canonical-quadrant-modality-atomic-draft-capsule-and-in-place-recall-topology.md) (the composer owns modes; it is not a mode of the scene), [ADR-0091](0091-sovereign-tab-context-isolation-and-tabbar-boundary-purity.md) §Checkpoint (the checkpointed pair of focus booleans becomes one `Focus` value), [ADR-0092](0092-uncompromising-scenebar-breadcrumb-topology-bidirectional-navigation-and-symmetric-keybindings.md) `[INV-NAV-03]` (the symmetry argument extends from scene history to focus planes), [ADR-0131](0131-help-dialog-architecture-tabbar-status-cluster-and-model-bar-purification.md) §Scene Navigation (the Help block documents a crossing, not a clearing)

> **Amendment note (2026-11-01)**: the chord is written `Shift-Tab` in every
> keycap, hint row, Help row, and doc table; it was `⇧Tab` before. Only the
> *spelling* changed. `Key::BACKTAB` is still the chord, `ToggleFocusPlane` is
> still the sole crossing, the involution and the non-remappability arguments
> above are untouched, and no user types a different key. The token now matches
> how the vocabulary already spells the other shift chords (`Shift-y`,
> `Ctrl-Shift-c`) and survives a terminal font without U+21E7; the rationale for
> the vocabulary rule lives in [ADR-0054](0054-universal-key-affordance-engine-width-adaptive-floor-truncation-and-modeless-f1-topology.md)
> §Amendment.

---

## Context and Problem Statement

The Thread scene hosts two surfaces: the **composer**, where a prompt is drafted, and the **transcript**, where execution is inspected. Both are permanent. Neither is a temporary state the other returns from. Yet the input model treated them asymmetrically in every dimension that matters:

1. **The entry direction had a chord; the exit direction had a different, overloaded one.** `Ctrl+P` / `Ctrl+N` (`SurfaceVerb::FocusPrevTarget` / `FocusNextTarget`) and bare `↑`/`↓` moved attention onto the transcript. Coming back required `Esc` (`SurfaceVerb::ClearFocusedTarget`, canonical chord hard-coded to `Key::ESC`). Two unrelated gestures for one reversible operation: the model had a forward map and no inverse.
2. **`Esc` accumulated five jobs on one keystroke.** In dispatch order: dismiss an open completion, clear the transcript focus, interrupt a running round (via the double-press window, [ADR-0025](0025-scene-scoped-interrupt-chord.md)), dismiss an overlay, or step back inside the scene. Which one fired depended on four independent predicates, and the first match consumed the key. A user pressing `Esc` to stop a runaway tool call could instead find themselves back in the composer with the round still running — or vice versa.
3. **The exit chord could not be configured, and the reason was circular.** `SurfaceOverrides::from_config` refuses to assign `Esc` to any verb (it is "the universal escape hatch"), yet `ClearFocusedTarget`'s *canonical* chord was `Esc`. So `session.clear_focus` appeared in the Help dialog and in the config schema while being unassignable in practice. Meanwhile a remap to another chord did not retire the canonical `Esc` arm, so the same verb had two live chords — a direct contradiction of the registry's own "a remapped verb's canonical chord goes inactive" rule.
4. **The state was two independent booleans.** `focused_target: Option<InteractiveTarget>` and `transcript_focused: bool` could express combinations with no meaning ("an entry is highlighted **and** the composer is live"). Fifty-six write sites hand-maintained the pair; eight mouse branches each wrote `focused_target = None; transcript_focused = true;` by hand. The notion of "browse focus" was not a state at all but a blur condition on the composer.
5. **A stated principle was contradicted by the code.** `keymap.rs` declared "zero loss of printable characters: typing in transcript bounces back to composer", and `App::transcript_focused`'s own doc comment repeated it — while `resolve_printable` returned `InputAction::None`, enrolling the opposite behaviour in a test. The documentation promised a symmetric exit that did not exist.
6. **The prose drifted from the canonical chords.** Five sites described the entry direction as `Alt+↑`/`Alt+↓` while the canonical chords were `Ctrl+P`/`Ctrl+N`, and the scene resolver's `Up`/`Down` arms read `key.code` without inspecting modifiers, so every modifier combination of `↑`/`↓` behaved as the bare key.

The root cause is conceptual, not incidental: the model never said what the two surfaces **are** to each other. It modeled one privileged surface (the composer) plus a mode it could fall into, and then tried to represent the mode's exit with the same chord that means "cancel" everywhere else. Every defect above is a consequence.

## Decision Drivers

- **Peer surfaces need a symmetric operator.** If entering a surface and leaving it are different gestures, the model is wrong — not the gesture. Reversibility must be structural, not a convention the next contributor has to remember.
- **One chord, one job.** `Esc` is the product's cancel/interrupt vocabulary ([ADR-0025](0025-scene-scoped-interrupt-chord.md), [ADR-0110](0110-running-state-interaction-topology-and-action-permissibility-discipline.md)). A chord that cancels *and* navigates is a chord whose behaviour cannot be predicted from its label.
- **Unrepresentable beats forbidden.** The state space should not contain "an entry is highlighted while the composer is live". A compiler-enforced shape removes the class of bug rather than the instance.
- **Chord choice is a physical constraint, not a taste.** The chord must survive the four collision domains documented in `docs/explanation/terminal-shortcuts-and-conflicts.md`: no POSIX line-discipline key, no emulator-reserved sequence, no macOS Option dead key, no dependence on the Kitty enhanced-keyboard protocol.
- **Zero legacy burden.** The obsolete verb, its config name, and its Help row are deleted, not deprecated behind a flag.

## Architectural Decisions

### 1. `Focus` is one value with two planes

`crate::shell::focus::Focus` replaces both booleans:

```rust
pub enum FocusPlane { Composer, Transcript }

pub enum TranscriptFocus {
    Browse,                      // viewport attention (click into the transcript)
    Entry(InteractiveTarget),    // one activatable entry is highlighted
}

pub struct Focus { plane: FocusPlane, transcript: TranscriptFocus }
```

`App::focus: Focus` is the single owner. `Focus::entry()` is the **only** reader allowed to answer "is an entry focused": it returns `Some` exactly when the plane is `Transcript` *and* the sub-state is `Entry`, so the two facts cannot disagree. Both fields are private; callers transition through `enter_composer`, `enter_transcript`, `browse`, `toggle`, and `retain_entry`.

`TabContextState::focus` carries the same value across tab switches, so a tab is restored with one coherent plane — [ADR-0091](0091-sovereign-tab-context-isolation-and-tabbar-boundary-purity.md)'s checkpoint contract now has one field where it had two.

### 2. `Shift-Tab` is the sole explicit crossing, and it is an involution

`Key::BACKTAB` resolves to `InputAction::ToggleFocusPlane`, which calls `Focus::toggle()`. The operation flips the plane and changes nothing else, so:

```text
f(f(x)) == x            // plane AND remembered entry
```

That is `[INV-FOCUS-01]`, and it is why the transcript's sub-state is **remembered while the composer owns the keyboard**: leaving for the composer to re-read a draft and returning must land on the same highlighted entry, not force a re-walk of `Ctrl+P`.

`Shift-Tab` was chosen over every alternative for reasons that are checkable, not aesthetic:

| Candidate | Rejected because |
| --- | --- |
| `Esc` | Already the cancel/interrupt chord ([ADR-0025](0025-scene-scoped-interrupt-chord.md)); see decision 3. |
| `Tab` | Means "act inside the composer" (completion commit, `Tab` steer/follow-up toggle while running). A key whose meaning depends on `is_responding` is the asymmetry being removed. |
| `Ctrl+O` | POSIX `VDISCARD`; named as a hazard in `docs/explanation/terminal-shortcuts-and-conflicts.md` §2. |
| `Ctrl+Tab` | Intercepted by terminal-emulator tab switching; never reaches stdin. |
| `Alt+<letter>` | macOS Option composes diacritics (`∂`, `†`, `∑`) unless the user remaps the modifier. |
| `F6`–`F8` | Off the home row and undiscoverable; a two-pane scene needs no panel-cycle ring. |
| `Ctrl+G` | Free here, but `Ctrl+` space is already dense with readline bindings and `Ctrl+G` is `get-out` in adjacent tools — a near-miss hazard rather than a pairing. |

`Shift-Tab` is a dedicated CSI sequence (not a Kitty-only report), collides with nothing globally, and pairs with `Tab` literally: `Tab` acts *within* the composer plane, `Shift-Tab` crosses out of it. The two are advertised together in the composer hint row.

### 3. `Esc` never crosses planes

`resolve_esc`'s focus-clearing arm is deleted. `Esc` now resolves to, in order: dismiss an open completion, or interrupt a running round (scene-scoped, double-press confirmed, [ADR-0025](0025-scene-scoped-interrupt-chord.md)), or nothing. The `CommandId::CancelOrBack` executor loses its `focused_target.is_some()` branch for the same reason: `Esc` from a palette entry must not silently relocate the keyboard either.

`SurfaceVerb::ClearFocusedTarget` and its `session.clear_focus` config key are **deleted**, not deprecated. `InputAction::ClearFocusedTarget` survives with a narrower meaning — *demote the entry highlight to browse focus, staying on the transcript* — because pointer paths and transcript-mutation paths genuinely need to say that, and expressing it as an explicit action keeps them from writing the focus struct directly.

### 4. Implicit crossings stay, and they are gestures, not chords

Two gestures cross without a chord, because in each the crossing is what the gesture already means:

- **Clicking:** a click into the transcript content or a step summary enters the transcript plane (Browse / Entry respectively); a click into the composer's input box enters the composer plane. Clicking inert chrome (tab bar background, links) changes no plane.
- **Walking off the end:** `Ctrl+N` / `↓` past the last entry with nothing left to scroll returns to the composer; `Ctrl+P` / `↑` past the first entry with nothing left to scroll walks the entry list from the other end (unchanged). The transcript has nothing left to offer, so the keyboard goes where there is something to do.

Both are *accelerations* onto the same `Focus` value — no parallel state, no separate "mouse mode".

### 5. The sub-state degrades; the plane does not

`Focus::retain_entry` rewrites a stranded entry selection to `Browse` when the transcript mutates under the user (a step collapses, a round appends). The **plane is never touched**: the user's attention is still on the transcript, and silently reverting the keyboard to the composer is precisely the asymmetry this ADR removes.

### 6. Hints and Help derive from the same origin

`live_chat_hints` gains the crossing on every composer-plane row (`Shift-Tab transcript`) and replaces `Esc back` with `Shift-Tab composer` on the entry rows. The Help dialog's Scene Navigation block documents `Switch focus` and the entry walk; the `clear_focus` row is gone. Since `Shift-Tab` is grammar rather than a shortcut, `GlobalOverrides::from_config` refuses to reassign `CommandId::ToggleFocusPlane` — but `canonical_global_chord` still returns `Key::BACKTAB`, so the registry table, the chrome, and the Help dialog read one value.

### 7. Both planes announce ownership in the same channel

A symmetric model with an asymmetric picture is still asymmetric to the user. The composer answered "the keyboard is here" in the **background** channel — a bright `input_surface()` panel — and answered "it left" by recessing that same panel to `input_surface_inactive()`. The transcript answered the same question only in the **foreground** channel: a hue tint on one summary line, and only while that line was collapsed. `Shift-Tab` therefore had exactly one visible half.

> **Amendment note (2026-11-02)**: the *cue geometry* below was revised in
> place. The channel is unchanged — both planes still answer in the background —
> but the transcript now states it at two scales, as a **pane wash** over the whole
> viewport plus a one-cell **entry marker** in the highlighted row's gutter,
> instead of a gutter rail plus a full-width band behind the header. Three of the
> four earlier properties were right and survive (the channel, the zero-layout
> constraint, and "ownership is not a disclosure fact"); two were wrong:
>
> - **A rail is edges, not a surface.** Painting the two gutter columns put the
>   cue at the pane's extremes and left the middle — every column the user is
>   actually reading — exactly as it was, so the crossing read as two stray
>   vertical artifacts rather than as "this pane is live". The composer does not
>   outline itself; it *is* a lit panel. The transcript's peer of that is the
>   pane's own surface.
> - **A band under the header fights the header.** A header row's foreground is
>   the disclosure × lifecycle × interaction composition, so a background under it
>   had to invert the running accent and the collapsed muted rung that the
>   renderers had resolved on purpose — and it landed on the one row that is
>   already the loudest thing on screen. It read as a highlighted component, not
>   as a live surface, and it forced a contrast re-seat (`band_text`) to stay
>   legible against itself.
>
> The amended geometry:

| State | Cue | Token |
| --- | --- | --- |
| `Composer` | lit input panel | `input_surface()` |
| `Transcript(Browse)` | **pane wash** — the whole transcript surface lifted one quiet step (`pane_focus_edge_style()` on `Reset`-surface profiles) | `pane_focus()` |
| `Transcript(Entry)` | the wash, plus one **marker vertical bar** in the highlighted entry's gutter | `pane_marker_style()` |

Three properties are load-bearing, and each is why the previous shape failed:

- **Area carries the pane cue.** `Shift-Tab` enters at `Browse`, with no entry highlighted, so an entry-only cue would leave the primary gesture with nothing to show for itself. Because the cue lands on every cell of the viewport, its step must stay *small*: the contrast is not what makes it visible, the number of cells that change is. A step the size of `selected_bg` would wash out every tone the renderers resolved for the page — the opposite failure to the rail, and just as wrong.
- **Zero layout cost, zero cooperation cost, and the lift happens at the source.** The wash arrives through `focus_plane::transcript_theme`: `draw_transcript` renders the stream with `Theme::pane_lifted()` while the transcript owns the keyboard, so every renderer paints the lifted pane through the struct it already reads and no renderer has to know the cue exists. The alternative — a per-cell restyle over the viewport after the stream — is what first shipped, and it measured at **5.8 ms on a 400x120 pane, thirteen times the cost of painting the entire frame**, spent on a focus indicator. The theme path costs one struct copy (a `Cow` borrow on the composer plane). One rect fill at the pane's rect covers the blank rows no renderer touches.
  The lift is **additive** and **background-only**: every background inside the pane shifts by the page's own delta, and no foreground moves. Repainting only the cells that happened to sit on the page tone is the obvious implementation and it is wrong — it leaves a code band, a command card, and a sent-message panel at their absolute tones while the page rises to meet them, so the page-to-band *separation* collapses (on the default scheme a code band loses more than half its elevation) and the document flattens into one slab. Foregrounds stay put because they are semantic: a running hue or a muted rung means the same thing at either page level. `pane_lifted_theme_matches_a_cell_wise_restyle` pins the equivalence, which is what keeps the field list honest.
  The marker occupies `band.x - 1`, a `TRANSCRIPT_H_INSET` gutter column nothing else paints. The pane can never reflow because the keyboard crossed.
- **The row is never repainted, and ownership is not a disclosure fact.** The marker is a separate cell, so the summary keeps the disclosure/lifecycle/hover composition it resolved for the page — no contrast re-seat, no inversion of a running step's in-flight hue. It is drawn on expanded entries too: suppressing the cue on the expanded case removed the signal precisely where inspection happens.

The marker is deliberately **not** `selected_bg` and is a **neutral luminance rung**, never a hue: a keyboard-ownership cue must never read as a selection the user made, nor as any lifecycle state.

Two further constraints fall out of moving the entry cue out of the row's background:

- **The lift is additive.** Every background in the pane is shifted by the page's own delta, not only the cells that happen to be on the page tone. Repainting just the page cells is the obvious implementation and it is wrong: it raises the page to meet the code bands, command cards, and sent-message panels, so their *separation* from the page collapses (on the default scheme a code band loses more than half its elevation) and the document flattens into one slab. Shifting everything preserves every relative step; only where the stack sits changes.
- **Focus and hover are independent.** `Interaction` carries `Focused` and `FocusedHovered` as separate variants. Collapsing them (the first shape) discarded the pointer's answer whenever the keyboard already owned the row — and clicking a summary focuses it *while* leaving the pointer on it, so the ordinary way of reaching a row made it go visually dead.

**Three guarantees are compile-time or test-derived rather than disciplinary.**

1. The headroom measurement walks a classification of *every* field of `Theme` (an exhaustive destructure, `deny(unused_variables)` inside the classifier), so adding a field forces a decision about whether the pane lift must carry it.
2. `pane_lifted()` names every field too — no `..*self`. That struct-update form is the specific hazard: with it, a new background is silently carried over verbatim, so it stops being lifted and nothing says so. Naming every field makes the omission `error[E0063]`, and `pane_lifted_lifts_exactly_the_classified_backgrounds` closes the remaining gap by asserting, **derived from the classifier**, that the lifted set equals the classed-as-background set in both directions. A hand-picked field list in that test is precisely how the original defect survived.
3. `Theme` is `Copy`: the lift hands the renderer a lifted *value*, so a frame costs one move, and any future heap-owned field breaks the derive on the day it would introduce the cost.

The classification is still a human judgement (`is this a background the transcript pane draws?`), and the compiler can only force *a* decision, not a correct one. What bounds the damage is that the two halves are compared against each other: a wrong classification cannot silently truncate a band, because the lift and the headroom measurement must agree.

A related trap that is worth naming, because it is invisible on a palette with generous headroom: the plane question must be asked of the **base** theme. On a lifted theme `app_bg` already *is* the wash, so `pane_focus_delta()` reads zero and `has_pane_wash()` reports `false` — a caller handing the lifted theme back to `draw_pane_focus` gets the gutter fallback painted over an already-lifted page. `draw_transcript` therefore keeps both values with distinct meanings, and `the_plane_question_is_asked_of_the_base_theme` pins it.

**Whether a palette can carry the wash is measured, not declared.** `has_pane_wash()` asks `pane_focus_delta()`, which needs two things from the palette itself: an RGB page to shift, and enough **channel headroom** above the bands the transcript builds. The declared tone is a *wish*; the effective tone is that wish scaled back to the largest step the palette can absorb, so a careless `pane.wash_bg` override — or a hand-authored palette whose bands sit near a channel limit — degrades to a smaller wash instead of clamping a band and flattening the document. A wish that cannot be honoured at all (a band pinned at the pole, a `Reset` page) yields no wash and the cue falls back to `pane_focus_edge_style()`.

That fallback is the one place the edge form survives, and it is a constraint rather than a preference: a page that *is* the terminal's own background cannot be shifted, and a palette with no headroom cannot be shifted without inverting the bands above it. The gutter is the largest region those profiles can afford. On `Structured` the fallback and the marker both degrade to DEC reverse video (ADR-0053). Both answers are correct by construction, which is why classifying a theme as `Chromatic` no longer implies it can carry a whole-pane cue.

One value stays one truth at the paint boundary as well: `draw_transcript` forces `focused_entry` to `None` on the composer plane, so a caller that hands in both a live composer and a highlighted entry cannot paint "an entry is lit while the composer is live" — the state `[INV-FOCUS-03]` makes unrepresentable in the model is made unwritable at the renderer too.

### Normative Invariants Established

- **[INV-FOCUS-01] Single symmetric crossing**: A chat scene has exactly two peer focus planes. `Shift-Tab` MUST be the only explicit crossing chord, it MUST be an involution (`toggle` twice restores plane *and* remembered sub-state), and `Esc` MUST NOT cross planes. Enforced by `crate::shell::focus` unit tests (`toggle_is_an_involution_from_either_plane`, `crossing_keeps_the_highlighted_entry`), by `shell::input::tests::paste_escape::escape_never_crosses_focus_planes`, and by `shell::session::tests::focus_navigation_and_interrupt_react_to_mode`.
- **[INV-FOCUS-02] No typing into an unfocused draft**: a printable keystroke MUST be inserted only while the composer plane owns the keyboard; on the transcript plane it MUST be inert rather than absorbed into the draft. Enforced by the `Char` arm of `resolve_chat_surface_key_motion` and `shell::session::tests::typed_character_in_transcript_is_inert_and_does_not_bounce`.
- **[INV-FOCUS-03] One value, one truth**: the focus state is a single `Focus` value. No second field, flag, or `bool` may describe it, so "an entry is highlighted while the composer is live" is unrepresentable. Enforced by the type: the fields are private and `Focus::entry()` is the only entry accessor.
- **[INV-FOCUS-04] Both planes announce ownership in the background channel** (amended 2026-11-02): because exactly one plane owns the keyboard, each plane MUST say so in the *same* channel. The composer does it with `input_surface` / `input_surface_inactive`. The transcript MUST do it with the **pane wash** (`pane_focus()` over the whole viewport, `FocusPlane::Transcript`) plus one **marker cell** in the highlighted entry's reserved gutter (`FocusPlane::Transcript(Entry)`). Three constraints follow and are separately enforced:
  - A foreground tint alone MUST NOT be the transcript's cue: it is suppressed on an expanded entry and out-shone by a lifecycle accent on a collapsed one, so the crossing has no visible half.
  - A cue MUST NOT be confined to the pane's **edges**: the rail form satisfied "background channel" and still failed, because the cue was present at the two extreme columns and absent everywhere the reader looks.
  - A cue MUST NOT repaint an entry's **header row**: its tone is the disclosure × lifecycle composition, so a band under it inverts the very signals those renderers resolved. The row's painted tones MUST be identical on both planes.
  - Reverse video is the `Structured` archetype's form of the same channel; `Reset`-surface archetypes carry the cue through the marker alone.

  - A cue MUST NOT cost a viewport pass: the pane state is a **theme** fact the renderers paint with, never a post-hoc per-cell restyle. Enforced by `nuo_view::focus_plane::cost::the_pane_cue_does_not_restyle_the_viewport` and `the_fallback_costs_only_the_gutters`.

  Enforced by `nuo_view::focus_plane` unit tests (`the_pane_cue_lifts_the_whole_page`, `the_composer_plane_owns_the_unlifted_page`, `the_lift_preserves_every_relative_step_inside_the_pane`, `the_post_pass_touches_nothing_when_the_theme_carries_the_cue`, `reset_surfaces_fall_back_to_the_gutter`, `fallback_spans_exactly_the_viewport`, `marker_sits_in_the_gutter_beside_the_row`, `marker_degrades_without_a_gutter`), by `nuo_view::disclosure::tests::focus_is_a_marker_not_a_tint` / `focus_marker_survives_an_expanded_entry` / `focus_and_hover_are_independent`, by `nuo_ui_primitives::theme::tests::pane_focus_is_a_small_but_visible_wash_in_every_color_scheme` / `pane_lifted_theme_matches_a_cell_wise_restyle` / `a_palette_without_headroom_refuses_the_wash` / `an_oversized_wash_is_scaled_back_to_the_headroom` / `a_theme_override_cannot_flatten_the_pane` / `every_reachable_palette_lands_on_a_correct_pane_cue` / `entry_focus_marker_is_neutral_and_visible`, and by `shell::render::tests::focus_plane::*` (painted-grid assertions on both planes, including `marking_an_entry_does_not_recolour_its_summary`, `wash_preserves_the_relative_step_of_a_content_band`, `wash_keeps_the_sent_message_panel_raised_above_the_page`, and `a_focused_row_under_the_pointer_still_shows_the_hover_cue`).

## Consequences

- **Positive**: the crossing is reversible by construction, so the "how do I get back?" question has one answer that is the same gesture both ways. `Esc` regains a single, predictable job, which restores the interrupt's reliability during a runaway round. The 56 hand-maintained focus write sites collapse onto one value with five named transitions, and the eight mouse branches that each hand-wrote the pair now call `enter_transcript` / `enter_composer`. Two defects fall out for free: `ClearFocusedTarget`'s double-chord behaviour after a remap disappears with the verb, and the "docs say typing bounces, code says otherwise" contradiction is resolved in the code's favour with the promise removed rather than silently broken. The keyboard's location is now legible in the same channel on both sides of the crossing, so the symmetric model is symmetric on screen as well as in the type.
- **Negative & Trade-offs**: `Esc`-to-return is muscle memory for anyone who has used the previous model, and it now leaves the plane alone. Mitigation: the composer hint row advertises `Shift-Tab` on every row, the entry row's first nav chord is `Shift-Tab composer`, the Help dialog documents it, and the implicit exits (click the composer; walk off the end of the entry list) still work. `Shift-Tab` is a two-key chord where `Esc` was one — accepted, because a one-key crossing is what made the collision possible. `session.clear_focus` disappears from the config schema: a config carrying it is ignored (unknown verbs are skipped, not errors), so no migration is required. The amended cue (§7 amendment) costs one restyle pass over the transcript viewport per frame while the transcript owns the keyboard — a cell-wise background rewrite with no glyph work, no damage outside the pane, and no layout participation; the marker is exactly one cell. It is also, by construction, *quiet*: a whole-surface state has to be small to be legible, so the transcript's half of the crossing is a softer signal than the composer's lit panel. That asymmetry is the deliberate consequence of the two planes having different areas, and the reason the entry marker exists at all — it is the only part of the cue that says *which row*, and it is stated where nothing else paints.

## Rejected Alternatives & Negative Knowledge

### Alternative A: Keep `Esc`, add a symmetric twin for entry (Rejected)

- **Why considered**: zero migration; the chord is already in muscle memory; no new grammar.
- **Why rejected**: `Esc` cannot be the crossing *and* the interrupt. Any twin (`Ctrl+P` as "enter", `Esc` as "leave") leaves the two directions as different chords, which is the defect itself, and it keeps the interrupt hostage to a focus predicate.

### Alternative B: `Ctrl+G` as the crossing (Rejected)

- **Why considered**: unused in this codebase, reachable from the home row, no line-discipline collision.
- **Why rejected**: it does not pair with anything, so it must be memorised; and `Ctrl+G` is the conventional get-out chord in adjacent terminal tools, making a mis-press land on "what does this do?" rather than on a predictable crossing. `Shift-Tab` needs no teaching because `Tab` is already documented as the composer-plane verb.

### Alternative C: Make it a remappable `SurfaceVerb` (Rejected)

- **Why considered**: every other surface chord is remappable; consistency argues for one more.
- **Why rejected**: `SurfaceVerb`s are shortcuts over state the user can also reach otherwise; the crossing is the plane's *only* exit, and the file already refuses to remap `Esc`/`BackTab` on the grounds that they are grammar. A remappable crossing would also have to solve "what happens to a half-remapped `Tab`/`Shift-Tab` pair" — a problem created solely by making it remappable.

### Alternative D: Enter the transcript at the last entry on every crossing (Rejected)

- **Why considered**: one fewer piece of state; the toggle becomes a pure plane flip.
- **Why rejected**: it makes the crossing lossy. A user who highlights a failing step, glances at the composer to check what they typed, and crosses back would land somewhere else and have to re-walk. The remembered sub-state is the difference between a symmetric operator and a reset.

### Alternative E: Keep `transcript_focused` as `FocusPlane::Transcript(Browse)` only (Rejected)

- **Why considered**: smaller diff; reuse the existing boolean as a plane discriminant.
- **Why rejected**: it cannot represent the entry highlight, so `focused_target` would have to stay alive as a second field — reintroducing the two-source state that `[INV-FOCUS-03]` forbids and keeping every one of the 56 write sites on manual consistency.

### Alternative F: Separate verbs for "enter" and "leave" that happen to be bound to the same chord (Rejected)

- **Why considered**: keeps the action vocabulary explicit about direction; each side could carry different guards.
- **Why rejected**: the guards would diverge (as they did: only the leave side was reachable while running), and two actions mapped to one chord mean the vocabulary no longer names what the user did. The user pressed one key; the model records one act.

## Links

- Amends: [ADR-0025](0025-scene-scoped-interrupt-chord.md), [ADR-0040](0040-cross-domain-action-matrix-and-polymorphic-tui-spatial-topology.md), [ADR-0051](0051-composer-canonical-quadrant-modality-atomic-draft-capsule-and-in-place-recall-topology.md), [ADR-0092](0092-uncompromising-scenebar-breadcrumb-topology-bidirectional-navigation-and-symmetric-keybindings.md), [ADR-0131](0131-help-dialog-architecture-tabbar-status-cluster-and-model-bar-purification.md)
- Complements: [ADR-0110](0110-running-state-interaction-topology-and-action-permissibility-discipline.md) (read inspection stays open while a round runs), [ADR-0035](0035-domain-scoped-surface-architecture-and-encapsulated-dialog-lifecycle.md) (overlays trap focus above the planes)
- Documentation: `docs/reference/tui/interaction.md`, `docs/explanation/terminal-shortcuts-and-conflicts.md`
