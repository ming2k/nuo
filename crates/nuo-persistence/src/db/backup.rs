//! Autonomous rolling backup management and disaster recovery (ADR-0090).
//!
//! Provides point-in-time snapshot creation using SQLite's `VACUUM INTO`,
//! rolling retention pruning, integrity verification, and non-destructive atomic restores.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use super::PersistenceHandle;

/// Prefix used for auto-generated backup files.
pub const BACKUP_PREFIX: &str = "nuo_backup_";
/// Default number of rolling backups retained if uncapped.
pub const DEFAULT_BACKUP_RETENTION_LIMIT: usize = 7;

/// Metadata for a verified database snapshot.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BackupMetadata {
    pub path: PathBuf,
    pub created_at_ms: u64,
    pub size_bytes: u64,
    pub schema_version: u32,
}

/// Autonomous backup and disaster recovery manager.
#[derive(Debug, Clone)]
pub struct BackupManager {
    backups_dir: PathBuf,
}

impl BackupManager {
    /// Create a new `BackupManager` storing snapshots in `backups_dir`.
    pub fn new(backups_dir: PathBuf) -> Self {
        Self { backups_dir }
    }

    /// Default backup directory under application state.
    pub fn default_dir() -> PathBuf {
        crate::paths::get().db_file().with_extension("backups")
    }

    /// Root directory holding managed backup files.
    pub fn backups_dir(&self) -> &Path {
        &self.backups_dir
    }

    /// List all verified backup snapshots in chronological order (newest first).
    pub fn list_backups(&self) -> Result<Vec<BackupMetadata>, String> {
        if !self.backups_dir.exists() {
            return Ok(Vec::new());
        }

        let mut entries = Vec::new();
        let read_dir = fs::read_dir(&self.backups_dir)
            .map_err(|e| format!("failed to read backups dir: {e}"))?;

        for entry in read_dir {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            let path = entry.path();
            let file_name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default();

            if file_name.starts_with(BACKUP_PREFIX) && file_name.ends_with(".db") {
                if let Ok(metadata) = self.inspect_backup(&path) {
                    entries.push(metadata);
                }
            }
        }

        // Sort descending: newest first
        entries.sort_by_key(|entry| std::cmp::Reverse(entry.created_at_ms));
        Ok(entries)
    }

    /// Inspect and verify a backup candidate file.
    pub fn inspect_backup(&self, path: &Path) -> Result<BackupMetadata, String> {
        let file_meta =
            fs::metadata(path).map_err(|e| format!("cannot read metadata for {:?}: {e}", path))?;
        let version = self.verify_backup(path)?;

        let created_at_ms = file_meta
            .modified()
            .ok()
            .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as u64)
            .unwrap_or_else(|| {
                // Parse timestamp from file name: nuo_backup_{timestamp_ms}_{uuid}.db
                let name = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or_default();
                name.strip_prefix(BACKUP_PREFIX)
                    .and_then(|tail| tail.split('_').next())
                    .and_then(|ts| ts.parse::<u64>().ok())
                    .unwrap_or(0)
            });

