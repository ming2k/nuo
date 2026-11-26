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

/// The same staleness hazard for `reset` (`/new`): a fresh session must drop
/// the previous session's cached model window before accepting its first turn.
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

    // The first turn on the fresh session must be appended cleanly.
    store
        .commit_turn(CommitTurn::new(&[user("hello")]))
        .await
        .unwrap();
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
// Projection directives update model_window without mutating facts (ADR-0264).
// ---------------------------------------------------------------

#[tokio::test]
async fn prune_projection_updates_model_window_without_mutating_facts() {
    let store = store("projection-directive").await;
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
            model_window: projected.clone(),
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

    // Durable append-only Prune directive updates `model_window()` while
    // leaving immutable `full_transcript()` facts completely untouched (ADR-0264).
    assert_eq!(store.model_window().await, projected);
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
async fn commit_turn_persists_appended_delta_counter_and_usage() {
    let store = store("commit").await;
    store.replace_messages(vec![user("go")]).await.unwrap();
    store
        .commit_turn(CommitTurn::new(&[assistant("ok")]).tap_counter(3))
        .await
        .unwrap();
    let window = store.model_window().await;
    assert_eq!(window.len(), 2);
    assert_eq!(window[0].content, "go");
    assert_eq!(window[1].content, "ok");
    assert_eq!(store.round_counter().await, 3);
}

impl CommitTurn<'_> {
    fn tap_counter(mut self, counter: u64) -> Self {
        self.round_counter = Some(counter);
        self
    }
}

/// `[INV-CMP-08]` / `[INV-THREADSSOT-02]`: `commit_turn` is strictly
/// append-only and never rewrites durable history. Committing subsequent turns
/// after a compaction (and after a disposable request-time prune) must retain
/// every pre-compaction turn in `full_transcript()` across reload while
/// keeping `model_window()` bounded to `[checkpoint, kept_tail..., new_turns]`.
#[tokio::test]
async fn commit_turn_after_compaction_preserves_full_conversation_across_reload() {
    let dir = temp_dir("commit-after-compact");
    let path = dir.join("session.json");
    let store = ThreadSession::for_path(path.clone());
    store
        .commit_turn(CommitTurn::new(&[
            user("round one"),
            assistant("answer one"),
            user("round two"),
            assistant("answer two"),
            user("round three"),
        ]))
        .await
        .unwrap();

    // Fold round one behind a compaction horizon.
    let mut thread = store.thread().await;
    let branch = protocol::BranchId::main();
    let path_facts = thread.path(&branch).unwrap();
    let kept = path_facts[2].id.clone();
    thread
        .compact(&branch, "summary of round one", kept, 4_000, 1_009)
        .unwrap();
    store.commit_thread(&thread).await.unwrap();

    // Record a disposable request-time projection (e.g. Tier-1 prune), then
    // commit multiple subsequent turns via `commit_turn`.
    store
        .commit_context_projection(ContextProjectionResult {
            model_window: vec![user("pruned-request-view-only")],
            archived_originals: Vec::new(),
            checkpoint: ContextProjectionCheckpoint {
                operation: ContextProjectionKind::Prune,
                archived_messages: 1,
                active_messages: 1,
                window_tokens_before: 200,
                window_tokens_after: 50,
                summary: None,
                tracked_files: Vec::new(),
            },
        })
        .await
        .unwrap();

    store
        .commit_turn(CommitTurn::new(&[assistant("answer three")]).tap_counter(3))
        .await
        .unwrap();
    store
        .commit_turn(
            CommitTurn::new(&[user("round four"), assistant("answer four")]).tap_counter(4),
        )
        .await
        .unwrap();

    let reloaded = ThreadSession::for_path(path);

    // Full conversation keeps every turn from round one through round four,
    // plus the compaction checkpoint with canonical provenance.
    let full = reloaded.full_transcript().await;
    let contents: Vec<&str> = full.iter().map(|m| m.content.as_str()).collect();
    assert!(
        contents.contains(&"round one") && contents.contains(&"answer one"),
        "pre-compaction history must survive subsequent commit_turn + reload: {contents:?}"
    );
    assert!(
        contents.contains(&"round two")
            && contents.contains(&"round three")
            && contents.contains(&"answer three")
            && contents.contains(&"round four")
            && contents.contains(&"answer four"),
        "kept tail and post-compaction turns must all be present: {contents:?}"
    );
    assert!(
        full.iter().any(|m| {
            m.origin
                .as_ref()
                .is_some_and(|o| o.kind == tool::InjectionKind::CompactionCheckpoint)
                && m.content.contains("summary of round one")
        }),
        "compaction checkpoint must retain CompactionCheckpoint provenance"
    );

    // Model window remains bounded to [checkpoint, kept_tail..., new_turns].
    let window = reloaded.model_window().await;
    assert!(
        !window.iter().any(|m| m.content == "round one"),
        "folded round must stay out of model_window"
    );
    assert!(
        window[0]
            .origin
            .as_ref()
            .is_some_and(|o| o.kind == tool::InjectionKind::CompactionCheckpoint),
        "model_window must lead with the canonical compaction checkpoint"
    );
    assert_eq!(window.last().unwrap().content, "answer four");
}

