---
id: ADR-0044
title: "Composer-Native History Search, Ghost Preview, and Draft Stash Topology"
status: accepted
date: 2026-10-21
scope: tui/nuo-tui, architecture/surfaces, interaction/composer, presentation/overlays, UX/history
superseded_by: null
negative_knowledge: true
---

# 0044. Composer-Native History Search, Ghost Preview, and Draft Stash Topology

- Status: Accepted
- Date: 2026-10-21
- Deciders: Nuo Architecture Working Group
- Consulted: Interface, Usability, Terminal, Runtime, and Core Infrastructure Teams
- Informed: System Architects, Release Engineering
- Complements: [ADR-0035](0035-domain-scoped-surface-architecture-and-encapsulated-dialog-lifecycle.md), [ADR-0038](0038-thread-entity-server-ssot-fanout-and-orthogonal-action-lifecycle.md), [ADR-0040](0040-cross-domain-action-matrix-and-polymorphic-tui-spatial-topology.md), [ADR-0043](0043-thread-domain-nomenclature-unification-compact-tabbar-affordance-and-c-x-namespace-reorganization.md)

---

## Context and Problem Statement

In [ADR-0035](0035-domain-scoped-surface-architecture-and-encapsulated-dialog-lifecycle.md), a global architectural rule was instituted: *"Every dialog requiring text entry must own an embedded TextInput component"*. While this rule successfully isolated independent application-level modals (such as `Models` and `Connections`) from polluting chat state, applying it unconditionally to prompt history search created an ergonomic fracture:

1. **Dual-Input Cognitive Schizophrenia**:
   When opening history search via `Ctrl-R`, a floating card opened above the Composer with an embedded text field rendered in its header: `History  › [query]`. The terminal's physical caret was seized and relocated from the Composer up into this floating header (`CaretOwner::Overlay`). Operators were confronted with two visually competing input fields stacked vertically on top of each other, creating immediate confusion regarding active typing context and destroying muscle memory.

2. **Blind Selection and Multi-Line Prompt Obfuscation**:
   Candidate prompts in the history panel were aggressively folded into single rows (`first line... ↵`). Real-world agent interactions frequently involve multi-line instructions, structured code patches, or prompt templates. Because the floating dialog lacked full inspection capability and the Composer below remained disconnected, operators were forced to blindly select entries without being able to review full prompt bodies before committing.

3. **Fragile Draft Safety and Mental Friction**:
   Operators often search history while in the middle of drafting a prompt. Without a formal, explicit draft stashing lifecycle, operators feared losing uncommitted input when opening history search, leading to hesitation and poor ergonomics.

We require an uncompromising, modernized TUI architecture where the Composer is the sole authoritative input surface, the floating history drawer functions purely as a candidate selector, candidate browsing projects live multi-line ghost previews directly into the Composer, and drafts are atomically stashed and restored with zero risk of loss.

---

## Decision Drivers

- **Single Input Source of Truth (`[INV-HIST-01]`)**: The Composer is the sole input and caret host in thread view. Floating history panels MUST NEVER render internal input fields or steal the physical caret.
- **Zero-Friction In-Place Live Preview (`[INV-HIST-02]`)**: Navigating candidate history rows must dynamically project the full, multi-line prompt body into the Composer viewport as a styled Ghost Preview before commitment.
- **Atomic Draft Stash & Guaranteed Rollback (`[INV-HIST-03]`)**: Initiating history recall or search must atomically park any active draft (text, cursor position, and staged attachments). Dismissing (`Esc`) guarantees 100% loss-free draft restoration.
- **Unified Extension Key Dispatch (`[INV-HIST-04]`)**: All typing, editing, and navigation during history search must route through the unified three-layer Composer Extension pipeline rather than an isolated dialog modal loop.

---

## Considered Options

### Option 1: Retain Dual Inputs with Bidirectional Text Mirroring
- Keep the `History  › ` header input inside the floating dialog, but synchronize its buffer bidirectionally with the Composer below.
- *Assessment*: Rejected. Violates Single Source of Truth (`[INV-HIST-01]`). Retains two competing input boxes on screen, introduces race conditions and caret flickering between two surfaces, and maintains confusing visual clutter.

