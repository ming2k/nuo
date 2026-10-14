//! In-flight streaming loop detector: continuity-verified degenerative-output
//! circuit breaker for LLM token generation.
//!
//! # Judging philosophy
//!
//! Evaluates **continuity and volume**, not instantaneous state:
//!
//! 1. *Dwell*: a periodic tail counts only while the stream keeps extending it.
//!    Suspicion accumulates in `DwellTrail` push by push and discharges to zero
//!    the moment the tail leaves the cycle. Escalation requires roughly
//!    `MIN_DWELL_CHARS` of uninterrupted repetition.
//! 2. *Budget*: character-class density (digit/data floods) spends toward
//!    `MAX_DEGENERATE_BUDGET_CHARS` before becoming actionable, with exponential
//!    decay when density lapses.
//! 3. *Monotonic sequences*: ascending streaks across consecutive lines
//!    (e.g. `Step 1... Step 2...`).

/// Chars of continuous periodic repetition required before a mechanical candidate escalates.
pub const MIN_DWELL_CHARS: usize = 3_000;

/// Cumulative digit-dense chars (within decay accounting) before a data flood halts the stream.
pub const MAX_DEGENERATE_BUDGET_CHARS: usize = 8_192;

/// Upper unit length for the tail-run block scan.
const MAX_TAIL_SCAN_UNIT: usize = 64;

/// Digit-density threshold classifying a window as raw-data flood.
const DIGIT_DENSITY_RATIO: f32 = 0.88;

/// Classification of detected degenerative output patterns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DegeneratePattern {
    /// Arbitrary periodic repetition ending at the current tail.
    Periodic {
        period: usize,
        repetitions: usize,
        pattern: String,
        suffix_len: usize,
    },
    /// Monotonic sequence repetition (e.g. `Step 1, Step 2, Step 3 …`).
    MonotonicSequence { template: String, count: usize },
    /// Unbounded digit or raw data generation.
    UnboundedDigitStream { length: usize },
}

impl DegeneratePattern {
    /// Human-readable summary of the detected pattern.
    pub fn description(&self) -> String {
        match self {
            Self::Periodic {
                period,
                repetitions,
                pattern,
                suffix_len,
            } => {
                let preview: String = {
                    let mut s: String = pattern.chars().take(20).collect();
                    if pattern.chars().count() > 20 {
                        s.push_str("...");
                    }
                    s
                };
                format!(
                    "periodic loop (period={period}, repetitions={repetitions}, suffix={suffix_len} chars, pattern='{preview}')"
                )
            }
            Self::MonotonicSequence { template, count } => {
                format!("monotonic sequence '{template}' repeated {count} times")
            }
            Self::UnboundedDigitStream { length } => {
                format!("unbounded digit/data stream ({length} chars)")
            }
        }
    }

    /// Extent of the degenerate tail this verdict accounts for, in chars.
    pub fn tail_chars(&self) -> usize {
        match self {
            Self::Periodic { suffix_len, .. } => *suffix_len,
            Self::MonotonicSequence { count, .. } => *count * 24,
            Self::UnboundedDigitStream { length } => *length,
        }
    }
}

/// One candidate observation for the dwell trail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrailObservation {
    pub period: usize,
    pub unit: Vec<u8>,
    pub suffix_len: usize,
}

/// Continuity ledger for periodic candidates across chunk pushes.
#[derive(Debug, Default)]
struct DwellTrail {
    active: Option<TrailObservation>,
    depth: usize,
}

impl DwellTrail {
    fn observe(
        &mut self,
        observation: Option<TrailObservation>,
        pushed_chars: usize,
        dwell_threshold: usize,
    ) -> bool {
        let Some(next) = observation else {
            self.active = None;
            self.depth = 0;
            return false;
        };

        match &self.active {
            Some(prev) if prev.period == next.period => {
                self.depth = self.depth.saturating_add(pushed_chars);
                self.depth = self.depth.min(dwell_threshold);
            }
            _ => {
                self.depth = next.suffix_len.min(dwell_threshold);
            }
        }
        self.active = Some(next);
        self.depth >= dwell_threshold
    }
}

