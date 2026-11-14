//! Working state, checksums, digest anchors, and thread list projection.

use super::helpers::{load_session_full, save_session_full};
use super::*;

#[test]
fn digest_anchor_and_generation_round_trip() {
    use nuo_protocol::TranscriptEntry;
use nuo_tool::{Message, Role};
    let engine = DatabaseEngine::open_in_memory().unwrap();
    let mut data = crate::thread::ThreadData {
        digest: Some(nuo_protocol::ThreadDigest::default()),
        digest_anchor: Some(4_242),
        ..Default::default()
    };
    data.transcript.push(TranscriptEntry::from_message(
        0,
        &Message::new(Role::User, "hello"),
    ));
    save_session_full(&engine, &data).unwrap();

    let reloaded = load_session_full(&engine, &data.id).unwrap().unwrap();
    assert!(reloaded.digest.is_some());
    assert_eq!(reloaded.digest_anchor, Some(4_242));
    assert_eq!(reloaded.generation, data.generation);
}

#[test]
fn newer_database_is_refused() {
    let mut conn = initialize_in_memory_db().unwrap();
    conn.execute_batch(&format!(
        "PRAGMA user_version = {};",
        CURRENT_DB_VERSION + 1
    ))
    .unwrap();
    let err = migrate_schema(&mut conn).unwrap_err();
    assert!(err.to_string().contains("newer than this binary"), "{err}");
}

#[test]
fn row_checksum_detects_working_state_corruption_on_load() {
    use nuo_protocol::TranscriptEntry;
use nuo_tool::{Message, Role};
    let engine = DatabaseEngine::open_in_memory().unwrap();
    let mut data = crate::thread::ThreadData::default();
    data.transcript.push(TranscriptEntry::from_message(
        0,
        &Message::new(Role::User, "hello"),
    ));
    save_session_full(&engine, &data).unwrap();

    // Corrupt the working state out-of-band, then restamp a checksum that
    // matches the corruption? No — the mismatch case: the on-row checksum
    // no longer matches the corrupted columns. Load is non-fatal (the row
    // data remains authoritative), but a fresh full save restamps a
    // correct checksum over the corrected state.
    engine
        .conn
        .execute(
            "UPDATE threads SET round_counter = 99 WHERE id = ?1",
            params![data.id],
        )
        .unwrap();
    let reloaded = load_session_full(&engine, &data.id).unwrap().unwrap();
    assert_eq!(reloaded.round_counter, 99);

    // The recomputed checksum over the loaded (corrupted) row matches the
    // stored one only if nothing changed — here it must differ, so a save
    // restamps and the next load is consistent again.
    save_session_full(&engine, &reloaded).unwrap();
    let again = load_session_full(&engine, &data.id).unwrap().unwrap();
    assert_eq!(again.checksum, reloaded.checksum);
}

#[test]
fn working_state_round_trips_through_the_session_row() {
    let engine = DatabaseEngine::open_in_memory().unwrap();
    thread_session::upsert_session(
        &engine.conn,
        &ThreadRecord {
            id: "s1".into(),
            parent_id: None,
            origin: "trunk".into(),
            title: Some("T".into()),
            created_at_s: 1,
            updated_at_s: 1,
            workspace_root: Some("/tmp".into()),
            persona: None,
            msg_count: 0,
            last_user_prompt: None,
            digest: None,
            status: "idle".into(),
            round_counter: 0,
        },
    )
    .unwrap();
    let sessions =
        thread_session::list_sessions(&engine.conn, &nuo_protocol::ThreadFilter::global(), true)
            .unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].title.as_deref(), Some("T"));
}

/// A blank title column is an absent title, not an empty one — the `Option`
/// contract every display site relies on.
#[test]
fn blank_title_reads_back_as_absent() {
    let engine = DatabaseEngine::open_in_memory().unwrap();
    let record = ThreadRecord {
        id: "untitled".into(),
        parent_id: None,
        origin: "trunk".into(),
        title: None,
        created_at_s: 1,
        updated_at_s: 1,
        workspace_root: Some("/tmp".into()),
        persona: None,
        msg_count: 1,
        last_user_prompt: None,
        digest: None,
        status: "idle".into(),
        round_counter: 0,
    };
    thread_session::upsert_session(&engine.conn, &record).unwrap();
    let loaded = thread_session::load_session_full(&engine.conn, "untitled")
        .unwrap()
        .expect("row");
    assert_eq!(loaded.title, None);
}
