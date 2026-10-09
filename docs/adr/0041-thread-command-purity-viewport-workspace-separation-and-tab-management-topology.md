---
id: ADR-0041
title: "Thread Command Purity, Viewport Workspace Separation, and Explicit Tab Management Topology"
status: accepted
date: 2026-10-18
scope: tui/nuo-tui, interface/cli, interaction/keyboard, architecture/surfaces
superseded_by: null
negative_knowledge: true
---

# 0041. Thread Command Purity, Viewport Workspace Separation, and Explicit Tab Management Topology

- Status: Accepted
- Date: 2026-10-18
- Deciders: Nuo Architecture Working Group
- Consulted: Interface, Usability, Terminal, Runtime, and Core Infrastructure Teams
- Informed: System Architects, Release Engineering
- Complements: [ADR-0035](0035-domain-scoped-surface-architecture-and-encapsulated-dialog-lifecycle.md), [ADR-0038](0038-thread-entity-server-ssot-fanout-and-orthogonal-action-lifecycle.md), [ADR-0039](0039-client-tab-workspace-scene-unification-and-reactive-surface-sync.md), [ADR-0040](0040-cross-domain-action-matrix-and-polymorphic-tui-spatial-topology.md)

---

## Context and Problem Statement

Following the establishment of the `ClientTab` workspace (ADR-0039) and the Three-Tier Header Topology (ADR-0040), the interaction architecture retained a critical category mistake inherited from early monolithic single-viewport terminal interfaces:

1. **Category Mistake in Composer Slash Commands**:
   The chat composer slash grammar (`/`) conflated **Thread Harness Commands** (which govern the scoped dialog entity, e.g. `/model`, `/compact`, `/fork`, `/clear`, `/export`) with **Client Workspace Actions** (which govern viewport arrangement and system orchestration, e.g. `/dashboard`, `/settings`).
2. **Breakage of Mental Model and Flow Continuity**:
   Executing `/dashboard` or `/settings` within an active thread text buffer violently evicted the operator from their dialogue context, replacing the thread surface with a global hub. Furthermore, exposing `/settings` alongside session-local tools misled operators into questioning whether mutations applied only to the active thread or to the global host. Autocompletion menus presented a jarring mixture of local prompt transformations and viewport evictions.
3. **Architectural Drift Between Router and Event Loop**:
   While ADR-0039 ratified `ClientTab::Dashboard` and `ClientTab::Settings` as sovereign peers to `ClientTab::Thread`, the runtime action router still routed `NavigateDashboard` and `NavigateSettings` through legacy `enter_scene(...)` paths, mutating the monolithic `SceneKind` directly and occluding the underlying session rather than dispatching `SurfaceRouter::open_tab(...)`.

To achieve an uncompromising, future-proof terminal interface free of legacy compromises, we must decouple the Thread Composer from Workspace Viewport orchestration and formalize explicit Tab Management topologies.

---

## Decision Drivers

- **Thread Command Purity (`[INV-CMD-01]`)**: The `/` slash command grammar within the Thread Composer MUST be hermetic to the active thread entity. Commands that mutate, create, or switch client-level tabs MUST NOT be registered as executable in-session harness commands.
- **Sovereign Workspace Peerage (`[INV-TAB-06]`)**: `Dashboard` and `Settings` MUST exist strictly as sovereign, top-level `ClientTab` peers. Accessing them MUST be driven exclusively through client-tier workspace mechanics (Dedicated Keychords, New Tab Menu, or Global Command Palette).
- **Educational Router Guardrails (`[INV-ROUTER-01]`)**: Legacy entry of `/dashboard` or `/settings` inside the composer buffer MUST NOT silently execute viewport eviction or return a raw parser failure. The router MUST intercept such inputs non-destructively, preserving the input buffer and emitting an educational guidance toast directing the operator to the canonical workspace chord.
- **Strict Tab-Native Action Dispatch (`[INV-TAB-07]`)**: Viewport navigation actions (`NavigateDashboard`, `NavigateSettings`) MUST exclusively dispatch through `SurfaceRouter::open_tab()`, ensuring singleton idempotency, non-destructive background retention, and clean `Alt+1..9` peer switching.

---

## Considered Options

### Option 1: Retain Hybrid Omnibar Grammar in Composer
- Continue registering `/dashboard` and `/settings` alongside session harness commands.
- *Assessment*: Rejected. Violates the principle of least astonishment. Entangles session-scoped autonomy with client-container orchestration, degrades autocomplete signal-to-noise ratio, and perpetuates the illusion that a global cluster dashboard is a child view of a single dialogue.

### Option 2: Split Modal Grammar with Prefixes (`:` vs `/`)
- Introduce Vim-style `:` for client workspace actions (e.g. `:dashboard`, `:settings`, `:tabnew`) while reserving `/` for thread harness commands.
- *Assessment*: Rejected per **[INV-TAB-05]** and **[INV-AGENT-01]**. Modal command prefixes introduce parsing ambiguities in Markdown/shell text blocks, increase keystroke friction, and conflict with standard slash-centric agent ergonomics.

### Option 3: Hermetic In-Session Harness with Sovereign Tab Orchestration (Chosen)
- Purge `/dashboard` and `/settings` from the Thread Composer's executable command registry and server `BuiltinCmd` harness vocabulary, retiring `AgentResponse::OpenHostPanel`.
- Establish explicit workspace entry topologies:
  1. **Direct Ergonomic Keychords**: `C-x d` (Dashboard Tab), `C-x ,` or `C-x c` (Settings Tab).
  2. **Dedicated Tab Management Menu**: `Ctrl+T` / `Alt+T` to invoke the `NewTabMenu`, offering explicit instantiation of New Thread, Dashboard, or Settings.
  3. **Global Command Palette**: `C-x p` (Action matrix discovery).
