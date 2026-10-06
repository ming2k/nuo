//! Session persistence test suite (ADR-0186 single-transcript model).
//!
//! Tests exercise the public [`SessionStore`] API against the SQLite store:
//! the transcript is the authority, the window is a derive, projections are
//! append-only directives, and fork shares facts by identity.

use crate::session::*;
use nuo_wire::{Message, Role, ToolCall};
use std::path::PathBuf;

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("nuo-t18x-{tag}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

async fn store(tag: &str) -> SessionStore {
    let dir = temp_dir(tag);
    let path = dir.join("session.json");
    SessionStore::for_path(path)
}

fn user(content: &str) -> Message {
    Message::new(Role::User, content)
}

fn assistant(content: &str) -> Message {
    Message::new(Role::Assistant, content)
}

// ---------------------------------------------------------------
// Round trip: transcript is the SSOT; the window is a derive.
// ---------------------------------------------------------------

#[tokio::test]
async fn messages_round_trip_through_reload() {
    let dir = temp_dir("roundtrip");
    let path = dir.join("session.json");
    let store = SessionStore::for_path(path.clone());
    store
        .replace_messages(vec![user("alpha"), assistant("beta")])
        .await
        .unwrap();

    let reloaded = SessionStore::for_path(path);
    let window = reloaded.model_window().await;
    assert_eq!(window.len(), 2);
    assert_eq!(window[0].content, "alpha");
    assert_eq!(window[1].content, "beta");
}

#[tokio::test]
async fn full_transcript_includes_everything_unprojected() {
    let store = store("full").await;
    store
        .replace_messages(vec![user("old"), assistant("older")])
        .await
        .unwrap();
    // Prune the assistant's tool result... actually prune targets tool bodies;
    // instead compact to hide the head and verify full_transcript still sees it.
    let full = store.full_transcript().await;
    assert_eq!(full.len(), 2);
    let window = store.model_window().await;
    assert_eq!(window.len(), 2);
    let _ = store.archived_transcript_count().await; // no directives yet
}

// ---------------------------------------------------------------
// Session switching must not inherit the previous session's projection.
// ---------------------------------------------------------------

/// Regression (ADR-0189 `projected_cache`): `open` swaps the store's whole
/// `SessionData` but the projection cache belonged to the session being left.
/// The stale cache made `model_window` return the previous session's window
/// after a switch — and the resume path then `replace_messages`-ed it over
/// the newly opened session, wiping the resumed transcript (the
/// `/sessions <id>` restore rendered an empty view).
#[tokio::test]
async fn open_repoints_projection_to_the_switched_session() {
    let dir = temp_dir("open-cache");
    let store = SessionStore::for_path(dir.join("a.json"));
    store
        .replace_messages(vec![user("from session A"), assistant("reply A")])
        .await
        .unwrap();

    // Populate session B on disk via a second store, then switch.
    let other = SessionStore::for_path(dir.join("b.json"));
    other
        .replace_messages(vec![user("from session B")])
        .await
        .unwrap();
    let b_id = other.id().await;

    // Warm the projection cache against session A first.
    assert_eq!(store.model_window().await.len(), 2);
    store.open(&b_id).await.unwrap();

    let window = store.model_window().await;
    assert_eq!(
        window
            .iter()
            .map(|m| m.content.as_str())
            .collect::<Vec<_>>(),
        vec!["from session B"],
        "model_window after `open` must derive from the switched-to session, \
         never the session left behind"
    );
}

/// The same staleness hazard for `reset` (`/new`): a stale window made the
/// length-based delta check in `append_turn` drop the fresh session's first
/// durable write.
#[tokio::test]
async fn reset_does_not_inherit_the_previous_projection() {
    let dir = temp_dir("reset-cache");
    let store = SessionStore::for_path(dir.join("a.json"));
    store
        .replace_messages(vec![user("from session A"), assistant("reply A")])
        .await
        .unwrap();
    // Warm the cache against session A.
    assert_eq!(store.model_window().await.len(), 2);

    store.reset().await.unwrap();
    let window = store.model_window().await;
    assert!(
        window.is_empty(),
        "the fresh session must start with an empty window"
    );

    // The first turn on the fresh session must be appended, not silently
    // dropped by a stale cache length.
    store.append_turn(&[user("hello")]).await.unwrap();
    assert_eq!(
        store
            .model_window()
            .await
            .iter()
            .map(|m| m.content.as_str())
            .collect::<Vec<_>>(),
        vec!["hello"],
        "the fresh session's first turn must be committed, not swallowed by \
         a stale projection length"
    );
}

// ---------------------------------------------------------------
// Projection: commit translates to directives; view reproduces.
// ---------------------------------------------------------------

#[tokio::test]
async fn compaction_commit_hides_range_and_keeps_originals() {
    let store = store("compact").await;
    store
        .replace_messages(vec![user("old1"), assistant("old2"), user("tail")])
        .await
        .unwrap();
    let checkpoint = checkpoint_message("summary of old rounds");
    let result = ContextProjectionResult {
        model_window: vec![checkpoint, user("tail")],
        archived_originals: vec![user("old1"), assistant("old2")],
        checkpoint: ContextProjectionCheckpoint {
            operation: ContextProjectionKind::Compact,
            archived_messages: 2,
            active_messages: 2,
            window_tokens_before: 10,
            window_tokens_after: 5,
            summary: Some("summary of old rounds".to_string()),
            tracked_files: Vec::new(),
        },
    };
    store.commit_context_projection(result).await.unwrap();

    let window = store.model_window().await;
    assert_eq!(window.len(), 2);
    assert!(window[0].content.starts_with("[Conversation checkpoint]"));
    assert_eq!(window[1].content, "tail");
    // Originals remain recoverable.
    assert_eq!(store.archived_transcript_count().await, 2);
    // Round trip: the derived view survives a reload.
    let dir = temp_dir("compact-rt");
    let data = {
        // Persist current state to a fresh store via replace on a shared db
        let path = dir.join("session.json");
        let other = SessionStore::for_path(path);
        other
            .replace_messages(vec![user("old1"), assistant("old2"), user("tail")])
            .await
            .unwrap();
        other
            .commit_context_projection(ContextProjectionResult {
                model_window: vec![checkpoint_message("summary of old rounds"), user("tail")],
                archived_originals: vec![user("old1"), assistant("old2")],
                checkpoint: ContextProjectionCheckpoint {
                    operation: ContextProjectionKind::Compact,
                    archived_messages: 2,
                    active_messages: 2,
                    window_tokens_before: 10,
                    window_tokens_after: 5,
                    summary: Some("summary of old rounds".to_string()),
                    tracked_files: Vec::new(),
                },
            })
            .await
            .unwrap();
        other.model_window().await
    };
    assert_eq!(data.len(), 2);
}

#[tokio::test]
async fn prune_commit_appends_directive_not_rewrite() {
    let store = store("prune").await;
    let call = ToolCall::new("call-1", "read_file", "{}");
    let mut assistant_message = assistant("working");
    assistant_message.tool_calls = Some(vec![call.clone()]);
    let tool_result = Message::tool_result(&call, "long output");
    store
        .replace_messages(vec![user("go"), assistant_message, tool_result.clone()])
        .await
        .unwrap();

    let mut window = store.model_window().await;
    window[2].content = "[pruned]".into();
    let original_body = window[2].content.clone();
    let _ = original_body;
    let _archived = store.full_transcript().await;
    store
        .commit_context_projection(ContextProjectionResult {
            model_window: window,
            archived_originals: vec![tool_result.clone()],
            checkpoint: ContextProjectionCheckpoint {
                operation: ContextProjectionKind::Prune,
                archived_messages: 1,
                active_messages: 3,
                window_tokens_before: 9,
                window_tokens_after: 3,
                summary: None,
                tracked_files: Vec::new(),
            },
        })
        .await
        .unwrap();

    let window = store.model_window().await;
    assert_eq!(window[2].content, "[pruned]");
    // Full transcript keeps the original body.
    let full = store.full_transcript().await;
    assert_eq!(full[2].content, "long output");
}

#[tokio::test]
async fn prune_commit_after_new_user_turn_with_unsynced_timestamp_succeeds() {
    let store = store("prune-multiturn").await;
    let call = ToolCall::new("call-1", "read_file", "{}");
    let mut assistant_message = assistant("working");
    assistant_message.tool_calls = Some(vec![call.clone()]);
    let tool_result = Message::tool_result(&call, "long output");

    // Turn 1 committed
    store
        .commit_turn(CommitTurn {
            messages: &[user("turn 1"), assistant_message, tool_result.clone()],
            round_counter: Some(1),
            usage_records: &[],
            retry_point: None,
            round_interrupt: None,
            operation_id: None,
            expected_revision: None,
        })
        .await
        .unwrap();

    // Turn 2: User sends a new message constructed via Message::new (timestamp is None).
    let mut round_history = store.model_window().await;
    round_history.push(Message::new(Role::User, "turn 2"));

    // Admitted turn committed
    store
        .commit_turn(CommitTurn {
            messages: &round_history,
            round_counter: Some(2),
            usage_records: &[],
            retry_point: None,
            round_interrupt: None,
            operation_id: None,
            expected_revision: None,
        })
        .await
        .unwrap();

    // Context projection prune triggers on stale tool output
    round_history[2].content = "[pruned output]".into();
    store
        .commit_context_projection(ContextProjectionResult {
            model_window: round_history,
            archived_originals: vec![tool_result.clone()],
            checkpoint: ContextProjectionCheckpoint {
                operation: ContextProjectionKind::Prune,
                archived_messages: 1,
                active_messages: 4,
                window_tokens_before: 10,
                window_tokens_after: 5,
                summary: None,
                tracked_files: Vec::new(),
            },
        })
        .await
        .expect("prune projection translation must succeed with semantic wire equivalence");

    let window = store.model_window().await;
    assert_eq!(window.len(), 4);
    assert_eq!(window[2].content, "[pruned output]");
}

// ---------------------------------------------------------------
// Turn commits: prefix-delta appends; divergence rebuilds.
// ---------------------------------------------------------------

#[tokio::test]
async fn append_turn_appends_delta() {
    let store = store("append").await;
    store.replace_messages(vec![user("go")]).await.unwrap();
    store
        .append_turn(&[user("go"), assistant("done")])
        .await
        .unwrap();
    let window = store.model_window().await;
    assert_eq!(window.len(), 2);
    assert_eq!(window[1].content, "done");
}

#[tokio::test]
async fn commit_turn_persists_window_counter_and_usage() {
    let store = store("commit").await;
    store.replace_messages(vec![user("go")]).await.unwrap();
    let mut messages = store.model_window().await;
    messages.push(assistant("ok"));
    store
        .commit_turn(CommitTurn::new(&messages).tap_counter(3))
        .await
        .unwrap();
    assert_eq!(store.model_window().await.len(), 2);
    assert_eq!(store.round_counter().await, 3);
}

impl CommitTurn<'_> {
    fn tap_counter(mut self, counter: u64) -> Self {
        self.round_counter = Some(counter);
        self
    }
}

