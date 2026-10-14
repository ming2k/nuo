//! Public API integration tests for StreamLoopDetector: continuity-gated
//! verdicts, exact trailing-run reporting, and suffix trimming.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use nuo_harness::stream_loop_detector::{DegeneratePattern, StreamLoopDetector};

/// The canonical false-positive family: bounded decorative runs must stay
/// completely silent (no candidate, no arbitration pause).
#[test]
fn decorated_table_border_never_escalates() {
    let mut detector = StreamLoopDetector::new(512);
    let line = format!("┌─ Context Usage ─{}┐\n", "─".repeat(60));
    assert!(detector.push_and_check(&line).is_none());
}

#[test]
fn separator_rule_lines_never_escalate() {
    let mut detector = StreamLoopDetector::new(512);
    let rule = "────────────\n";
    for _ in 0..30 {
        assert!(detector.push_and_check(rule).is_none());
    }
}

#[test]
fn below_dwell_threshold_repeated_pushes_stay_silent() {
    let mut detector = StreamLoopDetector::new(1024);
    for _ in 0..4 {
        assert!(detector.push_and_check(&"~".repeat(256)).is_none());
    }
}

#[test]
fn continuous_single_char_flood_escalates_at_threshold() {
    let mut detector = StreamLoopDetector::new(1024);
    let chunk = "~".repeat(256);
    let mut last = None;
    for _ in 0..14 {
        last = detector.push_and_check(&chunk);
    }
    assert!(matches!(
        last,
        Some(DegeneratePattern::Periodic { period: 1, .. })
    ));
}

#[test]
fn burst_plus_prose_is_not_cumulative() {
    let mut detector = StreamLoopDetector::new(2048);
    for _ in 0..6 {
        detector.push_and_check(&"ab".repeat(400));
        detector.push_and_check("Ordinary prose resumes the narrative here. ");
    }
    let r = detector.push_and_check(&"ab".repeat(300));
    assert!(r.is_none());
}

#[test]
fn periodic_verdict_carries_exact_tail_geometry() {
    // Whole-window periodicity: 20 chars x 40 copies.
    let text = "StepMarker!".repeat(40);
    let obs = StreamLoopDetector::observe_periodic_tail(&text).unwrap();
    assert_eq!(obs.period, "StepMarker!".len());
    assert_eq!(obs.suffix_len, text.len());
}

#[test]
fn kmp_path_handles_long_units_wholly_periodic() {
    let unit = "The quick brown fox jumps over the lazy dog! ";
    let text = unit.repeat(24);
    let obs = StreamLoopDetector::observe_periodic_tail(&text).unwrap();
    assert_eq!(obs.period, unit.chars().count());
}

#[test]
fn mixed_document_with_periodic_tail_reports_tail_extent() {
    let head = "The configuration file accepts nested tables and inline comments freely. ";
    let unit = "==--==";
    let text = format!("{head}{}", unit.repeat(8));
    let obs = StreamLoopDetector::observe_periodic_tail(&text).unwrap();
    assert_eq!(obs.period, unit.len());
    assert!(obs.suffix_len >= 2 * unit.len() && obs.suffix_len < text.len());
}

#[test]
fn trim_keeps_one_evidence_copy_of_unit() {
    let original = "Head\nABABABAB";
    let trimmed = StreamLoopDetector::trim_suffix(
        original,
        &DegeneratePattern::Periodic {
            period: 2,
            repetitions: 4,
            pattern: "AB".to_string(),
            suffix_len: 8,
        },
    );
    assert!(trimmed.starts_with("Head\nAB\n"));
    assert!(trimmed.contains("stream truncated"));
}

#[test]
fn monotonic_progression_still_detected() {
    let mut detector = StreamLoopDetector::new(512);
    let input = "Step 1: alpha\nStep 2: alpha\nStep 3: alpha\nStep 4: alpha\nStep 5: alpha\n";
    assert!(matches!(
        detector.push_and_check(input),
        Some(DegeneratePattern::MonotonicSequence { count: 5, .. })
    ));
}

