//! Schema catalog for fixed-K rebuildable content shards (ADR-0242, ADR-0255).
//!
//! Every content shard (`shards/content-XX.db`) uses the same immutable
//! baseline + numbered migration + `schema_migrations` ledger + `sqlite_master`
//! structural verification machinery as `nuo.db` and `role_memory.db`
//! (ADR-0192). Ad-hoc `CREATE TABLE IF NOT EXISTS` DDL is prohibited.

use super::catalog::{Body, Migration};
use super::schema::Schema;

pub const CURRENT_CONTENT_SHARD_VERSION: u32 = 1;

pub const MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    name: "baseline",
    code_revision: 1,
    body: Body::Sql(include_str!(
        "../../../migrations/content_shard/0001_baseline.sql"
    )),
}];

pub static CONTENT_SHARD_SCHEMA: Schema = Schema::new(
    "content-shard.db",
    MIGRATIONS,
    CURRENT_CONTENT_SHARD_VERSION,
);

#[cfg(test)]
pub const BASELINE_CHECKSUM: &str =
    "b05133879a2760f0821a1d0c74b979959b7fc1816fe806eac837ee29a26be2ae";

#[cfg(test)]
pub const SHIPPED_CHECKSUMS: &[(u32, &str, &str)] = &[(1, "baseline", BASELINE_CHECKSUM)];
