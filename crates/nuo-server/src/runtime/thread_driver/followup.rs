//! The driver-owned follow-up queue authority (M4).

/// The owned state and request loop for one live session.
///
/// A frontend currently assembles the driver after startup wiring and moves it
/// into a Tokio task. (The ADR-0037 §6 `SessionRegistry` factory was removed
/// as dormant; if the server move resumes, the fields can become private
/// without changing the driver model.)
#[allow(clippy::type_complexity)]
/// The driver-owned follow-up queue authority (M4).
///
/// One queue per hosted session driver, keyed by target session (the
/// primary and any other hosted session — each item carries its target's id,
/// and the busy check is against the *target's* own round lifecycle). The
/// frontend sends `AgentRequest::FollowUp` and renders the `QueueUpdated`
/// snapshots; it never decides when an item ships.
pub(crate) struct FollowUpQueue {
    pub(super) items: Vec<QueuedFollowUp>,
    paused: std::collections::HashSet<String>,
}

pub(crate) struct QueuedFollowUp {
    pub session_id: String,
    pub message: nuo_protocol::QueuedMessage,
}

impl FollowUpQueue {
    pub(crate) fn new() -> Self {
        Self {
            items: Vec::new(),
            paused: std::collections::HashSet::new(),
        }
    }

    pub(crate) fn is_paused(&self, session_id: &str) -> bool {
        self.paused.contains(session_id)
    }

    pub(crate) fn set_paused(&mut self, session_id: &str, paused: bool) {
        if paused {
            self.paused.insert(session_id.to_string());
        } else {
            self.paused.remove(session_id);
        }
    }

    pub(crate) fn enqueue(&mut self, session_id: &str, message: nuo_protocol::QueuedMessage) {
        self.items.push(QueuedFollowUp {
            session_id: session_id.to_string(),
            message,
        });
    }

    pub(crate) fn remove(&mut self, session_id: &str, input_id: &str) {
        self.items
            .retain(|item| !(item.session_id == session_id && item.message.id == input_id));
    }

    pub(crate) fn clear(&mut self, session_id: &str) {
        self.items.retain(|item| item.session_id != session_id);
    }

    /// Reorder one item within its session's queue by `delta` positions
    /// (queue modal `K`/`J`), clamped at the session's slice boundaries.
    /// Unknown ids are a no-op.
    pub(crate) fn reorder(&mut self, session_id: &str, input_id: &str, delta: i32) {
        let positions: Vec<usize> = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| item.session_id == session_id)
            .map(|(position, _)| position)
            .collect();
        let Some(local) = positions
            .iter()
            .position(|&position| self.items[position].message.id == input_id)
        else {
            return;
        };
        let target_local =
            (local as i64 + delta as i64).clamp(0, positions.len() as i64 - 1) as usize;
        if target_local == local {
            return;
        }
        let from = positions[local];
        let to = positions[target_local];
        let item = self.items.remove(from);
        self.items.insert(to, item);
    }

    /// The authoritative snapshot for one session.
    pub(crate) fn snapshot(&self, session_id: &str) -> (Vec<nuo_protocol::QueuedMessage>, bool) {
        let items = self
            .items
            .iter()
            .filter(|item| item.session_id == session_id)
            .map(|item| item.message.clone())
            .collect();
        (items, self.paused.contains(session_id))
    }

    /// Dequeue and return the first shippable item for `session_id`, if any.
    pub(crate) fn dequeue_front(
        &mut self,
        session_id: &str,
    ) -> Option<nuo_protocol::QueuedMessage> {
        let position = self
            .items
            .iter()
            .position(|item| item.session_id == session_id)?;
        let item = self.items.remove(position);
        Some(item.message)
    }

    pub(crate) fn has_items_for(&self, session_id: &str) -> bool {
        self.items.iter().any(|item| item.session_id == session_id)
    }
}

#[cfg(test)]
mod followup_queue_tests {
    use super::super::recovery::ship_queued_follow_ups;
    use super::*;
    use nuo_engine::RoundLifecycle;
    use nuo_protocol::AgentResponse;
    use std::sync::Arc;
    use tokio::sync::mpsc;

    fn message(id: &str, text: &str) -> nuo_protocol::QueuedMessage {
        nuo_protocol::QueuedMessage {
            id: id.to_string(),
            text: text.to_string(),
            display_text: None,
            images: Vec::new(),
            sent_at_ms: None,
        }
    }