/// In-flight detector monitoring token streams for non-converging degeneration patterns.
pub struct StreamLoopDetector {
    buffer: String,
    window_size: usize,
    monotonic_streak: (Option<String>, usize),
    digit_budget_spent: usize,
    trail: DwellTrail,
    max_degenerate_budget_chars: usize,
    dwell_threshold: usize,
    window_chars: usize,
    window_digitish: usize,
    scratch_chars: Vec<char>,
    scratch_skeletons: Vec<String>,
}

fn is_digitish(c: char) -> bool {
    c.is_ascii_digit() || c == '.' || c == ','
}

impl Default for StreamLoopDetector {
    fn default() -> Self {
        Self::new(1024)
    }
}

impl StreamLoopDetector {
    pub fn new(window_size: usize) -> Self {
        Self {
            buffer: String::new(),
            window_size,
            monotonic_streak: (None, 0),
            digit_budget_spent: 0,
            trail: DwellTrail::default(),
            max_degenerate_budget_chars: MAX_DEGENERATE_BUDGET_CHARS,
            dwell_threshold: MIN_DWELL_CHARS,
            window_chars: 0,
            window_digitish: 0,
            scratch_chars: Vec::new(),
            scratch_skeletons: Vec::new(),
        }
    }

    /// Sets a custom dwell threshold for detection escalation (useful in tests or tight budgets).
    pub fn with_dwell_threshold(mut self, threshold: usize) -> Self {
        self.dwell_threshold = threshold;
        self
    }

    /// Sets custom budget for digit-dense floods.
    pub fn with_degenerate_budget(mut self, budget: usize) -> Self {
        self.max_degenerate_budget_chars = budget;
        self
    }

    /// Feed a stream chunk; returns a mechanical verdict only when continuity plus volume clear threshold.
    pub fn push_and_check(&mut self, chunk: &str) -> Option<DegeneratePattern> {
        if chunk.is_empty() {
            return None;
        }
        let pushed_chars = chunk.chars().count();

        self.buffer.push_str(chunk);
        for c in chunk.chars() {
            self.window_chars += 1;
            if is_digitish(c) {
                self.window_digitish += 1;
            }
        }
        if self.buffer.len() > self.window_size {
            let excess = self.buffer.len() - self.window_size;
            let mut cut = excess;
            while !self.buffer.is_char_boundary(cut) && cut < self.buffer.len() {
                cut += 1;
            }
            for c in self.buffer[..cut].chars() {
                self.window_chars -= 1;
                if is_digitish(c) {
                    self.window_digitish -= 1;
                }
            }
            self.buffer.drain(..cut);
        }

        // 1. Data flood budget
        if self.window_chars >= 64
            && (self.window_digitish as f32 / self.window_chars as f32) > DIGIT_DENSITY_RATIO
        {
            self.digit_budget_spent = self.digit_budget_spent.saturating_add(pushed_chars);
            if self.digit_budget_spent >= self.max_degenerate_budget_chars {
                self.digit_budget_spent = 0;
                return Some(DegeneratePattern::UnboundedDigitStream {
                    length: self.window_chars,
                });
            }
            return None;
        }
        self.digit_budget_spent /= 2;

        // 2. Periodic tail, gated by continuous dwell
        self.scratch_chars.clear();
        self.scratch_chars.extend(self.buffer.chars());
        if let Some(observation) = Self::observe_periodic_tail_chars(&self.scratch_chars) {
            if self
                .trail
                .observe(Some(observation), pushed_chars, self.dwell_threshold)
                && let Some(obs) = self.trail.active.clone()
            {
                return Some(DegeneratePattern::Periodic {
                    period: obs.period,
                    repetitions: obs.suffix_len / obs.period,
                    pattern: String::from_utf8_lossy(&obs.unit).into_owned(),
                    suffix_len: obs.suffix_len,
                });
            }
        } else {
            self.trail.observe(None, pushed_chars, self.dwell_threshold);
        }

        // 3. Monotonic line progression
        if let Some(pat) = self.advance_monotonic_streak() {
            return Some(pat);
        }

        None
    }

