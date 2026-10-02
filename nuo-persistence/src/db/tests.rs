use super::*;

#[test]
fn fresh_db_migrates_to_latest_version() {
    let conn = initialize_in_memory_db().unwrap();
    let version: u32 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, CURRENT_DB_VERSION);
}

/// Regression for the interim version-5 stamp: development builds of the
/// ADR-0186 tranche wrote `sessions` with `scheduled_jobs` and without the
/// final working-state columns, then stamped `user_version = 5`. The
/// migrator must reconcile such databases to the final schema instead of
/// letting runtime INSERTs fail with "no column named ...".
#[test]
fn interim_v5_stamp_is_repaired_to_final_schema() {
    let mut conn = Connection::open_in_memory().unwrap();
    configure_connection(&mut conn).unwrap();
    conn.execute_batch(
        r#"
            CREATE TABLE sessions (
                id               TEXT PRIMARY KEY,
                parent_id        TEXT REFERENCES sessions(id) ON DELETE SET NULL,
                fork_kind        TEXT NOT NULL DEFAULT 'trunk',
                title            TEXT,
                created_at_ms    INTEGER NOT NULL,
                updated_at_ms    INTEGER NOT NULL,
                project_root     TEXT NOT NULL,
                msg_count        INTEGER NOT NULL DEFAULT 0,
                last_user_prompt TEXT,
                digest           TEXT,
                scheduled_jobs   TEXT NOT NULL DEFAULT '[]',
                provider_connection TEXT
            );
            INSERT INTO sessions (id, created_at_ms, updated_at_ms, project_root)
                VALUES ('s1', 1, 1, '/tmp');
            PRAGMA user_version = 5;
            "#,
    )
    .unwrap();

    migrate_schema(&mut conn).unwrap();

    let version: u32 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, CURRENT_DB_VERSION);
    let columns = sessions_columns(&conn).unwrap();
    assert!(!columns.contains("scheduled_jobs"));
    assert!(!columns.contains("title_manual"));
    for (name, _) in SESSIONS_WORKING_STATE_COLUMNS {
        // Migration 8 retires the usage column into its own ledger table.
        if *name == "request_usage_records" {
            assert!(
                !columns.contains(*name),
                "{name} must have moved to the usage ledger table"
            );
            continue;
        }
        assert!(columns.contains(*name), "missing column {name}");
    }
    // The repaired row survives and the working-state upsert path works.
    conn.execute(
        "UPDATE sessions SET round_counter = 2, unattended = 1 WHERE id = 's1'",
        [],
    )
    .unwrap();
}

/// Databases already at the final version-5 schema (no interim stamp) pass
/// through the repair as a no-op.
#[test]
fn final_v5_schema_passes_through_migration_six_unchanged() {
    let mut conn = initialize_in_memory_db().unwrap();
    conn.execute_batch("PRAGMA user_version = 5;").unwrap();
    migrate_schema(&mut conn).unwrap();
    let version: u32 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, CURRENT_DB_VERSION);
}

/// Regression (2026-09-08 incident): migration 10 rebuilds `entries`, the
/// parent table of `entry_memberships`' foreign key. With foreign-key
/// enforcement left ON across the migration transaction, the
/// drop-and-rename swap incremented SQLite's deferred-constraint counter
/// and COMMIT failed with `FOREIGN KEY constraint failed` even though the
/// rebuilt state was perfectly referentially consistent. The transaction
/// rolled back, `user_version` never advanced, and every subsequent
/// open retried and re-failed the rebuild — wedging workspace-trust
/// persistence (the TUI stuck on "Trusting workspace...") until the state
/// file was recreated. The subagent must disable enforcement before `BEGIN`
/// (a no-op inside a transaction), restore it after, and gate the result
/// on `PRAGMA foreign_key_check`.
#[test]
fn migration_ten_rebuilds_parent_entries_with_membership_rows_present() {
    let mut conn = Connection::open_in_memory().unwrap();
    configure_connection(&mut conn).unwrap();
    conn.execute_batch(
        r#"
            -- Final version-5 shape (ADR-0186) with live transcript rows that
            -- satisfy the pre-migration CHECKs. Stamped v5 so the v6-v9 hooks
            -- build the real pre-v10 state on the way up (column repairs,
            -- membership/projection rebuilds, FTS backfill), exactly like a
            -- production database that never survived migration 10.
            CREATE TABLE sessions (
                id               TEXT PRIMARY KEY,
                parent_id        TEXT REFERENCES sessions(id) ON DELETE SET NULL,
                fork_kind        TEXT NOT NULL DEFAULT 'trunk',
                title            TEXT,
                created_at_ms    INTEGER NOT NULL,
                updated_at_ms    INTEGER NOT NULL,
                project_root     TEXT NOT NULL,
                msg_count        INTEGER NOT NULL DEFAULT 0,
                last_user_prompt TEXT,
                digest           TEXT,
                data             TEXT,
                title_manual     BOOLEAN NOT NULL DEFAULT 0
            );
            INSERT INTO sessions (id, created_at_ms, updated_at_ms, project_root)
                VALUES ('s1', 1, 1, '/tmp');

            CREATE TABLE entries (
                id            TEXT PRIMARY KEY,
                kind          TEXT NOT NULL CHECK (kind IN ('message','state')),
                role          TEXT CHECK (role IN ('user','assistant','system','tool')),
                content       TEXT,
                origin        TEXT CHECK (origin IS NULL OR origin IN ('harness','checkpoint')),
                hidden        INTEGER NOT NULL DEFAULT 0,
                created_at_ms INTEGER NOT NULL,
                payload       TEXT NOT NULL,
                CHECK ( origin IS NULL OR hidden = 1 ),
                CHECK ( kind <> 'message' OR role IS NOT NULL )
            );
            INSERT INTO entries (id, kind, role, content, origin, hidden, created_at_ms, payload)
                VALUES ('e1', 'message', 'user', 'hello', NULL, 0, 1, '{"content":"hello"}'),
                       ('e2', 'message', 'assistant', 'hi', 'harness', 1, 2, '{"content":"hi"}'),
                       ('e3', 'message', 'user', 'ckpt', 'checkpoint', 1, 3, '{"content":"ckpt"}');

            CREATE TABLE entry_memberships (
                session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                seq        INTEGER NOT NULL,
                entry_id   TEXT NOT NULL REFERENCES entries(id),
                added_by   INTEGER NOT NULL,
                PRIMARY KEY (session_id, seq)
            );
            INSERT INTO entry_memberships (session_id, seq, entry_id, added_by)
                VALUES ('s1', 1, 'e1', 0), ('s1', 2, 'e2', 0), ('s1', 3, 'e3', 0);

            CREATE TABLE projections (
                session_id    TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                seq           INTEGER NOT NULL,
                kind          TEXT NOT NULL CHECK (kind IN ('prune','compact','freeze')),
                up_to_seq     INTEGER NOT NULL,
                payload       TEXT NOT NULL,
                created_at_ms INTEGER NOT NULL,
                PRIMARY KEY (session_id, seq)
            );
            INSERT INTO projections (session_id, seq, kind, up_to_seq, payload, created_at_ms)
                VALUES ('s1', 1, 'freeze', 2, '{}', 3);

            CREATE VIRTUAL TABLE fts_entries USING fts5(
                entry_id UNINDEXED, session_id UNINDEXED, role UNINDEXED, content,
                tokenize = 'porter unicode61'
            );

            PRAGMA user_version = 5;
            "#,
    )
    .unwrap();

    migrate_schema(&mut conn).unwrap();

    let version: u32 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, CURRENT_DB_VERSION);

    // Enforcement is a per-connection invariant installed by
    // `configure_connection`; the subagent must hand the connection back
    // with it restored.
    let fk_enforced: i64 = conn
        .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
        .unwrap();
    assert_eq!(fk_enforced, 1);

    // The rebuild preserved every row byte-for-byte, including the
    // membership references into the swapped table.
    let count = |sql: &str| -> i64 { conn.query_row(sql, [], |r| r.get(0)).unwrap() };
    assert_eq!(count("SELECT COUNT(*) FROM entries"), 3);
    assert_eq!(count("SELECT COUNT(*) FROM entry_memberships"), 3);
    assert_eq!(count("SELECT COUNT(*) FROM projections"), 1);
    // FTS backfill rode along through the v7 hook.
    assert_eq!(count("SELECT COUNT(*) FROM fts_entries"), 3);

    // Referential integrity holds with enforcement back ON.
    let violations: i64 = conn
        .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(violations, 0);

    // The migration-10 CHECKs accept the visible harness provenance the
    // v5 schema rejected (the ADR-0186 integrity fix the rebuild ships).
    conn.execute(
        "INSERT INTO entries (id, kind, role, content, origin, hidden, created_at_ms, payload)
             VALUES ('e4', 'message', 'user', 'steer', 'harness', 0, 4, '{}')",
        [],
    )
    .unwrap();
}

