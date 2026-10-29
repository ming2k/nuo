# Built-in Command Reference

Reference for the built-in slash commands available inside the Nuo terminal interface. The vocabulary is a single declared table in `crates/nuo-server/src/startup.rs` (`BuiltinCmd::ALL`); input completion, `/help`, and dispatch are all generated from it, so a command appears in all three places or the build breaks.

---

## Built-in commands

Type these at the composer to open the corresponding surface or perform the action.

| Command | Summary |
| :--- | :--- |
| `/models` | Switch the active model (context preserved) |
| `/connections` | Manage LLM provider connections |
| `/tools` | Manage session tools (enable/disable) |
| `/mcp` | Manage MCP servers (status, reconnect) |
| `/compact` | Compact older rounds into durable context memory |
| `/new` | Start a fresh thread tab (or reset in-place if current thread is empty), preserving history |
| `/permissions` | Show or clear always-allowed tool rules |
| `/unattended` | Toggle unattended execution posture |
| `/confinement` | Toggle workspace filesystem confinement (confine file tools to workspace) |
| `/role` | Switch agent role (identity, capability, and workspace) |
| `/search` | Semantic search over session history |
| `/threads` | Browse or resume past threads — a lineage forest: branches nest under their trunk at any depth, each labelled with the address it forked at (`⑂r2.1`) |
| `/fork` | Fork thread into a child thread at head, at round (`/fork 2`), or after a turn (`/fork 2.1`); the address is persisted and shown in `/threads`, the dashboard dock, and `nuo thread ls` (ADR-0058, ADR-0120) |
| `/tree` | Visual DAG thread tree and branch navigation |
| `/diff` | View what this thread changed in the workspace, grouped by whether the change can be undone |
| `/undo` | Undo the last round's reversible file changes; refuses anything that moved on since |
| `/stats` | View context token accounting and thread stats |
| `/trace` | Inspect session execution trace and latency waterfall |
| `/usage` | Cross-session token usage statistics overlay |
| `/quotas` | Provider quota pool and allowance dashboard |
| `/jobs` | Inspect and manage background processes and sub-subagents |
| `/skills` | Browse available skills and view trust status |
| `/init` | Scaffold a project-local `.nuo/` config tree |
| `/trust` | Trust project-authored asset domains |
| `/untrust` | Revoke project asset trust |
| `/export` | Export thread to clipboard as Markdown |
| `/retry` | Retry last failed model request |
| `/help` | Show available commands and keybindings |
| `/exit` | Exit the program |

---

## Subcommands

Several commands take a subcommand argument:

| Command | Subcommand | Effect |
| :--- | :--- | :--- |
| `/permissions` | `clear` | Clear all always-allowed tool rules |
| `/unattended` | `on` / `off` | Enable / disable unattended decisions and tool auto-approval |
| `/confinement` | `on` / `off` | Enable / disable workspace filesystem confinement |
| `/jobs` | `kill` / `logs` | Terminate an active background job / show its recent output |
| `/skills` | `list` / `status` | List discovered skills / show health and trust state |
| `/trust` | `all`, `instructions`, `ex-workspace`, `mcp`, `skills`, `hooks`, `status`, `revoke` | Trust or inspect a project asset domain |

---

## Aliases

Aliases resolve at dispatch but are not listed by `/help`, which keeps to the canonical names
above. They *are* offered by input completion under their own spelling — selecting one commits
the canonical command, so the transcript only ever records the canonical form.

| Alias | Canonical |
| :--- | :--- |
| `/resume`, `/session`, `/sessions` | `/threads` |
| `/auto`, `/delegate`, `/autopilot`, `/yolo` | `/unattended` |
| `/unconfine`, `/unconfined`, `/jail`, `/escape` | `/confinement` |
| `/setup` | `/init` |

The table is declared once, in `crates/nuo-server/src/startup.rs` (`BuiltinCmd::from_alias`),
and the completion catalog is generated from that same declaration — so an alias cannot be
documented, or offered in the popup, without being dispatchable.

---

## Mistyped commands

A `/`-prefixed draft is always a command: the client ships it to the dispatcher rather than
guessing, so a name that is not in the vocabulary is answered with a typed error instead of
being appended to the transcript as a prompt (ADR-0119, `[INV-CMD-02]`).

```
Unknown command: /modles. Did you mean /models? (Prefix the input with a space to send it as text.)
```

- The nearest built-in is named when there is one (prefix match, then a bounded edit distance).
- The rejected invocation is handed back to the composer, so fixing a typo costs one
  keystroke rather than a retype. If you have already typed something else, your text is
  untouched.
- To send text that merely *starts* with `/`, prefix it with a space: ` /etc/hosts` is prose
  and goes to the model. The marker space is consumed, not sent: the model receives
  `/etc/hosts`. Two or more leading spaces are ordinary indentation and are sent verbatim.

---

## Undoing changes (`/diff`, `/undo`)

Every tool call that changed state outside the conversation is recorded in a
durable workspace journal (`workspace_effects`, ADR-0125). `/diff` projects it and
`/undo` acts on it, and both are honest about the boundary:

- **Reversible** — `write_file`, `edit_text`: the journal captured the file's
  bytes *before* the call, so `/undo` can put them back exactly.
- **Irreversible** — `execute_command` and every network or MCP tool. These are
  recorded as receipts (what ran, when, what it left on disk) but can never be
  undone: a shell command may have written anything, published anything, or
  changed a database, and the harness cannot tell which from the command text.
  `/undo` names them and refuses rather than pretending.
- **Unknown** — a call that was journaled but never completed (the process died
  between the write and its result). The journal cannot say whether these
  changed anything, so `/diff` lists them separately and `/undo` leaves them.

`/undo` restores byte-for-byte, and **only** when the file still holds exactly
what this thread wrote. If you (or a later round, or a formatter) changed the
file since, it is left alone and reported:

```
Not restored (the file changed after this thread wrote it):
- src/lib.rs
  the file changed after this thread wrote it; restoring the old content would discard that work
```

That refusal is the point. An `/undo` that overwrote your own edits would be
worse than one that declines.

## Approving a change

When a write needs your authority, the prompt shows the change itself, not just the
path — a unified diff with `+N -M` counts, computed from the file's current bytes
against what the call would write.

An approval prompt whose most decision-relevant content is hidden behind a toggle is
a prompt that gets approved without being read, so the diff is always visible; the
`Details` expansion adds the full argument JSON.

Two honest non-answers are also shown explicitly:

- *"No change: this call rewrites the file with identical content."* — an idempotent
  write.
- *"This call's effect cannot be shown in advance."* — `execute_command`, a network
  fetch, a subagent. No preview is possible without causing the effect, and a prompt
  that implied otherwise would be inventing confidence.

Previews are pure: they read the workspace and nothing else. The diff you approve is
computed by the same function that performs the edit, so what you see is what gets
written.

## Seeing a whole turn before it runs

When the model issues several writes in one turn, each tool card shows its diff
**before anything executes** — the turn's entire footprint is visible as a set, not
as N separable approvals. A batch of five writes is one reviewable change, not five
questions asked one at a time.

This is display-only. A preview appearing in the transcript is *not* evidence that a
change happened: no bytes are written until the tool runs, and a preview is never
persisted, so a resumed session never replays a diff that was never applied. The
authoritative record of what changed is `/diff`.

---

## Related

- [Terminal UI Reference](tui/index.md): the composer and chrome these commands drive.
- [Lifecycle and Action Semantics Reference](lifecycle-and-action-semantics.md): the cross-domain action matrix.
