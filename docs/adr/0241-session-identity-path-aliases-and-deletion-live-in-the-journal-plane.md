---
id: ADR-0241
title: "Session Identity, Path Aliases, and Deletion Live in the Journal Plane"
status: accepted
date: 2026-11-22
scope: persistence/thread, persistence/journal, persistence/db
superseded_by: null
negative_knowledge: true
---

# 0241. Session Identity, Path Aliases, and Deletion Live in the Journal Plane

- **Status**: Accepted
- **Date**: 2026-11-22
- **Related**: [ADR-0236](0236-the-session-journal-is-the-durable-source-of-truth-and-sqlite-is-a-derived-projection.md),
  [ADR-0239](0239-the-journal-carries-the-branch-graph-and-sqlite-branch-rows-are-derived.md)

---

## Context and Problem Statement

Two identity facts still lived only in SQLite:

1. **Path aliasing.** `ThreadSession::for_path` mapped a legacy snapshot path
   to a session id through `kv_store` (`path:<path>`). Deleting the derived
   database (the ADR-0236 acid test) lost the mapping, so reopening the path
   created a new session even though its journal was intact.
2. **Deletion.** `delete` removed the SQLite row and then removed the journal
   directory. A crash between the two, or a projection queued before the
   delete, could resurrect the session. The in-process tombstone only covered
   the current process lifetime.

## Architectural Decisions

### 1. `[INV-JOURNAL-ID-01]` A path alias is durable in the journal plane

`load_or_seed` resolves a legacy path through
`<journal_root>/aliases/<sha256(path)>.txt` first; the SQLite KV is a fallback
for stores created before this ADR. The alias is written when a path is first
seeded and refreshed whenever a resolved id is read. Deleting `nuo.db*` no
longer changes which session a path names.

### 2. `[INV-JOURNAL-ID-02]` Deletion is a terminal journal frame

`SessionJournal::mark_deleted`:

1. appends a `Deleted` frame and waits for its `fdatasync`;
2. writes a `DELETED` marker into the session directory (atomic rename);
3. removes the segment and snapshot bodies.

Replay stops at a `Deleted` frame and returns `None`; `is_deleted` is true for
the marker or the replay state. `load_or_seed` treats a deleted identity as
**absent** and seeds a fresh session instead of falling back to the derived
row. The process-level tombstone still blocks projections that were queued
before the delete, so a queued `FullRewrite` cannot recreate the row.

### 3. `[INV-JOURNAL-ID-03]` The database owner lock is the journal-root lease

The single-writer contract is the existing `PersistenceHandle` advisory lock
on the database. Every journal write path checks `is_lock_contention` and
returns a read-only error when another process owns the store. A second
process therefore cannot append a journal even though the journal lives beside
the database. Multi-process access would partition sessions by owner; it never
shares a journal.

## Consequences

- `rm -rf nuo.db*` (keeping `journal/`) preserves path identity, deletion, and
  transcript/branch state. This is the acceptance test.
- A deleted id is never served from a stale derived row.
- A crash between the tombstone and the segment cleanup leaves a small
  directory; the marker is the terminal receipt. Re-running deletion reclaims
  the bodies, and the bootstrap catch-up sweeps every `DELETED` directory.
- The alias file is tiny and idempotent; no migration is required for existing
  sessions (the first open writes it).

## Alternatives Considered

| Alternative | Why rejected |
|---|---|
| Keep the alias in SQLite | Deleting the derived view changes identity — the exact failure this ADR removes |
| Remove the journal directory for deletion | A crash before removal resurrects the session; there is no terminal receipt |
| A second lease file under `journal/` | The database owner lock already serializes writers; a second lease only adds a failure mode |

## Negative Knowledge

- **Do not treat "no journal state" as "deleted".** A new session has no
  journal; only a `Deleted` frame or marker is terminal.
- **Do not delete the marker during segment reclamation.** The marker is the
  tombstone, not a cache.
- **Do not write journals without the owner lock.** A read-only client that
  appends would fork the session history.