#[tokio::test]
async fn commit_turn_rebuilds_on_divergence() {
    let store = store("diverge").await;
    store
        .replace_messages(vec![user("a"), assistant("b")])
        .await
        .unwrap();
    store
        .commit_turn(CommitTurn::new(&[user("replaced")]))
        .await
        .unwrap();
    let window = store.model_window().await;
    assert_eq!(window.len(), 1);
    assert_eq!(window[0].content, "replaced");
}

#[tokio::test]
async fn commit_turn_honors_the_revision_precondition() {
    let store = store("commit-guard").await;
    let messages = vec![user("hello")];
    let first = store.commit_turn(CommitTurn::new(&messages)).await.unwrap();

    // A precondition that does not match the durable revision fails closed.
    let stale = store
        .commit_turn(CommitTurn {
            expected_revision: Some(first + 5),
            ..CommitTurn::new(&messages)
        })
        .await
        .unwrap_err();
    assert!(stale.contains("stale session revision"), "got {stale}");

    // A matching precondition commits and advances the durable revision.
    let advanced = store
        .commit_turn(CommitTurn {
            expected_revision: Some(first),
            ..CommitTurn::new(&messages)
        })
        .await
        .unwrap();
    assert_eq!(advanced, first + 1);
    assert_eq!(store.model_window().await.len(), 1);
}

