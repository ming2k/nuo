//! The provider-retry state machine: one typed lifecycle, one vocabulary, one
//! owner per state.
//!
//! # Why this type exists
//!
//! A transport setback used to be represented twice, independently:
//!
//! - the activity bar formatted `retry {attempt-1}/{max-1}` from
//!   `ProviderRetryState`, and
//! - a transcript entry formatted `retry {attempt}/{max}` from
//!   `MessageKind::ProviderRetry`,
//!
//! both fed by the same `RoundEvent::RetryScheduled`. The two counters disagreed
//! on screen for every retry (`retry 5/29` beside `retry 6/30`), because the
//! vocabulary was re-derived at each paint site instead of owned once. The same
//! entry was then *rewritten in place* into a permanent transcript notice when
//! the round was interrupted, so a transport detail became a conversation
//! message.
//!
//! This module is the single fold point. Everything downstream — the bar's
//! clause, the notice-log receipt, the recovered-round anchor — reads
//! [`RetryProgress`] and never re-derives a count or a phrase from a string.
//!
//! # The lifecycle
//!
//! ```text
//!            RetryScheduled                  now >= retry_at
//!   (none) ──────────────────► BackingOff ──────────────────► Reattempting
//!                                │                                │
//!                                │ user interrupt                 │ round completes
//!                                ▼                                ▼
//!                          round-interrupt anchor          recovered anchor
//!                          (one line + pointer)            (fault list as detail)
//! ```
//!
//! Four transitions are visible to a user, and each has exactly one home:
//!
//! | transition | what the user sees |
//! |---|---|
//! | `(none) → BackingOff` | the bar grows a `retry N/M (next in Xs)` clause, and a receipt lands in the notice log |
//! | attempt N → N+1 | the clause's count increments in place — never a new transcript row |
//! | `BackingOff → Reattempting` | the clause switches to `(running for Xs)`; nothing else changes |
//! | `Reattempting → recovered` | the clause retires and a `▲ recovered` turn anchor lands in the transcript, carrying every fault as its detail |
//! | `Reattempting → exhausted/interrupted` | the clause retires and the round-interrupt anchor lands, carrying the last fault as its one-line detail |
//!
//! # Why the phase is derived, not stored
//!
//! `BackingOff` and `Reattempting` are a pure function of the wall clock against
//! the scheduled instant ([`RetryProgress::phase`]). Storing a phase field would
//! mean a second writer that has to be driven by a timer, and any tick it missed
//! would leave the bar claiming the wrong state. Deriving it means the displayed
//! state cannot drift from the scheduled one.
//!
//! The *terminal* transitions are not derived: they are events, and they are the
//! caller's job (the round completed, or it stopped). This type never decides
//! that a retry recovered — it only knows how many faults it carries.

use std::time::Instant;

/// How much of the retry budget a round has consumed, and what it is doing now.
///
/// Constructed only by [`RetryProgress::scheduled`], which clamps the attempt
/// into `1..=max_attempts`, so every instance satisfies
/// `1 <= attempt <= max_attempts` — the count on screen can never read
/// `retry 0/0` or `retry 7/5`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryProgress {
    /// The attempt this round is on, 1-based. The first request is attempt 1;
    /// a retry is attempt 2 and up.
    attempt: usize,
    /// The total attempts the round is allowed (the configured retry budget
    /// plus the first request).
    max_attempts: usize,
    /// When the backoff ends and the request is re-sent.
    retry_at: Instant,
    /// Every fault this round has hit so far, oldest first, one line each.
    /// Already summarized by the producer (`runtime::retry::public_retry_reason`)
    /// — this type never truncates or rewords a fault.
    faults: Vec<String>,
}

/// What the retry is doing right now. Derived from the clock
/// ([`RetryProgress::phase`]), never stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryPhase {
    /// Waiting out the backoff before the next attempt.
    BackingOff,
    /// The backoff has elapsed; the request is in flight again.
    Reattempting,
}

impl RetryProgress {
    /// Begin (or advance) the setback for `attempt` of `max_attempts`.
    ///
    /// `fault` is this attempt's failure, appended to the running list so the
    /// recovered anchor can report every fault the round survived, not just the
    /// last one.
    pub fn scheduled(
        attempt: usize,
        max_attempts: usize,
        retry_at: Instant,
        fault: impl Into<String>,
    ) -> Self {
        let max_attempts = max_attempts.max(1);
        let attempt = attempt.clamp(1, max_attempts);
        Self {
            attempt,
            max_attempts,
            retry_at,
            faults: vec![fault.into()],
        }
    }

