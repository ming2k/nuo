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
//!                          round-interrupt anchor            (nothing: the
//!                          (one line + pointer)           receipt is the record)
//! ```
//!
//! Three transitions are visible to a user, and each has exactly one home:
//!
//! | transition | what the user sees |
//! |---|---|
//! | `(none) → BackingOff` | the bar grows a `retry 1 in 4s` clause, and a `retry 1 of 4` receipt lands in the notice log |
//! | retry N → N+1 | the clause's ordinal increments in place — never a new transcript row |
//! | `BackingOff → Reattempting` | the clause drops the countdown and reads `retry 1`; nothing else changes |
//! | `Reattempting → recovered` | the clause retires; the round completes as any round does. A recovered retry leaves no transcript row (ADR-0134 deleted `retry_recovered`): the receipt is its record |
//! | `Reattempting → exhausted/interrupted` | the clause retires and the round-interrupt anchor lands, carrying the last fault as its one-line detail |
//!
//! The division of labour between the two live homes is deliberate
//! (`[INV-RETRY-08]`): the **clause** is the only surface allowed to carry a
//! countdown, because it is repainted every frame and can stay true. The
//! **receipt** states only time-invariant facts, because a card stack is read at
//! an arbitrary later moment — a countdown frozen into a record is not redundant
//! but wrong.
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
//! that a retry recovered — it only knows how many faults it carries, and the
//! fault list exists for the interrupt anchor's detail line.

use std::time::Instant;

