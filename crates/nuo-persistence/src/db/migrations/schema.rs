//! A versioned SQLite schema: an immutable baseline plus numbered migrations.
//!
//! The runner is schema-agnostic. `nuo.db` and `role_memory.db` each own a
//! `Schema` value, so a second durable database cannot drift back into
//! `CREATE TABLE IF NOT EXISTS` ad-hoc DDL (ADR-0192).

use std::sync::OnceLock;

use super::catalog::Migration;

/// One object exactly as SQLite stores it in `sqlite_master`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaObject {
    pub kind: String,
    pub name: String,
    pub sql: String,
}

pub struct Schema {
    /// Stable name for logs and diagnostics (`nuo.db`, `role_memory.db`).
    pub name: &'static str,
    pub migrations: &'static [Migration],
    pub current_version: u32,
    /// The expected schema, built once per process from `migrations`.
    /// `Result` is cached so a broken catalog fails every open with the same
    /// actionable message instead of panicking.
    pub(crate) expected: OnceLock<Result<Vec<SchemaObject>, String>>,
}

impl Schema {
    pub const fn new(
        name: &'static str,
        migrations: &'static [Migration],
        current_version: u32,
    ) -> Self {
        Self {
            name,
            migrations,
            current_version,
            expected: OnceLock::new(),
        }
    }

    pub fn migration(&self, version: u32) -> Option<&'static Migration> {
        self.migrations
            .iter()
            .find(|migration| migration.version == version)
    }
}
