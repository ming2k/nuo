---
id: ADR-0119
title: "Slash Command Admission: Thread-Scoped Naming, Server-Authoritative Vocabulary, and Recoverable Rejection"
status: accepted
date: 2026-10-28
scope: protocol/nuo-protocol, server/nuo-server, host/nuo, tui/composer, tui/event-loop
superseded_by: null
negative_knowledge: true
---

# 0119. Slash Command Admission: Thread-Scoped Naming, Server-Authoritative Vocabulary, and Recoverable Rejection

- Status: Accepted
- Date: 2026-10-28
- Deciders: Nuo Architecture Working Group
- Consulted: Server Core, Protocol, Interface Architecture
- Amends: [ADR-0041](0041-thread-command-purity-viewport-workspace-separation-and-tab-management-topology.md) — supplies the vocabulary and the admission rule that `[INV-CMD-01]` presupposed without defining

---

## Context and Problem Statement

Four defects shared one root cause: nothing in the system owned the question *"what is a
`/`-prefixed draft, and who decides?"*

1. **The vocabulary was named for a layer, not a scope.** The command vocabulary was called
   "harness commands". The harness is a *runtime wiring layer*; it is not the boundary the
   commands respect. That name invited the exact drift `[INV-CMD-01]` warns about —
   once "harness" means "anything the agent runtime runs", a client workspace view
   (`/dashboard`, `/settings`) looks admissible, and the firewall between thread-scoped
   operations and client navigation dissolves.
2. **Admission was decided on the client, with the wrong information.** The TUI held a
   `recognized_command` check against its local `CommandCatalog` and, on a miss, rewrote
   `InputAction::SendSlash` into `InputAction::SendChat`. That is a category error twice
   over: the catalog is a *projection* of the server's vocabulary (built-ins are compiled
   into `nuo-server`; an embedding adds more through `SlashCommandRegistry`), so the client
   could not recognize commands it must accept; and a misspelled command silently became a
   **user turn in the transcript**, which reads to the model as an instruction and to the
   user as a sent message they cannot recall.
3. **Rejection destroyed the draft.** The Enter that submits a command consumes the composer
   buffer. The dispatcher's unknown-command path answered with a plain
   `CommandResult::Error`, which is a *display* fact — nothing in it let a frontend give the
   text back. A one-letter typo therefore cost a full retype of the invocation.

Session evidence is unambiguous: the visible symptom users reported was "a mistyped `/…`
gets appended as a prompt and cannot be taken back", and the visible symptom for `@`
mentions was "completion only fires at the start of the line".

## Decision Drivers

- **Single authority per fact.** Command *membership* is a server fact (built-ins plus
  embedding-registered handlers); command *display* is a client fact. Neither may be
  inferred from the other.
- **Errors must be recoverable when nothing happened.** A rejected name dispatched nothing
  and touched no state; the draft is therefore still the user's property.
- **Naming carries the boundary.** A name that says "harness" cannot enforce "thread
  scope"; a name that says "slash command" states the surface and the scope at once.
- **Machine-readable, not prose-sniffed.** A frontend must not have to match error strings
  to decide whether to restore a draft.

## Architectural Decisions

### 1. The vocabulary is *slash commands* — thread-scoped by name and by rule

The completion/handoff contract in `nuo-tool` (re-exported as `nuo_wire`) is renamed in its
documentation and its types' doc contracts from "harness command" to **slash command**: a
`/name` control-plane operation scoped to the current thread (its model, context, tools, and
session). The name is the boundary: a workspace view is client navigation, is not
thread-scoped, and therefore has no place in that vocabulary (`[INV-CMD-01]`).

### 2. Admission belongs to the dispatcher (`[INV-CMD-02]`)

The TUI emits `SendSlash` for **every** `/`-prefixed draft and never downgrades one to
`SendChat`. It cannot do otherwise: its catalog is a projection, and an embedding's
registered handlers are absent from it. The router's job ends at classifying input by its
surface form.

`handlers_slash::dispatch` is the single admission point:

- a built-in, alias, or registered extension command runs;
- the retired-name steering table (`/btw` → fork) redirects;
- anything else produces `CommandResult::UnknownCommand`.

A `/`-prefixed draft therefore **never** becomes a transcript user turn by accident. The
escape hatch is explicit and user-controlled: exactly one leading space makes the text
prose, and that marker space is **consumed** (shipping it would put an invisible character at
the head of the prompt, and the error message advertises this hatch, so it is a contract).
Two or more leading spaces are ordinary indentation and stay verbatim.

### 2b. One owner for the vocabulary, and only one (`[INV-CMD-04]`)

Removing the client-side *judgement* was not enough; the client also held a second *copy* of
the vocabulary — a 480-line hand-written catalog in `shell/completion.rs` — and production
`/` completion ran off that copy while the server's own `complete_slash_items` was reachable
only from tests. The two lists had drifted in both directions:

| | |
|---|---|
| built-in the popup could not offer | `/quotas` |
| aliases the dispatcher resolves, popup could not complete | `/auto`, `/yolo`, `/session`, `/unconfine`, `/unconfined` |
| alias the popup offered, dispatcher could not admit | `/setup` |

Measured, not assumed — and invisible to every test, because both copies were individually
self-consistent.

The vocabulary therefore has exactly one declaration (`nuo_server::startup`) and exactly one
completion implementation (`nuo_server::input_completion`). The TUI calls
`complete_slash_items` in-process for its zero-latency tier (the function is pure and I/O-free,
so there is no round-trip to avoid) and keeps **only** presentation: turning wire offsets into
byte edits and painting rows. `/setup` was then declared where admission actually happens, and
`completion_catalog_matches_the_dispatchers_vocabulary` asserts agreement in both directions:
every advertised name is admitted, and every admitted command is advertised.

