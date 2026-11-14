//! Session driver dispatch and recovery tests.

use super::super::recovery::{
    needs_activity_reconcile, recover_crashed_round, round_owned_request,
};
use super::*;

use nuo_engine::RoundLifecycle;
use nuo_protocol::{RequestUsageStatus};
use nuo_tool::{Message, Role};

use std::sync::Arc;

fn image() -> nuo_tool::ImagePart {
    nuo_tool::ImagePart {
        mime: "image/png".to_string(),
        data: "AAAA".to_string(),
    }
}

#[test]
fn round_owned_requests_close_their_own_activity_lifecycle() {
    assert!(round_owned_request(&AgentRequest::Prompt {
        text: "hi".to_string(),
        images: vec![image()],
        sent_at_ms: Some(1),
    }));
    assert!(round_owned_request(&AgentRequest::FollowUp {
        thread_id: "s".to_string(),
        message: nuo_protocol::QueuedMessage {
            id: "i".to_string(),
            text: "hi".to_string(),
            display_text: None,
            images: Vec::new(),
            sent_at_ms: None,
        },
    }));
    assert!(round_owned_request(&AgentRequest::Steer {
        thread_id: "s".to_string(),
        message: nuo_protocol::QueuedMessage {
            id: "i".to_string(),
            text: "hi".to_string(),
            display_text: None,
            images: Vec::new(),
            sent_at_ms: None,
        },
    }));
    assert!(round_owned_request(&AgentRequest::CancelSteer {
        thread_id: "s".to_string(),
        input_id: "i".to_string(),
    }));
}

#[test]
fn control_plane_requests_need_the_driver_reconcile() {
    // The TUI optimistically paints "queued" for these; none of them emit
    // a terminal lifecycle event of their own, so the driver must.
    assert!(!round_owned_request(&AgentRequest::SlashCommand(
        "/delegate on".to_string()
    )));
    assert!(!round_owned_request(&AgentRequest::TrustWorkspace {
        domains: Vec::new(),
        decision: nuo_protocol::TrustDecision::Grant,
    }));
    assert!(!round_owned_request(&AgentRequest::Interrupt));
    assert!(!round_owned_request(&AgentRequest::SwitchConnection {
        provider: "openai".to_string(),
        model: "gpt".to_string(),
        api_key: None,
        base_url: None,
    }));
    assert!(!round_owned_request(&AgentRequest::ToggleTool {
        name: "execute_command".to_string(),
        enabled: false,
    }));
    assert!(!round_owned_request(&AgentRequest::ToggleMcpServer {
        name: "github".to_string(),
        enabled: true,
    }));
    assert!(!round_owned_request(&AgentRequest::RefreshProviderModels));
    assert!(!round_owned_request(&AgentRequest::QueryThreadContext));
    assert!(!round_owned_request(&AgentRequest::PermissionReply {
        request_id: "r".to_string(),
        decision: nuo_protocol::PermissionDecision::Always,
        parent_call_id: None,
    }));
    assert!(!round_owned_request(&AgentRequest::UserQuestionReply {
        request_id: "r".to_string(),
        answers: Vec::new(),
        parent_call_id: None,
    }));
    assert!(!round_owned_request(&AgentRequest::StdinReply {
        request_id: "r".to_string(),
        text: "y".to_string(),
        parent_call_id: None,
    }));
    assert!(!round_owned_request(&AgentRequest::InterruptSubagent {
        call_id: "call_1".to_string()
    }));
    assert!(!round_owned_request(&AgentRequest::UpdateTuiLayout(
        "default".to_string()
    )));
    assert!(!round_owned_request(&AgentRequest::DeleteThread {
        id: "s".to_string(),
    }));
    assert!(!round_owned_request(&AgentRequest::RenameThread {
        id: "s".to_string(),
        title: Some("t".to_string()),
    }));
    assert!(!round_owned_request(&AgentRequest::QueryThreadDetail {
        id: "s".to_string(),
    }));
}

