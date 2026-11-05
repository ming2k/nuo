---
id: ADR-0140
title: "Command Effect Classes: A Reading Is Not an Act, So Only State Mutations Earn a Durable Ledger Record"
status: accepted
date: 2026-12-18
scope: protocol/nuo-protocol, server/nuo-server, persistence/nuo-persistence, presentation/nuo-view, host/nuo, tui/event-loop
superseded_by: null
negative_knowledge: true
---

# 0140. Command Effect Classes: A Reading Is Not an Act, So Only State Mutations Earn a Durable Ledger Record

- Status: Accepted
- Date: 2026-12-18
- Deciders: Nuo Architecture Working Group
- Consulted: Server Core, Protocol, Persistence, Interface Architecture
- Amends: [ADR-0050](0050-authentic-thread-title-lifecycle-clean-scene-head-and-semantic-tab-projection.md) — the boundary is not "invocation durable / reply ephemeral" but "**act** durable / **reading** ephemeral"; a reading's invocation is not an act either. [ADR-0117](0117-transcript-admission-purity-dialogue-only-thread-documents.md) — the ledger-derived operation row keeps its seam join but its population narrows to acts, which is what finally makes `[INV-TRANSCRIPT-04]` true by construction rather than by agreement of two producers. [ADR-0119](0119-slash-command-admission-thread-scoped-naming-server-authority-and-recoverable-rejection.md) — a rejected command keeps its recoverable draft but stops leaving a durable scar.

---

## Context and Problem Statement

The durable command ledger (`session.commands`) records **every** slash-command invocation together with
its typed reply, and the client joins the ledger into the rendered thread by timestamp seam
([ADR-0117](0117-transcript-admission-purity-dialogue-only-thread-documents.md) §3). That rule was
written for acts — "it is something a human did" — but nothing in the codebase distinguishes an act from
a **reading**.

A reading is a command that reports the state of the world at the moment it is asked: `/help`,
`/stats`, `/usage`, `/quotas`, `/trace`, `/search`, `/diff`, `/jobs`, `/jobs logs`, `/skills list`,
`/skills status`, `/trust status`, `/role` (bare), `/permissions` (bare), `/tree`, `/threads` (bare),
`/export`. Re-running one produces a *different* answer on a different day by design. There are 40+
hand-written `record_*` call sites across `crates/nuo-server/src/handlers_slash/{commands,session_ops}.rs`
and `handlers_provider.rs`, and not one of them asks whether what it is recording is an act.

Three defects follow from the missing distinction. Each is independently sufficient to require a fix.

### 1. Resume resurrects stale state as if it were current

A reading's durable row is re-projected on resume and rendered *at the seam where it happened* — which is,
by construction, the seam where it was **true**. A resumed thread therefore shows
`/usage · 1.2M tokens across 14 threads` from last Tuesday, `/jobs · 3 Running` for processes that died
with that server, and `/skills status · 12 total (9 enabled)` for a registry that has since re-scanned.
The transcript is the one surface whose contract is "everything in it was said, in this order"
(`[INV-TRANSCRIPT-01]`); a superseded reading violates the stronger and more useful contract of a
transcript — *everything in it was true when it was said*. The reader cannot tell a live snapshot from a
stale one, because the row carries no freshness signal and none is possible.

### 2. A reading is disseminated into every consumer of dialogue

The ledger is shipped in the full-attach snapshot (`SessionSnapshot::commands`,
`crates/nuo-protocol/src/events.rs`), in `AgentResponse::ConversationReplaced`, and rendered into
`/export` as a blockquote block (`crates/nuo-server/src/export.rs`). `/search` feeds every ledger record
with a body longer than 15 characters into its candidate pool
(`crates/nuo-server/src/search_lexical.rs`). So a `/help` reply — a static list of commands — is
transported over the wire on every attach, copied into every export of the thread, and ranked as
*history* by the search backend. The cost is paid forever for a fact that was never history.

### 3. This is the same defect class ADR-0116 fixed for notices, left in the command path