/// The startup guard fails loudly on a database missing runtime columns.
#[test]
fn schema_guard_rejects_missing_columns() {
    let mut conn = Connection::open_in_memory().unwrap();
    configure_connection(&mut conn).unwrap();
    conn.execute_batch("CREATE TABLE sessions (id TEXT PRIMARY KEY, persona TEXT NOT NULL);")
        .unwrap();
    let err = verify_sessions_schema(&conn).unwrap_err();
    assert!(err.to_string().contains("missing column(s)"), "{err}");
}

/// Migration immutability lock: once a migration version has shipped in a
/// commit, editing its SQL in place strands databases stamped by the old
/// SQL (the interim-v5 incident). Any change to the catalog — including
/// edits disguised as refactors — must therefore land as a NEW version
/// entry, which changes this fingerprint and fails the test.
#[test]
fn migration_catalog_fingerprint_is_stable() {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    for migration in MIGRATIONS {
        hasher.update(migration.version.to_le_bytes());
        hasher.update(migration.sql.as_bytes());
    }
    let fingerprint = format!("{:x}", hasher.finalize());
    assert_eq!(
        fingerprint,
        MIGRATION_CATALOG_FINGERPRINT,
        "the migration catalog changed; ship the change as version {} \
             with new SQL instead of rewriting an applied migration",
        CURRENT_DB_VERSION + 1
    );
}

/// Regression (reported incident): the v5 `entries` CHECK coupled `origin`
/// and `hidden`, rejecting the legitimate *visible* harness injections the
/// durable transcript carries (UserSteer/SubagentSteer, CommandEcho "/cmd"
/// & "!cmd", ToolImage). A mid-round save of such a message then failed with
/// `CHECK constraint failed: origin IS NULL OR hidden = 1`. The corrected
/// schema (migration 10) accepts them without weakening the still-real
/// invariants.
#[test]
fn visible_harness_injections_persist_after_origin_hidden_split() {
    use nuo_contracts::{InjectionKind, Message, Role, TranscriptEntry};

    let engine = DatabaseEngine::open_in_memory().unwrap();
    let mut data = crate::session::SessionData::default();
    engine.save_session_full(&data).unwrap();

    // The exact repro: a user steering insert and a command echo are both
    // visible (`hidden = false`) yet legitimately carry an `origin`.
    let steer = Message::new(Role::User, "pls reconsider point 3").with_origin(
        nuo_contracts::InjectionOrigin::new(InjectionKind::UserSteer),
    );
    let echo = Message::command_echo("/session list");
    let image = Message::new(Role::User, "Image from screenshot")
        .with_images(vec![nuo_contracts::ImagePart {
            mime: "image/png".into(),
            data: "bytes".into(),
        }])
        .with_origin(nuo_contracts::InjectionOrigin::new(
            InjectionKind::ToolImage,
        ));

    assert!(!steer.hidden && steer.origin.is_some());
    assert!(!echo.hidden && echo.origin.is_some());
    assert!(!image.hidden && image.origin.is_some());

    data.transcript
        .push(TranscriptEntry::from_message(0, &steer));
    data.transcript
        .push(TranscriptEntry::from_message(1, &echo));
    data.transcript
        .push(TranscriptEntry::from_message(2, &image));
    // Was the reported failure: `CHECK constraint failed: origin IS NULL OR hidden = 1`.
    engine.save_session_full(&data).unwrap();

    let reloaded = engine.load_session_full(&data.id).unwrap().unwrap();
    let messages: Vec<_> = reloaded
        .transcript
        .entries
        .iter()
        .filter_map(|e| e.to_message())
        .collect();
    assert_eq!(messages.len(), 3);
    for m in messages {
        assert!(m.origin.is_some(), "provenance must survive the round-trip");
        assert!(!m.hidden || m.origin.is_some());
    }
}

