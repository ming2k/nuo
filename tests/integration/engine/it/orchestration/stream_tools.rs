//! Partial tool streams: execute only after the provider attempt settles.

use super::*;

use super::retry::{PartialToolRetryProvider, RetryReadTool, ToolThenRetryProvider};

#[tokio::test]
async fn partial_tool_stream_is_not_executed_before_provider_retry() {
    let directory =
        std::env::temp_dir().join(format!("nuo-retry-partial-tool-{}", uuid::Uuid::new_v4()));
    let _ = std::fs::create_dir_all(&directory);
    let session = Arc::new(ThreadSession::for_path(directory.join("session.json")));
    let tool_calls = Arc::new(AtomicUsize::new(0));
    let agent = Arc::new(Agent::new(
        Arc::new(PartialToolRetryProvider(AtomicUsize::new(0))),
        vec![Arc::new(RetryReadTool(tool_calls.clone()))],
        nuo_engine::AgentIdentity::default(),
    ));
    let (tx, mut rx) = mpsc::unbounded_channel();

    execute_round(
        RoundContext {
            agent,
            tx,
            token: CancellationToken::new(),
            session_id: session.id().await,
            session: session.clone(),
            projection: ContextProjectionSettings {
                request_overhead_tokens: 0,
                budget: nuo_protocol::CompactionPolicy::default().resolve(100_000),
                preserve_rounds: 6,
                summarize: false,
                prune: false,
                prune_protect_tokens: 0,
            },
            retry_max_attempts: 3,
            stream_restart_max_attempts: 2,

            retry_base_ms: 1,
            retry_max_ms: 10,
            emit_round_completed: false,
        },
        RoundInput {
            prompt: "work".to_string(),
            hidden: false,
            display_prompt: None,
            sent_at_ms: None,
            images: Vec::new(),
            driver: nuo_engine::orchestration::RoundDriver::Fresh,
        },
    )
    .await
    .unwrap();
    assert_eq!(tool_calls.load(Ordering::SeqCst), 0);
    assert!(
        session
            .model_window()
            .await
            .iter()
            .any(|message| message.content == "done")
    );
    let responses = std::iter::from_fn(|| rx.try_recv().ok()).collect::<Vec<_>>();
    assert!(responses.iter().any(|response| matches!(
        response,
        AgentResponse::Round { scope, .. }
            if matches!(scope.event(), RoundEvent::RetryScheduled { attempt: 2, .. })
    )));
    assert!(!responses.iter().any(|response| matches!(
        response,
        AgentResponse::Round { scope, .. }
            if matches!(scope.event(), RoundEvent::ToolCall { .. })
    )));
    let _ = std::fs::remove_dir_all(directory);
}

#[tokio::test]
async fn turn_resumes_provider_request_after_completed_tool_activity() {
    let directory = std::env::temp_dir().join(format!("nuo-retry-tool-{}", uuid::Uuid::new_v4()));
    let _ = std::fs::create_dir_all(&directory);
    let session = Arc::new(ThreadSession::for_path(directory.join("session.json")));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let tool_calls = Arc::new(AtomicUsize::new(0));
    let agent = Arc::new(Agent::new(
        Arc::new(ToolThenRetryProvider {
            attempts: AtomicUsize::new(0),
            requests: requests.clone(),
        }),
        vec![Arc::new(RetryReadTool(tool_calls.clone()))],
        nuo_engine::AgentIdentity::default(),
    ));
    let (tx, mut rx) = mpsc::unbounded_channel();

    execute_round(
        RoundContext {
            agent,
            tx,
            token: CancellationToken::new(),
            session_id: session.id().await,
            session: session.clone(),
            projection: ContextProjectionSettings {
                request_overhead_tokens: 0,
                budget: nuo_protocol::CompactionPolicy::default().resolve(100_000),
                preserve_rounds: 6,
                summarize: false,
                prune: false,
                prune_protect_tokens: 0,
            },
            retry_max_attempts: 4,
            stream_restart_max_attempts: 3,

            retry_base_ms: 1,
            retry_max_ms: 10,
            emit_round_completed: false,
        },
        RoundInput {
            prompt: "work".to_string(),
            hidden: false,
            display_prompt: None,
            sent_at_ms: None,
            images: Vec::new(),
            driver: nuo_engine::orchestration::RoundDriver::Fresh,
        },
    )
    .await
    .unwrap();
    assert_eq!(tool_calls.load(Ordering::SeqCst), 1);
    assert!(
        session
            .model_window()
            .await
            .iter()
            .any(|message| message.content == "done")
    );
    let requests = requests.lock().expect("request log lock poisoned");
    assert_eq!(requests.len(), 4);
    assert_eq!(
        requests[1], requests[2],
        "the retry must resend the exact request checkpoint after the tool result"
    );
    assert!(
        std::iter::from_fn(|| rx.try_recv().ok()).any(|response| matches!(
            response,
            AgentResponse::Round { scope, .. }
                if matches!(scope.event(), RoundEvent::RetryScheduled { attempt: 2, .. })
        ))
    );
    let _ = std::fs::remove_dir_all(directory);
}
