---
id: ADR-0131
title: "Help Dialog Architecture, TabBar Status Cluster, and Model Bar Purification"
status: accepted
date: 2026-10-31
scope: presentation/nuo-view, presentation/nuo-ui-primitives, host/nuo, tui/chrome, tui/event-loop, tui/overlays
superseded_by: null
negative_knowledge: true
---

# 0131. Help Dialog Architecture, TabBar Status Cluster, and Model Bar Purification

- Status: Accepted
- Date: 2026-10-31
- Deciders: Core Engineering, Architecture Working Group
- Amends: [ADR-0043](0043-thread-domain-nomenclature-unification-compact-tabbar-affordance-and-c-x-namespace-reorganization.md) (Retires `C-x` scene namespace), [ADR-0121](0121-notice-log-residency-retained-disclosure-record-and-bubble-as-pointer.md) §5 (Relocates persistent notice affordance from model bar to TabBar status cluster)

---

## Context and Problem Statement

ADR-0121 established the `L2` surface-log tier for notices and recognized that a retained disclosure record requires a persistent, on-screen affordance. However, section 5 placed this affordance (`!N notices`) on the `model_bar`. 

This placement caused severe architectural, semantic, and human-factors defects:
1. **Domain Conflation**: The `model_bar` is solely responsible for LLM runtime route identity, instance provenance (`@provider`), reasoning effort, and context-window token telemetry. Injecting client-level diagnostic error notices into this row conflates inference telemetry with application system health, confusing users into assuming model failure.
2. **Visual & Layout Distortion**: In `model_bar.rs`, accommodating the notice badge distorted the shrink ladder; under narrow terminal widths, model identity itself was sacrificed while the notice badge remained, defeating the very purpose of a `model_bar`. Furthermore, hit-testing calculations for context telemetry and notices overlapped at `rect.x + inner`.
3. **Shortcut Fragmentation & The `C-x` Legacy Burden**: Keycaps were scattered across disconnected UI rows (`C-x menu` on TabBar, `Alt-T`/`Alt-C` on ModelBar, `Alt-[`/`Alt-]` on SceneBar). The `C-x` namespace was a vestigial Emacs-style leader chord that only aggregated client actions while remaining blind to active scene context, lacking discovery for ordinary terminal users.

## Decision Drivers

- **Domain Demarcation**: `model_bar` MUST strictly express model route and context telemetry.
- **Client-Global Consistency**: Diagnostic notices are client-global records (ADR-0121) and belong to the Client Tier (Tier 1 TabBar), never conversation or model chrome.
- **Zero Layout Jitter**: Notice indications in the TabBar must be ultra-compact (`[!N]`) and bounded to prevent reflowing tab labels.
- **Unified Contextual Help**: Keybindings must be accessible from a sovereign, categorized `HelpDialog` containing both active scene context and client functional sub-blocks.
- **Non-Printable Universal Activation**: Shortcuts to open help must never collide with text entry in the active composer (`F1` and `Alt-h`).
- **Zero Legacy Burden** ([ADR-0059](0059-canonical-thread-persistence-schema-and-zero-legacy-clean-break.md)): Abolish the `C-x` leader chord and `scene_namespace_armed` state completely.

## Architectural Decisions

### 1. Model Bar Purification
The `model_bar` is cleansed of all notice-related properties (`unread_faults`, `notices_badge_w`, `notices_label`, and hit rects). Its width calculation and shrink ladder focus purely on:
- Context token gauge (left)
- Model identity, reasoning effort, and provider instance (right)

### 2. TabBar Status Cluster
The right edge of the Client Global TabBar (`Tier 1`) is standardized into a unified status cluster:
- **Fault Notice Badge (`[!N]`)**: Rendered only when unread faults exist (`unread_faults > 0`), styled with error emphasis, occupying a fixed ~4 columns. Clicking it or pressing `Ctrl-O` opens `NoticeDock`.
- **Help Affordance (`F1 help`)**: Constant rightmost affordance providing immediate visual discoverability without layout jitter.