    /// Advance to the next scheduled attempt, carrying the fault list forward.
    pub fn with_next_attempt(
        mut self,
        attempt: usize,
        max_attempts: usize,
        retry_at: Instant,
        fault: impl Into<String>,
    ) -> Self {
        let max_attempts = max_attempts.max(1);
        self.max_attempts = max_attempts;
        self.attempt = attempt.clamp(1, max_attempts);
        self.retry_at = retry_at;
        // Every attempt contributes exactly one fault, including a repeat of
        // the previous one. De-duplicating here would make `faults().len()`
        // disagree with `retries()` — and the recovered anchor is rebuilt on
        // resume from the persisted fault list alone (`RetryResolution`), so a
        // live count that the resume path cannot reproduce is exactly the
        // live/resume split ADR-0117 `[INV-TRANSCRIPT-04]` forbids.
        self.faults.push(fault.into());
        self
    }

    /// The attempt this round is on (1-based).
    pub fn attempt(&self) -> usize {
        self.attempt
    }

    /// The attempt budget.
    pub fn max_attempts(&self) -> usize {
        self.max_attempts
    }

    /// How many retries have been consumed — `attempt - 1`, the count a reader
    /// means by "it retried twice".
    ///
    /// Invariant: this equals [`Self::faults`]`.len()`. One attempt past the
    /// first is one retry is one fault, which is what lets the recovered anchor
    /// be rebuilt identically from either the live progress or the persisted
    /// fault list.
    pub fn retries(&self) -> usize {
        self.attempt.saturating_sub(1)
    }

    /// Every fault this round has hit, oldest first.
    pub fn faults(&self) -> &[String] {
        &self.faults
    }

    /// When the current backoff ends.
    pub fn retry_at(&self) -> Instant {
        self.retry_at
    }

    /// What the retry is doing at `now`.
    pub fn phase(&self, now: Instant) -> RetryPhase {
        if now < self.retry_at {
            RetryPhase::BackingOff
        } else {
            RetryPhase::Reattempting
        }
    }

    /// The bar's clause: `retry 6/30 (next in 8s)` while backing off,
    /// `retry 6/30 (running for 1.2s)` once the attempt is in flight.
    ///
    /// The **only** place this sentence is composed. The count is
    /// `attempt/max_attempts` — the same pair the harness prints when it
    /// announces the retry and the same pair the recovered anchor reports, so
    /// the three can never disagree.
    pub fn label(&self, now: Instant) -> String {
        let timing = match self.phase(now) {
            RetryPhase::BackingOff => format!(
                "next in {}",
                format_retry_duration(self.retry_at.saturating_duration_since(now))
            ),
            RetryPhase::Reattempting => format!(
                "running for {}",
                format_retry_duration(now.saturating_duration_since(self.retry_at))
            ),
        };
        format!("retry {}/{} ({timing})", self.attempt, self.max_attempts)
    }

    /// The width-constrained form: just the attempt counter, `(6/30)`.
    ///
    /// Used by the bar when the full clause would crowd out the status label.
    /// It is a method here rather than a substring of [`Self::label`] because
    /// parsing the phrase back out of its own rendering is how the two
    /// vocabularies drifted apart in the first place.
    pub fn compact_label(&self) -> String {
        format!("({}/{})", self.attempt, self.max_attempts)
    }
}

