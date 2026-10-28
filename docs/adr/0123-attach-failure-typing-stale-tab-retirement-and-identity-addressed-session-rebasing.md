---
id: ADR-0123
title: "Attach Failure Typing, Stale Tab Retirement, and Identity-Addressed Session Rebasing"
status: accepted
date: 2026-10-28
scope: protocol/nuo-protocol, server/nuo-server, host/nuo, tui/event-loop, tui/surfaces
superseded_by: null
negative_knowledge: true
---

# 0123. Attach Failure Typing, Stale Tab Retirement, and Identity-Addressed Session Rebasing

- Status: Accepted
- Date: 2026-10-28
- Deciders: Nuo Architecture Working Group
- Consulted: Protocol, Server Core, Interface Architecture
- Amends: [ADR-0039](0039-client-tab-workspace-scene-unification-and-reactive-surface-sync.md) — supplies the tab-liveness contract `[INV-TAB-01..03]` presupposed without defining; [ADR-0091](0091-sovereign-tab-context-isolation-and-tabbar-boundary-purity.md) — extends `[INV-TAB-12]` from context preservation to *session identity* preservation

---

## Context and Problem Statement

A client tab can outlive the server session behind it. The repository already said so —
in a constant nobody used:

```rust
// crates/nuo-protocol/src/version.rs — declared, re-exported nowhere, referenced nowhere
/// The attach target no longer exists: no hosted session holds the id and no
/// durable binding records it. This is a *recoverable, expected* outcome — a
/// client tab can outlive the session behind it ... so the frontend must
/// retire the stale tab instead of aborting.
pub const ERR_SESSION_GONE: &str = "session_gone";
pub const ERR_SESSION_UNRESUMABLE: &str = "session_unresumable";
```

The comment describes the correct behaviour precisely. No code implemented any of it.
Four independent gaps composed into a **client that quits when you click an old tab**:

### 1. The failure channel erased the distinction the protocol had already drawn

`SessionRegistry::resolve_id` produced three semantically different failures as one
`ResolveOutcome::Error(String)`, and `serve.rs` sent it through the *code-less*
`send_error`:

| Situation | Truth | Wire result |
| :--- | :--- | :--- |
| No hosted entry, no durable binding | gone forever — retire the tab | `{"message": "unknown session id 'x'"}` |
| Durable binding, assemble failed | retryable — keep the tab | `{"message": "could not resume session x: …"}` |
| Host cannot create sessions | a server posture problem | `{"message": "this host cannot create sessions"}` |

A machine-readable `code` field existed on `Wire::Error` and was already used for protocol
negotiation (`protocol_mismatch`). The attach path never set it, so the client had nothing
to branch on but prose.

### 2. The client downgraded the typed error to a `String`, then threw it

`transport.rs` matched `Wire::Error { message, .. }` — the `..` discarding the code — and
returned `Err(format!("server rejected the attach: {message}"))`. Declared error taxonomy,
discarded at the first hop.

### 3. The attach loop had no recovery edge

`runner.rs` wrote `let handshake = target_server.connect(action).await?;`. Every attach
failure — including a *provably expected* one — propagated with `?` to `main`, and the
process exited. `[INV-TAB-03]` says a tab can be closed; nothing said a tab that had already
lost its session must be retirable rather than fatal.

### 4. Ghost tabs were *manufactured*, not merely retained

This is the part that made the bug intermittent and confusing. `AppMutation::LiveSession`
rebased session identity **by "whichever tab happens to be active"**:

```rust
AppMutation::LiveSession(session_id) => {
    app.live_session_id = session_id.clone();
    if let Some(active_tab) = app.surfaces.active_tab_mut() {   // ← positional addressing
        if let TabKind::Thread(ref mut id) = active_tab.kind {
            *id = session_id;                                   // ← unconditional rewrite
            active_tab.title = None;
        }
    }
}
```

The server's `DeleteSession` handler deletes-then-`reset()`s when the target was the active
session, minting a fresh never-persisted id and broadcasting `ConversationCleared { session_id }`
(`handlers_session.rs`). Meanwhile every picker path fired the deletion
**fire-and-forget** and *synchronously* swapped the tab first:

1. `send_intent(DeleteSession { empty_session })` — queued, not awaited
2. `replace_tab(active_idx, target_tab)` — active tab is now the **target**
3. `switch_to_target = target; should_quit = true` — the TUI run ends
4. `ConversationCleared { new_id }` lands before the loop drains → `LiveSession(new_id)`
5. Positional rebasing rewrites **the target tab** to `new_id`

The user then re-attaches to `target` correctly — but `preserved_tabs` carries a tab whose id
is `new_id`: an empty session no client ever attached, hence reaped by the idle-empty reaper
after `IDLE_EMPTY_SESSION_TTL` (5 minutes). That is the observed time-dependence: click the
stray tab within the window and it appears to work; later it is gone and the click is fatal.

The emptiness of the old thread was never the defect. **The defect was that the discard
receipt was addressed to the wrong tab**, because sessions were addressed by position.

---

## Decision Drivers

- **Recoverable failure must be recoverable, not fatal**: an expected outcome (a tab whose
  session is gone) MUST NOT be able to terminate the client process.
- **Typed error taxonomy end to end**: a distinction drawn on the server MUST survive to the
  branch that acts on it. No layer may flatten a code into prose.
- **Identity, not position**: every state transition that changes *which session a tab
  represents* MUST address its target by the session id it is leaving, never by "the active
  tab". Positional addressing is the failure mode, not the implementation detail.
- **No speculative liveness oracle**: a tab's session is verified against server-authoritative
  evidence at the moment of attachment, not guessed from client-side broadcasts (session
  suspension also emits `SessionRemoved` and is *not* a loss — a guess would be wrong in a
  documented case).

---

## Architectural Decisions

### 1. `ResolveOutcome::Error` becomes a typed refusal

```rust
pub enum ResolveOutcome {
    Welcome(BoundSession),
    Pick { sessions: Vec<SessionOverview> },
    /// A refusal the client is expected to branch on.
    Refused(Refusal),
}

pub struct Refusal {
    pub message: String,
    /// `None` means "no specific taxonomy" — the client must treat it as fatal
    /// rather than invent a recovery it cannot justify.
    pub code: Option<&'static str>,
}
```

`resolve_id` classifies by *evidence*, not by string:

- hosted entry missing **and** `lookup_session_workspace(id)` is `None` → `ERR_SESSION_GONE`
- durable binding present, `assemble_hosted` failed → `ERR_SESSION_UNRESUMABLE`
- `AssembleErr::NoHost` → `ERR_SESSION_GONE` (this host can never serve that id)

`serve.rs` routes through `send_error_with_code`, and both constants are re-exported from
`nuo_wire` so no downstream crate can re-spell them.

### 2. The client preserves the code in a typed attach error

```rust
// transport.rs
pub struct AttachError {
    pub code: Option<String>,
    pub message: String,
}
pub enum Handshake { Attached { … }, Pick(Vec<SessionOverview>) }
type AttachResult = Result<Handshake, AttachError>;
```

`connect`, `ClientTarget::connect`, `finish_handshake*` all carry `AttachError`. `Display`
prints `message` alone, so diagnostics do not regress. Transport *I/O* failures (connection
refused, framing, timeout) are `AttachError { code: None }`.

### 3. The attach loop gains a retirement edge instead of a fatal `?`

`runner.rs` matches the typed refusal:

- `ERR_SESSION_GONE` → **retire that tab** from `preserved_tabs`, paint a fault disclosure
  through `App::record_notice` (so it is retained in the log, per [ADR-0121](0121-notice-log-residency-retained-disclosure-record-and-bubble-as-pointer.md)),
  drop `target`, and degrade to the picker. **The process lives.**
- `ERR_SESSION_UNRESUMABLE` → keep the tab (its history must not be silently discarded),
  surface the fault, degrade to the picker.
- any other `Err` → genuinely fatal transport fault, still fatal.

Retirement is a pure function over `preserved_tabs`, so it is testable without a server.

