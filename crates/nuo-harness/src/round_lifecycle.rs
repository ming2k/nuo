//! One session's round lifecycle: at most one active round, superseded by the
//! next `begin`, with stale rounds detected by a generation counter.
//!
//! This type consolidates the cancellation-token slot + generation-counter
//! protocol that used to be threaded through the transport layer as two
//! separate `Arc`s and re-implemented at four call sites (interactive round,
//! pursuit, `!` shell command, `/btw` side session). The protocol itself is
//! deliberately binary — no active round, or an active round identified by a
//! generation; display-level nuance ("running" vs. "pursue" vs.
//! awaiting-permission) lives outside it, in
//! [`nuo_wire::LoopStatus`] and the parked-request tables.
//!
//! The two stop paths differ on purpose:
//!
//! - **Interrupt** (`cancel_current` alone): the in-flight round unwinds and
//!   still emits its own `[Interrupted]` cleanup, because the generation is
//!   *not* bumped.
//! - **Session switch** (`supersede` + `cancel_current`): the generation bump
//!   invalidates the in-flight round, so its generation-guarded cleanup is
//!   suppressed and the switch handler owns the terminal events.
//!
//! Both paths record *why* they stopped (C11): [`RoundLifecycle::record_interrupt`]
//! parks a reason — stamped with the stop-request moment — on the lifecycle the
//! instant the cancellation is requested, and the unwinding round task reads it
//! back via [`RoundLifecycle::take_interrupt`] when it emits its terminal
//! cleanup. This is what lets one `HarnessError::Interrupted` unwind render as
//! "Esc Esc" versus "new message" versus "process exited" without threading a
//! reason through every producer of the error.
//!
//! Parked reasons are one-round-scoped by construction: a stop site parks
//! unconditionally (even with no round live), but [`RoundLifecycle::begin`]
//! clears the slot, so a reason parked while idle can never leak into the
//! next round and mislabel a successful round as interrupted.

use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::RwLock as AsyncRwLock;
use tokio_util::sync::CancellationToken;

/// At-most-one-active-round protocol for one session.
///
/// Cloned into an `Arc` by the owning session (primary or `/btw` side) and
/// shared by every driver that can start or stop a round.
#[derive(Debug, Default)]
pub struct RoundLifecycle {
    token_slot: AsyncRwLock<Option<CancellationToken>>,
    generation: AtomicU64,
    /// The reason the current/most-recent round was stopped, parked by the
    /// stop site ([`Self::record_interrupt`]) and consumed by the unwinding
    /// round task ([`Self::take_interrupt`]). `Mutex` (not `RwLock`) because
    /// [`Self::take_interrupt`] mutates.
    interrupt_reason: std::sync::Mutex<Option<ParkedInterrupt>>,
    /// Round-boundary wake (ADR-0197 M4): notified whenever the session
    /// transitions out of "a round is live" — a current round's `finish`, or
    /// a supersede that ends the live round. Session drivers `select!` on
    /// this to ship queued follow-ups at the round boundary; a watcher that
    /// observes `is_running() == false` after the wake may rely on it.
    finished: tokio::sync::Notify,
    /// Whether an interrupt was requested during the current/most-recent
    /// round (ADR-0197 M4): set by the stop sites, cleared by `begin`. The
    /// follow-up queue authority reads it at the round boundary — a round
    /// the user interrupted must not auto-fire more prompts; the queue
    /// parks (paused) until the user re-arms it.
    interrupted: std::sync::atomic::AtomicBool,
}

/// The result of [`RoundLifecycle::begin`].
#[derive(Debug)]
pub struct RoundBegin {
    /// Token the new round listens on for cancellation.
    pub token: CancellationToken,
    /// Generation identifying this round; pass to
    /// [`RoundLifecycle::is_current`] / [`RoundLifecycle::finish`].
    pub generation: u64,
    /// The superseded round's token, if one was installed. Cancel it *after*
    /// rejecting pending permissions/inputs so parked replies resolve first.
    pub previous: Option<CancellationToken>,
}

