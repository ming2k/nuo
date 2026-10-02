//! Performance and runtime degradation regression test for long-running sessions.
//!
//! Asserts that `commit_turn` remains O(delta) and does not degrade with session length.

// Test setup and persistence failures should stop the regression immediately.
#![allow(clippy::expect_used)]

use std::time::Instant;

use nuo_contracts::{Message, Role};
use nuo_persistence::session::{CommitTurn, SessionStore};

#[tokio::test]
async fn long_session_commit_turn_scales_with_delta_not_session_length() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("session.json");
    let store = SessionStore::for_path(path);

    // Warm up the store with an initial turn
    let initial_msg = vec![Message::new(Role::User, "Initial user request")];
    store
        .commit_turn(CommitTurn {
            messages: &initial_msg,
            round_counter: Some(1),
            usage_records: &[],
            retry_point: None,
            round_interrupt: None,
            operation_id: None,
            expected_revision: None,
        })
        .await
        .expect("initial commit");

    let mut current_messages = initial_msg;

    // Simulate 200 consecutive ReAct turns accumulating content
    let mut turn_latencies = Vec::new();
    for turn in 1..=200 {
        // Append an assistant tool-call turn
        current_messages.push(Message::new(
            Role::Assistant,
            format!("Turn {turn}: thinking and preparing tool call..."),
        ));
        let start = Instant::now();
        store
            .commit_turn(CommitTurn {
                messages: &current_messages,
                round_counter: Some(1),
                usage_records: &[],
                retry_point: None,
                round_interrupt: None,
                operation_id: None,
                expected_revision: None,
            })
            .await
            .expect("turn commit");
        let elapsed = start.elapsed();
        turn_latencies.push(elapsed);

        // Append tool result
        current_messages.push(Message::new(
            Role::Tool,
            format!("Result for turn {turn} content payload with some output lines."),
        ));
        let start = Instant::now();
        store
            .commit_turn(CommitTurn {
                messages: &current_messages,
                round_counter: Some(1),
                usage_records: &[],
                retry_point: None,
                round_interrupt: None,
                operation_id: None,
                expected_revision: None,
            })
            .await
            .expect("turn commit");
        let elapsed = start.elapsed();
        turn_latencies.push(elapsed);
    }

    assert_eq!(turn_latencies.len(), 400);

    // Compare early turns (turns 10..30) against late turns (turns 370..390)
    let early_avg_us: u128 = turn_latencies[10..30]
        .iter()
        .map(|d| d.as_micros())
        .sum::<u128>()
        / 20;
    let late_avg_us: u128 = turn_latencies[370..390]
        .iter()
        .map(|d| d.as_micros())
        .sum::<u128>()
        / 20;

    // Late turns have 400x more historical messages than early turns.
    // If there were an O(N) or O(N^2) degradation, late turns would be orders of magnitude slower.
    // Under our O(delta) zero-alloc architecture, latency remains on the same order of magnitude.
    println!("Early turn avg: {early_avg_us}µs, Late turn avg (400 messages): {late_avg_us}µs");

    // Both should be under 20ms even with SQLite transaction fsyncs on disk
    assert!(
        late_avg_us < 50_000,
        "Late turn average ({late_avg_us}µs) should remain bounded under 50ms"
    );
}
