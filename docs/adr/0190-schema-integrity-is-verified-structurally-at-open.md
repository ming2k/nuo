---
id: ADR-0190
title: "Schema Integrity Is Verified Structurally at Open"
status: accepted
date: 2026-11-14
scope: persistence, database, governance
superseded_by: null
supersedes: []
negative_knowledge: true
---

# 0190. Schema Integrity Is Verified Structurally at Open

- Status: Accepted
- Date: 2026-11-14

## Context and Problem Statement

`PRAGMA user_version` records which number a database was stamped with, not
whether the DDL that produced the file matches the DDL the current binary
expects. The previous startup guards checked only that `sessions` carried a set
of columns and that `threads`, `thread_policies`, and `workspace_effects`
existed. A database with `user_version = 38` but a missing `threads.origin`
passed those guards and failed later in an arbitrary query. A development build
could also stamp a version without applying all of its DDL.

The migration ledger (ADR-0189) catches checksum drift for migrations that ran,
but the baseline snapshot itself has no per-row checksum to compare against the
live file. The missing invariant is: *the live schema is exactly the schema this
binary's catalog produces.*

## Architectural Decisions

### 1. Compare `sqlite_master` against an in-memory build of the catalog

On every open, the runner builds the catalog's schema in an in-memory SQLite
database, reads `sqlite_master`, and compares it with the live database:

- objects are ordered by `(type, name)` and filtered to `sql IS NOT NULL`
  (SQLite's internal objects are excluded);
- the comparison covers tables, indexes, views, triggers, and virtual-table
  shadow objects;
- SQL text is compared byte-for-byte, because both sides execute the same
  embedded scripts. No normalization can hide a changed `CHECK`, index, or
  trigger.

The expected side is built once per process in a `OnceLock`; the cost is one
in-memory schema build, not one per query.

### 2. Fail with the offending object

A mismatch names the missing, extra, or changed object and includes the
database path. The process refuses to serve; it does not attempt repair. This
turns a class of latent runtime failures into one actionable startup error.

### 3. Constraints are part of the verified schema

The baseline uses `STRICT` tables, `CHECK` constraints, `json_valid` on JSON
columns, foreign keys with explicit delete behavior, and triggers for
append-only evidence. Because the verifier compares `sqlite_master` text, a
database that lost a constraint fails the same gate as one that lost a table.

### 4. Migration-time integrity gates

When this open actually applies migrations, the runner additionally runs:

- `PRAGMA foreign_key_check` after the migration transaction commits;
- `PRAGMA quick_check` after a migration, not on every launch.

Every-open verification is structural; page-level and reference-level checks
run only when the database changed.

## Consequences

- **Positive**: a stamped-but-wrong database cannot reach a query. The v36-style
  failure becomes a startup refusal that names the missing object. Schema
  changes that forget an index or trigger fail the same gate.
- **Negative & Incurred Costs**: every open pays for one in-memory baseline
  build (cached per process) and a `sqlite_master` comparison. Any DDL
  formatting change in a migration is a real schema change; it cannot be
  normalized away. A future migration that intentionally produces different
  text on different SQLite builds would be rejected and must be rewritten.
- **Neutral**: the verifier compares schema, not data. Data invariants remain
  the responsibility of constraints and migration tests.

## Discarded Alternatives (Negative Knowledge)

- **Trust `user_version` plus table-existence guards**: the exact gap that let a
  missing `threads.origin` reach production.
- **Compare only table and column names**: misses changed `CHECK` constraints,
  indexes, triggers, and foreign keys.
- **Normalize SQL text before comparing**: makes whitespace changes pass, but
  also makes a changed constraint or partial-index predicate pass. The catalog
  is the authority; exact text is the cheapest honest comparison.
- **Run `integrity_check` on every open**: O(database size) on the startup path
  for a check that is only meaningful after the file changed.

## Links

- [ADR-0189](0189-immutable-baseline-plus-numbered-migrations.md)
- [ADR-0191](0191-persistence-failure-is-fail-fast-without-a-database-cli.md)