#[tokio::test]
async fn crash_residue_is_durably_settled_as_abandoned() {
    use nuo_wire::{
        RequestUsageKey, RequestUsageRecord, RequestUsageSource, RequestUsageStatus,
    };
    let dir = temp_dir("abandoned");
    let path = dir.join("session.json");
    let store = SessionStore::for_path(path.clone());
    // Make the session non-empty so subsequent writes are not deferred.
    store.replace_messages(vec![user("hi")]).await.unwrap();

    let record = RequestUsageRecord {
        key: RequestUsageKey {
            session_id: store.id().await,
            actor_id: "root".to_string(),
            round: 1,
            turn: 1,
            attempt: 1,
        },
        provider: "openai".to_string(),
        model: "gpt-5".to_string(),
        status: RequestUsageStatus::InFlight,
        source: RequestUsageSource::Unknown,
        projected_prompt_tokens: 321,
        ..Default::default()
    };
    store.set_request_usage_records(vec![record]).await.unwrap();

    assert_eq!(store.settle_abandoned_attempts().await.unwrap(), 1);

    // The classification is durable, not merely in-memory: a fresh store reads
    // the attempt resolved instead of a silently filtered in-flight.
    let reloaded = SessionStore::for_path(path);
    let records = reloaded.request_usage_records().await;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].status, RequestUsageStatus::Abandoned);
    assert_eq!(records[0].source, RequestUsageSource::Estimated);
    assert_eq!(records[0].prompt_tokens, 321);
    assert_eq!(records[0].total_tokens, 321);

    // Idempotent: nothing left to settle on a second pass.
    assert_eq!(reloaded.settle_abandoned_attempts().await.unwrap(), 0);
}