[ADR-0116](0116-content-residency-and-surface-layer-topology.md) removed the transcript as the default
destination for content that is not dialogue, and
[ADR-0121](0121-notice-log-residency-retained-disclosure-record-and-bubble-as-pointer.md) gave receipts a
real home ([`SurfaceLog`](../../crates/nuo-view/src/model/log.rs)). The command path never got the same
treatment: a reading still defaults onto the time axis exactly as a diagnostic notice did before
ADR-0116. Meanwhile the client has already grown the correct destination for most of them — a
surface-opening command is intercepted before dispatch (`crates/nuo/src/shell/session.rs` maps
`/models`, `/tools`, `/mcp`, `/skills`, `/usage`, `/stats`, `/trace`, `/quotas`, `/threads`, `/help`,
`/exit`, … to `InputAction::Open*`) and therefore never becomes a ledger row at all. The rows that remain
are the residue of the pre-surface era: the readings the client could not intercept.

### 4. The write path is asymmetric, which hides the defect

`record_ack` (`crates/nuo-server/src/handlers_slash/record.rs`) takes no `resp_tx`: it persists an
`Ack` record and emits **nothing** on the wire. `/stats`, `/trace`, `/usage`, and `/quotas` reach it, so
for any client that is not the TUI, the reply to those commands does not exist. The ledger is being used
as a message bus — the one thing a durability layer must never become — and its failure mode is silent
in exactly the client class that cannot intercept the command locally.

## Decision Drivers

- Driver 1: **A transcript is a claim about what was true when it was said.** A durable row must therefore
  be an act, whose truth does not decay.
- Driver 2: **Mechanism over convention.** The classification must be a property of the one vocabulary
  table that already generates completion, `/help`, the catalog, and dispatch
  ([ADR-0119](0119-slash-command-admission-thread-scoped-naming-server-authority-and-recoverable-rejection.md)
  §2b), not a judgement re-made at 40 call sites.
- Driver 3: **The reply must always reach the user.** Dropping a reading from the ledger must never drop
  it from the wire; the asymmetry in §4 is a defect to fix in the same change.
- Driver 4: **`[INV-TRANSCRIPT-04]` (live equals resume) must become true by construction.** A row that no
  longer exists durably must not exist live either, and vice versa.
- Driver 5: **No new UI vocabulary.** Receipt residency already exists (`SurfaceLog`, the notice-center
  dialog, the typed client catalog of [ADR-0138](0138-notice-catalog-typed-registry-of-every-client-disclosure.md));
  the command path joins it rather than inventing a parallel tier.

## Architectural Decisions

### 1. Every command declares its effect in the single vocabulary table

`crates/nuo-server/src/startup.rs` gains a closed two-value classification, declared per command **and**
per declared subcommand, in the same macro that generates `BuiltinCmd`, `BuiltinCmd::ALL`,
`BuiltinCmd::SPECS`, and `BuiltinCmd::from_slash`:

```rust
// crates/nuo-protocol/src/command.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandEffect {
    /// Changes durable state the thread must remember: posture, role, provider
    /// selection, trust grants, compaction, thread topology, workspace files,
    /// background jobs. An act: its truth does not decay.
    Mutation,
    /// Reports the state of the world at the moment of asking. Re-running it
    /// produces a different answer by design. A reading is not an act: it has
    /// no durable record and no thread row.
    Reading,
}
```

```rust
define_builtin_commands! {
    Jobs = "/jobs" : {
        summary: "Inspect and manage background processes and sub-subagents",
        usage: ["/jobs", "/jobs kill <job_id>", "/jobs logs <job_id>"],
        examples: [ /* … */ ],
        intent_keywords: [ /* … */ ],
        category: Automation,
        // Two declarations, not one. `bare` is the argument-less invocation;
        // `verb` is the fallback for an argument the vocabulary cannot
        // enumerate. They differ exactly where it matters: `/role` with no
        // argument lists roles (a reading), `/role my-role` switches identity
        // (an act) — and user role ids are dynamic, so no verb table could
        // ever enumerate them.
        effect: { bare: Reading, verb: Mutation },
        subcommands: [
            ("kill", "Terminate an active background job", Mutation),
            ("logs", "Show recent stdout/stderr output of a background job", Reading),
        ],
    },
    Search = "/search" : {
        summary: "Semantic search over session history",
        usage: ["/search <query>"],
        examples: [("/search parser bug", "Find earlier discussion")],
        intent_keywords: ["search", "find", "history", "recall"],
        category: Session,
        effect: { bare: Reading, verb: Reading },
    },
}
```

Three properties follow from putting the declaration here, and each is a compile error rather than a
review note:

1. **Omission is unrepresentable.** A new command without an `effect:` block does not build; a declared
   subcommand without its third tuple element does not build.
2. **The class is co-located with the thing it classifies**, so a reviewer reading `/jobs` sees the verb
   table's effects inline. A separate side table would drift silently.
3. **Both fields are mandatory.** There is deliberately no `verb` default, for the same reason
   `RoundEvent::class` has no wildcard arm: a default is precisely how `/role <dynamic-role-id>` would be
   silently misfiled as a reading, and a lost mutation is unrecoverable while a spurious one is merely
   noisy.

Resolution is a single function, the analogue of `RoundEvent::class`:

```rust
impl BuiltinCmd {
    /// This command's declared effect. `sub` is the first token after the
    /// command word: `None` is the bare invocation, a declared verb uses its own
    /// declaration, and an undeclared verb takes the command's `verb` effect.
    pub fn effect(self, sub: Option<&str>) -> CommandEffect;
}

/// The single resolver. Aliases are folded by `from_slash` (ADR-0119 §2b: the
/// vocabulary has exactly one owner). A name outside the built-in vocabulary —
/// an application-registered handler, a project command prompt, the retired
/// `/btw` steering — takes `Mutation`, i.e. the pre-existing over-recording
/// behaviour, because extension code may have mutated anything.
pub fn effect_for(typed: &str, parts: &[&str]) -> CommandEffect;
```

### 2. The class rides the wire at two points, for two different questions

- `nuo_protocol::CommandSpec` gains `effect: CommandEffect`, so a frontend can decide **before dispatch**
  whether to stage an optimistic row. This is the same projection of the single vocabulary that
  `CommandCatalog` already is; it exists so that a reading never paints a row that must then be withdrawn.
- `RoundEvent::CommandResult` gains `effect: CommandEffect`, so a frontend can decide **after the fact**,
  authoritatively, with no second message and no catalog at all. The event is the server's admission
  verdict, mirroring ADR-0119 §2 — admission belongs to the dispatcher, never to the frontend.

The two are not redundant: the catalog is an optimisation for the optimistic path, the event is authority.
A frontend that never fetched a catalog still files the reply correctly.

### 3. The recorder becomes a gate: emit always, persist only acts

Every `record_*` helper in `crates/nuo-server/src/handlers_slash/record.rs` funnels through one private
function. The decision is made there, once:

```rust
fn is_durable(effect: CommandEffect, result: Option<&CommandResult>) -> bool {
    // A rejection is not an act: nothing ran and nothing reached the model
    // (ADR-0119 §3). This holds regardless of the declared effect, so a typo of
    // an effect command leaves no scar either.
    if matches!(result, Some(CommandResult::UnknownCommand { .. })) {
        return false;
    }
    effect == CommandEffect::Mutation
}
```

The funnel's shape is `emit always, persist iff durable`:

```rust
async fn emit_and_maybe_record(/* session, resp_tx, name, args, result, duration_ms */) {
    let effect = effect_for(name, &parts(name, args));
    if is_durable(effect, result.as_ref()) {
        session.mutate_commands(|c| c.push(record)).await;
    }
    if let Some(result) = result {
        // The reply reaches the client whether or not it was durable. This is
        // also the fix for the asymmetry in §4: `record_ack` currently emits
        // nothing at all.
        resp_tx.send(round_response(&id, RoundEvent::CommandResult { name, args, result, effect }));
    }
}
```

`is_durable` has a third condition, and it is the one the ledger learned from the surface commands:

```rust
fn is_durable(performed: Performed, effect: CommandEffect, result: Option<&CommandResult>) -> bool {
    performed == Performed::Here          // an act happened *in this process*
        && effect.is_durable()            // the command is an act
        && !matches!(result, Some(CommandResult::UnknownCommand { .. }))
}
```

`Performed::Surface` is the four commands whose surface the client owns (`/models`, `/connections`,
`/tools`, `/mcp`). They reach the dispatcher only from a client that cannot host the modal, where the
server performs nothing; recording a state change this process never made would be a lie in the audit.
Those arms now answer with a guidance reply instead of being silent.

Two consequences are deliberate:

