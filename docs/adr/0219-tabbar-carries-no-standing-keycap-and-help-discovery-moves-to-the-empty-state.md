---
id: ADR-0219
title: "TabBar Carries No Standing Keycap and Help Discovery Moves to the Empty State"
status: accepted
date: 2026-12-20
scope: presentation/nuo-view, presentation/nuo-ui-primitives, tui/chrome, tui/event-loop
superseded_by: null
amends:
  - "[ADR-0131](0131-help-dialog-architecture-tabbar-status-cluster-and-model-bar-purification.md) §2 (the constant `F1 help` affordance is retired from the TabBar status cluster)"
  - "[ADR-0092](0092-uncompromising-scenebar-breadcrumb-topology-bidirectional-navigation-and-symmetric-keybindings.md) (the chord-free SceneBar no longer leans on a standing TabBar entrance)"
negative_knowledge: true
---

# 0219. TabBar Carries No Standing Keycap and Help Discovery Moves to the Empty State

- **Status**: Accepted
- **Date**: 2026-12-20
- **Amends**: [ADR-0131](0131-help-dialog-architecture-tabbar-status-cluster-and-model-bar-purification.md)
  §2, [ADR-0092](0092-uncompromising-scenebar-breadcrumb-topology-bidirectional-navigation-and-symmetric-keybindings.md),
  [ADR-0054](0054-universal-key-affordance-engine-width-adaptive-floor-truncation-and-modeless-f1-topology.md)
  (`F1 help` as a TabBar *label* — the `F1 more` overflow trigger is untouched)

## Context and Problem Statement

ADR-0131 §2 made `F1 help` a constant rightmost affordance on the Client TabBar,
justified as "immediate visual discoverability without layout jitter". In
practice it is neither free nor the right place:

1. **It taxes the scarcest resource on the row.** The TabBar's job is tab
   titles. A 7-column fixed keycap plus `BAR_LEGEND_GAP_MIN` (6) removed 13
   columns from the tab rail permanently — on a 44-column minimum terminal that
   is nearly a third of the row, spent on a key the average user presses once,
   for a dialog that is already reachable by `F1`, `Alt-h`, and `/help`.
2. **It teaches nothing.** `F1 help` names the key and the destination but not
   the value; there is no room on a one-row band to say what is behind it. The
   empty state has room: it already rotates durable capability pages
   ("Branch thread into a new tab with `/fork`", "Recall past prompts with
   `Ctrl-r`") on a wall-clock cadence, and help belongs in that list rather than
   in permanent chrome.
3. **It competed with the surface it advertised.** ADR-0131
   〔INV-HELP-04〕 makes the Help dialog the only surface that prints a chord for
   a *remappable* binding. A permanent keycap for the entrance diluted that
   rule, because the keycap was itself a chord printed outside the dialog.
4. **It made the status cluster two-purpose.** The cluster's other member —
   `[!N]` — is conditional and diagnostic (ADR-0121/0131). A constant keycap
   beside a conditional badge forced the row to reserve space for the constant
   member in every scene, every width, always.

## Decision

1. **The TabBar status cluster carries no standing keycap.** `[!N]` (unread
   fault badge) is the row's only right-edge affordance, and it appears only
   when unread faults exist. The `help_rect` hit target, the `HelpBadge` UI key,
   `KeyAffordance::tabbar_help`, and `KeyAffordance::render_tabbar_spans` are
   deleted rather than left dormant: a hit target with nothing painted is a
   phantom click region. The rail's `BAR_LEGEND_GAP_MIN` reservation is also
   conditional now — with no badge there is nothing to separate from.
2. **Help discovery moves to the empty-state capability carousel.** One tour
   page reads `See every shortcut with F1 or Alt-h`, placed second so a
   first-time user meets it inside the first minute of a session. It is a page
   among peers, not a billboard: it rotates away like every other page and
   nothing re-pins it.
3. **The chords stay what they were.** `F1` and `Alt-h` still toggle
   `DialogKind::Help`, and `/help` still opens it. This ADR removes an
   advertisement, not a binding.
4. **The Floor-1 overflow trigger is unaffected.** `F1 more` / `F1 help` as the
   *overflow* affordance of a width-constrained legend row (ADR-0054
   `[INV-AFFORDANCE-02]`) is a different mechanism with a different job —
   it is how a row admits what it could not print — and it stays.

## Consequences

- **Positive**:
  - 13 columns return to the tab rail at every width: tab titles truncate later
    and fewer tabs fall behind the `‹N` / `N▾` overflow window.
  - The status cluster is now purely conditional, so a quiet session has an
    empty right edge and the tab rail can bleed into it.
  - Help discovery is taught where it can be *argued for* ("every shortcut"),
    not merely labelled.
- **Negative & Trade-offs**:
  - A session that already has messages shows no on-screen help affordance at
    all. Discovery depends on the tour, `/help`, or remembering `F1`; the
    mitigation is that the tour rotates within seconds on the surface every new
    session starts on.
  - Users who navigated by clicking `F1 help` must press `F1` (or run `/help`).
    There is no pointer path to the Help dialog while a transcript exists.
  - ADR-0092's argument that the chord-free SceneBar is safe because "`F1` /
    `Alt-h` is a standing TabBar affordance" no longer holds literally; the
    affordance now lives in the tour.

## Negative Knowledge

Do not re-pin a constant keycap (or any constant legend) to the TabBar to
"solve" discoverability. The row exists to name open tabs; every fixed
affordance on it is paid for by tab titles in every scene and at every width.
When a capability needs advertising, the empty state is the cheap surface —
it rotates, it has prose, and it disappears the moment the user no longer needs
to be taught. If discoverability must be restored inside a live session, prefer
a conditional signal (something appears when there is something to say) over a
standing one.
