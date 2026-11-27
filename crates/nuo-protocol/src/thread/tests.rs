use super::*;
use crate::{ObservationLifecycle, ObservationMetrics};
use nuo_tool::Role;

fn notice(text: &str) -> FactPayload {
    FactPayload::SystemNotice {
        source: "test".to_string(),
        notice_type: "note".to_string(),
        content: text.to_string(),
    }
}

fn message(text: &str) -> FactPayload {
    FactPayload::Message {
        message: Message::new(Role::User, text),
    }
}

fn thread() -> Thread {
    Thread::new(ThreadId::new("thread-1"), 1_000)
}

#[test]
fn new_thread_has_a_main_branch() {
    let thread = thread();
    assert_eq!(thread.state.branches.len(), 1);
    assert_eq!(thread.state.active_branch, BranchId::main());
    assert!(thread.path(&BranchId::main()).unwrap().is_empty());
    assert_eq!(thread.revision, ThreadRevision(0));
}

#[test]
fn append_updates_head_and_revision() {
    let mut thread = thread();
    let id = thread
        .append(notice("one"), FactContext::default(), 1_001)
        .unwrap();
    assert_eq!(thread.state.active_branch().unwrap().head, Some(id.clone()));
    assert_eq!(thread.revision, ThreadRevision(1));
    assert_eq!(thread.path(&BranchId::main()).unwrap()[0].id, id);
}

#[test]
fn sequence_is_unique_and_monotonic() {
    let mut thread = thread();
    let first = thread
        .append(notice("one"), FactContext::default(), 1_001)
        .unwrap();
    let _ = first;
    let fact = ThreadFact::new(
        FactId::from_raw("duplicate-seq"),
        None,
        1,
        1_002,
        notice("two"),
        FactContext::default(),
    );
    assert!(matches!(
        thread.graph.insert(fact),
        Err(ThreadError::DuplicateSequence { .. })
    ));
}

#[test]
fn sequence_must_increase_along_the_parent_chain() {
    let mut thread = thread();
    let first = thread
        .append(notice("one"), FactContext::default(), 1_001)
        .unwrap();
    let fact = ThreadFact::new(
        FactId::from_raw("older"),
        Some(first),
        1,
        1_002,
        notice("older"),
        FactContext::default(),
    );
    assert!(matches!(
        thread.graph.insert(fact),
        Err(ThreadError::InvalidFact { .. })
    ));
}

#[test]
fn a_missing_parent_is_rejected() {
    let mut thread = thread();
    let fact = ThreadFact::new(
        FactId::from_raw("orphan"),
        Some(FactId::from_raw("missing")),
        1,
        1_000,
        notice("orphan"),
        FactContext::default(),
    );
    assert!(matches!(
        thread.graph.insert(fact),
        Err(ThreadError::MissingParent { .. })
    ));
}

#[test]
fn fork_is_a_cursor_and_shares_the_prefix() {
    let mut thread = thread();
    let first = thread
        .append(notice("one"), FactContext::default(), 1_001)
        .unwrap();
    let second = thread
        .append(notice("two"), FactContext::default(), 1_002)
        .unwrap();
    let branch = thread
        .fork(first.clone(), "feature", BranchPolicy::default(), 1_003)
        .unwrap();
    assert_eq!(thread.state.branches.len(), 2);
    assert_eq!(
        thread.state.branch(&branch).unwrap().head,
        Some(first.clone())
    );
    assert_eq!(thread.path(&branch).unwrap().len(), 1);
    assert_eq!(thread.path(&BranchId::main()).unwrap().len(), 2);

    thread
        .append_to(&branch, notice("branch"), FactContext::default(), 1_004)
        .unwrap();
    assert_eq!(thread.path(&branch).unwrap().len(), 2);
    assert_eq!(thread.path(&BranchId::main()).unwrap().len(), 2);
    assert_ne!(
        thread.path(&branch).unwrap()[1].id,
        thread.path(&BranchId::main()).unwrap()[1].id
    );
    assert_eq!(second, thread.path(&BranchId::main()).unwrap()[1].id);
}

#[test]
fn fork_rejects_a_fact_outside_the_selected_branch() {
    let mut thread = thread();
    let a = thread
        .append(notice("a"), FactContext::default(), 1_001)
        .unwrap();
    let b = thread
        .append(notice("b"), FactContext::default(), 1_002)
        .unwrap();
    let side = thread
        .fork(a, "side", BranchPolicy::default(), 1_003)
        .unwrap();
    // `b` is only on main, not on the side branch's live path.
    let error = thread
        .fork_from(&side, b, "bad", BranchPolicy::default(), 1_004)
        .unwrap_err();
    assert!(matches!(error, ThreadError::FactNotLive(_)));
}

