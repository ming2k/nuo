//! The dispatch queue: block/resume, reorder, recall/restore, and the queue-pointer stash/commit cycle.
use super::*;

impl App {
    /// Whether one outbox row is addressable/manipulable in the queue modal.
    ///
    /// **This is the single definition of the outbox's visible set.** Every
    /// addressing helper ([`Self::queued_at`], [`Self::remove_queued_at`],
    /// [`Self::recall_queued_at`], [`Self::move_queued`]) and every projection
    /// (the queue modal, the activity strip) MUST filter through it, because a
    /// display index and a mutation index that disagree silently mutate the
    /// wrong row — `D` deleting a different item than the highlighted one.
    ///
    /// `Dispatching` is excluded: the request is in flight and the item is
    /// waiting on its ack, so removing or reordering it locally would outrun
    /// the server's answer.
    pub fn is_outbox_visible(item: &QueuedDispatch) -> bool {
        item.state != QueuedDispatchState::Dispatching
    }

    /// The viewed session's visible outbox rows, in dispatch order. The one
    /// projection every surface and every index-based mutation agrees on.
    fn outbox_rows(&self, session_id: &str) -> impl Iterator<Item = (usize, &QueuedDispatch)> {
        self.pending_dispatch
            .iter()
            .enumerate()
            .filter(move |(_, item)| item.session_id == session_id && Self::is_outbox_visible(item))
    }

    /// The visible outbox rows as surfaces consume them: the queue modal and the
    /// activity strip both project from here, so "what the strip counts" and
    /// "what the modal lists" cannot drift from "what `D` deletes".
    pub fn outbox_projection(&self, session_id: &str) -> Vec<&QueuedDispatch> {
        self.outbox_rows(session_id).map(|(_, item)| item).collect()
    }

    /// Total visible outbox rows across every session. The keymap context's
    /// `queue_count` answers "is there outbox work anywhere", which must be the
    /// same population every per-session surface counts.
    pub fn outbox_total(&self) -> usize {
        self.pending_dispatch
            .iter()
            .filter(|item| Self::is_outbox_visible(item))
            .count()
    }

    /// Remove one client-owned steer row by id, returning its full payload.
    ///
    /// Only `Steering` rows match: the arm that ends a steer must never consume
    /// a `Waiting` follow-up that happens to share an id, because that row is a
    /// projection of the server's queue and still has a life of its own.
    pub fn take_steer_row(&mut self, session_id: &str, input_id: &str) -> Option<QueuedDispatch> {
        let position = self.pending_dispatch.iter().position(|item| {
            item.session_id == session_id
                && item.id == input_id
                && item.state == QueuedDispatchState::Steering
        })?;
        self.pending_dispatch.remove(position)
    }

    /// Reconcile [`App::pending_images`] / [`App::pending_text_pastes`]
    /// against the chips that currently survive in [`App::input`], and
    /// relabel the surviving chips so their `#N` matches their new 1-based
    /// position in the truncated vectors. Cheap to run on every input
    /// mutation: it is a single linear scan over the input string.
    ///
    /// This is the prune + relabel pass that drops orphaned staged entries
    /// whenever the user deletes or edits a chip — by backspace, selection
    /// delete, or hand-typing over the chip text. Mirrors codex's
    /// `reconcile_deleted_elements` and claude-code's `parseReferences`
    /// effect, adapted to nuo's "chip text lives in the input" model.
    pub fn reconcile_attachments(&mut self) {
        let new_input = composer_attachments::reconcile(
            &self.input,
            &mut self.pending_images,
            &mut self.pending_text_pastes,
        );
        self.input = new_input;
    }

    /// How many staged messages are visible in this session's outbox (front
    /// pops first). Counts exactly the rows the queue modal and the activity
    /// strip render — see [`Self::is_outbox_visible`]. A count that disagreed
    /// with the rendered rows would make `N queued` name items the operator
    /// cannot address.
    ///
    /// The count is **per session**, deliberately: the strip above the composer
    /// speaks about the session on screen, while the command registry's
    /// `AppContext::queue_count` uses [`Self::outbox_total`] to answer the
    /// different question "is there outbox work anywhere". Two names, two
    /// scopes, never one.
    pub fn pending_count(&self, session_id: &str) -> usize {
        self.pending_dispatch
            .iter()
            .filter(|item| item.session_id == session_id && Self::is_outbox_visible(item))
            .count()
    }

    pub fn remove_dispatch(&mut self, session_id: &str, input_id: &str) -> Option<QueuedDispatch> {
        let position = self
            .pending_dispatch
            .iter()
            .position(|item| item.session_id == session_id && item.id == input_id)?;
        self.pending_dispatch.remove(position)
    }

    /// Is this session's outbox paused? While paused the **driver** ships
    /// nothing for it (M4: `FollowUpQueue::is_paused` gates the round-boundary
    /// ship), even after natural completion + idle.
    ///
    /// This set is the client's optimistic projection of the server's
    /// authoritative flag — [`Self::set_queue_blocked`] always accompanies the
    /// `QueuePaused` verb, and the `QueueUpdated` snapshot overwrites it. The
    /// queue modal pauses on open and resumes on close; `Ctrl+P` and
    /// `/queue block` toggle it.
    pub fn is_queue_blocked(&self, session_id: &str) -> bool {
        self.queue_blocked_sessions.contains(session_id)
    }

    /// Force the pause on, regardless of its current state. Used when the queue
    /// modal opens so items can be managed safely (delete / reorder / re-edit)
    /// without the driver shipping one mid-edit.
    ///
    /// Local projection only — the caller sends `QueuePaused`.
    pub fn block_queue(&mut self, session_id: &str) {
        self.queue_blocked_sessions.insert(session_id.to_string());
    }

