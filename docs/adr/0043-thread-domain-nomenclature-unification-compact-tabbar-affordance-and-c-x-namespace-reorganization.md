---
id: ADR-0043
title: "Thread Domain Nomenclature Unification, Compact TabBar Affordance, and C-x Namespace Reorganization"
status: accepted
date: 2026-10-20
scope: tui/nuo-tui, architecture/taxonomy, interface/cli, presentation/chrome, interaction/keyboard
superseded_by: null
negative_knowledge: true
---

# 0043. Thread Domain Nomenclature Unification, Compact TabBar Affordance, and C-x Namespace Reorganization

- Status: Accepted
- Date: 2026-10-20
- Deciders: Nuo Architecture Working Group
- Consulted: Interface, Usability, Terminal, Runtime, and Core Infrastructure Teams
- Informed: System Architects, Release Engineering
- Complements: [ADR-0038](0038-thread-entity-server-ssot-fanout-and-orthogonal-action-lifecycle.md), [ADR-0039](0039-client-tab-workspace-scene-unification-and-reactive-surface-sync.md), [ADR-0040](0040-cross-domain-action-matrix-and-polymorphic-tui-spatial-topology.md), [ADR-0041](0041-thread-command-purity-viewport-workspace-separation-and-tab-management-topology.md), [ADR-0042](0042-client-viewport-routing-ownership-tab-history-and-scene-self-projection.md)

---

## Context and Problem Statement

Following the establishment of the persistent Thread entity model ([ADR-0038](0038-thread-entity-server-ssot-fanout-and-orthogonal-action-lifecycle.md)), the unified Client Tab Workspace ([ADR-0039](0039-client-tab-workspace-scene-unification-and-reactive-surface-sync.md)), and the Three-Tier Header Topology ([ADR-0040](0040-cross-domain-action-matrix-and-polymorphic-tui-spatial-topology.md)), several legacy inconsistencies and ergonomic friction points remained across user-facing layers:

1. **Terminology Divergence (Session vs. Thread Leakage)**:
   While the server SSOT and persistence layer formally standardized on `Thread` (`[INV-THREAD-01]`), user-facing and TUI interaction layers suffered from incomplete migration. Operators were presented with `1:thread-xxxx` in the TabBar, yet confronted with `/sessions` slash commands, `nuo session list` CLI verbs, and `C-x s -> sessions` which-key descriptions. This terminology fracture blurred the boundary between persistent agent threads and ephemeral connections.

2. **Inverted Visual Hierarchy and Wasteful TabBar Keycap Geometry**:
   On Row 1 (Client Global TabBar), the right-hand client menu entry was rendered as `Ctrl-x menu` spanning 11 horizontal columns. The prominent bold key token (`Ctrl-x`) preceded the subdued label (`menu`), drawing excessive operator gaze away from active tab identifiers. Furthermore, using the verbose 6-character `Ctrl-x` rather than the standardized compact `C-x` consumed precious horizontal space in constrained 80-column terminal environments.

3. **Incoherent `C-x` Client Namespace and Missing Core Workspace Bindings**:
   The `C-x` two-stroke namespace was originally designed as a scene lifecycle shortcut ([ADR-0023](0023-scene-namespace-palette-entry-and-borderless-popups.md)), but was promoted to the Client Global Workspace Menu in ADR-0040. Despite documentation and command registries citing `C-x ,` for `Settings`, this binding was omitted from the `SceneVerb` table, making direct keyboard navigation to Settings impossible. Furthermore, `SceneVerb::Leave` continued to advertise "leave scene", which conflicted with the modern tabbed workspace paradigm.

We require a holistic, zero-legacy harmonization that establishes uniform `Thread` domain nomenclature across all CLI, slash, dialog, and keymap surfaces, rationalizes the `C-x` workspace namespace, and introduces a compact "Label-first, Dim-chord" TabBar affordance.

---

## Decision Drivers

- **Domain Nomenclature Purity (`[INV-THREAD-02]`)**: Eliminate all user-facing references to "session" when describing persistent multi-instance agent threads. Slash commands, CLI utilities, dialog titles, and which-key hints must uniformly reflect `Thread` / `Threads`.
- **Compact Cognitive Affordance (`[INV-UI-02]`)**: The TabBar client menu affordance must prioritize concept recognition over mechanical invocation: prominent action label on the left, muted/dim shortcut on the right, formatted as `menu C-x` to save horizontal real estate.
- **Hermetic Workspace Namespace Completion (`[INV-NAMESPACE-01]`)**: The `C-x` two-stroke family must represent the canonical Client Global Workspace Namespace, wiring all singleton views (`p` Palette, `d` Dashboard, `,` Settings, `s` Threads) and tab lifecycle actions (`w` Close Tab, `C-c` Quit) deterministically.

---

## Considered Options

### Option 1: Incremental Aliases without Renaming
- Keep `/sessions`, `DialogKind::Sessions`, and `nuo session` as the primary implementations, adding `/threads` merely as an undocumented alias.
- *Assessment*: Rejected. Preserves terminology debt and institutional confusion. New operators will continue to encounter split naming.