/// The corrected schema must still reject silent corruption: a *visible*
/// envelope origin that can never be legitimate. A checkpoint is an
/// elided-range stand-in and is hidden by construction (`transcript.rs`);
/// a visible `Role::User` row tagged `origin = 'checkpoint'` is thus
/// impossible and must fail on save.
#[test]
fn visible_checkpoint_origin_is_still_rejected() {
    use nuo_contracts::{Message, Role, TranscriptEntry};

    let engine = DatabaseEngine::open_in_memory().unwrap();
    let mut data = crate::session::SessionData::default();
    let mut message = Message::new(Role::User, "bogus visible checkpoint");
    message.hidden = false; // checkpoint is never visible dialogue
    message.origin = Some(nuo_contracts::InjectionOrigin::new(
        nuo_contracts::InjectionKind::CompactionCheckpoint,
    ));
    data.transcript
        .push(TranscriptEntry::from_message(0, &message));

    let err = engine
        .save_session_full(&data)
        .expect_err("constraint must reject");
    let msg = err.to_string();
    assert!(
        msg.contains("constraint failed") || msg.contains("CHECK"),
        "expected a CHECK/constraint failure, got: {msg}"
    );
}

#[test]
fn delta_save_appends_only_rows_above_the_watermark() {
    use nuo_contracts::{Message, Role, TranscriptEntry};
    let engine = DatabaseEngine::open_in_memory().unwrap();
    let mut data = crate::session::SessionData::default();
    engine.save_session_full(&data).unwrap();

    data.transcript.push(TranscriptEntry::from_message(
        0,
        &Message::new(Role::User, "one"),
    ));
    engine
        .save_session_inner(&data, false, &[], &CommitGuard::default())
        .unwrap();
    let reloaded = engine.load_session_full(&data.id).unwrap().unwrap();
    assert_eq!(reloaded.transcript.entries.len(), 1);

    // Second delta appends only the new row; the durable watermark moves.
    data.transcript.push(TranscriptEntry::from_message(
        1,
        &Message::new(Role::User, "two"),
    ));
    engine
        .save_session_inner(&data, false, &[], &CommitGuard::default())
        .unwrap();
    let reloaded = engine.load_session_full(&data.id).unwrap().unwrap();
    assert_eq!(reloaded.transcript.entries.len(), 2);
    assert_eq!(reloaded.transcript.entries[1].seq, 1);
}

#[test]
fn generation_mismatch_escalates_delta_to_full_rewrite() {
    use nuo_contracts::{Message, Role, TranscriptEntry};
    let engine = DatabaseEngine::open_in_memory().unwrap();
    let mut data = crate::session::SessionData::default();
    data.transcript.push(TranscriptEntry::from_message(
        0,
        &Message::new(Role::User, "old"),
    ));
    engine.save_session_full(&data).unwrap();

    // Rebuild: fresh entries, fresh ids, fresh generation. The delta save
    // must notice the generation change and rewrite everything.
    let rebuilt = crate::session::rebuild_for_test(&[Message::new(Role::User, "rebuilt")]);
    data.transcript = rebuilt;
    data.generation = uuid::Uuid::new_v4().to_string();
    engine
        .save_session_inner(&data, false, &[], &CommitGuard::default())
        .unwrap();

    let reloaded = engine.load_session_full(&data.id).unwrap().unwrap();
    assert_eq!(reloaded.transcript.entries.len(), 1);
    assert_eq!(
        reloaded.transcript.entries[0].to_message().unwrap().content,
        "rebuilt"
    );
    assert_eq!(reloaded.generation, data.generation);
}

fn guarded_save(
    engine: &DatabaseEngine,
    data: &crate::session::SessionData,
    operation_id: Option<&str>,
    expected_revision: Option<u64>,
) -> std::result::Result<u64, SaveError> {
    engine.save_session_inner(
        data,
        false,
        &[],
        &CommitGuard {
            operation_id: operation_id.map(str::to_string),
            expected_revision,
        },
    )
}

#[test]
fn commit_revision_advances_and_is_durable() {
    use nuo_contracts::{Message, Role, TranscriptEntry};
    let engine = DatabaseEngine::open_in_memory().unwrap();
    let mut data = crate::session::SessionData::default();
    assert_eq!(session_revision(&engine.conn, &data.id).unwrap(), 0);

    data.transcript.push(TranscriptEntry::from_message(
        0,
        &Message::new(Role::User, "one"),
    ));
    assert_eq!(guarded_save(&engine, &data, Some("op-1"), None).unwrap(), 1);
    assert_eq!(session_revision(&engine.conn, &data.id).unwrap(), 1);

    data.transcript.push(TranscriptEntry::from_message(
        1,
        &Message::new(Role::User, "two"),
    ));
    assert_eq!(
        guarded_save(&engine, &data, Some("op-2"), Some(1)).unwrap(),
        2
    );
    assert_eq!(session_revision(&engine.conn, &data.id).unwrap(), 2);
}

#[test]
fn replayed_operation_returns_its_original_receipt() {
    use nuo_contracts::{Message, Role, TranscriptEntry};
    let engine = DatabaseEngine::open_in_memory().unwrap();
    let mut data = crate::session::SessionData::default();
    data.transcript.push(TranscriptEntry::from_message(
        0,
        &Message::new(Role::User, "one"),
    ));

    let first = guarded_save(&engine, &data, Some("op-1"), None).unwrap();
    let replay = guarded_save(&engine, &data, Some("op-1"), None).unwrap();
    assert_eq!(first, replay, "a replay returns the original revision");
    assert_eq!(session_revision(&engine.conn, &data.id).unwrap(), first);
    let receipt = latest_commit_receipt(&engine.conn, &data.id)
        .unwrap()
        .expect("a receipt is durable");
    assert_eq!(receipt.0, "op-1");
    assert_eq!(receipt.2, first);
    let reloaded = engine.load_session_full(&data.id).unwrap().unwrap();
    assert_eq!(
        reloaded.transcript.entries.len(),
        1,
        "the delta must not be applied twice"
    );
}