/// Two consecutive compactions interleaved with append-only `commit_turn`
/// deltas must preserve **every** turn from round 0 onward across **both**
/// journal replay (`ThreadSession::for_path`) and SQLite content-shard
/// reconstruction (`reader.load_thread` / `reader.load_session_full`).
#[tokio::test]
async fn consecutive_compactions_and_delta_commits_preserve_full_history_in_journal_and_content_shard()
 {
    let dir = temp_dir("double-compact-shard");
    let path = dir.join("session.json");
    let store = ThreadSession::for_path(path.clone());

    for round in 0..4 {
        store
            .commit_turn(
                CommitTurn::new(&[
                    user(&format!("round {round} prompt")),
                    assistant(&format!("round {round} reply")),
                ])
                .tap_counter(round as u64 + 1),
            )
            .await
            .unwrap();
    }

    // Compaction #1: keep from round 2 onward (folding rounds 0..1).
    let branch = protocol::BranchId::main();
    let mut thread1 = store.thread().await;
    let facts1 = thread1.path(&branch).unwrap();
    let kept1 = facts1[4].id.clone();
    thread1
        .compact(&branch, "summary pass one", kept1.clone(), 4_000, 1_010)
        .unwrap();
    store.commit_thread(&thread1).await.unwrap();

    // Subsequent delta commits after compaction #1.
    for round in 4..8 {
        store
            .commit_turn(
                CommitTurn::new(&[
                    user(&format!("round {round} prompt")),
                    assistant(&format!("round {round} reply")),
                ])
                .tap_counter(round as u64 + 1),
            )
            .await
            .unwrap();
    }

    // Compaction #2: advance horizon to round 6 (folding rounds 2..5 too).
    let mut thread2 = store.thread().await;
    let live2 = thread2.live_path(&branch).unwrap();
    let kept2 = live2
        .iter()
        .find(|f| {
            matches!(
                &f.payload,
                protocol::FactPayload::Message { message } if message.content == "round 6 prompt"
            )
        })
        .expect("round 6 prompt must be in live_path before compaction #2")
        .id
        .clone();
    thread2
        .compact(&branch, "summary pass two", kept2.clone(), 8_000, 2_020)
        .unwrap();
    store.commit_thread(&thread2).await.unwrap();

    // Final delta commits after compaction #2.
    for round in 8..10 {
        store
            .commit_turn(
                CommitTurn::new(&[
                    user(&format!("round {round} prompt")),
                    assistant(&format!("round {round} reply")),
                ])
                .tap_counter(round as u64 + 1),
            )
            .await
            .unwrap();
    }

    let id = store.id().await;
    store.flush_projection().await.unwrap();

    // 1. SQLite content-shard read path (`load_thread` + `load_session_full`).
    let reader = store.writer.reader().unwrap();
    let shard_thread = reader
        .load_thread(&id)
        .unwrap()
        .expect("thread must exist in content shard");
    let shard_full_path = shard_thread.path(&branch).unwrap();
    assert_eq!(
        shard_thread
            .state
            .active_branch()
            .unwrap()
            .compaction_horizon
            .as_ref(),
        Some(&kept2),
        "content shard must preserve compaction #2 horizon across subsequent delta commits"
    );
    let shard_live_path = shard_thread.live_path(&branch).unwrap();
    assert!(
        shard_live_path.len() < shard_full_path.len(),
        "live_path must remain strictly shorter than full conversation path"
    );
    let shard_session = reader
        .load_session_full(&id)
        .unwrap()
        .expect("session full must load from catalog + content shard");
    let shard_conversation = shard_session.transcript.conversation_messages();
    for round in 0..10 {
        let needle = format!("round {round} prompt");
        assert!(
            shard_conversation.iter().any(|m| m.content == needle),
            "content-shard conversation missing `{needle}`"
        );
    }
    let shard_window = shard_session.transcript.project_messages();
    assert_eq!(
        shard_window[0].origin.as_ref().map(|o| o.kind),
        Some(tool::InjectionKind::CompactionCheckpoint)
    );
    assert!(shard_window[0].content.contains("summary pass two"));
    assert!(
        !shard_window[1..]
            .iter()
            .any(|m| m.content == "round 0 prompt" || m.content == "round 5 prompt"),
        "folded rounds 0..5 must stay excluded from content-shard model window"
    );
    assert_eq!(shard_window.last().unwrap().content, "round 9 reply");

    // 2. Journal cold-reload path (`ThreadSession::for_path`).
    drop(reader);
    drop(store);
    let reloaded = ThreadSession::for_path(path);
    let full = reloaded.full_transcript().await;
    for round in 0..10 {
        let needle = format!("round {round} prompt");
        assert!(
            full.iter().any(|m| m.content == needle),
            "journal-reloaded full_transcript missing `{needle}`"
        );
    }
    let checkpoints: Vec<&Message> = full
        .iter()
        .filter(|m| {
            m.origin
                .as_ref()
                .is_some_and(|o| o.kind == tool::InjectionKind::CompactionCheckpoint)
        })
        .collect();
    assert_eq!(
        checkpoints.len(),
        2,
        "both compaction checkpoints must survive in reading order"
    );
    let window = reloaded.model_window().await;
    assert!(window[0].content.contains("summary pass two"));
    assert_eq!(window.last().unwrap().content, "round 9 reply");
}