### 4. `LiveSession` becomes identity-addressed and cannot invent a tab

```rust
AppMutation::LiveSession { previous_id, next_id } => { … }
```

The rebase applies **only** to the tab whose `thread_id() == previous_id`. If no tab holds
`previous_id`, nothing is rewritten — which is exactly the race above, and it now converges
instead of corrupting. `previous_id` is captured server-side *before* the reset and carried
on `AgentResponse::ConversationCleared`.

### 5. Retirement happens at the attach boundary, keyed on the refusal code

A stale tab is removed **before the next run mounts it**, by a pure function over
the preserved tab list (`retire_preserved_tab`), so a tab whose session is gone
never enters a UI frame at all. This is deliberately not a `ClientTab` flag: an
`Orphaned` state that is set and then immediately removed in the same breath
would be dead state — a field that exists to be checked by nothing. Retirement is
a transition, and it is expressed as one.

Liveness therefore has exactly one author: the attach loop, and exactly one
witness: a server-attested `ERR_SESSION_GONE`. `MonitorEvent::SessionRemoved` is
never consulted, because suspension and idle teardown publish it while leaving the
transcript durable — an inference from it would delete live tabs with recoverable
history.

The consequence is that a click whose session died *during* a run converges
rather than crashing: the click ends the run with `switch_to_target`, the next
attach is refused as gone, the tab is retired, and the client lands on the picker.
The loop terminates because the degrading attach is `Picker`, which cannot itself
be refused as gone.

### 6. Protocol break (no compatibility shim)

`ConversationCleared` gains a required `previous_id`. This is a deliberate clean break:
`PROTOCOL_VERSION` is raised and `MIN_PROTOCOL_VERSION` is raised to match, so a stale server
is rejected at the handshake with `protocol_mismatch` instead of silently mis-rebasing tabs.
The version-skew machinery already exists and is tested; reusing it is cheaper and more honest
than a `#[serde(default)]` that would reintroduce positional fallback on the server's behalf.
No legacy path, no dual-shape frame, no "if previous_id is empty, fall back to active tab".

---

## Normative Invariants Established

- **`[INV-ATTACH-01]` Typed Refusal End to End**: An attach refusal MUST carry a
  machine-readable code from the server through the transport to the decision site. A layer
  that converts a refusal to prose without its code violates this invariant. Enforced by
  `attach_refusal_preserves_code` (transport), `wire_error_frame_carries_the_refusal_code`
  (wire), `gone_and_unresumable_are_distinct_codes` and `unknown_session_is_refused_as_gone`
  (server) and `unknown_id_is_refused_with_session_gone_code` (end to end).

- **`[INV-ATTACH-02]` Recoverable Attach Failure Is Never Fatal**: A refusal whose code is
  `session_gone` or `session_unresumable` MUST NOT terminate the client. It MUST retire (resp.
  retain) the tab and return the client to a usable attach surface. Enforced by
  `stale_tab_retirement_is_keyed_on_the_gone_id_only` (the retirement transition) and
  the attach loop's refusal match, which has no fatal arm for either recoverable code.

- **`[INV-TAB-13]` Identity-Addressed Session Rebasing**: A state change that alters which
  session a tab represents MUST be addressed by the session id being left. Rebasing by
  "active tab" is forbidden. Enforced by
  `conversation_cleared_does_not_rebase_a_replacement_tab`.

- **`[INV-TAB-14]` No Speculative Liveness**: A tab MUST NOT be retired from a
  broadcast or from an inferred absence. Only a server-attested `session_gone`
  refusal may retire it. Enforced by `stale_tab_retirement_is_keyed_on_the_gone_id_only`.

---

## Consequences

- **Positive**: The reported crash class is structurally impossible — the only path that
  consumes an attach refusal can no longer exit. Ghost tabs stop being manufactured, closing
  the *source* rather than the symptom. Two long-dead protocol constants become load-bearing,
  removing the "documented behaviour with no mechanism" debt.
