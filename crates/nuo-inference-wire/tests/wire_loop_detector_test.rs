//! Coverage for the symbols that survived the codec-lane cut (ADR-0143).
//!
//! The credential-provider tests that lived here were deleted with the code they
//! covered: `StaticApiKey`, `DynamicCredentials`, `CredentialsProvider`, and
//! `Endpoint::resolve_api_key` had zero consumers in any shipped binary, so
//! keeping tests green for them would have preserved exactly the dead layer the
//! cut removed.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use nuo_inference_wire::StreamLoopDetector;

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
