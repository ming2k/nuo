---
id: ADR-0051
title: "Composer Canonical Quadrant Modality, Unified Filter Engine, and Atomic Draft Capsule Topology"
status: accepted
date: 2026-10-26
scope: tui/nuo-tui, architecture/composer, interaction/modality, input/routing
superseded_by: null
negative_knowledge: true
---

# 0051. Composer Canonical Quadrant Modality, Unified Filter Engine, and Atomic Draft Capsule Topology

- Status: Accepted
- Date: 2026-10-26
- Deciders: Nuo Architecture Working Group
- Consulted: TUI Presentation, Terminal Infrastructure, Usability Working Group
- Informed: System Architects, Release Engineering
- Amends: [ADR-0044](0044-composer-native-history-search-ghost-preview-and-draft-stash-topology.md)
- Complements: [ADR-0035](0035-domain-scoped-surface-architecture-and-encapsulated-dialog-lifecycle.md), [ADR-0040](0040-cross-domain-action-matrix-and-polymorphic-tui-spatial-topology.md), [ADR-0047](0047-autonomous-surface-interaction-architecture-and-spatial-event-delegation.md)

---

## Context and Problem Statement

In [ADR-0044](0044-composer-native-history-search-ghost-preview-and-draft-stash-topology.md), we successfully eliminated the dual-input schizophrenia of `HistorySearch` by positioning the Composer as the sole physical text entry host in thread view (`[INV-HIST-01]`). However, as the Composer grew to incorporate multiple interaction patterns—multi-line prompt drafting, in-place session prompt stepping, cross-session history search, slash command completion, mention expansion, and rich attachment chips—a deeper structural fracture emerged:

1. **Blurry Modal Boundaries and "Inline" Nomenclature Confusion**:
   The code and design conflated sequential history cursor traversal (`↑`/`↓`) with overlay search (`Ctrl-R`). The former was ambiguously termed "inline recall" (a misnomer in Chinese and conceptual design, given that AI prompts are multi-line and the traversal replaces the entire buffer in-place). Operators lacked a clear mental model distinguishing **in-place sequential buffer stepping** (session-scoped) from **overlay fuzzy querying** (cross-session).

2. **The Frankenstein Extension-Dialog Hybrid**:
   `HistorySearch` was caught between two conflicting architectural paradigms: it remained an entry in `DialogKind` on the global `overlay_stack` ([ADR-0035](0035-domain-scoped-surface-architecture-and-encapsulated-dialog-lifecycle.md)), yet it operated as a `ComposerExtension` ([ADR-0044](0044-composer-native-history-search-ghost-preview-and-draft-stash-topology.md)) borrowing the composer's text buffer. This hybrid posture caused key routing anomalies: while inside `HistorySearch`, pressing `Ctrl-R` again was swallowed as an inert no-op (`InputAction::None`), breaking the universal shell muscle memory (Bash / Zsh / Fzf) of cycling to next/older matches.

3. **Ad-Hoc State Sprawl in Application Root**:
   Draft saving across different transient actions resulted in ad-hoc field sprawl across the `App` struct (`history_draft`, `history_draft_images`, `history_draft_text_pastes`, `injection_stashed_input`, `history_index`, `suggestion_index`, `completion_dismissed`). Without a formal state capsule, intermediate state preservation was brittle and lacked a uniform lifecycle protocol.

We require an uncompromising, forward-looking architecture that formally establishes the **Canonical Quadrant Modality** for the Composer, decouples in-place history stepping from overlay search, unifies candidate filtering, enables continuous `Ctrl-R` match cycling, and encapsulates draft preservation in an atomic capsule.

---

## Decision Drivers

- **Canonical Quadrant Modality (`[INV-COMP-01]`)**: The Composer's interaction space must be strictly partitioned into four mutually exclusive, concisely named canonical modes: `Edit`, `Recall`, `Search`, and `Complete`.
- **In-Place Stepping vs. Overlay Search Orthogonality (`[INV-COMP-02]`)**: In-place session history traversal (`Recall`) and cross-session fuzzy search (`Search`) are distinct architectural domains with separate triggers, visual footprints, and keymap bindings.
- **Atomic Draft Capsule Lifecycle (`[INV-COMP-03]`)**: Entering any transient sub-mode (`Recall`, `Search`) must automatically capture a structured `DraftCapsule` (text, cursor byte, staged attachments). Cancellation guarantees 100% loss-free atomic rollback; confirmation guarantees safe adoption.
- **Cyclic Match Discovery Parity (`[INV-COMP-04]`)**: When in `Search` mode, repeated activation chords (`Ctrl-R`) must cycle through ranked candidate entries, matching standard terminal expectations.

---

## Considered Options

