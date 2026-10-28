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
| `/diff` | View workspace modifications made in this thread |
| `/undo` | Undo the last thread turn and file changes |
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

## Related

- [Terminal UI Reference](tui/index.md): the composer and chrome these commands drive.
- [Lifecycle and Action Semantics Reference](lifecycle-and-action-semantics.md): the cross-domain action matrix.
