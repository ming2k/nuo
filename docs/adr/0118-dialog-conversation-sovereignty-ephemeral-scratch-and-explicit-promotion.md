---
id: ADR-0118
title: "Dialog Conversation Sovereignty: Ephemeral Scratch by Default, Scoped Membership for Durable Dialogues, and the Explicit Promotion Boundary"
status: accepted
date: 2026-12-09
scope: server/nuo-server, protocol/nuo-protocol, persistence/nuo-persistence, host/nuo, tui/surfaces, agent/conversation
superseded_by: null
negative_knowledge: true
---

# 0118. Dialog Conversation Sovereignty: Ephemeral Scratch by Default, Scoped Membership for Durable Dialogues, and the Explicit Promotion Boundary

- Status: Accepted
- Date: 2026-12-09
- Deciders: Nuo Architecture Working Group
- Consulted: Server Core, Persistence, Interface Architecture
- Amends: [ADR-0058](0058-granular-causal-session-forking-sovereign-tab-projection-and-aside-retirement.md) — completes the aside retirement by supplying the general rule the `/btw` retirement was a special case of; [ADR-0104](0104-fork-client-projection-reconciliation-and-aside-retirement-completion.md) — restates `[INV-FORK-04]` for the dialog case
- Succeeds from: [ADR-0116](0116-content-residency-and-surface-layer-topology.md), [ADR-0117](0117-transcript-admission-purity-dialogue-only-thread-documents.md)

---

## Context and Problem Statement

A dialog that converses — a cockpit console asking the Archivist, a modal that summarises a session, a
retrieval panel that answers follow-ups — needs three things: a provider, a message list, and somewhere
to put the exchange. The system offers exactly one of each per session: the session's agent, the session's
`model_window`, and the session's `TranscriptEntry` table. So the path of least resistance is to run the
dialog *as* the session, and the exchange lands in the thread transcript.

That is the wrong identity, and the codebase has already discovered this twice by other means:

- **`/btw` aside.** A side conversation attached to the thread was retired
  ([ADR-0058](0058-granular-causal-session-forking-sovereign-tab-projection-and-aside-retirement.md),
  completed by [ADR-0104](0104-fork-client-projection-reconciliation-and-aside-retirement-completion.md))
  in favour of a **fork**: a new session with its own id, its own transcript, and a preserved parent.
  The lesson generalises — a side conversation needs its own conversation identity, not a share of
  someone else's.
- **The Archivist.** Its service holds a bounded `Vec<Message>` scratch, is documented as *"no workspace
  session store, no transcript persistence — the Archivist has no workspace and its answers are ephemeral
  cockpit dialogue"*, and reaches the client through a control-plane verb whose receipt lands in the
  cockpit console log, never the timeline.

Both are correct. What is missing is the rule that makes correctness the only reachable state: **a
dialog must not be able to obtain the handles that would let it write into a thread.** Today that is a
convention; a service that is handed `Arc<SessionStore>` has, by construction, the ability to append
dialogue, ledger rows, and projection directives to whatever session id it also holds, and no type or
test objects.

Three failure modes follow from the missing rule: (a) a dialog's question/answer pair becomes dialogue
that the model later sees as if the user had said it; (b) a dialog's receipts occupy rounds, breaking the
thread's round sequence; (c) an embedding author adding a new dialog has no stated default, so the
cheapest implementation — reuse the session — is also the wrong one.

## Decision Drivers

- **Conversation identity is not optional** — every message belongs to exactly one conversation, and a
  dialog's conversation is not the thread's.
- **Capability, not intent** — a dialog must be *unable* to write into a thread, not merely instructed
  not to.
- **Memory must not be lost by accident, nor kept by accident** — a dialog that the user closes should not
  leave residue; a dialog that genuinely needs recall should have a first-class home rather than an
  improvised one.
- **Crossing into the thread must be a user act** — promoting a dialog's content into the conversation is
  a deliberate operation with provenance, not a side effect of asking a question.
