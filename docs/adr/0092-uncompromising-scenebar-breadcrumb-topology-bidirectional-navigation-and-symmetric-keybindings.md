---
id: ADR-0092
title: "Uncompromising SceneBar Breadcrumb Topology, Bidirectional Navigation, and Symmetric Keybindings"
status: accepted
date: 2026-11-26
scope: tui/nuo-tui, architecture/spatial, presentation/chrome, interaction/keymap
superseded_by: null
negative_knowledge: true
---

# 0092. Uncompromising SceneBar Breadcrumb Topology, Bidirectional Navigation, and Symmetric Keybindings

- Status: Accepted
- Date: 2026-11-26
- Deciders: Core Engineering, Architecture Working Group
- Amends: [ADR-0040](0040-cross-domain-action-matrix-and-polymorphic-tui-spatial-topology.md), [ADR-0042](0042-client-viewport-routing-ownership-tab-history-and-scene-self-projection.md), [ADR-0060](0060-three-tier-spatial-header-topology-governance-band-and-clean-tab-stack.md), [ADR-0062](0062-polymorphic-scenebar-projection-sovereign-thread-context-and-consolidated-activity-strip.md), [ADR-0075](0075-orthogonal-workspace-container-topology-and-intra-tab-scene-discipline.md)

---

## Context and Problem Statement

Following the establishment of the Three-Tier Spatial Header ([ADR-0060](0060-three-tier-spatial-header-topology-governance-band-and-clean-tab-stack.md), [ADR-0062](0062-polymorphic-scenebar-projection-sovereign-thread-context-and-consolidated-activity-strip.md)), Intra-Tab Scene Discipline ([ADR-0075](0075-orthogonal-workspace-container-topology-and-intra-tab-scene-discipline.md)), and Tab Context Isolation ([ADR-0091](0091-sovereign-tab-context-isolation-and-tabbar-boundary-purity.md)), several legacy compromises and ergonomic anomalies persisted in the visual presentation and keyboard/pointer navigation of Tier 2 (`SceneBar`):

1. **The `< thread > subagent` Pseudo-Tag Conflation**:
   In `crates/nuo-tui/src/view_header.rs`, the drill-in navigation stack rendered a leading Back affordance as `"< "` followed by ancestor and current crumbs joined with `" > "`. In monospace terminals, this concatenated into `< thread > subagent`. Operators consistently misperceived `< thread >` as an XML/HTML tag or an interactive horizontally cycling carousel/dropdown rather than an authentic ancestor breadcrumb. Furthermore, using ASCII `<` and `>` violated the project's typographic design standards ([ADR-0062], `design.rs`), which mandate canonical single guillemets `›` (`\u{203A}`) for hierarchy and `‹` (`\u{2039}`) for navigation.

2. **Shortcut Inflation & Modal Fragmentation**:
   Intra-tab stack navigation suffered from fragmented, ad-hoc keybindings accumulated across evolutionary layers:
   - `Alt+Left` / `Alt+Right`: Borrowed from desktop web browser metaphors ([ADR-0040]), but notoriously intercepted by major terminal emulators (macOS Terminal, iTerm2, Readline) as word-jump gestures (`backward-word` / `forward-word`).
   - Bare `[` / `]`: Borrowed from Vim pager conventions, but because they are printable characters, they required brittle modal gates ("only when composer is empty and no interactive target is focused"), preventing users from starting draft prompts with markdown links (`[text](url)`) or code arrays.
   - Ad-hoc `q`: Injected exclusively into `subagent` view as a pager quit hack, failing to work in any other drill-in depth and colliding with input buffers.
   - Overloading `Esc`: Attempting to use `Esc` for viewport scene exits was explicitly rejected by **`[INV-TUI-CLEAN-02]`** because `Esc` is the universal in-place cancellation chord (interrupting model rounds, clearing tool focus, dismissing autocompletion, dropping text selections); co-opting it for scene exits caused accidental context loss and disorientation.

3. **Passive SceneBar Chrome (Zero Pointer Hit Testing)**:
   While Tier 1 (`TabBar`) supported full pointer hit-testing via `UiKey::TabBarItem(index)`, Tier 2 (`SceneBar`) was implemented as purely passive rendering. Clicking the Back affordance (`‹`) or clicking an ancestor breadcrumb (`thread`) was completely inert, failing to register interaction rects in `ComponentTree` and creating a jarring discrepancy between mouse affordances across header rows.

