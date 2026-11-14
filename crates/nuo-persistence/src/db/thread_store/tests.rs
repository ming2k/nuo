use super::*;
use nuo_protocol::{BranchPolicy, FactContext};
use nuo_tool::{Message, Role};

fn store() -> SqliteThreadStore {
    let conn = crate::db::migrations::open_in_memory_for_tests().expect("migrated db");
    SqliteThreadStore::new(conn)
}

fn payload(text: &str) -> FactPayload {
    FactPayload::Message {
        message: Message::new(Role::User, text),
    }
}

async fn created_thread() -> (SqliteThreadStore, Thread) {
    let store = store();
    let thread = Thread::new(ThreadId::new("thread-1"), 1_000);
    store.create(thread.clone()).await.unwrap();
    (store, thread)
}

#[tokio::test]
async fn create_then_hydrate_round_trips() {
    let (store, thread) = created_thread().await;
    let hydrated = store
        .hydrate(&ThreadId::new("thread-1"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(hydrated.id, thread.id);
    assert_eq!(hydrated.state.active_branch, BranchId::main());
    assert_eq!(hydrated.state.branches.len(), 1);
    assert_eq!(hydrated.revision, ThreadRevision(0));
}

#[tokio::test]
async fn duplicate_create_is_rejected() {
    let store = store();
    let thread = Thread::new(ThreadId::new("thread-1"), 1_000);
    store.create(thread.clone()).await.unwrap();
    let error = store.create(thread).await.unwrap_err();
    assert!(matches!(error, ThreadStoreError::AlreadyExists(_)));
}

#[tokio::test]
async fn append_then_hydrate_persists_fact_and_branch_head() {
    let (store, mut thread) = created_thread().await;
    let id = thread
        .append(payload("hello"), FactContext::default(), 1_001)
        .unwrap();
    store
        .append(thread.drain_delta(0), ThreadRevision(0))
        .await
        .unwrap();

    let hydrated = store
        .hydrate(&ThreadId::new("thread-1"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(hydrated.max_seq(), 1);
    assert_eq!(hydrated.state.active_branch().unwrap().head, Some(id));
    assert_eq!(hydrated.graph.len(), 1);
}

#[tokio::test]
async fn replay_is_idempotent() {
    let (store, mut thread) = created_thread().await;
    thread
        .append(payload("hello"), FactContext::default(), 1_001)
        .unwrap();
    let delta = thread.drain_delta(0);
    store
        .append(delta.clone(), ThreadRevision(0))
        .await
        .unwrap();
    let ack = store.append(delta, ThreadRevision(0)).await.unwrap();
    assert_eq!(ack.revision, ThreadRevision(1));
}

#[tokio::test]
async fn stale_revision_is_rejected() {
    let (store, mut thread) = created_thread().await;
    thread
        .append(payload("hello"), FactContext::default(), 1_001)
        .unwrap();
    let error = store
        .append(thread.drain_delta(0), ThreadRevision(7))
        .await
        .unwrap_err();
    assert!(matches!(error, ThreadStoreError::RevisionConflict { .. }));
}

#[tokio::test]
async fn fork_delta_persists_branch_cursor() {
    let (store, mut thread) = created_thread().await;
    let first = thread
        .append(payload("hello"), FactContext::default(), 1_001)
        .unwrap();
    store
        .append(thread.drain_delta(0), ThreadRevision(0))
        .await
        .unwrap();
    let branch = thread
        .fork(first, "side", BranchPolicy::default(), 1_002)
        .unwrap();
    store
        .append(thread.drain_delta(1), ThreadRevision(1))
        .await
        .unwrap();

    let hydrated = store
        .hydrate(&ThreadId::new("thread-1"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(hydrated.state.branches.len(), 2);
    assert_eq!(hydrated.state.branch(&branch).unwrap().name, "side");
}

#[tokio::test]
async fn compaction_horizon_persists() {
    let (store, mut thread) = created_thread().await;
    let a = thread
        .append(payload("a"), FactContext::default(), 1_001)
        .unwrap();
    let b = thread
        .append(payload("b"), FactContext::default(), 1_002)
        .unwrap();
    store
        .append(thread.drain_delta(0), ThreadRevision(0))
        .await
        .unwrap();
    thread
        .compact(&BranchId::main(), "summary", b.clone(), 100, 1_003)
        .unwrap();
    store
        .append(thread.drain_delta(2), ThreadRevision(2))
        .await
        .unwrap();

    let hydrated = store
        .hydrate(&ThreadId::new("thread-1"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        hydrated
            .state
            .branch(&BranchId::main())
            .unwrap()
            .compaction_horizon,
        Some(b)
    );
    assert!(
        !hydrated
            .path(&BranchId::main())
            .unwrap()
            .iter()
            .any(|fact| fact.id == a)
    );
}
#[tokio::test]
async fn command_replay_and_fencing_are_durable() {
    let (store, _thread) = created_thread().await;
    store
        .acquire_lease(
            &ThreadId::new("thread-1"),
            "owner-a",
            std::time::Duration::from_secs(60),
        )
        .await
        .unwrap();
    let fact = nuo_protocol::ThreadFact::new(
        FactId::from_raw("fact-1"),
        None,
        1,
        1_001,
        payload("one"),
        FactContext::default(),
    );
    let command = nuo_protocol::CommandEnvelope {
        meta: nuo_protocol::CommandMeta {
            command_id: "cmd-1".to_string(),
            expected_revision: ThreadRevision(0),
            fencing_token: 1,
        },
        command: nuo_protocol::ThreadCommand::AppendFacts { facts: vec![fact] },
    };
    let ack = store
        .execute(&ThreadId::new("thread-1"), command.clone())
        .await
        .unwrap();
    assert!(!ack.replayed);
    let replay = store
        .execute(&ThreadId::new("thread-1"), command)
        .await
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.revision, ack.revision);
}

#[tokio::test]
async fn blob_put_and_gc_keep_referenced_bytes() {
    let (store, mut thread) = created_thread().await;
    let blobs = store.blobs();
    let reference = blobs
        .put(b"tool output", "text/plain", Trust::Trusted)
        .await
        .unwrap();
    thread
        .append(
            FactPayload::Observation {
                call_id: "call-1".to_string(),
                tool_name: "read".to_string(),
                output_ref: reference.clone(),
                metrics: nuo_protocol::ObservationMetrics::default(),
                lifecycle: nuo_protocol::ObservationLifecycle::Raw {
                    content: "tool output".to_string(),
                },
            },
            FactContext::turn(1, 0),
            1_001,
        )
        .unwrap();
    store
        .append(thread.drain_delta(0), ThreadRevision(0))
        .await
        .unwrap();
    assert_eq!(
        blobs.get(&reference.hash).await.unwrap().unwrap(),
        b"tool output"
    );

    let report = store
        .gc(&ThreadId::new("thread-1"), std::time::Duration::ZERO)
        .await
        .unwrap();
    assert_eq!(report.facts_removed, 0);
    assert_eq!(report.blobs_removed, 0);
    assert_eq!(
        blobs.get(&reference.hash).await.unwrap().unwrap(),
        b"tool output"
    );
}

#[tokio::test]
async fn gc_removes_pre_horizon_facts_and_their_blobs() {
    let (store, mut thread) = created_thread().await;
    let blobs = store.blobs();
    let reference = blobs
        .put(b"old output", "text/plain", Trust::Trusted)
        .await
        .unwrap();
    thread
        .append(
            FactPayload::Observation {
                call_id: "call-1".to_string(),
                tool_name: "read".to_string(),
                output_ref: reference.clone(),
                metrics: nuo_protocol::ObservationMetrics::default(),
                lifecycle: nuo_protocol::ObservationLifecycle::Raw {
                    content: "old output".to_string(),
                },
            },
            FactContext::turn(1, 0),
            1_001,
        )
        .unwrap();
    let kept = thread
        .append(payload("kept"), FactContext::default(), 1_002)
        .unwrap();
    store
        .append(thread.drain_delta(0), ThreadRevision(0))
        .await
        .unwrap();
    thread
        .compact(&BranchId::main(), "summary", kept, 10, 1_003)
        .unwrap();
    store
        .append(thread.drain_delta(2), ThreadRevision(2))
        .await
        .unwrap();

    let report = store
        .gc(&ThreadId::new("thread-1"), std::time::Duration::ZERO)
        .await
        .unwrap();
    assert!(report.facts_removed >= 1);
    assert_eq!(report.blobs_removed, 1);
    assert!(blobs.get(&reference.hash).await.unwrap().is_none());
}

#[tokio::test]
async fn stale_fencing_token_is_rejected() {
    let (store, _thread) = created_thread().await;
    let lease = store
        .acquire_lease(
            &ThreadId::new("thread-1"),
            "owner-a",
            std::time::Duration::from_secs(60),
        )
        .await
        .unwrap();
    let command = nuo_protocol::CommandEnvelope {
        meta: nuo_protocol::CommandMeta {
            command_id: "cmd-1".to_string(),
            expected_revision: ThreadRevision(0),
            fencing_token: lease.fencing_token + 1,
        },
        command: nuo_protocol::ThreadCommand::SetPolicy {
            policy: ThreadPolicy::default(),
        },
    };
    let error = store
        .execute(&ThreadId::new("thread-1"), command)
        .await
        .unwrap_err();
    assert_eq!(error, ThreadStoreError::WriterLeaseLost);
}
#[tokio::test]
async fn a_file_store_survives_reopen_with_its_commands_and_lease() {
    let dir = std::env::temp_dir().join(format!("nuo-thread-store-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("nuo.db");
    let store = SqliteThreadStore::open(&path).unwrap();
    let thread = Thread::new(ThreadId::new("thread-1"), 1_000);
    store.create(thread).await.unwrap();
    let lease = store
        .acquire_lease(
            &ThreadId::new("thread-1"),
            "owner-a",
            std::time::Duration::from_secs(60),
        )
        .await
        .unwrap();
    let fact = nuo_protocol::ThreadFact::new(
        FactId::from_raw("fact-1"),
        None,
        1,
        1_001,
        payload("durable"),
        FactContext::default(),
    );
    let command = nuo_protocol::CommandEnvelope {
        meta: nuo_protocol::CommandMeta {
            command_id: "cmd-1".to_string(),
            expected_revision: ThreadRevision(0),
            fencing_token: lease.fencing_token,
        },
        command: nuo_protocol::ThreadCommand::AppendFacts { facts: vec![fact] },
    };
    store
        .execute(&ThreadId::new("thread-1"), command.clone())
        .await
        .unwrap();
    drop(store);

    let reopened = SqliteThreadStore::open(&path).unwrap();
    let hydrated = reopened
        .hydrate(&ThreadId::new("thread-1"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(hydrated.max_seq(), 1);
    let replay = reopened
        .execute(&ThreadId::new("thread-1"), command)
        .await
        .unwrap();
    assert!(replay.replayed);
    let _ = std::fs::remove_dir_all(dir);
}