/// How much of the retry budget a round has consumed, and what it is doing now.
///
/// Constructed only by [`RetryProgress::scheduled`], which clamps the attempt
/// into `1..=max_attempts`, so every instance satisfies
/// `1 <= attempt <= max_attempts` — the ordinal on screen can never be a
/// negative or out-of-range count.
///
/// # What is *not* rendered
///
/// `max_attempts` is policy, not news: it is the configured ceiling
/// (`provider_retry_max_attempts`), and with the stepped backoff it is reached
/// after minutes. Painting it on the bar (the retired `retry 2/30` form)
/// competed for the eye with the two facts the user actually needs — *which*
/// failure this is, and how long until the next attempt — and the outstanding
/// budget is still stated where it becomes decision-relevant: once per attempt in
/// the retained receipt ([`RetryProgress::receipt_label`]), and in the
/// exhausted-round error ("Exhausted N retry attempts — …"). The field stays
/// because the clamp needs it and the wire event carries it; the slide rule is
/// simply not the sentence.
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
    /// round-interrupt anchor can report what the round actually died on rather
    /// than only that it died.
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
        // disagree with `retries()`.
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
    /// Invariant: for an episode folded from the wire this equals
    /// [`Self::faults`]`.len()`. One attempt past the first is one retry is one
    /// fault, so the ordinal a reader sees and the number of failures recorded are
    /// two readings of one fact rather than two counters. ([`Self::retry_ordinal`]
    /// derives from this, not from the list, so a state constructed mid-episode —
    /// a resumed round, a test fixture — cannot report a wrong ordinal merely
    /// because it carries a short fault list.)
    pub fn retries(&self) -> usize {
        self.attempt.saturating_sub(1)
    }

    /// How many retries the round was allowed — `max_attempts - 1`.
    ///
    /// The denominator that belongs beside [`Self::retry_ordinal`]. The wire's
    /// `max_attempts` counts the *initial request* as attempt 1, so it is one more
    /// than the number of retries the round may spend; pairing it with a retry
    /// ordinal would put two different units on either side of an `of`
    /// (`retry 1 of 30` for a 30-request budget, which is really 29 retries) and
    /// would contradict the exhausted-round error's "Exhausted 29 retry attempts".
    /// Saturating, because a `max_attempts` of 1 (or a clamp-degenerate 0) allows
    /// zero retries and must not underflow.
    ///
    /// A capacity of 0 means **the ratio is not sayable** and
    /// [`Self::receipt_label`] falls back to the bare ordinal: a round that was
    /// allowed no retries cannot have reached `retry 1`, so `retry 1 of 0` would be
    /// an impossible count — the exact class of bug the constructor's clamp exists
    /// to prevent.
    pub fn retries_capacity(&self) -> usize {
        self.max_attempts.saturating_sub(1)
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

    /// Which retry this is, 1-based — the ordinal a reader means by "this is the
    /// first retry".
    ///
    /// Equals [`Self::retries`] (`attempt - 1`: one attempt past the first
    /// request is one retry) floored at 1, so the clamp's degenerate
    /// `attempt == 1` input still cannot put `retry 0` on screen.
    ///
    /// For an episode folded from the wire ([`Self::scheduled`] then
    /// [`Self::with_next_attempt`]) it is also [`Self::faults`]`.len()`: every
    /// attempt contributes exactly one fault, so "first retry" and "first
    /// failure" are two readings of one fact rather than two counters. A state
    /// constructed mid-episode (a resumed round, a test fixture) carries only the
    /// faults it was given, which is why the ordinal is derived from the attempt
    /// pair and not counted off the list.
    pub fn retry_ordinal(&self) -> usize {
        self.retries().max(1)
    }

    /// The bar's live clause: `retry 1 in 4s` while waiting out the backoff,
    /// `retry 1` once the retried request is in flight again.
    ///
    /// The **only** place this sentence is composed. Three things are
    /// deliberately *not* in it:
    ///
    /// - **The budget** (`/30`). `max_attempts` is policy, not news: it is the
    ///   configured ceiling, reached only after minutes of stepped backoff. It
    ///   spent columns on the one row that also has to carry the status verb and
    ///   the round timer, and it is stated where it becomes decision-relevant
    ///   instead — [`Self::receipt_label`] in the notice log, and the exhausted
    ///   round's `Exhausted N retry attempts — …` error.
    /// - **The attempt number** (`retry 2` for the first retry). [`Self::attempt`]
    ///   counts from the initial request, so it reads one higher than what the
    ///   user is watching happen. The ordinal is [`Self::retry_ordinal`].
    /// - **A second elapsed timer** (`running for 1.2s`). While the retry is in
    ///   flight the bar's status verb is already `waiting for model` and the
    ///   round's own elapsed clock sits on the same row; a third clock counting
    ///   the same wait upward was noise, and it churned every frame.
    pub fn label(&self, now: Instant) -> String {
        let ordinal = self.retry_ordinal();
        match self.phase(now) {
            RetryPhase::BackingOff => format!(
                "retry {ordinal} in {}",
                format_retry_countdown(self.retry_at.saturating_duration_since(now))
            ),
            RetryPhase::Reattempting => format!("retry {ordinal}"),
        }
    }

    /// The width-constrained form: the ordinal without the countdown, `retry 1`.
    ///
    /// Used by the bar when the full clause would crowd out the status label.
    /// It is a method here rather than a prefix of [`Self::label`] because
    /// parsing the phrase back out of its own rendering is how the two
    /// vocabularies drifted apart in the first place.
    pub fn compact_label(&self) -> String {
        format!("retry {}", self.retry_ordinal())
    }

    /// The retained-record form: `retry 1 of 29`.
    ///
    /// **A record is a card, not a countdown.** The notice log is read after the
    /// fact, at a moment unrelated to when the retry was scheduled, so a live
    /// phrase baked into it is not merely redundant — it is wrong: `retry 1 in 4s`
    /// frozen at 12:00:01 is a false statement to anyone reading it at 12:00:40,
    /// and nothing in the log can ever tick it back to the truth. So the record
    /// states only the facts that cannot decay: which retry this was, and how much
    /// retry budget the round had ([`Self::label`], the countdown, is the bar's and
    /// the bar's alone).
    ///
    /// # Why the budget is `retries_capacity`, not `max_attempts`
    ///
    /// Both sides of the `of` are **retries**, not attempts. `max_attempts` counts
    /// the initial request as attempt 1, so pairing it with a retry ordinal
    /// produced `retry 1 of 30` for a round with `provider_retry_max_attempts =
    /// 30` — a ratio of two different units that reads as "30 retries allowed"
    /// while the honest answer is 29, and that contradicted the exhausted-round
    /// error ("Exhausted 29 retry attempts") beside it. [`Self::retries_capacity`]
    /// is `max_attempts - 1`, the same subtraction the ordinal already applies.
    ///
    /// The budget lives here because this is where it becomes decision-relevant:
    /// "how much rope was left?" is a question only a record can answer, and it is
    /// the one question the live bar must not spend columns on. Same
    /// [`Self::retry_ordinal`] as the clause, so the two cannot disagree about
    /// which retry it was.
    ///
    /// When [`Self::retries_capacity`] is 0 the ratio is unsayable (no retry was
    /// ever allowed, so no retry happened) and this degrades to the bare ordinal
    /// rather than printing `retry 1 of 0`.
    pub fn receipt_label(&self) -> String {
        match self.retries_capacity() {
            0 => format!("retry {}", self.retry_ordinal()),
            capacity => format!("retry {} of {capacity}", self.retry_ordinal()),
        }
    }
}

