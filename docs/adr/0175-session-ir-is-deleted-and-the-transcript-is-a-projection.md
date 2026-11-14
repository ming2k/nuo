---
id: ADR-0175
title: "SessionIR Is Deleted and the Transcript Is a Projection"
status: accepted
date: 2026-11-13
scope: protocol/session-ir, persistence/thread, persistence/db, engine/compaction, server/offstream
superseded_by: null
supersedes: []
negative_knowledge: true
---

# 0175. SessionIR Is Deleted and the Transcript Is a Projection

- Status: Accepted
- Date: 2026-11-13
- Related: [RFC-0003](../rfc/0003-thread-branch-fact-and-request-assembly.md), [ADR-0173](0173-thread-is-the-durable-ssot-branch-is-a-cursor.md), [ADR-0174](0174-thread-command-receipts-fencing-leases-and-blob-liveness-share-one-durable-door.md)

## Context and Problem Statement

RFC-0003 requires one durable conversation model. The tree still held two:
`SessionIR` (a second request compiler and causal graph) and a transcript
(`entries` + `entry_memberships`) that acted as the durable message SSOT.
Compaction read the IR while the live path wrote the transcript, so the two
could hold different conversations. Keeping either as an alias would preserve
the split brain.

## Architectural Decisions

### 1. `SessionIR` and its compiler are deleted

The module, its `CausalNode` / `NodePayload` / `SessionDelta` types, and
`compile_session_request` are removed. `RequestView -> RequestBuilder` is the
only conversation-to-request path. The compaction engine now operates directly
on `Thread` facts.

### 2. Facts are the only durable conversation rows

`causal_nodes` stores `ThreadFact` payloads; `thread_branches` stores cursors;
`thread_policies.thread_policy_json` stores the aggregate policy; `thread_blobs`
and `thread_blob_refs` store CAS bytes and the reference ledger. Migration 36
converts every existing transcript into facts and then drops `entries`,
`entry_memberships`, `projections`, `blob_refs`, and `fts_entries`.

### 3. The transcript is an in-memory per-branch projection

`Transcript` remains only as a pure view type rebuilt from a thread branch
(`transcript_from_thread`). It is written for compatibility with UI/export code
but is never the durable source: a save derives facts from the thread, and a
load projects them back. `SessionStore` is renamed `ThreadSession`; the
`nuo-persistence::session` module is `nuo-persistence::thread`.

### 4. FTS is derived from facts

`fts_thread_facts` replaces `fts_entries` and is populated from the facts in the
same transaction. History search reads it; there is no trigger on a retired
transcript table.

### 5. Working state is not a conversation fact

Todos are stored on `thread_policies.todos_json` (migration 37). They survive a
reload without becoming a fact or a transcript row.

### 6. Server branch control is explicit

`ControlRequest::{ForkBranch, ListBranches, SwitchBranch, DeleteBranch}` map to
`ThreadSession` cursor operations. `/fork` creates and switches to a branch
cursor; the old child-thread copy path, `ForkPoint`, and
`ThreadForkKind::Fork` are deleted. A subagent remains a new `Thread` with its
own facts and lifecycle.

## Consequences

- **Positive**: One durable truth; compaction, request assembly, and UI all read
  the same facts. Two whole layers and their drift are gone.
- **Negative & Incurred Costs**: Migration 36 rewrites every existing thread
  once. The transcript projection still allocates a message window per session
  view; it will be narrowed to `TranscriptView` pages as client pagination
  lands.

## Discarded Alternatives (Negative Knowledge)

- **Keep `SessionIR` as a read-only alias**: it is still a second compiler and a
  second conversation shape.
- **Keep transcript rows as a cache beside facts**: two durable writers can
  disagree; the FTS index is the only derived row allowed.
- **Rename the module only**: names do not remove a write path.

## Links

- [RFC-0003](../rfc/0003-thread-branch-fact-and-request-assembly.md)
- [ADR-0173](0173-thread-is-the-durable-ssot-branch-is-a-cursor.md)
- [ADR-0174](0174-thread-command-receipts-fencing-leases-and-blob-liveness-share-one-durable-door.md)
