# How to Manage Threads and Sessions

Browse, resume, stop, and delete role-scoped thread history, and drive the session server lifecycle.

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

`nuo --resume` resumes the most recent matching thread. From inside the interface, the `/threads` command opens the same picker. Both are role-scoped: the active thread's `(role, workspace)` decides which history is visible, and switching into a different role switches the history bucket.

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

## 3. List, Stop, and Delete Threads

Thread operations are role-scoped. `--role` defaults to `developer`, whose
workspace policy binds it to the current project. A workspace-free role such as
`philosophist` or `ops` resolves to its unbound history instead.

```bash
# Developer history in this project (the default scope).
nuo thread ls
nuo thread ls --json

# A different role, or a different workspace binding.
nuo thread ls --role philosophist
nuo thread ls --role coder --project /repo-a

# Explicit expansion: one role across workspaces, or every role in this one.
nuo thread ls --role developer --all-workspaces
nuo thread ls --all-roles

# Runtime-only stop: the driver ends, the transcript remains.
nuo thread stop <thread_id>
nuo thread stop --all --force

# Permanent deletion: stops a hosted target, then removes its durable row,
# transcript, derived indexes, snapshot, and log.
nuo thread rm <thread_id>
nuo thread rm --all --dry-run     # preview scope and counts
nuo thread rm --all               # prompts; default is "no"
nuo thread rm --all --force       # scripted teardown
```

`rm --all` deletes only the selected role's history in the selected workspace.
`--all-workspaces` expands the workspace dimension; `--all-roles` expands the
role dimension. Both together are the explicit global admin operation. The
command lists the exact id set first, quotes the role/workspace/count, and then
sends that set to the server (ADR-0171). Threads created after the snapshot
survive. Subagent runs belonging to selected parents are deleted with them and
reported separately.

`--force` is only valid with `--all`; `--dry-run` is only valid with `--all`.
A single-id `rm` uses the thread's stored role and workspace; an explicit
`--role` acts as a guard and a mismatch is refused.

`rm` does not remove configuration, credentials, permissions, prompt history,
usage records, or the workspace effect journal. It removes thread history.

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