- **A wrong prediction is reversible.** The class staged before dispatch is read from the catalog, which
  arrives *asynchronously* and is empty (`CommandCatalog::default()`) before it lands — so a reading can be
  staged by mistake. The event's class is therefore paired with a withdrawal: a `Reading` reply removes the
  staged row instead of leaving it to be marked `Cancelled`. Without that, a `/usage` dispatched before the
  catalog arrived left a permanent ghost row saying the command never answered, for a command that answered
  in full. This is why the catalog is documented as an optimisation and never as authority.
- **Zero call-site churn on the decision.** All 40+ `record_*` sites keep their signature; the class is
  resolved from `(name, args)` by the same vocabulary the dispatcher used, so a handler cannot opt in or
  out of being recorded. This is the difference between a mechanism and 40 chances to drift.
- **One writer, mechanically.** `handlers_provider.rs` used to write the ledger directly for a provider
  switch. It now routes through `record_act_only` — same gate, no duplicate reply, because `/models`'
  acknowledgment rides its own `CommandAck` notice. A source-level test
  (`content_purity.rs::command_ledger_has_exactly_one_writer`) fails if a third writer appears.
- **`record_ack` gains `resp_tx`.** Seven sites pick up the parameter, and the four commands that were
  persist-only become visible to non-TUI clients. A durability layer stops being used as a message bus.

### 4. A dispatched mutation always leaves a trace (completeness net)

The funnel decides *whether* a produced reply is durable; it cannot force a handler to produce one. A
future handler that performs a mutation and records nothing would leave the ledger silently blind — the
failure mode `[INV-PROV-07]` names for orphan contracts, applied to audit.

`dispatch` therefore brackets the handler:

```rust
// crates/nuo-server/src/handlers_slash/dispatch.rs
let mark = LedgerMark { id: session.id().await, len: session.commands().await.len() };
let outcome = /* … existing match … */;
if effect == CommandEffect::Mutation {
    let after = LedgerMark { id: session.id().await, len: session.commands().await.len() };
    // A session reset (`/new`, `/role … --new`, a fork) mints a new ledger; the
    // handler already recorded into it, so an identity change counts as recorded.
    if after.id == mark.id && after.len == mark.len {
        session.mutate_commands(|c| c.push(CommandRecord::new(name, args))).await;
    }
}
```

The net is a backstop, not a substitute: it writes an invocation-only row (`result: None`), which renders
as the bare invocation. Two ledger reads per mutation command is the entire cost.

### 5. A reading's reply is a receipt, and never a thread row

Because `[INV-TRANSCRIPT-04]` is not negotiable, removing the durable row forces the live row out too.
The client therefore files a reading's reply exactly where ADR-0116/ADR-0121 put every other receipt:

- `crates/nuo-view/src/model/layer.rs`'s job is unchanged — `ContentLayer` still has no transcript arm —
  and a reading's reply is appended to the client's receipt log (`SurfaceLog(SurfaceId::NOTICES)`, ADR-0121)
  through the typed catalog of [ADR-0138](0138-notice-catalog-typed-registry-of-every-client-disclosure.md),
  as a new closed variant `ClientNotice::CommandReceipt { invocation, body }` under the existing
  `ClientTopic::Command` topic. Severity derives from the result (`Error`/`UnknownCommand` → `Failed`,
  everything else → `Info`); the full `to_text()` body rides in the line's `detail`, which the notice
  center already renders as an expandable, scrollable, copyable body. No new surface, no new dialog, no new
  pager.
- `handle_send_slash` stages no `pending_command` row when the catalog declares `Reading`. When the name is
  **not** in the catalog (an application-registered handler) it also stages none, and the existing
  `SettleCommandResult` fallback materialises the row when the result arrives — so the "optimistic row"
  and "authoritative event" paths cannot disagree about a row that was never painted.
- A `Mutation` result keeps today's behaviour exactly: settle the pending row in place, or materialise the
  fallback row when there was none.

The seam join of ADR-0117 §3 survives untouched; its population shrinks to acts. `/unattended on` still
appears in the thread at the moment it happened, because a posture change *is* part of the thread's story;
`/usage` does not, because a token count is not.

### 6. Historical readings are retired with the schema, not filtered at read time

Existing sessions carry reading rows. Rendering them would restore exactly the live≠resume asymmetry this
ADR removes: the live thread shows nothing, the resumed thread shows a stale snapshot.