### Option 2: Full Radical Renaming with Backward Compatibility Guarantees (Chosen)
- Formally rename the canonical slash command to `/threads`, the interactive picker to `ThreadsDialog`, the CLI command to `nuo thread`, and the which-key action to `threads`.
- Maintain legacy aliases (`/sessions`, `/session`, `/resume`) for muscle memory backward compatibility without advertising them.
- Invert and compact the TabBar affordance to `menu C-x` (8 columns), applying `theme.brand()`/bold to `menu` and `theme.dim()` to `C-x`.
- Complete the `C-x` namespace table: wire `Settings` (`C-x ,`), retitle `Leave` to `close tab`, and ensure all singleton workspaces have dedicated two-stroke bindings.

---

## Rejected Alternatives

Per **[INV-AGENT-01]**, the following alternative architectures were evaluated and rejected:

1. **Rejected: Renaming Transport and Authentication Sessions to "Threads"**
   - *Reason*: Violates engineering separation of concerns. HTTP/WebSocket network connections and OAuth authorization grants are authentic network/security *sessions*, not agent conversation *threads*. Renaming socket handles or OAuth states to "threads" would reintroduce the inverted category confusion.
2. **Rejected: Retaining `Ctrl-x` Notation on the TabBar**
   - *Reason*: In constrained terminal rows, every character counts. `Ctrl-x` consumes 6 columns compared to 3 for `C-x`. Nuo's design vocabulary already standardizes on Emacs-style chord representation (`C-x`, `M-w`) throughout keymaps and documentation.
3. **Rejected: Binding Settings to `C-x c` Instead of `C-x ,`**
   - *Reason*: `C-x c` easily misfires as `C-x C-c` (the universal Nuo and Emacs quit chord). Keeping bare `c` inert prevents catastrophic accidental process terminations, preserving the safety invariant established in ADR-0023.

---

## Decision Outcome

Chosen option: **Option 2**.

### 1. Canonical Thread Nomenclature (`[INV-THREAD-02]`)

1. **Slash Commands**:
   - The primary command for inspecting and switching threads is **`/threads`** (Category: `Thread`).
   - Legacy aliases `/sessions`, `/session`, and `/resume` map to `BuiltinCmd::Threads` for backward compatibility.
2. **Modal Dialog**:
   - `DialogKind::Sessions` and `SessionsDialog` are renamed to `DialogKind::Threads` and `ThreadsDialog`.
   - The surface title renders as **`Threads`**, and the footer hint renders `/threads`.
3. **CLI Interface**:
   - `nuo thread list`, `nuo thread delete`, and `nuo thread inspect` become the canonical verbs. `nuo session` remains a deprecated alias.

### 2. Compact TabBar Affordance Topology (`[INV-UI-02]`)

On Row 1 (Client Global TabBar), the right-aligned affordance is restructured:

```text
Prior Layout (11 cols, inverted focus):
[1:thread-a1b2]  [2:dashboard]                   Ctrl-x menu
                                                 ^^^^^^ (bold, prominent chord)

Modernized Layout (8 cols, concept-first, -3 cols saved):
[1:thread-a1b2]  [2:dashboard]                      menu C-x
                                                    ^^^^ (brand, clear label)  ^^^ (dim hint)
```

- **Width**: Reduced from 11 columns to 8 columns (`menu C-x`).
- **Styling**: `menu` renders with label emphasis; `C-x` renders with subtle/dim styling (`theme.dim()`), preventing visual capture.

### 3. Reorganized `C-x` Client Workspace Namespace (`[INV-NAMESPACE-01]`)

The two-stroke `C-x` table is structured deterministically around Client Workspace peers:

| Second Stroke | Verb Variant | Which-Key Description | Semantic Action |
| :--- | :--- | :--- | :--- |
| **`p`** / `b` | `Switcher` | `command palette` | Summon unified command palette / surface switcher. |
| **`d`** | `Dashboard` | `dashboard` | Focus or mount singleton Dashboard tab. |
| **`,`** | `Settings` | `settings` | Focus or mount singleton Settings tab. |
| **`s`** | `Threads` | `threads` | Summon interactive Threads switcher dialog. |
| **`w`** / `k` | `Close` | `close tab` | Dismiss active dialog or detach current tab. |
| **`C-c`** | `Quit` | `quit nuo` | Armed process termination. |
| `Esc` | *(Floor)* | `cancel` | Cancel namespace arming. |

---

## Invariants & Behavioral Boundaries

- **[INV-THREAD-02] Universal Thread Nomenclature**: User-facing documentation, command lists, and UI modals MUST designate persistent multi-instance agent dialogs as `Thread` or `Threads`. The term `Session` MUST NOT be exposed in conversational UX.
- **[INV-UI-02] Label-First Compact TabBar Affordance**: The TabBar right affordance MUST render the semantic label before the key hint, abbreviated as `menu C-x`, occupying no more than 8 columns.
- **[INV-NAMESPACE-01] Complete Workspace Peerage in C-x**: Every top-level singleton tab (`Dashboard`, `Settings`) and global discovery surface (`Palette`, `Threads`) MUST possess an explicit first-class binding within `SceneVerb`. Bare `c` MUST remain inert.

---

## Operational Consequences & Migration Path

- **Zero Breaking Workflow Disruption**: Operators retaining `/sessions` or `/resume` in muscle memory experience zero disruption due to transparent alias routing.
- **Screen Real Estate Reclamation**: Narrow viewports gain 3 additional text columns on Row 1, allowing longer thread identifiers and reducing tab title truncation.
- **Cohesive Cognitive Model**: Operators interact with `threads` consistently across CLI, TUI, slash commands, and keyboard overlays.
