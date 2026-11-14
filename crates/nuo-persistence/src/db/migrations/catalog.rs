//! The immutable migration catalog.
//!
//! One entry per durable schema version. The baseline is a generated snapshot of
//! the whole schema; every later version is a new numbered file beside it. A
//! migration is never edited after it ships: the database ledger records its
//! checksum, so editing it is detected at the next open (ADR-0189).

use sha2::{Digest, Sha256};

/// Current durable schema version. A new migration bumps this to its version.
pub const CURRENT_DB_VERSION: u32 = 2;

/// One immutable schema step.
pub struct Migration {
    pub version: u32,
    /// Stable human name, also covered by the checksum.
    pub name: &'static str,
    /// Manual revision of the Rust body. Bump this whenever the body changes;
    /// SQL is checksummed byte-for-byte and does not need a manual revision.
    pub code_revision: u32,
    pub body: Body,
}

pub enum Body {
    /// A self-contained SQL script, embedded at compile time.
    Sql(&'static str),
    /// A data migration that needs the Rust type system. Runs inside the same
    /// transaction as the version stamp and the ledger row.
    Rust(fn(&rusqlite::Transaction<'_>) -> rusqlite::Result<()>),
    /// Schema DDL followed by a Rust data pass, in one transaction.
    SqlThenRust {
        sql: &'static str,
        run: fn(&rusqlite::Transaction<'_>) -> rusqlite::Result<()>,
    },
}

impl Migration {
    pub fn sql(&self) -> Option<&'static str> {
        match self.body {
            Body::Sql(sql) | Body::SqlThenRust { sql, .. } => Some(sql),
            Body::Rust(_) => None,
        }
    }

    /// SHA-256 over the migration's identity and body. Stored in
    /// `schema_migrations` and re-checked on every open.
    pub fn checksum(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.version.to_le_bytes());
        hasher.update(self.code_revision.to_le_bytes());
        hasher.update(self.name.as_bytes());
        hasher.update([0]);
        match self.body {
            Body::Sql(sql) | Body::SqlThenRust { sql, .. } => hasher.update(sql.as_bytes()),
            // A function address is not stable (PIE/ASLR); the manual
            // `code_revision` above is the immutability guard for Rust bodies.
            Body::Rust(_) => {}
        }
        format!("{:x}", hasher.finalize())
    }
}

pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "baseline",
        code_revision: 1,
        body: Body::Sql(include_str!("../../../migrations/0001_baseline.sql")),
    },
    Migration {
        version: 2,
        name: "drop_derived_thread_columns",
        code_revision: 1,
        body: Body::Sql(include_str!(
            "../../../migrations/0002_drop_derived_thread_columns.sql"
        )),
    },
];

/// Immutability guard for the baseline file. The ledger checksum already
/// catches an edit on an existing database; this constant catches it before a
/// fresh database is stamped with a changed baseline.
#[cfg(test)]
pub const BASELINE_CHECKSUM: &str =
    "cbaae58782a16955a53a50887ee498efd8864b57f4b3aa40ddc9caa67a052226";

pub static NUO_SCHEMA: super::schema::Schema =
    super::schema::Schema::new("nuo.db", MIGRATIONS, CURRENT_DB_VERSION);
