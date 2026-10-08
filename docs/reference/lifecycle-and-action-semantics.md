# Lifecycle and Action Semantics Reference

- Status: Living Reference
- Scope: architecture/lifecycle, server/session, client/actions, runtime/state-machine
- Deciders: Nuo Architecture Working Group

---

## 1. Architectural Entity Tiers

The Nuo system operates across three orthogonal entity tiers:

```text
┌────────────────────────────────────────────────────────────────────────┐
│                        1. Client Tier (UI/IPC)                         │
│   Terminal 1 (TUI)       Terminal 2 (TUI)       CLI Script (nuo run)   │
└───────────────────┬──────────────────┬──────────────────┬──────────────┘
                    │ (Attach/Detach)  │ (Attach/Detach)  │
                    ▼                  ▼                  ▼
┌────────────────────────────────────────────────────────────────────────┐
│                       2. Session Tier (Logical)                        │
│   Session A (Interactive)    Session B (Autonomous)   Session C (Trunk)│
│   - Context & Transcript      - Active tool tasks      - Role & Bounds │
└───────────────────┬──────────────────┬──────────────────┬──────────────┘
                    │                  │                  │
                    ▼                  ▼                  ▼
┌────────────────────────────────────────────────────────────────────────┐
│                       3. Server Tier (Coordinator)                     │
│   Single-Writer SQLite Host (nuo.db)    Local Control Socket & IPC     │
└────────────────────────────────────────────────────────────────────────┘
```

1. **Client Tier (`Client`)**:
   An ephemeral interactive frontend or headless command process (e.g. `nuo` TUI, `nuo -p`, web client). Multiple clients can operate concurrently in separate terminal windows.
2. **Session Tier (`Session`)**:
   A stateful agent workspace hosted on the server, identified by a UUID. Holds transcript turns, pending tool operations, active subagents, cognitive state, and is persisted in SQLite (`nuo.db`).
3. **Server Tier (`Server`)**:
   A single-instance coordinator process owning SQLite exclusive writing rights (`nuo.db.owner.lock`), local IPC socket binding (`server.sock`), session scheduling, and background services.

---

## 2. Action Taxonomy & Behavioral Semantics

| Action | Target Tier | Triggers | Semantics & State Transitions | Durability & Residue |
| :--- | :--- | :--- | :--- | :--- |
| **`Attach`** | Client → Session | `nuo`, `nuo attach [id]`, `nuo resume` | Binds client IPC streams to a specified (or fresh) hosted session. Increases server's active interactive client count (`interactive_count + 1`). If the session was running autonomously, upgrades its human channel back to Interactive. | Session transcript and turns sync from `nuo.db`. |
| **`Detach`** | Client ⇸ Session | Closing terminal tab, network disconnect, SIGHUP, terminal exit | Unbinds client connection from the hosted session. Decreases server's active client count (`interactive_count - 1`). **The session is NOT terminated.** It transitions to **Autonomous Mode**: background tasks, tool executions, and LLM turns continue uninterrupted. If `interactive_count` reaches 0 in client-driven mode, server arms a 1.5s debounce timer before shutting down. | Full state and in-flight outputs persist to `nuo.db`. |
| **`Interrupt`** | Session (Turn) | `Esc Esc`, `Ctrl+C`, UI interrupt button | Cancels in-flight LLM stream, tool execution, or pending task queue for the current round via `CancellationToken`. Records `Interrupted` round event. **Does NOT disconnect client, does NOT terminate session.** Re-arms input prompt immediately. | Interrupted turns are recorded cleanly in SQLite transcript. |
| **`Exit` / `EndSession`** | Session | TUI `/exit`, `/quit`, Web end session | Explicit declaration of session completion. Client sends `AgentRequest::EndSession`. Server cancels driver task, fires session end hooks, unregisters session mailbox from registry, broadcasts `SessionRemoved`, and client exits. | State and transcript remain durable in `nuo.db` for review/resumption, but active in-memory coordinator is dropped. |
| **`KillSession`** | Session | `nuo session delete [id]`, RPC control verb | Forcefully unhosts and cancels the session. Optionally cascades deletion of session records from SQLite `nuo.db`. | Permanent deletion if requested, or settled as unhosted. |
| **`Stop` / `Shutdown`** | Server | `nuo server stop`, `nuo stop`, all clients closed timeout | Initiates phased server draining: (1) revoke discovery `server.json`, (2) stop ingress and wait for active tools/turns within grace budget, (3) execute SQLite WAL checkpoint, unlink `server.sock`, and release `server.lock`. Exit code 0. | Zero in-flight data corruption. |
| **`Reload`** | Server (Config) | `nuo server reload` | Hot re-reads configuration files (`config.toml`), reloads model provider credentials, and refreshes MCP/skills catalog. **Zero disconnection**: all active WebSocket/IPC connections and running sessions remain undisturbed. | In-memory configuration update only. |
| **`Restart`** | Server | `nuo server restart` | Level 2 (Graceful): Requests graceful shutdown with 3s budget, unlinks discovery record, spawns replacement binary. Connected clients enter auto-reconnect loop and transparently re-attach. Level 3 (`--force`): Immediately issues SIGKILL to lingering process and forces socket/lock rebind. | Resumes cleanly from durable SQLite state. |
| **`Takeover`** | Server | Starting server on occupied socket/port/lock | Initiated during server startup before database initialization. Probes PID holding `server.lock`, `nuo.db.owner.lock`, or UDS. Requests graceful exit (500ms budget), then escalates to SIGKILL if lingering, guaranteeing deterministic single-writer semantics. | Reclaims stale locks without operator intervention. |

---

## 3. Session State Machine

```text
               ┌──────────────────────┐
               │    Fresh (Staged)    │
               └──────────┬───────────┘
                          │ First turn / Prompt
                          ▼
               ┌──────────────────────┐
    ┌─────────►│     Interactive      │◄─────────┐
    │          │  (Client Attached)   │          │
    │          └──────────┬───────────┘          │
    │                     │                      │
    │ Client Re-attach    │ Client Detach        │ Client Re-attach
    │ (nuo attach)        │ (Terminal Closed)    │ (nuo attach)
    │                     ▼                      │
    │          ┌──────────────────────┐          │
    └──────────┤      Autonomous      ├──────────┘
               │ (Background Running) │
               └──────────┬───────────┘
                          │ /exit or EndSession
                          ▼
               ┌──────────────────────┐
               │  Ended (Persisted)   │
               └──────────────────────┘
```

1. **Interactive State**:
   A client is actively attached (`human_channel.attached == true`). Interactive prompts (`ask_user`, permission requests) wait for human input from the attached terminal.
2. **Autonomous State**:
   No client is currently attached (`human_channel.attached == false`). The session continues executing in background. Interactive questions resolve via automated policy (or pause), and tool outputs append to SQLite.
3. **Ended State**:
   The operator explicitly closed the session via `/exit` or `EndSession`. The runtime harness is dismantled, but the history is immutable and reloadable.