- **Zero legacy burden** ([ADR-0059](0059-canonical-thread-persistence-schema-and-zero-legacy-clean-break.md)) —
  no "dialog mode" flag threaded through the existing session path.

## Architectural Decisions

### 1. A dialog conversation has no session handle: ephemeral scratch is the default

A dialog-owned conversation is a service-local, bounded message list plus a borrowed provider:

```rust
pub struct DialogConversation {
    scratch: TokioMutex<Vec<Message>>,   // bounded, most-recent-wins
    cap: usize,                          // message cap, not a token budget
    // deliberately absent: Arc<SessionStore>, Arc<dyn FactSink>, session id
}
```

`[INV-DIALOG-01]` — **no thread handle, no thread write.** The constructor does not receive, hold, or
accept a `SessionStore` or a `FactSink`, and it does not carry a thread's session id. Persistence
capability is therefore absent from the type, not withheld by policy. This is the Archivist's shape,
promoted to the general contract: *a dialog cannot pollute a transcript it has no way to address.*

Scratch is bounded by message count, not by tokens, because it is a working memory with a working
lifetime: when the dialog closes, the scratch goes with it, and the receipts in the surface log
([ADR-0116](0116-content-residency-and-surface-layer-topology.md)) are the only trace.

### 2. A dialog that needs durable recall gets its own membership scope, never a thread's

`TranscriptEntry` is a global, immutable fact keyed by id; position lives in per-session membership
(`entry_memberships(session_id, seq)`), which is precisely why the same entry can appear in a fork at a
different position with the same identity. A dialog conversation that must be recallable reuses that
machinery with its **own scope** rather than borrowing a thread's:

```text
scope(dialog:<surface-id>)  ──►  its own membership sequence over entries
thread scope                ──►  unaffected
```

`[INV-DIALOG-02]` — **separate scope, or no persistence.** Dialog content is either ephemeral (default) or
living in a `dialog:`-scoped membership. It is never admitted into a thread's membership, so it can never
appear in that thread's projection, search results, export, or model window.

This capability is *opt-in per dialog* and is not built on speculation: the default is ephemeral, and
scope is added for a dialog whose users demonstrably need recall across reopen.

### 3. Crossing into the thread is an explicit, provenance-stamped operation

The only legal path from a dialog into a conversation is a user act ("insert into this thread" /
"send to thread"). That act is:

1. an **operation**, recorded in the durable command ledger (it is something a human did), and
2. a **dialogue fact** appended through the thread's single writer
   ([ADR-0117](0117-transcript-admission-purity-dialogue-only-thread-documents.md)), carrying
   `InjectionOrigin::Dialog { surface, .. }`.

The model sees an attributed injected note; the transcript shows where it came from; the audit trail shows
who did it and when. `[INV-DIALOG-03]` — **no implicit promotion.** There is no code path that moves
dialog content into a thread without a user-initiated operation.

### 4. The rule is enforced where the mistake would be made

- The dialog surface registry (`DialogView`) exposes receipts through `log()`, so a dialog that wants to
  remember its own answers has a correct, typed destination.
- The architecture test suite asserts that the dialog modules (`shell/surfaces/dialogs*`,
  `shell/overlays/*`) reference no thread-document type and no `SessionStore`/`FactSink` type. The check
  is a property of the module graph, not of individual function bodies, so new dialog code cannot opt out.

## Normative Invariants Established

- `[INV-DIALOG-01]` **No thread handle.** A dialog conversation never holds a session store, a fact sink, or
  a thread's session id. Enforced by: the service's constructor signature and the absence of those fields;
  architecture test over the dialog module tree.
- `[INV-DIALOG-02]` **Sovereign scope.** Durable dialog content lives in a `dialog:`-scoped membership and is
  never admitted to a thread's membership. Enforced by: the persistence API taking an explicit scope, and
  the thread projection reading only its own scope.