A clean break from these legacy burdens is required to establish a mathematically sound, self-describing, and ergonomic navigation architecture.

---

## Decision Drivers

- **Zero Visual Ambiguity**: Strictly eradicate pseudo-tag artifacts (`< thread >`). All hierarchy must use canonical single guillemets (`›`), and navigation affordances must explicitly declare their function and keychord.
- **Symmetric & Collision-Free Keybindings**: Eliminate un-modified bracket gating and view-specific single-letter hacks. Elevate `Alt+[` and `Alt+]` to the single authoritative, collision-free canonical keypair for bidirectional intra-tab stack navigation.
- **Pointer-Keyboard Parity**: Elevate SceneBar breadcrumbs and navigation affordances to first-class interactive hit targets (`UiKey::SceneHistoryBack`, `UiKey::SceneHistoryForward`, `UiKey::SceneBreadcrumb(usize)`).
- **Preservation of `[INV-TUI-CLEAN-02]`**: Keep `Esc` strictly dedicated to in-place cancellation and focus clearing, never allowing it to trigger scene exits.

---

## Architectural Decisions

```text
══════════════════════════════════════════════════════════════════════════════════════════
  TIER 1 CONTAINER CHROME: TabBar (Row 0) — Workspace Containers (Alt+1..9, Click)
══════════════════════════════════════════════════════════════════════════════════════════
  [Thread: Refactor DB]  [Dashboard]  [Settings]                                 C-x menu
──────────────────────────────────────────────────────────────────────────────────────────
  TIER 2 SCENE CHROME: SceneBar (Row 1) — Task Identity & Canonical Breadcrumb Trail
══════════════════════════════════════════════════════════════════════════════════════════
  THREAD b3c4  Fix retry logic in auth loop            ‹ Alt+[  thread › subagent
                                                       ▲        ▲        ▲
                                                       │        │        └─ Leaf (Active)
                                                       │        └─ Ancestor (Clickable)
                                                       └─ Back Target (Clickable)
──────────────────────────────────────────────────────────────────────────────────────────
  TIER 3 GOVERNANCE CHROME: ThreadContextBar (Row 2, Conditional)
══════════════════════════════════════════════════════════════════════════════════════════
  [DEVELOPER]  auto  open                                                  ~/projects/nuo
```

### 1. Canonical Breadcrumb Formatting (`[INV-SCENE-03]`)
- When intra-tab drill-in depth > 1, the right side of `SceneBar` renders:
  - If backward navigation is possible (`can_back`): A distinct, muted Back affordance `‹ ` — a **pure chevron**, with a dedicated hit target.
  - The breadcrumb path: Ancestor items rendered in muted tone (`theme.dim()`), joined by canonical single right-pointing angle quotation marks (` › `, `\u{203A}`).
  - Active leaf: The current scene label rendered in bold brand style (`label_style`), without trailing separators.
  - If forward navigation is possible (`can_forward`): A distinct, muted Forward affordance ` ›` with a dedicated hit target.
- Pseudo-tag delimiters (`< ... >`) and raw ASCII arrows are permanently abolished.
- **No chord text on the bar.** The affordances name *where you can go*, not *what to press*: a printed `Alt+[` is a hardcoded copy of a remappable binding and a second discovery surface competing with the Help dialog (ADR-0131 §3). Chords live in `DialogKind::Help`, resolved from the live keymap so a user remap is reflected.

### 2. Symmetric Modifier Navigation (`[INV-NAV-03]`)
- Bidirectional intra-tab scene navigation is canonically and exclusively bound to:
  - **Back**: `Alt+[` (`Key::ALT_BRACKET_LEFT`)
  - **Forward**: `Alt+]` (`Key::ALT_BRACKET_RIGHT`)
- `Alt+Left` and `Alt+Right` are permanently retired from viewport routing, fully restoring standard Readline word-skipping (`backward-word` / `forward-word`) in text fields and eliminating terminal emulator interception conflicts.
- Raw un-modified brackets (`[` / `]`) and ad-hoc scene-local `q` exits are permanently retired:
  - Typing `[` or `]` into an empty or non-empty composer always inserts the literal character without modal gating.
  - `Alt+[` and `Alt+]` bypass text editing entirely, triggering clean navigation regardless of draft state or focus mode.
