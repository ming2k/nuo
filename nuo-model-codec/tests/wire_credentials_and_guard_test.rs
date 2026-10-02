#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use futures::{SinkExt, StreamExt};
use nuo_model_codec::{
    CredentialsProvider, DynamicCredentials, Endpoint, StaticApiKey, StreamLoopDetector,
    WireChunk, WireError, WireStream,
};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

#[tokio::test]
async fn test_static_credentials_provider() {
    let provider = StaticApiKey::new("test-secret-key-123");
    let token = provider.get_token().await.expect("token resolution");
    assert_eq!(token, "test-secret-key-123");

    let ep = Endpoint::openai("", "gpt-4o")
        .with_credentials_provider(Arc::new(provider));
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

    let ep = Endpoint::anthropic("", "claude-3-7-sonnet")
        .with_credentials_provider(Arc::new(dynamic));

    assert_eq!(ep.resolve_api_key().await.unwrap(), "token-generation-0");
    assert_eq!(ep.resolve_api_key().await.unwrap(), "token-generation-1");
    assert_eq!(ep.resolve_api_key().await.unwrap(), "token-generation-2");
}

#[tokio::test]
async fn test_wire_stream_loop_detector_intercepts_runaway() {
    let (mut tx, rx) = futures::channel::mpsc::channel(16);
    let stream = WireStream::new(rx);
    let detector = StreamLoopDetector::new(512).with_dwell_threshold(300);
    let mut guarded = stream.with_loop_detector(detector);

    tokio::spawn(async move {
        // Send a repeating pattern
        for _ in 0..50 {
            if tx.send(Ok(WireChunk::content("repeat_cycle_"))).await.is_err() {
                break;
            }
        }
    });

    let mut loop_detected = false;
    while let Some(chunk_res) = guarded.next().await {
        match chunk_res {
            Ok(_) => {}
            Err(WireError::DegenerativeLoop(reason)) => {
                assert!(reason.contains("periodic loop"));
                loop_detected = true;
                break;
            }
            Err(e) => panic!("Unexpected error: {e}"),
        }
    }

    assert!(loop_detected, "Stream loop detector must trip on runaway output");
}