/// A stop reason parked at the moment the cancellation was **requested**, with
/// the clock reading of that same moment (ADR-0127). Parking the timestamp
/// here — not stamping it later in the unwinding round's tail — is what keeps
/// the durable record honest: by the time a superseded round's tail runs, the
/// superseding message has already been sent, so a tail-time clock read lands
/// *after* it and the resume seam-merge projects the marker below the newer
/// round's answer, reading as an interrupt of a round that completed
/// normally.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParkedInterrupt {
    pub reason: nuo_wire::RoundInterruptReason,
    /// Unix-epoch milliseconds at stop-request time.
    pub at_ms: u64,
}

impl RoundLifecycle {
    pub fn new() -> Self {
        Self::default()
    }

    /// Begin a new round: bump the generation, install a fresh cancellation
    /// token, and return the superseded predecessor (if any) for the caller
    /// to cancel.
    ///
    /// Also clears any parked interrupt reason: the stop sites park
    /// unconditionally — even when no round is live — so a reason parked
    /// while idle (an Esc Esc with nothing running, a `/resume` switch on an
    /// already-quiet session) must never leak into the *next* round's tail
    /// and mislabel a perfectly successful round as interrupted. The caller
    /// that replaces a live predecessor parks the superseded reason *after*
    /// this `begin` (see `start_interactive_round`), so legitimate labels
    /// are untouched.
    pub async fn begin(&self) -> RoundBegin {
        let token = CancellationToken::new();
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        self.interrupted
            .store(false, std::sync::atomic::Ordering::SeqCst);
        *crate::sync::poison_lock(&self.interrupt_reason) = None;
        let previous = self.token_slot.write().await.replace(token.clone());
        RoundBegin {
            token,
            generation,
            previous,
        }
    }

    /// Whether `generation` still identifies the active round.
    pub fn is_current(&self, generation: u64) -> bool {
        self.generation.load(Ordering::SeqCst) == generation
    }

    /// End-of-round cleanup: release the token slot, but only when
    /// `generation` is still the active round. Returns whether the caller is
    /// still current — and should therefore emit its terminal idle snapshot.
    pub async fn finish(&self, generation: u64) -> bool {
        let mut slot = self.token_slot.write().await;
        if !self.is_current(generation) {
            return false;
        }
        slot.take();
        drop(slot);
        // Round-boundary wake: the driver's follow-up queue ships here.
        self.finished.notify_waiters();
        true
    }

    /// The round-boundary wake signal (ADR-0197 M4). Fires whenever the
    /// live round ends; a waiter re-checks [`Self::is_running`] to confirm
    /// the idle transition (spurious wakes are possible and harmless).
    pub fn finished(&self) -> &tokio::sync::Notify {
        &self.finished
    }

