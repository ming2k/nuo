---
id: ADR-0189
title: "The Durable Schema Is an Immutable Baseline Plus Numbered Migrations"
status: accepted
date: 2026-11-14
scope: persistence, database, governance
superseded_by: null
supersedes: []
negative_knowledge: true
---

# 0189. The Durable Schema Is an Immutable Baseline Plus Numbered Migrations

- Status: Accepted
- Date: 2026-11-14

## Context and Problem Statement

The database carried a 38-step migration chain that replayed on every fresh
database. Most entries had `sql: ""`; their real work lived in version-keyed
Rust functions spread across four files and dispatched by an
`if migration.version == N` ladder in the runner. The chain accumulated
conditional repairs for development builds, retired tables, and interim
schemas. It had no per-step history and no checksum stored in the database:
`PRAGMA user_version` was the only durable state.

The failure mode is concrete. Migration 36 called the current runtime loader,
which selected `threads.origin` (created later by migration 38) and decoded
`FactPayload` rows the migration had not written yet. Every database upgraded
from v0.0.86 with at least one session rolled back with
`no such column: t.origin` and stayed at v33. The compile-time catalog
fingerprint did not catch it because it covered only `version + sql`, and
migration 36's `sql` was empty.

## Architectural Decisions

### 1. One immutable baseline, then numbered changes

`crates/nuo-persistence/migrations/0001_baseline.sql` is the whole schema at
version 1. It is immutable: a schema change is a new numbered file beside it,
never an edit to the baseline. The edit is detected by the ledger checksum and
by `baseline_checksum_is_stable`.

A migration is a first-class value with a version, a stable name, a manual
`code_revision` for Rust bodies, and one of three bodies:

- `Sql(&'static str)` — embedded at compile time with `include_str!`;
- `Rust(fn(&Transaction) -> Result<()>)` — data migration;
- `SqlThenRust` — DDL followed by a data pass in the same transaction.

There is no version dispatch ladder. The catalog is the single place where a
migration's identity and body meet.

### 2. Fresh databases apply the baseline only

A database with `user_version = 0` and no schema objects receives
`0001_baseline.sql` directly. The retired 38-step history is deleted from the
runtime, not replayed. This removes obsolete DDL, interim repairs, and the
possibility that a new user inherits a historical migration bug.

### 3. Pre-baseline databases are refused, not migrated

This release is a clean break. A database stamped by the retired chain has no
`schema_migrations` ledger; the runner refuses it with an error that names the
database path and tells the user to move the file aside. No conversion, no
best-effort replay, no silent data loss. `nuo` has no `db` administration
subcommand; recovery is file-level backup/restore (ADR-0191).

### 4. The ledger is authoritative; `user_version` is the fast gate

`schema_migrations(version, name, checksum, applied_at_ms, execution_ms)` is
written inside the same transaction as the migration and the `user_version`
stamp. On open:

- applied versions must be an exact prefix of the catalog;
- every checksum must match the compiled body;
- `user_version` must equal the ledger's last version;
- a version newer than the binary is refused.

A rolled-back migration leaves neither a ledger row nor a version stamp. There
is no `success = 0` archaeology: SQLite DDL is transactional.

### 5. The baseline is STRICT and constraint-bearing

Every regular table is `STRICT`; JSON columns carry `json_valid` checks;
booleans are `INTEGER CHECK (x IN (0,1))`; foreign keys cascade where the row
belongs to a thread. The baseline also drops dead scaffolding (`thread_revisions`,
`asset_attestations`, unused thread/policy columns) and standardizes fact and
request-projection scope on `thread_id`. Details of the structural gate are in
ADR-0190.

### 6. Adding a migration

1. Add `migrations/000N_<name>.sql` (or a Rust body).
2. Append a `Migration` with `version = N`, a stable `name`, and
   `code_revision = 1`.
3. Bump `CURRENT_DB_VERSION`.
4. Add an upgrade test from the previous baseline plus any data fixtures.
5. Never edit `0001_baseline.sql` or an already-shipped migration.

## Consequences

- **Positive**: one code path for fresh and upgraded databases; no replay of
  retired DDL; checksum drift and stamped-but-wrong databases fail loudly; the
  catalog is small enough to review.
- **Negative & Incurred Costs**: existing pre-baseline databases must be moved
  aside or exported by the user. The 38-step history remains only in git
  history. Future data migrations still require Rust bodies and manual
  `code_revision` discipline.
- **Neutral**: `user_version` and the ledger duplicate one integer. The ledger is
  authoritative; the pragma is an O(1) gate, and disagreement is a hard error.

## Discarded Alternatives (Negative Knowledge)

- **Keep replaying the full history for new databases**: obsolete DDL remains
  executable and a broken historical step still reaches new users.
- **Keep the `if version == N` dispatch**: it is implicit, non-exhaustive, and
  separates a migration's identity from its body.
- **Auto-migrate or best-effort convert pre-baseline databases**: the retired
  chain's semantics are exactly what this ADR retires; a converter would keep
  the baggage alive under a new name.
- **Adopt Flyway (the Java tool)**: the migration bodies are Rust data
  transformations, not SQL files; a JVM tool would split schema ownership and
  add a second writer beside the embedded single-writer door.
- **Add `nuo db status/check/repair`**: users run an application, not a database
  administration suite. Fail-fast errors plus file-level backup are sufficient
  (ADR-0191).

## Links

- [ADR-0173](0173-thread-is-the-durable-ssot-branch-is-a-cursor.md)
- [ADR-0174](0174-thread-command-receipts-fencing-leases-and-blob-liveness-share-one-durable-door.md)
- [ADR-0190](0190-schema-integrity-is-verified-structurally-at-open.md)
- [ADR-0191](0191-persistence-failure-is-fail-fast-without-a-database-cli.md)
