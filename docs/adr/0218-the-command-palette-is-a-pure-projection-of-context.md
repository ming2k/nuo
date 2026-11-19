---
id: ADR-0218
title: "The Command Palette Is a Pure Projection of Context"
status: accepted
date: 2026-11-18
scope: tui/palette, tui/keymap, tui/event-loop, tui/overlays, presentation/ui-primitives
superseded_by: null
negative_knowledge: true
---

# 0218. The Command Palette Is a Pure Projection of Context

- Status: Accepted
- Date: 2026-11-18
- Implements: [RFC-0005](../rfc/0005-contextual-command-palette.md)
- Amends: [ADR-0077](0077-workspace-namespace-pure-surface-topology-and-categorized-palette.md) (facets: `/` removed, `:` added; category badges replaced by context-first groups), [ADR-0151](0151-command-domain-orthogonality-sovereign-tab-navigation-and-help-modal-purification.md) `[INV-PLANE-05]` (the palette is natural-language *and* context-aware)

---

## Context and Problem Statement

The palette (`DialogKind::Switcher`) listed only global registry commands. It
had nothing to say about the scene or dialog the user was looking at, which is
where guidance is most needed.

Its context was assembled by hand in five call sites that disagreed, so the
highlighted row and the executed row could differ. The 28 `Scope::Dialog(_)`
registry rows had no executor: they existed only as reference text, next to
the real key resolvers. The right-hand keycaps were free-form strings that
ignored user remaps.

## Decision

The palette is `crates/nuo/src/shell/palette/`, a pure projection
`context → sources → entries → rank → rows`. Scene and dialog verbs are
declared once in `palette::verbs`, as the exact `InputAction` their key
produces. The palette and the in-dialog key reference (`?`) both read those
tables. `DialogKind::Switcher` becomes `DialogKind::Palette`.

## Invariants

- **`[INV-PAL-CTX-01]` One context, one projection.** `PaletteContext::capture`
  is the only palette code that reads `App`. Render, list navigation, pointer
  hits and execution all call `palette::project`.
- **`[INV-PAL-CTX-02]` Keycaps are honest.** Every scene, dialog and tab keycap
  is resolved through `input::route_event` in the key state its `Requires`
  declares, and must yield the entry's action. This is enforced by
  `palette::tests::every_*_keycap_resolves_to_*`. No keycap may render as `?`.
- **`[INV-PAL-CTX-03]` The palette implements no verb.** `InputAction::PaletteRun`
  pops the palette, then dispatches `Effect::Command` through
  `execute_command_by_id` or re-enters `dispatch_action` with `Effect::Action`.
- **`[INV-PAL-CTX-04]` Relevance hides; availability explains.** An entry whose
  scene or dialog is not present is absent. An entry that applies but cannot
  run is dimmed, sorted after runnable entries, and carries a reason. Enter on
  it shows the reason and keeps the palette open.
- **`[INV-PAL-CTX-05]` Verb tables are exhaustive.** `scene_verbs` and
  `dialog_verbs` match every `SceneKind` and `DialogKind` with no wildcard arm.

## Consequences

- Deleted as dead or duplicated: `overlays/command_palette.rs`,
  `keymap::commands_for_palette`, the `Scope::Dialog` variant and its 28 rows
  and `CommandId`s, `CommandId::{CopyFocusedEntry, CopyDiagnostic}` (now
  thread verbs `y` / `Y`), `InputAction::ViewCloseSelected`,
  `SurfaceStore::switcher_rows*`, `SwitcherTarget`, `SceneKind::hint`,
  `DialogKind::hint`, and `ComposerState::recent_commands` (replaced by
  `palette::Recents`, keyed by a stable entry key).
- <kbd>Delete</kbd> in the palette is explicitly inert, so it cannot fall
  through to the composer underneath.
- `ui_primitives::chord::display_token` gains `*`. The palette's glyph test
  found that the Models dialog's favourite key rendered as `?`.

## Rejected Alternatives

1. **`Scope::Scene(_)` registry rows.** These would need a second executor per
   verb that already has an `InputAction` handler. That is the dead-row failure
   in another form.
2. **Caching the projection on the dialog during render.** Render would mutate
   state. Re-projecting over unchanged state is deterministic.
3. **Hiding unavailable entries.** A search for a blocked verb would then
   return nothing, and the user could not tell "does not exist" from "not now".
4. **Listing the server slash catalogue.** This violates `[INV-PLANE-01]`. The
   composer's completion menu owns that vocabulary.