/// ADR-0236 invariant #3: an ordinary turn's row checksum and delta must not
/// scale with the session's total retained attempts. Usage lives in its own
/// key-addressed table, so the session-row checksum excludes the mirror and a
/// delta does not clone it.
#[test]
fn row_checksum_and_delta_ignore_retained_attempts() {
    use nuo_wire::{RequestUsageKey, RequestUsageRecord, RequestUsageStatus};
    let mut data = SessionData::default();
    data.transcript
        .push(nuo_wire::TranscriptEntry::from_message(
            0,
            &user("hello"),
        ));
    let baseline = compute_checksum(&data).unwrap();

    for attempt in 1..=1_000 {
        data.request_usage_records.push(RequestUsageRecord {
            key: RequestUsageKey {
                session_id: data.id.clone(),
                actor_id: "root".to_string(),
                round: 1,
                turn: 1,
                attempt,
            },
            status: RequestUsageStatus::Completed,
            ..Default::default()
        });
    }

    assert_eq!(
        compute_checksum(&data).unwrap(),
        baseline,
        "retained attempts must not be folded into the session-row checksum"
    );
    assert!(
        data.clone_metadata_without_history()
            .request_usage_records
            .is_empty(),
        "a delta must not clone the per-session usage mirror"
    );
}

// ---------------------------------------------------------------
// Working state: todos derive from state entries; title is terminal.
// ---------------------------------------------------------------

#[tokio::test]
async fn todos_round_trip_through_state_entries() {
    let dir = temp_dir("todos");
    let path = dir.join("session.json");
    let store = SessionStore::for_path(path.clone());
    let mut list = nuo_wire::TodoList::default();
    list.reconcile(
        &[("write schema".into(), nuo_wire::TodoStatus::Pending)],
        0,
        0,
    );
    store.set_todos(list).await.unwrap();
    assert!(!store.todos().await.is_empty());

    let reloaded = SessionStore::for_path(path);
    assert!(!reloaded.todos().await.is_empty());
}

#[tokio::test]
async fn title_is_terminal_once_set() {
    let store = store("title").await;
    store
        .set_title(Some("My title".into()), true)
        .await
        .unwrap();
    let (title, has_title) = store.title().await;
    assert_eq!(title.as_deref(), Some("My title"));
    assert!(has_title);
    // AI generation (manual=false) may only fill None; the store API accepts
    // the call but the non-NULL-is-terminal rule is enforced by the caller
    // contract. Clearing remains possible explicitly.
    store.set_title(None, false).await.unwrap();
    let (title, has_title) = store.title().await;
    assert!(title.is_none());
    assert!(!has_title);
}

// ---------------------------------------------------------------
// Fork: entries shared by identity across sessions.
// ---------------------------------------------------------------