A read-time filter was the first design and it is wrong. The class was never recorded, so any filter must
re-derive it from `(name, args)` — which means `nuo-persistence` has to import the server's command
vocabulary, or take it as an injected oracle. Both make the durability layer depend on a *later* layer's
policy in order to keep data that has no value: the acts a thread took are reconstructible from its own
state (posture, role, provider pin, trust grants), while a stale measurement is useful to nobody.

So the ledger is retired the way ADR-0059 retires content — by the schema. `CURRENT_SCHEMA_VERSION` moves
to 17, and `migrate_session_data` clears the ledger when it loads a row stamped below it. Rows written at
v17 are acts-only by construction and are never touched again. Nothing is rewritten on disk, and a
downgrade to a pre-0140 binary reads the same file and re-renders the stale rows — bounded by the protocol
floor (§2), which already refuses to attach a mixed pair.

### 7. The vocabulary, the schema, and the type names stay

`session.commands` and `CommandRecord` are not renamed. The narrowed meaning is documented on both types,
and the field is not split, because a ledger whose every row is an act needs no per-row class: adding
`effect` to `CommandRecord` would be a constant field on every row — the anti-inflation the invariant
policy forbids. The one rename that *is* required is a documentation one: every doc comment that currently
asserts "records each invocation" must say "records each act".

## Normative Invariants Established

- `[INV-CMD-05]` **Declared effect.** Every command declares `effect: { bare, verb }` and every declared
  subcommand declares its own effect in the single vocabulary table. Enforced by: the macro requiring both
  fields and the third tuple element (compile error on omission), and `effect_for` being the only resolver
  that dispatch and the recorder both call.
- `[INV-CMD-06]` **A reading is not a record.** A `Reading` command never writes the durable ledger and
  never holds a thread row; its reply is an ephemeral receipt whose only home is a non-transcript layer.
  Enforced by: the single recorder funnel's `is_durable` gate plus the funnel test that reads, jobs, diff,
  search, skills, role-list, trust-status, and export leave `session.commands()` empty while still
  producing a `RoundEvent::CommandResult` with `effect == Reading`.
- `[INV-CMD-07]` **An act leaves a trace.** A dispatched `Mutation` command always produces at least one
  ledger row, even when its handler records nothing. Enforced by: the dispatch completeness net and the
  test asserting an `Effect`-classified handler that emits no record still leaves exactly one
  invocation-only row.
- `[INV-CMD-08]` **One reply, two homes, no overlap.** For any command result, exactly one of "a ledger row"
  and "a receipt line" is produced, decided by the declared effect alone. Enforced by: the client's single
  filing decision on `RoundEvent::CommandResult.effect`, tested in both directions (a reading files a
  receipt and leaves the thread untouched; a mutation settles a row and files no receipt).

## Consequences

- **Positive**: a resumed thread stops asserting falsehoods. Every row in it was an act, so every row is
  still true on the day it is read.
- **Positive**: `/export` stops emitting `> /help` blocks and `/search` stops ranking a static command list
  as history; the attach snapshot stops carrying kilobytes of dead readings. The savings scale with session
  age, which is exactly when resume matters.
- **Positive**: the classification becomes a compile-time obligation rather than a judgement. A new command
  cannot be added in the wrong class silently, and the reviewer sees the class next to the summary.
- **Positive**: `[INV-TRANSCRIPT-04]` is now true by construction for readings (neither path produces a
  row) instead of by the agreement of two producers.
- **Positive**: the `record_ack` asymmetry is fixed — four commands that were invisible to every non-TUI
  client now reply on the wire.
- **Negative & trade-offs**: a reading is gone once the client process exits. A user who wants last week's
  `/jobs` table must re-run it, and after a resume the answer may differ. This is the decision, not a
  regression: a stale answer *with no freshness signal* is worse than no answer.
- **Negative & trade-offs**: the receipt log is client-global (ADR-0121), so a `/diff` receipt from one
  thread is visible from another. Mitigated by the receipt's label being the full invocation, so provenance
  travels with the line; thread-scoping the log is out of scope here and would be its own decision.
- **Negative & trade-offs**: a mutation's reply can still be stale in the same way (an `/undo` outcome is a
  moment). The line is drawn at "did it change durable state", because the change is what the thread must
  remember; the reply is attached to it as evidence, which is honest at any age.
- **Negative & trade-offs**: a client too old to read `effect` ignores it and paints rows for readings that
  were never persisted — precisely the asymmetry being removed. Mitigation: the protocol floor moves with
  the field (see the Implementation Plan), so a mixed pair refuses to attach rather than lying.

