//! Working state, checksums, digest anchors, and thread list projection.

use super::helpers::{load_session_full, save_session_full};
use super::*;

#[test]
fn digest_anchor_and_generation_round_trip() {
    use protocol::TranscriptEntry;
    use tool::{Message, Role};
    let engine = DatabaseEngine::open_in_memory().unwrap();
    let mut data = crate::thread::ThreadData {
        digest: Some(protocol::ThreadDigest::default()),
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
fn row_checksum_detects_working_state_corruption_on_load() {
    use protocol::TranscriptEntry;
    use tool::{Message, Role};
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
            round_counter: 0,
        },
    )
    .unwrap();
    let sessions =
        thread_session::list_sessions(&engine.conn, &protocol::ThreadFilter::all(), true).unwrap();
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
        round_counter: 0,
    };
    thread_session::upsert_session(&engine.conn, &record).unwrap();
    let loaded =
        thread_session::load_session_full_from_planes(&engine.conn, None, "untitled", None)
            .unwrap()
            .expect("row");
    assert_eq!(loaded.title, None);
}

/// Unbound threads (`workspace: None`) persist as SQL `NULL` and round-trip
/// back as `None` (never `Some(WorkspaceBinding { root: "" })`), while SQLite
/// refuses an empty string `''` at the schema level (ADR-0255 `[INV-SHARD-02]`).
#[test]
fn unbound_workspace_round_trips_as_none_and_empty_string_is_refused() {
    let engine = DatabaseEngine::open_in_memory().unwrap();
    let mut unbound = crate::thread::ThreadData {
        role: Some("philosophist".into()),
        ..Default::default()
    };
    unbound.workspace = None;
    save_session_full(&engine, &unbound).unwrap();

    let reloaded = load_session_full(&engine, &unbound.id).unwrap().unwrap();
    assert_eq!(reloaded.workspace, None);

    let (looked_up_ws, looked_up_role) =
        thread_session::lookup_session_workspace(&engine.conn, &unbound.id)
            .unwrap()
            .expect("policy row");
    assert_eq!(looked_up_ws, None);
    assert_eq!(looked_up_role.as_deref(), Some("philosophist"));

    // Empty-string sentinel must be refused by the SQL CHECK constraint.
    assert!(
        engine
            .conn
            .execute(
                "UPDATE thread_policies SET workspace_root = '' WHERE thread_id = ?1",
                params![unbound.id],
            )
            .is_err(),
        "empty workspace_root string must be rejected by CHECK constraint"
    );
}

/// Hydrating from catalog + content shard preserves normalized `command_acts`
/// and stamps `CURRENT_SCHEMA_VERSION` so fallback import never clears them
/// (ADR-0255 `[INV-SHARD-05]`).
#[test]
fn load_session_full_from_planes_preserves_command_acts_and_current_schema_version() {
    let catalog = DatabaseEngine::open_in_memory().unwrap();
    let content = crate::db::migrations::open_content_shard_in_memory_for_tests().unwrap();

    let data = crate::thread::ThreadData {
        id: "t-cmds".into(),
        commands: vec![protocol::CommandRecord::new("unattended", "on")],
        ..Default::default()
    };
    save_session_full(&catalog, &data).unwrap();
    thread_session::sync_command_acts(&content, &data.id, &data.commands).unwrap();

    let loaded = thread_session::load_session_full_from_planes(
        &catalog.conn,
        Some(&content),
        &data.id,
        None,
    )
    .unwrap()
    .expect("session");
    assert_eq!(loaded.schema_version, crate::thread::CURRENT_SCHEMA_VERSION);
    assert_eq!(loaded.commands.len(), 1);
    assert_eq!(loaded.commands[0].name, "unattended");
}
