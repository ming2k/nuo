# Command Effect Classes — Implementation Blueprint

Design record for [ADR-0140](../adr/0140-command-effect-classes-readings-are-receipts-not-records.md).
This document is the engineering surface: the complete classification of the existing vocabulary, the
exact sites that change, the mechanisms that make each invariant unrepresentable-if-violated, and the test
plan. Read the ADR first for *why*; this file is *where* and *how*.

---

## 1. The distinction in one table

| | **Act** (`CommandEffect::Mutation`) | **Reading** (`CommandEffect::Reading`) |
| :--- | :--- | :--- |
| What it does | changes durable state | reports the state of the world at the instant it is asked |
| Truth over time | does not decay — "unattended was switched on at 14:02" stays true | decays immediately — a token count, a job list, a file diff |
| Durable ledger row | **yes** | **never** |
| Thread row (resume join) | yes, at its timestamp seam | never |
| Live reply | settles the staged row in place | a receipt in the client's receipt log |
| Re-running it | is a second, distinct act | answers the same question again, possibly differently |
| Wire | `RoundEvent::CommandResult { effect: Mutation, .. }` | `RoundEvent::CommandResult { effect: Reading, .. }` |

The last row matters: **both classes still produce a reply on the wire.** Dropping a durable record must
never drop the answer. A reading is ephemeral, not silent.

---

## 2. Classification of the built-in vocabulary

Derived from the handler bodies in `crates/nuo-server/src/handlers_slash/{commands,session_ops}.rs` and
`crates/nuo/src/shell/session.rs` (the client intercept table). 29 commands, 21 declared subcommand verbs.

### 2.1 Commands

| Command | Surface | `bare` | `verb` | Why |
| :--- | :--- | :--- | :--- | :--- |
| `/models` | client modal (`InputAction::OpenModels`) | Mutation | Mutation | provider/model selection is durable session state; `record_provider_ack` is its server-side twin |
| `/connections` | client modal | Mutation | Mutation | credentials/connection topology change |
| `/tools` | client modal | Mutation | Mutation | session `disabled_tools` mask |
| `/mcp` | client modal | Mutation | Mutation | MCP server lifecycle (reconnect/disable) |
| `/compact` | server | Mutation | Mutation | writes a durable checkpoint into the thread |
| `/new` | server | Mutation | Mutation | mints a new thread |
| `/permissions` | server | Reading | Reading | bare lists the always-allowed rules; handler's fallback arm lists too |
| `/unattended` | server | Mutation | Mutation | posture, persisted (`session.set_unattended`) |
| `/confinement` | server | Mutation | Mutation | shared confinement flag |
| `/role` | server | Reading | Mutation | bare lists roles; a role id switches identity, capability, workspace — and ids are dynamic, so no verb table can enumerate them |
| `/search` | server | Reading | Reading | lexical score over the live thread; answer changes as the thread grows |
| `/threads` | client modal + server | Reading | Mutation | bare opens the picker; an id switches threads |
| `/fork` | server | Mutation | Mutation | mints a child thread |
| `/tree` | server | Reading | Reading | DAG snapshot at this instant; `record_ack` today writes a stale node count |
| `/diff` | server | Reading | Reading | workspace journal projection; the whole point is "what changed *so far*" |
| `/undo` | server | Mutation | Mutation | restores file bytes |
| `/stats` | client modal (`OpenThreadStats`) | Reading | Reading | token accounting snapshot |
| `/trace` | client modal (`OpenSessionTrace`) | Reading | Reading | latency waterfall snapshot |
| `/usage` | client modal (`OpenUsage`) | Reading | Reading | cross-thread usage snapshot |
| `/quotas` | client modal (`OpenQuotas`) | Reading | Reading | provider quota snapshot |
| `/jobs` | server | Reading | Reading | bare lists jobs; `kill` is declared below |
| `/skills` | client modal + server | Reading | Reading | list/status; the retired `reload` replies with guidance |
| `/init` | server | Mutation | Mutation | writes a `.nuo/` tree |
| `/trust` | server | Mutation | Mutation | grants/refusals are workspace-global state; `status` is declared below |
| `/untrust` | server | Mutation | Mutation | revokes every grant |
| `/export` | server | Reading | Reading | copies the thread to the clipboard; no durable session state changes |
| `/retry` | server | Mutation | Mutation | resumes a parked round (round state changes) |
| `/help` | client modal + server | Reading | Reading | static list |
| `/exit` | client | Reading | Reading | ends the process; today `record_invocation` writes a resultless row for it |

