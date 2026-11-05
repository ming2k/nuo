---
id: ADR-0151
title: "Command Domain Orthogonality, Sovereign Tab Navigation, and Help Modal Purification"
status: accepted
date: 2026-12-23
scope: presentation/nuo-view, presentation/nuo-ui-primitives, tui/chrome, tui/event-loop, tui/overlays, architecture/taxonomy
superseded_by: null
negative_knowledge: true
---

# 0151. Command Domain Orthogonality, Sovereign Tab Navigation, and Help Modal Purification

- Status: Accepted
- Date: 2026-12-23
- Deciders: Nuo Architecture Working Group
- Consulted: Presentation Substrate, TUI Subsystem, Core Protocol
- Amends:
  - [ADR-0041](0041-thread-command-purity-viewport-workspace-separation-and-tab-management-topology.md) (Thread Command Purity & Workspace Tab Management)
  - [ADR-0131](0131-help-dialog-architecture-tabbar-status-cluster-and-model-bar-purification.md) (Retires in-modal single-key accelerator dispatch `[INV-HELP-02]` in favor of pure reference modal semantics)
- Complements: [ADR-0039](0039-client-tab-workspace-scene-unification-and-reactive-surface-sync.md), [ADR-0092](0092-uncompromising-scenebar-breadcrumb-topology-bidirectional-navigation-and-symmetric-keybindings.md)

---

## Context and Problem Statement

Following the retirement of the legacy `C-x` leader namespace in ADR-0131, the client interface accumulated several critical architectural compromises, domain conflations, and interaction anomalies:

1. **The In-Modal Accelerator Anomaly and the `d` Key Island**:
   ADR-0131 introduced `[INV-HELP-02]` ("Direct Action Dispatch"), which bound single unadorned printable keys (`t`, `w`, `d`, `,`, `s`, `p`, `o`) inside `DialogKind::Help`. Outside the Help dialog, pressing bare `d` merely inserted the letter `d` into the prompt composer. Consequently, the session dashboard appeared to have a shortcut of bare `d` that mysteriously *only* functioned while viewing the F1 Help modal.
2. **Help Dialog Visual Confusion and Redundant Dual Chords**:
   The Help modal displayed dual-chord notations such as `t, Alt-t`, `w, Alt-w`, `,, Alt-,`, and `o, Alt-o`, juxtaposed against bare `d` for Dashboard and `p` for Command Palette. This breached the core expectation of a help modal as an authoritative keybinding reference cheat sheet, transforming it into an unpredictable modal launcher.
3. **Domain Conflation: Client Workspace Actions Masquerading as Thread Slash Commands**:
   ADR-0041 established `[INV-CMD-01]`, strictly barring client workspace actions (`Dashboard`, `Settings`, `CloseTab`, `CommandPalette`) from the thread slash command catalog (`BuiltinCmd`). However, the client keymap registry still assigned pseudo-slash commands (`/commands`, `/dashboard`, `/settings`, `/close`) to these workspace actions, and the Help modal explicitly advertised `/commands` and `/dashboard` in prompt-style notations. Thread Composer input is reserved for agent cognition and conversational turns; routing workspace viewports through composer slash strings violates the strict dual-tier command architecture.
4. **Collision with Terminal and POSIX Readline Standards**:
   Blindly assigning `Alt-d` to the Dashboard tab usurps the standard GNU Readline forward word-kill (`kill-word`) command, codified in `docs/explanation/terminal-shortcuts-and-conflicts.md` Layer 1 ("In-Buffer Readline Integrity: Application actions NEVER steal these chords"). Furthermore, on unconfigured macOS terminals, `Option-d` emits `∂` (partial differential).

---

## Decision Drivers

- **Zero Compromise on Domain Purity (`[INV-CMD-01]`)**: The Thread Composer belongs strictly to the conversation and thread-scoped built-ins. Client workspace navigation belongs to the client window and tab plane.
- **Strict Avoidance of Readline and Terminal Emulator Collisions**: Terminal line editing (`Alt-d` word-kill, `Ctrl-w` backward word-kill, `Ctrl-a/e` home/end) must remain inviolable in text fields.
- **Help Modal as Pure Reference Cheat Sheet**: A help modal must document real, global keybindings. It must not act as a modal launcher intercepting bare printable characters.
- **Single Source of Truth Discovery (`[INV-AFFORDANCE-02]`)**: Every action displays exactly ONE canonical resolved chord. No dual-keycap confusion (`t, Alt-t`) and no fake modal accelerators (`d`).

---

## Architectural Decisions & Normative Invariants

### 1. `[INV-PLANE-01]` Command Domain Orthogonality

