# How to Manage Threads and Sessions

Browse, resume, and terminate hosted threads, and drive the session server lifecycle.

---

## Before You Start

Threads are the sole conversational resource noun. The server hosts every thread across every project.

---

## 1. Browse and Resume Threads

From a shell, open the interactive thread picker (no id), or attach directly to a thread by id:

```bash
nuo -t
nuo -t <thread_id>
```

`nuo --resume` resumes the most recent matching thread. From inside the interface, the `/threads` command opens the same picker.

Both listings lead with the **most recently created** thread and read downward to
the oldest one. A fork stays nested beneath the trunk it came from no matter how
new it is, and a trunk's branches are likewise newest first. Positions come from
creation time only — never from the last time a thread was touched — so the list
does not shift under the cursor while a model streams (ADR-0128).

The dashboard fleet table is the exception: its `#N` labels are the addresses the console
accepts (for example `@3 please retry`), counting from the oldest session upward (ADR-0120, ADR-0128).

---

## 2. Global Thread Fleet Dashboard

Open the full-screen fleet dashboard (`/dashboard` or `d` via Help `[F1 / Alt-h]`) to monitor and direct every hosted thread across all projects:

- **Fleet Overview**: Live metrics band displaying active thread count, running state, items requiring attention (`NeedsApproval`, `NeedsInput`, `Failed`), and total tokens.
- **Structured Fleet Table**: Lists threads with authentic titles (`Mission / Intent`), visual status chips, workspace names, active tool execution or turn progress, context token metrics, and age.
- **Deep-Dive Dossier**: Focused thread details display the cognitive mission, intent, milestone progress, tool execution state, and lineage fork point.
- **Direct Fleet Controls**:
  - `Enter` / `a`: Switch to and attach the selected thread.
  - `i`: Interrupt the active generation or round.
  - `k`: Terminate the selected thread (two-press confirmation safety).
  - `s`: Suspend the thread into memory.
  - `p`: Send an immediate prompt or task to the selected thread.
  - `n`: Launch a brand-new thread.
  - `/`: Live substring filter across titles, IDs, statuses, and workspaces.
  - `u`: Switch between Thread Fleet table and decoupled Model Usage analytics.

---

## 3. List and Terminate Threads

List hosted threads and their state, then terminate one by id:

```bash
nuo thread ls
nuo thread rm <thread_id>
```

---

## 4. Drive the Server Lifecycle

Start, stop, reload, restart, and inspect the session server:

```bash
nuo server start [--fg] [--port <n>]
nuo server stop
nuo server reload
nuo server restart
nuo server status
nuo server token
```

`nuo server start` runs the server detached by default; pass `--fg` for containers and service managers. `nuo status` observes health without ever spawning a server.

---

## Related

- [Inspect Token Usage](inspect-usage.md)
- [Reference: Lifecycle and Action Semantics](../reference/lifecycle-and-action-semantics.md)