- Implement router guardrails to guide operators transitioning from older command habits.

---

## Rejected Alternatives

Per **[INV-AGENT-01]**, the following alternative architectures were evaluated and rejected:

1. **Rejected: Embedding Dashboard or Settings as Sub-Scenes in `ClientTab::history`**
   - *Reason*: Pushing a cluster dashboard onto a thread's drill-in stack (`Thread A -> Dashboard -> Thread B`) creates circular navigation graphs and breaks LIFO unwinding semantics. Global hubs must remain top-level peers on the Tab bar, never nested stack frames.
2. **Rejected: Raw Silent No-Op or Generic Syntax Error for Legacy Slash Commands**
   - *Reason*: Punishing operator muscle memory with an opaque "Unknown command: /dashboard" causes confusion. Providing actionable, non-destructive interception with guidance hints trains operator ergonomics without destroying in-flight drafts.
3. **Rejected: Multi-Step Wizard for Viewport Navigation**
   - *Reason*: Requiring operators to traverse multiple modal dialogs just to inspect cluster telemetry adds unnecessary friction. Direct ergonomic shortcuts (`C-x d`) must provide zero-latency single-stroke switching.

---

## Decision Outcome

Chosen option: **Option 3**.

### 1. Dual-Tier Command & Action Stratification

```text
┌────────────────────────────────────────────────────────────────────────┐
│                        Client Workspace Plane                          │
│                                                                        │
│   Scope: Viewports, Tabs, Cluster Hub, Configuration, Lifecycle       │
│   Triggers:                                                            │
│     - Global Keychords: C-x d (Dashboard), C-x , (Settings), C-t (Menu) │
│     - Global Command Palette: C-x p                                    │
│     - Workspace Tab Bar: Alt+1..9 (Switch), Alt+W (Close Tab)          │
├────────────────────────────────────────────────────────────────────────┤
│                                                                        │
│                         Thread Domain Plane                            │
│                                                                        │
│   Scope: Prompt Engineering, Context Pruning, Model Steer, Branching  │
│   Triggers:                                                            │
│     - Hermetic Slash Commands: /model, /compact, /fork, /clear, /btw   │
│     - Turn Execution: Enter (Dispatch), Ctrl+Enter (Steer)             │
│     - Interrupts: Esc Esc / Ctrl+C (Interrupt Active Round)            │
│                                                                        │
└────────────────────────────────────────────────────────────────────────┘
```

### 2. Tab Management Topologies

1. **Tab Bar Creation Menu (`Ctrl+T` / `Alt+T`)**:
   Spawns a lightweight, focused selection surface over the active viewport:
   - `[1] 💬 New Thread` — Spawns new session and mounts `TabKind::Thread`.
   - `[2] 📊 Cluster Dashboard` — Focuses existing `TabKind::Dashboard` or mounts a new instance.
   - `[3] ⚙️  Global Settings` — Focuses existing `TabKind::Settings` or mounts a new instance.
2. **Idempotent Tab Mount & Activation**:
   ```rust
   // event_loop/actions.rs
   InputAction::NavigateDashboard => {
       let tab_idx = app.surfaces.open_tab(ClientTab::dashboard());
       app.sync_scene_to_active_tab();
   }
   InputAction::NavigateSettings => {
       let tab_idx = app.surfaces.open_tab(ClientTab::settings());
       app.sync_scene_to_active_tab();
   }
   ```
3. **Educational Redirection Guardrail**:
   When `/dashboard` or `/settings` is detected in the chat composer upon submit:
   - The router consumes the submission without appending to thread history.
   - The composer line is cleared or preserved without triggering an LLM turn.
   - An informative toast or hint notification is published:
     `"💡 Notice: /dashboard is a workspace view. Press 'C-x d' or open via Tab Menu (Ctrl+T)."`

---

## Invariants & Behavioral Boundaries

- **[INV-CMD-01] Hermetic Thread Slash Commands**: The slash command registry exposed within the thread input composer and server harness command catalog (`BuiltinCmd`) MUST ONLY contain actions that operate directly on the current thread's state, context, model, or tool execution. Client workspace operations (`/dashboard`, `/settings`) are strictly excluded from the server harness vocabulary and wire commands.
- **[INV-TAB-06] Sovereign Workspace Peerage**: Full-screen workspaces `Dashboard` and `Settings` MUST be mounted as singleton peers in `SurfaceRouter::tabs`. They MUST NOT be pushed as sub-scenes into any thread's `history` navigation stack.
- **[INV-TAB-07] Viewport-Native Action Dispatch**: All actions requesting navigation to `Dashboard` or `Settings` MUST route through `open_tab(...)`, activating the tab if already open or mounting it at the end of the tab sequence.
- **[INV-ROUTER-01] Non-Destructive Workspace Interception**: Submissions of `/dashboard` or `/settings` within the thread composer MUST be intercepted at the router level, preventing accidental message transmission and guiding the operator via non-blocking feedback.

---

## Positive Consequences

- Eliminates cognitive category mistakes between thread prompt engineering and client workspace orchestration.
- Restores clean visual autocompletion focused 100% on dialogue and agent control.
- Eliminates context-eviction shock when typing in the composer.
- Completely harmonizes the codebase with ADR-0039 and ADR-0040's Three-Tier spatial architecture.