#[test]
fn compaction_is_branch_scoped() {
    let mut thread = thread();
    let a = thread
        .append(notice("a"), FactContext::default(), 1_001)
        .unwrap();
    let b = thread
        .append(notice("b"), FactContext::default(), 1_002)
        .unwrap();
    let branch = thread
        .fork(b.clone(), "side", BranchPolicy::default(), 1_003)
        .unwrap();

    thread
        .compact(&BranchId::main(), "summary", b.clone(), 100, 1_004)
        .unwrap();
    assert_eq!(
        thread
            .state
            .branch(&BranchId::main())
            .unwrap()
            .compaction_horizon,
        Some(b.clone())
    );
    assert!(
        thread
            .state
            .branch(&branch)
            .unwrap()
            .compaction_horizon
            .is_none()
    );

    let main_path = thread.path(&BranchId::main()).unwrap();
    let side_path = thread.path(&branch).unwrap();
    assert!(side_path.iter().any(|fact| fact.id == a));
    // ADR-0246 `[INV-CMP-08]`: the fold bounds the request window, not the
    // conversation. `a` is still on the branch, and still said.
    assert!(main_path.iter().any(|fact| fact.id == a));
    assert!(main_path.iter().any(|fact| fact.id == b));
    assert!(
        main_path
            .iter()
            .any(|fact| matches!(fact.payload, FactPayload::Compaction { .. }))
    );
}

#[test]
fn the_compaction_checkpoint_names_the_fact_it_kept() {
    let mut thread = thread();
    let a = thread
        .append(notice("a"), FactContext::default(), 1_001)
        .unwrap();
    let b = thread
        .append(notice("b"), FactContext::default(), 1_002)
        .unwrap();
    let checkpoint = thread
        .compact(&BranchId::main(), "summary", b.clone(), 100, 1_003)
        .unwrap();

    let fact = thread.graph.get(&checkpoint).expect("the checkpoint fact");
    let FactPayload::Compaction {
        first_kept_fact_id, ..
    } = &fact.payload
    else {
        panic!("a compact call appends a Compaction fact");
    };
    assert_eq!(
        first_kept_fact_id, &b,
        "the checkpoint names the seam by identity, never by a parsed position"
    );
    // The seam is *derivable* from that reference: the folded range is
    // everything before the kept fact in conversation order.
    let conversation: Vec<_> = thread
        .path(&BranchId::main())
        .unwrap()
        .into_iter()
        .map(|fact| fact.id.clone())
        .collect();
    let kept_index = conversation
        .iter()
        .position(|id| id == &b)
        .expect("the kept fact is on the conversation path");
    assert_eq!(kept_index, 1, "everything before `b` was folded");
    assert_eq!(conversation[0], a);
}

#[test]
fn a_fold_never_moves_a_fact_out_of_the_gc_roots() {
    let mut thread = thread();
    let a = thread
        .append(notice("a"), FactContext::default(), 1_001)
        .unwrap();
    let b = thread
        .append(notice("b"), FactContext::default(), 1_002)
        .unwrap();
    thread
        .compact(&BranchId::main(), "summary", b.clone(), 100, 1_003)
        .unwrap();
    // `a` is folded but still reachable: the kept tail's parent edge crosses
    // the folded range, so retention is not a side effect of a token budget.
    let live = thread.live_facts();
    assert!(live.contains(&a));
    assert!(live.contains(&b));
}

#[test]
fn the_request_window_stops_at_the_horizon() {
    let mut thread = thread();
    let a = thread
        .append(notice("a"), FactContext::default(), 1_001)
        .unwrap();
    let b = thread
        .append(notice("b"), FactContext::default(), 1_002)
        .unwrap();
    thread
        .compact(&BranchId::main(), "summary", b.clone(), 100, 1_003)
        .unwrap();

    let conversation: Vec<_> = thread
        .path(&BranchId::main())
        .unwrap()
        .into_iter()
        .map(|fact| fact.id.clone())
        .collect();
    let window: Vec<_> = thread
        .live_path(&BranchId::main())
        .unwrap()
        .into_iter()
        .map(|fact| fact.id.clone())
        .collect();
    assert!(conversation.contains(&a));
    assert!(
        !window.contains(&a),
        "the horizon bounds the request window"
    );
    assert!(window.contains(&b));
}

#[test]
fn fork_after_compaction_accepts_folded_facts() {
    let mut thread = thread();
    let a = thread
        .append(notice("a"), FactContext::default(), 1_001)
        .unwrap();
    let b = thread
        .append(notice("b"), FactContext::default(), 1_002)
        .unwrap();
    thread
        .compact(&BranchId::main(), "summary", b.clone(), 100, 1_003)
        .unwrap();
    // A fold is context relief, so a folded round is still a fork point: the
    // branch cursor can name any fact the conversation reaches (ADR-0246).
    assert!(
        thread
            .fork(a, "old", BranchPolicy::default(), 1_004)
            .is_ok()
    );
    assert!(
        thread
            .fork(b, "kept", BranchPolicy::default(), 1_005)
            .is_ok()
    );
}