Client Workspace actions (`CommandId::NavigateDashboard`, `CommandId::NavigateSettings`, `CommandId::CommandPalette`, `CommandId::CloseTab`, `CommandId::NewTab`) belong strictly to the Client Workspace Plane.
- They MUST NOT declare `slash: Some(...)` triggers in `COMMAND_REGISTRY`.
- They MUST NOT be advertised as slash commands in Help, chrome, or documentation.
- The thread composer's `/` prompt is reserved exclusively for Thread Domain operations (`BuiltinCmd`).
- Submissions of legacy workspace tokens in the composer remain non-destructive router guardrails (`[INV-ROUTER-01]`), never legitimate registered slash commands.

### 2. `[INV-PLANE-02]` Purified Help Dialog Reference Semantics

`DialogKind::Help` is exclusively a read-only reference cheat sheet for discovering live keybindings.
- ADR-0131 `[INV-HELP-02]` ("Direct Action Dispatch" via single bare characters `t`, `w`, `d`, `,`, `s`, `p`, `o`) is **formally revoked and abolished**.
- `modal_keys.rs` for `DialogKind::Help` MUST ONLY process modal navigation (`Up`/`Down`/`j`/`k`/`PageUp`/`PageDown`/`Home`/`End`) and modal dismissal (`Esc`/`F1`/`Alt-h`).
- Bare printable keys are inert inside the Help dialog.

### 3. `[INV-PLANE-03]` Single Canonical Keycap Discovery

In the Help dialog and all UI hint rows:
- Every action MUST display exactly **one canonical resolved chord** (derived from the live keymap via `GlobalOverrides::effective_binding`).
- Dual-chord notations (`t, Alt-t`, `w, Alt-w`, `,, Alt-,`, `o, Alt-o`) and lone in-modal letters (`d`, `s`, `p`) are completely removed.
- Actions render clean, authoritative keycaps: `Alt-t`, `Alt-w`, `Alt-0`, `Alt-,`, `Alt-1..9`, `Ctrl-Shift-P`.

### 4. `[INV-PLANE-04]` Zero-Conflict Workspace Tab Chords

Client-level sovereign workspace tabs are navigated via zero-conflict, ergonomic chords:
- **`Alt-0`**: Canonical direct chord for `CommandId::NavigateDashboard` (opens or focuses the Dashboard singleton tab `ClientTab::dashboard()`).
- **`Alt-1..9`**: Direct ordinal jump to tabs 1 through 9.
- **`Alt-[` / `Alt-]`**: Symmetric tab history back / forward (`[INV-NAV-03]`).
- **`Alt-,`**: Canonical direct chord for `CommandId::NavigateSettings` (opens or focuses Settings tab `ClientTab::settings()`).
- **`Alt-t`**: New thread in a new tab (`CommandId::NewTab`).
- **`Alt-w`**: Close active tab (`CommandId::CloseTab`).
- **Readline Invariance**: `Alt-d` in the composer remains 100% dedicated to Readline forward word-kill (`kill-word`), preserving Layer 1 text-editing integrity.

### 5. `[INV-PLANE-05]` Natural-Language Command Palette

The Command Palette (`DialogKind::Switcher`, canonically bound to `Ctrl-Shift-P` / `Key::CTRL_SHIFT_P`):
- Operates via natural-language fuzzy matching over human-readable labels (`"Dashboard"`, `"Settings"`, `"New Tab"`, `"Close Tab"`).
- The `/commands` and `/palette` pseudo-slash commands are purged from discovery and documentation.

---

## Consequences

### Positive
- **Complete Elimination of Cognitive Dissonance**: Users no longer wonder why `d` only works in F1; Dashboard is an equal peer tab with global chord `Alt-0` and ordinal index access `Alt-1..9`.
- **Clean Reference Help Modal**: F1 displays a pristine reference table with zero misleading single-letter accelerators or redundant dual chords.
- **Readline Protection**: Operators retain standard `Alt-d` forward word deletion without accidental viewport displacement.
- **Domain Hygiene**: Clean architectural boundary between client workspace navigation and thread agent execution.

### Negative & Trade-offs
- Users who memorized pressing `F1` followed by bare `d` or `t` must now use the direct global shortcuts (`Alt-0`, `Alt-t`) or close the modal and navigate via standard tab controls.

---

## Rejected Alternatives

1. **Stealing `Alt-d` for Dashboard**: Rejected because `Alt-d` is standard GNU Readline forward word-kill (`kill-word`). Hijacking it violates `docs/explanation/terminal-shortcuts-and-conflicts.md` Layer 1 integrity.
2. **Promoting `/dashboard` as a Primary Slash Command**: Rejected because it violates ADR-0041 `[INV-CMD-01]` by polluting the thread dialogue composer with client workspace viewport routing.
3. **Retaining Single-Letter Accelerators in Help**: Rejected because displaying `t, Alt-t` and `d` creates irreconcilable confusion regarding whether single letters work outside the modal.
