//! The human-request broker (ADR-0141): single owner of every parked
//! human-decision oneshot.
//!
//! Three protocols used to park an agent mid-round on a human decision —
//! permission approval, `ask_user` questions, and interactive stdin — each
//! kept its own `HashMap<String, oneshot::Sender<..>>` with its own reply /
//! cancel / metrics plumbing, while `docs/reference/state-model.md` §Parked
//! request protocols already specified them as one rule:
//!
//! ```text
//! requested/parked --> replied | cancelled
//! ```
//!
//! This module is that rule, enforced once. [`HumanRequestBroker`] owns:
//!
//! - **park** — register a request, get its receiver and park timestamp;
//! - **reply** — settle exactly-once from a wire-originated (human) reply;
//! - **cancel** — settle everything still parked (turn aborted, session
//!   end, interrupt) with the kind recorded so metrics can tell a user
//!   cancel from a teardown cancel;
//! - **metrics** — per-kind counts of parked / user-replied / cancelled /
//!   policy-settled, and cumulative parked→reply latency, read by
//!   `/metrics` and the server monitor.
//!
//! # Provenance
//!
//! The axis the old code lacked. A settlement arrives either from a client
//! connection (a human — or something acting with the human's authority,
//! like a scripted `PermissionReply`) or from policy inside the harness
//! (no human reachable). The broker tags every settlement; only wire replies
//! are [`ReplyProvenance::User`], and any result text handed to the model
//! names its true source, so a labeled recommendation can never masquerade
//! as a decision.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use nuo_wire::PermissionDecision;
use nuo_wire::human_request::{HumanReply, HumanRequestKind, ReplyProvenance};

use crate::sync::poison_lock;

/// One parked request's oneshot plus bookkeeping.
struct Parked {
    sender: tokio::sync::oneshot::Sender<Settled>,
    kind: HumanRequestKind,
    parked_at: Instant,
}

/// The settlement that wakes a parked tool call. Provenance is attached at
/// settlement, not at park, because the harness only learns *how* a request
/// resolved when it resolves. Provenance travels with the payload so the
/// park site (which formats the model-visible tool result) can name the
/// true source without re-deriving it.
pub struct Settled {
    pub reply: HumanReply,
    pub provenance: ReplyProvenance,
}

/// Counters for one request kind. Plain atomics — read on `/metrics` and
/// the monitor snapshot; contention is irrelevant at human-decision rates.
#[derive(Default)]
struct KindMetrics {
    parked: AtomicU64,
    user_replied: AtomicU64,
    cancelled: AtomicU64,
    policy_settled: AtomicU64,
    /// Sum of park→settlement milliseconds across user replies.
    wait_ms_total: AtomicU64,
    /// Posture-gate refusals: the request never parked.
    refused: AtomicU64,
}

impl KindMetrics {
    fn snapshot(&self) -> HumanRequestMetrics {
        let waited = self.user_replied.load(Ordering::Relaxed);
        HumanRequestMetrics {
            parked: self.parked.load(Ordering::Relaxed),
            user_replied: waited,
            cancelled: self.cancelled.load(Ordering::Relaxed),
            policy_settled: self.policy_settled.load(Ordering::Relaxed),
            refused: self.refused.load(Ordering::Relaxed),
            avg_wait_ms: self
                .wait_ms_total
                .load(Ordering::Relaxed)
                .checked_div(waited)
                .unwrap_or(0),
        }
    }
}

/// A point-in-time metrics snapshot for one request kind. Plain values so
/// it can cross to the monitor / `/metrics` without holding a lock.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HumanRequestMetrics {
    pub parked: u64,
    pub user_replied: u64,
    pub cancelled: u64,
    pub policy_settled: u64,
    pub refused: u64,
    pub avg_wait_ms: u64,
}

/// The single owner of parked human-decision oneshots. The old
/// `AskUserState` / `InputState` / `PermissionState.pending` maps converge
/// here; their public wrappers on `Agent` (`reply_user_question`, …)
/// delegate to this broker so the wire surface never changes.
pub struct HumanRequestBroker {
    parked: Mutex<HashMap<String, Parked>>,
    metrics: [KindMetrics; 3],
}

impl Default for HumanRequestBroker {
    fn default() -> Self {
        Self::new()
    }
}

impl HumanRequestBroker {
    pub fn new() -> Self {
        Self {
            parked: Mutex::new(HashMap::new()),
            // [Permission, Question, Input] — indexed by kind_index().
            metrics: [
                KindMetrics::default(),
                KindMetrics::default(),
                KindMetrics::default(),
            ],
        }
    }

    fn kind_index(kind: HumanRequestKind) -> usize {
        match kind {
            HumanRequestKind::Permission => 0,
            HumanRequestKind::Question => 1,
            HumanRequestKind::Stdin => 2,
        }
    }

