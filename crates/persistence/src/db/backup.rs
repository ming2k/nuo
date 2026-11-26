//! Autonomous rolling backup management and disaster recovery (ADR-0090).
//!
//! Backups are **not** part of the writer. A snapshot is read-only work over
//! the whole database, so it runs on its own short-lived read-only connection:
//! `VACUUM INTO` needs no write lock, and putting it behind the single writer
//! would stop every session's projection for the duration of a multi-gigabyte
//! copy.
//!
//! Provides point-in-time snapshot creation using SQLite's `VACUUM INTO`,
//! rolling retention pruning, integrity verification, and non-destructive atomic restores.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};

/// Prefix used for auto-generated SQLite backup files.
pub const BACKUP_PREFIX: &str = "nuo_backup_";
/// Prefix used for auto-generated rolling journal snapshot directories.
pub const JOURNAL_BACKUP_PREFIX: &str = "nuo_journal_backup_";
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
        host::paths::get().db_file().with_extension("backups")
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

            if file_name.starts_with(BACKUP_PREFIX)
                && file_name.ends_with(".db")
                && let Ok(metadata) = self.inspect_backup(&path)
            {
                entries.push(metadata);
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

    /// Create an atomic online hot-backup, then prune excess snapshots.
    ///
    /// The snapshot is taken over a **dedicated read-only connection**: the
    /// single writer is never involved, so a multi-gigabyte `VACUUM INTO` does
    /// not stall any session's projection. `VACUUM INTO` reads a single
    /// consistent snapshot, so concurrency with live writers is safe.
    pub fn create_rolling_backup(
        &self,
        db_path: &Path,
        retention_limit: usize,
    ) -> Result<BackupMetadata, String> {
        let (meta, _) = self.create_rolling_backup_with_pruned(db_path, retention_limit)?;
        Ok(meta)
    }

    /// Create an atomic online hot-backup and prune excess snapshots, returning
    /// both the new snapshot metadata and the exact number of pruned files.
    pub(crate) fn create_rolling_backup_with_pruned(
        &self,
        db_path: &Path,
        retention_limit: usize,
    ) -> Result<(BackupMetadata, usize), String> {
        crate::fs_privacy::ensure_private_dir(&self.backups_dir)
            .map_err(|e| format!("failed to create backup dir: {e}"))?;

        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let short_id = &uuid::Uuid::new_v4().to_string()[..8];
        let file_name = format!("{BACKUP_PREFIX}{now_ms}_{short_id}.db");
        let target_path = self.backups_dir.join(file_name);

        vacuum_into(db_path, &target_path).map_err(|e| format!("online hot backup failed: {e}"))?;
        crate::fs_privacy::restrict_private_file(&target_path);

        let meta = self.inspect_backup(&target_path)?;

        // Prune older backups beyond retention limit and preserve exact count.
        let pruned = self.prune_excess_backups(retention_limit).unwrap_or(0);

        Ok((meta, pruned))
    }

    /// Create an atomic rolling point-in-time snapshot of the authoritative
    /// `journal/` directory (ADR-0255 `[INV-SHARD-03]`).
    ///
    /// Immutable artifacts (`snap-*.json`, `DELETED`) are hard-linked (falling
    /// back to copy across filesystems) for near-zero extra disk usage; active
    /// append-only segments (`seg-*.log`) and aliases are copied so subsequent
    /// turns never mutate the snapshot. Returns `Ok(None)` when `journal_root`
    /// does not exist or is empty.
    pub fn create_journal_snapshot(
        &self,
        journal_root: &Path,
        retention_limit: usize,
    ) -> Result<Option<PathBuf>, String> {
        let (path, _) = self.create_journal_snapshot_with_pruned(journal_root, retention_limit)?;
        Ok(path)
    }

    /// Create an atomic rolling journal snapshot and return the exact number
    /// of pruned older snapshot directories.
    pub(crate) fn create_journal_snapshot_with_pruned(
        &self,
        journal_root: &Path,
        retention_limit: usize,
    ) -> Result<(Option<PathBuf>, usize), String> {
        if !journal_root.is_dir() {
            return Ok((None, 0));
        }
        let mut has_entries = false;
        if let Ok(mut rd) = fs::read_dir(journal_root) {
            has_entries = rd.next().is_some();
        }
        if !has_entries {
            return Ok((None, 0));
        }

        crate::fs_privacy::ensure_private_dir(&self.backups_dir)
            .map_err(|e| format!("failed to create backup dir: {e}"))?;

        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let short_id = &uuid::Uuid::new_v4().to_string()[..8];
        let final_name = format!("{JOURNAL_BACKUP_PREFIX}{now_ms}_{short_id}");
        let staging_path = self.backups_dir.join(format!(".tmp_{final_name}"));
        let target_path = self.backups_dir.join(final_name);

        if let Err(error) = snapshot_journal_tree(journal_root, &staging_path) {
            let _ = fs::remove_dir_all(&staging_path);
            return Err(error);
        }

        if let Err(error) = fs::rename(&staging_path, &target_path) {
            let _ = fs::remove_dir_all(&staging_path);
            return Err(format!(
                "failed to publish journal snapshot {}: {error}",
                target_path.display()
            ));
        }

        let pruned = self
            .prune_excess_journal_snapshots(retention_limit)
            .unwrap_or(0);
        Ok((Some(target_path), pruned))
    }

    /// List rolling journal snapshot directories (newest first).
    pub fn list_journal_snapshots(&self) -> Result<Vec<PathBuf>, String> {
        if !self.backups_dir.exists() {
            return Ok(Vec::new());
        }
        let mut entries: Vec<(u64, PathBuf)> = Vec::new();
        let read_dir = fs::read_dir(&self.backups_dir)
            .map_err(|e| format!("failed to read backups dir: {e}"))?;
        for entry in read_dir.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(tail) = name.strip_prefix(JOURNAL_BACKUP_PREFIX) else {
                continue;
            };
            let ts = tail
                .split('_')
                .next()
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0);
            entries.push((ts, path));
        }
        entries.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
        Ok(entries.into_iter().map(|(_, p)| p).collect())
    }

    /// Prune oldest rolling journal snapshot directories exceeding `retention_limit`.
    pub fn prune_excess_journal_snapshots(&self, retention_limit: usize) -> Result<usize, String> {
        if retention_limit == 0 {
            return Ok(0);
        }
        let snapshots = self.list_journal_snapshots()?;
        if snapshots.len() <= retention_limit {
            return Ok(0);
        }
        let mut pruned = 0;
        for excess in &snapshots[retention_limit..] {
            if fs::remove_dir_all(excess).is_ok() {
                pruned += 1;
            }
        }
        Ok(pruned)
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

fn snapshot_journal_tree(src: &Path, dst: &Path) -> Result<(), String> {
    crate::fs_privacy::ensure_private_dir(dst).map_err(|e| {
        format!(
            "failed to create journal snapshot dir {}: {e}",
            dst.display()
        )
    })?;
    let read_dir = fs::read_dir(src)
        .map_err(|e| format!("failed to read journal dir {}: {e}", src.display()))?;
    for entry in read_dir.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.ends_with(".tmp") || name_str.starts_with(".tmp") {
            continue;
        }
        let src_path = entry.path();
        let dst_path = dst.join(&name);
        if src_path.is_dir() {
            snapshot_journal_tree(&src_path, &dst_path)?;
        } else if src_path.is_file() {
            let immutable = name_str.starts_with("snap-") || name_str == "DELETED";
            if immutable && fs::hard_link(&src_path, &dst_path).is_ok() {
                continue;
            }
            fs::copy(&src_path, &dst_path).map_err(|e| {
                format!(
                    "failed to copy journal file {} to {}: {e}",
                    src_path.display(),
                    dst_path.display()
                )
            })?;
            crate::fs_privacy::restrict_private_file(&dst_path);
        }
    }
    Ok(())
}

