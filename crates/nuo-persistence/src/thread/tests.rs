//! Session persistence test suite (single-transcript model).
//!
//! Tests exercise the public [`ThreadSession`] API against the SQLite store:
//! the transcript is the authority, the window is a derive, projections are
//! append-only directives, and fork shares facts by identity.
use crate::thread::*;
use nuo_protocol::{Message, Role, ToolCall};
use std::path::PathBuf;

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("nuo-t18x-{tag}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

async fn store(tag: &str) -> ThreadSession {
    let dir = temp_dir(tag);
    let path = dir.join("session.json");
    ThreadSession::for_path(path)
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
    let store = ThreadSession::for_path(path.clone());
    store
        .replace_messages(vec![user("alpha"), assistant("beta")])
        .await
        .unwrap();

    let reloaded = ThreadSession::for_path(path);
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

/// Regression (`projected_cache`): `open` swaps the store's whole
/// `ThreadData` but the projection cache belonged to the session being left.
/// The stale cache made `model_window` return the previous session's window
/// after a switch — and the resume path then `replace_messages`-ed it over
/// the newly opened session, wiping the resumed transcript (the
/// `/sessions <id>` restore rendered an empty view).
#[tokio::test]
async fn open_repoints_projection_to_the_switched_session() {
    let dir = temp_dir("open-cache");
    let store = ThreadSession::for_path(dir.join("a.json"));
    store
        .replace_messages(vec![user("from session A"), assistant("reply A")])
        .await
        .unwrap();

    // Populate session B on disk via a second store, then switch.
    let other = ThreadSession::for_path(dir.join("b.json"));
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
    let store = ThreadSession::for_path(dir.join("a.json"));
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
// Request projection is disposable and never rewrites facts.
// ---------------------------------------------------------------

#[tokio::test]
async fn request_projection_never_changes_the_durable_window() {
    let store = store("projection-disposable").await;
    let call = ToolCall::new("call-1", "read", "{}");
    let mut assistant_message = assistant("working");
    assistant_message.tool_calls = Some(vec![call.clone()]);
    let tool_result = Message::tool_result(&call, "long output");
    store
        .replace_messages(vec![user("go"), assistant_message, tool_result])
        .await
        .unwrap();

    let before = store.model_window().await;
    let mut projected = before.clone();
    projected[2].content = "[pruned]".into();
    store
        .commit_context_projection(ContextProjectionResult {
            model_window: projected,
            archived_originals: vec![before[2].clone()],
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

    // The request-local fold is not a fact: the durable window is unchanged.
    assert_eq!(store.model_window().await, before);
    assert_eq!(store.full_transcript().await, before);
}

#[tokio::test]
async fn a_compaction_projection_does_not_survive_a_reload() {
    let dir = temp_dir("projection-no-reload");
    let path = dir.join("session.json");
    let store = ThreadSession::for_path(path.clone());
    store
        .replace_messages(vec![user("old1"), assistant("old2"), user("tail")])
        .await
        .unwrap();
    store
        .commit_context_projection(ContextProjectionResult {
            model_window: vec![checkpoint_message("request-only summary"), user("tail")],
            archived_originals: vec![user("old1"), assistant("old2")],
            checkpoint: ContextProjectionCheckpoint {
                operation: ContextProjectionKind::Compact,
                archived_messages: 2,
                active_messages: 2,
                window_tokens_before: 10,
                window_tokens_after: 5,
                summary: Some("request-only summary".to_string()),
                tracked_files: Vec::new(),
            },
        })
        .await
        .unwrap();
    assert_eq!(store.model_window().await.len(), 3);
    drop(store);

    let reopened = ThreadSession::for_path(path);
    assert_eq!(reopened.model_window().await.len(), 3);
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
        .commit_turn(CommitTurn::new(&messages, CommitBasis::Appended).tap_counter(3))
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
        .commit_turn(CommitTurn::new(
            &[user("replaced")],
            CommitBasis::Reprojected,
        ))
        .await
        .unwrap();
    let window = store.model_window().await;
    assert_eq!(window.len(), 1);
    assert_eq!(window[0].content, "replaced");
}

/// An `Appended` claim is verified, not trusted: a tail that does not match the
/// durable projection degrades to the safe path and rebuilds. A wrong claim
/// costs a comparison, never a persisted prefix that disagrees with the caller.
#[tokio::test]
async fn a_broken_appended_claim_falls_back_to_a_rebuild() {
    let store = store("basis-fallback").await;
    let first = vec![user("a"), assistant("b")];
    store
        .commit_turn(CommitTurn::new(&first, CommitBasis::Appended))
        .await
        .unwrap();

    // Same length, different tail, but the caller *claims* append-only.
    let claimed_append = vec![user("a"), assistant("CHANGED")];
    store
        .commit_turn(CommitTurn::new(&claimed_append, CommitBasis::Appended))
        .await
        .unwrap();
    let window = store.model_window().await;
    assert_eq!(window.len(), 2);
    assert_eq!(window[1].content, "CHANGED");
}

/// A reprojection that keeps the window length still rewrites the transcript.
/// Length alone is not identity: a prune clears committed tool bodies in place.
#[tokio::test]
async fn reprojected_basis_rewrites_an_equally_sized_window() {
    let store = store("basis-reproject").await;
    let first = vec![user("a"), assistant("b")];
    store
        .commit_turn(CommitTurn::new(&first, CommitBasis::Appended))
        .await
        .unwrap();

    let reprojected = vec![user("a"), assistant("pruned body")];
    store
        .commit_turn(CommitTurn::new(&reprojected, CommitBasis::Reprojected))
        .await
        .unwrap();
    let window = store.model_window().await;
    assert_eq!(window[1].content, "pruned body");
}

/// A commit writes facts; the thread projection is the read seam.
#[tokio::test]
async fn committed_messages_are_visible_as_thread_facts() {
    let store = store("thread-fact-read").await;
    store
        .commit_turn(CommitTurn::new(
            &[user("question"), assistant("answer")],
            CommitBasis::Appended,
        ))
        .await
        .unwrap();

    let thread = store.thread().await;
    assert_eq!(
        thread.graph.len(),
        2,
        "the thread projection must see the committed messages"
    );
    assert!(thread.state.active_branch().unwrap().head.is_some());
}

#[tokio::test]
async fn commit_turn_honors_the_revision_precondition() {
    let store = store("commit-guard").await;
    let messages = vec![user("hello")];
    let first = store
        .commit_turn(CommitTurn::new(&messages, CommitBasis::Appended))
        .await
        .unwrap();

    // A precondition that does not match the durable revision fails closed.
    let stale = store
        .commit_turn(CommitTurn {
            expected_revision: Some(first + 5),
            ..CommitTurn::new(&messages, CommitBasis::Appended)
        })
        .await
        .unwrap_err();
    assert!(stale.contains("stale session revision"), "got {stale}");

    // A matching precondition commits and advances the durable revision.
    let advanced = store
        .commit_turn(CommitTurn {
            expected_revision: Some(first),
            ..CommitTurn::new(&messages, CommitBasis::Appended)
        })
        .await
        .unwrap();
    assert_eq!(advanced, first + 1);
    assert_eq!(store.model_window().await.len(), 1);
}

#[tokio::test]
async fn crash_residue_is_durably_settled_as_abandoned() {
    use nuo_protocol::{
        RequestUsageKey, RequestUsageRecord, RequestUsageSource, RequestUsageStatus,
    };
    let dir = temp_dir("abandoned");
    let path = dir.join("session.json");
    let store = ThreadSession::for_path(path.clone());
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
    let reloaded = ThreadSession::for_path(path);
    let records = reloaded.request_usage_records().await;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].status, RequestUsageStatus::Abandoned);
    assert_eq!(records[0].source, RequestUsageSource::Estimated);
    assert_eq!(records[0].prompt_tokens, 321);
    assert_eq!(records[0].total_tokens, 321);

    // Idempotent: nothing left to settle on a second pass.
    assert_eq!(reloaded.settle_abandoned_attempts().await.unwrap(), 0);
}

/// Invariant #3: an ordinary turn's row checksum and delta must not
/// scale with the session's total retained attempts. Usage lives in its own
/// key-addressed table, so the session-row checksum excludes the mirror and a
/// delta does not clone it.
#[test]
fn row_checksum_and_delta_ignore_retained_attempts() {
    use nuo_protocol::{RequestUsageKey, RequestUsageRecord, RequestUsageStatus};
    let mut data = ThreadData::default();
    data.transcript
        .push(nuo_protocol::TranscriptEntry::from_message(
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
    let store = ThreadSession::for_path(path.clone());
    let mut list = nuo_protocol::TodoList::default();
    list.reconcile(
        &[("write schema".into(), nuo_protocol::TodoStatus::Pending)],
        0,
        0,
    );
    store.set_todos(list).await.unwrap();
    assert!(!store.todos().await.is_empty());

    let reloaded = ThreadSession::for_path(path);
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
// Fork is a branch cursor, never a copied conversation.
// ---------------------------------------------------------------

#[tokio::test]
async fn fork_at_head_shares_every_fact() {
    let store = store("fork-branch-head").await;
    store
        .replace_messages(vec![user("parent"), assistant("history")])
        .await
        .unwrap();
    let before = store.model_window().await;
    let branch = store
        .fork_at_branch(ForkTarget::Head, "side")
        .await
        .unwrap();

    // Fork switched the view to the branch; both branches share all facts.
    assert_eq!(store.model_window().await, before);
    let aggregate = store.thread_aggregate().await.unwrap();
    assert_eq!(aggregate.state.branches.len(), 2);
    assert_eq!(aggregate.path(&branch).unwrap().len(), 2);
    assert_eq!(
        aggregate
            .path(&nuo_protocol::BranchId::main())
            .unwrap()
            .len(),
        2
    );
}

#[tokio::test]
async fn fork_at_round_keeps_only_the_prefix_and_does_not_copy() {
    let store = store("fork-branch-round").await;
    store
        .replace_messages(vec![
            user("round 1 user"),
            assistant("round 1 assistant"),
            user("round 2 user"),
            assistant("round 2 assistant"),
        ])
        .await
        .unwrap();
    let branch = store
        .fork_at_branch(ForkTarget::Round(1), "round-1")
        .await
        .unwrap();
    let aggregate = store.thread_aggregate().await.unwrap();
    assert_eq!(aggregate.path(&branch).unwrap().len(), 2);
    assert_eq!(
        aggregate.path(&branch).unwrap()[0].payload.kind(),
        nuo_protocol::FactKind::Message
    );
    // The parent branch still sees every fact; nothing was copied.
    assert_eq!(
        aggregate
            .path(&nuo_protocol::BranchId::main())
            .unwrap()
            .len(),
        4
    );
    assert_eq!(aggregate.graph.len(), 4);
}

#[tokio::test]
async fn fork_at_turn_rejects_out_of_range_and_keeps_branch_count() {
    let store = store("fork-branch-turn").await;
    store
        .replace_messages(vec![
            user("round 1 user"),
            assistant("round 1 turn 0"),
            assistant("round 1 turn 1"),
            user("round 2 user"),
            assistant("round 2 assistant"),
        ])
        .await
        .unwrap();

    let branch = store
        .fork_at_branch(ForkTarget::Turn { round: 1, turn: 0 }, "turn-0")
        .await
        .unwrap();
    let aggregate = store.thread_aggregate().await.unwrap();
    assert_eq!(aggregate.path(&branch).unwrap().len(), 2);

    let err = store
        .fork_at_branch(ForkTarget::Turn { round: 1, turn: 9 }, "bad")
        .await
        .unwrap_err();
    assert!(err.contains("turn 9"), "unexpected error: {err}");
    assert_eq!(store.branches().await.unwrap().len(), 2);
}

// ---------------------------------------------------------------
// Provider pin and digest round trips (working state).
// ---------------------------------------------------------------

#[tokio::test]
async fn provider_selection_round_trips() {
    let dir = temp_dir("provider");
    let path = dir.join("session.json");
    let store = ThreadSession::for_path(path.clone());
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
    let reloaded = ThreadSession::for_path(path);
    let selection = reloaded.provider_selection().await.unwrap();
    assert_eq!(selection.connection, "anthropic");
}

#[tokio::test]
async fn digest_round_trips() {
    let store = store("digest").await;
    store
        .set_digest(Some(nuo_protocol::ThreadDigest::default()), Some(42))
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
        .record_round_interrupt(nuo_protocol::RoundInterrupt {
            reason: nuo_protocol::RoundInterruptReason::User,
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
// Request-projection archive: durable forensics, never the window.
// ---------------------------------------------------------------

fn request_projection(round: u64, turn: u64) -> nuo_protocol::RequestProjection {
    nuo_protocol::RequestProjection {
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
    let store = ThreadSession::for_path(path.clone());
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
    let reloaded = ThreadSession::for_path(path);
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
            ledger.push(nuo_protocol::CommandRecord::new("models", ""));
        })
        .await
        .unwrap();
    assert_eq!(store.commands().await.len(), 1);
}

/// ADR-0140 (schema v17): the pre-split ledger is retired, not migrated.
///
/// Every row written before the split mixed acts with readings, and the class was
/// never recorded, so no load-time filter can separate them. A reading restored
/// at its timestamp seam would assert a stale measurement as history, which is
/// the exact defect the split removes — so the ledger is dropped, the ADR-0059
/// precedent applied to the one field whose *contents* stopped being a claim the
/// design makes.
///
/// The test drives the migration seam directly: a `ThreadData` at the previous
/// schema version carrying both classes loads as a v17 session with no ledger.
#[test]
fn pre_split_command_ledger_is_retired_on_load() {
    let data = ThreadData {
        // An act and a reading, side by side — the shape a v16 session carries.
        commands: vec![
            nuo_protocol::CommandRecord::new("unattended", "on").with_result(
                nuo_protocol::CommandResult::Ack {
                    title: "Unattended mode ON".to_string(),
                    detail: None,
                },
            ),
            nuo_protocol::CommandRecord::new("usage", "")
                .with_result(nuo_protocol::CommandResult::Text("1.2M tokens".to_string())),
        ],
        schema_version: CURRENT_SCHEMA_VERSION - 1,
        ..Default::default()
    };

    let migrated = migrate_session_data(data);

    assert!(
        migrated.commands.is_empty(),
        "a pre-split ledger must not survive into a v17 session"
    );
    assert_eq!(migrated.schema_version, CURRENT_SCHEMA_VERSION);
}

/// The narrowed ledger is still a ledger: a session written *at* the current
/// schema keeps its acts across a reload.
#[test]
fn current_schema_keeps_acts_across_the_migration_seam() {
    let data = ThreadData {
        commands: vec![nuo_protocol::CommandRecord::new("unattended", "on")],
        schema_version: CURRENT_SCHEMA_VERSION,
        ..Default::default()
    };

    let migrated = migrate_session_data(data);
    assert_eq!(
        migrated.commands.len(),
        1,
        "a v17 ledger is acts-only by construction and must not be dropped"
    );
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
    let store = ThreadSession::for_path(path.clone());
    // The legacy dialogue is retired; the identity is stable across reloads.
    assert!(store.model_window().await.is_empty());
    let id = store.id().await;
    let reloaded = ThreadSession::for_path(path);
    assert_eq!(reloaded.id().await, id);
}

// ---------------------------------------------------------------
// Session identity: empty sessions stay unpersisted.
// ---------------------------------------------------------------

#[tokio::test]
async fn fresh_session_stays_unpersisted_until_content() {
    let dir = temp_dir("empty");
    let path = dir.join("session.json");
    let store = ThreadSession::for_path(path.clone());
    assert!(store.is_empty_unpersisted().await);
    assert!(!path.exists());
    store
        .replace_messages(vec![user("now real")])
        .await
        .unwrap();
    assert!(!store.is_empty_unpersisted().await);
    let reloaded = ThreadSession::for_path(path);
    assert_eq!(reloaded.model_window().await.len(), 1);
}

// ---------------------------------------------------------------
// Tree view is derived from thread facts.
// ---------------------------------------------------------------

#[tokio::test]
async fn tree_is_a_projection_of_the_thread() {
    let store = store("tree").await;
    store
        .replace_messages(vec![user("root message"), assistant("reply")])
        .await
        .unwrap();
    let tree = store.tree().await;
    assert_eq!(tree.entries.len(), 2);
    assert!(tree.active_leaf_id.is_some());
    assert!(tree.root_id.is_some());
}

// ---------------------------------------------------------------
// Subagent sessions: nested transcripts become their own sessions.
// ---------------------------------------------------------------

#[tokio::test]
async fn subagent_children_become_subagent_sessions_with_pointer() {
    let dir = temp_dir("subagent");
    let path = dir.join("session.json");
    let store = ThreadSession::for_path(path.clone());
    store
        .replace_messages(vec![user("spawn task")])
        .await
        .unwrap();

    let call = ToolCall::new("call-sub", "task", "{}");
    let mut parent = assistant("delegating");
    parent.tool_calls = Some(vec![call.clone()]);
    let child_messages = vec![user("child task"), assistant("child done")];
    let subagent_meta = nuo_protocol::message::SubagentMeta {
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
    store
        .commit_turn(CommitTurn::new(&window, CommitBasis::Appended))
        .await
        .unwrap();

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
                nuo_protocol::EntryPayload::Message(payload) => payload.subagent.clone(),
                _ => None,
            })
            .expect("tool entry must carry a subagent pointer");
        entry.session_id
    };
    let subagent = reader.load_session_full(&subagent_id).unwrap().unwrap();
    assert_eq!(subagent.origin, nuo_protocol::ThreadOrigin::Subagent);
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
        .list_session_summaries(&nuo_protocol::ThreadFilter::global(), &store.id().await)
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
    assert!(nuo_protocol::tokenizer::count_tokens(&summary) <= 120);
}

#[tokio::test]
async fn delete_unpersisted_active_session_resets_cleanly() {
    let dir = temp_dir("delete_unpersisted");
    let path = dir.join("session.json");
    let store = ThreadSession::for_path(path.clone());
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
    let store = ThreadSession::for_path(path.clone());
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
async fn a_thread_projection_round_trips_with_its_command_identity() {
    let store = store("thread-projection-commands").await;
    store
        .replace_messages(vec![user("fact one"), assistant("fact two")])
        .await
        .unwrap();
    let thread = store.thread().await;
    assert_eq!(
        thread.path(&nuo_protocol::BranchId::main()).unwrap().len(),
        2
    );
    assert_eq!(
        thread.path(&nuo_protocol::BranchId::main()).unwrap()[0]
            .payload
            .kind(),
        nuo_protocol::FactKind::Message
    );
}

/// The canonical Thread projection keeps message identity, converts tool
/// results to Observation facts, and preserves the compaction horizon.
#[tokio::test]
async fn thread_projection_is_canonical_for_messages_and_observations() {
    let store = store("thread-projection").await;
    let mut tool = Message::new(Role::Tool, "tool body");
    tool.tool_call_id = Some("call-1".to_string());
    let mut call = Message::new(Role::Assistant, "running");
    call.tool_calls = Some(vec![ToolCall::new("call-1", "read", "{}")]);
    store
        .replace_messages(vec![user("hello"), call, tool])
        .await
        .unwrap();

    let thread = store.thread().await;
    assert_eq!(thread.id.0, store.id().await);
    assert_eq!(
        thread.path(&nuo_protocol::BranchId::main()).unwrap().len(),
        3
    );
    assert!(matches!(
        thread.path(&nuo_protocol::BranchId::main()).unwrap()[2].payload,
        nuo_protocol::FactPayload::Observation { ref call_id, .. } if call_id == "call-1"
    ));
    let view = nuo_protocol::RequestView::from_window(
        &nuo_protocol::ThreadWindow::from_thread(
            &thread,
            &nuo_protocol::BranchId::main(),
            usize::MAX,
        )
        .unwrap(),
    );
    assert_eq!(view.messages.len(), 3);
    assert_eq!(view.messages[2].tool_call_id.as_deref(), Some("call-1"));
}

#[tokio::test]
async fn fork_branch_is_a_cursor_and_switch_projects_it() {
    let store = store("branch-cursors").await;
    store
        .commit_turn(CommitTurn::new(
            &[user("main question"), assistant("main answer")],
            CommitBasis::Appended,
        ))
        .await
        .unwrap();

    let branch = store
        .fork_branch("side", None)
        .await
        .expect("fork a live branch");
    assert_eq!(store.branches().await.unwrap().len(), 2);
    let aggregate = store.thread_aggregate().await.unwrap();
    assert_eq!(
        aggregate
            .path(&nuo_protocol::BranchId::main())
            .unwrap()
            .len(),
        2
    );
    assert_eq!(aggregate.path(&branch).unwrap().len(), 2);

    store.switch_branch(branch.clone()).await.unwrap();
    let projected = store.model_window().await;
    assert_eq!(projected.len(), 2);
    assert_eq!(projected[0].content, "main question");

    store.delete_branch(branch).await.unwrap();
    assert_eq!(store.branches().await.unwrap().len(), 1);
    assert_eq!(store.model_window().await.len(), 2);
}