### 3. Rejection is a typed, recoverable result (`[INV-CMD-03]`)

`CommandResult::UnknownCommand { name, suggestion }` is a first-class wire variant, not an
`Error` with a recognized prefix:

- `name` is the typed invocation, so a frontend can restore it verbatim;
- `suggestion` is the nearest built-in (`startup::suggest_command`, prefix pass then a
  bounded edit-distance pass), **diagnostics only** — it is never dispatched and never
  offered as a completion;
- `to_text()` owns the wording, so live render, resume, and `/export` agree.

The applier reacts: on `UnknownCommand`, the invocation is handed back to the composer when
the composer is empty (caret at end, completion dismissal latched). If the user has already
typed something else, their text wins — the rejection remains visible in the transcript and
in input history for `↑`. A typo costs one edit, not a retype.

### 4. Cursor indices are normalized at the wire boundary

`App::cursor_position` is a **byte** offset (editor motions, the composer text grid, and the
`@`-mention scanner all index by byte). The control-plane completion contract is a
**Unicode scalar index**. `App::wire_cursor()` performs the conversion at the one boundary
where the two meet, and the request, the echoed response, and the cached-response
comparison all use it.

Sending bytes where the backend reads chars made every completion request whose prefix was
not pure ASCII unresolvable: the engine could not place the caret, returned no items, and a
mention typed anywhere after non-ASCII text (any CJK paragraph) never opened its menu —
reported as "`@file` only works at the start of a line". The mention *scanner* was already
byte-correct, which is why the popup's absence was the only visible symptom.

## Consequences

- **Positive**: The command/workspace firewall is enforced by vocabulary and by a single
  admission point instead of by convention. Typos are recoverable and self-correcting
  (`Did you mean /models?`). `@`-mention completion works for any prefix, including CJK
  prose, and unit tests now exercise the server's engine rather than a second
  implementation.
- **Negative & Trade-offs**: An embedding's registered handler is invisible to client
  completion until the server publishes it in the catalog, so the popup may under-offer
  while dispatch still accepts. Accepted deliberately: under-offering is recoverable,
  over-offering or mis-admitting is not. The consequence is that dispatch must stay the
  authority — any future client-side "is this a command?" shortcut reintroduces defect (2).
- **Negative & Trade-offs**: The TUI now ships unknown drafts to the server, costing one
  round-trip before the error appears. Commands are already synchronous control-plane
  operations on that same channel, so the latency is the existing one.

## Rejected Alternatives & Negative Knowledge

### Alternative A: Keep the client-side downgrade, but only for names the catalog rejects

- **Why considered**: Zero protocol change; the error would appear instantly.
- **Why rejected**: It re-creates exactly the failure being fixed — a mistyped command
  becomes a transcript prompt — and it denies registered extension commands, whose names the
  client provably cannot know.

### Alternative A2: Keep the client's second catalog copy and "keep them in sync"

- **Why considered**: `/` completion stays a pure in-process call with no protocol change.
- **Why rejected**: That was the status quo, and it had already drifted three ways (§2b) with
  zero test coverage — two individually self-consistent lists never disagree *with
  themselves*. Sync-by-discipline is not a mechanism. Calling the server's function in-process
  keeps the same zero-latency property (the function is pure) while making a second copy
  impossible to reintroduce without deleting a call.

### Alternative B: Send unknown commands to the model with an "I meant a command" prefix

- **Why considered**: Would let the model help with intent.
- **Why rejected**: It spends a round and a context slot to answer a spelling question the
  vocabulary can answer locally, and it puts a fabricated instruction into the dialogue.

### Alternative C: Restore the draft unconditionally on any command error

- **Why considered**: Simplest recovery rule.
- **Why rejected**: A command that *did* run and failed (a `/trust` with a bad subcommand)
  has already changed or attempted state; restoring its text over a fresh draft invites
  accidental re-execution. Recovery is bound to `UnknownCommand` specifically, i.e. to the
  case where nothing happened.

### Alternative D: Convert offsets only inside the server engine

- **Why considered**: One less frontend concern.
- **Why rejected**: The backend cannot distinguish "byte offset" from "scalar index" on the
  wire — the contract is typed as a scalar index, and `replace_start`/`replace_end` in the
  response are scalars the *client* must translate. The conversion belongs where the local
  representation meets the contract.

### Alternative E: Trim the leading space instead of consuming the escape marker

- **Why considered**: Would have made the escape hatch "work" with no router change.
- **Why rejected**: Trimming every prompt is a semantic change to *all* prose — it eats
  intentional indentation. The marker is a syntax element, so the classifier that reads it
  consumes it: one leading space selects prose, two or more are formatting and stay verbatim.

## Links

- [ADR-0041](0041-thread-command-purity-viewport-workspace-separation-and-tab-management-topology.md) — thread command purity (`[INV-CMD-01]`), which this ADR completes
- [ADR-0058](0058-granular-causal-session-forking-sovereign-tab-projection-and-aside-retirement.md) — the retired `/btw` steering this dispatch table continues to honour
- [ADR-0117](0117-transcript-admission-purity-dialogue-only-thread-documents.md) — the transcript is dialogue-only; a rejected command is an operation row, never a user turn
- [ADR-0118](0118-dialog-conversation-sovereignty-ephemeral-scratch-and-explicit-promotion.md) — explicit promotion is required for content to cross into a thread
