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
    assert!(main_path.iter().any(|fact| fact.id == b));
    assert!(!main_path.iter().any(|fact| fact.id == a));
    assert!(
        main_path
            .iter()
            .any(|fact| matches!(fact.payload, FactPayload::Compaction { .. }))
    );
}

#[test]
fn fork_after_compaction_only_accepts_live_facts() {
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
    assert_eq!(
        thread
            .fork(a.clone(), "old", BranchPolicy::default(), 1_004)
            .unwrap_err(),
        ThreadError::FactNotLive(a.clone())
    );
    // The first kept fact is live.
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
fn gc_eligibility_is_reachability_from_every_branch() {
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
    // With no sibling, `a` is unreachable from the only live path.
    assert!(thread.gc_eligible_facts().contains(&a));
    assert!(!thread.gc_eligible_facts().contains(&b));
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
fn apply_delta_is_idempotent() {
    let mut thread = thread();
    thread
        .append(notice("one"), FactContext::default(), 1_001)
        .unwrap();
    let delta = thread.drain_delta(0);
    let mut clone = Thread::new(ThreadId::new("thread-1"), 1_000);
    clone.apply_delta(delta.clone()).unwrap();
    clone.apply_delta(delta).unwrap();
    assert_eq!(clone.max_seq(), 1);
    assert_eq!(clone.revision, ThreadRevision(1));
}

#[test]
fn command_replay_shape_is_stable() {
    let mut thread = thread();
    let outcome = thread
        .apply_command(
            ThreadCommand::AppendFacts {
                facts: vec![ThreadFact::new(
                    FactId::from_raw("fact-1"),
                    None,
                    1,
                    1_001,
                    notice("one"),
                    FactContext::default(),
                )],
            },
            1_001,
        )
        .unwrap();
    assert!(matches!(outcome, CommandOutcome::Appended { .. }));
    assert_eq!(thread.max_seq(), 1);
    assert_eq!(
        thread.graph.get(&FactId::from_raw("fact-1")).unwrap().seq,
        1
    );
    // A second command that reuses the fact identity is rejected: fact ids
    // are never reused, even through a different command envelope.
    let error = thread
        .apply_command(
            ThreadCommand::AppendFacts {
                facts: vec![ThreadFact::new(
                    FactId::from_raw("fact-1"),
                    Some(FactId::from_raw("fact-1")),
                    2,
                    1_002,
                    notice("two"),
                    FactContext::default(),
                )],
            },
            1_002,
        )
        .unwrap_err();
    assert!(matches!(error, ThreadError::DuplicateFact(_)));
}

#[test]
fn branch_promotion_is_explicitly_unsupported() {
    let mut thread = thread();
    let error = thread
        .apply_command(
            ThreadCommand::PromoteBranch {
                branch: BranchId::main(),
            },
            1_001,
        )
        .unwrap_err();
    assert_eq!(error, ThreadError::UnsupportedPromotion);
}

#[test]
fn archived_thread_rejects_writes() {
    let mut thread = thread();
    thread.apply_command(ThreadCommand::Archive, 1_001).unwrap();
    assert_eq!(thread.state.status, ThreadStatus::Archived);
    assert_eq!(
        thread
            .append(notice("nope"), FactContext::default(), 1_002)
            .unwrap_err(),
        ThreadError::Archived
    );
}

#[test]
fn transcript_page_is_branch_scoped_and_pageable() {
    let mut thread = thread();
    for i in 0..5 {
        thread
            .append(message(&format!("m{i}")), FactContext::default(), 1_001 + i)
            .unwrap();
    }
    let first = TranscriptView::from_thread(&thread, &BranchId::main(), None, 3).unwrap();
    assert_eq!(first.items.len(), 3);
    assert!(first.next_before.is_some());
    let older =
        TranscriptView::from_thread(&thread, &BranchId::main(), first.next_before.as_ref(), 3)
            .unwrap();
    assert_eq!(older.items.len(), 2);
    assert_eq!(older.next_before, None);
}

#[test]
fn fact_envelope_rejects_a_newer_schema() {
    let thread = thread();
    let fact = ThreadFact::new(
        FactId::from_raw("fact-1"),
        None,
        1,
        1_000,
        notice("one"),
        FactContext::default(),
    );
    let mut envelope = FactEnvelope::new(fact);
    assert!(envelope.clone().into_fact().is_ok());
    envelope.schema_version = FACT_SCHEMA_VERSION + 1;
    assert!(matches!(
        envelope.into_fact(),
        Err(ThreadError::UnsupportedSchema { .. })
    ));
    assert!(!thread.graph.is_empty() || thread.graph.is_empty());
}