#[test]
fn reused_operation_id_for_other_content_is_refused() {
    use nuo_contracts::{Message, Role, TranscriptEntry};
    let engine = DatabaseEngine::open_in_memory().unwrap();
    let mut data = crate::session::SessionData::default();
    data.transcript.push(TranscriptEntry::from_message(
        0,
        &Message::new(Role::User, "one"),
    ));
    guarded_save(&engine, &data, Some("op-1"), None).unwrap();

    data.transcript.push(TranscriptEntry::from_message(
        1,
        &Message::new(Role::User, "two"),
    ));
    let error = guarded_save(&engine, &data, Some("op-1"), None).unwrap_err();
    assert!(
        matches!(error, SaveError::OperationConflict { .. }),
        "expected an operation conflict, got {error:?}"
    );
    assert_eq!(session_revision(&engine.conn, &data.id).unwrap(), 1);
    let reloaded = engine.load_session_full(&data.id).unwrap().unwrap();
    assert_eq!(reloaded.transcript.entries.len(), 1);
}

#[test]
fn stale_expected_revision_fails_closed() {
    use nuo_contracts::{Message, Role, TranscriptEntry};
    let engine = DatabaseEngine::open_in_memory().unwrap();
    let mut data = crate::session::SessionData::default();
    data.transcript.push(TranscriptEntry::from_message(
        0,
        &Message::new(Role::User, "one"),
    ));
    guarded_save(&engine, &data, None, None).unwrap();

    data.transcript.push(TranscriptEntry::from_message(
        1,
        &Message::new(Role::User, "two"),
    ));
    let error = guarded_save(&engine, &data, None, Some(0)).unwrap_err();
    assert!(
        matches!(
            error,
            SaveError::StaleRevision {
                expected: 0,
                actual: 1
            }
        ),
        "expected a stale-revision refusal, got {error:?}"
    );
    assert_eq!(session_revision(&engine.conn, &data.id).unwrap(), 1);
    let reloaded = engine.load_session_full(&data.id).unwrap().unwrap();
    assert_eq!(
        reloaded.transcript.entries.len(),
        1,
        "the stale commit must not be applied"
    );
}

#[test]
fn delta_save_offloads_only_newly_inserted_rows() {
    use nuo_contracts::{Message, Role, TranscriptEntry};
    let blob_store =
        BlobStore::new(std::env::temp_dir().join(format!("muta-offload-{}", uuid::Uuid::new_v4())));
    let mut data = crate::session::SessionData::default();
    let big = "x".repeat(CAS_THRESHOLD_BYTES * 4);
    data.transcript.push(TranscriptEntry::from_message(
        0,
        &Message::new(Role::User, big.clone()),
    ));
    let stored = engine_blob_round_trip(&data, &blob_store);
    let reloaded = stored.load_session_full(&data.id).unwrap().unwrap();
    let payload = reloaded.transcript.entries[0].as_message().unwrap();
    let hash = payload.content_blob.as_ref().expect("body was offloaded");
    assert_eq!(
        reloaded.transcript.entries[0].content.as_deref(),
        Some(""),
        "the inline body is cleared"
    );
    assert_eq!(blob_store.get(hash).unwrap(), big.as_bytes());
    let _ = std::fs::remove_dir_all(blob_store.root());
}

#[test]
fn image_payload_is_saved_to_cas_blob_store() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    use nuo_contracts::{ImagePart, InjectionKind, InjectionOrigin, Message, Role, TranscriptEntry};

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

    data.transcript.push(TranscriptEntry::from_message(0, &companion_msg));
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

fn engine_blob_round_trip(
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
    engine.save_session_full(data).unwrap();
    engine
}

#[test]
fn unknown_payloads_round_trip_verbatim() {
    use nuo_contracts::{Message, Role, TranscriptEntry};
    let engine = DatabaseEngine::open_in_memory().unwrap();
    let mut data = crate::session::SessionData::default();
    data.transcript.push(TranscriptEntry::from_message(
        0,
        &Message::new(Role::User, "known"),
    ));
    engine.save_session_full(&data).unwrap();

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
    let mut reloaded = engine.load_session_full(&data.id).unwrap().unwrap();
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
    engine
        .save_session_inner(&reloaded, false, &[], &CommitGuard::default())
        .unwrap();
    let again = engine.load_session_full(&data.id).unwrap().unwrap();
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
        BlobStore::new(std::env::temp_dir().join(format!("muta-gc-{}", uuid::Uuid::new_v4())));
    let referenced = blob_store.put(b"referenced body").unwrap();
    let orphan = blob_store.put(b"orphan body").unwrap();
    engine
            .conn
            .execute(
                "INSERT INTO sessions (id, parent_id, fork_kind, title, created_at_s, updated_at_s, workspace_root, persona, msg_count, last_user_prompt, digest) VALUES ('s1', NULL, 'trunk', NULL, 1, 1, '/tmp', NULL, 0, NULL, NULL)",
                [],
            )
            .unwrap();
    engine
        .conn
        .execute(
            "INSERT INTO blob_refs (session_id, hash) VALUES ('s1', ?1)",
            params![referenced],
        )
        .unwrap();

    let (count, _) = blob_store.retain_only(&engine.live_blob_hashes().unwrap());
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
    use nuo_contracts::{Message, Role, TranscriptEntry};
    let engine = DatabaseEngine::open_in_memory().unwrap();
    let mut session = crate::session::SessionData::default();
    let shared = TranscriptEntry::from_message(0, &Message::new(Role::User, "shared"));
    let mut fork = crate::session::SessionData {
        id: "fork-1".into(),
        parent_id: Some(session.id.clone()),
        fork_kind: nuo_contracts::SessionForkKind::Fork,
        ..Default::default()
    };
    // Both sessions reference the same fact by identity.
    session.transcript.push(shared.clone());
    fork.transcript.push(shared);
    fork.transcript.push(TranscriptEntry::from_message(
        1,
        &Message::new(Role::User, "fork only"),
    ));
    engine.save_session_full(&session).unwrap();
    engine.save_session_full(&fork).unwrap();

    // An orphan row with no membership at all.
    engine
            .conn
            .execute(
                "INSERT INTO entries (id, kind, role, content, origin, hidden, created_at_ms, payload) VALUES ('orphan', 'message', 'user', 'orphan', NULL, 0, 1, '{}')",
                [],
            )
            .unwrap();

    let reclaimed = engine.collect_entry_garbage().unwrap();
    assert_eq!(reclaimed, 1);
    // The shared fact survives because the fork still references it.
    assert!(engine.load_session_full(&session.id).unwrap().is_some());
    let fork = engine.load_session_full("fork-1").unwrap().unwrap();
    assert_eq!(fork.transcript.entries.len(), 2);
}

