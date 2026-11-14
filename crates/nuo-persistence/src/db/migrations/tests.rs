//! Migration body and checksum tests.
//!
//! These exercise the pieces a normal open cannot: the Rust and
//! SQL-then-Rust bodies, rollback of a failing body, and the checksum's
//! sensitivity to code revisions.

use rusqlite::Connection;

use super::apply::execute_body;
use super::catalog::{Body, Migration};
use super::runner::table_exists;
use super::verify;

#[test]
fn rust_body_runs_inside_the_migration_transaction() {
    let mut conn = Connection::open_in_memory().unwrap();
    let tx = conn.transaction().unwrap();
    execute_body(
        &tx,
        &Body::Rust(|tx| {
            tx.execute_batch("CREATE TABLE rust_body_probe(id INTEGER) STRICT;")?;
            Ok(())
        }),
    )
    .unwrap();
    tx.commit().unwrap();

    assert!(table_exists(&conn, "rust_body_probe").unwrap());
}

#[test]
fn sql_then_rust_runs_both_in_order() {
    let mut conn = Connection::open_in_memory().unwrap();
    let tx = conn.transaction().unwrap();
    execute_body(
        &tx,
        &Body::SqlThenRust {
            sql: "CREATE TABLE ordered_probe(id INTEGER) STRICT;",
            run: |tx| {
                tx.execute("INSERT INTO ordered_probe(id) VALUES(7)", [])?;
                Ok(())
            },
        },
    )
    .unwrap();
    tx.commit().unwrap();

    let value: i64 = conn
        .query_row("SELECT id FROM ordered_probe", [], |row| row.get(0))
        .unwrap();
    assert_eq!(value, 7);
}

#[test]
fn a_failing_body_rolls_back_its_sql() {
    let mut conn = Connection::open_in_memory().unwrap();
    let tx = conn.transaction().unwrap();
    let result = execute_body(
        &tx,
        &Body::SqlThenRust {
            sql: "CREATE TABLE rollback_probe(id INTEGER) STRICT;",
            run: |tx| {
                tx.execute("INSERT INTO missing_table VALUES(1)", [])?;
                Ok(())
            },
        },
    );
    assert!(result.is_err());
    drop(tx);

    assert!(!table_exists(&conn, "rollback_probe").unwrap());
}

#[test]
fn checksum_covers_the_manual_rust_revision() {
    let first = Migration {
        version: 2,
        name: "same-name",
        code_revision: 1,
        body: Body::Rust(|_| Ok(())),
    };
    let second = Migration {
        version: 2,
        name: "same-name",
        code_revision: 2,
        body: Body::Rust(|_| Ok(())),
    };
    assert_ne!(first.checksum(), second.checksum());
}

#[test]
fn checksum_covers_sql_bytes() {
    let first = Migration {
        version: 2,
        name: "sql",
        code_revision: 1,
        body: Body::Sql("CREATE TABLE a(id INTEGER) STRICT;"),
    };
    let second = Migration {
        version: 2,
        name: "sql",
        code_revision: 1,
        body: Body::Sql("CREATE TABLE a(id INTEGER NOT NULL) STRICT;"),
    };
    assert_ne!(first.checksum(), second.checksum());
}

#[test]
fn a_baseline_database_matches_the_expected_schema() {
    let conn = super::runner::open_in_memory_for_tests().unwrap();
    verify::verify(&conn).unwrap();
}
