//! The immutable migration catalog.
//!
//! One entry per durable schema version. The baseline is a generated snapshot of
//! the whole schema; every later version is a new numbered file beside it. A
//! migration is never edited after it ships: the database ledger records its
//! checksum, so editing it is detected at the next open (ADR-0189).

use sha2::{Digest, Sha256};

/// Current durable schema version. A new migration bumps this to its version.
pub const CURRENT_DB_VERSION: u32 = 9;

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
    Migration {
        version: 3,
        name: "permission_rules",
        code_revision: 1,
        body: Body::Sql(include_str!(
            "../../../migrations/0003_permission_rules.sql"
        )),
    },
    Migration {
        version: 4,
        name: "drop_request_projections",
        code_revision: 1,
        body: Body::Sql(include_str!(
            "../../../migrations/0004_drop_request_projections.sql"
        )),
    },
    Migration {
        version: 5,
        name: "projection_watermarks",
        code_revision: 1,
        body: Body::Sql(include_str!(
            "../../../migrations/0005_projection_watermarks.sql"
        )),
    },
    Migration {
        version: 6,
        name: "catalog_summary",
        code_revision: 1,
        body: Body::Sql(include_str!("../../../migrations/0006_catalog_summary.sql")),
    },
    Migration {
        version: 7,
        name: "command_acts",
        code_revision: 1,
        body: Body::Sql(include_str!("../../../migrations/0007_command_acts.sql")),
    },
    Migration {
        version: 8,
        name: "orthogonal_thread_role_workspace",
        code_revision: 1,
        body: Body::Sql(include_str!(
            "../../../migrations/0008_orthogonal_thread_role_workspace.sql"
        )),
    },
    Migration {
        version: 9,
        name: "pure_catalog_and_nullable_workspace",
        code_revision: 1,
        body: Body::Sql(include_str!(
            "../../../migrations/0009_pure_catalog_and_nullable_workspace.sql"
        )),
    },
];

/// Immutability guard for the baseline file. The ledger checksum already
/// catches an edit on an existing database; this constant catches it before a
/// fresh database is stamped with a changed baseline.
#[cfg(test)]
pub const BASELINE_CHECKSUM: &str =
    "cbaae58782a16955a53a50887ee498efd8864b57f4b3aa40ddc9caa67a052226";

/// Immutability guard for every shipped migration (v1..=CURRENT_DB_VERSION).
/// Editing any shipped SQL file after release would break existing databases on
/// next open with a checksum mismatch (ADR-0189).
#[cfg(test)]
pub const SHIPPED_CHECKSUMS: &[(u32, &str, &str)] = &[
    (1, "baseline", BASELINE_CHECKSUM),
    (
        2,
        "drop_derived_thread_columns",
        "e7d6bddaa77b2a494bd6ced770ef484e4992ea45b869364e6629227e81538513",
    ),
    (
        3,
        "permission_rules",
        "e4fa236a51bd3dd653f8b5570653d5e51ec8aa87bc5207f4f9ae3656440af0b0",
    ),
    (
        4,
        "drop_request_projections",
        "7ea1e4bd7747b7f9d28f7ec69443462a47b60df3f70cc6689fa8afb95277c783",
    ),
    (
        5,
        "projection_watermarks",
        "f832b89e219e735ce85983df02af27124fbbd1e749d2526b1f26efc0dab3cf64",
    ),
    (
        6,
        "catalog_summary",
        "8811f3485aa27caa80aab4d92e1f182e25469c6260ac297ad5356aa2b68059e7",
    ),
    (
        7,
        "command_acts",
        "50c387f0b0ae4c0f031d925cffb8e5017c23dd7102718e8df60ace412e9513eb",
    ),
    (
        8,
        "orthogonal_thread_role_workspace",
        "17a6c02226412e7834479219599c999ea75234ba142fd88dc232c192d9b6ac1c",
    ),
    (
        9,
        "pure_catalog_and_nullable_workspace",
        "a7932543c6bc2a89756185aee6e6c4a7fa9c7755e7aaaa725eec18de6cef1d46",
    ),
];

pub static NUO_SCHEMA: super::schema::Schema =
    super::schema::Schema::new("nuo.db", MIGRATIONS, CURRENT_DB_VERSION);