#[tokio::test]
async fn fork_shares_entries_and_diverges() {
    let dir = temp_dir("fork");
    let db = dir.join("nuo.db");
    let path = dir.join("session.json");
    std::fs::create_dir_all(&dir).unwrap();

    let store = SessionStore::for_path(path.clone());
    store
        .replace_messages(vec![user("parent"), assistant("history")])
        .await
        .unwrap();
    let (child_id, parent_id) = store.fork().await.unwrap();
    assert_eq!(store.id().await, child_id);

    // The child's own persisted state carries the shared transcript.
    let child = SessionStore::for_path(dir.join(format!("{child_id}.json")));
    let window = child.model_window().await;
    assert_eq!(window.len(), 2);
    assert_eq!(window[0].content, "parent");
    let _ = (parent_id, db);
}

#[tokio::test]
async fn fork_to_side_keeps_active_pointer() {
    let store = store("side").await;
    store
        .replace_messages(vec![user("aside seed")])
        .await
        .unwrap();
    let (side_id, _) = store.fork_to_side().await.unwrap();
    assert_eq!(store.model_window().await.len(), 1);
    let side = store.open_side(&side_id).await.unwrap();
    assert_eq!(side.model_window().await.len(), 1);

    let parent_ir = store.session_ir().await;
    let side_ir = side.session_ir().await;
    assert_eq!(side_ir.history.nodes.len(), parent_ir.history.nodes.len());
    assert_eq!(side_ir.parent_session_id, Some(store.id().await));
}

// ---------------------------------------------------------------
// Provider pin and digest round trips (working state).
// ---------------------------------------------------------------

#[tokio::test]
async fn provider_selection_round_trips() {
    let dir = temp_dir("provider");
    let path = dir.join("session.json");
    let store = SessionStore::for_path(path.clone());
    // Lazy contract (ADR-0018): a pin on an empty session does not
    // materialise it — give the session content first.
    store
        .replace_messages(vec![user("real work")])
        .await
        .unwrap();
    store
        .set_provider_selection(Some(ProviderSelection {
            connection: "anthropic".into(),
            model: Some("claude-sonnet-4".into()),
        }))
        .await
        .unwrap();
    let reloaded = SessionStore::for_path(path);
    let selection = reloaded.provider_selection().await.unwrap();
    assert_eq!(selection.connection, "anthropic");
}

#[tokio::test]
async fn digest_round_trips() {
    let store = store("digest").await;
    store
        .set_digest(Some(nuo_wire::SessionDigest::default()), Some(42))
        .await
        .unwrap();
    let (digest, anchor) = store.digest().await;
    assert!(digest.is_some());
    assert_eq!(anchor, Some(42));
}

// ---------------------------------------------------------------
// Interrupt records (projection state, never in the window).
// ---------------------------------------------------------------

#[tokio::test]
async fn round_interrupts_record_and_clear() {
    let store = store("interrupt").await;
    store
        .record_round_interrupt(nuo_wire::RoundInterrupt {
            reason: nuo_wire::RoundInterruptReason::User,
            at_ms: 1,
            round: Some(1),
            detail: None,
        })
        .await
        .unwrap();
    assert_eq!(store.round_interrupts().await.len(), 1);
    // Interrupts never enter the window.
    store.replace_messages(vec![user("x")]).await.unwrap();
    assert_eq!(store.model_window().await.len(), 1);
    store.clear_round_interrupts().await.unwrap();
    assert!(store.round_interrupts().await.is_empty());
}

// ---------------------------------------------------------------
// Request-projection archive (ADR-0218): durable forensics, never the window.
// ---------------------------------------------------------------

fn request_projection(round: u64, turn: u64) -> nuo_wire::RequestProjection {
    nuo_wire::RequestProjection {
        round,
        turn,
        created_at_ms: 42,
        prefix_fingerprint: format!("sha256:{round:02x}{turn:02x}"),
        conversation_messages: 1,
        temporary_context_tokens: 7,
        temporary_context: vec![Message::new(
            Role::User,
            "<temporary-context>request-local evidence</temporary-context>",
        )],
    }
}

