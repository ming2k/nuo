---
id: ADR-0040
title: "Cross-Domain Action Matrix, Three-Tier Header Topology, and Polymorphic TUI Architecture"
status: accepted
date: 2026-10-18
scope: tui/nuo-tui, architecture/spatial, wire/nuo-wire, server/nuo-server, interaction/keyboard
superseded_by: null
negative_knowledge: true
---

# 0040. Cross-Domain Action Matrix, Three-Tier Header Topology, and Polymorphic TUI Architecture

- Status: Accepted
- Date: 2026-10-18
- Deciders: Nuo Architecture Working Group
- Consulted: Interface, Design, Runtime, Terminal, and Usability Teams
- Informed: System Architects, Release Engineering
- Complements: [ADR-0038](0038-thread-entity-server-ssot-fanout-and-orthogonal-action-lifecycle.md), [ADR-0039](0039-client-tab-workspace-scene-unification-and-reactive-surface-sync.md)

---

## Context and Problem Statement

Following the establishment of the persistent `Conversation` entity (ADR-0038) and the client-centric `ClientTab` workspace (ADR-0039), the interaction architecture required a second-order harmonization across two critical dimensions:

1. **Domain-Asymmetric Action Conflation**:
   Prior lifecycle specifications defined actions primarily through the lens of turn-based dialogues (`Prompt`, `Interrupt`, `EndSession`), neglecting non-conversational singletons (`Dashboard`, `Settings`, and `Server`). For instance, operators attempted to reason about "interrupting" or "killing" a dashboard, or confused closing a settings pane with shutting down services. The system lacked an explicit stratification distinguishing **Universal Viewport Actions** (which apply to any tab) from **Domain-Specific Actions** (tailored to threads, monitoring streams, or configuration hubs).
2. **Header Hierarchy Collision and Global History Leaks**:
   Earlier terminal layout standards (ADR-0024 two-row head band) assumed that row 1 was statically fixed to a single thread ID and row 2 to a static scene label. In a tab-centric container hosting polymorphic sessions, this created severe architectural anomalies:
   - Client-level tab management, tab-level navigation history, and scene-level metadata were crushed into two ambiguous rows.
   - History stacks were held globally by the router (`scene_history`), meaning that drilling into a task inspection on Tab 1 and then switching to Tab 2 caused the back-navigation stack to interleave and cross-contaminate across unrelated tabs.
   - Operators navigating subagent tasks or diff inspections within a tab lacked a dedicated, browser-like navigation stack (Back, Forward, and Breadcrumbs).

We require a formalized cross-domain action matrix and a modernized **Three-Tier Header Topology** that maps Client, Tab, and Scene directly onto discrete visual and behavioral planes.

---

## Decision Drivers

- **Orthogonal Action Stratification (`[INV-ACT-01]`)**: Universal viewport navigation actions (`OpenTab`, `CloseTab`, `FocusTab`) must be mathematically decoupled from domain-specific operations (`Prompt`, `Filter`, `Commit`, `Shutdown`).
- **Three-Tier Header Stratification (`[INV-HEAD-01]`)**: The header area must organize into three strictly separated, dedicated single-row tiers:
  1. *Row 1: Client Global TabBar* (Client-level, Scene-independent viewport set).
  2. *Row 2: Tab Navigation & History Bar* (Tab-autonomous Back/Forward stack and breadcrumbs).
  3. *Row 3: Scene Context Head* (Scene-level contextual metadata).
- **Autonomous Tab History Isolation (`[INV-HEAD-02]`)**: Each mounted `ClientTab` MUST independently own its own navigation history stack (`Vec<SceneKind>` + cursor). Switching tabs MUST NOT leak, clobber, or interleave navigation history across tabs.
- **Bi-Directional History Navigation Ergonomics (`[INV-KEY-02]`)**: Drilling into views and stepping back MUST be operable via zero-latency physical chords (`Alt+Left` / `Esc` for Back, `Alt+Right` for Forward).

---

## Considered Options

### Option 1: Uniform Universal Action Surface (Everything Has the Same Verbs)
- Force every entity (Thread, Dashboard, Settings, Server) to implement an identical set of generic verbs (`Start`, `Stop`, `Pause`, `Resume`).
- *Assessment*: Rejected. Produces cognitive dissonance. "Pausing" a thread means queuing turns, while "pausing" a dashboard means freezing the event scroll; "stopping" a settings view is nonsensical. Domain precision is essential for developer clarity.

### Option 2: Shared Global Scene History with Conflated Header Rows
- Retain the two-row header and keep a single global `scene_history` vector on `SurfaceRouter`.
- *Assessment*: Rejected. Causes "history leakage" across tabs: pressing `Esc` on a clean tab jumps backward to a view opened inside a different tab.