        Ok(BackupMetadata {
            path: path.to_path_buf(),
            created_at_ms,
            size_bytes: file_meta.len(),
            schema_version: version,
        })
    }

    /// Run quick structural integrity and read `user_version` on a snapshot.
    pub fn verify_backup(&self, path: &Path) -> Result<u32, String> {
        if !path.exists() {
            return Err(format!("backup file does not exist: {:?}", path));
        }

        let conn = Connection::open(path)
            .map_err(|e| format!("failed to open backup SQLite file {:?}: {e}", path))?;

        let integrity: String = conn
            .query_row("PRAGMA quick_check", [], |row| row.get(0))
            .map_err(|e| format!("integrity check failed on {:?}: {e}", path))?;

        if integrity != "ok" {
            return Err(format!(
                "backup integrity check reported error: {integrity}"
            ));
        }

        let version: u32 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(|e| format!("failed to query schema version on {:?}: {e}", path))?;

        Ok(version)
    }

    /// Create an atomic online hot-backup via [`PersistenceHandle`], then prune excess snapshots.
    pub fn create_rolling_backup(
        &self,
        handle: &PersistenceHandle,
        retention_limit: usize,
    ) -> Result<BackupMetadata, String> {
        fs::create_dir_all(&self.backups_dir)
            .map_err(|e| format!("failed to create backup dir: {e}"))?;

        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let short_id = &uuid::Uuid::new_v4().to_string()[..8];
        let file_name = format!("{BACKUP_PREFIX}{now_ms}_{short_id}.db");
        let target_path = self.backups_dir.join(file_name);

        handle
            .create_backup_blocking(target_path.clone())
            .map_err(|e| format!("online hot backup failed: {e}"))?;

        let meta = self.inspect_backup(&target_path)?;

        // Prune older backups beyond retention limit
        let _ = self.prune_excess_backups(retention_limit);

        Ok(meta)
    }

    /// Prune oldest backups exceeding `retention_limit`. Returns number of deleted files.
    pub fn prune_excess_backups(&self, retention_limit: usize) -> Result<usize, String> {
        if retention_limit == 0 {
            return Ok(0);
        }

        let backups = self.list_backups()?;
        if backups.len() <= retention_limit {
            return Ok(0);
        }

        let mut pruned = 0;
        for excess in &backups[retention_limit..] {
            if fs::remove_file(&excess.path).is_ok() {
                pruned += 1;
            }
        }
        Ok(pruned)
    }

    /// Restore a backup file to `target_db_path`.
    ///
    /// Safety Invariant [INV-PERSIST-BACKUP-02]:
    /// 1. Verifies snapshot integrity before touching active state.
    /// 2. Atomically preserves existing database to `<target_db>.pre_restore.bak` as a safety net.
    /// 3. Safely copies verified snapshot into place.
    pub fn restore_backup(
        &self,
        backup_path: &Path,
        target_db_path: &Path,
    ) -> Result<PathBuf, String> {
        if !backup_path.exists() {
            return Err(format!("backup source file not found: {:?}", backup_path));
        }

        // Pre-flight validation
        self.verify_backup(backup_path)?;

        let parent = target_db_path
            .parent()
            .ok_or_else(|| format!("invalid target db path {:?}", target_db_path))?;
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;

        // Generate emergency rescue backup if target database already exists
        let emergency_path = target_db_path.with_extension("pre_restore.bak");
        if target_db_path.exists() {
            fs::copy(target_db_path, &emergency_path).map_err(|e| {
                format!(
                    "failed to preserve current state to {:?}: {e}",
                    emergency_path
                )
            })?;
        }

        // Clean up active WAL sidecars if any
        let wal_path = PathBuf::from(format!("{}-wal", target_db_path.display()));
        let shm_path = PathBuf::from(format!("{}-shm", target_db_path.display()));
        let _ = fs::remove_file(&wal_path);
        let _ = fs::remove_file(&shm_path);

        // Copy backup to temporary file in same folder, then rename atomically
        let tmp_path = target_db_path.with_extension("restore.tmp");
        fs::copy(backup_path, &tmp_path)
            .map_err(|e| format!("failed to copy backup to tmp location: {e}"))?;

        fs::rename(&tmp_path, target_db_path)
            .map_err(|e| format!("failed to atomically commit restored database: {e}"))?;

        Ok(emergency_path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_verify_nonexistent_or_corrupted_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let manager = BackupManager::new(tmp.path().to_path_buf());
        let missing = tmp.path().join("none.db");
        assert!(manager.verify_backup(&missing).is_err());

        let corrupted = tmp.path().join("corrupted.db");
        fs::write(&corrupted, b"not a sqlite database file").unwrap();
        assert!(manager.verify_backup(&corrupted).is_err());
    }

    #[test]
    fn test_rolling_backup_creation_and_pruning() {
        let tmp = tempfile::tempdir().unwrap();
        let db_file = tmp.path().join("test.db");
        let backups_dir = tmp.path().join("backups");

        let handle = PersistenceHandle::spawn(db_file.clone(), None);
        handle
            .set_kv_blocking("test_key".into(), "test_val".into())
            .unwrap();

        let manager = BackupManager::new(backups_dir.clone());

        // Create 4 rolling backups with retention cap 2
        let b1 = manager.create_rolling_backup(&handle, 2).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(15));
        let b2 = manager.create_rolling_backup(&handle, 2).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(15));
        let b3 = manager.create_rolling_backup(&handle, 2).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(15));
        let b4 = manager.create_rolling_backup(&handle, 2).unwrap();

        assert_eq!(b1.schema_version, crate::db::CURRENT_DB_VERSION);
        assert_eq!(b4.schema_version, crate::db::CURRENT_DB_VERSION);

        let list = manager.list_backups().unwrap();
        assert_eq!(list.len(), 2, "must be capped at retention limit");
        assert_eq!(list[0].path, b4.path, "newest should be first");
        assert_eq!(list[1].path, b3.path, "second newest retained");
        assert!(!b1.path.exists(), "oldest should have been pruned");
        assert!(!b2.path.exists(), "second oldest should have been pruned");
    }

    #[test]
    fn test_restore_backup_safeguard_and_rescue_copy() {
        let tmp = tempfile::tempdir().unwrap();
        let db_file = tmp.path().join("original.db");
        let backups_dir = tmp.path().join("backups");
        let manager = BackupManager::new(backups_dir.clone());

        let handle = PersistenceHandle::spawn(db_file.clone(), None);
        handle
            .set_kv_blocking("restore_key".into(), "first_snapshot".into())
            .unwrap();

        // Create backup 1
        let snap1 = manager.create_rolling_backup(&handle, 5).unwrap();

        // Mutate original
        handle
            .set_kv_blocking("restore_key".into(), "mutated_val".into())
            .unwrap();

        // Drop handle and test restore into db_file
        drop(handle);

        let rescue = manager.restore_backup(&snap1.path, &db_file).unwrap();
        assert!(rescue.exists(), "rescue .pre_restore.bak must exist");

        // Open restored database and verify it has 'first_snapshot'
        let restored_handle = PersistenceHandle::spawn(db_file.clone(), None);
        let val = restored_handle
            .with_reader(|r| r.get_kv("restore_key"))
            .unwrap();
        assert_eq!(val.as_deref(), Some("first_snapshot"));
    }
}
