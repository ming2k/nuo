//! Connection lifecycle and snapshot backup management for `DatabaseEngine`.
//!
//! Provides factory constructors for SQLite storage backing Nuo persistence and
//! online snapshot creation using `VACUUM INTO`.
use std::path::Path;

use rusqlite::{Result, params};

use super::*;
use crate::blobs::BlobStore;

impl DatabaseEngine {
    /// Open or create a database engine on a file path.
    pub(crate) fn open(db_path: &Path, blob_store: Option<BlobStore>) -> Result<Self> {
        let conn = initialize_db(db_path)?;
        Ok(Self { conn, blob_store })
    }

    /// Open an in-memory database engine. Test-only: the shipped surface has
    /// no way to name a connection — every real read and write goes through
    /// the handle's two doors.
    #[cfg(test)]
    pub(crate) fn open_in_memory() -> Result<Self> {
        let conn = initialize_in_memory_db()?;
        Ok(Self {
            conn,
            blob_store: None,
        })
    }

    /// Create an online hot backup snapshot using `VACUUM INTO`.
    pub(crate) fn create_backup(&self, target_path: &Path) -> Result<()> {
        if let Some(parent) = target_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if target_path.exists() {
            let _ = std::fs::remove_file(target_path);
        }
        let target_str = target_path.to_str().ok_or_else(|| {
            rusqlite::Error::InvalidParameterName("backup path is not valid utf-8".into())
        })?;
        self.conn.execute("VACUUM INTO ?1", params![target_str])?;
        Ok(())
    }
}