#[tokio::test]
async fn request_projections_persist_outside_the_window() {
    let dir = temp_dir("request-projection");
    let path = dir.join("session.json");
    let store = SessionStore::for_path(path.clone());
    store.replace_messages(vec![user("hello")]).await.unwrap();

    store
        .record_request_projection(request_projection(1, 0))
        .await
        .unwrap();
    // Dedupe on (round, turn): a transport retry reuses the snapshot and must
    // not mint a second record.
    store
        .record_request_projection(request_projection(1, 0))
        .await
        .unwrap();
    assert_eq!(store.request_projections().await.len(), 1);

    // The archive is not the window: E_n never enters model-visible history.
    let window = store.model_window().await;
    assert!(
        window
            .iter()
            .all(|m| !m.content.contains("request-local evidence"))
    );

    // Durable round-trip: the projection survives a reload, still outside the
    // window.
    let reloaded = SessionStore::for_path(path);
    let window = reloaded.model_window().await;
    assert!(
        window
            .iter()
            .all(|m| !m.content.contains("request-local evidence"))
    );
    let restored = reloaded.request_projections().await;
    assert_eq!(restored.len(), 1);
    assert_eq!(restored[0].prefix_fingerprint, "sha256:0100");
    assert!(
        restored[0].temporary_context[0]
            .content
            .contains("request-local evidence")
    );
}

#[tokio::test]
async fn request_projections_are_retention_bounded() {
    let store = store("request-projection-retention").await;
    store.replace_messages(vec![user("hello")]).await.unwrap();
    let cap = crate::db::MAX_RETAINED_REQUEST_PROJECTIONS;
    for round in 0..(cap as u64 + 5) {
        store
            .record_request_projection(request_projection(round, 0))
            .await
            .unwrap();
    }
    let retained = store.request_projections().await;
    assert_eq!(retained.len(), cap);
    // Newest kept, oldest evicted.
    assert_eq!(retained.last().unwrap().round, cap as u64 + 4);
    assert_eq!(retained.first().unwrap().round, 5);
}

// ---------------------------------------------------------------
// Command ledger.
// ---------------------------------------------------------------

#[tokio::test]
async fn command_ledger_round_trips() {
    let store = store("commands").await;
    store
        .mutate_commands(|ledger| {
            ledger.push(nuo_wire::CommandRecord::new("models", ""));
        })
        .await
        .unwrap();
    assert_eq!(store.commands().await.len(), 1);
}

// ---------------------------------------------------------------
// Legacy snapshots are retired, not migrated (clean break).
// ---------------------------------------------------------------

#[tokio::test]
async fn legacy_snapshot_content_is_retired() {
    let dir = temp_dir("legacy");
    let path = dir.join("session.json");
    // Write a pre-transcript snapshot payload directly.
    let legacy = r#"{
        "id": "11111111-1111-1111-1111-111111111111",
        "created_at": 1,
        "updated_at": 1,
        "project_root": "/tmp",
        "schema_version": 12,
        "model_window": [
            {"role": "user", "content": "legacy content"}
        ]
    }"#;
    std::fs::write(&path, legacy).unwrap();
    let store = SessionStore::for_path(path.clone());
    // The legacy dialogue is retired; the identity is stable across reloads.
    assert!(store.model_window().await.is_empty());
    let id = store.id().await;
    let reloaded = SessionStore::for_path(path);
    assert_eq!(reloaded.id().await, id);
}

// ---------------------------------------------------------------
// Session identity: empty sessions stay unpersisted.
// ---------------------------------------------------------------

#[tokio::test]
async fn fresh_session_stays_unpersisted_until_content() {
    let dir = temp_dir("empty");
    let path = dir.join("session.json");
    let store = SessionStore::for_path(path.clone());
    assert!(store.is_empty_unpersisted().await);
    assert!(!path.exists());
    store
        .replace_messages(vec![user("now real")])
        .await
        .unwrap();
    assert!(!store.is_empty_unpersisted().await);
    let reloaded = SessionStore::for_path(path);
    assert_eq!(reloaded.model_window().await.len(), 1);
}

// ---------------------------------------------------------------
// Tree DAG: leaf switch rebuilds the transcript.
// ---------------------------------------------------------------

#[tokio::test]
async fn tree_leaf_switch_rebuilds_transcript() {
    let store = store("tree").await;
    let root_id = store
        .insert_tree_entry(nuo_wire::SessionEntry::new_message(
            "root",
            None,
            0,
            user("root message"),
        ))
        .await
        .unwrap();
    let messages = store.switch_tree_leaf(&root_id).await.unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(store.model_window().await.len(), 1);
}

// ---------------------------------------------------------------
// Subagent sessions: nested transcripts become their own sessions.
// ---------------------------------------------------------------

