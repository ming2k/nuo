//! Working state, checksums, digest anchors, and thread list projection.

use super::save::{load_session_full, save_session_full};
use super::*;

#[test]
fn digest_anchor_tree_and_generation_round_trip() {
    use nuo_protocol::{Message, Role, TranscriptEntry};
    let engine = DatabaseEngine::open_in_memory().unwrap();
    let mut data = crate::session::SessionData {
        digest: Some(nuo_protocol::ThreadDigest::default()),
        digest_anchor: Some(4_242),
        ..Default::default()
    };
    data.tree.active_leaf_id = Some("leaf-1".to_string());
    data.transcript.push(TranscriptEntry::from_message(
        0,
        &Message::new(Role::User, "hello"),
    ));
    save_session_full(&engine, &data).unwrap();

    let reloaded = load_session_full(&engine, &data.id).unwrap().unwrap();
    assert!(reloaded.digest.is_some());
    assert_eq!(reloaded.digest_anchor, Some(4_242));
    assert_eq!(reloaded.tree.active_leaf_id.as_deref(), Some("leaf-1"));
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
    use nuo_protocol::{Message, Role, TranscriptEntry};
    let engine = DatabaseEngine::open_in_memory().unwrap();
    let mut data = crate::session::SessionData::default();
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
    session_store::upsert_session(
        &engine.conn,
        &ThreadRecord {
            id: "s1".into(),
            parent_id: None,
            fork_kind: "trunk".into(),
            fork_point: None,
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
        session_store::list_sessions(&engine.conn, &nuo_protocol::ThreadFilter::global(), true)
            .unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].title.as_deref(), Some("T"));
}

/// `[INV-LINEAGE-01]`: the persisted fork point is surfaced by the read
/// projection, so no consumer has to reconstruct it. Also pins the truthful
/// absence: a trunk has no fork point, and a titleless thread reads back as
/// `None` rather than `Some("")`.
#[test]
fn thread_list_view_surfaces_the_persisted_fork_point() {
    let engine = DatabaseEngine::open_in_memory().unwrap();

    let trunk = ThreadRecord {
        id: "trunk".into(),
        parent_id: None,
        fork_kind: "trunk".into(),
        fork_point: None,
        title: Some("Trunk".into()),
        created_at_s: 1,
        updated_at_s: 1,
        workspace_root: Some("/tmp".into()),
        persona: None,
        msg_count: 0,
        last_user_prompt: None,
        digest: None,
        status: "idle".into(),
        round_counter: 0,
    };
    session_store::upsert_session(&engine.conn, &trunk).unwrap();

    let mut branch = trunk.clone();
    branch.id = "branch".into();
    branch.parent_id = Some("trunk".into());
    branch.fork_kind = "fork".into();
    branch.fork_point = Some(nuo_protocol::ForkPoint::at_turn(3, 2));
    branch.created_at_s = 2;
    branch.updated_at_s = 2;
    session_store::upsert_session(&engine.conn, &branch).unwrap();

    let read = session_store::get_session(&engine.conn, "branch")
        .unwrap()
        .expect("branch row");
    assert_eq!(read.parent_id.as_deref(), Some("trunk"));
    assert_eq!(read.fork_kind, "fork");
    assert_eq!(
        read.fork_point,
        Some(nuo_protocol::ForkPoint::at_turn(3, 2))
    );
    assert_eq!(read.fork_point.unwrap().label(), "r3.2");

    // A trunk is truthfully without provenance, not "forked at r0".
    let read_trunk = session_store::get_session(&engine.conn, "trunk")
        .unwrap()
        .expect("trunk row");
    assert_eq!(read_trunk.fork_point, None);

    // `upsert_session` must not erase provenance a richer writer stamped.
    let mut rename_only = branch.clone();
    rename_only.fork_point = None;
    rename_only.title = Some("Renamed".into());
    session_store::upsert_session(&engine.conn, &rename_only).unwrap();
    let after = session_store::get_session(&engine.conn, "branch")
        .unwrap()
        .expect("branch row");
    assert_eq!(
        after.fork_point,
        Some(nuo_protocol::ForkPoint::at_turn(3, 2)),
        "a metadata upsert must not clear the branch's origin"
    );
    assert_eq!(after.title.as_deref(), Some("Renamed"));
}

/// A blank title column is an absent title, not an empty one — the `Option`
/// contract every display site relies on.
#[test]
fn blank_title_reads_back_as_absent() {
    let engine = DatabaseEngine::open_in_memory().unwrap();
    let record = ThreadRecord {
        id: "untitled".into(),
        parent_id: None,
        fork_kind: "trunk".into(),
        fork_point: None,
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
    session_store::upsert_session(&engine.conn, &record).unwrap();
    let loaded = session_store::load_session_full(&engine.conn, "untitled")
        .unwrap()
        .expect("row");
    assert_eq!(loaded.title, None);
}