- `[INV-DIALOG-03]` **Explicit promotion only.** Content crosses from a dialog into a thread only through a
  user-initiated operation recorded in the ledger, with `InjectionOrigin::Dialog` provenance. Enforced by:
  the absence of any other admission path (`ThreadFact` has no dialog constructor) and a round-trip test
  asserting the provenance survives.

## Consequences

- **Positive**: the "dialog polluted my thread" failure class becomes unreachable rather than discouraged. A
  new dialog author starts from ephemeral scratch and must make an explicit, reviewable decision to obtain
  persistence.
- **Positive**: dialog conversations that *do* need recall gain a first-class home (scoped membership) with
  the same immutability and forking semantics as threads, instead of an improvised store.
- **Negative & trade-offs**: a dialog cannot inherit the session's conversation for free, so follow-ups such
  as "summarise what we were just talking about" must be served from the dialog's own bounded scratch (or
  from retrieval tools that read the store). Mitigation: the scratch cap is generous relative to a dialog's
  working lifetime, and the Archivist already demonstrates retrieval-backed answers over all sessions.
- **Negative & trade-offs**: scoped membership is a persistence capability that must not be reached by
  accident. Mitigation: the scope is a required argument at the persistence boundary (no default), so the
  caller always states which conversation it is writing.

## Rejected Alternatives & Negative Knowledge

### Alternative A (Rejected): run dialogs as turns in the session with a "dialog" flag

- **Why considered**: Cheapest possible implementation — the agent, provider, tools, and store are all
  already wired to the session.
- **Why rejected**: A flag changes presentation, not identity. The messages still enter `model_window` (the
  model then treats a side question as something the user said in this conversation), still occupy rounds,
  and still reach export and search. It also re-creates the `is_command_echo` append-then-filter pattern
  [ADR-0117](0117-transcript-admission-purity-dialogue-only-thread-documents.md) rejects, including its
  live/resume divergence.

### Alternative B (Rejected): fork a child session per dialog activation

- **Why considered**: It is the `/btw` answer, it already works, and it guarantees isolation.
- **Why rejected**: A fork is a *conversation the user is now in* — it mounts a tab, preserves the parent,
  and survives attach. That is the right model for an aside the user chose to pursue, and the wrong model
  for a modal that answers a question and closes. Forking per activation would litter the session registry
  with single-turn children and give every cockpit question a durable identity it does not need.

### Alternative C (Rejected): a shared process-global scratch for all dialogs

- **Why considered**: One buffer, no per-dialog lifecycle, trivial to implement.
- **Why rejected**: It merges unrelated conversations into one context — the same category error as
  sharing the transcript, at a smaller scale. Follow-ups would resolve against another dialog's history, and
  the failure would be intermittent and unattributable.

### Alternative D (Rejected): rely on the dialog to redact its own writes

- **Why considered**: The Archivist already does the right thing by convention; documentation could make
  that convention general.
- **Why rejected**: A convention that a service must remember is exactly the "mechanisms over
  exhortations" anti-pattern the repository forbids. Handing a dialog a `SessionStore` and asking it to
  behave is the capability error at the root of the bug.

## Links

- Related ADRs: [ADR-0116](0116-content-residency-and-surface-layer-topology.md) (layer residency), [ADR-0117](0117-transcript-admission-purity-dialogue-only-thread-documents.md) (transcript admission purity), [ADR-0058](0058-granular-causal-session-forking-sovereign-tab-projection-and-aside-retirement.md) (aside retirement), [ADR-0104](0104-fork-client-projection-reconciliation-and-aside-retirement-completion.md) (fork projection reconciliation), [ADR-0035](0035-domain-scoped-surface-architecture-and-encapsulated-dialog-lifecycle.md) (encapsulated dialog entities), [ADR-0059](0059-canonical-thread-persistence-schema-and-zero-legacy-clean-break.md) (zero legacy burden)
- Code: `crates/nuo-server/src/archivist_service.rs` (the reference implementation), `crates/nuo-protocol/src/transcript.rs` (entry identity and membership), `crates/nuo-persistence/src/db/migrations.rs` (`entry_memberships`)