#[test]
fn live_facts_are_the_union_of_branch_paths() {
    let mut thread = thread();
    let a = thread
        .append(notice("a"), FactContext::default(), 1_001)
        .unwrap();
    let b = thread
        .append(notice("b"), FactContext::default(), 1_002)
        .unwrap();
    let branch = thread
        .fork(a.clone(), "side", BranchPolicy::default(), 1_003)
        .unwrap();
    thread
        .compact(&BranchId::main(), "summary", b.clone(), 100, 1_004)
        .unwrap();
    let live = thread.live_facts();
    assert!(live.contains(&a));
    assert!(live.contains(&b));
    assert!(live.contains(&thread.state.branch(&branch).unwrap().head.clone().unwrap()));
}

#[test]
fn folded_facts_stay_reachable_after_sibling_branch_deletion() {
    let mut thread = thread();
    let a = thread
        .append(notice("a"), FactContext::default(), 1_001)
        .unwrap();
    let b = thread
        .append(notice("b"), FactContext::default(), 1_002)
        .unwrap();
    let branch = thread
        .fork(a.clone(), "side", BranchPolicy::default(), 1_003)
        .unwrap();
    thread
        .compact(&BranchId::main(), "summary", b.clone(), 100, 1_004)
        .unwrap();
    assert!(thread.live_facts().contains(&a));
    thread.delete_branch(&branch, 1_005).unwrap();
    assert!(
        thread.live_facts().contains(&a),
        "the folded fact is still on main's conversation path"
    );
    let kept = thread
        .append(notice("kept"), FactContext::default(), 1_006)
        .unwrap();
    thread
        .compact(&BranchId::main(), "second", kept.clone(), 10, 1_007)
        .unwrap();
    assert!(
        thread.live_facts().contains(&kept),
        "the kept fact is live by construction"
    );
}

#[test]
fn delete_branch_keeps_shared_facts() {
    let mut thread = thread();
    let a = thread
        .append(notice("a"), FactContext::default(), 1_001)
        .unwrap();
    let branch = thread
        .fork(a.clone(), "side", BranchPolicy::default(), 1_002)
        .unwrap();
    thread.delete_branch(&branch, 1_003).unwrap();
    assert_eq!(thread.state.branches.len(), 1);
    assert!(thread.graph.contains(&a));
}

#[test]
fn last_branch_cannot_be_deleted() {
    let mut thread = thread();
    let error = thread.delete_branch(&BranchId::main(), 1_001).unwrap_err();
    assert_eq!(error, ThreadError::LastBranch);
}

#[test]
fn duplicate_fact_id_is_rejected() {
    let mut thread = thread();
    let id = thread
        .append(notice("a"), FactContext::default(), 1_001)
        .unwrap();
    let fact = ThreadFact::new(
        id.clone(),
        None,
        99,
        1_002,
        message("duplicate"),
        FactContext::default(),
    );
    assert_eq!(
        thread.graph.insert(fact),
        Err(ThreadError::DuplicateFact(id))
    );
}

#[test]
fn tool_messages_are_not_facts() {
    let mut thread = thread();
    let error = thread
        .append(
            message_with_role(Role::Tool, "tool output"),
            FactContext::default(),
            1_001,
        )
        .unwrap_err();
    assert!(matches!(error, ThreadError::InvalidFact { .. }));
}

fn message_with_role(role: Role, text: &str) -> FactPayload {
    FactPayload::Message {
        message: Message::new(role, text),
    }
}

#[test]
fn tool_observations_round_trip() {
    let mut thread = thread();
    thread
        .append(
            FactPayload::Observation {
                call_id: "call-1".to_string(),
                tool_name: "read".to_string(),
                output_ref: BlobRef::new("hash", 10, "text/plain")
                    .with_preview("preview")
                    .untrusted(),
                metrics: ObservationMetrics::default(),
                lifecycle: ObservationLifecycle::Raw {
                    content: "output".to_string(),
                },
            },
            FactContext::turn(1, 0),
            1_001,
        )
        .unwrap();
    let json = serde_json::to_string(&thread).unwrap();
    let decoded: Thread = serde_json::from_str(&json).unwrap();
    assert_eq!(thread, decoded);
}

#[test]
fn archived_thread_rejects_writes() {
    let mut thread = thread();
    thread.state.status = ThreadStatus::Archived;
    assert_eq!(
        thread
            .append(notice("nope"), FactContext::default(), 1_002)
            .unwrap_err(),
        ThreadError::Archived
    );
}