### 2.2 Declared subcommand verbs

| Command | Verb | Effect | Handler site |
| :--- | :--- | :--- | :--- |
| `/permissions` | `clear` | Mutation | `commands.rs:40` |
| `/unattended` | `on`, `off` | Mutation | `commands.rs:56` |
| `/confinement` | `on`, `off` | Mutation | `commands.rs:119` |
| `/role` | `developer`, `ops`, `philosophist` (+ user ids) | Mutation | `commands.rs:172` |
| `/jobs` | `kill` | Mutation | `commands.rs:991` |
| `/jobs` | `logs` | Reading | `commands.rs:1019` |
| `/skills` | `list`, `status` | Reading | `commands.rs:1568` |
| `/trust` | `all`, `instructions`, `ex-workspace`, `mcp`, `skills`, `hooks`, `revoke`, `deny` | Mutation | `commands.rs:1169` |
| `/trust` | `status` | Reading | `commands.rs:1291` |

### 2.3 The governing rule for `verb`

**The `verb` default is the effect of the handler's fallback arm, not of its most consequential verb.**

This is not a style preference; it is what makes the declaration match reality. Three handlers dispatch on
a `match` whose `_` arm does something benign:

```rust
// crates/nuo-server/src/handlers_slash/commands.rs — jobs()
let sub = parts.get(1).copied().unwrap_or("list");
match sub {
    "kill" => { /* Mutation */ }
    "logs" => { /* Reading */ }
    _ => { /* lists jobs — so `/jobs whatever` is a READING */ }
}
```

`/jobs whatever` lists. Declaring `/jobs`'s `verb` as `Mutation` (because `kill` mutates) would file a
durable act-row for an unrecognised argument that merely listed things. The declaration therefore states
`verb: Reading` and enumerates `kill` as `Mutation`, which is exactly what an unenumerated argument does.
The same shape holds for `/permissions` (`else` lists) and `/skills` (`other` → error, no act).

`/trust` is the one case where the fallback is an error (`trust_route` returns `Err` for an unknown verb);
its `verb` stays `Mutation` and the resulting `Error` row persists. See §5.3 for why that is accepted.

### 2.4 Outside the vocabulary

| Source | Effect | Why |
| :--- | :--- | :--- |
| Application-registered handler (`SlashCommandRegistry`) | Mutation | extension code may have mutated anything; over-recording is the safe direction |
| Project command prompt (`.nuo/commands/*.md`) | Mutation | it dispatches a model turn |
| Retired-name steering (`/btw`, `/aside`, `/side`) | Mutation | it forks a thread |
| `!shell` passthrough | not a slash command | dispatched as chat by `enter_shell`, never reaches the dispatcher |
| Unknown name | n/a | `UnknownCommand` is never durable (see §5.2) |

---

## 3. Mechanism

### 3.1 Protocol (`crates/nuo-protocol`)

```rust
// src/command.rs
/// What a slash command does, which is what decides whether it is remembered.
///
/// This is the one distinction the DURABLE ledger exists to make. `Mutation` is
/// an act whose truth does not decay; `Reading` is a snapshot that is stale the
/// moment it is produced. Only the former is a record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandEffect {
    Mutation,
    Reading,
}
```

Two wire additions, each answering a different question:

- `completion::CommandSpec.effect` (the bare invocation) and `CommandSpec.verb_effect` (an undeclared
  argument), plus `CommandSubcommandSpec.effect` for a declared verb — lets a frontend decide **before
  dispatch** whether to stage an optimistic row (`CommandCatalog` is already the projection of the
  vocabulary). `verb_effect` is mandatory and separate from `effect` because the fallback is the handler's
  `_` arm, which is not always the bare arm: `/jobs` lists with no argument *and* with an unrecognised one,
  while `/role` lists bare and switches on any argument.
