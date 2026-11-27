//! Structural verification: a live database must have exactly the schema its
//! catalog produces (ADR-0190).
//!
//! `PRAGMA user_version` only says which number was stamped. It cannot tell
//! whether the DDL that produced the file matches the DDL this binary expects,
//! so a database stamped by a different build (or edited by hand) can pass the
//! version gate and fail later in a random query. Comparing `sqlite_master`
//! against an in-memory build of the catalog closes that gap.

use rusqlite::{Connection, Result};

use super::apply::build_expected;
use super::history::incompatible;
use super::schema::{Schema, SchemaObject};

pub fn schema_objects(conn: &Connection) -> Result<Vec<SchemaObject>> {
    let mut stmt = conn.prepare(
        "SELECT type, name, sql
           FROM sqlite_master
          WHERE sql IS NOT NULL
            AND name NOT LIKE 'sqlite_%'
          ORDER BY type ASC, name ASC",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(SchemaObject {
            kind: row.get(0)?,
            name: row.get(1)?,
            sql: row.get(2)?,
        })
    })?;
    rows.collect()
}

fn expected(schema: &Schema) -> Result<&[SchemaObject], String> {
    match schema.expected.get_or_init(|| {
        let mut conn = Connection::open_in_memory().map_err(|error| error.to_string())?;
        build_expected(&mut conn, schema.migrations).map_err(|error| error.to_string())?;
        schema_objects(&conn).map_err(|error| error.to_string())
    }) {
        Ok(objects) => Ok(objects),
        Err(error) => Err(error.clone()),
    }
}

/// The live schema must be byte-for-byte the catalog's schema. Object order is
/// normalized (`type`, `name`); SQL text is not normalized because both sides
/// execute the same embedded scripts.
pub fn verify(conn: &Connection, schema: &Schema) -> Result<()> {
    let actual = schema_objects(conn)?;
    let expected = expected(schema).map_err(incompatible)?;

    if actual == expected {
        return Ok(());
    }

    let missing: Vec<&str> = expected
        .iter()
        .filter(|object| {
            !actual
                .iter()
                .any(|candidate| same_identity(candidate, object))
        })
        .map(|object| object.name.as_str())
        .collect();
    if !missing.is_empty() {
        return Err(incompatible(format!(
            "{}: database is missing schema object(s) {} that this binary's catalog creates",
            schema.name,
            missing.join(", ")
        )));
    }

    let extra: Vec<&str> = actual
        .iter()
        .filter(|object| {
            !expected
                .iter()
                .any(|candidate| same_identity(candidate, object))
        })
        .map(|object| object.name.as_str())
        .collect();
    if !extra.is_empty() {
        return Err(incompatible(format!(
            "{}: database contains schema object(s) {} that this binary's catalog does not create",
            schema.name,
            extra.join(", ")
        )));
    }

    let changed = actual
        .iter()
        .zip(expected)
        .find(|(left, right)| left != right)
        .map(|(left, right)| {
            format!(
                "{}: {} {}: database SQL differs from catalog SQL",
                schema.name, left.kind, right.name
            )
        })
        .unwrap_or_else(|| format!("{}: schema object order/content differs", schema.name));
    Err(incompatible(changed))
}

fn same_identity(left: &SchemaObject, right: &SchemaObject) -> bool {
    left.kind == right.kind && left.name == right.name
}
