---
id: ADR-0133
title: "Attach Readiness Boundary: Network-Free Handshake, Typed Session Phases, and Authoritative Late Hydration"
status: accepted
date: 2026-10-30
scope: protocol/nuo-protocol, server/nuo-server, host/nuo, provider/nuo-provider, harness/nuo-harness, tui/event-loop, tui/surfaces
superseded_by: null
negative_knowledge: true
---

# 0133. Attach Readiness Boundary: Network-Free Handshake, Typed Session Phases, and Authoritative Late Hydration

- Status: Accepted
- Date: 2026-10-30
- Deciders: Nuo Architecture Working Group
- Consulted: Protocol, Server Core, Interface Architecture, Provider Substrate
- Amends: [ADR-0123](0123-attach-failure-typing-stale-tab-retirement-and-identity-addressed-session-rebasing.md) — extends post-refusal recovery to post-Welcome assembly failures; [ADR-0049](0049-universal-state-snapshot-protocol-and-isolated-subprocess-worker-architecture.md) — makes `SessionSnapshot` the actual first hydration payload; [ADR-0034](0034-deterministic-lifecycle-governance-phased-draining-and-tiered-restart.md) — keeps the claim/lock ordering under the new phase boundary
- Resolves: the `timed out waiting for handshake from server` startup failure caused by eager OAuth resolution inside `bootstrap::assemble`

---

## Context and Problem Statement

A user launching `nuo` received:

```
Error: AttachError { code: None, message: "timed out waiting for handshake from server" }
```

The server log showed the exact composition:

```
14:55:10.591  Select arrives; resolve_auto -> assemble_hosted -> bootstrap::assemble
14:55:20.728  WARN OAuth token resolution failed for 'ga-fox':
              TCP connect to 172.217.194.95:443 exceeded the 10s connect timeout
14:55:20.775  WARN send welcome: IPC write error: Broken pipe
```

The client's `HANDSHAKE_TIMEOUT` was 10 s and the provider's connect timeout was
also 10 s, so the client always gave up first. The defect is not one slow
request. It is a boundary defect with three layers:

### 1. The handshake was treated as "assembly complete"

`Wire::Select` → `Wire::Welcome` (`serve.rs`) ran `resolve_with_declaration` →
`assemble_hosted` → `bootstrap::assemble` inline. That path contained:

- `refresh_oauth_if_needed(...).await` — HTTP token refresh (`OAUTH_REQUEST_TIMEOUT`
  30 s) plus a cross-process credential lock;
- `SkillRegistry::load(...).await` — one GET per configured remote skill repo
  and per skill file, serially awaited, 30 s per GET;
- `fire_session_start(...).await` — external shell hooks, 60 s per hook;
- `session.resume(...).await` — a full durable session load.

The module's own documentation already promised the opposite:
`bootstrap::assemble` says catalog sync, skills, MCP, and the scheduler "all run
in the background so they never delay the first frame". MCP was backgrounded;
OAuth was the exception, and it was on the critical path.

### 2. `Welcome` carried both identity and hydration

`Wire::Welcome` carried `messages`, `round_counter`, `provider`, `model`,
`round_interrupts`, `retry_resolutions`, and `command_catalog`. Producing it
therefore required a completed agent, a full transcript read, and a durable
state load. The first frame was a function of data volume *and* network latency.

### 3. The client had one shared completion deadline

`HANDSHAKE_TIMEOUT = 10 s` covered attach, control, and monitor, and wrapped the
entire assembly. A timeout is `code: None`, which the attach loop treats as
fatal. A recoverable "this session is still warming" outcome was therefore
indistinguishable from a dead server, and the only response was process exit.

**Conclusion:** moving the OAuth call into a `tokio::spawn` would fix one
instance and leave the class intact. Any future slow local or remote operation
would re-couple the first frame to it. The boundary itself must change.

---

## Decision Drivers

- **The first frame must be a bounded, local acknowledgement.** No network I/O,
  external process, full-transcript read, or unbounded lock wait may precede it.
- **Existence is not readiness.** A session can be claimed before its harness is
  usable; the client must be able to render that honestly instead of waiting.
- **Readiness must be typed and observable.** Progress and failure after the
  handshake are states, not silence and not a socket close.
- **Network work never gates readiness.** Provider credentials, remote skill
  repositories, MCP connections, and remote catalogs are background or lazy.
