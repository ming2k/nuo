---
id: ADR-0252
title: "One Action Manifest: Declared Primitives, Declared Compositions, and a Total Trigger Mapping"
status: accepted
date: 2026-11-25
scope: protocol/action, tool/completion, server/handlers, tui/input, tui/event-loop, tui/keymap, governance
superseded_by: null
negative_knowledge: true
---

# 0252. One Action Manifest: Declared Primitives, Declared Compositions, and a Total Trigger Mapping

- **Status**: Accepted
- **Date**: 2026-11-25
- **Amends**:
  [ADR-0040](0040-cross-domain-action-matrix-and-polymorphic-tui-spatial-topology.md) §1 — its action matrix
  becomes a declared manifest instead of a prose table;
  [ADR-0119](0119-slash-command-admission-thread-scoped-naming-server-authority-and-recoverable-rejection.md)
  §2b — the slash vocabulary becomes one namespace of the manifest, and the client intercept table becomes a
  checked projection of it;
  [ADR-0140](0140-command-effect-classes-readings-are-receipts-not-records.md) — the effect class gains a third
  member (`SessionControl`), separating "durable state change" from "occupies a row in this thread's story";
  [ADR-0041](0041-thread-command-purity-viewport-workspace-separation-and-tab-management-topology.md)
  `[INV-CMD-01]` — thread-command purity is now enforced by the manifest and routing table, not by a hand-written
  match table.

---

## Context and Problem Statement

The repository already owns three correct partial answers, and they do not compose:

| Question | Owner today | Gap |
| :--- | :--- | :--- |
| What `/`-prefixed names exist? | `define_builtin_commands!` (ADR-0119 §2b) | 31 flat scenario verbs |
| Is an invocation an act or a reading? | `CommandEffect` (ADR-0140) | two classes; `/new` has no honest home |
| Which actions exist per domain? | ADR-0040 §1 table | prose; no mechanism |

Nothing owns the question *"what can this system be told to do, and who performs it?"* The cost is visible
and measured, not argued:

### 1. One verb, five implementations, divergent outcomes

"Create a thread" has five client call sites. Pre-fix baseline measured across all five sites
(`shell/tests/verb_and_instance_baseline.rs`):

| Site | Trigger | Empty thread | Non-empty thread | Mechanism (pre-fix) |
| :--- | :--- | :--- | :--- | :--- |
| `actions/tabs.rs` | TabBar `+` | `SlashCommand("/new")` | `Subscribe(New(None))` | resident |
| `actions/sessions.rs` | `Alt-t` (`CreateNewSession`) | `SlashCommand("/new")` | `Subscribe(New(None))` | resident |
| `actions/palette.rs` | Palette `CommandId::NewTab` | `SlashCommand("/new")` | `switch_to_new` + `should_quit` | **teardown** |
| `actions/mouse.rs` | Help portal `HelpActionKind::NewTab` | `SlashCommand("/new")` | `switch_to_new` + `should_quit` | **teardown** |
| `actions/commands.rs` | Composer `/new` (and `/new <prompt>`) | staged `pending_command` row + `SlashCommand("/new")` | `switch_to_new` + `should_quit` | **teardown** |

Three of those five (`palette.rs`, `mouse.rs`, `commands.rs`) tore the whole TUI down to re-attach,
discarding every resident workspace — the exact regression
[ADR-0251](0251-multiplexed-thread-sessions-and-resident-per-thread-workspaces.md) was written to end.
Additionally, the composer path staged a local `pending_command` row on an empty thread, leaving a
non-dialogue residue row that flipped `App::is_current_thread_empty()` on the next invocation. And
because `resolve_enter` intercepted `/new` unconditionally (`ArgsPolicy::Any`), typing `/new <prompt>`
in the composer silently dropped the prompt argument instead of forwarding it to the new thread.

### 2. Adding one user-facing action rewrites up to six declaration sites

Measured on the current tree:

| Layer | File | Size |
| :--- | :--- | :--- |
| Key interpretation | `input/action.rs` | 207 variants |
| Command identity | `keymap/{mod,commands}.rs` | 42 ids / 41 registry rows |
| Client intercept table | `shell/session.rs` | one hand-written `match` over string literals |
| Server vocabulary | `server/composition/startup.rs` | 31 builtins |
| Wire request | `protocol/events/request.rs` | 74 variants |
| Driver dispatch | `runtime/thread_driver/driver/mod.rs` | 60 `AgentRequest::` arms |

Three of those (identity, intercept, vocabulary) describe the *same* fact — this action's name, when it is
available, which layer runs it — and any two of them disagreeing is a bug. ADR-0119 §2b already recorded
three-way drift between two of them, found by measurement and caught by no test.

### 3. The same defect class, found in six other verbs

`/new` is an *instance*, not the problem. The same shape recurs:

- **Interrupt** — keyboard `InputAction::Interrupt`, palette `CommandId::InterruptTask`, and the dashboard
  console's `ConsoleVerb::Interrupt` + `@n` addressing. The console reaches a *different thread's* agent;
  the palette reaches the foreground one.
- **Interrupt a subagent** — `Esc Esc` in the Subagent scene resolves to `InterruptSubagent { call_id }`,
  because reusing `Interrupt` leaked scope (ADR-0025).
- **End a thread** — `/exit` and double-`Ctrl+C` send `AgentRequest::EndThread` (keeps disk history), while
  the palette's `KillThread` sends `ControlRequest::KillThread` (removes it). Two wire verbs for one intent.
- **Switch model** — `/models` (surface-owned), `SwitchConnection` (wire), and `/role` (re-staffs).
  `Performed::Surface` exists solely to paper over this split.
- **Open any panel** — 30 `slash:` rows in the client registry, one hand-written intercept match, and 31
  server builtins: three tables for one vocabulary.
- **Pause the follow-up queue** — `Ctrl+P`, `/queue block`, and the queue modal's open/close hook, whose
  "owe a release" bookkeeping lives only in client memory.

### 4. The gap is not "no unified entry point"

A single enum for every interaction is not the fix, and would make things worse. `InputAction`'s 207
variants are mostly **keystroke interpretation** (`Backspace`, `ModalDown`, `PaletteBackspace`, `ScrollUp`,
`InsertChar`) — work that can only happen client-side because the terminal bytes never leave the client.
Routing them through the server would either cost a round-trip per keystroke or pollute the server's
vocabulary with verbs that perform nothing, which is precisely ADR-0119's recorded mistake: *the vocabulary
was named for a layer rather than a scope*.

The real gap is that **no declaration states who owns an action, what class it is, and where its result
lands**, so each of the six sites guesses, and the guesses disagree.

## Decision Drivers

- **One fact, one owner.** An action's name, availability, and performing layer are one fact. Six
  descriptions of it are five chances to drift.
- **Mechanism over discipline.** ADR-0119 §2b's drift was invisible to every test because each list was
  internally consistent. The replacement must be a single declaration plus bidirectional gates tying
  manifest, server builtins, and client routes together.
- **The client owns triggering and presentation; the server owns semantics.** This split already exists and
  works (ADR-0119 §2b: membership is a server fact, display is a client fact). It must be preserved, not
  collapsed.
- **Primitives alone do not cover the product.** A minimal-granularity manifest without composed actions
  pushes sequencing into the client — the "client becomes the renovation crew" failure, and the direct cause
  of the five implementations in §1.
- **Local-only interactions must be first-class.** A keystroke that never crosses the wire is not a
  degenerate case; it is the majority of the input space and must be declarable, not left to a `_` arm.
- **Arguments are never silently dropped.** Routing `/new <prompt>` through a zero-argument action drops
  user intent; argument policy must be explicit per route.

## Architectural Decisions

### 1. One manifest, two layers: primitives and compositions