### Option 3: Two-Tier Action Stratification with 3-Tier Header Topology (Chosen)
- Bifurcate actions into **Universal Viewport Operations** and **Domain-Specific Operations** across four discrete domains: Conversation, Dashboard, Settings, and Server.
- Standardize the header into three discrete vertical rows mapping 1:1 to Client, Tab, and Scene tiers.
- Equip each `ClientTab` with its own autonomous browser-style history stack (`history: Vec<SceneKind>`, `cursor: usize`).

---

## Rejected Alternatives

Per **[INV-AGENT-01]**, the following alternative architectures were evaluated and rejected:

1. **Rejected: Embedding Configuration and Dashboard Under Thread Commands**
   - *Reason*: Treating settings and cluster monitoring as synthetic tools or messages inside a thread transcript pollutes the LLM context window with system telemetry and forces artificial turn cycles for local configuration edits.
2. **Rejected: Multi-Row Tab Bars When Tabs Exceed Screen Width**
   - *Reason*: Allowing the tab bar to wrap onto multiple vertical rows steals precious reading height from the main transcript area. Instead, horizontal scrolling with directional overflow markers (`<`, `>`) preserves fixed vertical geometry.
3. **Rejected: Global Esc Key Clearing the Entire Viewport**
   - *Reason*: Pressing `Esc` at the root of a tab's history stack must NOT close the client window. Viewport closure must require an explicit `/close`, `Alt+W`, or window close event.

---

## Decision Outcome

Chosen option: **Option 3**.

### 1. Cross-Domain Action Matrix (`[INV-ACT-01]`)

```text
                                  Action Taxonomy
                                         │
                 ┌───────────────────────┴───────────────────────┐
                 ▼                                               ▼
     Universal Viewport Actions                         Domain-Specific Actions
  (OpenTab, CloseTab, FocusTab)                                  │
                                 ┌───────────────┬───────────────┴───────────────┐
                                 ▼               ▼                               ▼
                             [Thread]       [Dashboard]                      [Settings]
```

#### A. Universal Viewport Actions (All Tabs)
- **`OpenTab(target)`**: Mounts a new tab or focuses an existing singleton tab (`Dashboard`, `Settings`).
- **`CloseTab` (`Alt+W`, `/close`)**: Detaches the current tab view. If the closed tab is a thread, the thread continues running in background. Closing the final tab exits the client.
- **`FocusTab(index)` (`Alt+1`..`Alt+9`)**: Directly switches active focus to the tab at ordinal index.
- **`CycleTab(direction)` (`Ctrl+Tab`, `Ctrl+Shift+Tab`)**: Cycles sequentially between open tabs.

#### B. Domain-Specific Actions

| Domain | Actions | Triggers | Semantics |
| :--- | :--- | :--- | :--- |
| **`Thread`**<br>*(Multi-instance)* | **`Prompt`**<br>**`Steer`**<br>**`Interrupt`**<br>**`Fork`**<br>**`Kill`** | `Enter`<br>`Ctrl+Enter`<br>`Esc Esc` / `Ctrl+C`<br>`/fork`<br>`/kill` | - Dispatches prompt + attachments into agent round.<br>- Injects high-priority steering turn into active loop.<br>- Cancels current round/tool; preserves context.<br>- Creates divergent child thread branch.<br>- Halts execution driver and closes tab. |
| **`Dashboard`**<br>*(Singleton Hub)* | **`SubscribeFeed`**<br>**`PauseFeed`**<br>**`Filter`**<br>**`InspectTask`**<br>**`KillJob`** | Auto on focus<br>`Space` / Scroll<br>Query input<br>`Enter` on row<br>`x` on job | - Subscribes to server broadcast event bus.<br>- Freezes auto-scroll to inspect historical lines.<br>- Narrows visible list by status / role.<br>- Mounts selected session as a new thread tab.<br>- Terminates hung background process. |
| **`Settings`**<br>*(Singleton Hub)* | **`Mutate`**<br>**`Commit`**<br>**`Probe`**<br>**`Reload`**<br>**`Reset`** | Edit fields<br>`Enter`<br>`p`<br>`nuo server reload`<br>`/reset` | - Edits provider, credentials, MCP, or theme values.<br>- Submits mutation to server single-writer for broadcast.<br>- Requests live connection / MCP health check.<br>- Hot re-reads configuration files without disconnect.<br>- Restores field or section to factory default. |
| **`Server`**<br>*(Daemon Coordinator)* | **`Shutdown`**<br>**`Restart`**<br>**`Takeover`** | `nuo server stop`<br>`nuo server restart`<br>Boot collision | - Executes phased 3-stage drain and unlinks sockets.<br>- Triggers phased restart; clients auto-reconnect.<br>- Forcibly terminates stale PID pinning sockets/locks. |

---