- **Hydration is authoritative and late.** One `SessionSnapshot` carries the
  complete visible state; the client rebuilds from it rather than accumulating
  ad-hoc replay frames.
- **A recoverable failure is never fatal** (`[INV-ATTACH-02]`, extended here to
  post-Welcome assembly).
- **No historical baggage.** A clean protocol break is preferable to keeping the
  old hydration-in-handshake path alive behind serde defaults.

---

## Architectural Decisions

### 1. `Wire::Welcome` becomes an identity acknowledgement

```rust
Welcome {
    session_id: String,
    phase: SessionPhase,
}
```

It is written as soon as a session identity is claimed: before the store is
opened for resume, before the agent exists, before skills/hooks/credentials.
`PROTOCOL_VERSION` and `MIN_PROTOCOL_VERSION` advance to **18**; the old
hydration fields are removed rather than defaulted, because a default would
preserve the very "assemble inside the handshake" behaviour.

### 2. A typed readiness state machine

```rust
pub enum SessionPhase {
    Binding,
    Loading,
    Preparing { stage: PrepareStage },
    Ready,
    Failed { code: Option<String>, message: String },
}
pub enum PrepareStage { Session, Skills, Hooks, Provider }
```

`AgentResponse::SessionPhase(SessionPhaseUpdate)` streams transitions after
Welcome. `Failed.code` reuses the ADR-0123 taxonomy (`session_gone`,
`session_unresumable`), so `[INV-ATTACH-01]` survives past the handshake.

### 3. The registry splits claim from build

`SessionRegistry` gains a `building` map alongside `sessions`:

- `claim` does only bounded local work: project root, one indexed durable
  binding lookup on the blocking pool, a store open that does not load the
  transcript, channel construction, and spawning the build task.
- `build` runs after Welcome; its `SessionStore` load, harness construction,
  local skill discovery and bounded SessionStart hooks happen inside visible
  `Loading` / `Preparing` phases.
- A build failure emits `SessionPhase::Failed` and removes the entry; it never
  closes the connection and never exits the client.
- `session_count` and `has_active_work` include building entries so an idle
  server cannot exit under an in-flight claim.

### 4. Requests are buffered on both sides of the boundary

The client gates composer submission on `Ready` and queues pre-`Ready` prompts
(including `-p`). The server also owns a bounded pre-`Ready` request channel; a
misbehaving client applies backpressure instead of losing or reordering input.

### 5. Hydration is one authoritative snapshot

`SessionSnapshot` gains `commands`, `round_interrupts`, `retry_resolutions`, and
`command_catalog`, and the attach path builds it with the durable transcript and
ledger. The server sends the snapshot before `Ready`; the client rebuilds its
whole document from it, reusing the `ConversationReplaced` reconstruction path
but addressing the snapshot's own `session_id`. This implements the snapshot-first
rule ADR-0049 declared but did not enforce.

### 6. Network I/O is removed from the boundary, not merely moved

- Eager OAuth refresh is deleted from both `bootstrap::assemble` and
  `handlers_provider::activate`. Credentials resolve lazily and single-flight on
  the first provider request; a failure there is a retained notice, not a
  session failure.
- Remote skill repositories are skipped by the readiness-time `load_local`; the
  full `reload` runs in the background after the local registry exists.
- SessionStart hooks remain in the build (they can inject context), inside a
  `Preparing{Hooks}` phase and a 10 s group budget.
- MCP and catalog refresh remain background and do not gate `Ready`.

### 7. Client timeouts become per-verb liveness deadlines

`HANDSHAKE_TIMEOUT` is replaced by `ATTACH_ACK_TIMEOUT` (5 s attach first frame),
`CONTROL_REPLY_TIMEOUT` (10 s) and `MONITOR_FIRST_FRAME_TIMEOUT` (10 s). Because
the server no longer assembles inside the handshake, the attach deadline is a
liveness signal. A pre-Welcome state frame is now a hard protocol error rather
than a silently dropped frame.

---

## Normative Invariants Established

- **`[INV-ATTACH-03]` Handshake Is a Bounded, Network-Free Acknowledgement**:
  between `Select` and `Welcome` the server MUST perform only bounded local
  identity work — no network I/O, no external process, no full-transcript read,
  and no unbounded lock wait. Enforced by
  `welcome_then_snapshot_then_ready_is_the_order` (a hosted attach acknowledges
  before hydration) and `claim_path_is_network_free` (server source-level
  contract over `claim`).