`protocol::action` owns the manifest schema. Each domain publishes a closed `Namespace`:

```rust
/// Who performs this action. Never inferred from a trigger, a name, or a text shape.
pub enum Authority {
    /// The server performs it; the client may optimise with an optimistic projection.
    Server,
    /// The client performs it and the server only answers (the surface-owned
    /// commands: `/models`, `/connections`, `/tools`, `/mcp`).
    Surface,
    /// The client performs it and it never crosses the wire (navigation, scrolling,
    /// focus, editing). Declared so that "local" is a decision, not a default arm.
    Local,
}

pub struct Primitive {
    pub id: ActionId,
    pub authority: Authority,
    pub class: ActionClass,
    /// What the client must re-read after this action succeeded.
    pub invalidates: &'static [Projection],
    /// Hard preconditions stated as data (actions fail if unmet).
    pub requires: &'static [Requirement],
    /// Whether replay without a request id is harmless.
    pub idempotency: Idempotency,
}

pub struct Composition {
    pub id: ActionId,
    /// Ordered primitives. Compositions cannot contain compositions.
    pub steps: &'static [ActionId],
    /// Hard preconditions stated as data. Branching policies (such as recycling an
    /// empty thread vs opening a new tab) are not preconditions and do not belong here.
    pub requires: &'static [Requirement],
    pub invalidates: &'static [Projection],
    /// Where the result lands. The client reads this; it never infers it.
    pub presentation: Presentation,
    /// The slash spelling, if this composition is reachable from the composer.
    pub slash: Option<&'static str>,
}
```

**Compositions live on the server too.** This is the load-bearing decision. A client that is given only
primitives must sequence them, which means the client re-learns "create → bind workspace → inherit posture →
switch foreground" — the exact knowledge that produced five divergent client paths in §1. The client chooses
**which** declared composition to invoke and **how** to render it; it never invents a sequence.

| Layer | Owns |
| :--- | :--- |
| Server | every primitive's semantics, **and every composition**: the step list, preconditions, invalidation set, and action class. |
| Client | **which** declared action to invoke for a given trigger, and **how** to render it. It holds no step list, no ordering, and no class of its own. |

### 1b. Baseline measurement and acceptance tests

`shell/tests/verb_and_instance_baseline.rs` drives **all five** trigger families for "create a thread"
(`TabBar +`, `Alt-t`, `Palette NewTab`, `HelpPortal NewTab`, and `Composer /new`) and compares
`(wire frames, (total rows, dialogue rows), switch mechanism)`:

- **Pre-fix defect**: `Palette`, `HelpPortal`, and `Composer` tore down the TUI (`TeardownReattach`) on a
  non-empty thread, while `TabBar +` and `Alt-t` used resident `Subscribe(New(None))`. On an empty thread,
  `Composer /new` left a staged `(1, 0)` non-dialogue residue row.
- **Target acceptance** (`all_triggers_agree_on_a_non_empty_thread`, `all_triggers_agree_on_an_empty_thread`):
  all five triggers produce the exact same `(wire, transcript, mechanism)` tuple — `Subscribe(New(None))`,
  `(1, 1)`, `Resident` on a non-empty thread; `SlashCommand("/new")`, `(0, 0)`, `Resident` on an empty
  thread — and `/new <prompt>` preserves the prompt argument on both empty and non-empty threads.

### 2. `ActionClass` has three members, isomorphic to `CommandEffect`

```rust
pub enum ActionClass {
    /// Changes durable state the thread must remember. Earns a ledger record and
    /// occupies a row in this thread's story.
    Act,
    /// Reports the world at the moment of asking. No ledger record, no row; the
    /// reply is a receipt (ADR-0140).
    Reading,
    /// Changes durable *topology* rather than this thread's content: it creates,
    /// binds, switches, or ends threads. It is an act in the audit sense, but it
    /// can never be a row in the thread it creates — that thread did not exist
    /// when the action ran, and the thread it leaves is over.
    SessionControl,
}
```

