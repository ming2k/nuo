//! Execute one migration body inside the migration transaction.
//!
//! Kept in one place so the live migration path and the in-memory expected
//! schema (used by verification) cannot drift (ADR-0189).

use rusqlite::{Connection, Result, Transaction};

use super::catalog::Body;

pub fn execute_body(tx: &Transaction<'_>, body: &Body) -> Result<()> {
    match body {
        Body::Sql(sql) => tx.execute_batch(sql),
        Body::Rust(run) => run(tx),
        Body::SqlThenRust { sql, run } => {
            tx.execute_batch(sql)?;
            run(tx)
        }
    }
}

/// Build the schema a fresh database gets from `migrations`, with no ledger
/// rows. Used only for expected-schema verification.
pub fn build_expected(
    conn: &mut Connection,
    migrations: &[super::catalog::Migration],
) -> Result<()> {
    super::history::ensure_ledger(conn)?;
    let tx = conn.transaction()?;
    for migration in migrations {
        execute_body(&tx, &migration.body)?;
    }
    tx.commit()
}