#[tokio::test]
async fn activity_reconcile_fires_only_for_control_plane_requests_with_no_live_round() {
    let lifecycle = Arc::new(RoundLifecycle::new());
    let unattended = AgentRequest::SlashCommand("/unattended on".to_string());

    // Idle harness + control-plane request → the driver must reconcile.
    assert!(
        needs_activity_reconcile(&unattended, &lifecycle).await,
        "idle + slash command needs the reconcile"
    );

    // Round-owned requests never need the reconcile — the round task emits
    // its own terminal idle snapshot.
    assert!(
        !needs_activity_reconcile(
            &AgentRequest::Prompt {
                text: "hi".to_string(),
                images: Vec::new(),
                sent_at_ms: None,
            },
            &lifecycle,
        )
        .await,
        "prompt closes its own lifecycle"
    );

    // A live round owns the display: even a control-plane request is left
    // alone so the round's timer/turn counters are not reset.
    let begin = lifecycle.begin().await;
    assert!(
        !needs_activity_reconcile(&unattended, &lifecycle).await,
        "live round suppresses the reconcile"
    );
    assert!(lifecycle.finish(begin.generation).await);

    // Back to idle → the reconcile is armed again.
    assert!(
        needs_activity_reconcile(&unattended, &lifecycle).await,
        "idle again → reconcile re-arms"
    );
}

fn usage_record(
    session_id: &str,
    actor: &str,
    round: u64,
    turn: u32,
    status: nuo_protocol::RequestUsageStatus,
) -> nuo_protocol::RequestUsageRecord {
    nuo_protocol::RequestUsageRecord {
        key: nuo_protocol::RequestUsageKey {
            session_id: session_id.to_string(),
            actor_id: actor.to_string(),
            round,
            turn,
            attempt: 1,
        },
        provider: "relay".to_string(),
        model: "m".to_string(),
        status,
        ..Default::default()
    }
}

fn residue_store(directory: &std::path::Path) -> Arc<ThreadSession> {
    std::fs::create_dir_all(directory).expect("create test directory");
    Arc::new(ThreadSession::for_path(directory.join("session.json")))
}

#[tokio::test]
async fn crash_residue_arms_retry_point_for_the_in_flight_round() {
    // The scenario from the wild: the process died mid-round, so no
    // graceful path armed `/retry` and the resumed session answered
    // "Nothing to retry". Recovery must arm it from the durable
    // `InFlight` usage record.
    let directory = std::env::temp_dir().join(format!("nuo-crash-retry-{}", uuid::Uuid::new_v4()));
    let store = residue_store(&directory);
    let session_id = store.id().await;
    // Round 2 in flight (turn 2 = the second ReAct turn, the one that
    // died), one committed assistant turn from turn 1, a settled round 1.
    // The transcript carries no round delimiters: the crashed round's
    // opener is the *last* visible user message, so round 1's assistant
    // reply must not be counted into round 2.
    store
        .set_request_usage_records(vec![
            usage_record(&session_id, "root", 1, 1, RequestUsageStatus::Completed),
            usage_record(&session_id, "root", 2, 1, RequestUsageStatus::Completed),
            usage_record(&session_id, "root", 2, 2, RequestUsageStatus::InFlight),
            usage_record(
                &session_id,
                "subagent:c1",
                2,
                5,
                RequestUsageStatus::InFlight,
            ),
        ])
        .await
        .unwrap();
    store
        .replace_messages(vec![
            Message::new(Role::User, "round 1 prompt"),
            Message::new(Role::Assistant, "round 1 answer"),
            Message::new(Role::User, "round 2 prompt"),
            Message::new(Role::Assistant, "round 2 turn 1"),
            Message::new(Role::Tool, "ok"),
        ])
        .await
        .unwrap();
    store.set_round_counter(2).await.unwrap();

    let residue = recover_crashed_round(
        &store,
        store.request_usage_records().await,
        1_700_000_000_000,
    )
    .await;

    // Interrupt for the dangling round, so the transcript explains it.
    assert_eq!(residue.interrupts.len(), 1);
    assert_eq!(residue.interrupts[0].round, Some(2));
    assert_eq!(
        residue.interrupts[0].reason,
        nuo_protocol::RoundInterruptReason::Terminated
    );
    // The point names the highest in-flight round (not the subagent's key),
    // counts only committed root turns, and watermarks the durable
    // window.
    let point = residue
        .retry_point
        .expect("crash residue arms a retry point");
    assert_eq!(point.round, 2);
    assert_eq!(point.turns_committed, 1, "one committed ReAct turn");
    assert_eq!(point.history_watermark, 5);
    assert_eq!(point.at_ms, 1_700_000_000_000);

    let _ = std::fs::remove_dir_all(directory);
}