### Option 2: Full-Screen Centered Modal with Embedded Preview Pane
- Open a large, centered modal dialog that dims the background and contains both search, list, and detail panes (similar to a heavy GUI dialog).
- *Assessment*: Rejected. Heavily breaks terminal spatial continuity. Dimming the conversation transcript obscures recent dialogue context needed to formulate the query, and contradicts the agile, glance-and-pick philosophy of modern CLI/TUI tools.

### Option 3: Composer-Native Unified Mode with Floating Candidate Drawer and Live Ghost Preview (Chosen)
- Eliminate the input field from the history overlay. The overlay renders strictly candidate rows, counts, and metadata.
- When `Ctrl-R` is activated, the Composer transitions into `[History 🔍]` search mode. The operator types directly in the Composer, which owns the physical cursor and drives live fuzzy filtering.
- As the operator navigates candidates (↑/↓), the Composer viewport renders the selected entry in full multi-line ghost preview. `Enter` commits the preview; `Esc` rolls back atomically to the stashed draft.

---

## Decision Outcome

Chosen: **Option 3: Composer-Native Unified Mode with Floating Candidate Drawer and Live Ghost Preview**.

### Invariants Ratified

- **`[INV-HIST-01] Composer-Unified Input SSOT`**: The Composer is the sole text entry surface for prompt formulation and history filtering. Floating history overlays MUST NOT render embedded `TextInput` fields or claim `CaretOwner::Overlay`.
- **`[INV-HIST-02] Live Ghost Multiline Preview`**: Candidate navigation in history overlays dynamically renders the selected entry's full multi-line content into the Composer viewport with distinct visual ghost styling (e.g., muted tint or preview indicator), eliminating blind selection.
- **`[INV-HIST-03] Atomic Draft Stash & Rollback Guarantee`**: Entering history search or in-place recall automatically stashes the current prompt buffer, cursor offset, and attachment vectors into a dedicated `DraftStash`. `Esc` guarantees atomic rollback. `Enter` / `Tab` commits the candidate.
- **`[INV-HIST-04] Zero Dual-Input Chrome`**: Floating history drawers are pure candidate selectors. Their headers display scope, candidate counts, and match statistics, without redundant prompt glyphs or carets.

---

## Rejected Alternatives

1. **Retaining `HistorySearch` as an Autonomous `DialogKind`**:
   - *Why rejected*: ADR-0035's dogma that every dialog is an encapsulated entity works for independent views (`/models`, `/connections`), but fundamentally clashes with Composer extensions. History is an intrinsic capability of the Composer, not a standalone workflow destination. Treating it as an autonomous dialog forced duplicate state management and unnatural caret routing.

2. **In-Place Buffer Overwriting Without Dedicated Stash**:
   - *Why rejected*: Modifying the primary editor buffer directly during candidate scrolling without an immutable stash model pollutes undo/redo stacks, risks unrecoverable draft loss on terminal disconnect or focus loss, and creates fragile state recovery logic.

3. **Single-Line Truncation in Both Drawer and Composer**:
   - *Why rejected*: Fails to solve the primary usability limitation of terminal prompt history: AI prompts are inherently multi-line, structured, and long. Constraining preview to single-line summaries forces operators into trial-and-error workflows.

---

## Prospective Architecture and Migration Path

1. **Overlay Modernization (`src/overlays/history.rs`)**:
   - Strip `query`, `cursor_position`, and `show_caret` from `HistoryPanelProps`.
   - Update `draw_header` to render `History [count/total]` and search scope pills (`[Current Thread]` vs `[All Workspaces]`), without fake prompt glyphs or physical caret positioning.

2. **Composer Extension Unification (`src/composer_extension.rs` & `src/app/composer.rs`)**:
   - History search becomes an integrated Composer state. The Composer maintains `CaretOwner::Composer` throughout history filtering.
   - The Composer text buffer acts as the live search query needle.

3. **Ghost Preview Rendering (`src/composer.rs` & `src/render/composer.rs`)**:
   - When a history candidate is highlighted, the Composer draws the candidate text with Ghost styling while preserving search query state in the background.

4. **Draft Stash Lifecycle (`src/app/history.rs`)**:
   - Implement structured `DraftStash` capture upon entering history search. Ensure clean cancellation rollback on `Esc` and promotion on `Enter`.