### 3. Sovereign Help Dialog (`DialogKind::Help`)
A first-class modal dialog entity is introduced (`DialogKind::Help` with `DialogScope::Client`):
- **Current Scene Block**: Dynamically reflects available actions in the active scene (e.g., Thread: notices, history recall, context telemetry, connection details, interrupt).
- **Scene Navigation Block**: The intra-tab scene history (step back / forward), transcript step focus, and the clear-focus chord. This is the block that relieves the SceneBar of carrying keycaps: it documents the chords *resolved from the live keymap*, including any user remap.
- **Client Domain Sub-blocks**:
  - `Tabs & Workspaces`: new thread tab, close active tab, and `Alt-1..9` direct tab jump.
  - `Views & Navigation`: dashboard, saved threads, settings.
  - `Composer & System`: send, steer/follow-up mode toggle, command palette, slash commands.
- **Direct Action Dispatch**: Pressing any advertised accelerator while the dialog is active dispatches the command and immediately dismisses the dialog.

### 4. Non-Printable Activation Chords (`F1` and `Alt-h`)
- `F1`: Universal discoverability standard.
- `Alt-h`: Home-row ergonomic shortcut immune to text composer character capture, avoiding conflict on macOS/Linux terminal environments.
- The `C-x` namespace prefix and its arming state machine are retired.

## Normative Invariants Established

- `[INV-HELP-01]` **Sovereign Contextual Help**: Help is modeled as `DialogKind::Help`, presenting both active scene operations and partitioned client domain blocks.
- `[INV-HELP-02]` **Direct Action Dispatch**: Single-key accelerators inside the Help dialog execute immediately and dismiss the modal.
- `[INV-HELP-03]` **Non-Printable Dual Activation**: Help dialog invocation is bound to `F1` and `Alt-h`.
- `[INV-HELP-04]` **Sole Chord Discovery Surface**: The Help dialog is the *only* surface that prints a chord for a remappable binding. Chrome renders navigation *affordances* (a `‹` / `›` chevron, a `F1 help` entrance) but never a chord literal, and every chord the dialog prints is resolved from the live keymap (`GlobalOverrides::effective_binding` / `SurfaceOverrides::effective_binding`) rather than written as a string — so a user remap changes the display, and a command with no chord renders `unbound` instead of a plausible keycap. This is enforced by `overlays/help.rs` tests (`chords_resolve_from_the_canonical_keymap`, `chords_follow_user_remaps`) plus the SceneBar assertions in `view_header.rs` that reject any `Alt+[` / `Alt+]` text.
- `[INV-AFFORDANCE-01]` **TabBar Status Cluster & Model Bar Purity**: Notice badges MUST NOT reside on `model_bar`. Persistent notice affordances reside exclusively in the TabBar right cluster alongside the help affordance.
- `[INV-AFFORDANCE-02]` **Chrome Renders Only Resolved Chords**: every keycap painted by chrome (activity strip, queue bar, headers) MUST be a chord the caller resolved from the live keymap — never a literal. A command with no binding renders **no keycap** rather than a plausible one, and a retired chord (`C-x …`) must never reappear. Enforced by `chrome::tests::activity_strip_keycaps_are_the_resolved_bindings` and the `C-x` rejection in `chrome::tests`, plus the resolved-chord assertions in `overlays/help.rs`.

## Consequences

- **Positive**:
  - `model_bar` is 100% pure telemetry with zero hitbox conflicts.
  - Zero layout jitter: Tab labels are protected from wide dynamic notice strings.
  - Centralized, context-aware shortcut discovery relieves other chrome rows from carrying explanatory keycaps.
  - Eliminates obscure Emacs leader chords for modern users while preserving high-throughput keyboard ergonomics.
- **Negative & Trade-offs**:
  - Users accustomed to typing `C-x` must use `Alt-h` or `F1` to open the help dialog, or use direct global shortcuts (`Alt-T`, `Alt-W`, etc.).

## Rejected Alternatives & Negative Knowledge

- **Bare `?` Activation**: Rejected because in chat-first AI agent TUI environments, the composer text box is focused by default; bare `?` would either be captured as text or require disruptive modal unfocusing.
- **Notice Badge inside Composer**: Rejected because composer is ephemeral input scratchpad, not client system chrome.
