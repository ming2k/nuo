//! Supervision fault injection across every persistence error and transition.

use super::save::save_session_full;
use super::*;

/// Supervision fault-injection suite (D6). Every variant of
/// [`PersistenceError`] and every supervisor transition is exercised.

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
    let health = wait_for_health(&handle.subscribe_health(), |h| *h == WriterHealth::Healthy).await;
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
    let health = wait_for_health(&handle.subscribe_health(), |h| *h == WriterHealth::Healthy).await;
    assert_eq!(health, WriterHealth::Healthy);

    // The respawned writer owns a real engine: the value is there.
    let reader = handle.reader().unwrap();
    assert_eq!(reader.get_kv("after").unwrap().as_deref(), Some("v"));
}

/// Reader-wedge self-heal: removing the on-disk `-wal`/`-shm`
/// pair under a live writer leaves every fresh read-only open failing at
/// the WAL-index attach (`disk I/O error`) while the writer keeps
/// serving and its health stays `Healthy`. Past the failure threshold
/// the handle requests an engine rebuild; the supervisor retires the old
/// generation with the `RetireEngine` handshake — the engine's clean
/// close checkpoints the still-open unlinked WAL back into the main
/// database — and only then opens the fresh generation. Both are
/// asserted: the pre-wedge write survives (recovery), and reader opens
/// succeed afterwards (service). A concurrent fresh open against a live
/// wedged engine is itself a disk-I/O failure, which is why the
/// handshake exists.
#[tokio::test]
async fn reader_wedge_heals_through_engine_rebuild() {
    let (_dir, path) = temp_db();
    let handle = PersistenceHandle::spawn(path.clone(), None);
    // A small commit stays far under the 1000-page autocheckpoint
    // boundary, so its frames live only in the WAL — the wedge deletes
    // exactly the file holding the durable copy.
    let value = format!("pre-wedge-{}", std::process::id());
    handle
        .set_kv("wedge-key".into(), value.clone())
        .await
        .expect("writer must be serving before the wedge");

    // The wedge: delete the WAL sidecars while the writer is live.
    let wedge = |suffix: &str| {
        let sidecar = std::path::PathBuf::from(format!("{}{}", path.display(), suffix));
        let _ = std::fs::remove_file(&sidecar);
    };
    wedge("-wal");
    wedge("-shm");

    // Fresh read-only opens now fail. Crossing the threshold fires the
    // one-shot rebuild request (fire-and-forget; the call stays sync).
    for _ in 0..READER_REBUILD_FAILURE_THRESHOLD {
        assert!(
            handle.reader().is_err(),
            "read-only open must fail against the wedged WAL state"
        );
    }

    // The rebuild lands asynchronously: poll until reader opens succeed.
    let deadline = Instant::now() + Duration::from_secs(10);
    let healed = loop {
        if Instant::now() >= deadline {
            break false;
        }
        if handle.reader().is_ok() {
            break true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    };
    assert!(healed, "engine rebuild never restored reader opens");

    // The pre-wedge write is durable: the retired generation's clean
    // close checkpointed the unlinked WAL into the main database before
    // the fresh generation opened.
    let reader = handle.reader().unwrap();
    assert_eq!(
        reader.get_kv("wedge-key").unwrap().as_deref(),
        Some(value.as_str())
    );
    // A subsequent failure burst starts a fresh episode: the latch and
    // counter were cleared by the successful open.
    assert_eq!(
        handle
            .reader_open_failures
            .load(std::sync::atomic::Ordering::SeqCst),
        0
    );
}

/// D3: a commit that is durable but whose acknowledgement was
/// lost (writer death between COMMIT and ack) is resolved by operation
/// identity on replay — exactly one committed result, no duplicated
/// transcript or usage.
#[tokio::test]
async fn commit_survives_a_lost_acknowledgement_without_duplication() {
    use nuo_protocol::{Message, Role, TranscriptEntry};
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

/// D7: reader pressure is observable — the pool size, the
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

/// Cross-project FTS recall end-to-end — persist a session
/// with a distinctive message, then find it back with a raw
/// multi-word query through `search_history` (the Archivist's
/// retrieval plane). Also pins the workspace filter.
#[test]
fn search_history_finds_persisted_transcripts() {
    use nuo_protocol::{Message, Role, TranscriptEntry};

    let engine = DatabaseEngine::open_in_memory().unwrap();
    let mut data = crate::session::SessionData {
        workspace: Some(nuo_protocol::WorkspaceBinding::new("/tmp/proj-a")),
        title: Some("Retry loop debugging".into()),
        ..Default::default()
    };
    let msg = Message::new(
        Role::User,
        "we need to debug the exponential backoff in the retry loop",
    );
    data.transcript.push(TranscriptEntry::from_message(0, &msg));
    save_session_full(&engine, &data).unwrap();
    crate::db::derived::flush_all(&engine.conn).unwrap();

    // Bare multi-word query (the shape that used to blow up MATCH
    // with `no such column: needle`).
    let hits = history_search::search_history(&engine.conn, "retry backoff", None, 20).unwrap();
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
        history_search::search_history(
            &engine.conn,
            "retry",
            Some(&nuo_protocol::WorkspaceFilter::Path("/tmp/other".into())),
            20,
        )
        .unwrap()
        .is_empty()
    );
    assert_eq!(
        history_search::search_history(
            &engine.conn,
            "retry",
            Some(&nuo_protocol::WorkspaceFilter::Path("/tmp/proj-a".into())),
            20,
        )
        .unwrap()
        .len(),
        1
    );

    // Operator-ish input is sanitized, not executed as FTS grammar.
    assert!(
        history_search::search_history(&engine.conn, "retry OR NOT needle", None, 20).is_ok(),
        "sanitized query must not error"
    );
}

/// Layer 3 (deterministic leg): when the strict AND query
/// recalls nothing because its words never co-occur, the relaxed OR
/// form recalls the entries carrying any word, BM25-ranked so
/// multi-word matches float up.
#[test]
fn relaxed_recall_widens_the_net_after_a_strict_miss() {
    use nuo_protocol::{Message, Role, TranscriptEntry};

    let engine = DatabaseEngine::open_in_memory().unwrap();
    for (id, text) in [
        ("s-retry", "we debugged the retry loop's backoff"),
        ("s-paint", "repaint the widget border only"),
    ] {
        let mut data = crate::session::SessionData {
            id: id.to_string(),
            workspace: Some(nuo_protocol::WorkspaceBinding::new("/tmp/proj-r")),
            title: Some(id.to_string()),
            ..Default::default()
        };
        let msg = Message::new(Role::User, text);
        data.transcript.push(TranscriptEntry::from_message(0, &msg));
        save_session_full(&engine, &data).unwrap();
    }
    crate::db::derived::flush_all(&engine.conn).unwrap();

    // Strict: both words together appear in no single entry.
    let strict = history_search::search_history(&engine.conn, "retry border", None, 20).unwrap();
    assert!(strict.is_empty(), "strict AND must miss: {strict:?}");

    // Relaxed: each entry carries one word; the retry entry also
    // matches more of the query. Both are recalled, OR-joined.
    let relaxed =
        history_search::search_history_relaxed(&engine.conn, "retry border", None, 20).unwrap();
    assert_eq!(relaxed.len(), 2, "both entries surface: {relaxed:?}");
    let ids: std::collections::HashSet<_> = relaxed.iter().map(|h| h.session_id.as_str()).collect();
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

/// The sanitizer converts free text into a safe MATCH
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
