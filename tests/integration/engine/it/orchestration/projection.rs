//! Request-projection archiving outside the model window.

use super::*;

use super::interrupts::InstantProvider;
#[tokio::test]
async fn execute_round_archives_a_request_projection_outside_the_window() {
    let directory = std::env::temp_dir().join(format!("nuo-projection-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&directory).expect("create test directory");
    let session = Arc::new(ThreadSession::for_path(directory.join("session.json")));
    let agent = Arc::new(Agent::new(
        Arc::new(InstantProvider),
        Vec::new(),
        nuo_harness::AgentIdentity::default(),
    ));
    let (tx, _rx) = mpsc::unbounded_channel();

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
            driver: nuo_harness::orchestration::RoundDriver::Fresh,
        },
    )
    .await
    .unwrap();

    // The archive insert is fire-and-forget; poll briefly for the writer.
    let mut projections = Vec::new();
    for _ in 0..200 {
        projections = session.request_projections().await;
        if !projections.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert_eq!(
        projections.len(),
        1,
        "one logical invocation must archive exactly one projection"
    );
    assert!(!projections[0].prefix_fingerprint.is_empty());
    assert!(projections[0].conversation_messages >= 1);

    // The archive is not history.
    let window = session.model_window().await;
    assert!(
        window
            .iter()
            .all(|m| !m.content.contains("temporary-context"))
    );
    let _ = std::fs::remove_dir_all(directory);
}
