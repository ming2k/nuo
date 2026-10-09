---
id: ADR-0039
title: "Client-Centric Tab Workspace, Unified Scene-Session Architecture, and Reactive Surface Synchronization"
status: accepted
date: 2026-10-18
scope: tui/nuo-tui, wire/nuo-wire, interface/cli, presentation/tabs
superseded_by: null
negative_knowledge: true
---

# 0039. Client-Centric Tab Workspace, Unified Scene-Session Architecture, and Reactive Surface Synchronization

- Status: Accepted
- Date: 2026-10-18
- Deciders: Nuo Architecture Working Group
- Consulted: Interface, Design, Runtime, Protocol, and Terminal Teams
- Informed: System Architects, Release Engineering
- Complements: [ADR-0035](0035-domain-scoped-surface-architecture-and-encapsulated-dialog-lifecycle.md), [ADR-0038](0038-thread-entity-server-ssot-fanout-and-orthogonal-action-lifecycle.md)

---

## Context and Problem Statement

Following the establishment of the persistent `Conversation` entity and the eradication of client-driven auto-kill in ADR-0038, the terminal user interface (`nuo-tui`) exhibited structural dissonance in viewport management:

1. **Scene-Session Conceptual Cleavage**:
   Under ADR-0035, the interface stratified full-screen workspaces into five `SceneKind` variants (`Conversation`, `Dashboard`, `Settings`, `TaskInspection`, `Aside`), while simultaneously maintaining a separate session switching mechanic (`/switch`, `C-x s`, `AttachAction`). This dual-routing architecture required the frontend to arbitrate both a scene history stack and ambient session pointers, making lateral multi-tasking cumbersome.
2. **Monolithic Single-Viewport Constraint**:
   Developers conducting multi-agent workflows (e.g. running an autonomous long-horizon refactor in Conversation A while conducting a quick codebase query in Conversation B) were forced to either multiplex external terminal windows or toggle repeatedly through full-screen modal drill-ins.
3. **Ambiguity Around Viewport Teardown**:
   Because the interface lacked an explicit Tab container abstraction, the boundary between "closing a view" and "terminating a task" was blurred. Operators expected familiar tab ergonomics (closing a tab detaches the view; terminating an agent requires an explicit kill action).

We require a modernized, uncompromising surface architecture that establishes the Client as a pure Interactive Viewport container, unifies disparate scenes into a demand-driven Tab workspace, and harmonizes multi-thread navigation with server-backed reactive state synchronization.

---

## Decision Drivers

- **Client Container Purity (`[INV-TAB-01]`)**: The TUI process is strictly an interactive presentation viewport. The lifecycle of the client and its individual tabs MUST be decoupled from backend thread execution.
- **Unified Workspace Abstraction (`[INV-TAB-02]`)**: Full-screen thread views, the system dashboard, and settings must unify under a single, coherent `ClientTab` container model.
- **Demand-Driven Tab Mounting (`[INV-TAB-03]`)**: The client MUST NOT indiscriminately mount all server-hosted threads as tabs. Tabs are mounted strictly on demand when explicitly opened or created by the operator.
- **Non-Destructive Tab Closure (`[INV-TAB-04]`)**: Closing a tab in the client MUST execute a clean `Detach` operation. It MUST NEVER cancel in-flight thread turns or trigger server teardown.
- **Strict Slash Command Grammar (`[INV-TAB-05]`)**: Viewport and tab navigation commands must strictly conform to canonical `/` slash commands (e.g. `/tab`, `/close`, `/new`), rejecting foreign modal syntaxes (such as Vim `:q`).

---

## Considered Options

### Option 1: External Terminal Multiplexer Reliance
- Delegate all multi-session tab management entirely to external multiplexers (tmux, Zellij, WezTerm, iTerm2). Keep the Nuo TUI strictly single-session.
- *Assessment*: Rejected. Forces developers into external tool configuration, breaks cross-platform feature parity (Windows Terminal vs Linux vs macOS), and prevents native client-side cross-tab notifications, progress badges, and unified session orchestration.

### Option 2: Server-Pushed Universal Tab Bar
- Have the server push its entire thread registry to the client, creating a tab for every stored session.
- *Assessment*: Rejected. In realistic development setups with dozens of archived sessions, the tab bar would immediately overflow and become unnavigable.

### Option 3: Client-Centric Demand-Driven Tab Workspace (Chosen)
- Formulate the client workspace around a lightweight, client-owned `ClientTab` collection:
  - `Tab::Thread(ThreadId)`: Multi-instance dialogue viewport.
  - `Tab::Dashboard`: Singleton cluster and thread monitoring viewport.
  - `Tab::Settings`: Singleton global configuration center.
- Tabs mount dynamically upon explicit user action (e.g. `/new`, selecting a session from Dashboard, `/settings`).
- Closing a tab performs an isolated `Detach`. Closing the last tab exits the client process cleanly.