- **Positive**: Because `[INV-TAB-13]` is addressed by session identity, the same fix removes a
  latent title-corruption bug: the old positional rebase also cleared `title` on whatever tab
  was active, silently wiping an unrelated thread's title (against
  [ADR-0050](0050-authentic-thread-title-lifecycle-clean-scene-head-and-semantic-tab-projection.md)).
- **Negative & Trade-off**: A protocol bump rejects same-binary-skew servers. Mitigation: the
  existing handshake negotiation reports the exact remedy, and the client auto-respawns a stale
  server image on version drift (`ensure_server`).
- **Negative & Trade-off**: Client and server must land together. This is deliberate for a
  semantic frame change; a shim would restore the very positional fallback being deleted.

## Rejected Alternatives & Negative Knowledge

### Alternative A: Defensive guards at the click site
- **Why considered**: Cheapest possible patch — bounds-check the index, catch the failure.
- **Why rejected**: Symptom relocation. The ghost tab would still be displayed; the doomed
  attach would still be constructed; only the death would move one frame later, and the user
  would be left with a permanently dead tab. It also leaves the manufacturing race intact.

### Alternative B: Retire tabs from `MonitorEvent::SessionRemoved`
- **Why considered**: A push-based liveness signal requires no attach round-trip.
- **Why rejected**: Factually wrong. `SessionRemoved` is emitted by *suspension*
  (`registry.rs`), which is documented as "the session is not over, merely parked", and by
  idle-session teardown whose transcript is durable. Retiring on it would delete live tabs with
  recoverable history. `[INV-TAB-14]` exists to keep this from being reintroduced.

### Alternative C: `#[serde(default)] previous_id` with positional fallback
- **Why considered**: Keeps an old server talking to a new client.
- **Why rejected**: The fallback *is* the bug. Any server that omits the field is a server whose
  rebase would be positional again; accepting it converts a compile-time incompatibility into a
  runtime corruption that only appears after a five-minute reap. A clean protocol break is the
  honest option.

### Alternative D: Make the client re-attach automatically to the reaped replacement id
- **Why considered**: Would appear seamless.
- **Why rejected**: The replacement id is a *never-persisted empty session* the client never
  asked for. Adopting it fabricates user history and hides that the tab's real content is gone.
  Retirement with a retained disclosure tells the truth.

### Alternative E: Mark tabs non-live eagerly from the `host_sessions` mirror
- **Why considered**: The dashboard already holds a row list; absence looks like a liveness
  oracle.
- **Why rejected**: The mirror is filtered (`include_idle`) and eventually consistent; absence
  would retire tabs that are merely mid-attach. Liveness is a server attestation, not an
  inference ([INV-TAB-14]).

### Alternative F: A persisted `TabLiveness` flag on `ClientTab`
- **Why considered**: Would let `activate_tab_at_index` short-circuit a doomed attach before
  the user pays a run teardown for it.
- **Why rejected**: The flag would be set and consumed within the same attach boundary — the
  retirement already prevents the tab from ever being mounted again — so it would be state
  with no reader, and a second writable path to the same fact. Two ways to decide "is this tab
  live" is precisely how the positional rebase bug arose: a fact with more than one author.
  Retirement as a transition keeps one author and one witness.

## Links

- [ADR-0039](0039-client-tab-workspace-scene-unification-and-reactive-surface-sync.md) — `ClientTab` topology whose liveness contract this ADR supplies
- [ADR-0041](0041-thread-command-purity-viewport-workspace-separation-and-tab-management-topology.md) — tab management actions routed through `open_tab`
- [ADR-0050](0050-authentic-thread-title-lifecycle-clean-scene-head-and-semantic-tab-projection.md) — authentic tab titles, which positional rebasing silently cleared
- [ADR-0091](0091-sovereign-tab-context-isolation-and-tabbar-boundary-purity.md) — sovereign tab context; extended here from context to identity
- [ADR-0119](0119-slash-command-admission-thread-scoped-naming-server-authority-and-recoverable-rejection.md) — precedent: a rejection is a typed, recoverable outcome, not a fatal string
- [ADR-0121](0121-notice-log-residency-retained-disclosure-record-and-bubble-as-pointer.md) — the funnel through which a retirement disclosure is retained
