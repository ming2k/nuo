---
id: ADR-0104
title: "Fork Client Projection Reconciliation and Aside-Retirement Completion"
status: accepted
date: 2026-12-01
scope: persistence/session, wire/protocol, server/nuo-server, presentation/nuo-ui, host/nuo
superseded_by: null
negative_knowledge: true
---

# 0104. Fork Client Projection Reconciliation and Aside-Retirement Completion

- Status: Accepted
- Date: 2026-12-01
- Deciders: Nuo Architecture Working Group
- Amends: [ADR-0058](0058-granular-causal-session-forking-sovereign-tab-projection-and-aside-retirement.md)

---

## Context and Problem Statement

[ADR-0058](0058-granular-causal-session-forking-sovereign-tab-projection-and-aside-retirement.md)
introduced granular causal session forking and retired the `/btw` aside substrate.
Two of its commitments were **not** honoured by the implementation that followed,
and the retirement itself left dead residue.

1. **Sovereign tab projection was overstated.** ADR-0058 §3 promised that a fork
   "mounts the child as a sovereign `ClientTab::Thread` … allowing concurrent
   multi-branch exploration … while preserving the parent session intact". The
   client, however, is a single-attach terminal
   ([ADR-0041](0041-thread-command-purity-viewport-workspace-separation-and-tab-management-topology.md),
   [ADR-0042](0042-client-viewport-routing-ownership-tab-history-and-scene-self-projection.md)):
   one connection hosts exactly one live session, and every tab switch to a
   different thread — `Alt+1..9` included — is implemented as quit-and-reattach.
   There is no concurrent live multi-tab, and there cannot be one without
   re-architecting the client transport.

2. **`/btw` retirement was incomplete (`[INV-DEPR-01]`).** ADR-0058 §4 required
   intercepting a submitted `/btw <prompt>` and steering it onto `/fork` with
   educational guidance. The aside wire events, registry, and overlay were removed,
   but the redirect itself was never written: `/btw` simply returned
   `Unknown command`.

Additionally, the retirement left dead residue — `'aside'` fork-kind match arms in
persistence, a `TimelineKind::Aside` variant, and a `routes_to_side` translation
scaffold with an orphaned `ChromeEdit::TransportSetback` mutation — a latent
violation of the zero-legacy clean break established by
[ADR-0059](0059-canonical-thread-persistence-schema-and-zero-legacy-clean-break.md).

## Decision Drivers

- **Epistemic honesty**: an *accepted* ADR must describe the shipped system. A
  stale promise is a latent defect that misleads every downstream reader.
- **`[INV-FORK-04]` restated**: forking MUST NOT replace or destroy the parent
  session; the parent stays fully operational.
- **`[INV-DEPR-01]` completion**: retired `/btw` habits MUST be steered onto
  `/fork`, never dead-ended.
- **Zero legacy burden** (ADR-0059): retired-concept residue is purged, never
  defensively shimmed.

## Architectural Decisions

### 1. A fork is a server-side sovereign branch plus a switchable tab descriptor

A fork creates a new child session record (fresh id, `parent_id`,
`fork_kind = Fork`) and slices the child's transcript and causal path at the
target, **leaving the parent untouched** — the parent IR is never rewritten.
It is announced by `AgentResponse::SessionForked`. The client mounts a
`ClientTab::Thread` descriptor and preserves the tab stack across a re-attach;
switching to any thread (parent or child) is the same quit-and-reattach the
single-attach client already uses. This restates `[INV-FORK-04]` as *durable
parent preservation plus tab-descriptor persistence* — the strongest guarantee
the shipped architecture can make — and retires the unreachable "concurrent
multi-branch exploration" phrasing.

### 2. Retired `/btw` steers into a real fork

A submitted `/btw <prompt>` (with its `/aside` / `/side` synonyms) is intercepted
in the slash dispatcher: the server forks at head and carries the message into
the new branch through `AgentResponse::SessionForked.prompt`, which the client
submits once it has re-attached to the child. Command completion steers
`btw`/`aside`/`side` onto `/fork`. `[INV-DEPR-01]` is thereby complete.

### 3. Turn-granular transcripts, round-granular IR

The transcript is the message SSOT and is sliced exactly at the requested
round/turn boundary (`[INV-FORK-03]`). The canonical `SessionIR` is the
causal/compaction substrate and has no turn resolution, so a fork anchors the
child IR at the exact transcript-cutoff node when the IR already holds it, and
otherwise at the round-level leaf. An out-of-range `/fork <round>.<turn>` fails
with a precise error rather than being silently clamped to the round boundary.

### 4. Aside residue purged

The `'aside'` fork-kind match arms (persistence), the `TimelineKind::Aside`
variant (protocol), and the `routes_to_side` translation scaffold with its
orphaned `ChromeEdit::TransportSetback` mutation are deleted.

## Consequences

- **Positive**: The fork contract matches the shipped single-attach client;
  `/btw` muscle memory lands in a working branch; the aside residue is gone.
- **Negative & Trade-offs**: "Concurrent multi-branch exploration" remains
  unavailable — it is deferred pending a client transport that can host several
  live sessions. This ADR records that limitation rather than pretending
  otherwise.

## Rejected Alternatives & Negative Knowledge

### Alternative A: Implement true concurrent multi-tab (Rejected)

- **Why considered**: It is the literal reading of ADR-0058 §3.
- **Why rejected**: The client transport binds one connection to one session; a
  faithful implementation is a multi-session client re-architecture, out of scope
  for a reconciliation, and it would break the single-focus scene model of
  ADR-0041/ADR-0042.

### Alternative B: Keep the silent turn clamp (Rejected)

- **Why rejected**: Silent clamping makes `/fork 2.9` on a two-turn round quietly
  produce a different branch than requested. Failing closed with the real turn
  count is the honest contract.

## Links

- Related ADRs:
  [ADR-0058](0058-granular-causal-session-forking-sovereign-tab-projection-and-aside-retirement.md),
  [ADR-0059](0059-canonical-thread-persistence-schema-and-zero-legacy-clean-break.md),
  [ADR-0041](0041-thread-command-purity-viewport-workspace-separation-and-tab-management-topology.md),
  [ADR-0042](0042-client-viewport-routing-ownership-tab-history-and-scene-self-projection.md),
  [ADR-0103](0103-presentation-substrate-purification-nuo-ui-extraction-and-nuo-tui-retirement.md)