## Rejected Alternatives & Negative Knowledge

### Alternative A: Keep recording everything, hide readings in the UI

- **Why considered**: Smallest change; no protocol, no migration, no classification.
- **Why rejected**: It converts a residency question into a display question, which is exactly the failure
  ADR-0117 documents (Alternative B, "append-then-filter"): every consumer re-implements the filter, and the
  recorded row still flows into export, search, attach replay, and the resumed transcript. It also cannot
  satisfy `[INV-TRANSCRIPT-04]` honestly — the row exists and is merely not painted in one of the two
  paths.

### Alternative B: Infer the class from the `CommandResult` variant

- **Why considered**: The variants look correlated with intent (`Search`, `PermissionList`,
  `WorkspaceChanges` are readings; `UndoOutcome`, `Compacted` are acts).
- **Why rejected**: `Ack` is produced by both (`/unattended on` and `/stats`), and `Text` is produced by
  both (`/init` and `/export`). The correlation is a coincidence of today's handlers; a new variant would
  silently pick a side, which is the drift this ADR exists to remove.

### Alternative C: Mark records ephemeral instead of not writing them

- **Why considered**: Keeps a complete audit trail in one place; "ephemeral" is a familiar escape hatch.
- **Why rejected**: An ephemeral record is still a record: it still ships in the attach snapshot, still
  reaches export and search unless every consumer filters it, and still has to be pruned by an age or type
  rule that no consumer agrees on. The ledger's value is that every row in it is an act; a mixed ledger has
  no value at all.

### Alternative D: Declare the effect in the client

- **Why considered**: The TUI already keeps an intercept table (`crates/nuo/src/shell/session.rs`) and
  could own the class in the same place.
- **Why rejected**: ADR-0119 §2b established that the vocabulary has exactly one owner, the server — and it
  had already drifted three ways when two lists existed. A client-owned class would be a fourth list, and a
  non-TUI client (the Web app, a headless harness) would disagree by construction.

### Alternative E: Delete the ledger and derive everything from dialogue

- **Why considered**: If only acts matter and most acts emit state-change events (`UnattendedChanged`,
  `ConfinementChanged`, `ConversationCleared`, `ThreadForked`), why keep a ledger at all?
- **Why rejected**: The events describe the *current* state, not the *history* of decisions, and several
  acts have no state event at all — trust grants are workspace-global, `/init` writes files outside the
  session, `/undo` restores file content, `/jobs kill` ends a process. Losing those loses the answer to
  "why is this workspace trusted / why is this file back to its old content", which is the audit the ledger
  exists for. The ledger is narrowed, not removed.

### Alternative F: Cap the ledger to the newest N records

- **Why considered**: Bounds growth without any classification work.
- **Why rejected**: Evicts acts (the `/unattended` posture reconstruction in
  `crates/nuo-server/src/handlers_slash/session_ops.rs` scans backwards through the ledger for the newest
  posture ack) while keeping readings, i.e. exactly backwards. A cap also makes audit depend on how chatty
  the user was, which is not a property worth coupling to.

## Implementation

Every step below is landed in the same change as this record; the invariant each one establishes names
the test that now fails if it is violated.

