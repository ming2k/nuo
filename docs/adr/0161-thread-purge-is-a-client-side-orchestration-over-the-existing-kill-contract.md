# ADR-0161: Thread Purge Is a Client-Side Orchestration over the Existing Kill Contract

- **Status:** Superseded by [ADR-0171](0171-thread-removal-is-durable-and-server-owned.md)
- **Date:** 2026-11-08
- **Supersedes:** none
- **Superseded by:** [ADR-0171](0171-thread-removal-is-durable-and-server-owned.md)
- **Related:** [ADR-0043](0043-thread-management-is-a-server-owned-session-store.md), [ADR-0048](0048-thread-administration-is-destructive-and-requires-confirmation.md), [ADR-0038](0038-thread-entity-server-ssot-fanout-and-orthogonal-action-lifecycle.md), [ADR-0011](0011-nuo-tui-presentation-and-nuo-server-container.md)

## Context

`nuo thread rm <id>` terminates a hosted thread by id. Each invocation enqueues a
single `ControlRequest::KillThread { thread_id }` to the discovered server, which
`registry.kill_session` commits and permanently deletes from `nuo.db`. This is
correct for a single thread, but there is no way to remove all historical
threads at once. A user who wants to start over (a clean slate after months of
accumulated sessions, CI image teardown, a scratch project) must either:

- list ids, then pipe them through repeated `nuo thread rm`, risking a mid-batch
  stall the moment the list drifts; or
- drop the whole `nuo.db`, which also destroys configuration, permissions,
  lineage, and any active grants — far more than the user asked for.

The raw `rm <id>` loop and the full-database wipe are the two existing escape
hatches, and both are poor: one is tedious and racy, the other is grossly
over-broad.

### Constraint frame

- **Single-writer invariant (INV-SSOT-01).** Every SQLite write is funneled
  through the owning server process. Nothing outside the server writes the store.
- **Threads are server-owned entities** [ADR-0038]. The server is the SSOT for
  thread lifecycle; the CLI is a remote window onto that lifecycle.
- **Administrative termination is destructive** [ADR-0048]. Destructive
  management already requires explicit, affirmative user consent (an interactive
  confirm, not just a flag).
- **`Detach` never terminates.** A backgrounded session keeps running; only an
  explicit administrative kill ends it. "Delete all threads" must therefore
  cover *running* sessions, not only finished ones — otherwise it is a lie.
- **Protocol surface is precious [ADR-0006].** Every new instruction on the wire
  is permanent; it must be justified by a capability that cannot be composed from
  existing ones.

## Decision

Add `nuo thread rm --all [--force]` as a **client-side orchestration**: the CLI
enumerates the live session snapshot from the discovered server, prompts for
affirmative confirmation (unless `--force`), then issues one existing
`KillThread` per thread id over the already-trusted control channel. No new wire
instruction, no new persistence path, no bypass of the single-writer invariant.

This is deliberately **not** a new `PurgeThreads` server instruction and not a
`control --purge` verb. Rationale:

- **It composes correctly under INV-SSOT-01.** Each constituent `KillThread`
  already travels from the client to the server's single writer and is committed
  there. A client loop of N existing instructions is architecturally identical
  to a new atomic purge instruction, except that it adds zero new protocol
  surface.
- **It keeps the CLI-as-window model intact.** The server remains the sole
  authority over session lifecycle; the CLI merely reuses the snapshot it
  already exposes to the monitor, plus the kill contract, to express "all".
  There is no new state, no batch semantics to reason about on the server, and no
  drift between "what purge believes" and "what `KillThread` would have killed".
- **"Delete all" is a user-intent statement, not a new primitive.** The atomicity
  the word "all" implies is *planning* atomicity (fix the id set up front) rather
  than *transactional* atomicity (the ids can still race with new spawns). For a
  management operation that is the right trade: a snapshot is a reasonable
  definition of "all", and a concurrently-spawned thread surviving a purge is a
  benign, clearly-reportable outcome — not corruption.
- **One protocol instruction per thread keeps per-session hook budgets and
  `SessionEnd` broadcasts intact.** The existing kill path runs each session's
  shutdown hooks with its own budget and fanout. A bulk instruction that tried to
  run K hook budgets inside one RPC would either serialize them into a long
  call or violate the established per-session lifecycle isolation.

### Contract

- `nuo thread rm --all` prompts for an explicit confirmation quoting the number
  of threads to be removed, defaulting to **no**.
- `nuo thread rm --all --force` skips the prompt; it is the scripted/headless
  escape hatch, mirroring the established "flag as explicit affirmative" pattern.
- `--force` without `--all` is rejected: forcing a single targeted kill is an
  oxymoron (a targeted kill is already unambiguous) and would imply the flag is
  about skipping the id, which it is not.
- An id and `--all` are mutually exclusive: supplying both is an arg error, not a
  silent preference.
- The enumeration is a point-in-time snapshot; the count is quoted **before**
  confirmation so the user decides against what actually exists now.
- Live (running) threads are included. The mode removes every thread the server
  holds, not only finished history. Users who want only-finished cleaning get a
  separate, future, filtered verb rather than a silent assumption baked into this
  one.

## Consequences

- One-shot teardown of an accumulated server (scratch projects, CI images,
  clean-slate restarts) no longer requires a full database drop that nukes
  configuration, permissions, and lineage.
- Destructive bulk action stays behind an affirmative confirm unless `--force` is
  explicitly passed, preserving ADR-0048's safety posture.
- The protocol surface and the single-writer invariant are untouched; the change
  is confined to the headless client (`commands/thread.rs`) and its argument
  grammar (`cli.rs`) plus unit/integration coverage.
- A concurrent spawn during the loop survives and is reported in the batch
  summary; this is documented benign behavior, not a correctness gap.
- Should "delete only finished history" later be wanted, it composes cleanly as a
  future filter flag over the same snapshot-and-loop machinery, without a new
  wire verb.

## Validation

- `cargo check --workspace --all-targets` is clean.
- `nuo`, `nuo-server`, and integration suites remain green.
- Unit coverage pins the argument grammar: `--all`, `--force`, their mutual
  exclusions, and the default `rm <id>` path.
- Integration coverage drives `rm --all --force` against a live test server and
  asserts the thread table is emptied and the exit status is success.