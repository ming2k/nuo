---
id: ADR-0135
title: "Canonical Thread Nomenclature: Protocol-Level Wire Unification, Zero-Compromise Invariant Enforcement, and Session Category Cleansing"
status: accepted
date: 2026-12-14
scope: protocol/nuo-protocol, persistence/nuo-persistence, server/nuo-server, host/nuo, tui/surfaces, tui/event-loop
superseded_by: null
negative_knowledge: true
---

# 0135. Canonical Thread Nomenclature: Protocol-Level Wire Unification, Zero-Compromise Invariant Enforcement, and Session Category Cleansing

- Status: Accepted
- Date: 2026-12-14
- Deciders: Nuo Architecture Working Group
- Amends: [ADR-0038](0038-thread-entity-server-ssot-fanout-and-orthogonal-action-lifecycle.md) (§1, `[INV-THREAD-01]`), [ADR-0043](0043-thread-domain-nomenclature-unification-compact-tabbar-affordance-and-c-x-namespace-reorganization.md) (Completes full-stack naming unification), [ADR-0133](0133-attach-readiness-boundary-network-free-handshake-typed-session-phases-and-authoritative-late-hydration.md) (Advances protocol version to 19)

---

## Context and Problem Statement

ADR-0038 (§1) established the canonical three-tier taxonomy (`Client`, `Thread`, `Server`) and enacted `[INV-THREAD-01]`:
> *"The multi-instance persistent dialogue workload MUST be formally designated as Thread. Code, APIs, and wire envelopes MUST isolate thread identity from transport connection handles."*

Subsequently, ADR-0043 unified user-facing nomenclature: slash commands (`/threads`), TUI modal dialogs (`ThreadsDialog`), and CLI verbs (`nuo thread ls`, `nuo thread rm`).

However, the architecture halted at a dangerous half-measure: **the IPC wire protocol, SQLite persistence layer, and server runtime engine retained legacy `session` nomenclature**. This introduced systematic architectural and cognitive rot:

1. **Dual-Vocabulary Schizophrenia**:
   - The user inspects `nuo thread ls` and navigates `ClientTab::Thread(id)`.
   - The wire protocol exchanges `session_id`, `SessionPhase`, `SessionSnapshot`, `SessionTree`, and `MonitoredSession`.
   - The server hosts `SessionRegistry`, `HostedSession`, and `SessionStore`.
   - The frontend event loop constantly translates between `viewed_session_id` and `TabKind::Thread`.
2. **Category Inflation of "Session"**:
   The word `session` was used simultaneously for:
   - The conversational agent task workload (which is a **Thread**);
   - The physical IPC connection stream between client and server (which is a **Connection Session**);
   - The OAuth browser/device flow state (which is an **Auth Session**);
   - The presentation viewport lifetime (which is a **Client Viewport**).
   When a word means everything, it means nothing.
3. **Breach of Engineering Discipline**:
   Retaining `session_id` on the wire while preaching `Thread` to the user violates "Mechanisms Over Exhortations". It signals to developers that architectural red-lines can be ignored whenever refactoring across crate boundaries is perceived as "too high workload."

---

## Decision Drivers

- **Zero-Compromise Domain Purity**: The business entity representing an agent dialogue and cognitive execution graph is a **Thread** everywhere—from user keystrokes to database rows.
- **No Backward Compatibility Shims**: Refuse transitional serde aliases (`#[serde(alias = "session_id")]`) or dual-vocabulary shims. A clean, atomic protocol break guarantees zero hidden technical debt.
- **Strict Boundary Allocation**: The word `Session` is restricted exclusively to ephemeral network transport connections and cryptographic auth tokens.
- **Mechanically Enforced Invariants**: Establish compiler-verifiable typestates and wire invariants.

---

## Architectural Decisions

### 1. Protocol Cleansing (`nuo-protocol`)

1. **Protocol Version Bump**:
   Advance `PROTOCOL_VERSION = 19` and `MIN_PROTOCOL_VERSION = 19`. Older clients connecting to v19 servers fail fast at the handshake boundary with `protocol_mismatch`.
2. **Wire Types Unification**:
   - `session_id: String` ➔ `thread_id: String` across all `AgentRequest`, `AgentResponse`, `Wire`, and `ControlRequest` envelopes.
   - `SessionPhase` ➔ `ThreadPhase` (`Ready`, `Warming`, `Failed`).
   - `SessionSnapshot` ➔ `ThreadSnapshot`.
   - `SessionDetail` ➔ `ThreadDetail`.
   - `SessionTree` ➔ `ThreadTree`.
   - `MonitoredSession` ➔ `MonitoredThread`.
   - `SessionOverview` ➔ `ThreadOverview`.
   - `SessionContextSnapshot` ➔ `ThreadContextSnapshot`.
   - `SessionForkKind` ➔ `ThreadForkKind`.

### 2. Persistence & Storage Alignment (`nuo-persistence`)

- `SessionStore` ➔ `ThreadStore`.
- `SessionRecord` ➔ `ThreadRecord`.
- Database table columns and indexes migrate from `session_id` to `thread_id`.

### 3. Server Coordinator Alignment (`nuo-server`)

- `SessionRegistry` ➔ `ThreadRegistry`.
- `HostedSession` ➔ `HostedThread`.
- Server event broadcast channels and handlers align on `thread_id`.

### 4. Client Viewport Alignment (`nuo::shell`)

- `current_session_id` ➔ `current_thread_id`.
- `viewed_session_id` ➔ `viewed_thread_id`.
- Complete elimination of `session_id` in client state and routing.

---

## Normative Invariants Established

- **`[INV-THREAD-03]` Full-Stack Thread SSOT**:
  The business entity representing an autonomous agent dialogue, execution context, and tool graph MUST be identified and named as `Thread` across all tiers: Presentation, IPC Wire, Server Registry, and Persistence. Any protocol field identifying this workload MUST be named `thread_id`, never `session_id`.
- **`[INV-SESSION-01]` Transport Purity of Session Nomenclature**:
  The word `Session` is strictly reserved for ephemeral network connections (`TransportSession`, `ClientConnection`) and authentication tokens (`OAuthSession`). Using `Session` to designate conversational state or durable task workloads is an architectural defect.

---

## Negative Knowledge & Rejected Alternatives

### 1. Rejected: Preserving `serde(alias = "session_id")` for "Smooth" Upgrade
- *Reason*: Transitional aliases decay into permanent baggage. When two names for the same field exist, new engineers and AI models mix them interchangeably, documentation splits, and serialization tests lose determinism. Nuo enforces atomic protocol breaks via SemVer and `PROTOCOL_VERSION`.

### 2. Rejected: Renaming Transport Handles to "Threads"
- *Reason*: Re-affirmed from ADR-0043. Socket streams and OAuth tokens are genuine sessions. Renaming them to threads would invert the category error in the opposite direction.
