---
id: ADR-0250
title: "Drafts Are Server-Scoped and Reconciled Against the Catalog"
status: accepted
date: 2026-11-24
scope: protocol/events, server/handlers, tui/nuo, persistence/client-state
superseded_by: null
negative_knowledge: true
---

# 0250. Drafts Are Server-Scoped and Reconciled Against the Catalog

- **Status**: Accepted
- **Date**: 2026-11-24
- **Amends**: [ADR-0247](0247-a-draft-belongs-to-a-thread-and-outlives-every-view-transition.md) (replaces "accepted residue" for externally deleted threads with deterministic catalog reconciliation and server-scoped storage)

---

## Context and Problem Statement

ADR-0247 stored unsent composer drafts as one JSON file per thread under `$XDG_STATE_HOME/nuo/drafts/`. Because drafts can contain staged base64 images (`pending_images`), each orphaned file left behind by a non-client deletion (`nuo thread rm`, another machine, or a reset dev server) can be megabytes of permanent disk leakage.

Furthermore, a flat `drafts/` directory did not scope drafts by server identity: connecting to different servers mixed unrelated thread IDs in the same namespace and prevented safe reconciliation.

---

## Architectural Decisions

### 1. `[INV-DRAFT-06]` Draft storage is partitioned by server identity

Each server endpoint / instance maps to a sanitized, hash-disambiguated subdirectory:

```text
$XDG_STATE_HOME/nuo/drafts/<server-scope>/<readable-thread-id>-<sha256(id)[..8]>.json
```

The flat legacy layout is not migrated; clean break under the scoped directory.

### 2. `[INV-DRAFT-07]` Deterministic catalog reconciliation on connect (no TTL guesswork)

The client does not enumerate all server threads. Instead, on attach/connect, if the local ledger for the active server scope holds `K` parked draft thread IDs (excluding the currently active thread), it sends a single batch request:

- `AgentRequest::CheckThreadsExist { thread_ids: Vec<String> }`
- `AgentResponse::ThreadsExistence { missing_thread_ids: Vec<String> }`

The server resolves `thread_ids` via a pure catalog primary-key check (`O(K)`), returning only IDs that do not exist in the catalog. The client authoritatively `forget`s and `unlink`s each confirmed-missing draft.

### 3. `[INV-DRAFT-08]` Live deletion broadcast

When a thread is deleted on the server, the server broadcasts `AgentResponse::ThreadDeleted { thread_id }` so any connected client immediately forgets and unlinks that thread's draft without waiting for reconnect.

---

## Discarded Alternatives (Negative Knowledge)

- **Accept permanent orphan files ("accepted residue").** Unbounded disk leak, especially with multi-megabyte staged image attachments.
- **Time-based TTL expiry.** Silently destroys drafts the user intentionally left open for days.
- **Full client-side enumeration of all server threads.** Scales with server catalog size rather than the client's tiny local draft set `K`.
