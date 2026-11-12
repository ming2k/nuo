//! Persistence handle lifecycle: backup, reader leases, and lock recovery.

use super::save::save_session_full;
use super::*;

#[tokio::test]
async fn create_backup_and_with_reader_roundtrip() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let handle = PersistenceHandle::spawn(tmp.path().join("nuo.db"), None);

    handle
        .set_kv("backup:key".into(), "backup:val".into())
        .await
        .expect("set_kv");

    // Scoped with_reader reads key and immediately releases lease
    let read_val = handle
        .with_reader(|reader| reader.get_kv("backup:key"))
        .expect("with_reader");
    assert_eq!(read_val.as_deref(), Some("backup:val"));

    // Online hot backup
    let backup_file = tmp.path().join("backup.db");
    handle
        .create_backup(backup_file.clone())
        .await
        .expect("create_backup");
    assert!(backup_file.exists());

    // Verify backup is a valid standalone SQLite database
    let backup_handle = PersistenceHandle::spawn(backup_file, None);
    let val = backup_handle
        .with_reader(|reader| reader.get_kv("backup:key"))
        .expect("read from backup");
    assert_eq!(val.as_deref(), Some("backup:val"));
}

#[test]
fn derived_fts_rows_align_with_memberships_and_are_purged() {
    use nuo_protocol::{Message, Role, TranscriptEntry};

    let engine = DatabaseEngine::open_in_memory().unwrap();
    let mut data = crate::session::SessionData::default();
    data.transcript.push(TranscriptEntry::from_message(
        0,
        &Message::new(Role::User, "unique_needle_for_fts_test"),
    ));
    save_session_full(&engine, &data).unwrap();

    // The save queues; it does not index.
    let queued: i64 = engine
        .conn
        .query_row("SELECT COUNT(*) FROM fts_entries", [], |r| r.get(0))
        .unwrap();
    assert_eq!(queued, 0);

    crate::db::derived::flush_session(&engine.conn, &data.id).unwrap();

    // The rowid is the membership rowid, so `snippet`/`bm25` joins stay valid.
    let fts_rowid: i64 = engine
        .conn
        .query_row(
            "SELECT rowid FROM fts_entries WHERE content LIKE '%unique_needle_for_fts_test%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let membership_rowid: i64 = engine
        .conn
        .query_row(
            "SELECT rowid FROM entry_memberships WHERE session_id = ?1",
            params![data.id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(fts_rowid, membership_rowid);

    let results =
        history_search::search_history(&engine.conn, "unique_needle_for_fts_test", None, 10)
            .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].session_id, data.id);

    // Deleting a session purges both derived views and the cursor/queue rows.
    session_store::delete_session(&engine.conn, &data.id).unwrap();
    let count = |table: &str| -> i64 {
        engine
            .conn
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    };
    assert_eq!(count("fts_entries"), 0);
    assert_eq!(count("causal_nodes"), 0);
    assert_eq!(count("derived_cursors"), 0);
    assert_eq!(count("derived_pending"), 0);
}

#[test]
fn persistence_handle_recovers_after_transient_lock_release() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("nuo.db");
    let lock_path = db_path.with_extension("db.owner.lock");

    // Hold the lock initially to simulate predecessor holding it
    let external_lock = nuo_host::lock::ProcessLock::acquire(&lock_path).unwrap();

    // Spawn fails while lock is held
    let handle_fail = PersistenceHandle::spawn(db_path.clone(), None);
    assert!(handle_fail.ensure_ready().is_err());

    // Release predecessor's lock
    drop(external_lock);

    // Subsequent spawn succeeds and is usable
    let handle_ok = PersistenceHandle::spawn(db_path, None);
    assert!(handle_ok.ensure_ready().is_ok());
}