| # | Step | Landed as | Invariant / test |
| :--- | :--- | :--- | :--- |
| 0 | Delete the dead SQLite command path | `PersistenceHandle::record_command`, `PersistenceCommand::RecordCommand`, and `db::usage_projection::record_command` removed — zero callers, and the row they wrote violated the table's own `REFERENCES sessions(id)` with `foreign_key = ON` | `[INV-PROV-07]` (wired or deleted) |
| 1 | `CommandEffect` + declaration + resolver | `nuo_tool::completion::CommandEffect` (re-exported as `nuo_protocol::CommandEffect`); `effect: { bare, verb }` and per-verb effects in `define_builtin_commands!`; `startup::effect_for` | `every_builtin_declares_an_effect`, `effect_table_is_pinned` |
| 2 | Wire the class | `CommandSpec.effect` / `.verb_effect` / `CommandSubcommandSpec.effect`, `CommandCatalog::effect_for`, `RoundEvent::CommandResult.effect`; `PROTOCOL_VERSION` and `MIN_PROTOCOL_VERSION` → 20; census re-blessed | `[INV-WIRE-03]` |
| 3 | The recorder gate | `Performed`, `is_durable`, one `emit` funnel; `record_ack` + `resp_tx`; `record_surface_only` for the four no-op arms | `[INV-CMD-06]`: `readings_leave_the_ledger_empty`, `ack_emits_and_records_only_for_an_act`, `acts_still_record`, `a_rejection_is_never_durable`, `failure_follows_the_class`, `invocation_only_follows_the_class` |
| 4 | The completeness net | `dispatch` brackets every mutation arm by a ledger mark (`LedgerMark`); the four surface arms sit outside it | `[INV-CMD-07]`: `an_unrecorded_act_gains_an_invocation_only_row`, `a_recorded_act_is_not_double_recorded`, `a_session_reset_counts_as_recorded` |
| 5 | Client filing | catalog-driven staging in `handle_send_slash`; `effect`-driven filing in the `RoundEvent::CommandResult` arm; `ClientNotice::CommandReceipt` | `[INV-CMD-08]`: `reading_appends_a_receipt_and_no_thread_row`, `mutation_settles_a_thread_row_and_files_no_receipt`, `a_failed_reading_is_a_fault_receipt` |
| 6 | Retirement, not filtering | `CURRENT_SCHEMA_VERSION` → 17; `migrate_session_data` clears a pre-split ledger on load | `pre_split_command_ledger_is_retired_on_load`, `current_schema_keeps_acts_across_the_migration_seam` |
| 7 | Retire the claims | every doc comment that asserted "each invocation"; `docs/reference/commands.md` gained its *Recorded* column and per-verb classes; `docs/explanation/command-records-and-receipts.md` is the public rationale | documentation update |
| 8b | Make a wrong prediction reversible | `TranscriptEdit::WithdrawStagedCommand` — a `Reading` reply removes the row staged on a stale/empty catalog instead of leaving it to `CancelPendingCommands` | `[INV-CMD-08]`: `a_reading_withdraws_its_own_staged_row`, `withdrawal_leaves_other_pending_rows_alone`, `an_unknown_name_is_staged_and_a_known_reading_is_not` |
| 8 | Close the second writer | `handlers_provider.rs`'s provider-switch ack routes through `record_act_only` (same gate, no duplicate reply — its acknowledgment is a `CommandAck` notice) | `content_purity.rs::command_ledger_has_exactly_one_writer`, `command_effects_are_declared_not_inferred` |

Verification:

```bash
cargo check --workspace --all-targets
cargo test -p nuo-protocol -p nuo-tool -p nuo-persistence -p nuo-server -p nuo-view -p nuo
cargo test -p nuo --test architecture_firewall
cargo clippy --workspace --all-targets
```

## Links

- Amends: [ADR-0050](0050-authentic-thread-title-lifecycle-clean-scene-head-and-semantic-tab-projection.md)
  (durable act / ephemeral reading),
  [ADR-0117](0117-transcript-admission-purity-dialogue-only-thread-documents.md) (the join's population),
  [ADR-0119](0119-slash-command-admission-thread-scoped-naming-server-authority-and-recoverable-rejection.md)
  (rejection keeps its recovery, loses its scar)
- Applies: [ADR-0116](0116-content-residency-and-surface-layer-topology.md) (one home per content),
  [ADR-0121](0121-notice-log-residency-retained-disclosure-record-and-bubble-as-pointer.md) (receipt
  residency), [ADR-0138](0138-notice-catalog-typed-registry-of-every-client-disclosure.md) (typed receipt
  wording), [ADR-0059](0059-canonical-thread-persistence-schema-and-zero-legacy-clean-break.md) (clean
  break, no migration ceremony)
- Conceptual explanation: [Command Records and Receipts](../explanation/command-records-and-receipts.md)
- Reference: [Built-in Command Reference](../reference/commands.md)
- Code: `crates/nuo-server/src/startup.rs` (`define_builtin_commands!`, `BuiltinCmd::effect`),
  `crates/nuo-server/src/handlers_slash/record.rs` (the funnel),
  `crates/nuo-server/src/handlers_slash/dispatch.rs` (the net),
  `crates/nuo-protocol/src/command.rs` (`CommandEffect`), `crates/nuo/src/shell/session.rs` (catalog-driven
  staging), `crates/nuo/src/shell/event_loop/apply.rs` (filing)
