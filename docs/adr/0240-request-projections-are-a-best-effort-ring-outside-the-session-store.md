---
id: ADR-0240
title: "Request Projections Are a Best-Effort Ring Outside the Session Store"
status: accepted
date: 2026-11-22
scope: persistence/forensics, persistence/db, engine/cognition/agent, server/runtime
superseded_by: null
negative_knowledge: true
---

# 0240. Request Projections Are a Best-Effort Ring Outside the Session Store

- **Status**: Accepted
- **Date**: 2026-11-22
- **Related**: [ADR-0236](0236-the-session-journal-is-the-durable-source-of-truth-and-sqlite-is-a-derived-projection.md),
  [ADR-0238](0238-configuration-and-usage-are-process-snapshots-never-per-request-disk-reads.md)

---

## Context and Problem Statement

The request-projection archive lived in SQLite (`request_projections`), with a
foreign key to `threads(id)` and a per-request insert. Two problems followed:

1. A brand-new session's first request could be assembled before the derived
   `threads` row existed; the fire-and-forget insert then failed the foreign
   key and was logged as a lost record.
2. Forensic evidence shared a transaction and a writer with session state. It
   is not session state: it is bounded, optional, and allowed to be lost.

## Architectural Decisions

### 1. `[INV-FORENSICS-01]` The archive never touches the dispatch path

`ThreadSession::record_request_projection` and
`try_record_request_projection` append to
`<journal_root>/<session>/forensics.jsonl` through one process-wide writer
thread with a bounded queue. Neither awaits SQLite, a session row, or a
projection flush. A full queue drops the record and increments a debug log.

### 2. `[INV-FORENSICS-02]` The ring is bounded and lossy by design

- Files: `forensics.jsonl` and `forensics.1.jsonl` (one rotation).
- Rotation: at 8 MiB the current file replaces the archive; two files bound a
  session at ~16 MiB.
- Reads dedupe `(round, turn)` keeping the last capture, then take the newest
  64 (`MAX_RETAINED_REQUEST_PROJECTIONS`). This matches the retired table's
  `INSERT OR REPLACE` + retention ring semantics.
- No `fsync`: a crash may lose the last records. That is the contract; the
  archive is evidence, not a ledger.

### 3. `[INV-FORENSICS-03]` The table is retired, not shadowed

Migration `0004_drop_request_projections.sql` drops the table and its index;
`CURRENT_DB_VERSION` becomes 4. `PersistenceCommand::RecordRequestProjection`,
the handle methods, the usage-projection insert/load helpers, and the reader
method are deleted. There is no dual-write and no legacy read.

## Consequences

- A fresh session records its first request without any row existing.
- No foreign key, no request-path writer wakeup, no projection ordering
  dependency.
- The SQLite schema and the write surface both shrink.
- `/usage`-style forensic readers tail the ring; a missing record is an
  expected state after a crash or retention, never an error.

## Alternatives Considered

| Alternative | Why rejected |
|---|---|
| Keep the table and flush the projection first | Couples forensics to session-row ordering and reintroduces a request-path wait |
| Store projections in the journal itself | They are per-request and huge; they would inflate turn frames and the replay surface |
| Drop the feature entirely | The evidence is useful for request debugging; a bounded ring keeps it at near-zero cost |

## Negative Knowledge

- **Do not make the ring durable.** The moment it needs an fsync or an ack it
  belongs on the critical path and the original problem returns.
- **Do not put it in the derived SQLite view without a separate projector.**
  The source path must not depend on the view.
- **Do not keep the table "just in case".** A retired table is a second
  source of truth waiting to be read.