### 2. Three-Tier Header Topology (`[INV-HEAD-01]`, `[INV-HEAD-02]`)

```text
┌────────────────────────────────────────────────────────────────────────┐
│ [1* 🔨refactor-db]  [2 ●quick-query]  [3 📊dashboard]         C-x menu │ ◄ Row 1: Client Global TabBar (Client-level)
├────────────────────────────────────────────────────────────────────────┤
│ < Back (Alt+←) | > Fwd (Alt+→) | Conv > Subagent Task #1 > Diff        │ ◄ Row 2: Tab Navigation & History Stack (Tab-level)
├────────────────────────────────────────────────────────────────────────┤
│ conv: a1b2c3d4 | role: developer | ~/projects/nuo      unattended: off │ ◄ Row 3: Current Scene Head (Scene-level)
├────────────────────────────────────────────────────────────────────────┤
│                                                                        │
│                       Polymorphic Content Body Area                    │
│                                                                        │
├────────────────────────────────────────────────────────────────────────┤
│ [Activity Bar]: ● Thinking (DeepSeek V3) ...         Esc Esc Interrupt │ ◄ Contextual Activity / Status Bar
├────────────────────────────────────────────────────────────────────────┤
│ > What is the next step in the refactor?                               │ ◄ Contextual Input / Composer
├────────────────────────────────────────────────────────────────────────┤
│ Alt+1..4 Tab | Alt+W Close | /kill Terminate | Alt+←/→ History         │ ◄ Standing Footer Affordance
└────────────────────────────────────────────────────────────────────────┘
```

1. **Row 1: Client-Level Global TabBar**:
   - Fixed height: 1 row. Owned by the Client container. Scene-independent.
   - Renders all mounted tabs: `[1* ...]`, with numeric keys for `Alt+1..9` direct jumping.
   - Renders ambient liveness indicators (`●`) on inactive tabs running background work.
   - Carries the standing `C-x menu` client command namespace pair on the right.
2. **Row 2: Tab-Level Navigation Stack & Breadcrumbs**:
   - Fixed height: 1 row. Owned by the currently active `ClientTab`.
   - Renders browser-like navigation affordances:
     - `< Back (Alt+←)` (dimmed when at stack bottom).
     - `> Fwd (Alt+→)` (dimmed when at stack top).
     - Full drill-in breadcrumb trail (`Thread > Subagent Task > Diff`).
3. **Row 3: Scene-Level Context Head**:
   - Fixed height: 1 row. Owned by the active projected `Scene`.
   - Displays real-time operational metadata (UUID, role profile, bound workspace root for thread-scoped scenes, confinement status). Non-thread scenes (Dashboard, Settings) never display workspace roots.

---

### 3. Tab Autonomous History Model (`[INV-HEAD-02]`)

```rust
pub struct ClientTab {
    pub kind: TabKind,
    pub title: String,
    pub history: Vec<SceneKind>,
    pub cursor: usize,
}
```

- Each tab manages its own history lineage and pointer.
- Drilling into a subagent task on Tab 1 pushes `SceneKind::TaskInspection` to Tab 1's stack.
- Switching to Tab 2 and back preserves Tab 1's exact drill-in position, stack, and scroll state.
- `Alt+Left` / `Esc` navigates back within the active tab's stack; at the root level, `Esc` remains within the root scene.

---

## Invariants & Behavioral Boundaries

- **[INV-ACT-01] Strict Action Stratification**: Viewport lifecycle operations (`CloseTab`, `FocusTab`) MUST NOT be overloaded with domain-specific task controls (`Kill`, `Interrupt`, `Commit`).
- **[INV-HEAD-01] Strict Three-Tier Header Stratification**: The header area MUST strictly render as three discrete, dedicated single-row tiers: Row 1 Client TabBar, Row 2 Tab History Bar, Row 3 Scene Context Head.
- **[INV-HEAD-02] Autonomous Tab History Stacks**: Each mounted `ClientTab` MUST independently own its navigation history stack. Switching tabs MUST NOT leak, clobber, or interleave navigation history across tabs.
- **[INV-KEY-01] Tab Navigation Uniformity**: `Alt+1` through `Alt+9` MUST exclusively navigate tabs by ordinal index. `Alt+W` MUST close the active tab.
- **[INV-KEY-02] Bi-Directional History Navigation Chords**: `Alt+Left` MUST step backward in the active tab's navigation stack; `Alt+Right` MUST step forward.

---

## Positive Consequences

- Completely eliminates navigation history leakage across parallel threads.
- Delivers an intuitive, browser-grade multi-tasking workspace inside terminal constraints.
- Provides immediate visual transparency: tab switching (Row 1), drill-in depth (Row 2), and scene parameters (Row 3).
- Maintains strict 1:1 conceptual mapping between software entities and visual planes.
