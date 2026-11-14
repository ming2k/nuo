# Database migrations

The durable schema is an immutable baseline plus numbered migrations
(ADR-0189). This directory is the source of truth for the schema; the runtime
catalog is in `src/db/migrations/catalog.rs`.

## Layout

```text
crates/nuo-persistence/
  migrations/
    0001_baseline.sql        # immutable snapshot of the whole schema (v1)
    README.md
  src/db/migrations/
    catalog.rs               # MIGRATIONS + CURRENT_DB_VERSION + Body
    runner.rs                # open, apply, stamp, integrity gates
    history.rs               # schema_migrations ledger + validation
    verify.rs                # live sqlite_master vs in-memory catalog
    apply.rs                 # execute one migration body
```

## The one rule

`0001_baseline.sql` and every already-shipped migration are immutable. A schema
change is a new numbered file beside them. The ledger records a checksum; an
edit to a shipped migration is detected at the next open and refuses the
database (ADR-0190).

## Adding a schema-only migration

The catalog currently ends at v2 (`drop_derived_thread_columns`). The next
change is v3. Example: add an index for thread title search.

1. Create `migrations/0003_thread_title_index.sql`:

```sql
CREATE INDEX idx_threads_title ON threads(title);
```

2. Append to `MIGRATIONS` in `src/db/migrations/catalog.rs`:

```rust
pub const MIGRATIONS: &[Migration] = &[
    // ... existing v1 and v2 entries ...
    Migration {
        version: 3,
        name: "thread_title_index",
        code_revision: 1,
        body: Body::Sql(include_str!("../../../migrations/0003_thread_title_index.sql")),
    },
];

pub const CURRENT_DB_VERSION: u32 = 3;
```

3. Add an upgrade test (see below).

4. Update `CHANGELOG.md`.

The next app start applies v2 automatically. There is no `nuo db migrate`
command (ADR-0191).

## Adding a data migration

Use a Rust body when the change needs typed decoding, table rebuilds, or data
transformation:

```rust
// src/db/migrations/data_0003.rs
use rusqlite::{Result, Transaction};

pub fn run(tx: &Transaction<'_>) -> Result<()> {
    // Raw, version-scoped SQL only. Do not call runtime loaders or stores:
    // they evolve with the live schema and may assume columns that do not
    // exist at this version (the v36 failure mode).
    tx.execute_batch("UPDATE threads SET title = '' WHERE title IS NULL;")?;
    Ok(())
}
```

Catalog entry:

```rust
mod data_0003;

Migration {
    version: 3,
    name: "thread_title_not_null",
    code_revision: 1,
    body: Body::SqlThenRust {
        sql: include_str!("../../../migrations/0003_thread_title_not_null.sql"),
        run: data_0003::run,
    },
}
```

Rules for the Rust body:

- It receives the migration transaction; do not open another connection.
- Use only tables/columns that exist **at that version**.
- Never call `ThreadSession`, `thread_store`, or any runtime loader. Those are
  current-schema readers, not historical readers.
- Keep it deterministic and idempotent where practical.
- If the body changes after release, bump `code_revision`; that changes the
  checksum and forces a new version. Never silently change behavior.

## Tests every migration must add

1. **Catalog continuity** — `migration_catalog_is_contiguous` already asserts
   `1..=CURRENT_DB_VERSION` with no gaps; keep it passing.
2. **Fresh database** — open an in-memory database and assert the new shape
   (`db/tests/migration_scenarios.rs`).
3. **Upgrade from the previous version** — construct a database at
   `CURRENT_DB_VERSION - 1`, insert representative rows, run `migrate_schema`,
   assert both schema and data. A checked-in fixture or a catalog-prefix builder
   is acceptable; do not duplicate the previous DDL by hand.
4. **Negative checks** — if the migration introduces a constraint, test that a
   violating write is refused.

Run:

```bash
cargo test -p nuo-persistence --lib
cargo test -p nuo-tests --test integration
```

## What the runner does on open

1. Reads `PRAGMA user_version` and the `schema_migrations` ledger.
2. A database with `user_version = 0` and no objects gets the baseline.
3. A pre-baseline database (old 38-step chain, no ledger) is refused with a
   path-bearing error. Move `nuo.db`, `nuo.db-wal`, and `nuo.db-shm` aside to
   start fresh. The same applies to a pre-baseline `role_memory.db` (and its
   WAL files): it has no ledger and is refused rather than recreated.
4. An existing database validates every applied checksum and the ledger
   prefix.
5. Before any pending migration on an existing file, the runner writes a
   `VACUUM INTO` snapshot named `<db>.pre-v<N>.<timestamp>.bak`. A failed
   snapshot aborts the upgrade.
6. Pending migrations run in one `BEGIN IMMEDIATE` transaction; each writes its
   ledger row and `user_version` stamp in that same transaction.
7. `foreign_key_check` runs after commit; `quick_check` runs only when
   migrations were applied.
8. `sqlite_master` is compared with an in-memory build of the catalog. Any
   missing, extra, or changed object refuses the open and names the object and
   database path.

## Secondary durable databases

The runner is schema-agnostic. `role_memory.db` has its own baseline
(`migrations/role_memory/0001_baseline.sql`) and catalog
(`src/db/migrations/role_memory.rs`), and goes through the same ledger,
checksum, backup, and structural verification (ADR-0192). Do not add
`CREATE TABLE IF NOT EXISTS` to a durable store; add a numbered migration to
that schema's catalog.

## Do not

- Edit `0001_baseline.sql` or a shipped migration.
- Add a migration with a version lower than `CURRENT_DB_VERSION`; it will never
  run on an existing database.
- Call runtime data-access helpers from a migration body.
- Add a `nuo db` administration command. There is none by design.
- Auto-convert a pre-baseline database. The clean break is intentional.
