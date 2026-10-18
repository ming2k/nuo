---
id: ADR-0077
title: "Workspace Namespace Pure Surface Topology, Legacy Stroke Pruning, and Categorized Palette Architecture"
status: accepted
date: 2026-11-20
scope: tui/nuo-tui, interaction/keymap, presentation/chrome, interaction/command-palette
superseded_by: null
negative_knowledge: true
---

# 0077. Workspace Namespace Pure Surface Topology, Legacy Stroke Pruning, and Categorized Palette Architecture

- Status: Accepted
- Date: 2026-11-20
- Deciders: Nuo Architecture Working Group
- Consulted: Presentation (`nuo-tui`), Interaction Design, Runtime Governance
- Informed: System Architects, Release Engineering
- Amends: [ADR-0023](0023-scene-namespace-palette-entry-and-borderless-popups.md), [ADR-0040](0040-cross-domain-action-matrix-and-polymorphic-tui-spatial-topology.md), [ADR-0043](0043-thread-domain-nomenclature-unification-compact-tabbar-affordance-and-c-x-namespace-reorganization.md)

---

## Context and Problem Statement

The `C-x` scene namespace was established to provide a collision-free, two-stroke prefix for workspace orchestration (ADR-0023, ADR-0043). Over successive iterations, two forms of legacy baggage accumulated within this namespace and the command discovery layer:

1. **Semantic Category Pollution (`Quit` in Workspace Namespace)**:
   The `C-x` table retained `SceneVerb::Quit` bound to `C-c` (`C-x C-c`), mirroring a 40-year-old Emacs convention. In a modern tabbed terminal workspace, process termination is a client lifecycle event, not a viewport management action. Housing an armed application exit inside what is advertised as a workspace menu (`menu C-x`) created category pollution and occupied a valuable slot on the which-key card. Operators already possess canonical lifecycle exits via the global `Ctrl-C` chord and `/exit`.
2. **Ambiguous and Colliding Destructive Aliases (`k` for Close Tab)**:
   `SceneVerb::Leave` permitted both `w` and `k` (`C-x w` and `C-x k`). In Unix terminal culture, `k` is historically associated with `kill-buffer` or vi cursor movement, while modern windowing and browser environments uniformly standardize on `w` (`Alt+W`, `Ctrl+W`, `Cmd+W`). Maintaining `k` created cognitive friction, raised risks of accidental buffer kills, and blocked `k` from future mnemonic re-use.
3. **Command Palette Domain Jumble**:
   `C-x p` was designed as the universal fallback discovery engine. However, its entries were sorted purely alphabetically when unqueried, intermingling viewport navigation targets (`Session Dashboard`, `Settings`) with operational commands (`Interrupt Task`, `Quit Nuo`, `Close Tab`). Operators lacked immediate visual feedback on the domain type of each item and lacked quick facet filters.

A clean break from these historical anachronisms is required to align the interaction model with pure workspace management.

---

## Decision Drivers

- **Domain Hermeticism**: The `C-x` namespace must represent exclusively workspace surface lifecycle verbs (`Switcher`, `Dashboard`, `Settings`, `Threads`, `Leave`). Process exit is strictly a client lifecycle concern.
- **Uncompromising Ergonomics**: Modern tab navigation standardizes on `w` for tab dismissal (`Alt+W`, `C-x w`). Redundant destructive aliases (`k`) must be eliminated.
- **Universal Discovery Without Noise**: The Command Palette must remain the universal search fall-back for all surfaces and actions, but entries must visually declare their `CommandCategory` and support ergonomic prefix filtering (`@views`, `/slash`, `>actions`).
- **Zero Accidental Exits**: Both bare `c` and `C-c` following `C-x` must be inert, preventing accidental shutdowns from incomplete leader chords.

---

## Considered Options

### Option 1: Retain `Quit` and `k` for Legacy Compatibility
- Keep `C-x C-c` and `C-x k` to accommodate legacy Emacs muscle memory.
- *Assessment*: Rejected per **[INV-AGENT-01]**. Perpetuates historical baggage and pollutes the clean workspace model. Emacs users already default to global `Ctrl-C` in modern terminals.

### Option 2: Split Command Palette and Navigation into Disjoint Modals
- Remove navigation entries from `C-x p` and create a second modal (`C-x n`) solely for views.
- *Assessment*: Rejected. Degrades discoverability by creating 0-result dead ends when searching for "settings" or "dashboard", and incurs unnecessary UI bloat for a set of 3 primary tabs that already possess direct 2-stroke chords (`C-x d`, `C-x ,`).

