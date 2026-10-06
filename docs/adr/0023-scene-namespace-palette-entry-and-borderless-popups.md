---
id: ADR-0023
title: "Scene-Namespace Palette Entry, Standing Head Legend, and Borderless Elevation Popups"
status: accepted
date: 2026-10-06
scope: tui/nuo-tui, presentation/chrome, interaction/affordance, terminal/elevation
superseded_by: null
negative_knowledge: true
---

# 0023. Scene-Namespace Palette Entry, Standing Head Legend, and Borderless Elevation Popups

- Status: Accepted
- Date: 2026-10-06
- Deciders: Nuo Architecture Working Group
- Consulted: TUI, Interaction, and Design-System maintainers
- Informed: System Architects
- Amends: row-2 legend demand-gating, Command Palette entry unification, and chrome-vs-dispatch keycap honesty
- Amended by: [ADR-0024](0024-two-row-head-band-session-identity-and-scene-row.md) §2 (row 2 now carries the scene name + scene context +
  run-mode flags, with the `C-x menu` pair still standing on every scene)

---

## Context and Problem Statement

The `Ctrl+X` **scene namespace** introduced a two-stroke prefix whose
verbs are resolved *before* scene and modal dispatch, so a namespace verb fires
from **every** context — top level, behind a dialog, behind a sheet. The
Command Palette / surface switcher is one of those verbs (`C-x p`), and the
router proved it: `C-x p` opened the switcher even while another modal was the
keyboard foreground.

At the same time the palette still carried a **single-stroke** global binding,
`Ctrl-L`, with a special-case in the router that *swallowed* `Ctrl-L` whenever a
non-switcher modal was open:

```rust
CommandId::CommandPalette => {
    if overlay == Some(Dialog(Switcher)) || overlay.is_none() {
        return ViewSwitcherToggle;
    }
    if physical_key == Key::CTRL_L {
        return InputAction::None; // the "prohibition"
    }
}
```

Two frictions followed:

1. **Two entry points, one of them impaired.** `Ctrl-L` was a shadow path that
   behaved differently depending on the foreground — precisely the kind of
   context-sensitive divergence. The namespace entry (`C-x p`) is the genuinely universal one.
2. **Chrome told the wrong story.** The palette affordance lived as a
   right-aligned `Ctrl-l palette` keycap on the *session head's* row 1, so the
   shortcut appeared only on the main view and named a chord that was not the
   canonical one. Row 2 — the proper home for view affordances —
   was demand-gated and showed the namespace pair only on
   breadcrumb / settings / dashboard pages.
3. **Popups drew edge lines.** The floating which-key card, dropdown, popover,
   and tooltip all rendered explicit box borders even on modern
   TrueColor/256-color terminals, where a distinct background is a stronger,
   quieter separation than a stroke — the language the toast component already
   speaks.

This ADR ratifies the retirement of the `Ctrl-L` binding (and its modal
prohibition), a **standing** `C-x menu` legend on every scene's row 2, and a
**borderless** elevation treatment for floating popups on modern terminals —
and, under [INV-AGENT-01], records the rejected alternatives.

---

## Decision Drivers

- **One entry point, one behaviour**: a shortcut must mean the same thing in
  every context, or chrome cannot advertise it honestly.
- **Discoverability**: the palette is the app's discovery surface; its entry
  point must be visible on every scene, not just the main view.
- **Visual consistency**: popups should follow the same elevation language as
  toasts, not a per-component stroke decision.
- **Terminal independence**: the treatment must degrade correctly on ANSI-16
  and monochrome terminals ([ADR-0003](0003-autonomous-terminal-canvas-substrate-nuotc.md)).
- **No silent regressions**: retiring a chord must not make it insert a literal
  character (control characters fall through to the printable-insert arm unless
  explicitly swallowed).

---

## Considered Options

- **Option 1 (Chosen)**: Retire `Ctrl-L` as the canonical palette chord and its
  modal prohibition; make `C-x p` the sole canonical entry (`palette` stays
  user-remappable). Stand up row 2 on every scene carrying a `C-x menu`
  namespace pair. Render floating popups borderless on `Chromatic` terminals,
  framed on `Hybrid`/`Structured`.
- **Option 2**: Keep `Ctrl-L` as a second canonical chord and only *remove* the
  modal prohibition (both chords open the palette everywhere).
- **Option 3**: Keep `Ctrl-L` and the prohibition; only relabel the keycap.
- **Option 4**: Keep bordered popups everywhere; change only the namespace
  naming.

---

## Decision Outcome

Chosen option: **"Option 1"**.

### 1. `C-x p` is the canonical palette entry

`CommandId::CommandPalette` has **no** canonical single-stroke chord
(`canonical_global_chord` returns `None`; `canonical_global_key` no longer maps
`Ctrl-L`). Its canonical entry is the scene namespace's switcher verb —
advertised as `C-x p` in the registry `hint`, the switcher dialog hint, and the
new standing head legend. The command stays user-remappable via
`[keybindings] palette = "…"`; a remapped chord resolves through the Stage-5
globals and opens the switcher from every context (the old prohibition is gone).