/// A pending backoff as a countdown reads: whole seconds, rounded up so a wait
/// that is nearly over never reads `0s` and never overshoots below the tick.
///
/// Sub-second precision is deliberately absent. The bar repaints every frame,
/// and a tenths-of-a-second tail (`next in 0.8s`) is unreadable churn: the eye
/// cannot sample it, and it makes the row flicker while telling the user nothing
/// they can act on. Whole seconds tick once, which is the cadence a person can
/// actually follow.
///
/// This is the **only** duration formatter in the retry vocabulary. A separate
/// one-decimal variant (`format_retry_duration`) once existed for "record-shaped
/// prose", but the records stopped needing it: the receipt states the ordinal and
/// the budget ([`RetryProgress::receipt_label`]), the exhausted error states the
/// count, and the headless line states its configured backoff in whole seconds.
/// Keeping a formatter with no caller would leave a second reading of "how long
/// is this wait" available for a future paint site to reach for.
pub fn format_retry_countdown(duration: std::time::Duration) -> String {
    let millis = duration.as_millis() as u64;
    format!("{}s", millis.div_ceil(1_000).max(1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn at(now: Instant, ms: u64) -> Instant {
        now + Duration::from_millis(ms)
    }

    #[test]
    fn the_clause_names_which_retry_this_is_never_the_raw_attempt_number() {
        // The defect the label once had: it painted `attempt`, which counts from
        // the *initial* request, so the first retry read `retry 2`. A reader
        // asking "how many times has it failed?" means 1. One owner, one
        // ordinal: `attempt 6` is the fifth retry, and the label says 5.
        let now = Instant::now();
        let progress = RetryProgress::scheduled(6, 30, at(now, 8_000), "Google HTTP 503");
        assert_eq!(progress.label(now), "retry 5 in 8s");
        assert_eq!(progress.attempt(), 6, "the wire attempt is unchanged");
        assert_eq!(progress.retries(), 5);
        assert_eq!(progress.retry_ordinal(), 5);
        assert_eq!(
            progress.retry_ordinal(),
            progress.retries(),
            "the ordinal is the retry count, floored at 1"
        );
    }

    #[test]
    fn the_clause_does_not_spend_columns_on_the_budget() {
        // `max_attempts` is configured policy, not news. It used to ride the bar
        // as `6/30`, competing with the status verb on the one row that also
        // carries the round timer; it belongs in the retained receipt and the
        // terminal error, which are read after the fact.
        let now = Instant::now();
        let progress = RetryProgress::scheduled(2, 30, at(now, 4_000), "HTTP 429");
        assert!(
            !progress.label(now).contains("30"),
            "the live clause must not paint the budget: {}",
            progress.label(now)
        );
        assert!(
            !progress.compact_label().contains("30"),
            "nor may the narrow form: {}",
            progress.compact_label()
        );
        assert_eq!(
            progress.receipt_label(),
            "retry 1 of 29",
            "the budget is stated once, in the retained record"
        );
    }

    #[test]
    fn the_first_retry_is_the_first_failure() {
        // Both readings of "how bad is this?" are one number: the initial
        // request is not a retry and not a failure, so a scheduled retry is
        // always at least the first.
        let now = Instant::now();
        let first = RetryProgress::scheduled(2, 5, at(now, 1_000), "HTTP 503");
        assert_eq!(first.retry_ordinal(), 1);
        assert_eq!(first.faults().len(), 1);
        assert_eq!(first.label(now), "retry 1 in 1s");
        assert_eq!(first.receipt_label(), "retry 1 of 4");
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
    fn the_label_drops_the_countdown_once_the_attempt_is_in_flight() {
        let now = Instant::now();
        let progress = RetryProgress::scheduled(2, 5, at(now, 4_000), "HTTP 529");
        assert_eq!(progress.label(now), "retry 1 in 4s");
        assert_eq!(
            progress.label(at(now, 5_200)),
            "retry 1",
            "an elapsed backoff says *which* retry is running and stops counting: \
             the bar's status verb already states what it is waiting for, and the \
             round's own elapsed clock is on the same row"
        );
    }

    #[test]
    fn the_receipt_is_a_card_with_no_decaying_fact_in_it() {
        // The notice log is a card stack read at an arbitrary later moment, not a
        // ticking surface. So the recorded line must carry *only* facts that stay
        // true: which retry, and how much budget. A countdown baked into a record
        // is wrong the second after it is written and can never be corrected.
        let now = Instant::now();
        let progress = RetryProgress::scheduled(3, 30, at(now, 5_000), "HTTP 503");
        let receipt = progress.receipt_label();

        assert_eq!(receipt, "retry 2 of 29");
        for decaying in [" in ", "next", "running", "0s", "5s"] {
            assert!(
                !receipt.contains(decaying),
                "the record must not carry a live countdown ({decaying:?}): {receipt}"
            );
        }
        // Read a minute later it still says the same, true thing.
        assert_eq!(
            progress.receipt_label(),
            receipt,
            "the record is time-invariant"
        );
        // Whereas the bar's clause is deliberately not: that is the whole split.
        assert!(
            progress.label(at(now, 4_000)).contains("in "),
            "the live clause does carry the countdown"
        );
    }

    #[test]
    fn the_receipt_states_both_sides_of_the_ratio_in_the_same_unit() {
        // Both sides of `of` are *retries*. `max_attempts` counts the initial
        // request as attempt 1, so pairing it with a retry ordinal mixed units:
        // a round with `provider_retry_max_attempts = 30` may spend 29 retries,
        // and `retry 1 of 30` also contradicted the exhausted-round error
        // ("Exhausted 29 retry attempts") printed for the very same round.
        let now = Instant::now();
        for (max_attempts, expected_capacity) in [(2usize, 1usize), (5, 4), (30, 29), (60, 59)] {
            let first = RetryProgress::scheduled(2, max_attempts, at(now, 1_000), "HTTP 503");
            assert_eq!(
                first.retries_capacity(),
                expected_capacity,
                "max_attempts {max_attempts} allows {expected_capacity} retries"
            );
            assert_eq!(
                first.receipt_label(),
                format!("retry 1 of {expected_capacity}")
            );
            // The last reachable retry fills the denominator exactly — never
            // `retry 29 of 30`, which would imply a retry that never exists.
            let last = RetryProgress::scheduled(max_attempts, max_attempts, at(now, 1_000), "f");
            assert_eq!(
                last.receipt_label(),
                format!("retry {expected_capacity} of {expected_capacity}"),
                "the budget and the exhausted-error count agree"
            );
            assert_eq!(
                last.retries(),
                expected_capacity,
                "and that count is what the exhausted error prints"
            );
        }
        // A budget of one attempt allows zero retries: the capacity saturates at
        // 0 and `receipt_label` drops the ratio rather than printing `of 0`.
        let degenerate = RetryProgress::scheduled(1, 1, now, "f");
        assert_eq!(degenerate.retries_capacity(), 0);
        assert_eq!(degenerate.receipt_label(), "retry 1");
    }

    #[test]
    fn the_compact_form_is_the_same_ordinal_without_the_countdown() {
        let now = Instant::now();
        let progress = RetryProgress::scheduled(6, 30, at(now, 8_000), "HTTP 503");
        assert_eq!(progress.compact_label(), "retry 5");
        // Both forms read the same field, so the count can never disagree
        // between the wide and the narrow bar: the wide form contains the narrow
        // one verbatim, and both are built from `retry_ordinal`.
        assert!(
            progress.label(now).contains(&progress.compact_label()),
            "the wide form must carry the narrow one: {}",
            progress.label(now)
        );
        assert_eq!(
            progress.compact_label(),
            format!("retry {}", progress.retry_ordinal())
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
        // A producer bug (or a replayed event) must not put `retry 0` on screen:
        // the constructor is the clamp, and the ordinal floors at 1 on top of it.
        //
        // Both *forms* are checked, not just `label`. Checking only the live
        // clause is how `retry 1 of 0` shipped: `max_attempts = 1` allows zero
        // retries, so the ratio had no sayable denominator while the ordinal still
        // floored to 1 — an impossible count in the record that the clause's own
        // test could not see.
        let now = Instant::now();
        assert_eq!(
            RetryProgress::scheduled(0, 0, now, "f").label(now),
            "retry 1",
            "the degenerate attempt 0 still reads as the first retry"
        );
        assert_eq!(
            RetryProgress::scheduled(7, 5, now, "f").label(now),
            "retry 4",
            "an attempt past the budget clamps to the budget: 5 - 1 = 4"
        );
        // Zero-retry budget: no ratio is sayable, so neither form prints one.
        let no_budget = RetryProgress::scheduled(1, 1, now, "f");
        assert_eq!(no_budget.retries_capacity(), 0);
        assert_eq!(no_budget.receipt_label(), "retry 1");
        assert!(
            !no_budget.receipt_label().contains(" of 0"),
            "a zero denominator must never reach the screen: {}",
            no_budget.receipt_label()
        );
        let widened = RetryProgress::scheduled(1, 1, now, "f").with_next_attempt(9, 3, now, "g");
        assert_eq!(widened.attempt(), 3);
        assert_eq!(widened.max_attempts(), 3);
        assert_eq!(widened.receipt_label(), "retry 2 of 2");
    }

    #[test]
    fn a_pending_countdown_is_whole_seconds_and_never_zero() {
        // The bar repaints every frame, so a tenths tail is unreadable churn and
        // a `0s` reading is a lie while the request still has not been re-sent.
        assert_eq!(format_retry_countdown(Duration::from_millis(0)), "1s");
        assert_eq!(format_retry_countdown(Duration::from_millis(1)), "1s");
        assert_eq!(format_retry_countdown(Duration::from_millis(800)), "1s");
        assert_eq!(format_retry_countdown(Duration::from_millis(1_001)), "2s");
        assert_eq!(format_retry_countdown(Duration::from_secs(10)), "10s");
        assert_eq!(format_retry_countdown(Duration::from_millis(59_400)), "60s");
    }
}