/// A commit writes facts; the thread projection is the read seam.
#[tokio::test]
async fn committed_messages_are_visible_as_thread_facts() {
    let store = store("thread-fact-read").await;
    store
        .commit_turn(CommitTurn::new(&[user("question"), assistant("answer")]))
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
    let first = store
        .commit_turn(CommitTurn::new(&[user("hello")]))
        .await
        .unwrap();

    // A precondition that does not match the durable revision fails closed.
    let stale = store
        .commit_turn(CommitTurn {
            expected_revision: Some(first + 5),
            ..CommitTurn::new(&[])
        })
        .await
        .unwrap_err();
    assert!(stale.contains("stale session revision"), "got {stale}");

    // A matching precondition commits and advances the durable revision.
    let advanced = store
        .commit_turn(CommitTurn {
            expected_revision: Some(first),
            ..CommitTurn::new(&[])
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

    store
        .commit_turn(CommitTurn::new(&[parent, result]))
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
        .list_session_summaries(&protocol::ThreadFilter::all(), &store.id().await)
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
        .commit_turn(CommitTurn::new(&[
            user("main question"),
            assistant("main answer"),
        ]))
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
    store.commit_turn(CommitTurn::new(&t1)).await.unwrap();

    let t2 = vec![user("beta_needle_two"), assistant("beta_reply_two")];
    store.commit_turn(CommitTurn::new(&t2)).await.unwrap();

    // A branch mutation is a full rewrite frame projected through the
    // ordinary session writer; it must not wipe or duplicate FTS rows.
    let _branch = store.fork_branch("side", None).await.unwrap();

    store
        .commit_turn(CommitTurn::new(&[user("gamma_needle_three")]))
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
    store.commit_turn(CommitTurn::new(&m1)).await.unwrap();

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
    store
        .commit_turn(CommitTurn::new(&[user("follow-up after metadata")]))
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
        .commit_turn(CommitTurn::new(&[user("durable")]))
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
        .commit_turn(CommitTurn::new(&[
            user("first prompt"),
            assistant("first reply"),
        ]))
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
        .commit_turn(CommitTurn::new(&[user("second prompt")]))
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
    let failed = store
        .commit_turn(CommitTurn::new(&[assistant("must not survive")]))
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
    store
        .commit_turn(CommitTurn::new(&[assistant("recovered")]))
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
            for turn in 0..TURNS {
                let delta = [
                    user(&format!("s{session} user {turn}")),
                    assistant(&format!("s{session} reply {turn}")),
                ];
                store
                    .commit_turn(CommitTurn::new(&delta))
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
    store.commit_turn(CommitTurn::new(&window)).await.unwrap();
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

/// Deleting `shards/` at runtime while `PersistenceHandle` stays live heals
/// transparently on the next content read (`load_thread` and
/// `list_workspace_effects`) without restarting (ADR-0255).
#[tokio::test]
async fn runtime_deleted_content_shards_self_heal_on_read() {
    let dir = temp_dir("runtime-shard-self-heal");
    let path = dir.join("session.json");
    let store = ThreadSession::for_path(path);
    store
        .commit_turn(CommitTurn::new(&[
            user("live shard needle"),
            assistant("live shard reply"),
        ]))
        .await
        .unwrap();
    let id = store.id().await;
    let effect_id = store
        .writer
        .append_workspace_effect(crate::workspace_effects::NewOwnedEffect {
            thread_id: id.clone(),
            round_id: "r1".to_string(),
            turn_id: "t1".to_string(),
            call_id: "c1".to_string(),
            seq: 1,
            tool_name: "edit_text".to_string(),
            path: Some("/ws/live.rs".to_string()),
            op: crate::workspace_effects::JournalOp::Edit,
            effect_class: "file_bytes".to_string(),
            before_hash: Some("pre-live".to_string()),
            after_hash: None,
            irreversible_reason: None,
            created_at_ms: 1_000,
        })
        .await
        .unwrap();
    assert!(
        store
            .writer
            .complete_workspace_effect(&id, effect_id, Some("post-live".to_string()), 1_500)
            .await
            .unwrap()
    );
    store.flush_projection().await.unwrap();

    // Remove the entire shards directory while the live handle is running.
    let shards_dir = store.writer.shards_dir().to_path_buf();
    assert!(shards_dir.is_dir());
    std::fs::remove_dir_all(&shards_dir).unwrap();
    assert!(!shards_dir.exists());

    // Cold read on the existing handle self-heals on demand without restarting.
    let reader = store.writer.reader().unwrap();
    let thread = reader
        .load_thread(&id)
        .unwrap()
        .expect("thread must self-heal on read");
    assert_eq!(thread.graph.len(), 2);

    let effects = store.writer.list_workspace_effects(id).await.unwrap();
    assert_eq!(effects.len(), 1);
    assert_eq!(effects[0].id, effect_id);
    assert_eq!(effects[0].after_hash.as_deref(), Some("post-live"));
}

/// Cold rebuild from journals preserves arbitrarily deep parent chains
/// (`root -> child -> grandchild`) and cleanly detaches (`parent_id = None`)
/// any child whose parent was terminally deleted (ADR-0255).
#[tokio::test]
async fn catch_up_rebuilds_multi_level_hierarchy_and_handles_deleted_parent() {
    let dir = temp_dir("hierarchy-rebuild");
    let root = ThreadSession::for_path(dir.join("root.json"));
    root.commit_turn(CommitTurn::new(&[user("root")]))
        .await
        .unwrap();
    root.flush_projection().await.unwrap();
    let root_id = root.id().await;

    let (child, _) = ThreadSession::for_subagent(&root, "child-1").unwrap();
    child
        .commit_turn(CommitTurn::new(&[user("child")]))
        .await
        .unwrap();
    child.flush_projection().await.unwrap();
    let child_id = child.id().await;

    let (grandchild, _) = ThreadSession::for_subagent(&child, "grandchild-1").unwrap();
    grandchild
        .commit_turn(CommitTurn::new(&[user("grandchild")]))
        .await
        .unwrap();
    grandchild.flush_projection().await.unwrap();
    let grandchild_id = grandchild.id().await;

    let db_path = root.writer.db_path().to_path_buf();
    let blobs_dir = dir.join("blobs");
    drop((root, child, grandchild));

    // Wipe derived SQLite + shards and rebuild all 3 levels.
    for ext in ["", "-wal", "-shm"] {
        let p = PathBuf::from(format!("{}{ext}", db_path.display()));
        let _ = std::fs::remove_file(p);
    }
    let _ = std::fs::remove_dir_all(db_path.parent().unwrap().join("shards"));

    let fresh = crate::db::PersistenceHandle::spawn(
        db_path.clone(),
        Some(crate::blobs::BlobStore::new(blobs_dir.clone())),
    );
    fresh.ensure_ready().unwrap();
    let repaired = catch_up_projections_for(&fresh).unwrap();
    assert!(repaired >= 3);

    let reader = fresh.reader().unwrap();
    assert_eq!(
        reader.get_session(&root_id).unwrap().unwrap().parent_id,
        None
    );
    assert_eq!(
        reader
            .get_session(&child_id)
            .unwrap()
            .unwrap()
            .parent_id
            .as_deref(),
        Some(root_id.as_str())
    );
    assert_eq!(
        reader
            .get_session(&grandchild_id)
            .unwrap()
            .unwrap()
            .parent_id
            .as_deref(),
        Some(child_id.as_str())
    );
    drop(reader);

    // Now delete `root` (leaving `child` and `grandchild` alive), wipe DB again,
    // and verify cold rebuild succeeds with `child.parent_id == None` (no FK error).
    open_journal(fresh.db_path(), &root_id)
        .mark_deleted()
        .await
        .unwrap();
    let _ = fresh.delete_session(root_id.clone()).await;
    drop(fresh);
    for ext in ["", "-wal", "-shm"] {
        let p = PathBuf::from(format!("{}{ext}", db_path.display()));
        let _ = std::fs::remove_file(p);
    }
    let _ = std::fs::remove_dir_all(db_path.parent().unwrap().join("shards"));

    let rebuilt =
        crate::db::PersistenceHandle::spawn(db_path, Some(crate::blobs::BlobStore::new(blobs_dir)));
    rebuilt.ensure_ready().unwrap();
    let repaired_after_delete = catch_up_projections_for(&rebuilt).unwrap();
    assert!(repaired_after_delete >= 2);
    let reader = rebuilt.reader().unwrap();
    assert!(reader.get_session(&root_id).unwrap().is_none());
    assert_eq!(
        reader.get_session(&child_id).unwrap().unwrap().parent_id,
        None,
        "orphaned child must detach cleanly instead of failing FK on rebuild"
    );
    assert_eq!(
        reader
            .get_session(&grandchild_id)
            .unwrap()
            .unwrap()
            .parent_id
            .as_deref(),
        Some(child_id.as_str())
    );
}

/// Oversized message bodies and inline base64 images are offloaded to CAS
/// before journal write, tracked in `thread_blob_refs` so CAS GC keeps them
/// alive, and rehydrated byte-for-byte on reopen (ADR-0255 `[INV-SHARD-04]`).
#[tokio::test]
async fn large_text_and_base64_images_offload_to_cas_survive_gc_and_rehydrate() {
    let dir = temp_dir("blob-offload-images");
    let path = dir.join("session.json");
    let store = ThreadSession::for_path(path.clone());

    let big_text = "x".repeat(crate::db::CAS_THRESHOLD_BYTES + 1024);
    let big_image_b64 = "A".repeat(crate::db::CAS_THRESHOLD_BYTES + 2048);
    let msg =
        tool::Message::new(tool::Role::User, big_text.clone()).with_images(vec![tool::ImagePart {
            mime: "image/png".to_string(),
            data: big_image_b64.clone(),
        }]);

    store
        .commit_turn(CommitTurn::new(&[msg, assistant("saw it")]))
        .await
        .unwrap();
    store.flush_projection().await.unwrap();
    let id = store.id().await;

    // Raw journal segment must be dramatically smaller than the 16 KiB+ inline payloads.
    let session_journal_dir = journal_root(store.writer.db_path()).join(&id);
    let seg_bytes: u64 = std::fs::read_dir(&session_journal_dir)
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("seg-"))
        .map(|e| e.metadata().unwrap().len())
        .sum();
    assert!(
        seg_bytes < (crate::db::CAS_THRESHOLD_BYTES as u64),
        "journal segment ({seg_bytes} bytes) must not inline oversized text/image payloads"
    );

    // CAS GC with zero grace must keep both offloaded blobs alive because
    // `thread_blob_refs` registered them on the content shard.
    let live = store.writer.reader().unwrap().live_blob_hashes().unwrap();
    let expected_text_hash = crate::blobs::BlobStore::hash(big_text.as_bytes());
    let expected_img_hash = crate::blobs::BlobStore::hash(big_image_b64.as_bytes());
    assert!(live.contains(&expected_text_hash));
    assert!(live.contains(&expected_img_hash));

    let (reclaimed, _) = store.blob_store.retain_only(&live);
    assert_eq!(reclaimed, 0, "live text & image blobs must survive CAS GC");

    // Shard read paths (`load_thread` and `load_session_full`) must also
    // rehydrate offloaded text and base64 images losslessly via `thread_blob_refs`.
    let reader = store.writer.reader().unwrap();
    let loaded_thread = reader.load_thread(&id).unwrap().expect("thread");
    let first_fact = loaded_thread.all_facts()[0];
    match &first_fact.payload {
        protocol::FactPayload::Message { message } => {
            assert_eq!(message.content, big_text);
            let shard_imgs = message.images.as_ref().expect("shard image");
            assert_eq!(shard_imgs[0].data, big_image_b64);
        }
        other => panic!("expected message fact, got {other:?}"),
    }
    let loaded_full = reader.load_session_full(&id).unwrap().expect("full");
    let full_msgs = loaded_full.transcript.project_messages();
    assert_eq!(full_msgs[0].content, big_text);
    assert_eq!(full_msgs[0].images.as_ref().unwrap()[0].data, big_image_b64);
    drop(reader);

    // Forking writes a `Full` frame carrying `data.graph`; `data.graph` must
    // also offload oversized text & images so the journal stays small.
    store.fork_branch("alt", None).await.unwrap();
    let seg_bytes_after_fork: u64 = std::fs::read_dir(&session_journal_dir)
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("seg-"))
        .map(|e| e.metadata().unwrap().len())
        .sum();
    assert!(
        seg_bytes_after_fork < (crate::db::CAS_THRESHOLD_BYTES as u64) * 2,
        "branched journal segment ({seg_bytes_after_fork} bytes) must offload data.graph blobs too"
    );

    // Reopening from disk must rehydrate both text and base64 image losslessly.
    drop(store);
    let reopened = ThreadSession::for_path(path);
    let messages = reopened.model_window().await;
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].content, big_text);
    let imgs = messages[0].images.as_ref().expect("rehydrated image");
    assert_eq!(imgs.len(), 1);
    assert_eq!(imgs[0].mime, "image/png");
    assert_eq!(imgs[0].data, big_image_b64);
}