- `events::RoundEvent::CommandResult { name, args, result, effect }` — lets a frontend decide **after the
  fact**, authoritatively, with no catalog at all.

The `CommandCatalog` gains one helper, mirroring the server resolver:

```rust
impl CommandCatalog {
    /// The declared effect of `name` with first token `sub`. A name outside the
    /// catalog is `Mutation` (the over-recording default, §2.4).
    pub fn effect_for(&self, name: &str, sub: Option<&str>) -> CommandEffect;
}
```

Both are additive with `#[serde(default)]`-equivalent semantics, but they still move the protocol number:
a pre-0140 server omits `effect`, a pre-0140 client ignores it, and the two would disagree about which
replies are rows — the exact asymmetry this ADR removes. `PROTOCOL_VERSION` and `MIN_PROTOCOL_VERSION`
move to **20** and `nuo dev census --bless` renews `crates/nuo/src/dev/wire-census.txt`
(`[INV-WIRE-03]`). The census diff is the reviewable record of what changed on the wire.

### 3.2 Vocabulary (`crates/nuo-server/src/startup.rs`)

The `define_builtin_commands!` macro takes two new pieces of declaration and generates the resolution
alongside the tables it already generates:

```rust
define_builtin_commands! {
    Jobs = "/jobs" : {
        summary: "Inspect and manage background processes and sub-subagents",
        usage: ["/jobs", "/jobs kill <job_id>", "/jobs logs <job_id>"],
        examples: [ /* … unchanged … */ ],
        intent_keywords: [ /* … unchanged … */ ],
        category: Automation,
        // `bare` = argument-less invocation; `verb` = fallback for an argument
        // the verb table below does not enumerate. Both mandatory: a default is
        // how `/role <dynamic-id>` would be silently misfiled as a reading, and
        // a lost mutation is unrecoverable while a spurious one is only noisy.
        effect: { bare: Reading, verb: Reading },
        subcommands: [
            ("kill", "Terminate an active background job", Mutation),
            ("logs", "Show recent stdout/stderr output of a background job", Reading),
        ],
    },
}
```

Generated:

```rust
impl BuiltinCmd {
    pub const fn effect(self) -> CommandEffect;              // the `bare` field
    pub fn effect_for(self, sub: Option<&str>) -> CommandEffect;  // verb table, then `verb`
}
```

and the single resolver next to `BuiltinCmd::from_slash`, so aliases are folded by the same owner
(ADR-0119 §2b):

```rust
/// The effect of `parts[0]` with first token `parts[1]`.
///
/// Aliases resolve through `from_slash` — the vocabulary has exactly one owner,
/// and this function is the *only* place a class is decided. A name outside the
/// built-in set takes `Mutation` (see the blueprint §2.4).
pub fn effect_for(parts: &[&str]) -> CommandEffect;
```

`command_catalog()` (`startup.rs:618`) copies both fields into the protocol `CommandSpec` /
`CommandSubcommandSpec`.

### 3.3 The recorder gate (`crates/nuo-server/src/handlers_slash/record.rs`)

Today four helpers all call `session.mutate_commands(|c| c.push(record))` and three of them emit nothing.
They collapse into one funnel with three inputs — the declaration, the reply, and whether *this process*
performed the act:

```rust
/// Whether this reply is remembered as a record.
///
/// Three conditions, in order, each with its own reason:
///  1. an act actually happened **here** — a surface-only command (`/models` on a
///     client that owns the modal) performs nothing on this side;
///  2. the command is an act (declared `Mutation`);
///  3. the reply is not a rejection — nothing ran, so nothing is owed to the
///     ledger regardless of the declared class.
fn is_durable(performed: Performed, effect: CommandEffect, result: Option<&CommandResult>) -> bool {
    performed == Performed::Here
        && effect.is_durable()
        && !matches!(result, Some(CommandResult::UnknownCommand { .. }))
}

/// Emit always; persist iff durable.
async fn emit(
    session: &Arc<SessionStore>,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    performed: Performed,
    name: &str,
    args: &str,
    result: CommandResult,
    duration_ms: Option<u64>,
) { … }
```

with