    #[test]
    fn enqueue_preserves_fifo_order_per_target() {
        let mut queue = FollowUpQueue::new();
        queue.enqueue("primary", message("a", "first"));
        queue.enqueue("primary", message("b", "second"));
        queue.enqueue("thread-2", message("c", "queued"));

        let (primary_items, paused) = queue.snapshot("primary");
        assert_eq!(primary_items.len(), 2);
        assert_eq!(primary_items[0].id, "a");
        assert_eq!(primary_items[1].id, "b");
        assert!(!paused);

        let (other_items, _) = queue.snapshot("thread-2");
        assert_eq!(other_items.len(), 1);
        assert_eq!(other_items[0].id, "c");
    }

    #[test]
    fn dequeue_front_is_fifo_and_target_scoped() {
        let mut queue = FollowUpQueue::new();
        queue.enqueue("primary", message("a", "1"));
        queue.enqueue("primary", message("b", "2"));
        queue.enqueue("thread-2", message("c", "queued"));

        let first = queue.dequeue_front("primary").expect("must dequeue");
        assert_eq!(first.id, "a");
        assert!(queue.has_items_for("primary"));

        let second = queue.dequeue_front("primary").expect("must dequeue");
        assert_eq!(second.id, "b");
        assert!(!queue.has_items_for("primary"));
        // The other thread's item is untouched.
        assert!(queue.has_items_for("thread-2"));
    }

    #[test]
    fn reorder_clamps_within_the_target_slice() {
        let mut queue = FollowUpQueue::new();
        queue.enqueue("primary", message("a", "1"));
        queue.enqueue("primary", message("b", "2"));
        queue.enqueue("primary", message("c", "3"));
        queue.enqueue("thread-2", message("z", "queued"));

        // Move `c` two toward the front: clamped to the slice head.
        queue.reorder("primary", "c", -5);
        let (items, _) = queue.snapshot("primary");
        assert_eq!(items[0].id, "c");
        assert_eq!(items[1].id, "a");
        assert_eq!(items[2].id, "b");
        // The other thread's slice is untouched.
        let (other, _) = queue.snapshot("thread-2");
        assert_eq!(other[0].id, "z");

        // Move `c` back one.
        queue.reorder("primary", "c", 1);
        let (items, _) = queue.snapshot("primary");
        assert_eq!(items[0].id, "a");

        // Moving past the tail clamps.
        queue.reorder("primary", "a", 7);
        let (items, _) = queue.snapshot("primary");
        assert_eq!(items.last().map(|m| m.id.as_str()), Some("a"));

        // Unknown id: no-op.
        queue.reorder("primary", "nope", -1);
        let (items, _) = queue.snapshot("primary");
        assert_eq!(items.len(), 3);
    }

    #[test]
    fn remove_and_clear_are_target_scoped_and_idempotent() {
        let mut queue = FollowUpQueue::new();
        queue.enqueue("primary", message("a", "1"));
        queue.enqueue("thread-2", message("z", "queued"));

        queue.remove("primary", "a");
        assert!(!queue.has_items_for("primary"));
        queue.remove("primary", "a"); // idempotent
        assert!(queue.has_items_for("thread-2"));

        queue.clear("thread-2");
        assert!(!queue.has_items_for("thread-2"));
    }

    #[test]
    fn pause_gates_only_the_target() {
        let mut queue = FollowUpQueue::new();
        queue.enqueue("primary", message("a", "1"));
        queue.enqueue("thread-2", message("z", "queued"));

        queue.set_paused("primary", true);
        assert!(queue.is_paused("primary"));
        assert!(!queue.is_paused("thread-2"));
        queue.set_paused("primary", false);
        assert!(!queue.is_paused("primary"));
    }

    /// A **resume** must reach the ship pass; a pause must not.
    ///
    /// `[INV-OUTBOX-07]`: the pass is evaluated on every driver iteration, so a
    /// resume only has to change the gate — it does not have to *deliver* a
    /// signal. This test pins the weaker (sufficient) contract the hint
    /// provides: a resume with items schedules one pass, a pause schedules none,
    /// and an empty queue is not worth a pass.
    #[tokio::test]
    async fn resuming_a_paused_queue_schedules_a_ship_pass() {
        let mut queue = FollowUpQueue::new();
        queue.enqueue("primary", message("a", "1"));
        queue.set_paused("primary", true);
        let (tx, _rx) = mpsc::unbounded_channel();
        let (wake_tx, mut wake_rx) = mpsc::channel::<String>(8);

        // Pausing: no wake — nothing can ship while paused anyway.
        crate::handlers::handlers_chat::queue_paused(&tx, &mut queue, &wake_tx, "primary".to_string(), true)
            .await;
        assert!(
            wake_rx.try_recv().is_err(),
            "a pause must not schedule a ship pass"
        );
        assert!(queue.is_paused("primary"));

        // Resuming: exactly one wake for the target.
        crate::handlers::handlers_chat::queue_paused(&tx, &mut queue, &wake_tx, "primary".to_string(), false)
            .await;
        assert_eq!(
            wake_rx.try_recv().ok(),
            Some("primary".to_string()),
            "a resume must wake the ship loop, or the queue stalls silently"
        );
        assert!(
            wake_rx.try_recv().is_err(),
            "and exactly once: a wake storm would fight the boundary watchers"
        );

        // Resuming an empty queue is not worth a pass.
        crate::handlers::handlers_chat::queue_paused(&tx, &mut queue, &wake_tx, "nobody".to_string(), false)
            .await;
        assert!(
            wake_rx.try_recv().is_err(),
            "no items, no wake: the ship pass would find nothing"
        );
    }