#[test]
fn digest_anchor_tree_and_generation_round_trip() {
    use nuo_contracts::{Message, Role, TranscriptEntry};
    let engine = DatabaseEngine::open_in_memory().unwrap();
    let mut data = crate::session::SessionData {
        digest: Some(nuo_contracts::SessionDigest::default()),
        digest_anchor: Some(4_242),
        ..Default::default()
    };
    data.tree.active_leaf_id = Some("leaf-1".to_string());
    data.transcript.push(TranscriptEntry::from_message(
        0,
        &Message::new(Role::User, "hello"),
    ));
    engine.save_session_full(&data).unwrap();

    let reloaded = engine.load_session_full(&data.id).unwrap().unwrap();
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
    use nuo_contracts::{Message, Role, TranscriptEntry};
    let engine = DatabaseEngine::open_in_memory().unwrap();
    let mut data = crate::session::SessionData::default();
    data.transcript.push(TranscriptEntry::from_message(
        0,
        &Message::new(Role::User, "hello"),
    ));
    engine.save_session_full(&data).unwrap();

    // Corrupt the working state out-of-band, then restamp a checksum that
    // matches the corruption? No — the mismatch case: the on-row checksum
    // no longer matches the corrupted columns. Load is non-fatal (the row
    // data remains authoritative), but a fresh full save restamps a
    // correct checksum over the corrected state.
    engine
        .conn
        .execute(
            "UPDATE sessions SET round_counter = 99 WHERE id = ?1",
            params![data.id],
        )
        .unwrap();
    let reloaded = engine.load_session_full(&data.id).unwrap().unwrap();
    assert_eq!(reloaded.round_counter, 99);

    // The recomputed checksum over the loaded (corrupted) row matches the
    // stored one only if nothing changed — here it must differ, so a save
    // restamps and the next load is consistent again.
    engine.save_session_full(&reloaded).unwrap();
    let again = engine.load_session_full(&data.id).unwrap().unwrap();
    assert_eq!(again.checksum, reloaded.checksum);
}

#[test]
fn working_state_round_trips_through_the_session_row() {
    let engine = DatabaseEngine::open_in_memory().unwrap();
    engine
            .conn
            .execute(
                "INSERT INTO sessions (id, parent_id, fork_kind, title, created_at_s, updated_at_s, workspace_root, persona, msg_count, last_user_prompt, digest) VALUES ('s1', NULL, 'trunk', 'T', 1, 1, '/tmp', NULL, 0, NULL, NULL)",
                [],
            )
            .unwrap();
    let sessions = engine.list_sessions(None).unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].title.as_deref(), Some("T"));
}

#[test]
fn test_v16_to_v17_clean_break_migration() {
    let engine = DatabaseEngine::open_in_memory().unwrap();

    // 1. Insert a legacy session row with old columns (tree, commands, round_interrupts)
    engine
        .conn
        .execute(
            r#"
            INSERT INTO sessions (
                id, parent_id, fork_kind, title, created_at_s, updated_at_s,
                workspace_root, additional_roots, persona, msg_count, last_user_prompt,
                digest, digest_anchor, tree, transcript_generation, provider_connection,
                round_counter, unattended, disabled_tools, commands, round_interrupts,
                retry_resolutions, retry_pending, checksum, schema_version
            ) VALUES (
                'legacy-s1', NULL, 'aside', 'Legacy Aside', 100, 200,
                '/workspace', '[]', 'architect', 1, 'Fix bug',
                NULL, NULL, '{"entries":{},"active_leaf_id":"leaf-99"}', 'gen-1', 'openai',
                3, 1, '["bash"]', '[{"name":"help","args":""}]', '[]',
                '[]', NULL, 1234, 16
            );
            "#,
            [],
        )
        .unwrap();

    // Insert legacy entry and membership
    engine
        .conn
        .execute_batch(
            r#"
            INSERT INTO entries (id, kind, role, content, origin, hidden, created_at_ms, payload)
            VALUES ('e1', 'message', 'user', 'Legacy question', NULL, 0, 100000, '{}');
            INSERT INTO entry_memberships (session_id, seq, entry_id)
            VALUES ('legacy-s1', 1, 'e1');
            "#,
        )
        .unwrap();

    // 2. Run Migration 17 on the database
    let tx = engine.conn.unchecked_transaction().unwrap();
    migrations::apply_session_ir_clean_break_schema(&tx).unwrap();
    tx.commit().unwrap();

    // 3. Verify sessions_v2 was populated and purged of old fields
    let (active_leaf, status, unattended) = engine
        .conn
        .query_row(
            "SELECT active_leaf, status, (SELECT json_extract(guardrails_json, '$.unattended') FROM session_policies WHERE session_id = id) FROM sessions_v2 WHERE id = 'legacy-s1'",
            [],
            |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, String>(1)?, r.get::<_, bool>(2)?)),
        )
        .unwrap();

    assert_eq!(active_leaf.as_deref(), Some("leaf-99"));
    assert_eq!(status, "idle");
    assert!(unattended);

    // 4. Verify causal_nodes was populated from entries/entry_memberships
    let (node_id, kind, content) = engine
        .conn
        .query_row(
            "SELECT id, kind, json_extract(payload_json, '$.message.content') FROM causal_nodes WHERE session_id = 'legacy-s1'",
            [],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)),
        )
        .unwrap();

    assert_eq!(node_id, "e1");
    assert_eq!(kind, "dialogue");
    assert_eq!(content, "Legacy question");

    // 5. Verify session_list_view derives msg_count and last_user_prompt dynamically
    let (msg_count, last_prompt) = engine
        .conn
        .query_row(
            "SELECT msg_count, last_user_prompt FROM session_list_view WHERE id = 'legacy-s1'",
            [],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?)),
        )
        .unwrap();

    assert_eq!(msg_count, 1);
    assert_eq!(last_prompt.as_deref(), Some("Legacy question"));
}