Because the namespace resolves *before* modal dispatch, `C-x p` opens the
switcher even while another modal is foreground — which is exactly the
behaviour the prohibition used to deny `Ctrl-L`. Retiring `Ctrl-L` removes the
divergence rather than preserving it.

`Ctrl-L` is explicitly swallowed in the router (like the already-inert
`Ctrl-H`/`Ctrl-M`) so it never inserts a literal `l`.

### 2. A standing `C-x menu` legend on row 2

`ViewHints::has_content` returns `true` for every reachable scene (Session,
Settings, Dashboard, and any breadcrumb-identified page). Row 2 therefore
**stands up on every scene** and always carries the `C-x <label>` namespace
pair, where `<label>` is now **`menu`** (the namespace's headline verb is the
palette / switcher). Scene-specific segments (the aside chip, the breadcrumb)
lead it. Only the unreachable crumb-less aside/subagent page reports
`false` (so a malformed hint set still paints nothing).

> **Amended by ADR-0024**: row 2's scene-specific segments are now defined as
> the **scene row** — the scene's plain lowercase name, then its context, with
> the session's run-mode flags leading the `C-x menu` pair. The aside chip and
> the `Main › …` breadcrumb are superseded by the scene name and the scene's
> context respectively; the pair itself is unchanged.

The session head's row-1 `palette_key` field and its `Ctrl-l palette` keycap are
removed entirely: the palette affordance is a row-2 property now, uniform across
scenes.

### 3. Borderless elevation popups on modern terminals

Floating popups — the which-key card, the anchored tooltip, the generic
dropdown, and the anchored popover — are **borderless on `Chromatic`
terminals** (TrueColor / 256-color) and read by their elevated panel background
alone, matching the toast's visual language. On `Hybrid` (ANSI-16) and
`Structured` (monochrome / Linux VT), where a background delta is unavailable or
indistinct, they keep an explicit frame. This mirrors `elevation::modal_frame`'s
existing archetype split.

### Invariants & Behavioral Boundaries

- **INV-PAL-01**: The Command Palette has exactly one canonical entry, the
  scene namespace's `C-x p`. No `Global`-scope command advertises a
  single-stroke palette chord; `canonical_global_key(Ctrl-L)` is `None`.
- **INV-PAL-02**: `Ctrl-L` (and any unbound control letter) is inert on every
  surface — it never inserts a literal character.
- **INV-HINT-01**: Every reachable scene's head band renders a row-2 namespace
  pair named `menu`; chrome never advertises a chord the dispatcher does not
  honour.
- **INV-POPUP-01**: On `Chromatic`, floating popups draw **no** edge glyphs;
  on `Hybrid`/`Structured` they keep a frame. The archetype — never a per-call
  boolean — is the single switch.

### Positive Consequences

- One palette entry that behaves identically everywhere; the impaired shadow
  path is gone.
- The palette is discoverable from every scene via the standing legend.
- Popups share one elevation language with toasts; less visual noise on modern
  terminals.

### Negative Consequences & Trade-offs

- Muscle-memory users who learned `Ctrl-L` lose the default chord; mitigation:
  it remains a one-line `[keybindings]` remap, and the legend advertises the new
  entry on every scene.
- Row 2 now consumes one line on the main view even with no asides; mitigation:
  the band is a fixed, predictable two rows (no layout shift as state changes),
  and the legend carries real, actionable content.

---

## Rejected Alternatives & Negative Knowledge

### Option 2 (Rejected) — keep `Ctrl-L`, only drop the prohibition
- Why considered: zero migration cost; two familiar entries.
- Why rejected: two canonical chords for one command doubles the advertisement
  surface and keeps a single-stroke global that competes with readline's
  `Ctrl-L` lineage while adding nothing the namespace entry does not already
  provide. A single universal entry is the cleaner invariant.

### Option 3 (Rejected) — keep the chord and prohibition, relabel only
- Why considered: smallest possible diff.
- Why rejected: it preserves the exact defect (a chord that means
  different things by context) and leaves the palette invisible off the main
  view.

### Option 4 (Rejected) — keep bordered popups
- Why considered: a frame is unambiguous on every terminal, needing no archetype
  branch.
- Why rejected: on modern terminals a border is visual noise; the background
  channel already separates elevation (the toast proves it). The archetype split
  still keeps the frame where it is the only available channel.

### Rejected sub-idea — put the namespace keycap on row 1
- Why considered: keep the head single-row.
- Why rejected: row 1 is identity + status; a navigation affordance
  there re-creates the exact confusion this ADR removes, and a row-1 affordance
  cannot be shared with the crumb-identified pages that already own row 2.

---

## Links

- Related ADRs: [ADR-0003](0003-autonomous-terminal-canvas-substrate-nuotc.md) (retained terminal canvas),
  [ADR-0011](0011-nuo-tui-presentation-and-nuo-server-container.md) (TUI presentation),
  [ADR-0024](0024-two-row-head-band-session-identity-and-scene-row.md) (two-row head band)
- Related modules: `nuo-tui::keymap` (`scene_namespace`, registry),
  `nuo-tui::input::router`, `nuo-tui::view_header`,
  `nuo-tui::components::which_key`, `nuo-tui::components::{dropdown,popover,tooltip}`