/// A backoff span as a user reads it: whole seconds once it is long enough to
/// read as seconds, one decimal below that so a sub-second retry is not shown as
/// `0s`.
pub fn format_retry_duration(duration: std::time::Duration) -> String {
    let millis = duration.as_millis() as u64;
    if millis >= 10_000 {
        format!("{}s", millis.div_ceil(1_000))
    } else {
        format!("{:.1}s", millis as f64 / 1_000.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn at(now: Instant, ms: u64) -> Instant {
        now + Duration::from_millis(ms)
    }

    #[test]
    fn the_count_is_the_attempt_pair_never_a_derived_retry_count() {
        // The defect: the bar said `retry 5/29` while the transcript said
        // `retry 6/30` for the same scheduled event, because one site rendered
        // `attempt-1` and the other `attempt`. One label, one pair.
        let now = Instant::now();
        let progress = RetryProgress::scheduled(6, 30, at(now, 8_000), "Google HTTP 503");
        assert_eq!(progress.label(now), "retry 6/30 (next in 8.0s)");
        assert_eq!(progress.attempt(), 6);
        assert_eq!(progress.max_attempts(), 30);
        assert_eq!(
            progress.retries(),
            5,
            "`retries()` is the derived count, and nothing paints it as the attempt"
        );
    }

    #[test]
    fn the_phase_is_a_pure_function_of_the_clock() {
        let now = Instant::now();
        let progress = RetryProgress::scheduled(2, 5, at(now, 4_000), "HTTP 529");
        assert_eq!(progress.phase(now), RetryPhase::BackingOff);
        assert_eq!(
            progress.phase(at(now, 3_999)),
            RetryPhase::BackingOff,
            "one millisecond early is still backing off"
        );
        assert_eq!(
            progress.phase(at(now, 4_000)),
            RetryPhase::Reattempting,
            "the scheduled instant itself is in flight, not still waiting"
        );
        assert_eq!(progress.phase(at(now, 9_000)), RetryPhase::Reattempting);
    }

    #[test]
    fn the_label_switches_tense_at_the_scheduled_instant() {
        let now = Instant::now();
        let progress = RetryProgress::scheduled(2, 5, at(now, 4_000), "HTTP 529");
        assert_eq!(progress.label(now), "retry 2/5 (next in 4.0s)");
        assert_eq!(
            progress.label(at(now, 5_200)),
            "retry 2/5 (running for 1.2s)",
            "a backoff that has elapsed reads as an attempt in flight"
        );
    }

    #[test]
    fn the_compact_form_is_the_same_pair_without_the_timing() {
        let now = Instant::now();
        let progress = RetryProgress::scheduled(6, 30, at(now, 8_000), "HTTP 503");
        assert_eq!(progress.compact_label(), "(6/30)");
        // Both forms read the same two fields, so the count can never disagree
        // between the wide and the narrow bar: the wide form contains the pair
        // verbatim, and the narrow form is that pair plus its parens.
        assert!(
            progress.label(now).contains("6/30"),
            "the wide form must carry the same pair: {}",
            progress.label(now)
        );
        assert_eq!(
            progress.compact_label(),
            format!("({}/{})", progress.attempt(), progress.max_attempts())
        );
    }

    #[test]
    fn every_attempt_contributes_exactly_one_fault() {
        // `retries() == faults().len()` is the invariant that makes the live
        // anchor and the resumed anchor identical: the resume path has only the
        // persisted fault list, so a live count it cannot reproduce would be a
        // split between the two.
        let now = Instant::now();
        let progress = RetryProgress::scheduled(2, 5, at(now, 1_000), "Google HTTP 503");
        assert_eq!(progress.retries(), progress.faults().len());

        let progress = progress.with_next_attempt(3, 5, at(now, 2_000), "Google HTTP 503");
        assert_eq!(
            progress.faults(),
            ["Google HTTP 503", "Google HTTP 503"],
            "a repeated fault is a second setback and is recorded as one"
        );
        assert_eq!(progress.retries(), progress.faults().len());

        let progress = progress.with_next_attempt(4, 5, at(now, 3_000), "Google HTTP 429");
        assert_eq!(
            progress.faults(),
            ["Google HTTP 503", "Google HTTP 503", "Google HTTP 429"],
            "oldest first, one per attempt"
        );
        assert_eq!(progress.retries(), progress.faults().len());
        assert_eq!(progress.attempt(), 4);
    }

    #[test]
    fn an_out_of_range_attempt_cannot_produce_an_impossible_count() {
        // A producer bug (or a replayed event) must not put `retry 0/0` or
        // `retry 7/5` on screen: the constructor is the clamp.
        let now = Instant::now();
        assert_eq!(
            RetryProgress::scheduled(0, 0, now, "f").label(now),
            "retry 1/1 (running for 0.0s)"
        );
        assert_eq!(
            RetryProgress::scheduled(7, 5, now, "f").label(now),
            "retry 5/5 (running for 0.0s)"
        );
        let widened = RetryProgress::scheduled(1, 1, now, "f").with_next_attempt(9, 3, now, "g");
        assert_eq!(widened.attempt(), 3);
        assert_eq!(widened.max_attempts(), 3);
    }

    #[test]
    fn durations_read_as_seconds_once_they_are_seconds() {
        assert_eq!(format_retry_duration(Duration::from_millis(0)), "0.0s");
        assert_eq!(format_retry_duration(Duration::from_millis(1_200)), "1.2s");
        assert_eq!(format_retry_duration(Duration::from_millis(9_999)), "10.0s");
        assert_eq!(
            format_retry_duration(Duration::from_millis(10_000)),
            "10s",
            "ten seconds and up are whole seconds"
        );
        assert_eq!(format_retry_duration(Duration::from_millis(10_001)), "11s");
    }
}