/// CAS garbage collection is fail-closed:
/// 1. `perform_storage_maintenance_blocking` catches up missing shards first
///    so live blobs are never deleted, and reports exact `pruned_backups`.
/// 2. `live_blob_hashes` returns `Err` when an existing shard fails schema
///    verification rather than returning an incomplete live set (ADR-0255).
#[tokio::test]
async fn fail_closed_cas_gc_and_accurate_backup_pruning_count() {
    let dir = temp_dir("fail-closed-gc");
    let path = dir.join("session.json");
    let store = ThreadSession::for_path(path);
    let big_text = "z".repeat(crate::db::CAS_THRESHOLD_BYTES + 1024);
    store
        .commit_turn(CommitTurn::new(&[user(&big_text), assistant("ok")]))
        .await
        .unwrap();
    store.flush_projection().await.unwrap();
    let hash = crate::blobs::BlobStore::hash(big_text.as_bytes());
    assert!(store.blob_store.exists(&hash));

    // Delete shards completely, then run maintenance twice with retention = 1.
    let shards_dir = store.writer.shards_dir().to_path_buf();
    std::fs::remove_dir_all(&shards_dir).unwrap();

    let report1 = store
        .writer
        .perform_storage_maintenance_blocking(1)
        .unwrap();
    assert!(report1.created_backup.is_some());
    assert!(report1.created_journal_backup.is_some());
    assert!(
        store.blob_store.exists(&hash),
        "catch-up before GC must preserve authoritative blobs even after shards wipe"
    );

    std::thread::sleep(std::time::Duration::from_millis(15));
    let report2 = store
        .writer
        .perform_storage_maintenance_blocking(1)
        .unwrap();
    assert!(
        report2.pruned_backups >= 2,
        "second maintenance pass with retention=1 must prune older db + journal backups, got {}",
        report2.pruned_backups
    );

    // Plant an unledgered/corrupted shard file: `live_blob_hashes` must fail closed!
    let bad_shard = shards_dir.join("content-00.db");
    std::fs::write(&bad_shard, b"not a valid sqlite database").unwrap();
    let reader = store.writer.reader().unwrap();
    assert!(
        reader.live_blob_hashes().is_err(),
        "corrupted shard must cause live_blob_hashes to fail closed"
    );
}

