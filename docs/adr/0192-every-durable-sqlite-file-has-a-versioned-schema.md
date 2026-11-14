---
id: ADR-0192
title: "Every Durable SQLite File Has a Versioned Schema"
status: accepted
date: 2026-11-14
scope: persistence, database, governance
superseded_by: null
supersedes: []
negative_knowledge: true
---

# 0192. Every Durable SQLite File Has a Versioned Schema

- Status: Accepted
- Date: 2026-11-14

## Context and Problem Statement

The database rewrite initially covered `nuo.db` only. `role_memory.db` opened
its own connection and created its tables with `CREATE TABLE IF NOT EXISTS` and
`CREATE INDEX IF NOT EXISTS` on every open. That is the same unversioned model
the main database just retired: a schema change is an edit to live DDL, there
is no ledger, no checksum, and no way to tell whether a file matches the binary.
A second durable database with a different evolution path is architecture rot
by construction.

## Architectural Decisions

### 1. The migration runner is schema-agnostic

A `Schema` value owns:

- a stable name (`nuo.db`, `role_memory.db`);
- an immutable migration catalog;
- the current version;
- a per-process cache of the expected `sqlite_master` objects.

The runner, ledger, checksum, backup, and structural verifier take `&Schema`.
There is no second migration implementation for secondary databases.

### 2. `role_memory.db` has its own baseline

`migrations/role_memory/0001_baseline.sql` is the complete role-memory schema:
`role_memories`, its indexes, the FTS5 table, and the FTS triggers. It is STRICT
and constraint-bearing like the main baseline. The catalog lives in
`db/migrations/role_memory.rs`; `RoleMemoryStore` calls `initialize_schema` with
`ROLE_MEMORY_SCHEMA` instead of executing ad-hoc DDL.

### 3. The clean-break policy applies to every durable file

A pre-baseline `role_memory.db` has no `schema_migrations` ledger and is refused
with a path-bearing error, exactly like a pre-baseline `nuo.db`. It is not
silently recreated or converted. Role memory is a durable store, not a
disposable cache, so the refusal is honest.

### 4. Adding a schema change to a secondary database

The same rules as the main database apply: add a numbered SQL/Rust migration to
that schema's catalog, bump its current version, add an upgrade test, never edit
its baseline. Checksums and structural verification are per schema.

## Consequences

- **Positive**: one migration contract for every durable SQLite file; no
  `CREATE TABLE IF NOT EXISTS` drift; a role-memory schema change is reviewed
  and tested like any other.
- **Negative & Incurred Costs**: every durable file now needs a baseline,
  catalog, and tests. Pre-baseline secondary files are refused.
- **Neutral**: each file has its own ledger and version number; they do not need
  to share a version sequence.

## Discarded Alternatives (Negative Knowledge)

- **Leave `role_memory.db` on `CREATE TABLE IF NOT EXISTS`**: the exact rot this
  ADR retires, just in a smaller file.
- **Merge role memory into `nuo.db`**: changes the lifetime and locking of a
  derived dialogue store for no schema benefit.
- **Treat role memory as a disposable cache and delete it on mismatch**:
  silently destroys user data; a clear refusal is safer.
- **Copy the migration runner for the second database**: two implementations
  drift. The runner takes a `Schema`, not a copy.

## Links

- [ADR-0189](0189-immutable-baseline-plus-numbered-migrations.md)
- [ADR-0190](0190-schema-integrity-is-verified-structurally-at-open.md)
- [ADR-0191](0191-persistence-failure-is-fail-fast-without-a-database-cli.md)