- **`[INV-ATTACH-04]` Readiness Is Typed and Precedes Interactivity**: the
  authoritative `SessionSnapshot` (including the workspace-trust `HarnessState`)
  MUST be sent before `SessionPhase::Ready`; a client MUST NOT submit a turn
  before `Ready`; and a `SessionPhase::Failed` after Welcome MUST NOT terminate
  the client. Enforced by `welcome_then_snapshot_then_ready_is_the_order`
  (server), `client_queues_until_ready_then_flushes` and
  `post_welcome_failure_is_disclosed_and_never_quits` (client shell).

---

## Consequences

- **Positive**: the reported failure class is structurally impossible. Opening
  the client no longer performs a provider network call, a remote skill fetch, a
  hook, or a transcript load. The server's own documented "never delay the first
  frame" contract now holds for every operation, not just MCP.
- **Positive**: a slow or unreachable provider is now a normal, retained
  provider error on the first request instead of a dead client.
- **Positive**: late hydration makes reconnects uniform: every attach, first or
  second, receives the same authoritative snapshot.
- **Negative & Trade-off**: the protocol break rejects a pre-18 server at the
  handshake. This is deliberate and mirrors ADR-0123 §6: client and server ship
  from one binary, and a shim would restore the coupling being deleted.
- **Negative & Trade-off**: the client must render a warming state and queue
  input. The alternative — hiding assembly behind a spinner-less blank screen —
  is what produced the "it is just stuck" report.

## Rejected Alternatives & Negative Knowledge

### Alternative A: Background only the OAuth refresh
- **Why considered**: smallest patch; one `tokio::spawn` around
  `refresh_oauth_if_needed`.
- **Why rejected**: Welcome would still wait for local skill discovery, the
  harness build, SessionStart hooks, and a full session load. The next slow
  dependency would reproduce the same failure. It also leaves `Welcome` as a
  hydration payload, so the first frame remains a function of transcript size.

### Alternative B: Raise `HANDSHAKE_TIMEOUT`
- **Why considered**: the client timeout equals the provider connect timeout;
  a larger number would "fix" the observed case.
- **Why rejected**: it converts a deterministic failure into a probabilistic
  one and makes every genuine dead-server case take longer to detect. The
  deadline was measuring the wrong thing.

### Alternative C: Keep hydration fields and add a phase
- **Why considered**: no protocol break; old clients keep working.
- **Why rejected**: two sources of truth for the same state. The server would
  still have to assemble before Welcome to fill the fields, and the phase would
  be decorative — a badge on a coupling that remains.

### Alternative D: `#[serde(default)]` compatibility for the old Welcome
- **Why considered**: lets a new client talk to an old server.
- **Why rejected**: an old server has no phase and no post-Welcome snapshot. A
  defaulted empty transcript would silently render an empty session, and the
  client would have no way to learn it attached to a server that still blocks
  the handshake on OAuth. A clean break is the honest option.

### Alternative E: Retire the tab on any post-Welcome failure
- **Why considered**: simple and uniform.
- **Why rejected**: `session_unresumable` means the history is real and a retry
  may succeed. Retiring it would discard recoverable work. Only
  `session_gone` retires; every other failure keeps the tab and returns to the
  picker with a retained notice.

---

## Links

- [ADR-0049](0049-universal-state-snapshot-protocol-and-isolated-subprocess-worker-architecture.md) — the snapshot-first rule this ADR finally enforces
- [ADR-0123](0123-attach-failure-typing-stale-tab-retirement-and-identity-addressed-session-rebasing.md) — typed refusals, stale-tab retirement, identity-addressed rebasing
- [ADR-0127](0127-workspace-trust-review-obligation-request-lifecycle-and-consistent-refusal.md) — the pre-view trust gate whose ordering now rides the snapshot
- [ADR-0034](0034-deterministic-lifecycle-governance-phased-draining-and-tiered-restart.md) — server lifecycle and lock ordering
- [ADR-0119](0119-slash-command-admission-thread-scoped-naming-server-authority-and-recoverable-rejection.md) — precedent: a rejection is a typed, recoverable outcome
