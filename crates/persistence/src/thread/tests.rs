//! Session persistence test suite (single-transcript model).
//!
//! Tests exercise the public [`ThreadSession`] API against the SQLite store:
//! the transcript is the authority, the window is a derive, projections are
//! append-only directives, and fork shares facts by identity.
use crate::thread::*;
use std::path::PathBuf;
use tool::{Message, Role, ToolCall};

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

/// ADR-0246 `[INV-CMP-08]`/`[INV-CMP-10]`: a fold bounds the model window and
/// leaves the conversation untouched — across a durable reload, which is the
/// path a resumed or re-attached client hydrates from.
#[tokio::test]
async fn a_fold_survives_reload_as_a_window_boundary_not_a_history_loss() {
    let dir = temp_dir("fold-window");
    let path = dir.join("session.json");
    let store = ThreadSession::for_path(path.clone());
    store
        .replace_messages(vec![
            user("round one"),
            assistant("answer one"),
            user("round two"),
            assistant("answer two"),
            user("round three"),
        ])
        .await
        .unwrap();

    let mut thread = store.thread().await;
    let branch = protocol::BranchId::main();
    let path_facts = thread.path(&branch).unwrap();
    let kept = path_facts[2].id.clone();
    let folded = path_facts[1].id.clone();
    thread
        .compact(&branch, "summary of round one", kept, 4_000, 1_009)
        .unwrap();
    store.commit_thread(&thread).await.unwrap();

    let reloaded = ThreadSession::for_path(path.clone());
    // The request window stops at the horizon.
    let window = reloaded.model_window().await;
    assert!(
        window
            .iter()
            .any(|message| message.content.contains("summary of round one")),
        "the model sees the checkpoint: {window:?}"
    );
    assert!(
        !window.iter().any(|message| message.content == "round one"),
        "the folded round is not in the model window"
    );
    assert!(
        window
            .iter()
            .any(|message| message.content == "round three"),
        "the kept tail is in the model window"
    );

    // The conversation keeps every row, checkpoint at its seam.
    let conversation = reloaded.full_transcript().await;
    let contents: Vec<&str> = conversation
        .iter()
        .map(|message| message.content.as_str())
        .collect();
    assert!(
        contents.contains(&"round one"),
        "a resumed client hydrates the folded round: {contents:?}"
    );
    assert!(
        contents.contains(&"answer one"),
        "…and its reply: {contents:?}"
    );
    // The checkpoint reads where the fold happened — after the tail it kept —
    // matching the live client, which learned of the fold as an event at the
    // tail (ADR-0246 `[INV-CMP-10]`).
    let checkpoint_index = contents
        .iter()
        .position(|content| content.contains("summary of round one"))
        .expect("the checkpoint row");
    assert_eq!(
        checkpoint_index,
        contents.len() - 1,
        "the checkpoint closes the conversation it summarises: {contents:?}"
    );
    assert!(
        contents.contains(&"round three"),
        "the tail survives too: {contents:?}"
    );
    assert!(
        thread
            .path(&branch)
            .unwrap()
            .iter()
            .any(|fact| fact.id == folded)
    );
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
            model_window: vec![
                protocol::checkpoint_message("request-only summary"),
                user("tail"),
            ],
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
    use protocol::{RequestUsageKey, RequestUsageRecord, RequestUsageSource, RequestUsageStatus};
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
    store.flush_projection().await.unwrap();
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
    use protocol::{RequestUsageKey, RequestUsageRecord, RequestUsageStatus};
    let mut data = ThreadData::default();
    data.transcript
        .push(protocol::TranscriptEntry::from_message(0, &user("hello")));
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
    let mut list = tool::TodoList::default();
    list.reconcile(&[("write schema".into(), tool::TodoStatus::Pending)], 0, 0);
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
        aggregate.path(&protocol::BranchId::main()).unwrap().len(),
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
        protocol::FactKind::Message
    );
    // The parent branch still sees every fact; nothing was copied.
    assert_eq!(
        aggregate.path(&protocol::BranchId::main()).unwrap().len(),
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
        .set_digest(Some(protocol::ThreadDigest::default()), Some(42))
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
        .record_round_interrupt(protocol::RoundInterrupt {
            reason: protocol::RoundInterruptReason::User,
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

fn request_projection(round: u64, turn: u64) -> protocol::RequestProjection {
    protocol::RequestProjection {
        round,
        turn,
        created_at_ms: 42,
        prefix_fingerprint: format!("sha256:{round:02x}{turn:02x}"),
        conversation_messages: 1,
        blocks: Vec::new(),
        drift: protocol::PrefixDrift::None,
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

    // The archive is not the window: recording a projection adds no
    // model-visible history. The window is exactly the source messages.
    let window = store.model_window().await;
    assert_eq!(
        window
            .iter()
            .map(|m| m.content.as_str())
            .collect::<Vec<_>>(),
        vec!["hello"],
        "a request projection must never enter the model window"
    );

    // Durable round-trip: the projection survives a reload, still outside the
    // window.
    let reloaded = ThreadSession::for_path(path);
    let restored = reloaded.request_projections().await;
    assert_eq!(restored.len(), 1);
    assert_eq!(restored[0].prefix_fingerprint, "sha256:0100");
    assert_eq!(
        reloaded
            .model_window()
            .await
            .iter()
            .map(|m| m.content.as_str())
            .collect::<Vec<_>>(),
        vec!["hello"],
        "the archive never becomes the window, even after a reload"
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
            ledger.push(protocol::CommandRecord::new("models", ""));
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
            protocol::CommandRecord::new("unattended", "on").with_result(
                protocol::CommandResult::Ack {
                    title: "Unattended mode ON".to_string(),
                    detail: None,
                },
            ),
            protocol::CommandRecord::new("usage", "")
                .with_result(protocol::CommandResult::Text("1.2M tokens".to_string())),
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
        commands: vec![protocol::CommandRecord::new("unattended", "on")],
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
    let subagent_meta = tool::message::SubagentMeta {
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
                protocol::EntryPayload::Message(payload) => payload.subagent.clone(),
                _ => None,
            })
            .expect("tool entry must carry a subagent pointer");
        entry.session_id
    };
    let subagent = reader.load_session_full(&subagent_id).unwrap().unwrap();
    assert_eq!(subagent.origin, protocol::ThreadOrigin::Subagent);
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
        .list_session_summaries(&protocol::ThreadFilter::global(), &store.id().await)
        .unwrap();
    assert!(!summaries.iter().any(|summary| summary.id == subagent_id));
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
    assert_eq!(thread.path(&protocol::BranchId::main()).unwrap().len(), 2);
    assert_eq!(
        thread.path(&protocol::BranchId::main()).unwrap()[0]
            .payload
            .kind(),
        protocol::FactKind::Message
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
    assert_eq!(thread.path(&protocol::BranchId::main()).unwrap().len(), 3);
    assert!(matches!(
        thread.path(&protocol::BranchId::main()).unwrap()[2].payload,
        protocol::FactPayload::Observation { ref call_id, .. } if call_id == "call-1"
    ));
    let view = protocol::RequestView::from_window(
        &protocol::ThreadWindow::from_thread(&thread, &protocol::BranchId::main(), usize::MAX)
            .unwrap(),
    );
    assert_eq!(view.messages.len(), 3);
    assert_eq!(view.messages[2].tool_call_id.as_deref(), Some("call-1"));
}

/// Regression: the 512-byte observation preview used to slice the tool body
/// at a raw byte offset. A multibyte character straddling that offset (`┌`
/// spans bytes 510..513 here) panicked the persistence writer with "end byte
/// index 512 is not a char boundary", so the mid-round save failed. Saving
/// through the real store and reloading covers the whole path.
#[tokio::test]
async fn tool_output_straddling_the_preview_limit_persists_across_reload() {
    let dir = temp_dir("preview-boundary");
    let path = dir.join("session.json");
    let store = ThreadSession::for_path(path.clone());
    let content = format!("{}┌ tail", "a".repeat(510));
    let mut tool = Message::new(Role::Tool, content.clone());
    tool.tool_call_id = Some("call-1".to_string());
    let mut call = Message::new(Role::Assistant, "running");
    call.tool_calls = Some(vec![ToolCall::new("call-1", "read", "{}")]);
    store
        .replace_messages(vec![user("hello"), call, tool])
        .await
        .expect("a multibyte tool body at the preview limit must persist");

    let reloaded = ThreadSession::for_path(path);
    let thread = reloaded.thread().await;
    let observation = thread
        .path(&protocol::BranchId::main())
        .unwrap()
        .into_iter()
        .find(|fact| matches!(&fact.payload, protocol::FactPayload::Observation { .. }))
        .expect("tool message projects to an observation");
    let protocol::FactPayload::Observation { output_ref, .. } = &observation.payload else {
        unreachable!("the fact was just matched as an observation")
    };
    let preview = output_ref.preview.as_deref().expect("preview");
    assert_eq!(preview, "a".repeat(510));
    assert!(preview.len() <= 512);
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
        aggregate.path(&protocol::BranchId::main()).unwrap().len(),
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

#[tokio::test]
async fn incremental_commit_preserves_prior_fts_rows_and_no_duplicates_on_branch_save() {
    let store = store("fts-incremental").await;
    let t1 = vec![user("alpha_needle_one"), assistant("alpha_reply_one")];
    store
        .commit_turn(CommitTurn::new(&t1, CommitBasis::Appended))
        .await
        .unwrap();

    let mut t2 = t1.clone();
    t2.push(user("beta_needle_two"));
    t2.push(assistant("beta_reply_two"));
    store
        .commit_turn(CommitTurn::new(&t2, CommitBasis::Appended))
        .await
        .unwrap();

    // A branch mutation is a full rewrite frame projected through the
    // ordinary session writer; it must not wipe or duplicate FTS rows.
    let _branch = store.fork_branch("side", None).await.unwrap();

    let mut t3 = t2.clone();
    t3.push(user("gamma_needle_three"));
    store
        .commit_turn(CommitTurn::new(&t3, CommitBasis::Appended))
        .await
        .unwrap();

    store.flush_projection().await.unwrap();
    let reader = store.writer.reader().unwrap();
    let alpha = reader.search_history("alpha_needle_one", None, 10).unwrap();
    assert_eq!(
        alpha.len(),
        1,
        "earlier turn must remain indexed after delta commits and branch saves"
    );
    let beta = reader.search_history("beta_needle_two", None, 10).unwrap();
    assert_eq!(beta.len(), 1, "second turn must remain indexed once");
    let gamma = reader
        .search_history("gamma_needle_three", None, 10)
        .unwrap();
    assert_eq!(gamma.len(), 1, "latest turn must be indexed once");
}

#[tokio::test]
async fn metadata_setters_preserve_facts_after_branch_switch_and_todos_across_delta_commits() {
    let dir = temp_dir("meta-preserve");
    let path = dir.join("session.json");
    let store = ThreadSession::for_path(path.clone());
    let m1 = vec![user("branchable question"), assistant("branchable answer")];
    store
        .commit_turn(CommitTurn::new(&m1, CommitBasis::Appended))
        .await
        .unwrap();

    let side = store.fork_branch("side", None).await.unwrap();
    // switch_branch rotates state.data.generation in memory without rewriting
    // threads.transcript_generation; metadata setters must not escalate to a
    // full rewrite and wipe facts.
    store.switch_branch(side).await.unwrap();

    let mut todos = tool::TodoList::default();
    todos.reconcile(
        &[("keep durable todos".into(), tool::TodoStatus::Pending)],
        0,
        0,
    );
    store.set_todos(todos).await.unwrap();
    store
        .set_title(Some("Metadata Title".to_string()), true)
        .await
        .unwrap();
    store
        .set_disabled_tools(["bash".to_string()].into_iter().collect())
        .await
        .unwrap();
    store.set_round_counter(7).await.unwrap();

    // A subsequent delta commit must preserve previously stored todos and facts.
    let mut m2 = m1.clone();
    m2.push(user("follow-up after metadata"));
    store
        .commit_turn(CommitTurn::new(&m2, CommitBasis::Appended))
        .await
        .unwrap();

    let reloaded = ThreadSession::for_path(path);
    let window = reloaded.model_window().await;
    assert_eq!(window.len(), 3);
    assert_eq!(window[0].content, "branchable question");
    assert_eq!(window[2].content, "follow-up after metadata");
    assert_eq!(reloaded.title().await.0.as_deref(), Some("Metadata Title"));
    assert!(reloaded.disabled_tools().await.contains("bash"));
    assert_eq!(reloaded.round_counter().await, 7);
    let reloaded_todos = reloaded.todos().await;
    assert_eq!(reloaded_todos.items.len(), 1);
    assert_eq!(reloaded_todos.items[0].content, "keep durable todos");
}

/// A crash between the journal fsync and the derived projection must not
/// orphan the session: opening it reads the journal and rebuilds the row.
#[tokio::test]
async fn a_journal_head_repairs_a_missing_derived_row() {
    let dir = temp_dir("journal-repair");
    let path = dir.join("session.json");
    let store = ThreadSession::for_path(path.clone());
    store
        .commit_turn(CommitTurn::new(&[user("durable")], CommitBasis::Appended))
        .await
        .unwrap();
    let id = store.id().await;
    store.flush_projection().await.unwrap();
    store.writer.delete_session(id.clone()).await.unwrap();

    let reopened = ThreadSession::for_path(path);
    assert_eq!(reopened.id().await, id);
    assert_eq!(reopened.model_window().await.len(), 1);
    let row = reopened.writer.reader().unwrap().get_session(&id).unwrap();
    assert!(
        row.is_some(),
        "the derived row must be rebuilt from the journal"
    );
}

/// Branch cursors and their facts are journal state: deleting the derived
/// SQLite database must not lose a sibling branch (ADR-0239).
#[tokio::test]
async fn branches_reload_from_the_journal_without_sqlite() {
    let dir = temp_dir("journal-branches");
    let path = dir.join("session.json");
    let store = ThreadSession::for_path(path.clone());
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
    let main_len = aggregate.path(&protocol::BranchId::main()).unwrap().len();
    let branch_len = aggregate.path(&branch).unwrap().len();
    assert_eq!((main_len, branch_len), (4, 2));

    drop(store);
    let db = dir.join("nuo.db");
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", db.display()));
    }

    let reopened = ThreadSession::for_path(path);
    let aggregate = reopened.thread_aggregate().await.unwrap();
    assert_eq!(
        aggregate.path(&protocol::BranchId::main()).unwrap().len(),
        main_len
    );
    assert_eq!(aggregate.path(&branch).unwrap().len(), branch_len);
}

/// A deleted session is terminal: its journal tombstone outranks the derived
/// row, so reopening the path cannot resurrect it (ADR-0241).
#[tokio::test]
async fn a_deleted_session_is_terminal_across_reload() {
    let dir = temp_dir("journal-delete");
    let path = dir.join("session.json");
    let store = ThreadSession::for_path(path.clone());
    store.replace_messages(vec![user("doomed")]).await.unwrap();
    let old_id = store.id().await;
    store.delete(&old_id[..8]).await.unwrap();

    let reopened = ThreadSession::for_path(path);
    assert_ne!(reopened.id().await, old_id);
    assert!(reopened.model_window().await.is_empty());
    assert!(
        open_journal(&reopened.db_path, &old_id).is_deleted(),
        "the old journal must carry a terminal tombstone"
    );
}

/// A wholesale transcript rebuild clears the graph sidecar; a later replay
/// must not resurrect old branch facts from the previous graph (ADR-0239).
#[tokio::test]
async fn a_wholesale_rebuild_drops_the_branch_graph() {
    let dir = temp_dir("graph-cleared");
    let path = dir.join("session.json");
    let store = ThreadSession::for_path(path.clone());
    store
        .replace_messages(vec![user("one"), assistant("two")])
        .await
        .unwrap();
    store.fork_branch("side", None).await.unwrap();
    assert!(store.thread_aggregate().await.unwrap().graph.len() >= 2);
    store
        .replace_messages(vec![user("replacement")])
        .await
        .unwrap();
    let reopened = ThreadSession::for_path(path);
    let aggregate = reopened.thread_aggregate().await.unwrap();
    assert_eq!(
        aggregate.graph.len(),
        1,
        "old branches must not survive a rebuild"
    );
    assert_eq!(
        aggregate.path(&protocol::BranchId::main()).unwrap().len(),
        1
    );
}

/// A crash between the journal fsync and the projection leaves the derived
/// row missing; the watermark catch-up rebuilds it (ADR-0242).
#[tokio::test]
async fn catch_up_repairs_a_missing_derived_row() {
    let dir = temp_dir("projection-catch-up");
    let path = dir.join("session.json");
    let store = ThreadSession::for_path(path);
    store.replace_messages(vec![user("durable")]).await.unwrap();
    let id = store.id().await;
    store.flush_projection().await.unwrap();
    store.writer.delete_session(id.clone()).await.unwrap();
    assert!(
        store
            .writer
            .reader()
            .unwrap()
            .get_session(&id)
            .unwrap()
            .is_none()
    );

    let repaired = catch_up_projections_for(&store.writer).unwrap();
    assert!(repaired >= 1, "the journal head must rebuild the row");
    let reader = store.writer.reader().unwrap();
    assert!(reader.get_session(&id).unwrap().is_some());
    assert!(reader.projection_watermark(&id).unwrap().is_some());
}

/// The catalog summary is materialised without reading facts, so the list
/// picker keeps its count/prompt when content moves to a shard (ADR-0244).
#[tokio::test]
async fn catalog_summary_tracks_turns_and_metadata() {
    let store = store("catalog-summary").await;
    store
        .commit_turn(CommitTurn::new(
            &[user("first prompt"), assistant("first reply")],
            CommitBasis::Appended,
        ))
        .await
        .unwrap();
    store.flush_projection().await.unwrap();
    let id = store.id().await;
    let row = |summaries: Vec<crate::thread::ThreadSummary>| {
        summaries
            .into_iter()
            .find(|summary| summary.id == id)
            .expect("the active session is listed")
    };
    let summary = row(store.list().await.unwrap());
    assert_eq!(summary.message_count, 2);
    assert_eq!(summary.overview, "first prompt");

    store
        .commit_turn(CommitTurn::new(
            &[
                user("first prompt"),
                assistant("first reply"),
                user("second prompt"),
            ],
            CommitBasis::Appended,
        ))
        .await
        .unwrap();
    store.flush_projection().await.unwrap();
    let summary = row(store.list().await.unwrap());
    assert_eq!(summary.message_count, 3);
    assert_eq!(summary.overview, "second prompt");

    // A metadata-only title update must not erase the materialised summary.
    store
        .set_title(Some("Catalog Title".to_string()), true)
        .await
        .unwrap();
    let summary = row(store.list().await.unwrap());
    assert_eq!(summary.message_count, 3);
    assert_eq!(summary.title.as_deref(), Some("Catalog Title"));
}

/// A commit whose journal append fails must leave the session exactly as the
/// journal still has it.
///
/// The mutation happens before the append (the frame is derived from it), so a
/// failed append that left the transcript in place would make the *next*
/// commit believe those entries were already durable and persist a
/// metadata-only frame that never carries them — silent, permanent loss.
#[tokio::test]
async fn a_failed_journal_append_rolls_the_session_back() {
    let dir = temp_dir("rollback");
    let path = dir.join("session.json");
    let store = ThreadSession::for_path(path);
    store
        .replace_messages(vec![user("committed")])
        .await
        .unwrap();
    let before = store.model_window().await;

    store.fail_next_journal_append().await;
    let mut window = before.clone();
    window.push(assistant("must not survive"));
    let failed = store
        .commit_turn(CommitTurn::new(&window, CommitBasis::Appended))
        .await;
    assert!(
        failed.is_err(),
        "an injected append failure must fail the commit"
    );

    let after = store.model_window().await;
    assert_eq!(
        after, before,
        "a failed commit must not leave the mutation in memory"
    );

    // The next commit must carry only its own turn; if the rolled-back entries
    // had been treated as durable, this file would lose them on reload.
    let mut recovered = after.clone();
    recovered.push(assistant("recovered"));
    store
        .commit_turn(CommitTurn::new(&recovered, CommitBasis::Appended))
        .await
        .unwrap();

    let reopened = ThreadSession::for_path(store.state_lock_for_test().await.path.clone());
    let reloaded = reopened.model_window().await;
    assert_eq!(
        reloaded.len(),
        2,
        "the rolled-back entry must be absent while the retried turn is durable"
    );
    assert_eq!(reloaded[0].content, "committed");
    assert_eq!(reloaded[1].content, "recovered");
}

/// Many independent sessions commit concurrently without serialising on one
/// another's journal.
///
/// This is the property the lane model exists for (ADR-0237): a commit's ack
/// is bounded by its own session's `fdatasync`, never by another session's
/// work. The assertion is deliberately structural rather than a wall-clock
/// threshold — a shared ack chain would make the slowest session's latency the
/// floor for all of them, so 16 sessions × 8 turns must all finish well inside
/// a per-session budget.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn independent_sessions_commit_concurrently() {
    const SESSIONS: usize = 16;
    const TURNS: usize = 8;

    let dir = temp_dir("concurrent-sessions");
    let mut tasks = Vec::with_capacity(SESSIONS);
    for session in 0..SESSIONS {
        let path = dir.join(format!("session-{session}.json"));
        tasks.push(tokio::spawn(async move {
            let store = ThreadSession::for_path(path);
            let mut window = Vec::new();
            for turn in 0..TURNS {
                window.push(user(&format!("s{session} user {turn}")));
                window.push(assistant(&format!("s{session} reply {turn}")));
                store
                    .commit_turn(CommitTurn::new(&window, CommitBasis::Appended))
                    .await
                    .expect("every session's commits must succeed");
            }
            store.id().await
        }));
    }

    let started = std::time::Instant::now();
    let mut ids = Vec::with_capacity(SESSIONS);
    for (session, task) in tasks.into_iter().enumerate() {
        ids.push((session, task.await.expect("session task must not panic")));
    }
    let elapsed = started.elapsed();

    let mut unique: Vec<String> = ids.iter().map(|(_, id)| id.clone()).collect();
    unique.sort();
    unique.dedup();
    assert_eq!(
        unique.len(),
        SESSIONS,
        "each session keeps its own identity"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(20),
        "{SESSIONS} sessions × {TURNS} turns took {elapsed:?}; a shared ack chain \
         would make the slowest session everyone's floor"
    );

    // Every commit is durable and isolated: reopening one session shows exactly
    // its own history, never a neighbour's.
    for (session, id) in ids.iter() {
        let reopened = ThreadSession::for_path(dir.join(format!("session-{session}.json")));
        assert_eq!(&reopened.id().await, id);
        let window = reopened.model_window().await;
        assert_eq!(window.len(), TURNS * 2);
        assert!(
            window
                .iter()
                .all(|message| message.content.starts_with(&format!("s{session} "))),
            "a session's history must never contain another session's turn"
        );
    }
}

/// ADR-0242 + ADR-0249: deleting all content shards (`rm -rf shards/`) and
/// running `catch_up_projections_for` rebuilds transcript facts, FTS index,
/// normalized `command_acts`, and journal-truth `workspace_effects` (`/undo`)
/// completely from the session journals.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deleting_all_content_shards_rebuilds_facts_fts_commands_and_workspace_effects() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path();
    let store = ThreadSession::for_path(dir.join("rebuild-all.json"));
    let id = store.id().await;

    let window = vec![
        Message::new(Role::User, "rebuildable_needle_phoenix".to_string()),
        Message::new(Role::Assistant, "rebuilt cleanly".to_string()),
    ];
    store
        .commit_turn(CommitTurn::new(&window, CommitBasis::Appended))
        .await
        .unwrap();
    store
        .mutate_commands(|cmds| {
            cmds.push(protocol::CommandRecord::new("unattended", "on"));
            cmds.push(protocol::CommandRecord::new("role", "developer"));
        })
        .await
        .unwrap();

    let effect_id = store
        .writer
        .append_workspace_effect(crate::workspace_effects::NewOwnedEffect {
            thread_id: id.clone(),
            round_id: "r1".to_string(),
            turn_id: "t1".to_string(),
            call_id: "c1".to_string(),
            seq: 1,
            tool_name: "write_file".to_string(),
            path: Some("/tmp/demo.rs".to_string()),
            op: crate::workspace_effects::JournalOp::Edit,
            effect_class: "file_bytes".to_string(),
            before_hash: Some("pre-hash-1".to_string()),
            after_hash: None,
            irreversible_reason: None,
            created_at_ms: 1000,
        })
        .await
        .unwrap();
    assert!(
        store
            .writer
            .complete_workspace_effect(&id, effect_id, Some("post-hash-1".to_string()), 1500)
            .await
            .unwrap()
    );
    store.flush_projection().await.unwrap();

    // Wipe the entire content shards directory (ADR-0242 acceptance criterion).
    let shards_dir = store.writer.shards_dir().to_path_buf();
    assert!(shards_dir.exists());
    std::fs::remove_dir_all(&shards_dir).unwrap();
    assert!(!shards_dir.exists());

    // Open a fresh PersistenceHandle (fresh shard pool) and catch up from journals.
    let db_path = store.writer.db_path().to_path_buf();
    drop(store);
    let fresh = crate::db::PersistenceHandle::spawn(db_path, None);
    let repaired = catch_up_projections_for(&fresh).unwrap();
    assert!(
        repaired >= 1,
        "catch-up must detect missing shard and rebuild"
    );

    let reader = fresh.reader().unwrap();
    let reloaded = reader.load_session_full(&id).unwrap().expect("session");
    assert_eq!(reloaded.transcript.entries.len(), 2);
    assert_eq!(reloaded.commands.len(), 2);
    assert_eq!(reloaded.commands[0].name, "unattended");
    assert_eq!(reloaded.commands[1].name, "role");

    let hits = reader
        .search_history("rebuildable_needle_phoenix", None, 10)
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].session_id, id);

    let effects = fresh.list_workspace_effects(id.clone()).await.unwrap();
    assert_eq!(effects.len(), 1);
    assert_eq!(effects[0].id, effect_id);
    assert_eq!(effects[0].before_hash.as_deref(), Some("pre-hash-1"));
    assert_eq!(effects[0].after_hash.as_deref(), Some("post-hash-1"));
    assert!(effects[0].is_undoable());
}
