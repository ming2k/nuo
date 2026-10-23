#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use nuo_inference_wire::{
    CredentialsProvider, DynamicCredentials, Endpoint, StaticApiKey, StreamLoopDetector,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

#[tokio::test]
async fn test_static_credentials_provider() {
    let provider = StaticApiKey::new("test-secret-key-123");
    let token = provider.get_token().await.expect("token resolution");
    assert_eq!(token, "test-secret-key-123");

    let ep = Endpoint::openai("", "gpt-4o").with_credentials_provider(Arc::new(provider));
    assert_eq!(ep.resolve_api_key().await.unwrap(), "test-secret-key-123");
}

#[tokio::test]
async fn test_dynamic_credentials_provider_rotation() {
    let call_count = Arc::new(AtomicUsize::new(0));
    let counter_clone = call_count.clone();

    let dynamic = DynamicCredentials::new(move || {
        let count = counter_clone.fetch_add(1, Ordering::SeqCst);
        async move { Ok(format!("token-generation-{count}")) }
    });

    let ep =
        Endpoint::anthropic("", "claude-3-7-sonnet").with_credentials_provider(Arc::new(dynamic));

    assert_eq!(ep.resolve_api_key().await.unwrap(), "token-generation-0");
    assert_eq!(ep.resolve_api_key().await.unwrap(), "token-generation-1");
    assert_eq!(ep.resolve_api_key().await.unwrap(), "token-generation-2");
}

#[test]
fn test_wire_loop_detector_intercepts_runaway() {
    let mut detector = StreamLoopDetector::new(512).with_dwell_threshold(300);
    let mut detected = false;
    for _ in 0..50 {
        if let Some(pat) = detector.push_and_check("repeat_cycle_") {
            assert!(pat.description().contains("periodic loop"));
            detected = true;
            break;
        }
    }
    assert!(detected, "Stream loop detector must trip on runaway output");
}