---

## Rejected Alternatives

Per **[INV-AGENT-01]**, the following alternative architectures were evaluated and rejected:

1. **Rejected: Adoption of Vim `:q` / `:w` Command Grammar**
   - *Reason*: Nuo's command system is strictly unified around canonical `/` slash commands. Introducing modal colon commands creates command parsing collisions, cognitive dissonance, and violates the zero-legacy grammar contract.
2. **Rejected: Automatic Background Thread Termination on Tab Close**
   - *Reason*: Equating tab closure with business workload destruction violates autonomous agent safety. If a long-running build or tool pipeline is executing, closing the tab simply closes the viewport while execution proceeds in Autonomous Mode.
3. **Rejected: Multi-Process Tabs (Spawning a new client process per tab)**
   - *Reason*: Multi-process tabs waste memory and operating system handles. A single client process managing an in-memory tab collection over shared IPC multiplexing provides instantaneous tab switching and minimal resource overhead.

---

## Decision Outcome

Chosen option: **Option 3**.

### 1. Unified `ClientTab` Workspace Architecture

```text
┌────────────────────────────────────────────────────────────────────────┐
│                        Client Viewport Container                       │
│                                                                        │
│  Tabs: [1* Refactor DB]  [2 Quick Query]  [3 Dashboard]  [4 Settings]  │
│        (Conversation)    (Conversation)   (System Hub)   (Config Hub)  │
├────────────────────────────────────────────────────────────────────────┤
│                                                                        │
│                       Active Tab Content Area                          │
│                                                                        │
│   - Transcript Viewport (for Conversation Tabs)                        │
│   - Cluster Overview & Active Task Ledger (for Dashboard Tab)          │
│   - Model Providers, MCP, and Theme Configuration (for Settings Tab)   │
│                                                                        │
├────────────────────────────────────────────────────────────────────────┤
│  Composer / Input Zone (Contextual to active tab)                      │
└────────────────────────────────────────────────────────────────────────┘
```

1. **Tab Representation**:
   ```rust
   pub enum ClientTab {
       Conversation {
           id: String,
           title: String,
           role: String,
       },
       Dashboard,
       Settings,
   }
   ```
2. **Demand-Driven Inclusion**:
   - Starting a fresh session mounts `Tab::Thread`.
   - Running `/dashboard` (or `C-x d`) mounts or focuses `Tab::Dashboard`.
   - Running `/settings` (or `C-x c`) mounts or focuses `Tab::Settings`.
   - Opening an existing thread from Dashboard appends a new thread tab.
3. **Navigation & Shortcuts**:
   - `Alt+1` .. `Alt+9`: Direct jump to tab by ordinal index.
   - `/tab next`, `/tab prev` (or `Ctrl+Tab`, `Ctrl+Shift+Tab`): Sequential tab cycling.
   - `/close` (or `Ctrl+W`): Closes current tab (executes `Detach`).
   - `/kill`: Explicit business command that halts the active thread and closes its tab.

### 2. Reactive Surface Synchronization (`[INV-TAB-01]`)
- When the client has `Tab::Dashboard` or `Tab::Settings` open, it subscribes directly to the server's authoritative event broadcast stream.
- Configuration edits in one client immediately fan out and re-render open Settings tabs across all connected clients.
- Thread milestone completions fan out to update open Dashboard tabs in real-time with zero polling.

---

## Invariants & Behavioral Boundaries

- **[INV-TAB-01] Viewport Isolation**: The client process and its tab collection MUST remain strictly decoupled from server-side thread lifecycles. Tab creation and destruction MUST NOT mutate underlying thread durability.
- **[INV-TAB-02] Demand-Driven Tab Inclusion**: A client MUST only display tabs that were explicitly mounted or created in the current client session. Stored sessions in `nuo.db` MUST NOT be automatically converted into tabs on startup.
- **[INV-TAB-03] Non-Destructive Detach on Tab Close**: Closing a thread tab MUST detach the local connection without terminating the underlying thread or canceling running tool steps.
- **[INV-TAB-04] Exit on Empty Tab Set**: When the last remaining tab in a client viewport is closed, the client process MUST detach cleanly and terminate its interface process.
- **[INV-TAB-05] Slash-Only Command Uniformity**: Tab navigation, creation, and destruction MUST be driven exclusively via canonical `/` commands or designated ergonomic key bindings. Colon-prefixed modal commands are strictly prohibited.

---

## Positive Consequences

- Delivers a unified, fluid multi-tasking workspace inside a single terminal window.
- Eliminates the cognitive cleavage between full-screen scenes and background threads.
- Aligns tab closure semantics with modern browser and IDE paradigms.
- Preserves kernel-level agent autonomy during all frontend navigation operations.
