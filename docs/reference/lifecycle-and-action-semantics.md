# Lifecycle and Action Semantics Reference

- Status: Living Reference
- Scope: architecture/lifecycle, server/session, client/actions, runtime/state-machine, tui/spatial
- Deciders: Nuo Architecture Working Group
- Reference ADRs: [ADR-0033](../adr/0033-canonical-server-entity-dual-hosting-postures-and-endpoint-orthogonality.md), [ADR-0034](../adr/0034-deterministic-lifecycle-governance-phased-draining-and-tiered-restart.md), [ADR-0038](../adr/0038-thread-entity-server-ssot-fanout-and-orthogonal-action-lifecycle.md), [ADR-0039](../adr/0039-client-tab-workspace-scene-unification-and-reactive-surface-sync.md), [ADR-0040](../adr/0040-cross-domain-action-matrix-and-polymorphic-tui-spatial-topology.md), [ADR-0043](../adr/0043-thread-domain-nomenclature-unification-compact-tabbar-affordance-and-c-x-namespace-reorganization.md), [ADR-0060](../adr/0060-three-tier-spatial-header-topology-governance-band-and-clean-tab-stack.md), [ADR-0062](../adr/0062-polymorphic-scenebar-projection-sovereign-thread-context-and-consolidated-activity-strip.md)

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
| **`Thread`**<br>*(Multi-instance)* | **`Prompt`**<br>**`Steer`**<br>**`Interrupt`**<br>**`Fork`**<br>**`Kill`** | `Enter`<br>`Ctrl+Enter`<br>`Esc Esc` / `Ctrl+C`<br>`/fork`<br>`/kill` | - Dispatches prompt + attachments into agent round.<br>- Injects high-priority turn into running loop.<br>- Cancels active tool/stream; preserves context and stays attached.<br>- Creates divergent child branch from current node or specified round/turn boundary (ADR-0058).<br>- Explicitly terminates thread driver and closes tab. |
| **`Dashboard`**<br>*(Singleton Hub)* | **`SubscribeFeed`**<br>**`PauseFeed`**<br>**`Filter`**<br>**`InspectTask`**<br>**`KillJob`** | Auto on focus<br>`Space` / Scroll<br>Type query<br>`Enter` on row<br>`x` on job | - Subscribes to server broadcast bus for real-time cluster telemetry.<br>- Freezes auto-scroll to inspect historical lines.<br>- Filters visible list by status or keyword.<br>- Mounts selected thread as a new thread tab.<br>- Halts an orphaned background service process. |
| **`Settings`**<br>*(Singleton Hub)* | **`Mutate`**<br>**`Commit`**<br>**`Probe`**<br>**`Reload`**<br>**`Reset`** | Edit fields<br>`Enter`<br>`p`<br>`nuo server reload`<br>`/reset` | - Modifies provider, credentials, MCP, or theme options.<br>- Submits mutation to Server SSOT; Server writes and fans out update.<br>- Tests provider connection or probes MCP server live.<br>- Hot re-reads configuration files without dropping connections.<br>- Restores field or section to default values. |
| **`Server`**<br>*(Daemon Host)* | **`Shutdown`**<br>**`Restart`**<br>**`Takeover`** | `nuo server stop`<br>`nuo server restart`<br>Boot collision | - 3-stage graceful drain; closes connections; exits with 0.<br>- Phased restart; connected clients auto-reconnect.<br>- Forcibly terminates conflicting PID pinning sockets/locks. |

---

## 4. TUI Spatial Layout and Chrome Hierarchy

Under [ADR-0040](../adr/0040-cross-domain-action-matrix-and-polymorphic-tui-spatial-topology.md), [ADR-0043](../adr/0043-thread-domain-nomenclature-unification-compact-tabbar-affordance-and-c-x-namespace-reorganization.md), [ADR-0060](../adr/0060-three-tier-spatial-header-topology-governance-band-and-clean-tab-stack.md), and [ADR-0062](../adr/0062-polymorphic-scenebar-projection-sovereign-thread-context-and-consolidated-activity-strip.md), the terminal viewport is structured into three sovereign vertical zones: the Polymorphic Top Chrome (2 rows for non-thread scenes, 3 rows for thread scenes), the Polymorphic Content Body (with transient `StickyAnchor`), and the Bounded Bottom Chrome.