    /// Reset internal suspicion state.
    pub fn reset(&mut self) {
        self.buffer.clear();
        self.monotonic_streak = (None, 0);
        self.digit_budget_spent = 0;
        self.trail = DwellTrail::default();
        self.window_chars = 0;
        self.window_digitish = 0;
    }

    /// Inspect the buffer's tail for a periodic run.
    pub fn observe_periodic_tail(buffer: &str) -> Option<TrailObservation> {
        let chars: Vec<char> = buffer.chars().collect();
        Self::observe_periodic_tail_chars(&chars)
    }

    fn observe_periodic_tail_chars(chars: &[char]) -> Option<TrailObservation> {
        let n = chars.len();
        if n < 18 {
            return None;
        }

        // Path 1 — whole-window periodicity via KMP prefix function
        let mut pi = vec![0usize; n];
        for i in 1..n {
            let mut j = pi[i - 1];
            while j > 0 && chars[i] != chars[j] {
                j = pi[j - 1];
            }
            if chars[i] == chars[j] {
                j += 1;
            }
            pi[i] = j;
        }
        let kmp_period = n - pi[n - 1];
        if kmp_period < n
            && n / kmp_period >= 4
            && (0..n - kmp_period).all(|i| chars[i] == chars[i + kmp_period])
        {
            let unit: String = chars[n - kmp_period..].iter().collect();
            return Some(TrailObservation {
                period: kmp_period,
                unit: unit.into_bytes(),
                suffix_len: n,
            });
        }

        // Path 2 — longest >= 2-copy run at tail for small units
        let mut best: Option<(usize, usize)> = None;
        for p in 1..=MAX_TAIL_SCAN_UNIT.min(n / 2) {
            let last_mismatch = (0..n - p).rev().find(|&i| chars[i] != chars[i + p]);
            let run_start = match last_mismatch {
                Some(i) => i + 1,
                None => continue,
            };
            let run = n - run_start;
            if run >= 2 * p && best.as_ref().is_none_or(|(b, bp)| run > *b || (run == *b && p < *bp)) {
                best = Some((run, p));
            }
        }
        let (suffix_len, p) = best?;
        let unit: String = chars[suffix_len - p..suffix_len].iter().collect();
        Some(TrailObservation {
            period: p,
            unit: unit.into_bytes(),
            suffix_len,
        })
    }

    fn advance_monotonic_streak(&mut self) -> Option<DegeneratePattern> {
        const MONOTONIC_MIN_LINES: usize = 5;
        const TAIL_LINES: usize = 8;

        if self.scratch_skeletons.len() < TAIL_LINES {
            self.scratch_skeletons
                .extend(std::iter::repeat_with(String::new).take(TAIL_LINES));
        }
        for skeleton in &mut self.scratch_skeletons {
            skeleton.clear();
        }
        let mut numbers = [f64::NAN; TAIL_LINES];
        let mut used = 0usize;

        for line in self.buffer.lines().rev() {
            if used >= TAIL_LINES {
                break;
            }
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            let skeleton = &mut self.scratch_skeletons[used];
            let mut digits = String::new();
            let mut numbered = false;
            for c in line.chars() {
                if c.is_ascii_digit() {
                    digits.push(c);
                } else {
                    if !digits.is_empty() {
                        if !numbered {
                            numbers[used] = digits.parse::<f64>().unwrap_or(0.0);
                            numbered = true;
                        }
                        digits.clear();
                    }
                    let mapped = if c.is_alphanumeric() { 'x' } else { c };
                    skeleton.push(mapped);
                }
            }
            if !digits.is_empty() && !numbered {
                numbers[used] = digits.parse::<f64>().unwrap_or(0.0);
            }
            used += 1;
        }

        let streak = &mut self.monotonic_streak;
        if used == 0 {
            *streak = (None, 0);
            return None;
        }
        let first = &self.scratch_skeletons[0];
        let run = self.scratch_skeletons[..used]
            .iter()
            .take_while(|sk| *sk == first)
            .count();
        if streak.0.as_ref() != Some(first) {
            *streak = (Some(first.clone()), run);
        } else {
            streak.1 = run;
        }

        if run < MONOTONIC_MIN_LINES {
            return None;
        }

        let positions = &numbers[..run];
        let all_numbered = positions.iter().all(|n| n.is_finite());
        let ascending_step_one = positions.windows(2).all(|w| w[0] - w[1] == 1.0);
        if !(all_numbered && ascending_step_one) {
            return None;
        }

        Some(DegeneratePattern::MonotonicSequence {
            template: first.clone(),
            count: run,
        })
    }

