//! Schema catalog for the role dialogue memory database (ADR-0192).
//!
//! `role_memory.db` is a second durable SQLite file. It gets the same
//! baseline/ledger/verification contract as `nuo.db`, with its own catalog.

use super::catalog::{Body, Migration};
use super::schema::Schema;

pub const CURRENT_DB_VERSION: u32 = 1;

pub const MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    name: "baseline",
    code_revision: 1,
    body: Body::Sql(include_str!(
        "../../../migrations/role_memory/0001_baseline.sql"
    )),
}];

pub static ROLE_MEMORY_SCHEMA: Schema =
    Schema::new("role_memory.db", MIGRATIONS, CURRENT_DB_VERSION);

#[cfg(test)]
pub const BASELINE_CHECKSUM: &str =
    "4aaca9275e6b426d38bf802edc7fd4128c65e791a5c772912569ab99d6c67e2c";