/// Copy a live database into `target` with `VACUUM INTO`.
///
/// Opens its own read-only connection. `VACUUM INTO` takes one consistent read
/// snapshot, so the copy is atomic with respect to concurrent writers while the
/// writer's own connection (and therefore its command queue) is untouched.
/// `mode=ro` is required because `VACUUM INTO` writes the *destination*, not
/// the source; SQLite accepts it over a read-only source handle.
pub fn vacuum_into(db_path: &Path, target: &Path) -> Result<(), String> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("could not create backup dir: {e}"))?;
    }
    if target.exists() {
        fs::remove_file(target).map_err(|e| format!("could not replace backup target: {e}"))?;
    }
    let target_str = target
        .to_str()
        .ok_or_else(|| format!("backup path is not valid UTF-8: {}", target.display()))?;
    let source = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| format!("could not open {} read-only: {e}", db_path.display()))?;
    source
        .execute("VACUUM INTO ?1", [target_str])
        .map_err(|e| format!("VACUUM INTO {} failed: {e}", target.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::PersistenceHandle;

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
        let b1 = manager.create_rolling_backup(&db_file, 2).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(15));
        let b2 = manager.create_rolling_backup(&db_file, 2).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(15));
        let b3 = manager.create_rolling_backup(&db_file, 2).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(15));
        let b4 = manager.create_rolling_backup(&db_file, 2).unwrap();

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
        let snap1 = manager.create_rolling_backup(&db_file, 5).unwrap();

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

    #[test]
    fn journal_rolling_snapshot_isolates_segments_and_prunes_excess() {
        let tmp = tempfile::tempdir().unwrap();
        let journal_root = tmp.path().join("journal");
        let session_dir = journal_root.join("session-1");
        fs::create_dir_all(&session_dir).unwrap();
        let snap = session_dir.join("snap-00000000000000000001.json");
        let seg = session_dir.join("seg-00000000000000000002.log");
        fs::write(&snap, b"{\"id\":\"session-1\"}").unwrap();
        fs::write(&seg, b"initial-segment").unwrap();

        let manager = BackupManager::new(tmp.path().join("backups"));
        let s1 = manager
            .create_journal_snapshot(&journal_root, 2)
            .unwrap()
            .expect("snapshot 1");

        // Mutate the live segment after snapshot 1; snapshot 1 must retain its
        // point-in-time bytes.
        fs::write(&seg, b"initial-segment+appended").unwrap();
        assert_eq!(
            fs::read(s1.join("session-1").join("seg-00000000000000000002.log")).unwrap(),
            b"initial-segment"
        );
        assert_eq!(
            fs::read(s1.join("session-1").join("snap-00000000000000000001.json")).unwrap(),
            b"{\"id\":\"session-1\"}"
        );

        std::thread::sleep(std::time::Duration::from_millis(15));
        let s2 = manager
            .create_journal_snapshot(&journal_root, 2)
            .unwrap()
            .expect("snapshot 2");
        std::thread::sleep(std::time::Duration::from_millis(15));
        let s3 = manager
            .create_journal_snapshot(&journal_root, 2)
            .unwrap()
            .expect("snapshot 3");

        let list = manager.list_journal_snapshots().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0], s3);
        assert_eq!(list[1], s2);
        assert!(!s1.exists(), "oldest journal snapshot must be pruned");
    }
}