### Option 1: Ad-hoc Flag Patching (Status Quo)
- Keep adding boolean flags and dialog downcasts to `router.rs` and `App`.
- *Assessment*: Rejected. Escalates technical debt, increases keymap collision risk, and perpetuates modal ambiguity.

### Option 2: Heavyweight Modal Subsystem (Vim-Style Architecture)
- Introduce a full multi-mode text engine with Normal, Visual, Insert, and Command modes.
- *Assessment*: Rejected. Irrelevant to current system requirements. Imposes severe cognitive friction and mode fatigue on prompt formulation without solving the prompt augmentation and history borrowing collision.

### Option 3: Canonical Quadrant Modality with Atomic Draft Capsule and Unified Interception (Chosen)
- Establish four crisp, mutually exclusive canonical modes:
  1. **`Edit`**: The foundational free-text drafting state (Readline key conventions).
  2. **`Recall`**: In-place session prompt stepping via `↑`/`↓`/`Alt-P`/`Alt-N`. Replaces buffer in-place without overlay chrome.
  3. **`Search`**: Global fuzzy history query via `Ctrl-R`. Borrows composer as needle input, projects candidate drawer, supports cyclic `Ctrl-R` stepping.
  4. **`Complete`**: Inline token-attached completion (`CompleteKind::Command` for `/`, `CompleteKind::Context` for `@`).
- Encapsulate all stashed drafts into an atomic `DraftCapsule`.
- Enable cyclic `Ctrl-R` candidate stepping inside `Search`.

---

## Decision Outcome

Chosen: **Option 3: Canonical Quadrant Modality with Atomic Draft Capsule and Unified Interception**.

### Invariants & Behavioral Boundaries

- **`[INV-COMP-01] Canonical Quadrant Modality SSOT`**: At any instant, the Composer belongs to exactly one canonical mode: `ComposerMode::Edit`, `ComposerMode::Recall`, `ComposerMode::Search`, or `ComposerMode::Complete`. Key dispatch and hint rendering are strictly determined by this mode.
- **`[INV-COMP-02] In-Place Recall Orthogonality`**: `Recall` mode operates strictly within the Composer buffer boundaries, walking the current session's chronological prompt stack. It renders no floating drawers and requires no query needle. Any character input or cursor movement automatically crystallizes the entry and transitions back to `Edit`.
- **`[INV-COMP-03] Atomic Draft Capsule Guarantee`**: Entering `Recall` or `Search` captures an immutable `DraftCapsule` snapshot containing text, cursor offset, and all staged attachment Part vectors. `Esc` guarantees atomic rollback.
- **`[INV-COMP-04] History Search Cyclic Stepping`**: When in `Search` mode, pressing `Ctrl-R` resolves to cycling to the next ranked candidate (`InputAction::ModalDown`), providing parity with modern shell history search.

### Positive Consequences

- Eradicates confusion between in-place prompt recall (`↑`/`↓`) and overlay fuzzy search (`Ctrl-R`).
- Solves the user frustration where `Ctrl-R` could not cycle or navigate candidates.
- Replaces ad-hoc struct sprawl with clean, testable state capsule mechanics.
- Prepares the Composer for clean extensibility without regressions.

### Negative Consequences & Trade-offs

- Requires refactoring existing `ComposeTarget` and hint derivation to cleanly map to `ComposerMode`.
- *Mitigation*: Provide clear conversion traits and preserve legacy integration tests while migrating test assertions to verify canonical modes.

---

## Rejected Alternatives & Negative Knowledge

### 1. Ad-hoc Flag Patching
- *Why considered*: Minimal immediate code edits.
- *Why rejected*: Causes combinatorial state explosion in `router.rs` and fails to fix root-cause keymap collisions.

### 2. Vim Mode Overlay
- *Why considered*: Common mental model for modal editors.
- *Why rejected*: Unnecessary complexity; Nuo is an agile AI conversation orchestrator built on Readline idioms, not a code editor emulator.

### 3. Merging `Recall` and `Search` into Single History Mode
- *Why considered*: Both deal with historical prompts.
- *Why rejected*: Fundamentally distinct interaction physics. `Recall` is single-turn, zero-chrome, in-place, and linear-temporal. `Search` is cross-session, multi-candidate, needle-filtered, and spatial. Merging them reintroduces cognitive dissonance.

---

## Links

- Amends: [ADR-0044](0044-composer-native-history-search-ghost-preview-and-draft-stash-topology.md)
- Complements: [ADR-0035](0035-domain-scoped-surface-architecture-and-encapsulated-dialog-lifecycle.md), [ADR-0040](0040-cross-domain-action-matrix-and-polymorphic-tui-spatial-topology.md)