```text
┌────────────────────────────────────────────────────────────────────────┐
│ [Refactor DB]  [Fix Auth]  [dashboard]                        C-x menu │ ◄ Tier 1: TabBar (Client Domain, Always Present)
├────────────────────────────────────────────────────────────────────────┤
│ THREAD b3c4  Fix retry logic in auth loop      ‹ thread › subagent(2) │ ◄ Tier 2: SceneBar (Scene Domain, Polymorphic)
├────────────────────────────────────────────────────────────────────────┤
│ [DEVELOPER] ~/projects/nuo                      UNATTENDED UNCONFINED │ ◄ Tier 3: ThreadContextBar (Thread Domain Only)
├────────────────────────────────────────────────────────────────────────┤
│ ╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌│
│ ▾ tool:search_text "query" (completed, 12 matches)                     │ ◄ StickyAnchor (Dynamic Context Anchor on scroll)
│ ╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌│
│                                                                        │
│                 Polymorphic Content Viewport                           │
│                                                                        │
│   - Thread Scene: Live Transcript (Streaming Prose, Tools, Diff)       │
│   - Dashboard Scene: Cluster Overview & Background Service Ledger      │
│   - Settings Scene: Domain Configuration Matrix & Provider Editor      │
│                                                                        │
├────────────────────────────────────────────────────────────────────────┤
│ [2 Queued]  ✻ Responding (DeepSeek V3) ...          Esc Esc Interrupt  │ ◄ ActivityStrip (Consolidated, Max 1 row)
├────────────────────────────────────────────────────────────────────────┤
│ > What is the next step in the refactor?                               │ ◄ Composer (Adaptive)
├────────────────────────────────────────────────────────────────────────┤
│ ↵ Send · ⌥↵ Newline                 deepseek/deepseek-chat 4.2k/64k 🟢 │ ◄ ModelBar (Fixed 1 row)
└────────────────────────────────────────────────────────────────────────┘
```

### Top Chrome: Polymorphic Header Stack (`[INV-HEAD-05]`, `[INV-SCENE-02]`)

1. **Tier 1: `TabBar` (Client Domain, Always Present)**:
   - Fixed height: 1 row. Owned by the Client container.
   - Projects open tabs with clean semantic titles (e.g. `Refactor DB`, `dashboard`, `settings`) or compact untitled fallbacks (`thread#b3c4`), strictly omitting ordinal numeric prefixes (`1:`).
   - Carries the standing `C-x menu` global client affordance on the right edge.
2. **Tier 2: `SceneBar` (Scene Domain, Universal & Polymorphic)**:
   - Fixed height: 1 row. Universal to all scene types.
   - Left side: Scene identity and primary authentic task title (`THREAD <id_tail> <title>` for thread; `DASHBOARD <summary>` for dashboard; `SETTINGS <section>` for settings; zero placeholder string on untitled threads).
   - Right side: Right-aligned drill-in navigation stack / breadcrumb trail (`‹ thread › subagent #1 › diff`) with back affordance (`‹`).
3. **Tier 3: `ThreadContextBar` (Thread Domain, Sovereign & Conditional)**:
   - Height: 1 row on Thread viewports; **0 rows on non-thread viewports** (`Dashboard`, `Settings`).
   - Left side: `[ROLE]` badge (brand tone) + tilde-shortened `workspace` path.
   - Right side: Persistent safety execution posture flags (`UNATTENDED`, `UNCONFINED`) styled in warning tone.

### Content Zone: Polymorphic Content & Scrolled Disclosure Anchors

- **Polymorphic Content Surface**: Full-bleed body hosting the active scene (live transcript stream, cluster telemetry cards, or configuration editor).
- **StickyAnchor (`[INV-DISCLOSE-01]`)**: When transcript content scrolls past the top edge, active/expanded disclosure summaries (tools, diffs, searches, or reasoning blocks) pin dynamically directly under the top chrome as a contextual anchor and collapse handle.

### Bottom Chrome: Bounded Footer Stack (`[INV-FOOTER-02]`)

1. **ActivityStrip (Consolidated, Max 1 Row)**:
   - Single-row dynamic strip placed immediately above `Composer`.
   - Left: Staged follow-up indicators (`[2 Queued]`) and background worker badges (`[1 Task]`).
   - Center/Left: Live agent execution phase (`✻ Responding (DeepSeek V3)...`, `● Thinking...`, `Awaiting confirmation`).
   - Right: Global cancellation affordance (`Esc Esc Interrupt`).
   - Collapses to 0 rows when harness is idle and outbox/tasks are empty.
2. **Composer**: Auto-wrapping multi-line input box for natural language instructions and slash commands.
3. **ModelBar**: Pinning flush to the composer bottom edge, displaying contextual key affordances (`↵ Send · ⌥↵ Newline`), active model identifier, reasoning effort, instance suffix, context window budget tokens/percent, and network connection status. Storage durability degradation is signaled via status indicators and transient alerts rather than occupying an independent layout row.