ADR-0140's `CommandEffect` gains the matching `CommandEffect::SessionControl` variant. `CommandEffect` is
the wire-level slash-command projection of `ActionClass` (`Mutation ↔ Act`, `Reading ↔ Reading`,
`SessionControl ↔ SessionControl`), bridged by a total `From<CommandEffect> for ActionClass` conversion so
the two cannot drift.

Consequences that follow mechanically from the third member:

- `earns_a_record(class)` / `occupies_a_row(class)` is `Act` (`Mutation`) **only**.
- A `SessionControl` reply files a receipt; it never stages, settles, or materialises a thread row.
- The "session reset counts as recorded" special case is deleted, because a reset action is now declared.

### 3. `Presentation` is a declaration, not an inference at each trigger

```rust
pub enum Presentation {
    /// A row in the thread's transcript, settled in place.
    ThreadRow,
    /// A receipt in the client's notice log (ADR-0121), never a row.
    Receipt,
    /// Chrome only: no transcript, no receipt.
    Silent,
}
```

ADR-0233 `[INV-CMDMSG-02]` already forbids deciding a projection's fate from a message's *text*. This
extends it to the *trigger*: whether `/new` reaches the transcript must not depend on whether the user
pressed `+`, typed `/new`, clicked the help portal, or invoked the palette.

The client chooses **how** to render a receipt (notice topic, toast, notice centre, or both, per ADR-0121);
it never chooses **whether** the result is a receipt versus a transcript row.

### 4. Trigger mapping is total, and client routes are checked projections

Every trigger family resolves through a single canonical entry point with no silent fallthrough:

- `action_routes::ROUTES` replaces the hand-written `match` in `shell/session.rs`. Every row explicitly
  declares `RouteSource::Manifest(ActionId)` or `RouteSource::ClientOnly { reason }` (ADR-0041 `[INV-CMD-01]`).
- Routes that take optional arguments changing dispatch semantics (such as `/context --json` and
  `/new <prompt>`) declare `ArgsPolicy::ExactOnly` so bare spellings route to the zero-arg client action
  while parameterized invocations preserve their arguments verbatim.
- Unclaimed slash spellings are forwarded verbatim to the server dispatcher (ADR-0119 §2).

Bidirectional mechanical gates:

1. **Manifest ↔ Client Routes**: every `RouteSource::Manifest(id)` resolves to a declared composition whose
   `slash` matches; every slash-bearing composition in the manifest has a route.
2. **Manifest ↔ Server Builtins**: every slash-bearing composition in `thread_actions` (and future
   namespaces) agrees with `define_builtin_commands!` on spelling and `ActionClass` (`CommandEffect`), so
   the manifest cannot degenerate into an unwired static table.
3. **Manifest Internal Integrity**: every composition step names a declared primitive; every id is unique
   and belongs to its namespace; no `SessionControl` composition presents as `ThreadRow`.

### 5. Wire invocation & single client dispatch funnel (Phase 1 contract)

In Phase 1, compositions map onto existing wire envelopes without introducing a duplicate RPC layer:

- `thread.start_fresh` (`StartFreshThread`): all five client triggers funnel into one shared helper
  `start_fresh_thread(app, runtime, initial_prompt: Option<String>)`:
  - Empty thread (per ADR-0253 §5): if `initial_prompt` is `Some`, submit the prompt in place; otherwise
    send `AgentRequest::SlashCommand("/new")` with **no staged row**.
  - Non-empty thread: send `Wire::Subscribe { action: AttachAction::New(None), project: Some(cwd) }` over
    the live multiplexed connection (ADR-0251), queueing `initial_prompt` (if any) onto
    `composer.pending_prompts` for flush on `ThreadPhase::Ready`.
- `switch_to_new` and `switch_to_new_prompt` teardown paths are deleted from the TUI event loop.

### 6. Namespaces, because one primitive set cannot cover every domain