    /// Park a request and return its receiver. The caller emits the request
    /// event, fires observe-only hooks, then awaits the receiver; elapsed
    /// park time is charged to the round pause via `Agent::book_pause` as
    /// before.
    pub fn park(
        &self,
        request_id: String,
        kind: HumanRequestKind,
    ) -> tokio::sync::oneshot::Receiver<Settled> {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        poison_lock(&self.parked).insert(
            request_id,
            Parked {
                sender,
                kind,
                parked_at: Instant::now(),
            },
        );
        self.metrics[Self::kind_index(kind)]
            .parked
            .fetch_add(1, Ordering::Relaxed);
        receiver
    }

    /// Settle a parked request from a wire-originated reply. Returns
    /// `false` when no matching request is parked (unknown id, already
    /// settled, or the agent was dropped and rebuilt).
    ///
    /// Permission semantics carried over from the old store: rejecting one
    /// request settles the whole concurrent permission batch with `Reject`
    /// — the caller `join_all`s sibling parked calls and would otherwise
    /// deadlock waiting for replies the UI never shows again.
    pub fn reply_user(&self, request_id: &str, reply: HumanReply) -> bool {
        let Some(Parked {
            sender,
            kind,
            parked_at,
        }) = poison_lock(&self.parked).remove(request_id)
        else {
            return false;
        };
        let metrics = &self.metrics[Self::kind_index(kind)];
        metrics.user_replied.fetch_add(1, Ordering::Relaxed);
        metrics
            .wait_ms_total
            .fetch_add(parked_at.elapsed().as_millis() as u64, Ordering::Relaxed);
        let _ = sender.send(Settled {
            reply: reply.clone(),
            provenance: ReplyProvenance::User,
        });
        if let HumanReply::Permission(PermissionDecision::Reject) = reply {
            self.cancel_kind(HumanRequestKind::Permission);
        }
        true
    }

    /// Settle a parked request from harness policy (no human channel).
    /// Labeled with the policy so the model-visible result names its true
    /// source. Returns `false` when no matching request is parked.
    pub fn settle_by_policy(
        &self,
        request_id: &str,
        reply: HumanReply,
        policy: nuo_wire::human_request::AutonomousFallbackPolicy,
    ) -> bool {
        let Some(Parked {
            sender,
            kind,
            parked_at: _,
        }) = poison_lock(&self.parked).remove(request_id)
        else {
            return false;
        };
        self.metrics[Self::kind_index(kind)]
            .policy_settled
            .fetch_add(1, Ordering::Relaxed);
        let _ = sender.send(Settled {
            reply,
            provenance: ReplyProvenance::Policy { policy },
        });
        true
    }

    /// Owner-side variant of [`Self::settle_by_policy`] for requests this
    /// agent parked through [`Self::park`]: identical semantics, named for
    /// the call site that already holds the request (the ask_user posture
    /// gate) to avoid confusion with wire replies.
    pub fn settle_by_policy_owned(
        &self,
        request_id: String,
        reply: HumanReply,
        policy: nuo_wire::human_request::AutonomousFallbackPolicy,
    ) -> bool {
        self.settle_by_policy(&request_id, reply, policy)
    }

    /// Cancel every parked request. Used on interrupt / turn abort / session
    /// teardown so no tool call can deadlock on a vanished human. Each
    /// cancelled request settles with its kind's cancellation payload
    /// (`None` for Question / Input, `Reject` for Permission).
    pub fn cancel_all(&self) {
        let entries: Vec<Parked> = poison_lock(&self.parked)
            .drain()
            .map(|(_id, parked)| parked)
            .collect();
        for Parked {
            sender,
            kind,
            parked_at: _,
        } in entries
        {
            self.metrics[Self::kind_index(kind)]
                .cancelled
                .fetch_add(1, Ordering::Relaxed);
            let cancelled_reply = match kind {
                HumanRequestKind::Permission => HumanReply::Permission(PermissionDecision::Reject),
                HumanRequestKind::Question => HumanReply::Question(None),
                HumanRequestKind::Stdin => HumanReply::Stdin(None),
            };
            let _ = sender.send(Settled {
                reply: cancelled_reply,
                provenance: ReplyProvenance::Policy {
                    policy: nuo_wire::human_request::AutonomousFallbackPolicy::FailClosed,
                },
            });
        }
    }

