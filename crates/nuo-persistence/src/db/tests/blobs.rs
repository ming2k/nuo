//! CAS blob storage, payload round-trips, and garbage collection.

use super::save::{load_session_full, save_session_full};
use super::*;

#[test]
fn image_payload_is_saved_to_cas_blob_store() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    use nuo_protocol::{ImagePart, InjectionKind, InjectionOrigin, Message, Role, TranscriptEntry};

    let tmp = tempfile::tempdir().unwrap();
    let blob_store = BlobStore::new(tmp.path().to_path_buf());
    let mut data = crate::session::SessionData::default();
    let fake_image_bytes = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDRfakedata";
    let b64 = STANDARD.encode(fake_image_bytes);

    let companion_msg = Message::new(Role::User, "Image from tool")
        .with_images(vec![ImagePart {
            mime: "image/png".to_string(),
            data: b64,
        }])
        .with_origin(InjectionOrigin::new(InjectionKind::ToolImage));

    data.transcript
        .push(TranscriptEntry::from_message(0, &companion_msg));
    let _engine = engine_blob_round_trip(&data, &blob_store);

    let expected_hash = BlobStore::hash(fake_image_bytes);
    let stored_bytes = blob_store.get(&expected_hash);
    assert!(
        stored_bytes.is_some(),
        "Image bytes must be written to CAS BlobStore upon saving session"
    );
    assert_eq!(stored_bytes.unwrap(), fake_image_bytes);
    let _ = std::fs::remove_dir_all(blob_store.root());
}

pub(super) fn engine_blob_round_trip(
    data: &crate::session::SessionData,
    blob_store: &BlobStore,
) -> DatabaseEngine {
    let mut conn = Connection::open_in_memory().unwrap();
    configure_connection(&mut conn).unwrap();
    migrate_schema(&mut conn).unwrap();
    let engine = DatabaseEngine {
        conn,
        blob_store: Some(blob_store.clone()),
    };
    save_session_full(&engine, data).unwrap();
    engine
}

#[test]
fn unknown_payloads_round_trip_verbatim() {
    use nuo_protocol::{Message, Role, TranscriptEntry};
    let engine = DatabaseEngine::open_in_memory().unwrap();
    let mut data = crate::session::SessionData::default();
    data.transcript.push(TranscriptEntry::from_message(
        0,
        &Message::new(Role::User, "known"),
    ));
    save_session_full(&engine, &data).unwrap();

    // Inject an entry a newer binary wrote, with a payload kind this
    // binary cannot decode.
    let future_payload = r#"{"type":"future_kind","x":1}"#;
    engine
            .conn
            .execute(
                "INSERT INTO entries (id, kind, role, content, origin, hidden, created_at_ms, payload) VALUES (?1, 'message', 'user', NULL, NULL, 0, 1, ?2)",
                params!["future-entry", future_payload],
            )
            .unwrap();
    engine
            .conn
            .execute(
                "INSERT INTO entry_memberships (session_id, seq, entry_id) VALUES (?1, 1, 'future-entry')",
                params![data.id],
            )
            .unwrap();

    // Load: the unknown entry rides along, known entries still work.
    let mut reloaded = load_session_full(&engine, &data.id).unwrap().unwrap();
    assert_eq!(reloaded.transcript.entries.len(), 1);
    assert_eq!(reloaded.unknown_entries.len(), 1);
    assert_eq!(reloaded.unknown_entries[0].payload_json, future_payload);
    // The view excludes it and the seq floor prevents collisions.
    assert_eq!(reloaded.transcript.next_seq(), 2);

    // Save (delta): the unknown entry survives byte-identically.
    reloaded.transcript.push(TranscriptEntry::from_message(
        2,
        &Message::new(Role::User, "after"),
    ));
    session_store::save_session_inner(
        &engine.conn,
        engine.blob_store.as_ref(),
        &reloaded,
        false,
        &[],
        &CommitGuard::default(),
    )
    .unwrap();
    let again = load_session_full(&engine, &data.id).unwrap().unwrap();
    assert_eq!(again.unknown_entries.len(), 1);
    assert_eq!(again.unknown_entries[0].payload_json, future_payload);
    assert_eq!(again.transcript.entries.len(), 2);
    assert_eq!(
        again.transcript.entries[1].to_message().unwrap().content,
        "after"
    );
}

#[test]
fn blob_gc_reclaims_only_blobs_absent_from_the_reference_ledger() {
    let engine = DatabaseEngine::open_in_memory().unwrap();
    let blob_store =
        BlobStore::new(std::env::temp_dir().join(format!("nuo-gc-{}", uuid::Uuid::new_v4())));
    let referenced = blob_store.put(b"referenced body").unwrap();
    let orphan = blob_store.put(b"orphan body").unwrap();
    session_store::upsert_session(
        &engine.conn,
        &ThreadRecord {
            id: "s1".into(),
            parent_id: None,
            fork_kind: "trunk".into(),
            fork_point: None,
            title: None,
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
    engine
        .conn
        .execute(
            "INSERT INTO blob_refs (session_id, hash) VALUES ('s1', ?1)",
            params![referenced],
        )
        .unwrap();

    let (count, _) =
        blob_store.retain_only(&garbage_collection::live_blob_hashes(&engine.conn).unwrap());
    assert_eq!(count, 1);
    assert!(
        blob_store.get(&referenced).is_some(),
        "referenced blob survives"
    );
    assert!(blob_store.get(&orphan).is_none(), "orphan is reclaimed");
    let _ = std::fs::remove_dir_all(blob_store.root());
}

#[test]
fn entry_gc_reclaims_only_zero_reference_rows() {
    use nuo_protocol::{Message, Role, TranscriptEntry};
    let engine = DatabaseEngine::open_in_memory().unwrap();
    let mut session = crate::session::SessionData::default();
    let shared = TranscriptEntry::from_message(0, &Message::new(Role::User, "shared"));
    let mut fork = crate::session::SessionData {
        id: "fork-1".into(),
        parent_id: Some(session.id.clone()),
        fork_kind: nuo_protocol::ThreadForkKind::Fork,
        ..Default::default()
    };
    // Both sessions reference the same fact by identity.
    session.transcript.push(shared.clone());
    fork.transcript.push(shared);
    fork.transcript.push(TranscriptEntry::from_message(
        1,
        &Message::new(Role::User, "fork only"),
    ));
    save_session_full(&engine, &session).unwrap();
    save_session_full(&engine, &fork).unwrap();

    // An orphan row with no membership at all.
    engine
            .conn
            .execute(
                "INSERT INTO entries (id, kind, role, content, origin, hidden, created_at_ms, payload) VALUES ('orphan', 'message', 'user', 'orphan', NULL, 0, 1, '{}')",
                [],
            )
            .unwrap();

    let reclaimed = garbage_collection::collect_entry_garbage(&engine.conn).unwrap();
    assert_eq!(reclaimed, 1);
    // The shared fact survives because the fork still references it.
    assert!(load_session_full(&engine, &session.id).unwrap().is_some());
    let fork = load_session_full(&engine, "fork-1").unwrap().unwrap();
    assert_eq!(fork.transcript.entries.len(), 2);
}