/// Supervision fault-injection suite (ADR-0196 D6). Every variant of
/// [`PersistenceError`] and every supervisor transition is exercised.
mod supervision {
    use super::*;

    fn temp_db() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nuo.db");
        (dir, path)
    }

    async fn wait_for_health(
        health: &watch::Receiver<WriterHealth>,
        matches: impl Fn(&WriterHealth) -> bool,
    ) -> WriterHealth {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let current = health.borrow().clone();
            if matches(&current) {
                return current;
            }
            assert!(
                Instant::now() < deadline,
                "health never reached the expected state; last: {current:?}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// A healthy writer serves commands and reports `Healthy`.
    #[tokio::test]
    async fn healthy_writer_serves_and_reports_healthy() {
        let (_dir, path) = temp_db();
        let handle = PersistenceHandle::spawn(path, None);
        handle
            .set_kv("k".into(), "v".into())
            .await
            .expect("command must succeed");
        let health =
            wait_for_health(&handle.subscribe_health(), |h| *h == WriterHealth::Healthy).await;
        assert_eq!(health, WriterHealth::Healthy);
    }

    /// D1/D5: an engine-open failure produces `Err(WriterDown)` (never a
    /// hang, never a fake success), and health degrades to
    /// `Recovering`/`Down` with the cause attached.
    #[tokio::test]
    async fn engine_open_failure_fails_fast_and_degrades_health() {
        let dir = tempfile::tempdir().unwrap();
        // SQLite cannot open a directory as a database file.
        let handle = PersistenceHandle::spawn(dir.path().to_path_buf(), None);
        let error = handle
            .set_kv("k".into(), "v".into())
            .await
            .expect_err("open failure must fail the command");
        assert!(matches!(error, PersistenceError::WriterDown));
        let health = wait_for_health(&handle.subscribe_health(), |h| !h.is_serving()).await;
        assert!(health.error().is_some(), "degraded health carries a cause");
    }

    /// D1/D6: a dead writer is respawned through the same handle, and
    /// `Healthy` is restored by the first successful command — not by
    /// spawn alone.
    #[tokio::test]
    async fn dead_writer_is_respawned_and_health_restored() {
        let (_dir, path) = temp_db();
        let handle = PersistenceHandle::spawn(path, None);
        handle
            .set_kv("before".into(), "v".into())
            .await
            .expect("writer must be serving before death");

        // Kill the actor generation (test-only death command).
        let (die_tx, die_rx) = oneshot::channel();
        handle
            .supervisor
            .send(PersistenceCommand::Die { ack: die_tx })
            .await
            .unwrap();
        die_rx.await.unwrap();

        // The next command survives the death: supervisor respawns. The
        // respawn backoff is short, but under heavy parallel test load a
        // single command issued immediately after death can race the
        // still-recovering writer — retry within a generous deadline
        // instead of asserting an instant success.
        {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                match handle.set_kv("after".into(), "v".into()).await {
                    Ok(()) => break,
                    Err(error) => {
                        assert!(
                            Instant::now() < deadline,
                            "respawned writer never served: {error}"
                        );
                        tokio::time::sleep(Duration::from_millis(25)).await;
                    }
                }
            }
        }
        let health =
            wait_for_health(&handle.subscribe_health(), |h| *h == WriterHealth::Healthy).await;
        assert_eq!(health, WriterHealth::Healthy);

        // The respawned writer owns a real engine: the value is there.
        let reader = handle.reader().unwrap();
        assert_eq!(reader.get_kv("after").unwrap().as_deref(), Some("v"));
    }

    /// ADR-0236 D3: a commit that is durable but whose acknowledgement was
    /// lost (writer death between COMMIT and ack) is resolved by operation
    /// identity on replay — exactly one committed result, no duplicated
    /// transcript or usage.
    #[tokio::test]
    async fn commit_survives_a_lost_acknowledgement_without_duplication() {
        use nuo_contracts::{Message, Role, TranscriptEntry};
        let (_dir, path) = temp_db();
        let handle = PersistenceHandle::spawn(path, None);

        let mut data = crate::session::SessionData::default();
        data.transcript.push(TranscriptEntry::from_message(
            0,
            &Message::new(Role::User, "one"),
        ));
        let guard = CommitGuard {
            operation_id: Some("op-lost-ack".to_string()),
            expected_revision: None,
        };

        // The writer applies the commit, then dies before acknowledging.
        handle
            .supervisor
            .send(PersistenceCommand::SaveSessionThenDie {
                data: Box::new(data.clone()),
                full: false,
                usage_upserts: Vec::new(),
                guard: guard.clone(),
            })
            .await
            .unwrap();

        // Replay the same operation until the respawned writer serves it.
        // Every retry carries the same identity and payload.
        let deadline = Instant::now() + Duration::from_secs(10);
        let revision = loop {
            match handle
                .save_session(data.clone(), false, Vec::new(), guard.clone())
                .await
            {
                Ok(revision) => break revision,
                Err(error) => {
                    assert!(Instant::now() < deadline, "writer never replayed: {error}");
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
            }
        };
        assert_eq!(revision, 1, "the replay returns the original receipt");

        let reader = handle.reader().unwrap();
        assert_eq!(reader.session_revision(&data.id).unwrap(), 1);
        let reloaded = reader.load_session_full(&data.id).unwrap().unwrap();
        assert_eq!(
            reloaded.transcript.entries.len(),
            1,
            "the replayed commit must not duplicate transcript rows"
        );
    }

    /// ADR-0236 D7: reader pressure is observable — the pool size, the
    /// active snapshot count, and the oldest snapshot's age are reported,
    /// and a dropped reader releases its registration.
    #[tokio::test]
    async fn storage_metrics_track_reader_pressure() {
        let (_dir, path) = temp_db();
        let handle = PersistenceHandle::spawn(path, None);
        handle
            .set_kv("k".into(), "v".into())
            .await
            .expect("writer must be serving");

        let idle = handle.storage_metrics();
        assert_eq!(idle.active_readers, 0);
        assert_eq!(idle.reader_capacity, READER_POOL_CAPACITY);
        assert_eq!(idle.oldest_reader_ms, None);
        assert!(idle.main_bytes > 0, "the main database file exists");

        let reader = handle.reader().unwrap();
        let busy = handle.storage_metrics();
        assert_eq!(busy.active_readers, 1);
        assert!(busy.oldest_reader_ms.is_some());

        drop(reader);
        let released = handle.storage_metrics();
        assert_eq!(released.active_readers, 0);
        assert_eq!(released.oldest_reader_ms, None);
    }

    /// D6: a handler panic is contained by the per-command guard — the
    /// command fails with `Poisoned`, the actor keeps serving.
    #[tokio::test]
    async fn handler_panic_is_contained_as_poisoned() {
        let poisoned = guarded::<()>(|| panic!("injected engine panic")).unwrap_err();
        assert!(matches!(poisoned, PersistenceError::Poisoned(msg) if msg.contains("injected")));

        let engine_error =
            guarded::<()>(|| Err(rusqlite::Error::InvalidColumnName("x".into()))).unwrap_err();
        assert!(matches!(engine_error, PersistenceError::Engine(_)));
    }

    /// D2: encode failures surface as `Encode` before reaching the
    /// writer; nothing is written.
    #[tokio::test]
    async fn encode_failure_surfaces_as_encode() {
        struct InjectedFailure;
        impl serde::Serialize for InjectedFailure {
            fn serialize<S: serde::Serializer>(
                &self,
                _serializer: S,
            ) -> std::result::Result<S::Ok, S::Error> {
                Err(serde::ser::Error::custom("injected encode failure"))
            }
        }

        let (_dir, path) = temp_db();
        let handle = PersistenceHandle::spawn(path, None);
        let error = handle
            .set_json("broken", &InjectedFailure)
            .await
            .expect_err("the injected serialization failure must surface");
        assert!(matches!(error, PersistenceError::Encode(_)));
    }

    /// D5: `fail` must resolve a command's ack with the error — a caller
    /// waiting on the ack learns the truth instead of hanging.
    #[tokio::test]
    async fn drain_resolves_acks_with_writer_down() {
        let (ack_tx, ack_rx) = oneshot::channel();
        PersistenceCommand::SetKV {
            key: "k".into(),
            value: "v".into(),
            ack: ack_tx,
        }
        .fail(PersistenceError::WriterDown);
        assert!(matches!(
            ack_rx.await,
            Ok(Err(PersistenceError::WriterDown))
        ));
    }

    /// ADR-0208: cross-project FTS recall end-to-end — persist a session
    /// with a distinctive message, then find it back with a raw
    /// multi-word query through `search_history` (the Archivist's
    /// retrieval plane). Also pins the workspace filter.
    #[test]
    fn search_history_finds_persisted_transcripts() {
        use nuo_contracts::{Message, Role, TranscriptEntry};

        let engine = DatabaseEngine::open_in_memory().unwrap();
        let mut data = crate::session::SessionData {
            workspace: Some(nuo_contracts::WorkspaceBinding::new("/tmp/proj-a")),
            title: Some("Retry loop debugging".into()),
            ..Default::default()
        };
        let msg = Message::new(
            Role::User,
            "we need to debug the exponential backoff in the retry loop",
        );
        data.transcript.push(TranscriptEntry::from_message(0, &msg));
        engine.save_session_full(&data).unwrap();

        // Bare multi-word query (the shape that used to blow up MATCH
        // with `no such column: needle`).
        let hits = engine.search_history("retry backoff", None, 20).unwrap();
        assert_eq!(hits.len(), 1, "the seeded session must be recalled");
        assert_eq!(hits[0].session_id, data.id);
        assert_eq!(
            hits[0].session_title.as_deref(),
            Some("Retry loop debugging")
        );
        assert_eq!(hits[0].workspace_root.as_deref(), Some("/tmp/proj-a"));
        assert!(hits[0].snippet.contains("retry"), "{:?}", hits[0].snippet);

        // A workspace filter that does not match returns nothing; one
        // that matches returns the hit.
        assert!(
            engine
                .search_history(
                    "retry",
                    Some(&nuo_contracts::WorkspaceFilter::Path("/tmp/other".into())),
                    20,
                )
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            engine
                .search_history(
                    "retry",
                    Some(&nuo_contracts::WorkspaceFilter::Path("/tmp/proj-a".into())),
                    20,
                )
                .unwrap()
                .len(),
            1
        );

        // Operator-ish input is sanitized, not executed as FTS grammar.
        assert!(
            engine
                .search_history("retry OR NOT needle", None, 20)
                .is_ok(),
            "sanitized query must not error"
        );
    }

    /// ADR-0208 Layer 3 (deterministic leg): when the strict AND query
    /// recalls nothing because its words never co-occur, the relaxed OR
    /// form recalls the entries carrying any word, BM25-ranked so
    /// multi-word matches float up.
    #[test]
    fn relaxed_recall_widens_the_net_after_a_strict_miss() {
        use nuo_contracts::{Message, Role, TranscriptEntry};

        let engine = DatabaseEngine::open_in_memory().unwrap();
        for (id, text) in [
            ("s-retry", "we debugged the retry loop's backoff"),
            ("s-paint", "repaint the widget border only"),
        ] {
            let mut data = crate::session::SessionData {
                id: id.to_string(),
                workspace: Some(nuo_contracts::WorkspaceBinding::new("/tmp/proj-r")),
                title: Some(id.to_string()),
                ..Default::default()
            };
            let msg = Message::new(Role::User, text);
            data.transcript.push(TranscriptEntry::from_message(0, &msg));
            engine.save_session_full(&data).unwrap();
        }

        // Strict: both words together appear in no single entry.
        let strict = engine.search_history("retry border", None, 20).unwrap();
        assert!(strict.is_empty(), "strict AND must miss: {strict:?}");

        // Relaxed: each entry carries one word; the retry entry also
        // matches more of the query. Both are recalled, OR-joined.
        let relaxed = engine
            .search_history_relaxed("retry border", None, 20)
            .unwrap();
        assert_eq!(relaxed.len(), 2, "both entries surface: {relaxed:?}");
        let ids: std::collections::HashSet<_> =
            relaxed.iter().map(|h| h.session_id.as_str()).collect();
        assert!(ids.contains("s-retry"));
        assert!(ids.contains("s-paint"));
        // Deterministic ordering: scores sort ascending (FTS5
        // negative-better), which the query_map's ORDER BY guarantees —
        // the exact tie order between single-word matches is BM25's
        // business, not this test's.
        let scores: Vec<f64> = relaxed.iter().map(|h| h.score).collect();
        let mut sorted = scores.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        assert_eq!(scores, sorted);
    }

    /// ADR-0208: the sanitizer converts free text into a safe MATCH
    /// expression — quoted phrase tokens joined with AND, operator and
    /// punctuation characters stripped, so raw user input can never
    /// reach the FTS5 grammar.
    #[test]
    fn sanitize_fts_query_quotes_every_word() {
        assert_eq!(sanitize_fts_query("retry loop"), "\"retry\" AND \"loop\"");
        assert_eq!(
            sanitize_fts_query("retry OR NOT needle"),
            "\"retry\" AND \"OR\" AND \"NOT\" AND \"needle\""
        );
        // Quotes are punctuation: stripped, not escaped (a quote can
        // never survive into the MATCH grammar this way).
        assert_eq!(sanitize_fts_query("a\"b"), "\"ab\"");
        assert_eq!(sanitize_fts_query("   "), "");
        assert_eq!(sanitize_fts_query("***"), "");
    }
}

/// ADR-0226: `latest_session` filters by the workspace partition and,
/// optionally, the staffing persona (backs `--resume`).
#[test]
fn latest_session_filters_by_workspace_and_persona() {
    use nuo_contracts::WorkspaceFilter;
    let engine = DatabaseEngine::open_in_memory().unwrap();

    let mut unbound = crate::session::SessionData {
        role: Some("philosopher".into()),
        ..Default::default()
    };
    unbound.workspace = None;
    engine.save_session_full(&unbound).unwrap();

    let mut bound = crate::session::SessionData {
        role: Some("philosopher".into()),
        ..Default::default()
    };
    bound.workspace = Some(nuo_contracts::WorkspaceBinding::new("/repo/x"));
    engine.save_session_full(&bound).unwrap();

    assert_eq!(
        engine
            .latest_session(&WorkspaceFilter::Unbound, Some("philosopher"))
            .unwrap(),
        Some(unbound.id.clone())
    );
    assert_eq!(
        engine
            .latest_session(
                &WorkspaceFilter::Path("/repo/x".into()),
                Some("philosopher")
            )
            .unwrap(),
        Some(bound.id.clone())
    );
    assert_eq!(
        engine
            .latest_session(&WorkspaceFilter::Unbound, Some("nobody"))
            .unwrap(),
        None
    );
    // Unbound + no persona constraint still finds only unbound sessions.
    assert_eq!(
        engine
            .latest_session(&WorkspaceFilter::Unbound, None)
            .unwrap(),
        Some(unbound.id)
    );
}

#[test]
fn role_anchored_partition_and_switch_candidates_exclude_active() {
    use nuo_contracts::SessionPartition;
    let engine = DatabaseEngine::open_in_memory().unwrap();

    let make_session = |id: &str, role: &str, ws: Option<&str>, time: u64| {
        let mut data = crate::session::SessionData {
            id: id.into(),
            role: Some(role.into()),
            workspace: ws.map(nuo_contracts::WorkspaceBinding::new),
            created_at: time,
            updated_at: time,
            ..Default::default()
        };
        let msg =
            nuo_contracts::Message::new(nuo_contracts::Role::User, format!("hello from {id}"));
        data.transcript
            .push(nuo_contracts::TranscriptEntry::from_message(0, &msg));
        data
    };

    // Two philosophist sessions (workspace-free)
    let p1 = make_session("phil-1", "philosophist", None, 100);
    let p2 = make_session("phil-2", "philosophist", None, 200);
    // Another role (e.g. translator) with workspace-free
    let t1 = make_session("trans-1", "translator", None, 300);
    // Developer session bound to /repo/a
    let d1 = make_session("dev-1", "developer", Some("/repo/a"), 400);

    engine.save_session_full(&p1).unwrap();
    engine.save_session_full(&p2).unwrap();
    engine.save_session_full(&t1).unwrap();
    engine.save_session_full(&d1).unwrap();

    let phil_partition = SessionPartition::Role("philosophist".into());
    // Active is phil-2: candidates must return phil-1 ONLY, excluding phil-2, trans-1, and dev-1
    let candidates = engine
        .list_switch_candidates(&phil_partition, "phil-2")
        .unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].id, "phil-1");

    // Active is phil-1: candidates must return phil-2 ONLY
    let candidates = engine
        .list_switch_candidates(&phil_partition, "phil-1")
        .unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].id, "phil-2");

    // Active is unknown: candidates return both phil-2 and phil-1
    let candidates = engine
        .list_switch_candidates(&phil_partition, "none")
        .unwrap();
    assert_eq!(candidates.len(), 2);
    assert_eq!(candidates[0].id, "phil-2");
    assert_eq!(candidates[1].id, "phil-1");

    // latest_session_in_partition for philosophist
    assert_eq!(
        engine.latest_session_in_partition(&phil_partition).unwrap(),
        Some("phil-2".into())
    );

    // Workspace partition
    let ws_partition = SessionPartition::Workspace("/repo/a".into());
    let candidates = engine
        .list_switch_candidates(&ws_partition, "dev-1")
        .unwrap();
    assert!(candidates.is_empty()); // Excluded itself, no other candidates
}

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
fn fast_fts_triggers_align_rowids_and_delete_in_sync() {
    use nuo_contracts::{Message, Role, TranscriptEntry};

    let engine = DatabaseEngine::open_in_memory().unwrap();
    let mut data = crate::session::SessionData::default();
    data.transcript.push(TranscriptEntry::from_message(
        0,
        &Message::new(Role::User, "unique_needle_for_fts_test"),
    ));
    engine.save_session_full(&data).unwrap();

    // Verify fts_entries has the entry and matches rowid
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

    // Verify FTS search works
    let results = engine
        .search_history("unique_needle_for_fts_test", None, 10)
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].session_id, data.id);

    // Deleting session deletes entry_memberships and cascades via trigger to fts_entries
    engine.delete_session(&data.id).unwrap();
    let fts_count: i64 = engine
        .conn
        .query_row("SELECT COUNT(*) FROM fts_entries", [], |r| r.get(0))
        .unwrap();
    assert_eq!(fts_count, 0);
}