```rust
/// Where the act happened. `Surface` means the client owns the surface that
/// performs it (`/models`, `/connections`, `/tools`, `/mcp`); the server's arm is
/// a no-op and must not fabricate a record for state it never touched.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Performed { Here, Surface }
```

Public helper surface after the refactor (call sites keep their shape):

| Helper | `performed` | Emits? | Persists? |
| :--- | :--- | :--- | :--- |
| `record_command(...)` | `Here` | yes | iff `is_durable` |
| `record_command_with_duration(...)` | `Here` | yes | iff `is_durable` |
| `record_error(...)` | `Here` | yes | iff `is_durable` (a failed act persists; a failed reading does not) |
| `record_ack(...)` | `Here` | **yes — new** | iff `is_durable` |
| `record_invocation(...)` | `Here` | no (result: none) | iff `is_durable` |
| `record_act_only(...)` | `Here` | **no** — its reply rides another channel | iff `is_durable` |
| `record_surface_only(...)` | `Surface` | yes (guidance receipt) | never |
| `record_unknown_command(...)` | `Here` | yes | never |

`record_act_only` exists for exactly one live case: `/models`. The switch is performed by the picker
surface and acknowledged with a `CommandAck` notice, so the ledger row must be written without a second,
duplicate `CommandResult` reply. It routes through `is_durable` rather than calling `mutate_commands`
directly (which is what `handlers_provider.rs` used to do) because a second writer with its own judgement
is precisely the drift this funnel exists to prevent. `content_purity.rs`'s
`command_ledger_has_exactly_one_writer` now fails if a third one appears.

`record_ack` gaining `resp_tx` is not incidental: today `/stats`, `/trace`, `/usage` and `/quotas` write a
durable ack and emit **nothing**, so a non-TUI client gets silence. Seven call sites
(`commands.rs:42, 618, 866, 877, 894, 905, 996`) pick up the parameter. Note that two of those —
`/permissions clear` (:42) and `/jobs kill` (:996) — are *mutations* that were reaching the ledger through
the ack helper; they keep recording, now via the declared class rather than by accident.

### 3.4 The completeness net (`dispatch.rs`)

The gate cannot force a handler to produce a reply; a future mutation handler that records nothing would
leave the ledger blind. `dispatch` therefore brackets every arm that invokes a handler:

```rust
let mark = (session.id().await, session.commands().await.len());
/* … the existing match … */
if effect == CommandEffect::Mutation {
    let after = (session.id().await, session.commands().await.len());
    // A session reset (`/new`, `/role … --new`, a fork) mints a fresh ledger and
    // the handler already recorded into it, so an identity change counts as
    // recorded. Only a same-thread, same-length ledger is a missing record.
    if after.0 == mark.0 && after.1 == mark.1 {
        session.mutate_commands(|c| c.push(CommandRecord::new(name, args))).await;
    }
}
```

Two ledger reads per mutation command, no per-handler cooperation, and the failure it catches is silent
today. The four surface-only arms are *outside* the bracket — they call no handler, so no act happened
here (see `Performed::Surface`).

### 3.5 Client (`crates/nuo`)

**Staging (before dispatch).** `handle_send_slash`
(`crates/nuo/src/shell/event_loop/actions/commands.rs`) currently always pushes
`TranscriptMessage::pending_command`. It consults the catalog instead:

```rust
let effect = app.command_catalog.effect_for(cmd_name, cmd_args.split_whitespace().next());
if effect == CommandEffect::Mutation {
    app.messages.push(TranscriptMessage::pending_command(cmd_name, cmd_args)…);
}
```

When the name is absent from the catalog (an extension handler) nothing is staged either — the existing
`SettleCommandResult` fallback materialises the row when the reply arrives. So a row that was never painted
cannot be stranded as `Cancelled`.

**Filing (after dispatch).** `crates/nuo/src/shell/mod.rs`'s `RoundEvent::CommandResult` arm branches on
the event's own `effect`, which is authority:

- `Mutation` → unchanged: `TranscriptEdit::SettleCommandResult { invocation, result, fallback }`.
- `Reading` → `AppMutation::NoticeDisclosure` with a new catalog variant, no transcript edit at all.