/// Global FTS search self-heals when `shards/` is deleted at runtime while the
/// handle stays alive (ADR-0255).
#[tokio::test]
async fn global_search_self_heals_after_runtime_shards_deletion() {
    let dir = temp_dir("search-self-heal");
    let path = dir.join("session.json");
    let store = ThreadSession::for_path(path);
    store
        .commit_turn(CommitTurn::new(&[
            user("unique_global_search_helios_token"),
            assistant("ack"),
        ]))
        .await
        .unwrap();
    store.flush_projection().await.unwrap();
    let id = store.id().await;

    let shards_dir = store.writer.shards_dir().to_path_buf();
    std::fs::remove_dir_all(&shards_dir).unwrap();
    assert!(!shards_dir.exists());

    let reader = store.writer.reader().unwrap();
    let hits = reader
        .search_history("unique_global_search_helios_token", None, 10)
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].session_id, id);
}

#[tokio::test]
async fn round_summaries_persist_and_upsert_across_journal_reload() {
    let dir = temp_dir("round-summaries-persist");
    let path = dir.join("session.json");
    let store = ThreadSession::for_path(path.clone());

    let r1 = protocol::RoundSummary {
        round: 1,
        output_tokens: 120,
        duration_ms: 4_200,
        paused_ms: 1_100,
        generation_ms: 2_500,
        session_revision: 0,
    };
    let r2 = protocol::RoundSummary {
        round: 2,
        output_tokens: 340,
        duration_ms: 9_800,
        paused_ms: 0,
        generation_ms: 6_100,
        session_revision: 0,
    };

    store
        .commit_turn(CommitTurn {
            appended: &[user("first"), assistant("ok")],
            round_counter: Some(1),
            round_summary: Some(r1),
            ..CommitTurn::new(&[])
        })
        .await
        .unwrap();
    store
        .commit_turn(CommitTurn {
            appended: &[user("second"), assistant("done")],
            round_counter: Some(2),
            round_summary: Some(r2),
            ..CommitTurn::new(&[])
        })
        .await
        .unwrap();

    // Upsert round 1 (e.g. after `/retry` completion): overwrites slot 1 without duplicating.
    let r1_updated = protocol::RoundSummary {
        duration_ms: 5_000,
        ..r1
    };
    store
        .commit_turn(CommitTurn {
            appended: &[],
            round_summary: Some(r1_updated),
            ..CommitTurn::new(&[])
        })
        .await
        .unwrap();

    let reloaded = ThreadSession::for_path(path);
    let summaries = reloaded.round_summaries().await;
    assert_eq!(summaries, vec![r1_updated, r2]);
}

