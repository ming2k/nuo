# Lifecycle and Action Semantics Reference

- Status: Living Reference
- Scope: architecture/lifecycle, server/session, client/actions, runtime/state-machine, tui/spatial
- Deciders: Nuo Architecture Working Group
- Reference ADRs: [ADR-0033](../adr/0033-canonical-server-entity-dual-hosting-postures-and-endpoint-orthogonality.md), [ADR-0034](../adr/0034-deterministic-lifecycle-governance-phased-draining-and-tiered-restart.md), [ADR-0038](../adr/0038-thread-entity-server-ssot-fanout-and-orthogonal-action-lifecycle.md), [ADR-0039](../adr/0039-client-tab-workspace-scene-unification-and-reactive-surface-sync.md), [ADR-0040](../adr/0040-cross-domain-action-matrix-and-polymorphic-tui-spatial-topology.md)

---

## 1. Architectural Entity Tiers

The Nuo system operates across three orthogonal entity tiers:

```text
┌────────────────────────────────────────────────────────────────────────┐
│                        1. Client Tier (UI/IPC)                         │
│   Terminal 1 (TUI Viewport)    Terminal 2 (TUI Viewport)  Headless CLI │
└───────────────────┬──────────────────┬──────────────────┬──────────────┘
                    │ (Attach/Detach)  │ (Attach/Detach)  │
                    ▼                  ▼                  ▼
┌────────────────────────────────────────────────────────────────────────┐
│                        2. Thread Tier (Logical)                        │
│   Thread A (Multi-instance)              Thread B (Multi-instance)     │
│   - Context, role, transcript            - Autonomous background task  │
└───────────────────┬─────────────────────────────────────┬──────────────┘
                    │                                     │
                    ▼                                     ▼
┌────────────────────────────────────────────────────────────────────────┐
│                      3. Server Tier (Coordinator)                      │
│   - Exclusive SQLite Writer (nuo.db)   - Singleton Dashboard Stream    │
│   - Native Local IPC (server.sock)     - Singleton Settings Manager    │
└────────────────────────────────────────────────────────────────────────┘
```

1. **Client Tier (`Client`)**:
   An ephemeral interactive viewport container. Owns a local collection of `ClientTab` items mounted on demand.
2. **Thread Tier (`Thread`)**:
   A stateful agent dialogue hosted on the server, identified by a UUID. Holds transcript turns, pending tool operations, active subagents, and cognitive state, persisting authoritatively in SQLite (`nuo.db`).
3. **Server Tier (`Server`)**:
   A single-instance coordinator process owning SQLite exclusive writing rights (`nuo.db.owner.lock`), local IPC socket binding (`server.sock`), thread scheduling, and singleton services (`Dashboard`, `Settings`).

---

## 2. Universal Viewport Actions (All Tabs)

These actions govern the client's local tab container and apply uniformly across all mounted viewports:

| Action | Shortcuts / Slash | Semantics | Side Effects |
| :--- | :--- | :--- | :--- |
| **`OpenTab`** | `Ctrl+T`, `C-x d`, `C-x ,`, `/new` | Mounts a new tab or focuses an existing singleton tab. | Client-local; requests data stream from Server. |
| **`CloseTab`** | `Ctrl+W`, `/close` | Closes the active tab view (**pure Detach**). | If closed tab was a thread, it continues in Autonomous Mode. Closing last tab exits client process cleanly. |
| **`FocusTab`** | `Alt+1` .. `Alt+9` | Jumps directly to tab at ordinal index. | Repoints Layer 3 content projection instantly. |
| **`NextTab`** / **`PrevTab`** | `Ctrl+Tab`, `Ctrl+Shift+Tab` | Sequentially cycles through mounted tabs. | Re-anchors viewport. |

---

## 3. Domain-Specific Action Matrix

Operations tailored to specific session and coordinator domains:

| Domain | Action | Triggers | Semantics & Lifecycle Behavior |
| :--- | :--- | :--- | :--- |
| **`Thread`**<br>*(Multi-instance)* | **`Prompt`**<br>**`Steer`**<br>**`Interrupt`**<br>**`Fork`**<br>**`Kill`** | `Enter`<br>`Ctrl+Enter`<br>`Esc Esc` / `Ctrl+C`<br>`/fork`<br>`/kill` | - Dispatches prompt + attachments into agent round.<br>- Injects high-priority turn into running loop.<br>- Cancels active tool/stream; preserves context and stays attached.<br>- Creates divergent child branch from current node.<br>- Explicitly terminates thread driver and closes tab. |
| **`Dashboard`**<br>*(Singleton Hub)* | **`SubscribeFeed`**<br>**`PauseFeed`**<br>**`Filter`**<br>**`InspectTask`**<br>**`KillJob`** | Auto on focus<br>`Space` / Scroll<br>Type query<br>`Enter` on row<br>`x` on job | - Subscribes to server broadcast bus for real-time cluster telemetry.<br>- Freezes auto-scroll to inspect historical lines.<br>- Filters visible list by status or keyword.<br>- Mounts selected session as a new thread tab.<br>- Halts an orphaned background service process. |
| **`Settings`**<br>*(Singleton Hub)* | **`Mutate`**<br>**`Commit`**<br>**`Probe`**<br>**`Reload`**<br>**`Reset`** | Edit fields<br>`Enter`<br>`p`<br>`nuo server reload`<br>`/reset` | - Modifies provider, credentials, MCP, or theme options.<br>- Submits mutation to Server SSOT; Server writes and fans out update.<br>- Tests provider connection or probes MCP server live.<br>- Hot re-reads configuration files without dropping connections.<br>- Restores field or section to default values. |
| **`Server`**<br>*(Daemon Host)* | **`Shutdown`**<br>**`Restart`**<br>**`Takeover`** | `nuo server stop`<br>`nuo server restart`<br>Boot collision | - 3-stage graceful drain; closes connections; exits with 0.<br>- Phased restart; connected clients auto-reconnect.<br>- Forcibly terminates conflicting PID pinning sockets/locks. |

---

## 4. Five-Layer Polymorphic TUI Spatial Topology

```text
┌────────────────────────────────────────────────────────────────────────┐
│ [1* 🔨refactor-db]  [2 ●quick-query]  [3 📊dashboard (2)]  [4 ⚙️settings]│ ◄ Layer 1: Unified TabBar
├────────────────────────────────────────────────────────────────────────┤
│ conv: a1b2c3d4 | role: developer | ~/projects/nuo      unattended: off │ ◄ Layer 2: Contextual Subhead
├────────────────────────────────────────────────────────────────────────┤
│                                                                        │
│                                                                        │
│                 Layer 3: Polymorphic Content Body                      │
│                                                                        │
│   - Tab 1: Live Thread Transcript (Streaming Prose, Diff, Tools)        │
│   - Tab 3: Cluster Overview & Background Service Ledger                │
│   - Tab 4: Domain Configuration Matrix & Provider Editor               │
│                                                                        │
│                                                                        │
├────────────────────────────────────────────────────────────────────────┤
│ [Activity Bar]: ● Thinking (DeepSeek V3) ...         Esc Esc Interrupt │ ◄ Layer 4: Activity / Status
├────────────────────────────────────────────────────────────────────────┤
│ > What is the next step in the refactor?                               │ ◄ Layer 5: Contextual Input
├────────────────────────────────────────────────────────────────────────┤
│ Alt+1..4 Tab | Ctrl+W Close (Detach) | /kill Terminate | C-x Menu      │ ◄ Standing Footer Affordance
└────────────────────────────────────────────────────────────────────────┘
```

1. **Layer 1: Unified TabBar**: Pinned top row displaying mounted tabs, index numbers (`1`, `2`), active focus pill (`[1* ...]`), and background ambient activity indicators (`●`).
2. **Layer 2: Contextual Subhead**: Displays metadata tailored to the active tab's domain (Conversation ID & role, or Dashboard cluster stats, or Settings breadcrumbs).
3. **Layer 3: Polymorphic Content Body**: Full-bleed content surface hosting the transcript, cluster cards, or 3-pane configuration matrix.
4. **Layer 4 & 5: Contextual Input & Activity**: Bottom status bar + input composer tailored to active tab (Markdown composer, search filter, or field editor).
5. **Footer Affordance**: Standing single-row keycap legend at canvas floor.
