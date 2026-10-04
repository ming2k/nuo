//! In-flight streaming loop detector re-exported from `nous-model-wire`.

pub use nuo_model_codec::loop_detector::{
    DegeneratePattern, StreamLoopDetector, MAX_DEGENERATE_BUDGET_CHARS, MIN_DWELL_CHARS,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_loop_detector_integration() {
        let mut detector = StreamLoopDetector::new(1024).with_dwell_threshold(500);
        let chunk = "abcabc";
        let mut detected = None;
        for _ in 0..100 {
            if let Some(pat) = detector.push_and_check(chunk) {
                detected = Some(pat);
                break;
            }
        }
        assert!(matches!(detected, Some(DegeneratePattern::Periodic { .. })));
    }
}