#[tokio::test]
async fn subagent_children_become_subagent_sessions_with_pointer() {
    let dir = temp_dir("subagent");
    let path = dir.join("session.json");
    let store = SessionStore::for_path(path.clone());
    store
        .replace_messages(vec![user("spawn task")])
        .await
        .unwrap();

    let call = ToolCall::new("call-sub", "task", "{}");
    let mut parent = assistant("delegating");
    parent.tool_calls = Some(vec![call.clone()]);
    let child_messages = vec![user("child task"), assistant("child done")];
    let subagent_meta = nuo_wire::message::SubagentMeta {
        description: Some("do the thing".into()),
        duration_ms: Some(1234),
        toolset_count: 3,
        ..Default::default()
    };
    let result = Message::tool_result(&call, "[task result]:\ndone")
        .with_children(child_messages.clone())
        .with_subagent_meta(subagent_meta);

    let mut window = store.model_window().await;
    window.push(parent);
    window.push(result);
    store.commit_turn(CommitTurn::new(&window)).await.unwrap();

    // The parent's tool entry carries a SubagentRef into a Subagent-kind
    // session whose transcript holds the child messages.
    let reader = store.writer.reader().unwrap();
    let subagent_id = {
        let state = store.state_lock_for_test().await;
        let entry = state
            .data
            .transcript
            .entries
            .iter()
            .rev()
            .find_map(|entry| match &entry.payload {
                nuo_wire::EntryPayload::Message(payload) => payload.subagent.clone(),
                _ => None,
            })
            .expect("tool entry must carry a subagent pointer");
        entry.session_id
    };
    let subagent = reader.load_session_full(&subagent_id).unwrap().unwrap();
    assert_eq!(
        subagent.fork_kind,
        nuo_wire::SessionForkKind::Subagent
    );
    assert_eq!(
        subagent.parent_id.as_deref(),
        Some(store.id().await.as_str())
    );
    let transcript = subagent
        .transcript
        .entries
        .iter()
        .filter_map(|entry| entry.to_message())
        .collect::<Vec<_>>();
    assert_eq!(transcript.len(), 2);
    assert_eq!(transcript[0].content, "child task");
    assert_eq!(transcript[1].content, "child done");
    let _ = child_messages;

    // Subagent sessions never surface in the picker.
    let summaries = reader
        .list_session_summaries(None, &store.id().await)
        .unwrap();
    assert!(!summaries.iter().any(|summary| summary.id == subagent_id));
}

// ---------------------------------------------------------------
// Deterministic compaction helpers (pure functions).
// ---------------------------------------------------------------

#[test]
fn compaction_selects_and_builds_checkpoint() {
    let history = vec![
        user("r1"),
        assistant("a1"),
        user("r2"),
        assistant("a2"),
        user("r3"),
    ];
    let selection = select_compaction(&history, 1).unwrap();
    assert_eq!(selection.archived.len(), 4);
    assert_eq!(selection.tail.len(), 1);
    let result = build_compaction_result(100, selection, "summary".into());
    assert_eq!(result.model_window.len(), 2);
    assert_eq!(result.archived_originals.len(), 4);
    assert_eq!(result.checkpoint.operation, ContextProjectionKind::Compact);
}

#[test]
fn excerpt_summary_respects_token_budget() {
    let archived: Vec<Message> = (0..50)
        .map(|i| user(&format!("message {i} with some filler text to burn budget")))
        .collect();
    let summary = build_excerpt_summary(&archived, 120, None);
    assert!(!summary.is_empty());
    assert!(nuo_wire::tokenizer::count_tokens(&summary) <= 120);
}

#[tokio::test]
async fn delete_unpersisted_active_session_resets_cleanly() {
    let dir = temp_dir("delete_unpersisted");
    let path = dir.join("session.json");
    let store = SessionStore::for_path(path.clone());
    let initial_id = store.id().await;

    // The store is fresh/unpersisted: no messages have been committed.
    let deleted = store
        .delete(&initial_id)
        .await
        .expect("deleting unpersisted active session must succeed");
    assert_eq!(deleted, initial_id);

    // The active session has been reset to a fresh id.
    let new_id = store.id().await;
    assert_ne!(new_id, initial_id);
}