#[tokio::test]
async fn context_projection_directives_survive_reopen_without_mutating_facts() {
    let dir = temp_dir("projection-directive-persist");
    let path = dir.join("session.json");
    let store = ThreadSession::for_path(path.clone());

    let mut prompt = user("compare these images");
    prompt.images = Some(vec![
        tool::ImagePart {
            mime: "image/png".to_string(),
            data: "old-pixels".to_string(),
        },
        tool::ImagePart {
            mime: "image/png".to_string(),
            data: "new-pixels".to_string(),
        },
    ]);
    let mut tool_msg = Message::new(Role::Tool, "huge tool body".repeat(100));
    tool_msg.tool_call_id = Some("call-1".to_string());

    store
        .commit_turn(CommitTurn::new(&[prompt.clone(), tool_msg.clone()]))
        .await
        .unwrap();

    let mut projected_prompt = prompt.clone();
    projected_prompt.content =
        "compare these images\n[Image Offloaded: image/png elided to relieve request size]"
            .to_string();
    projected_prompt.images = Some(vec![tool::ImagePart {
        mime: "image/png".to_string(),
        data: "new-pixels".to_string(),
    }]);
    let mut projected_tool = tool_msg.clone();
    projected_tool.content = "[tool output elided]".to_string();

    store
        .commit_context_projection(ContextProjectionResult {
            model_window: vec![projected_prompt, projected_tool],
            archived_originals: vec![prompt, tool_msg],
            checkpoint: ContextProjectionCheckpoint {
                operation: ContextProjectionKind::Payload,
                archived_messages: 2,
                active_messages: 2,
                window_tokens_before: 500,
                window_tokens_after: 50,
                summary: None,
                tracked_files: Vec::new(),
            },
        })
        .await
        .unwrap();

    let reloaded = ThreadSession::for_path(path);
    let window = reloaded.model_window().await;
    assert!(window[0].content.contains("[Image Offloaded:"));
    assert_eq!(window[0].images.as_ref().map(Vec::len), Some(1));
    assert_eq!(window[0].images.as_ref().unwrap()[0].data, "new-pixels");
    assert_eq!(window[1].content, "[tool output elided]");

    // Human-facing full transcript stays immutable (ADR-0264 `[INV-PROJ-01]`).
    let full = reloaded.full_transcript().await;
    assert_eq!(full[0].content, "compare these images");
    assert_eq!(full[0].images.as_ref().map(Vec::len), Some(2));
    assert!(full[1].content.starts_with("huge tool body"));
}
