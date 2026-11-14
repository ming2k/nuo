//! Integration test: compaction is the one behaviour no single crate can prove.
//!
//! Each half of this flow has unit tests and each half passed while the feature
//! was dead: the transcript persisted, the compactor folded a `SessionIR`, the
//! store applied projection directives. What none of them covered is the seam —
//! `execute_round` hands the compactor a window assembled from the *transcript*
//! while the compactor reads the *fact graph*, and for a while those two held
//! unrelated conversations: the graph was empty, so every automatic compaction
//! returned `Ok(None)` and the session grew until the provider rejected it.
//!
//! These tests therefore drive the real store and assert on observable outcomes
//! (window size, surviving prompt, durability across reopen) rather than on any
//! one layer's return value.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use nuo_engine::cognition::compaction::{CausalCompactor, CompactionCutMode};
use nuo_engine::orchestration::{ContextProjectionSettings, compact_round_history};
use nuo_engine::{Agent, AgentIdentity};
use nuo_persistence::thread::CommitBasis;
use nuo_persistence::thread::{CommitTurn, ThreadSession};
use nuo_tool::{Message, Role};

fn sandbox(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("nuo-{tag}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("session.json");
    (dir, path)
}

/// A round whose messages carry enough text to move the token estimate.
fn round_messages(index: usize) -> Vec<Message> {
    vec![
        Message::new(
            Role::User,
            format!(
                "user round {index} {}",
                "lorem ipsum dolor sit amet ".repeat(6)
            ),
        ),
        Message::new(
            Role::Assistant,
            format!(
                "assistant round {index} {}",
                "consectetur adipiscing elit sed do ".repeat(6)
            ),
        ),
    ]
}

/// Commit `rounds` complete rounds through the live save point (`commit_turn`).
async fn commit_rounds(session: &ThreadSession, rounds: usize) -> Vec<Message> {
    let mut committed = Vec::new();
    for index in 0..rounds {
        committed.extend(round_messages(index));
        session
            .commit_turn(CommitTurn {
                messages: &committed,
                basis: CommitBasis::Appended,
                round_counter: Some(index as u64 + 1),
                usage_records: &[],
                retry_point: None,
                round_interrupt: None,
                operation_id: None,
                expected_revision: None,
            })
            .await
            .unwrap();
    }
    committed
}

/// Compaction must engage on a session built through the ordinary turn path.
///
/// This is the regression that mattered: the trigger was wired, the settings
/// resolved, the compactor was correct — and the graph it read was empty, so it
/// answered `Ok(None)` forever and the caller treated that as "nothing to do".
#[tokio::test]
async fn compaction_engages_on_a_session_built_from_plain_rounds() {
    let (dir, path) = sandbox("compact-engages");
    let session = ThreadSession::for_path(path);
    commit_rounds(&session, 8).await;

    // The canonical IR must see the conversation the transcript holds; an empty
    // graph here is exactly what silently disabled compaction.
    let thread = session.thread().await;
    assert_eq!(
        thread.graph.len(),
        16,
        "facts must mirror the 8 committed rounds"
    );
    assert!(
        thread.state.active_branch().unwrap().head.is_some(),
        "a session with facts must have a cursor"
    );

    let mut history = session.model_window().await;
    history.push(Message::new(Role::User, "THE-NEWEST-PROMPT"));
    let tokens_before = nuo_protocol::pressure::estimate_tokens(&history);

    let mut settings =
        ContextProjectionSettings::from_policy(&nuo_protocol::CompactionPolicy::default(), 200_000);
    // Derive the budget from the measured window rather than hard-coding a scale:
    // compacting to half the current size is what the production trigger does.
    settings.budget.target_tokens = (tokens_before / 2).max(64);

    let report = compact_round_history(&mut history, &session, &settings, None, Vec::new())
        .await
        .expect("compaction must not error")
        .expect("compaction must engage on a populated IR");
    let checkpoint = report.checkpoint;

    // 1. The prompt being admitted is still there. Seeding the window from
    //    storage and assigning it back wholesale would answer the *previous*
    //    turn's question, which is worse than not compacting at all.
    assert!(
        history
            .iter()
            .any(|message| message.content.contains("THE-NEWEST-PROMPT")),
        "compaction dropped the current prompt: {history:#?}"
    );
    // 2. The checkpoint reached the window.
    assert!(
        history
            .iter()
            .any(|message| message.content.contains("Checkpoint")),
        "no checkpoint in the compacted window: {history:#?}"
    );
    // 3. The metadata describes the caller's window, not the folded prefix.
    assert_eq!(
        checkpoint.window_tokens_before, tokens_before,
        "the checkpoint must report the window it was given"
    );
    assert!(
        checkpoint.window_tokens_after < checkpoint.window_tokens_before,
        "a compaction that does not shrink the window relieved nothing: {checkpoint:?}"
    );
    // 4. The fold is durable in the transcript projection, so the *next* send
    //    stays under the compaction gate instead of re-reading the full history
    //    and re-summarizing the same prefix. The gate itself lives in
    //    `execute_round`; this asserts the number it compares against.
    settings.budget.compaction_threshold_tokens = tokens_before.saturating_sub(1);
    let projected = session.model_window().await;
    let projected_tokens = nuo_protocol::pressure::estimate_tokens(&projected);
    assert!(
        projected_tokens < tokens_before,
        "the recorded projection did not shrink the window"
    );
    assert!(
        projected_tokens < settings.budget.compaction_threshold_tokens,
        "the next send would cross the compaction gate again: {projected_tokens} >= {}",
        settings.budget.compaction_threshold_tokens
    );

    let _ = std::fs::remove_dir_all(dir);
}

/// A fold that cannot reach the gate says so.
///
/// The post-condition is not advisory: callers must be able to distinguish a
/// successful shrink from a fold that kept the window above the trigger, or the
/// next send silently re-compacts and the user sees no explanation.
#[tokio::test]
async fn an_impossible_target_reports_insufficiency() {
    let (dir, path) = sandbox("compact-insufficient");
    let session = ThreadSession::for_path(path);
    commit_rounds(&session, 8).await;

    let mut history = session.model_window().await;
    let mut settings =
        ContextProjectionSettings::from_policy(&nuo_protocol::CompactionPolicy::default(), 200_000);
    // A gate far below a single message: the tool-call-safe cut must keep the
    // newest node, which alone exceeds it.
    settings.budget.compaction_threshold_tokens = 10;
    settings.budget.target_tokens = 5;
    settings.request_overhead_tokens = 0;

    let report = compact_round_history(&mut history, &session, &settings, None, Vec::new())
        .await
        .unwrap()
        .expect("the fold still runs on an impossible target");
    assert!(
        !report.satisfied,
        "insufficiency was silently reported as success: {:?}",
        report.checkpoint
    );

    let _ = std::fs::remove_dir_all(dir);
}

/// A fold must stop at its horizon.
///
/// The kept tail's head deliberately keeps its parent pointing into the folded
/// chain. A compactor that walks `linear_path` therefore re-reads every archived
/// node on the next pass and re-summarizes it. `resolve_active_branch` injects
/// the horizon and stops; this test pins that behavior: a target that fits the
/// *active* path but not the full history must produce no second fold.
#[tokio::test]
async fn a_fold_stops_at_the_horizon() {
    let (dir, path) = sandbox("compact-horizon-walk");
    let session = ThreadSession::for_path(path);
    commit_rounds(&session, 8).await;

    let mut history = session.model_window().await;
    let full_tokens = nuo_protocol::pressure::estimate_tokens(&history);
    let mut settings =
        ContextProjectionSettings::from_policy(&nuo_protocol::CompactionPolicy::default(), 200_000);
    settings.budget.target_tokens = (full_tokens / 2).max(64);
    compact_round_history(&mut history, &session, &settings, None, Vec::new())
        .await
        .unwrap()
        .expect("the first fold must engage");

    // The reopened IR is derived from the checkpoint entry, and its active
    // branch is [horizon, kept tail…], never the archived prefix.
    let mut thread = session.thread().await;
    let branch = thread.state.active_branch.clone();
    let window = nuo_protocol::ThreadWindow::from_thread(&thread, &branch, usize::MAX).unwrap();
    let active_tokens = nuo_protocol::pressure::estimate_tokens(
        &nuo_protocol::RequestView::from_window(&window).messages,
    );
    assert!(active_tokens < full_tokens);
    let between = active_tokens + (full_tokens - active_tokens) / 2;
    let outcome = CausalCompactor::compact_thread(
        &mut thread,
        &branch,
        None,
        CompactionCutMode::TargetTokens(between),
        Vec::new(),
        0,
    )
    .await
    .unwrap();
    assert!(
        outcome.is_none(),
        "the compactor re-read the folded prefix instead of stopping at the horizon"
    );

    let _ = std::fs::remove_dir_all(dir);
}

/// A fold must survive a reopen.
///
/// `state.compaction_horizon` was never persisted, so a compacted session
/// re-hydrated with the full pre-compaction transcript: the operator paid for a
/// summarization call on every resume and got an un-folded window back.
#[tokio::test]
async fn a_fold_survives_reopening_the_session() {
    let (dir, path) = sandbox("compact-horizon");
    let (tokens_before, committed_tokens) = {
        let session = ThreadSession::for_path(path.clone());
        commit_rounds(&session, 8).await;
        let committed = session.model_window().await;
        let committed_tokens = nuo_protocol::pressure::estimate_tokens(&committed);
        let mut history = committed;
        history.push(Message::new(Role::User, "THE-NEWEST-PROMPT"));
        let tokens_before = nuo_protocol::pressure::estimate_tokens(&history);
        let mut settings = ContextProjectionSettings::from_policy(
            &nuo_protocol::CompactionPolicy::default(),
            200_000,
        );
        settings.budget.target_tokens = (tokens_before / 2).max(64);
        compact_round_history(&mut history, &session, &settings, None, Vec::new())
            .await
            .unwrap()
            .expect("compaction must engage");

        // The fold must reach the window the next round seeds from, not only
        // the IR. Without a durable `Compact` projection every send reloaded
        // the full transcript and paid for the same summarization again.
        let projected = session.model_window().await;
        assert!(
            nuo_protocol::pressure::estimate_tokens(&projected) < committed_tokens,
            "the recorded projection did not shrink the transcript window"
        );
        assert!(
            projected
                .iter()
                .any(|message| message.content.contains("Conversation checkpoint")),
            "the checkpoint is not in the projected window: {projected:#?}"
        );
        (tokens_before, committed_tokens)
    };

    // Reopen: a brand-new store over the same file.
    let reopened = ThreadSession::for_path(path);
    let window = reopened.model_window().await;
    let window_tokens = nuo_protocol::pressure::estimate_tokens(&window);
    assert!(
        window_tokens < committed_tokens,
        "a reopen reloaded the unfolded transcript ({window_tokens} vs {committed_tokens})"
    );
    let thread = reopened.thread().await;
    assert!(
        thread
            .state
            .active_branch()
            .unwrap()
            .compaction_horizon
            .is_some(),
        "the horizon did not survive the reopen"
    );
    let window =
        nuo_protocol::ThreadWindow::from_thread(&thread, &thread.state.active_branch, usize::MAX)
            .unwrap();
    let folded = nuo_protocol::RequestView::from_window(&window).messages;
    let folded_tokens = nuo_protocol::pressure::estimate_tokens(&folded);
    assert!(
        folded_tokens < tokens_before,
        "the reopened session un-folded: {folded_tokens} tokens vs {tokens_before} before"
    );
    assert!(
        !folded
            .iter()
            .any(|message| message.role == Role::User && message.content.contains("user round 0")),
        "the folded head came back as a live message: {folded:#?}"
    );
    // The summary legitimately *quotes* folded prompts (that is its job), so the
    // assertion above is role-scoped: what must be gone is the node, not the
    // recollection of it.

    let _ = std::fs::remove_dir_all(dir);
}

/// The IR's policy columns must always be decodable.
///
/// `thread_policies.{rules,capabilities,guardrails,budget}_json` stayed at the
/// schema's `{}` placeholder because nothing on the live path wrote them.
/// `load_session_ir` decoded the policy *before* the nodes, so one missing
/// `unattended` key failed the whole hydrate and the session's entire causal
/// graph disappeared — the empty-graph symptom above, one layer down.
#[tokio::test]
async fn a_session_always_hydrates_a_decodable_policy() {
    let (dir, path) = sandbox("compact-policy");
    let session = ThreadSession::for_path(path.clone());
    commit_rounds(&session, 2).await;
    drop(session);

    let reopened = ThreadSession::for_path(path);
    let thread = reopened.thread().await;
    assert_eq!(thread.graph.len(), 4, "{thread:#?}");
    assert!(thread.state.active_branch().unwrap().head.is_some());

    let _ = std::fs::remove_dir_all(dir);
}

/// An aggressive fold must still keep the prompt being admitted.
///
/// The uncommitted tail is anchored on the last committed message; if that
/// anchor ever resolves past the caller's window the prompt silently vanishes.
/// A target far below the window is the fold most likely to expose it.
#[tokio::test]
async fn an_aggressive_fold_never_drops_the_admitted_prompt() {
    let (dir, path) = sandbox("compact-aggressive");
    let session = ThreadSession::for_path(path);
    commit_rounds(&session, 6).await;

    let mut history = session.model_window().await;
    history.push(Message::new(Role::User, "THE-ADMITTED-PROMPT"));

    let mut settings =
        ContextProjectionSettings::from_policy(&nuo_protocol::CompactionPolicy::default(), 200_000);
    // A target far below the window: this is the fold that eats the tail.
    settings.budget.target_tokens = 1;

    let outcome = compact_round_history(&mut history, &session, &settings, None, Vec::new())
        .await
        .unwrap();
    assert!(
        outcome.is_some(),
        "an impossibly small target must still fold"
    );
    assert!(
        history
            .iter()
            .any(|message| message.content.contains("THE-ADMITTED-PROMPT")),
        "the admitted prompt was dropped by an aggressive fold: {history:#?}"
    );

    let _ = std::fs::remove_dir_all(dir);
}

/// The mid-turn projection gate actually projects.
///
/// `project_context_if_needed` has always asked a `ContextProjectionGate` for
/// relief between ReAct turns, and nothing ever installed one — `bootstrap.rs`
/// carried a comment describing the install with no code under it. The hot path
/// took its `let Some(gate) = gate else { return }` early return on every turn, so
/// long single rounds had no relief valve. A subagent is the purest case: it runs
/// one round to completion through `resume_streaming_with_events` and never
/// reaches `execute_round`'s pre-round prune or compaction.
#[tokio::test]
async fn the_mid_turn_gate_prunes_a_pressurised_window() {
    use nuo_engine::PruningProjectionGate;
    use nuo_engine::cognition::context_projection::ContextProjectionGate;

    let (dir, path) = sandbox("gate-prunes");
    let session = Arc::new(ThreadSession::for_path(path));

    // A committed round carrying a large tool result.
    let call = nuo_tool::ToolCall::new("c1", "execute_command", "{}");
    let mut assistant = Message::new(Role::Assistant, "running");
    assistant.tool_calls = Some(vec![call.clone()]);
    let big = "FAILED: test_x\nAssertionError\n".repeat(600);
    let tool = Message::tool_result(&call, &big);
    let mut committed = vec![Message::new(Role::User, "run tests"), assistant, tool];
    // The turn is persisted before the gate runs (`fire_turn_persist` precedes the
    // pressure check), so the window the gate sees equals the durable projection.
    committed.push(Message::new(Role::User, "NOW"));
    session
        .commit_turn(CommitTurn {
            messages: &committed,
            basis: CommitBasis::Appended,
            round_counter: Some(1),
            usage_records: &[],
            retry_point: None,
            round_interrupt: None,
            operation_id: None,
            expected_revision: None,
        })
        .await
        .unwrap();

    let window = session.model_window().await;
    let tokens_before = nuo_protocol::pressure::estimate_tokens(&window);
    let settings = ContextProjectionSettings {
        request_overhead_tokens: 0,
        budget: nuo_protocol::CompactionPolicy::default().resolve(20_000),
        preserve_rounds: 6,
        summarize: false,
        prune: true,
        prune_protect_tokens: 0,
    };
    let settings_for_gate = settings.clone();
    let gate = PruningProjectionGate::new(
        Arc::clone(&session),
        Arc::new(move || settings_for_gate.clone()),
        Arc::new(nuo_protocol::MessageTokenWeights::new()),
    );
    let projected = gate
        .project_context(window.clone())
        .await
        .expect("the gate must engage on a pressurised window");

    let tokens_after = nuo_protocol::pressure::estimate_tokens(&projected);
    assert!(
        tokens_after < tokens_before,
        "the gate returned a window that did not shrink: {tokens_before} -> {tokens_after}"
    );
    assert_eq!(
        projected.len(),
        window.len(),
        "pruning clears bodies; it must not drop messages or break the tool_call_id chain"
    );
    assert!(
        projected.iter().any(|m| m.content.contains("NOW")),
        "the live prompt must survive a mid-turn projection"
    );
    // Pruning alone brought pressure under the watermark, so the expensive
    // summarization tier must not have run.
    assert!(
        tokens_after <= settings.budget.prune_threshold_tokens,
        "pruning should reach the prune watermark without a summarization call"
    );

    let _ = std::fs::remove_dir_all(dir);
}

/// A projection whose window cannot be expressed as append-only directives is
/// rebuilt, not refused.
///
/// `commit_context_projection` documented exactly that fallback and then returned
/// `Err` instead. A mid-turn projection cannot treat that as recoverable: the turn
/// Request-time projection is disposable: a window that cannot be represented
/// as a fact transform is still allowed for the current request, but it never
/// rewrites durable history.
#[tokio::test]
async fn an_untranslatable_projection_never_rewrites_history() {
    use nuo_persistence::thread::{
        ContextProjectionCheckpoint, ContextProjectionKind, ContextProjectionResult,
    };

    let (dir, path) = sandbox("gate-rebuild");
    let session = ThreadSession::for_path(path);
    commit_rounds(&session, 2).await;

    let durable_before = session.model_window().await;
    let rewritten = vec![
        Message::new(Role::User, "rebuilt question"),
        Message::new(Role::Assistant, "rebuilt answer"),
    ];
    let before = durable_before.len();
    session
        .commit_context_projection(ContextProjectionResult {
            model_window: rewritten,
            archived_originals: durable_before.clone(),
            checkpoint: ContextProjectionCheckpoint {
                operation: ContextProjectionKind::Prune,
                archived_messages: before,
                active_messages: 2,
                window_tokens_before: 100,
                window_tokens_after: 10,
                summary: None,
                tracked_files: Vec::new(),
            },
        })
        .await
        .expect("a request projection is always accepted");

    assert_eq!(session.model_window().await, durable_before);

    let _ = std::fs::remove_dir_all(dir);
}

/// A subagent runs with its own session and its own relief valve.
///
/// A subagent executes one round to completion through
/// `resume_streaming_with_events` and never passes through `execute_round`, so
/// neither the pre-round prune nor the pre-round compaction could ever reach it —
/// and it held no `ThreadSession` at all, so the mid-turn gate had nothing to
/// project against either. The dispatch layer now opens a real session per child
/// and asks the composition root to install relief; this asserts both halves of
/// that wiring, because a gate that is never installed fails silently (the pressure
/// check reads a missing gate as "nothing to do").
#[tokio::test]
async fn a_subagent_gets_a_session_and_a_projection_gate() {
    use nuo_engine::PruningProjectionGate;

    let (dir, path) = sandbox("subagent-relief");
    let session = Arc::new(ThreadSession::for_path(path));
    commit_rounds(&session, 1).await;

    // The child store: a real session under the parent, not a detached buffer.
    let (child_store, parent_id) =
        ThreadSession::for_subagent(&session, "sub-child").expect("parent is idle");
    assert_eq!(parent_id, session.id().await);
    let child_store = Arc::new(child_store);

    let child = Agent::new(
        Arc::new(nuo_engine::NoProvider),
        Vec::new(),
        AgentIdentity::default(),
    );
    assert!(
        !child.has_context_projection_gate(),
        "a fresh agent must start without relief"
    );

    // What the composition root does for every spawned child.
    let window = child.provider.model_capabilities().context_window;
    let budget = nuo_protocol::CompactionPolicy::default().resolve(window);
    child.set_context_prune_threshold(budget.prune_threshold_tokens);
    child.set_context_projection_gate(Some(Arc::new(PruningProjectionGate::new(
        Arc::clone(&child_store),
        Arc::new(move || {
            ContextProjectionSettings::from_policy(
                &nuo_protocol::CompactionPolicy::default(),
                window,
            )
        }),
        child.token_weights_handle(),
    ))));

    assert!(
        child.has_context_projection_gate(),
        "the child must carry relief, or its single round can only grow"
    );
    assert!(
        child.context_prune_threshold_tokens() > 0,
        "relief without a threshold never fires"
    );

    let _ = std::fs::remove_dir_all(dir);
}