#[test]
fn digit_flood_below_budget_silent_above_budget_reported() {
    // ≥0.88 density: digits and decimal points only — zero separators.
    let digits = "1729.3186.1408".repeat(2);
    let mut quiet = StreamLoopDetector::new(1024);
    assert!(quiet.push_and_check(&digits).is_none());

    // ~36 chars/push x 400 pushes ≈ 14K chars, clearing the 8192 budget.
    let mut loud = StreamLoopDetector::new(1024);
    let mut hit = None;
    for _ in 0..400 {
        let res = loud.push_and_check(&digits.repeat(3));
        if let Some(pat) = res {
            hit = Some(pat);
            break;
        }
    }
    assert!(matches!(
        hit,
        Some(DegeneratePattern::UnboundedDigitStream { .. })
    ));
}

#[test]
fn detects_arbitrary_periodic_repetitions() {
    // Continuity-gated: a single nominal burst is reported by the mechanical
    // layer only as *tail geometry*; escalation requires sustained dwell.
    let text = "abc".repeat(60);
    let obs = StreamLoopDetector::observe_periodic_tail(&text).expect("periodic tail");
    assert_eq!(obs.period, 3);
    assert_eq!(obs.suffix_len, text.len());

    // Multi-word periodic repetition, same exact-geometry contract.
    let text2 = "hello world\n".repeat(12);
    let obs2 = StreamLoopDetector::observe_periodic_tail(&text2).expect("periodic tail");
    assert_eq!(obs2.period, "hello world\n".len());
}

#[test]
fn detects_monotonic_step_progressions() {
    let mut detector = StreamLoopDetector::new(512);
    let step_sequence = "Step 1: check files\nStep 2: check files\nStep 3: check files\nStep 4: check files\nStep 5: check files\n";
    let pat = detector.push_and_check(step_sequence);
    assert!(matches!(
        pat,
        Some(DegeneratePattern::MonotonicSequence { count: 5, .. })
    ));
}

#[test]
fn detects_unbounded_pi_and_digit_floods() {
    // A single window of pure digits is classified mechanically; the public
    // push path additionally gates it behind the cumulative budget.
    let pi_stream = "3.14159265358979323846264338327950288419716939937510582097494459230781640628620899862803482534211706798214808651328230664709384460955058223172535940812848111745028410270193852110555964462294895493038196";
    let classified = StreamLoopDetector::classify_digit_density(pi_stream);
    assert!(classified.is_some());

    let mut detector = StreamLoopDetector::new(1024);
    let mut hit = None;
    for _ in 0..400 {
        if let Some(pat) =
            detector.push_and_check("3.1415926535897932384626433832795028841971693993751058209749 ")
        {
            hit = Some(pat);
            break;
        }
    }
    assert!(matches!(
        hit,
        Some(DegeneratePattern::UnboundedDigitStream { .. })
    ));
}

#[test]
fn does_not_falsely_trigger_on_code_and_text() {
    let mut detector = StreamLoopDetector::new(512);
    let code = r#"
        pub fn process_items(items: &[String]) -> Vec<String> {
            let mut result = Vec::new();
            for item in items {
                if !item.is_empty() {
                    result.push(item.to_uppercase());
                }
            }
            result
        }
    "#;
    assert!(detector.push_and_check(code).is_none());
}

#[test]
fn repeated_long_expression_is_not_a_loop() {
    let mut detector = StreamLoopDetector::new(1024);
    let analysis = " Graph length u64 = 0x09bab64a at ~0x0ee84663? Then graph would span from 0x0ee84673 - ... hmm: if graph_len counts bytes of graph ending just before\n\
        its own length field: graph_start = 0x0ee84663 - 0x09bab64a = 0x0ee84663 - 0x09bab64a";

    assert!(detector.push_and_check(analysis).is_none());
}

#[test]
fn trim_suffix_preserves_single_copy_and_truncation_notice() {
    let pattern = DegeneratePattern::Periodic {
        period: 3,
        repetitions: 5,
        pattern: "abc".to_string(),
        suffix_len: 15,
    };
    let trimmed = StreamLoopDetector::trim_suffix("Content start: abcabcabcabcabc", &pattern);
    assert!(trimmed.starts_with("Content start: abc"));
    assert!(trimmed.contains("[... stream truncated"));
}
