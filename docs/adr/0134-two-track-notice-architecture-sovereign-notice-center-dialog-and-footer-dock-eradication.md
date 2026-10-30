---
id: ADR-0134
title: "Two-Track Notice Architecture, Sovereign Notice Center Dialog, and Footer Dock Eradication"
status: accepted
date: 2026-12-12
scope: presentation/nuo-view, presentation/nuo-ui-primitives, host/nuo, tui/chrome, tui/event-loop, tui/surfaces, tui/overlays
superseded_by: null
negative_knowledge: true
---

# 0134. Two-Track Notice Architecture, Sovereign Notice Center Dialog, and Footer Dock Eradication

- Status: Accepted
- Date: 2026-12-12
- Deciders: Nuo Architecture Working Group
- Amends: [ADR-0116](0116-content-residency-and-surface-layer-topology.md) (Content residency layer routing), [ADR-0121](0121-notice-log-residency-retained-disclosure-record-and-bubble-as-pointer.md) §4 (Abolishes the `NoticeDock` footer row), [ADR-0131](0131-help-dialog-architecture-tabbar-status-cluster-and-model-bar-purification.md) §2 (Relinks the TabBar `[!N]` badge to the sovereign Notice Center dialog)

---

## Context and Problem Statement

ADR-0121 established the `L2` receipt-log tier for system notices and eliminated the defect where ephemeral toast bubbles erased past diagnostic faults. However, section 4 made the architectural choice to place the retained notice viewer as a **footer dock** (`FooterRowId::NoticeDock`), docked directly above the `Composer` (input prompt area) and sized dynamically between 25% and 50% of the viewport.

In practice, this footer dock created significant structural and human-factors defects:

1. **Spatial Inversion & Input Territory Usurpation**: The composer is the primary focus of human operator interaction. Docking a multi-row log pane directly above the active prompt box severely compresses conversational context and makes notices feel as if they are "erupting from the bottom" over the input tool.
2. **Layout Arithmetic Overhang**: Sizing the dock independently from its content required complex layout formulas (`notice_dock_rows`, `notice_dock_ceiling`, `expanded_detail_rows`) in `nuo-ui-primitives::design`. Whenever an expanded detail block was toggled, the footer stack resized and pushed the conversation upward.
3. **Mismatched Mental Model**: Users expect operational notices (warnings, system errors, catalog refresh failures) to function like a canonical **Notification Center**—an independent, card-based, scrollable/pageable review sheet that is summoned when needed and dismissed without disturbing the ongoing workspace view.
4. **Channel Conflation**: Transient signals (two-step armed gates, clipboard results) were conflated with durable operational records in the top-right bubble slot, leading to competition for a single presentation slot.

## Decision Drivers

- **Footer Purity & Zero Input Jitter**: The composer and activity bar must have rock-solid spatial stability. Operational notifications must never compress or displace the input prompt area.
- **Two-Track Demarcation**: Ephemeral interaction feedback (Track 1) and retained operational records (Track 2) must be strictly decoupled in lifecycle, storage, and presentation.
- **Sovereign Non-Intrusive Notice Review**: The notice repository must be accessible via a dedicated, global-scoped dialog (`DialogKind::Notices`) rendering a clear card-stack layout with keyboard navigation, expansion, and copying.
- **Non-Intrusive Notice Delivery**: Incoming notices must never pop up an intrusive modal unprompted. The client remains quiet, updating the persistent status cluster badge (`[!N]`) and surfacing a subtle pointer signal.
- **Zero Legacy Burden ([ADR-0059](0059-canonical-thread-persistence-schema-and-zero-legacy-clean-break.md))**: Completely eradicate `FooterRowId::NoticeDock`, the footer dock layout code, and monolithic ad-hoc viewer state cells.

## Architectural Decisions

### 1. Two-Track Notice Architecture

All disclosures are classified into two disjoint operational tracks with distinct presentation contracts:

* **Track 1: Ephemeral Transient Signals (Toast Pill)**:
  - Scope: Immediate user action acknowledgments (copy success/failure, `Esc` interrupt armed gate, `Ctrl-C` detach armed gate).
  - Lifecycle: Millisecond-scale dwell (1.5s–2.6s), dismissed on interaction or deadline.
  - Surface: Top-right `ToastBubble`. Never retained in the persistent log; zero clutter in notification history.
* **Track 2: Retained Operational Notices (Notice Center)**:
  - Scope: Provider retries, catalog refresh warnings, runtime link/clipboard errors, tool streaming failures.
  - Lifecycle: Appended unconditionally to the client-global `SurfaceLog`. Retained indefinitely during the process session.
  - Presentation:
    - **Arrival**: Emits a brief pointer toast with shortcut affordance (`! <topic>: <message> · Alt-o`) and increments the TabBar unread counter.
    - **Review**: Inspected inside the sovereign `DialogKind::Notices` dialog.