```rust
// crates/nuo/src/shell/app/notice_catalog.rs
/// A slash-command reply that is a reading: the answer to a question about the
/// world at the moment it was asked, and therefore not a record of anything
/// (ADR-0140). It lives in the receipt log beside every other disclosure
/// (ADR-0121) and never on the thread's time axis.
CommandReceipt { invocation: String, body: String },
```

Severity derives from the result (`Error`/`UnknownCommand` → `Error`, else `Info`), the topic is the
existing `ClientTopic::Command`, and `body` rides in the line's detail — which the notice center already
renders as an expandable, scrollable, copyable body. No new dialog, pager, or layer.

The transcript's own presentation of a *mutation's* reply is untouched, including the rejected-command
draft restore `[INV-CMD-03]` — `UnknownCommand` is `Reading`-filed for the ledger's purposes but its draft
restore is driven by the result variant, not by the effect, so nothing about recovery changes.

### 3.6 Historical rows: retired by the schema, not filtered at read time

A read-time filter was the first design and it is wrong: the class was never recorded, so any filter has to
re-derive it from `(name, args)`, which forces `nuo-persistence` to import (or be handed) a *later* layer's
vocabulary in order to preserve data worth nothing. The acts are reconstructible from the session's own
state; a stale measurement is useful to nobody.

So `CURRENT_SCHEMA_VERSION` moves to 17 and the load-time migration clears the ledger:

```rust
fn migrate_session_data(mut data: SessionData) -> SessionData {
    if data.schema_version < CURRENT_SCHEMA_VERSION {
        data.commands.clear();
    }
    data.schema_version = CURRENT_SCHEMA_VERSION;
    data
}
```

Rows written at v17 are acts-only by construction, so the branch can never fire again for them. The cost is
the ADR-0059 one: a downgrade to a pre-0140 binary reads the same file and re-renders the stale rows, which
the protocol floor (§3.1) already bounds by refusing to attach a mixed pair.

### 3.7 Dead code to delete first (`nuo-persistence`)

Found while tracing the write path, and it must go before the gate lands or the gate has two masters:

| Site | Finding |
| :--- | :--- |
| `db/handle.rs:290` `PersistenceHandle::record_command` | **zero callers repo-wide** |
| `db.rs:878` `PersistenceCommand::RecordCommand` | only producer is the dead handle method |
| `db/actor.rs:57` its arm | only consumer |
| `db/usage_projection.rs:62` `record_command` | writes `INSERT INTO commands … session_id = ""`, violating the table's own `REFERENCES sessions(id)` (`db/migrations.rs:145`) with `PRAGMA foreign_keys = ON` — the write could never have succeeded |

The session file (`session.commands`) is the ledger; the SQLite `commands` table is a second, never-written
copy. `[INV-PROV-07]`: wire it or delete it. Deleting is correct — the table's only other consumer is a
`DELETE FROM commands` inside a table rebuild.

---

## 4. Files touched