    /// Force the pause off, releasing what [`Self::block_queue`] set.
    ///
    /// Every close path is a *symmetric* release of the open path's pause — but
    /// a release that stops at this set leaves the driver paused forever, so
    /// the exit hooks that call it also send `QueuePaused{paused:false}`. Local
    /// projection only, like `block_queue`.
    pub fn resume_queue(&mut self, session_id: &str) {
        self.queue_blocked_sessions.remove(session_id);
    }

    /// The id of the queue-modal-highlighted item (M4: queue verbs
    /// address the server queue by id).
    pub fn queued_at(&self, session_id: &str, idx: usize) -> Option<&QueuedDispatch> {
        self.outbox_rows(session_id).nth(idx).map(|(_, item)| item)
    }

    /// Set (not toggle) the local pause projection; the server verb is
    /// authoritative (M4).
    pub fn set_queue_blocked(&mut self, session_id: &str, blocked: bool) {
        if blocked {
            self.queue_blocked_sessions.insert(session_id.to_string());
        } else {
            self.queue_blocked_sessions.remove(session_id);
        }
    }

    /// Remove the viewed session's outbox item at display index `idx`. Used by
    /// the queue modal's `D` delete. Returns the removed dispatch (mostly for
    /// tests).
    ///
    /// Local projection only: pair it with `QueueRemove` (or `CancelSteer` for a
    /// steer row), or the server keeps the item and ships it later
    /// (`[INV-OUTBOX-03]`).
    pub fn remove_queued_at(&mut self, session_id: &str, idx: usize) -> Option<QueuedDispatch> {
        let position = self.outbox_rows(session_id).nth(idx).map(|(pos, _)| pos)?;
        self.pending_dispatch.remove(position)
    }

    /// Move the viewed session's outbox item at display index `idx` by `delta`
    /// slots within the session's Waiting slice (`delta < 0` toward the front
    /// / next to pop, `delta > 0` toward the tail). Other items in the slice
    /// shift to make room (a true reorder, not a swap). Clamped at the slice
    /// boundaries so an item can never escape into another session's region
    /// of the deque.
    pub fn move_queued(&mut self, session_id: &str, idx: usize, delta: i32) {
        // Collect the positions (into the global deque) of this session's
        // Waiting items in display order — the selectable range.
        let positions: Vec<usize> = self.outbox_rows(session_id).map(|(pos, _)| pos).collect();
        let count = positions.len();
        if count == 0 {
            return;
        }
        let clamped_idx = idx.min(count - 1);
        let new_idx = (clamped_idx as i32 + delta).clamp(0, count as i32 - 1) as usize;
        if new_idx == clamped_idx {
            return;
        }
        let from = positions[clamped_idx];
        let target = positions[new_idx];
        // Remove the item, then re-insert at `target`. This lands the item at
        // the destination slot while the displaced neighbors shift to fill the
        // gap — a true reorder, not a swap. The single `target` works for both
        // directions: when moving toward the tail, removal of `from` (before
        // `target`) shifts `target` down by one, exactly offset by inserting
        // one past the neighbor; when moving toward the front, no shift occurs
        // and the item lands just before the neighbor. `from` is a valid index
        // by construction (enumerated from the deque above), so the remove is
        // guarded rather than `expect`-ed.
        if let Some(item) = self.pending_dispatch.remove(from) {
            self.pending_dispatch.insert(target, item);
        }
    }

    /// Remove and return the viewed session's newest `Waiting` row (LIFO).
    ///
    /// **Local projection only.** A `Waiting` row is the client's view of the
    /// driver's `FollowUpQueue`, so a caller that wants the removal to be real
    /// must also send `QueueRemove`; a removal that only touches the projection
    /// leaves the message queued server-side, where it ships at the next round
    /// boundary and duplicates the prompt the user is editing. The queue
    /// modal's `Enter` (`RecallQueuedSelected`) does exactly that.
    pub fn recall_queued(&mut self, session_id: &str) -> Option<QueuedDispatch> {
        let position = self.pending_dispatch.iter().rposition(|item| {
            item.session_id == session_id && item.state == QueuedDispatchState::Waiting
        })?;
        self.pending_dispatch.remove(position)
    }

    /// Remove and return the outbox item at display index `idx`
    /// (front-of-queue = 0). Used by the queue modal's `Enter` re-edit, which
    /// keys off the `↑/↓` selection rather than always targeting the newest —
    /// so a mid-queue item can be pulled back to the composer too.
    pub fn recall_queued_at(&mut self, session_id: &str, idx: usize) -> Option<QueuedDispatch> {
        let position = self.outbox_rows(session_id).nth(idx).map(|(pos, _)| pos)?;
        self.pending_dispatch.remove(position)
    }

    /// Put a recalled outbox item back into the composer.
    ///
    /// A queue recall is a user gesture aimed at *this* row, so the recalled
    /// text becomes the live buffer — but it uses `StashCurrent`, not `Replace`:
    /// a recall is exactly the case where the composer may hold a draft the user
    /// was writing, and discarding it was the silent-loss shape this policy
    /// retired. The draft goes to the capsule, where ↓ returns it.
    pub fn restore_dispatch(&mut self, dispatch: QueuedDispatch) {
        self.adopt_as_draft(
            dispatch.text,
            dispatch.images,
            dispatch.text_pastes,
            DraftAdoption::StashCurrent,
        );
    }

    /// Clear the stashed draft capsule when a prompt is committed: its content
    /// is historicised, so it is no longer the "unsent" slot and must not come
    /// back on a later ↓.
    pub fn clear_history_draft(&mut self) {
        self.clear_stashed_draft();
    }
}