    /// Cancel every parked request of one kind. The question teardown path
    /// uses this so permission/input lifecycles are untouched.
    pub fn cancel_kind(&self, kind: HumanRequestKind) {
        let (matching, kept): (Vec<_>, Vec<_>) = poison_lock(&self.parked)
            .drain()
            .partition(|(_, parked)| parked.kind == kind);
        for (id, parked) in kept {
            poison_lock(&self.parked).insert(id, parked);
        }
        for (
            _id,
            Parked {
                sender,
                kind,
                parked_at: _,
            },
        ) in matching
        {
            self.metrics[Self::kind_index(kind)]
                .cancelled
                .fetch_add(1, Ordering::Relaxed);
            let cancelled_reply = match kind {
                HumanRequestKind::Permission => HumanReply::Permission(PermissionDecision::Reject),
                HumanRequestKind::Question => HumanReply::Question(None),
                HumanRequestKind::Stdin => HumanReply::Stdin(None),
            };
            let _ = sender.send(Settled {
                reply: cancelled_reply,
                provenance: ReplyProvenance::Policy {
                    policy: nuo_wire::human_request::AutonomousFallbackPolicy::FailClosed,
                },
            });
        }
    }

    /// Number of requests currently parked. Read by the interrupt path to
    /// log what was torn down.
    pub fn parked_count(&self) -> usize {
        poison_lock(&self.parked).len()
    }

    /// Metrics snapshot for one request kind.
    pub fn metrics_snapshot(&self, kind: HumanRequestKind) -> HumanRequestMetrics {
        self.metrics[Self::kind_index(kind)].snapshot()
    }

    /// Record a refusal: the posture gate rejected a tool's request to park
    /// before any oneshot was created (no park, no settlement — only the
    /// counter moves). Surfaces in `/metrics` as the fail-closed rate.
    pub fn metrics_note_refused(&self, kind: HumanRequestKind) {
        self.metrics[Self::kind_index(kind)]
            .refused
            .fetch_add(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use nuo_wire::UserQuestionReply;

    use super::*;

    /// Block on a oneshot receiver without a runtime: oneshots settle
    /// synchronously once the sender fires, so try_recv suffices in tests.
    fn block_on_settled(rx: tokio::sync::oneshot::Receiver<Settled>) -> Settled {
        let mut rx = rx;
        loop {
            match rx.try_recv() {
                Ok(value) => return value,
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                    std::thread::yield_now();
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                    panic!("settled channel closed without a reply")
                }
            }
        }
    }

    #[test]
    fn park_reply_settles_exactly_once() {
        let broker = HumanRequestBroker::new();
        let rx = broker.park("q1".into(), HumanRequestKind::Question);
        assert!(broker.reply_user(
            "q1",
            HumanReply::Question(Some(UserQuestionReply {
                request_id: "q1".into(),
                answers: vec![vec!["A".into()]],
            }))
        ));
        // Second settle fails — exactly-once.
        assert!(!broker.reply_user("q1", HumanReply::Question(None)));
        let settled = block_on_settled(rx);
        assert_eq!(settled.provenance, ReplyProvenance::User);
        let m = broker.metrics_snapshot(HumanRequestKind::Question);
        assert_eq!((m.parked, m.user_replied, m.cancelled), (1, 1, 0));
    }

    #[test]
    fn policy_settlement_is_labeled() {
        let broker = HumanRequestBroker::new();
        let rx = broker.park("q2".into(), HumanRequestKind::Question);
        assert!(broker.settle_by_policy(
            "q2",
            HumanReply::Question(Some(UserQuestionReply {
                request_id: "q2".into(),
                answers: vec![vec!["Recommended".into()]],
            })),
            nuo_wire::human_request::AutonomousFallbackPolicy::RecommendedLabeled,
        ));
        let settled = block_on_settled(rx);
        match settled.provenance {
            ReplyProvenance::Policy { policy } => assert_eq!(
                policy,
                nuo_wire::human_request::AutonomousFallbackPolicy::RecommendedLabeled
            ),
            ReplyProvenance::User => panic!("policy settlement must not resolve as User"),
        }
    }

    #[test]
    fn cancel_all_settles_every_kind_with_none_or_reject() {
        let broker = HumanRequestBroker::new();
        let q = broker.park("q".into(), HumanRequestKind::Question);
        let p = broker.park("p".into(), HumanRequestKind::Permission);
        let i = broker.park("i".into(), HumanRequestKind::Stdin);
        broker.cancel_all();
        assert!(matches!(
            block_on_settled(q).reply,
            HumanReply::Question(None)
        ));
        assert!(matches!(
            block_on_settled(p).reply,
            HumanReply::Permission(PermissionDecision::Reject)
        ));
        assert!(matches!(block_on_settled(i).reply, HumanReply::Stdin(None)));
        for kind in [
            HumanRequestKind::Permission,
            HumanRequestKind::Question,
            HumanRequestKind::Stdin,
        ] {
            let m = broker.metrics_snapshot(kind);
            assert_eq!((m.parked, m.cancelled), (1, 1), "{kind:?}");
        }
        assert_eq!(broker.parked_count(), 0);
    }
}