| Crate | File | Change |
| :--- | :--- | :--- |
| `nuo-tool` | `src/completion.rs` | `CommandEffect` (the type's home: `nuo-protocol` re-exports this vocabulary); `CommandSpec.effect`/`.verb_effect`; `CommandSubcommandSpec.effect`; `CommandCatalog::effect_for` |
| `nuo-protocol` | `src/command.rs` | `CommandRecord`/`CommandResult` docs; module doc corrected (§5.1) |
| | `src/events.rs` | `RoundEvent::CommandResult.effect`; `CommandResult`'s class arm unchanged (`Operation`) |
| | `src/lib.rs` | `PROTOCOL_VERSION`/`MIN_PROTOCOL_VERSION` → 20; re-export `CommandEffect` |
| `nuo-server` | `src/startup.rs` | macro `effect:`/verb tuples, `BuiltinCmd::effect`/`effect_for`, `effect_for(parts)`, all 29 declarations, `command_catalog` projection |
| | `src/handlers_slash/record.rs` | `Performed`, `is_durable`, the single `emit` funnel, `record_ack` + `resp_tx` |
| | `src/handlers_slash/dispatch.rs` | the completeness net; the four surface arms answer with a receipt |
| | `src/handlers_slash/commands.rs` | ~40 sites: `parts` passed through; `record_surface_only` in the four no-op arms |
| | `src/handlers_slash/session_ops.rs` | 5 sites |
| | `src/handlers_provider.rs` | `record_provider_ack` routes through the funnel |
| | `src/handlers_slash/tests.rs` | new gate tests; existing per-command tests keep their assertions |
| `nuo-persistence` | `src/session/mod.rs` | `CURRENT_SCHEMA_VERSION` → 17; ledger cleared in `migrate_session_data` |
| | `src/db/{handle,actor,usage_projection}.rs`, `src/db.rs` | §3.7 deletion |
| `nuo` | `src/shell/event_loop/actions/commands.rs` | catalog-driven staging |
| | `src/shell/mod.rs` | `CommandResult` arm branches on `effect` |
| | `src/shell/app/notice_catalog.rs` | `ClientNotice::CommandReceipt` |
| | `src/shell/tests/*` | `[INV-CMD-06..08]`, resume-parity, receipt filing |
| | `src/dev/wire-census.txt` | re-blessed at protocol 20 |
| docs | `docs/reference/commands.md` | a *Recorded* column |
| | `docs/explanation/command-records-and-receipts.md` | new public rationale |
| | `docs/adr/index.md` | ADR-0140 row |

---

## 5. Rulings on the cases that need one

### 5.1 Doc comments that assert the old model

These become false and must be re-stated as "each **act**" in the same change:

- `crates/nuo-protocol/src/command.rs:1-16` (module doc: it now states both classes)
- `crates/nuo-protocol/src/events.rs:1917` ("the durable command ledger, projected into the transcript")
- `crates/nuo-view/src/model/thread.rs:26-29` (the class table's `Operation` row)
- `crates/nuo/src/shell/transcript.rs:183-192` ("one row per invocation")
- `crates/nuo-persistence/src/session/history.rs:276` ("the durable command ledger")
- `crates/nuo-server/src/handlers_slash/record.rs:1` ("invocations, durable acks, errors")

### 5.2 A rejection is never durable — for any class

`UnknownCommand` means nothing ran and nothing reached the model (ADR-0119 §3). Recording it made a typo a
permanent scar in the session's history while the draft it belonged to had already been handed back to the
composer. The gate exempts it unconditionally, independent of the declared class, so a mistyped *reading*
and a mistyped *act* behave identically. `[INV-CMD-03]` (recoverable draft) is unaffected: recovery keys on
the result variant, not on durability.

### 5.3 A failed act persists; a failed reading does not

`/undo` failing to read the workspace journal is an act the user attempted and the system refused — that
belongs in the record, because "I tried to undo and it refused" is exactly what a later reader needs.
`/diff` failing for the same reason is a reading that could not be produced — a receipt. The class, not the
error, decides.

The cost: a *malformed* subcommand of an act (`/trust nonsense`) persists one dimmed error row even though
nothing was attempted. Accepted, and stated as a principle: **a spurious record is merely noisy, a missing
one is unrecoverable.** Distinguishing "usage error" from "failed act" would require a new
`CommandResult` variant, and the codebase's own rule (`[INV-PROV-08]`) forbids telling them apart by
matching prose.

### 5.4 `/export` is a reading even though it touches the clipboard

The clipboard is client-process state that dies with the process, and the exported text is a projection of
the thread, not a change to it. Recording it produced a row whose body is
`"Session exported to clipboard (37 messages, 12043 chars)"` — a receipt, not history.

### 5.5 `/exit` is a reading

It currently reaches `record_invocation` and leaves a resultless row in the session it was used to kill.
Under the class, quitting writes nothing: the session's own `updated_at` already says when it stopped, and
a row saying "someone quit" is not an act on the conversation.

### 5.6 Surface-only commands answer, and record nothing

`/models`, `/connections`, `/tools` and `/mcp` reach `dispatch` only from a client that does *not* own the
modal, and the server performs nothing. Today those arms are silent
(`// Handled in client UI / overlay`). They now reply through the funnel with `Performed::Surface` and a
guidance `CommandResult::Text` — so the command is never silently dropped (`/usage` already sets this
precedent in `commands.rs:886-901`) and no row is fabricated for state this process never touched.

---

## 6. Test plan

Each invariant names the test that fails before its step lands.

| Invariant | Test | Where |
| :--- | :--- | :--- |
| `[INV-CMD-05]` | `every_builtin_declares_an_effect` — the macro cannot be satisfied otherwise, so this asserts the *resolver* agrees with the declaration for every `BuiltinCmd::ALL` entry and every declared verb | `nuo-server/src/startup.rs` tests |
| `[INV-CMD-05]` | `effect_table_is_pinned` — a snapshot of all 29 commands × bare/verb → class, so changing a classification is a visible diff | `nuo-server/src/startup.rs` tests |
| `[INV-CMD-06]` | `readings_leave_the_ledger_empty` — `/help`, `/usage`, `/stats`, `/trace`, `/jobs`, `/jobs logs`, `/diff`, `/search x`, `/skills list`, `/skills status`, `/trust status`, `/permissions`, `/role`, `/tree`, `/threads`, `/export` each leave `session.commands()` empty | `nuo-server/src/handlers_slash/tests.rs` |
| `[INV-CMD-06]` | `readings_still_reply` — each of the above still produces one `RoundEvent::CommandResult` with `effect == Reading` (this is the `record_ack` asymmetry regression) | same |
| `[INV-CMD-06]` | `acts_still_record` — `/unattended on`, `/confinement off`, `/permissions clear`, `/compact`, `/fork`, `/trust deny`, `/init`, `/undo`, `/retry` each leave exactly one row | same |
| `[INV-CMD-07]` | `mutation_completeness_net` — a handler stub that performs a mutation and records nothing still leaves exactly one invocation-only row | `nuo-server/src/handlers_slash/tests.rs` (needs a test-only registry entry) |
| `[INV-CMD-08]` | `receipt_files_and_thread_ignores` — a `Reading` event leaves `app.messages` unchanged and appends exactly one receipt line | `nuo/src/shell/tests/` |
| `[INV-CMD-08]` | `mutation_settles_and_files_no_receipt` — the mirror case | same |
| `[INV-TRANSCRIPT-04]` | `resume_equals_live_for_readings` — build a live thread, then a resumed thread from the same `(facts, ledger)`, assert identical rows; the reading's absence is what makes them equal | `nuo/src/shell/tests/transcript.rs` |
| legacy | `pre_0140_ledger_restores_only_acts` — a ledger of `/help`, `/jobs`, `/unattended on`, a `UnknownCommand` folds to exactly one row | `nuo-persistence/src/session/tests.rs` |
| `[INV-WIRE-03]` | census + protocol-number agreement | `nuo/tests/architecture_firewall.rs` (existing) |
| `[INV-CMD-02]` | `no_client_owned_command_vocabulary` — extend the existing single-owner test to assert the client's effect comes only from `CommandCatalog` | `nuo/src/shell/tests/completion.rs` (existing test, extended) |

Verification recipe:

```bash
cargo check --workspace
cargo test -p nuo-protocol
cargo test -p nuo-server
cargo test -p nuo-persistence
cargo test -p nuo-view
cargo test -p nuo
cargo test -p nuo --test architecture_firewall
cargo test -p nuo --test doc_governance
cargo clippy --workspace --all-targets
```

---

## 7. What this deliberately does not do

- **No new surface.** Receipts already have a home (ADR-0121) and a typed wording catalog (ADR-0138);
  readings join them, they do not get a pager of their own.
- **No ring buffer for readings.** A durability tier with a second retention policy is a policy nobody can
  reason about; ephemeral means ephemeral. If a reading turns out to deserve recall, the answer is to make
  it a `Mutation`-shaped act with a state event, not to grow the receipt log into an audit trail.
- **No `effect` field on `CommandRecord`.** Every row in the ledger is an act by construction, so the field
  would be a constant — exactly the tag inflation `docs/dev/standards/invariants.md` §2 forbids.
- **No rename of `session.commands`.** The narrowed meaning is documented on the type and the field; a
  rename would churn the wire, the snapshot, and every test for a word.
- **No cap on the ledger.** Evicting old rows would evict acts (the `/unattended` posture reconstruction
  scans backwards through the ledger) while keeping nothing worth keeping.