```text
thread.*       create, bind_workspace, inherit_posture, switch_foreground, interrupt,
               interrupt_subagent, end, read_status
queue.*        push, remove, clear, reorder, pause, snapshot
provider.*     select, register, add_connection, connect, authorize, edit_model
workspace.*    trust_grant, trust_revoke, reload_runtime
config.*       read, mutate, reset
```

ADR-0040 §1 `[INV-ACT-01]` already separates universal viewport actions from domain-specific ones. Each
namespace declares its own small primitive set, and **no namespace is forced to express its state machine
in another's vocabulary**.

### 7. Compositions are the product surface; primitives are the substrate

The compatibility guarantee is at the composition layer. A client (TUI, web, headless, an embedding) binds
its triggers to compositions it can display.

```text
thread.start_fresh = [thread.create, thread.bind_workspace,
                      thread.inherit_posture, thread.switch_foreground]
                     requires: []   (always valid; recycles iff empty per ADR-0253 §5)
                     invalidates: [Transcript, Chrome, Queue, ContextTokens, DraftOwner, Tabs, Posture]
                     presentation: Receipt   slash: "/new"
```

## Normative Invariants Established

- `[INV-VERB-01]` **Declared authority.** Every action declares `Authority` in the manifest. Semantics are
  decided by that field alone; no trigger, name, or text shape may override it.
- `[INV-VERB-02]` **One outcome per action.** The same action produces the same wire sequence, transcript
  delta, and switch mechanism from every trigger. Enforced by
  `all_triggers_agree_on_a_non_empty_thread` and `all_triggers_agree_on_an_empty_thread` across all five
  trigger families.
- `[INV-VERB-03]` **Checked trigger routing.** Client slash routes live in one table (`action_routes::ROUTES`)
  that agrees bidirectionally with the manifest and never silently drops arguments.
- `[INV-VERB-04]` **Manifest agrees with server declarations, both ways.** No composition step outside
  declared primitives; every slash-bearing composition matches `define_builtin_commands!` on spelling and
  effect class.
- `[INV-VERB-05]` **Presentation is declared, never inferred.** A `SessionControl` action never presents as
  `Presentation::ThreadRow`; filing reads `Presentation` / `ActionClass`, never trigger or text shape.
- `[INV-VERB-06]` **A class has one meaning.** `Act` (`CommandEffect::Mutation`) is the only class that
  earns a ledger record and occupies a thread row; `Reading` and `SessionControl` file receipts.

## Consequences & Trade-offs

- **Positive**: All five "create a thread" entry points collapse to one resident implementation; the
  `switch_to_new` teardown regression is completely eliminated; `/new <prompt>` preserves its argument.
- **Positive**: `CommandEffect::SessionControl` eliminates the completeness-net reset special case and
  the empty-thread `/new` residue row.
- **Positive**: Client slash routing (`action_routes::ROUTES`) is a checked projection of the manifest plus
  explicitly justified `ClientOnly` workspace views.
- **Negative & incurred**: Adding a new action requires declaring its manifest row and keeping it aligned
  with `define_builtin_commands!` and `ROUTES`. That cost is intentional: it prevents silent six-way drift.
- **Negative & accepted**: Phase 1 maps compositions onto existing `Wire` frames rather than introducing a
  generic `Wire::InvokeAction` envelope; generic composition RPC is deferred until multiple namespaces
  require server-side multi-step orchestration beyond thread creation.

## Discarded Alternatives & Negative Knowledge

### Alternative A — One enum for every interaction, dispatched at the server

- **Why considered**: Literally "one entry point"; the strongest reading of unification.
- **Why rejected**: 207 `InputAction` variants are keystroke interpretation that cannot leave the client.
  Routing them costs a round-trip per keystroke or pollutes the server vocabulary with no-op verbs
  (ADR-0119 §1).

### Alternative B — Primitives only, no compositions