- In accordance with **`[INV-TUI-CLEAN-02]`**, `Esc` remains strictly non-navigational.

### 3. Interactive Scene Navigation Targets (`[INV-INTERACT-01]`)
- `crates/nuo-tui/src/ui.rs` introduces first-class hit keys:
  - `UiKey::SceneBar`: Container background (Layer 19, inert click, cancels drag).
  - `UiKey::SceneHistoryBack`: The `‹ Alt+[` affordance (Layer 20, executes `app.tab_history_back()`).
  - `UiKey::SceneHistoryForward`: The `Alt+] ›` affordance (Layer 20, executes `app.tab_history_forward()`).
  - `UiKey::SceneBreadcrumb(usize)`: Ancestor crumb at index `d` (Layer 20). Clicking an ancestor unwinds the stack to depth `d`.
- `draw_scene_bar` computes exact character cell bounding boxes (`SceneNavRects`) and mounts them in the frame transaction.

---

## Normative Invariants Established

- **`[INV-SCENE-03] Canonical Breadcrumb Hierarchy`**: Drill-in navigation paths on Tier 2 SceneBar MUST render ancestors and leaves separated by ` › ` (`\u{203A}`). The pseudo-tag bracket format (`< thread > subagent`) is strictly prohibited. The bar carries **no chord text**: the back and forward affordances are pure chevrons (`‹` / `›`), because a printed chord is a copy of a binding the user can remap and it duplicates the Help dialog's single discovery surface (ADR-0131 §3).
- **`[INV-NAV-03] Symmetric Modifier Navigation`**: Bidirectional intra-tab scene stack navigation MUST be canonically and exclusively bound to `Alt+[` (Back) and `Alt+]` (Forward). Legacy arrow shortcuts (`Alt+Left` / `Alt+Right`), raw un-modified bracket shortcuts (`[` / `]`), and ad-hoc single-letter `q` exits are prohibited.
- **`[INV-INTERACT-01] Interactive Scene Navigation Targets`**: Tier 2 SceneBar navigation affordances (`‹` and `›`) and ancestor breadcrumb items MUST expose interactive hit targets in `ComponentTree`. Clicking them MUST deterministically navigate the active tab's scene stack.

---

## Consequences

- **Positive**:
  - Immediate visual clarity: Operators recognize the breadcrumb trail instantly as an authentic hierarchy, eliminating the `< thread >` pseudo-tag illusion.
  - Zero modal guessing: Developers can freely type markdown links `[foo](bar)` or indexing syntax `arr[0]` without accidental viewport navigation.
  - Seamless mouse interaction: Clicking `‹` or clicking `thread` returns to the parent thread immediately.
  - macOS & Linux terminal safety: `Alt+[` and `Alt+]` are immune to readline word-skipping (`backward-word`) interception that plagued `Alt+Left`/`Alt+Right`.
  - Layout honesty: without the chord text the breadcrumb row is 8 columns narrower per affordance, and a remapped `session.scene_back` chord can no longer leave stale `Alt+[` text on screen.
- **Negative & Trade-offs**:
  - Operators accustomed to pressing `q` in subagent view must adapt to `Alt+[` (or mouse clicking `‹`).
  - The chords are no longer self-describing from the header. This is a deliberate trade: `F1` / `Alt-h` is a standing TabBar affordance on every row, and the Help dialog's dedicated **Scene Navigation** block documents the chords *resolved from the live keymap* — which the header never could.

---

## Rejected Alternatives & Negative Knowledge

- **Rejected: Overloading `Esc` for Scene Stack Exits**:
  Using `Esc` to step back in the scene stack was rejected because it directly violates `[INV-TUI-CLEAN-02]`. When browsing subagents, users frequently press `Esc` to cancel step selection, close autocompletion, or drop text selections. Converting `Esc` into a viewport navigation verb causes accidental workspace disruption and loss of inspection context.
- **Rejected: Retaining Raw `[` / `]` with Empty-Composer Guards**:
  Allowing un-modified brackets to navigate when the composer is empty was rejected as a fragile anti-pattern. It creates an invisible state machine where a key's behavior flips between typing text and jumping viewports depending on whether whitespace exists in an input field.
