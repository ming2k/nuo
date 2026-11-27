//! Full turn through the real binary: spawn a server, attach headless, stream
//! one provider round, persist the thread, and stop the instance.
//!
//! The only upstream is [`MockProvider`]; no test in this tier may reach a
//! live vendor.

use std::time::Duration;

use nuo_test_support::{Cli, MockProvider, Sandbox};

#[test]
fn headless_turn_runs_against_a_hermetic_provider() -> nuo_test_support::Result<()> {
    let mock = MockProvider::with_response("pong")?;
    let sandbox = Sandbox::new()?;
    sandbox.configure_openai_mock("mock-provider", "mock", "mock-1", &mock.base_url())?;
    let bin = nuo_test_support::binary_path()?;
    let _cleanup = sandbox.server_cleanup(&bin);

    let output = Cli::new(&bin, &sandbox)
        .args(["--headless", "-p", "ping", "--json"])
        .timeout(Duration::from_secs(120))
        .output()?;
    assert!(
        output.status.success(),
        "headless run failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout)?;
    let events: Vec<serde_json::Value> = stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();

    let init = events
        .iter()
        .find(|event| event["type"] == "session_init")
        .ok_or("headless stream did not emit session_init")?;
    assert_eq!(init["provider"], "mock");
    assert_eq!(init["model"], "mock-1");
    assert!(
        events
            .iter()
            .any(|event| event["type"] == "text_delta" && event["delta"] == "pong"),
        "assistant delta was not streamed: {events:?}"
    );
    assert!(
        events
            .iter()
            .any(|event| event["type"] == "round_completed"),
        "round did not complete: {events:?}"
    );

    // The turn reached the configured endpoint with the exact model and the
    // resolved bearer credential. The title request is non-streaming; select
    // the streaming inference call.
    let requests = mock.wait_for_requests(1, Duration::from_secs(10))?;
    let main = requests
        .iter()
        .find(|request| request.body.contains("\"stream\":true"))
        .ok_or("mock provider never received the streaming inference request")?;
    assert!(main.path.ends_with("/v1/chat/completions"));
    assert!(main.body.contains("\"model\":\"mock-1\""));
    assert!(main.body.contains("ping"));
    assert_eq!(
        main.headers.get("authorization").map(String::as_str),
        Some("Bearer test-key"),
        "resolved credential did not reach the wire: {:?}",
        main.headers
    );

    // The session is durable and discoverable through the public CLI, not just
    // through the server's in-memory registry.
    let threads = Cli::new(&bin, &sandbox)
        .args(["thread", "ls", "--json"])
        .output()?;
    assert!(
        threads.status.success(),
        "thread ls failed: {}",
        String::from_utf8_lossy(&threads.stderr)
    );
    let rows: serde_json::Value = serde_json::from_slice(&threads.stdout)?;
    assert!(
        rows.as_array().is_some_and(|rows| !rows.is_empty()),
        "headless turn did not persist a discoverable thread: {rows:?}"
    );

    Ok(())
}