### 2. Complete Eradication of the Footer Dock

The `FooterRowId::NoticeDock` enum variant and all associated layout calculations are permanently excised:
- Removed `FooterRowId::NoticeDock` from `crates/nuo-view/src/footer_stack.rs`.
- Removed `notice_dock_rows`, `notice_dock_ceiling`, and `notice_dock_rows_expanded` from `crates/nuo-ui-primitives/src/design.rs`.
- Removed `notice_dock` from `TranscriptProps` in `crates/nuo/src/shell/render/mod.rs`.
- The footer stack now strictly hosts `TopGap`, `ActivityStrip`, `Composer`, and `ModelBar`. The conversation and composer layout are 100% immutable upon notice arrival.

### 3. Sovereign Notice Center Dialog (`DialogKind::Notices`)

Notice review is elevated to a first-class, client-global dialog:
- **Dialog Registration**: `DialogKind::Notices` declaring `DialogScope::Global`. Survives all tab switches and scene transitions without losing selection or scroll position.
- **Presentation**: Standard centered/card-stack modal view (`FixedModalSpec::PROVIDER` / `ContentModalSpec::TOOLS`), rendering notices in reverse chronological order (newest first).
- **Card-Stack Item Layout**:
  - Severity indicator icon (`ℹ`, `▲`, `✕`) in theme semantic color.
  - Topic pill (`trust`, `backend`, `copy`, `catalog`).
  - Formatted ISO / wall-clock timestamp.
  - Prominent headline title + optional expandable structured detail block.
- **Keyboard Ergonomics**:
  - `↑` / `k` and `↓` / `j`: Move selection across notice cards.
  - `Enter` / `Space`: Toggle expanded detail view for the focused notice.
  - `c`: Copy the focused notice text (or full diagnostic entry) to system clipboard with immediate confirmation.
  - `d` / `x`: Clear notice history.
  - `Esc`: Close dialog and return focus immediately to Composer.

### 4. Status Cluster & Universal Hotkey Integration

- **TabBar Status Cluster (`[!N]`)**: Maintained from ADR-0131. When `unread_faults > 0`, the `[!N]` badge renders in error posture. Clicking it or pressing `Alt-o` / `Ctrl-o` directly opens `DialogKind::Notices`.
- **Opening Marks Read**: Opening the Notice Center automatically acknowledges all unread faults, clearing the badge while preserving the history for continued inspection.

## Normative Invariants Established

- `[INV-NOTICE-01]` **Two-Track Notice Decoupling**: Transient keyboard action gates (`Esc`, `Ctrl-C`, copy receipts) MUST NOT enter the persistent notice log. Operational disclosures MUST enter `SurfaceLog` and be viewed through `DialogKind::Notices`.
- `[INV-NOTICE-02]` **Footer Stack Purity**: `footer_stack` MUST NOT contain notice presentation rows. Operational notice arrival MUST NOT alter footer geometry or reflow transcript lines.
- `[INV-NOTICE-03]` **Sovereign Global Dialog Scope**: The Notice Center dialog MUST have `DialogScope::Global` so that notice navigation state survives tab switching and scene routing.
- `[INV-NOTICE-04]` **Non-Intrusive Arrival**: Operational notice arrivals MUST NOT trigger unprompted modal overlays. Modals may only appear on explicit operator request (`Alt-o`, TabBar badge click, or command palette).

## Consequences

- **Positive**:
  - Eliminates the confusing "eruption from bottom" presentation; notifications now reside in a clean, predictable card-based dialog.
  - Composer input area achieves total layout immobility; zero jitter when network or background errors occur.
  - Drastically simplifies `footer_stack.rs` and `render/mod.rs` by dropping all dock sizing logic.
  - Standardizes notice navigation under the universal modal dialog system, reusing familiar `Esc`, `j`/`k`, and `c` keybindings.
- **Negative & Trade-offs**:
  - Reading a full diagnostic notice now requires opening a modal overlay rather than reading an in-situ footer pane. This is mitigated by the instant non-printable hotkey (`Alt-o`) and pointer hints.

## Rejected Alternatives & Negative Knowledge

- **Alternative A (Automatic Modal Popup on Fault Arrival)**: Rejected. Popping up a centered modal dialog when a background retry or network error occurs steals keyboard focus from the active composer, disrupting operator typing.
- **Alternative B (Preserving Footer Dock as an Opt-In Config Key)**: Rejected. Per ADR-0121 Alternative F and repository standards, maintaining two divergent rendering paths doubles the layout test matrix and perpetuates legacy technical debt.
- **Alternative C (Inline Dialogue Receipts)**: Rejected. Violates [ADR-0117](0117-transcript-admission-purity-dialogue-only-thread-documents.md) (Transcript Admission Purity); client operational noise must never pollute dialogue history.