### Option 3: Pure Workspace Namespace with Categorized Unified Palette (Chosen)
- Prune `SceneVerb::Quit` and excise `k` from `SceneVerb::Leave`.
- Standardize `C-x` on 5 pure surface verbs: `p` (Palette), `d` (Dashboard), `,` (Settings), `s` (Threads), `w` (Close Tab).
- Fortify the Command Palette with explicit `CommandCategory` badges and sigil-based prefix scoping (`@`, `/`, `>`).

---

## Architectural Decisions

### 1. Pure Surface Workspace Namespace (`[INV-TUI-SURF-01]`)

`SceneVerb::ALL` is strictly constrained to five surface lifecycle operations:

| Second Stroke | Verb Variant | Description | Semantic Action |
| :--- | :--- | :--- | :--- |
| **`p`** | `Switcher` | `command palette` | Open unified discovery palette |
| **`d`** | `Dashboard` | `dashboard` | Focus or mount singleton Dashboard tab |
| **`,`** | `Settings` | `settings` | Focus or mount singleton Settings tab |
| **`s`** | `Threads` | `threads` | Open persistent thread store picker dialog |
| **`w`** | `Leave` | `close tab` / `close overlay` | Close active foreground modal or tab |
| `Esc` | *(Floor)* | `cancel` | Cancel namespace arming |

- **Excision of `k`**: `SceneVerb::Leave` accepts exclusively `KeyCode::Char('w')`. Striking `k` while the namespace is armed is unrecognized and triggers immediate safe cancellation (`InputAction::CancelSceneNamespace`).
- **Excision of `Quit`**: `SceneVerb::Quit` is deleted. Striking `C-c` or `c` while armed cancels the namespace without terminating the client. Process lifecycle is governed exclusively by global `Ctrl-C` (with double-press confirmation) and `/exit`.

### 2. Categorized Command Palette & Prefix Facets

The Command Palette retains universal discoverability across all client capabilities, enhanced by domain stratification:

1. **Category Tagging**: Each row in `draw_command_palette` renders a compact, dim category badge (`[nav]`, `[settings]`, `[session]`, `[action]`) derived from `CommandCategory`.
2. **Category-Aware Default Ranking**: When the search query is empty, entries are stably grouped by domain rather than a flat alphabetical collision.
3. **Prefix Filtering**:
   - Query prefix **`@`**: Scopes results strictly to `CommandCategory::Navigate` and `CommandCategory::Settings` (e.g. `@dash`, `@settings`).
   - Query prefix **`/`**: Scopes results strictly to commands with slash triggers.
   - Query prefix **`>`**: Scopes results strictly to global and session actions.
   - Unprefixed text continues to execute fuzzy full-text discovery across all labels, descriptions, and shortcuts.

---

## Invariants & Behavioral Boundaries

- **`[INV-TUI-SURF-01]` Pure Workspace Namespace**:
  The `SceneVerb` table MUST NOT declare process termination or non-workspace verbs. All second strokes in `SceneVerb::ALL` MUST map 1:1 to surface navigation, overlay presentation, or viewport closure. Bare `c`, `C-c`, and `k` MUST remain completely inert within the `C-x` prefix.

---

## Rejected Alternatives & Negative Knowledge

1. **Rejected: Repurposing `k` as a Routing Modal Leader (`C-x k`)**
   - *Reason*: `k` has zero mnemonic correlation with routing and carries 40 years of destructive `kill-buffer` expectation. Re-assigning it to open a navigation dialog causes severe cognitive dissonance and risks destructive accidents.
2. **Rejected: Removing `threads` (`C-x s`) from the Namespace**
   - *Reason*: `threads` represents the server-side persistent conversation repository (ADR-0038). It is not redundant with in-memory tabs (`Alt+1..9`). Because slash commands are hermetic to the active composer (ADR-0041), `C-x s` is the sole universal mechanism to summon the thread store from non-chat scenes (Dashboard, Settings).

---

## Operational Consequences & Migration Path

- **Zero Breaking Disruption to Tab Dismissal**: Modern users already use `Alt+W` or `C-x w`.
- **Card Geometry Reclamation**: The `which-key` card height reduces by 1 row, reducing visual footprint on small terminal displays.
- **Enhanced Search Flow**: Searching in the Command Palette immediately clarifies whether an item is a navigation destination or an operational command.

---

## Links

- Supersedes / Amends: [ADR-0023](0023-scene-namespace-palette-entry-and-borderless-popups.md), [ADR-0040](0040-cross-domain-action-matrix-and-polymorphic-tui-spatial-topology.md), [ADR-0043](0043-thread-domain-nomenclature-unification-compact-tabbar-affordance-and-c-x-namespace-reorganization.md)
- Related Modules: `nuo-tui::keymap`, `nuo-tui::input::router`, `nuo-tui::components::which_key`, `nuo-tui::overlays::command_palette`