    /// Whether an interrupt was requested during the current/most-recent
    /// round (ADR-0197 M4). See the field docs.
    pub fn was_interrupted(&self) -> bool {
        self.interrupted.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Invalidate the current generation without installing a token. Session
    /// switches (`/resume`, `/session open|fork|new`, …) use this to suppress
    /// the in-flight round's generation-guarded cleanup events.
    pub fn supersede(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        // The live round just lost its validity — wake boundary watchers so
        // they re-read `is_running` (a supersede usually means the next
        // round is already starting; a spurious wake is harmless).
        self.finished.notify_waiters();
    }

    /// Take and cancel the live token, if any; returns whether one was
    /// cancelled. Does not bump the generation — see the module docs for the
    /// interrupt vs. session-switch distinction.
    pub async fn cancel_current(&self) -> bool {
        if let Some(token) = self.token_slot.write().await.take() {
            token.cancel();
            true
        } else {
            false
        }
    }

    /// Park the reason an in-flight round is being stopped (C11), stamped
    /// with the stop-request moment (ADR-0127). Called by the stop site at
    /// the same instant it requests the cancellation, so the unwinding round
    /// task can label its own terminal event without a reason ever being
    /// threaded through the error type — and without the record's timestamp
    /// drifting to tail time, where it lands after the superseding message.
    /// Last writer wins: a supersede that follows a plain interrupt re-labels
    /// the same unwind.
    pub fn record_interrupt(&self, reason: nuo_wire::RoundInterruptReason) {
        self.interrupted
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let parked = ParkedInterrupt {
            reason,
            at_ms: crate::orchestration::unix_epoch_ms(),
        };
        *self
            .interrupt_reason
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(parked);
    }

    /// [`Self::record_interrupt`] with an explicit clock reading, for stop
    /// sites that know the more honest instant: a supersede parks *before*
    /// cancelling the predecessor but the event the user anchored the stop to
    /// is the newer message's send time, so the marker re-projects before
    /// that message on resume (the seam rule, `transcript.rs`). `sent_at_ms`
    /// of `None` (a message with no captured send time) falls back to now.
    pub fn record_interrupt_at(
        &self,
        reason: nuo_wire::RoundInterruptReason,
        at_ms: Option<u64>,
    ) {
        self.interrupted
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let parked = ParkedInterrupt {
            reason,
            at_ms: at_ms.unwrap_or_else(crate::orchestration::unix_epoch_ms),
        };
        *crate::sync::poison_lock(&self.interrupt_reason) = Some(parked);
    }

    /// Consume the parked interrupt reason, if any (C11). Called once by the
    /// unwinding round task when it emits its terminal cleanup; the take
    /// semantics prevent a later round from reading a stale label.
    pub fn take_interrupt(&self) -> Option<ParkedInterrupt> {
        crate::sync::poison_lock(&self.interrupt_reason).take()
    }

    /// Coarse activity signal for watchers (e.g. the `/btw` parent-status
    /// banner): a live, uncancelled token means a round is running.
    pub async fn is_running(&self) -> bool {
        matches!(
            self.token_slot.read().await.as_ref(),
            Some(token) if !token.is_cancelled()
        )
    }

    /// Synchronous best-effort variant of [`Self::is_running`] for render-side
    /// summaries (e.g. the `/btw` asides list's `running` flag, ADR-0103).
    /// Uses `try_read`: when the lock is momentarily contended the answer is
    /// `false`, which a 400 ms-later refresh corrects — these are display
    /// hints, not protocol state.
    pub fn is_running_blocking(&self) -> bool {
        match self.token_slot.try_read() {
            Ok(slot) => matches!(
                slot.as_ref(),
                Some(token) if !token.is_cancelled()
            ),
            Err(_) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn begin_supersedes_predecessor() {
        let lifecycle = RoundLifecycle::new();
        let first = lifecycle.begin().await;
        assert!(first.previous.is_none());
        assert!(lifecycle.is_current(first.generation));
        assert!(lifecycle.is_running().await);

        let second = lifecycle.begin().await;
        let previous = second.previous.expect("second begin supersedes first");
        assert!(!first.token.is_cancelled(), "begin does not cancel");
        previous.cancel();
        assert!(lifecycle.is_current(second.generation));
        assert!(!lifecycle.is_current(first.generation));
    }

    #[tokio::test]
    async fn finish_is_generation_guarded() {
        let lifecycle = RoundLifecycle::new();
        let first = lifecycle.begin().await;
        let second = lifecycle.begin().await;

        // The stale round's cleanup is a no-op: slot keeps the successor's
        // token and the caller is told it is no longer current.
        assert!(!lifecycle.finish(first.generation).await);
        assert!(lifecycle.is_running().await);

        assert!(lifecycle.finish(second.generation).await);
        assert!(!lifecycle.is_running().await);
    }

    #[tokio::test]
    async fn supersede_invalidates_without_token_install() {
        let lifecycle = RoundLifecycle::new();
        let active = lifecycle.begin().await;
        lifecycle.supersede();
        assert!(!lifecycle.is_current(active.generation));
        // The token itself is untouched; cancelling it is a separate step.
        assert!(lifecycle.is_running().await);
        assert!(lifecycle.cancel_current().await);
        assert!(active.token.is_cancelled());
    }

    #[tokio::test]
    async fn cancel_current_on_empty_slot_is_a_no_op() {
        let lifecycle = RoundLifecycle::new();
        assert!(!lifecycle.cancel_current().await);
        let active = lifecycle.begin().await;
        assert!(lifecycle.cancel_current().await);
        assert!(active.token.is_cancelled());
        assert!(!lifecycle.is_running().await);
        // The generation is deliberately not bumped by cancellation.
        assert!(lifecycle.is_current(active.generation));
    }

    #[tokio::test]
    async fn begin_clears_reason_parked_while_idle() {
        // Stop sites park unconditionally — even with no live round. Without
        // the clear in `begin`, that reason leaks into the next round's tail
        // and mislabels a successful round as "interrupted · Esc Esc".
        let lifecycle = RoundLifecycle::new();
        lifecycle.record_interrupt(nuo_wire::RoundInterruptReason::User);
        assert_eq!(
            lifecycle.take_interrupt().map(|parked| parked.reason),
            Some(nuo_wire::RoundInterruptReason::User)
        );

        // Park again while idle; the next begin must discard it.
        lifecycle.record_interrupt(nuo_wire::RoundInterruptReason::Superseded);
        lifecycle.begin().await;
        assert_eq!(
            lifecycle.take_interrupt(),
            None,
            "a reason parked while idle must not survive into the next round"
        );
    }

    #[tokio::test]
    async fn begin_preserves_reason_parked_for_the_live_round() {
        // A stop site parks while a round IS live; the slot survives until
        // that round's tail consumes it — begin only clears *before* the new
        // round is admitted, and the replacement path parks after begin.
        let lifecycle = RoundLifecycle::new();
        let first = lifecycle.begin().await;
        lifecycle.record_interrupt(nuo_wire::RoundInterruptReason::User);
        first.token.cancel();
        // The tail of the cancelled round reads it back...
        assert_eq!(
            lifecycle.take_interrupt().map(|parked| parked.reason),
            Some(nuo_wire::RoundInterruptReason::User)
        );
        // ...and a stray late park while idle is again cleared by begin.
        lifecycle.record_interrupt(nuo_wire::RoundInterruptReason::User);
        let second = lifecycle.begin().await;
        assert_eq!(
            lifecycle.take_interrupt(),
            None,
            "begin admits the new round with a clean interrupt slot"
        );
        assert!(second.previous.is_some(), "second begin supersedes first");
    }

    #[test]
    fn parked_interrupt_carries_the_stop_request_clock() {
        // The timestamp is the stop-request moment, not tail time: a record
        // stamped when the unwinding round's cleanup finally runs can land
        // after the superseding message and misplace the marker on resume.
        let lifecycle = RoundLifecycle::new();
        lifecycle.record_interrupt_at(
            nuo_wire::RoundInterruptReason::Superseded,
            Some(1_000),
        );
        let parked = lifecycle
            .take_interrupt()
            .expect("park survives until taken");
        assert_eq!(
            parked.reason,
            nuo_wire::RoundInterruptReason::Superseded
        );
        assert_eq!(parked.at_ms, 1_000);

        // A park without an explicit instant falls back to the wall clock,
        // which is still the stop-request moment — never tail time.
        lifecycle.record_interrupt(nuo_wire::RoundInterruptReason::User);
        let parked = lifecycle.take_interrupt().expect("fallback park exists");
        assert!(parked.at_ms > 0, "wall-clock fallback is populated");
    }
}