- **Why considered**: Smallest manifest; maximum client freedom.
- **Why rejected**: Relocates sequencing into every client, reproducing the five divergent implementations
  in §1.

### Alternative C — Keep `CommandEffect` two-valued and special-case `/new`

- **Why considered**: Zero change to an existing enum.
- **Why rejected**: Forces a backstop special case ("session reset counts as recorded") into the
  completeness net, the exact anti-pattern ADR-0140 exists to remove.

### Alternative D — Let the client declare `Presentation`

- **Why considered**: ADR-0119 §2b says display is a client fact.
- **Why rejected**: Confuses *rendering* with *residency*. Transcript-versus-receipt is a content decision
  (ADR-0116/ADR-0121) that must agree across live and resumed sessions.

### Alternative E — Add `/dashboard`-style client commands back into the server vocabulary

- **Why considered**: Would make the two planes one.
- **Why rejected**: Violates ADR-0041 `[INV-CMD-01]`. `RouteSource::ClientOnly` / `Authority::Local` gives
  workspace views a declared home without polluting thread commands.

## Implementation

| # | Step | Invariant / gate |
| :--- | :--- | :--- |
| 0 | Baseline measurement across all 5 trigger families (`TabBar`, `Alt-t`, `Palette`, `HelpPortal`, `Composer`) | `[INV-VERB-02]` |
| 1 | `protocol::action`: `ActionId`, `Authority`, `ActionClass`, `Presentation`, `Primitive`, `Composition`, `Namespace` | `[INV-VERB-01]` |
| 2 | `CommandEffect::SessionControl` + total `From<CommandEffect> for ActionClass`; recorder gate narrowed to `Mutation`/`Act` | `[INV-VERB-06]` |
| 3 | `thread.*` namespace in `server::composition::thread_actions`, bidirectionally verified against `define_builtin_commands!` | `[INV-VERB-04]` |
| 4 | `nuo::shell::action_routes::ROUTES` replaces `shell/session.rs` literal match; `/new` uses `ExactOnly` so `/new <prompt>` preserves args | `[INV-VERB-03]` |
| 5 | All five "create a thread" sites collapse onto one resident `start_fresh_thread` funnel; `switch_to_new` teardown deleted; acceptance tests un-ignored | `[INV-VERB-02]` |
| 6 | Subsequent namespaces (`queue.*`, `workspace.*`, `config.*`, `provider.*`) follow the same pattern | `[INV-VERB-04]` |

Verification:

```bash
cargo check --workspace --all-targets
cargo test -p nuo-protocol -p nuo-server -p nuo
cargo test -p nuo-tests --test integration governance
cargo clippy --workspace --all-targets
```

## Links

- Amends: [ADR-0040](0040-cross-domain-action-matrix-and-polymorphic-tui-spatial-topology.md) §1,
  [ADR-0119](0119-slash-command-admission-thread-scoped-naming-server-authority-and-recoverable-rejection.md)
  §2b, [ADR-0140](0140-command-effect-classes-readings-are-receipts-not-records.md),
  [ADR-0041](0041-thread-command-purity-viewport-workspace-separation-and-tab-management-topology.md)
- Applies: [ADR-0179](0179-one-capability-one-producer-and-no-ghost-builtins.md),
  [ADR-0232](0232-a-classifier-is-only-closed-if-its-members-have-producers.md),
  [ADR-0233](0233-a-slash-command-is-never-a-conversation-message.md),
  [ADR-0116](0116-content-residency-and-surface-layer-topology.md),
  [ADR-0121](0121-notice-log-residency-retained-disclosure-record-and-bubble-as-pointer.md),
  [ADR-0025](0025-scene-scoped-interrupt-chord.md)
- Related: [ADR-0251](0251-multiplexed-thread-sessions-and-resident-per-thread-workspaces.md),
  [ADR-0253](0253-one-owner-per-field-thread-instance-and-viewport.md)
- Reference: `docs/dev/standards/adr.md` §1