    /// `[INV-OUTBOX-07]` — shipment does not depend on the wake hint.
    ///
    /// A wake is a lossy hint: `Notify::notified()` resolves once per stored
    /// permit, and the boundary watcher drops its send when the channel is full.
    /// While shipment lived *inside* the wake arm, a lost hint parked the queue
    /// forever and the only symptom was a message that never ran. The pass is now
    /// taken on every iteration, so this test can drive it with the wake channel
    /// deliberately absent: an idle, un-paused target with work must ship.
    #[tokio::test]
    async fn an_idle_target_ships_without_any_wake_hint() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(nuo_persistence::thread::ThreadSession::for_workspace(
            None, None,
        ));
        let id = store.id().await;
        let lifecycle = Arc::new(RoundLifecycle::new());
        let mut queue = FollowUpQueue::new();
        queue.enqueue(&id, message("pending-1", "run me"));
        // No pause, no live round — the gate is open and there is work.
        assert!(!queue.is_paused(&id));
        assert!(!lifecycle.is_running().await);

        // The wake channel is never used: nothing is sent on it, and every
        // permit it could have carried is drained here.
        let (wake_tx, mut wake_rx) = mpsc::channel::<String>(1);
        while wake_rx.try_recv().is_ok() {}

        let (tx, mut rx) = mpsc::unbounded_channel();
        let config: crate::SharedConfig = Arc::new(tokio::sync::RwLock::new(
            nuo_persistence::config::Config::default(),
        ));
        let agent = Arc::new(nuo_engine::Agent::new(
            Arc::new(nuo_engine::NoProvider),
            Vec::new(),
            nuo_protocol::AgentIdentity::default(),
        ));

        ship_queued_follow_ups(
            &agent, &store, &lifecycle, &tx, &config, &mut queue, &wake_tx,
        )
        .await;

        // The item left the queue: the pass acted on the invariant, with no hint
        // in sight. (The round itself then refuses for lack of a provider, which
        // is exactly why the child fixture is a no-provider agent and why only
        // the *dequeue* is asserted here: what is under test is the scheduler,
        // not the provider.)
        assert!(
            !queue.has_items_for(&id),
            "an idle un-paused queue must ship on the invariant alone"
        );
        // And the attempt announced itself on the wire.
        assert!(
            std::iter::from_fn(|| rx.try_recv().ok()).any(|response| matches!(
                response,
                AgentResponse::Round { scope, .. }
                    if matches!(
                        scope.event(),
                        nuo_protocol::RoundEvent::RoundInterrupted(_)
                            | nuo_protocol::RoundEvent::Error(_)
                    )
            )),
            "a refused start is disclosed, never silent"
        );
        drop(tmp);
    }

    /// The boundary contract (M4): the round lifecycle's interrupt
    /// parking must be observable at the round boundary, so the queue parks
    /// after an interrupted round instead of auto-firing more prompts.
    #[tokio::test]
    async fn lifecycle_reports_interruption_at_the_boundary() {
        let lifecycle = RoundLifecycle::new();
        assert!(!lifecycle.was_interrupted());

        let begin = lifecycle.begin().await;
        assert!(!lifecycle.was_interrupted());

        // The operator requests a stop mid-round…
        lifecycle.record_interrupt(nuo_protocol::RoundInterruptReason::User);
        assert!(lifecycle.was_interrupted());

        // …and the round unwinds to its boundary.
        lifecycle.cancel_current().await;
        assert!(lifecycle.finish(begin.generation).await);
        assert!(lifecycle.was_interrupted(), "the queue must park");

        // The next round clears it.
        let _ = lifecycle.begin().await;
        assert!(!lifecycle.was_interrupted());
    }
}