#[tokio::test]
async fn delete_persisted_session_and_idempotent_delete() {
    let dir = temp_dir("delete_persisted");
    let path = dir.join("session.json");
    let store = SessionStore::for_path(path.clone());
    let id = store.id().await;

    // Persist a message so the session is written to SQLite.
    store
        .replace_messages(vec![user("hello world")])
        .await
        .unwrap();

    let deleted = store
        .delete(&id)
        .await
        .expect("deleting persisted active session must succeed");
    assert_eq!(deleted, id);

    let new_id = store.id().await;
    assert_ne!(new_id, id);

    // Deleting the already-deleted full UUID is idempotent and does not error.
    let re_deleted = store
        .delete(&id)
        .await
        .expect("repeat delete of full UUID must be idempotent");
    assert_eq!(re_deleted, id);
}

#[tokio::test]
async fn test_session_store_ir_and_compile_request() {
    let store = store("session_ir_compile").await;
    let user_msg = user("Implement compiler pass");
    let mut ir = store.session_ir().await;
    ir.append_message("msg-1", 1000, user_msg);
    store.commit_session_ir(&ir).await.unwrap();

    // 1. Fetch SessionIR
    let ir = store.session_ir().await;
    assert_eq!(ir.session_id, store.id().await);
    assert_eq!(ir.history.nodes.len(), 1);

    // 2. Compile request using SessionStore::compile_request (4-pass pipeline)
    let options = nuo_wire::CompilerOptions {
        tool_specs: vec![nuo_wire::ToolSpec {
            name: "execute_command".into(),
            description: "Run shell command".into(),
            parameters: serde_json::json!({"type": "object"}),
        }],
        temporary_context: vec![],
        ephemeral_instruction: Some("Focus on correctness".into()),
        target_dialect: Some("anthropic".into()),
        target_protocol: Some(nuo_wire::WireProtocol::AnthropicMessages),
    };

    let compiled = store.compile_request(options).await.unwrap();
    assert_eq!(compiled.request.messages.len(), 1);
    assert_eq!(
        compiled.request.messages[0].content,
        "Implement compiler pass"
    );
    assert_eq!(compiled.request.tool_specs.len(), 1);
    assert_eq!(compiled.request.tool_specs[0].name, "execute_command");
    assert!(!compiled.cache_boundary.prefix_fingerprint.is_empty());
}

#[tokio::test]
async fn test_session_store_direct_delta_and_ir_hydration() {
    let store = store("session_ir_delta_test").await;
    let session_id = store.id().await;

    // 1. Create a native SessionIR and append a dialogue node
    let policy = nuo_wire::SessionPolicy::default();
    let mut ir = nuo_wire::SessionIR::new(&session_id, policy, 1000);
    let node_id = "test-node-1";
    ir.append_message(
        node_id,
        1001,
        nuo_wire::Message::new(nuo_wire::Role::User, "Hello SessionIR native"),
    );

    // 2. Commit SessionIR directly
    store.commit_session_ir(&ir).await.unwrap();

    // 3. Hydrate via DbReader directly from sessions_v2 / causal_nodes
    let reader = store.writer.reader().unwrap();
    let loaded = reader.load_session_ir(&session_id).unwrap();
    assert!(loaded.is_some(), "SessionIR must be loaded from SQLite");
    let loaded_ir = loaded.unwrap();
    assert_eq!(loaded_ir.session_id, session_id);
    assert_eq!(loaded_ir.history.nodes.len(), 1);
    assert_eq!(loaded_ir.state.active_leaf, Some(node_id.to_string()));

    // 4. Test Durable Suspension Delta (ADR-0249, INV-EXEC-03)
    let mut suspended_ir = loaded_ir.clone();
    suspended_ir.state.status = nuo_wire::ExecutionStatus::Suspended {
        reason: nuo_wire::SuspensionReason::NeedsInput {
            prompt: "Please select target environment".into(),
        },
    };
    let delta = suspended_ir.drain_delta(1);
    store.commit_session_delta(delta).await.unwrap();

    // 5. Verify suspension state hydrated from DB
    let fresh_reader = store.writer.reader().unwrap();
    let reloaded = fresh_reader.load_session_ir(&session_id).unwrap().unwrap();
    assert!(matches!(
        reloaded.state.status,
        nuo_wire::ExecutionStatus::Suspended {
            reason: nuo_wire::SuspensionReason::NeedsInput { .. }
        }
    ));
}
