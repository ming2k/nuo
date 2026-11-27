//! Connection lifecycle for `DatabaseEngine`.
//!
//! The engine is the **writer's** connection and nothing else. In particular it
//! does not own a blob store and does not create backups: both are filesystem
//! work, and the single writer thread must never perform filesystem work that
//! other sessions' projections are waiting behind. Blob writes happen during
//! `prepare_save`, backups happen on the storage-maintenance thread over its
//! own read-only connection (see `backup.rs`).
use std::path::Path;

use rusqlite::Result;

use super::*;

impl DatabaseEngine {
    /// Open or create the writer's database connection on a file path.
    pub(crate) fn open(db_path: &Path) -> Result<Self> {
        Self::open_with_observer(db_path, None)
    }

    /// Open or create the writer's database connection, forwarding migration
    /// progress to `observer` when pending migrations execute (ADR-0266).
    pub(crate) fn open_with_observer(
        db_path: &Path,
        observer: Option<&MigrationObserver>,
    ) -> Result<Self> {
        let conn = initialize_db_with_observer(db_path, observer)?;
        Ok(Self { conn })
    }

    /// Open an in-memory database engine. Test-only: the shipped surface has
    /// no way to name a connection — every real read and write goes through
    /// the handle's two doors.
    #[cfg(test)]
    pub(crate) fn open_in_memory() -> Result<Self> {
        let conn = initialize_in_memory_db()?;
        Ok(Self { conn })
    }
}