#[tokio::test]
async fn crash_residue_ignores_settled_sessions_and_lower_in_flight_rounds() {
    let directory =
        std::env::temp_dir().join(format!("nuo-crash-settled-{}", uuid::Uuid::new_v4()));
    let store = residue_store(&directory);
    // Nothing in flight → no residue at all.
    let empty = recover_crashed_round(&store, Vec::new(), 1).await;
    assert!(empty.interrupts.is_empty() && empty.retry_point.is_none());

    let session_id = store.id().await;
    store
        .set_request_usage_records(vec![
            usage_record(&session_id, "root", 1, 1, RequestUsageStatus::Completed),
            // Round 2 still in flight — but round 3 has since completed,
            // so 2 is history: the counter guard must retire it.
            usage_record(&session_id, "root", 2, 1, RequestUsageStatus::InFlight),
            usage_record(&session_id, "root", 3, 1, RequestUsageStatus::Completed),
        ])
        .await
        .unwrap();
    store.set_round_counter(3).await.unwrap();
    let superseded = recover_crashed_round(&store, store.request_usage_records().await, 1).await;
    assert!(superseded.retry_point.is_none(), "superseded round retired");
    // The dangling-round interrupt is still recorded for the transcript.
    assert_eq!(superseded.interrupts.len(), 1);
    assert_eq!(superseded.interrupts[0].round, Some(2));

    let _ = std::fs::remove_dir_all(directory);
}

#[tokio::test]
async fn crash_residue_arms_even_over_an_existing_terminated_interrupt() {
    // Two scenarios land here. (a) A graceful server kill: the registry
    // records `Terminated` but the round is exactly as resumable as a
    // crash's — suppressing the point would resurrect the bug for every
    // `nuo stop`. (b) A crash *during* a `/retry` resume: the resumed
    // round keeps its number, so the earlier run's `Terminated` record is
    // already present when the second crash is recovered. The interrupt
    // explains the transcript; the point offers recovery — independent.
    let directory =
        std::env::temp_dir().join(format!("nuo-crash-graceful-{}", uuid::Uuid::new_v4()));
    let store = residue_store(&directory);
    let session_id = store.id().await;
    store
        .set_request_usage_records(vec![usage_record(
            &session_id,
            "root",
            1,
            1,
            RequestUsageStatus::InFlight,
        )])
        .await
        .unwrap();
    store
        .record_round_interrupt(nuo_protocol::RoundInterrupt {
            reason: nuo_protocol::RoundInterruptReason::Terminated,
            at_ms: 1,
            round: Some(1),
            detail: None,
        })
        .await
        .unwrap();
    store.set_round_counter(1).await.unwrap();
    let residue = recover_crashed_round(&store, store.request_usage_records().await, 2).await;
    // The record already exists — `record_round_interrupt` dedupes on
    // (reason, round) — but the resume point is armed regardless.
    assert_eq!(residue.interrupts.len(), 1);
    assert!(
        residue.retry_point.is_some(),
        "an existing interrupt must not suppress the resume point"
    );

    let _ = std::fs::remove_dir_all(directory);
}

#[tokio::test]
async fn crash_residue_counts_turns_from_the_rounds_opening_user_message() {
    // A crash on the round's very first request: the user message is
    // durable, nothing streamed through yet → zero committed turns. A
    // prior round's assistant reply must not be counted into it.
    let directory = std::env::temp_dir().join(format!("nuo-crash-first-{}", uuid::Uuid::new_v4()));
    let store = residue_store(&directory);
    let session_id = store.id().await;
    store
        .set_request_usage_records(vec![usage_record(
            &session_id,
            "root",
            2,
            1,
            RequestUsageStatus::InFlight,
        )])
        .await
        .unwrap();
    store
        .replace_messages(vec![
            Message::new(Role::User, "round 1"),
            Message::new(Role::Assistant, "answer 1"),
            Message::new(Role::User, "round 2"),
        ])
        .await
        .unwrap();
    store.set_round_counter(2).await.unwrap();
    let residue = recover_crashed_round(&store, store.request_usage_records().await, 1).await;
    let point = residue
        .retry_point
        .expect("first-request crash arms a point");
    assert_eq!(point.turns_committed, 0, "nothing streamed through");
    assert_eq!(point.history_watermark, 3);

    let _ = std::fs::remove_dir_all(directory);
}