    /// Classify digit density.
    pub fn classify_digit_density(buffer: &str) -> Option<usize> {
        let chars: Vec<char> = buffer.chars().collect();
        let total = chars.len();
        if total < 64 {
            return None;
        }
        let digits = chars
            .iter()
            .filter(|c| c.is_ascii_digit() || **c == '.' || **c == ',')
            .count();
        let ratio = digits as f32 / total as f32;
        (ratio > DIGIT_DENSITY_RATIO).then_some(total)
    }

    /// Trim the degenerative repeating suffix from the full accumulated text.
    pub fn trim_suffix(full_text: &str, pattern: &DegeneratePattern) -> String {
        const NOTE: &str = "[... stream truncated: repetitive pattern aborted ...]";
        match pattern {
            DegeneratePattern::Periodic { pattern: unit, .. } => {
                if unit.is_empty() {
                    return full_text.to_string();
                }
                let trimmed = full_text.trim_end_matches(unit.as_str());
                format!("{trimmed}{unit}\n\n{NOTE}")
            }
            DegeneratePattern::MonotonicSequence { .. }
            | DegeneratePattern::UnboundedDigitStream { .. } => {
                format!("{}\n\n{NOTE}", full_text.trim_end())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_border_run_stays_silent() {
        let mut detector = StreamLoopDetector::new(1024);
        let border = "┌─ Context Usage ─".to_string() + &"─".repeat(120) + "┐\n";
        assert!(
            detector.push_and_check(&border).is_none(),
            "a bounded box-drawing run must never escalate"
        );
    }

    #[test]
    fn repeated_short_rule_lines_stay_silent() {
        let mut detector = StreamLoopDetector::new(1024);
        let rule = "══════════\n";
        for _ in 0..40 {
            assert!(detector.push_and_check(rule).is_none());
        }
    }

    #[test]
    fn verdict_requires_uninterrupted_dwell_not_cumulative_spread() {
        let mut detector = StreamLoopDetector::new(2048);
        let unit = "-=".repeat(500);
        for _ in 0..8 {
            detector.push_and_check(&unit);
            detector.push_and_check("\nAnd now some ordinary prose continues the document.\n");
        }
        let tail: String = std::iter::repeat_n("-=", MIN_DWELL_CHARS / 2 - 200).collect();
        assert!(detector.push_and_check(&tail).is_none());
    }

    #[test]
    fn continuous_run_beyond_threshold_escalates() {
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

    #[test]
    fn monotonic_sequence_escalation() {
        let mut detector = StreamLoopDetector::new(1024);
        let lines = [
            "Step 1: check files\n",
            "Step 2: check files\n",
            "Step 3: check files\n",
            "Step 4: check files\n",
            "Step 5: check files\n",
        ];
        let mut detected = None;
        for line in lines {
            if let Some(pat) = detector.push_and_check(line) {
                detected = Some(pat);
                break;
            }
        }
        assert!(matches!(detected, Some(DegeneratePattern::MonotonicSequence { .. })));
    }
}
