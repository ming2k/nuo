//! Composer input state: caret ownership, selection adoption/deletion, draft parking/restoring, Esc and Ctrl-C arming, token reporting.

use super::*;

impl App {
    /// The double-press confirmation policy for *time-windowed* gestures:
    /// the first press arms a wall-clock window, and a second press inside
    /// it fires (a later press starts a fresh window instead). Two gestures
    /// use this shape — Ctrl+C ×2 quit ([`Self::CTRL_C_ARM_WINDOW`]) and
    /// Esc ×2 interrupt ([`Self::ESC_ARM_WINDOW`]) — both frequent, reversible
    /// intents where a lapsed window must silently re-arm rather than trap.
    /// The rarer, destructive confirms (Ctrl+X → `y` history wipe, the
    /// dashboard's `k` kill) use the complementary *keystroke-armed* policy
    /// instead: the armed state lives exactly one keystroke, no timeout —
    /// a stray `y` an hour later must never wipe history.
    pub const CTRL_C_ARM_WINDOW: std::time::Duration = std::time::Duration::from_secs(2);
    /// The Esc interrupt double-press window — see
    /// [`Self::CTRL_C_ARM_WINDOW`] for the shared confirmation policy.
    pub const ESC_ARM_WINDOW: std::time::Duration = std::time::Duration::from_secs(2);
    /// Record an input-history entry with the on-disk cap mirrored in memory:
    /// `HISTORY_CAP` bounds the persisted union, so an unbounded in-memory
    /// `Vec` would grow past it over a long-lived TUI (each entry is small,
    /// but a multi-day session with heavy prompt reuse is unbounded anyway).
    /// Evicts from the oldest end.
    pub(super) fn push_history(&mut self, entry: nuo_wire::HistoryEntry) {
        self.input_history.push(entry);
        if self.input_history.len() > nuo_wire::history::HISTORY_CAP {
            let overflow = self.input_history.len() - nuo_wire::history::HISTORY_CAP;
            self.input_history.drain(..overflow);
        }
    }

    /// The token-source report for one session, from whichever source this
    /// frontend has: the shared in-process ledger (standalone path) or the
    /// on-demand harness snapshot (attach path). `None` in attach mode while
    /// the `QueryTokenUsage` round-trip is still in flight.
    pub fn token_source_report(
        &self,
        session_id: &str,
    ) -> Option<nuo_wire::TokenSourceReport> {
        if let Some(ledger) = &self.token_ledger {
            Some(ledger.snapshot_for_session(session_id))
        } else {
            self.token_report.clone()
        }
    }

    /// Whether the Ctrl+C quit window is currently armed (a second Ctrl+C
    /// before the deadline quits). Wall-clock based; an elapsed deadline
    /// reads as disarmed.
    pub fn ctrl_c_armed(&self) -> bool {
        self.ctrl_c_armed_until
            .is_some_and(|until| std::time::Instant::now() < until)
    }

    /// Arm the Ctrl+C quit window until the given deadline, or disarm it
    /// entirely when called with `None`.
    pub fn arm_ctrl_c(&mut self, until: Option<std::time::Instant>) {
        self.ctrl_c_armed_until = until;
    }

    /// Whether the Esc interrupt window is currently armed (a second Esc
    /// before the deadline interrupts the viewed session's running round).
    /// Wall-clock based; an elapsed deadline reads as disarmed.
    pub fn esc_armed(&self) -> bool {
        self.esc_armed_until
            .is_some_and(|until| std::time::Instant::now() < until)
    }

    /// Arm the Esc interrupt window until the given deadline, or disarm it
    /// entirely when called with `None`.
    pub fn arm_esc(&mut self, until: Option<std::time::Instant>) {
        self.esc_armed_until = until;
    }

    /// Register one Esc press in the interrupt-confirmation flow: the first
    /// press arms the window (returns `false`), a second press inside it
    /// fires (returns `true` and disarms), and a press after the window has
    /// lapsed starts a fresh window instead of firing a stale confirmation.
    pub fn esc_press(&mut self) -> bool {
        if self.esc_armed() {
            self.esc_armed_until = None;
            true
        } else {
            self.esc_armed_until = Some(std::time::Instant::now() + Self::ESC_ARM_WINDOW);
            false
        }
    }

    /// Per-frame bookkeeping for the Esc interrupt window: lapse it once
    /// the wall-clock deadline passes, or immediately when the *scene-scoped*
    /// interrupt target no longer has a running round — there is nothing left
    /// to interrupt, so keeping the toast up would mislead. The target is the
    /// one the scene's Esc resolves to (ADR-0205 scene scope): the viewed
    /// session on the Conversation/Aside scenes (`running_sessions`), or the
    /// focused child's own liveness on the Subagent scene. Never the runtime's
    /// global primary-only `is_responding` flag: an aside view armed from its
    /// own running round must survive the primary being idle, and a subagent
    /// arm must lapse the moment *that child* settles even while the parent
    /// keeps running.
    pub fn tick_esc_arm(&mut self) {
        if let Some(until) = self.esc_armed_until
            && std::time::Instant::now() >= until
        {
            self.esc_armed_until = None;
        }
        if !self.esc_armed() {
            return;
        }
        let target_running = if self.current_scene() == crate::surfaces::SceneKind::TaskInspection {
            self.focused_subagent_running()
        } else {
            self.running_sessions
                .contains(self.current_session_id.as_str())
        };
        if !target_running {
            self.esc_armed_until = None;
        }
    }

    pub fn byte_cursor(&self) -> usize {
        self.input
            .char_indices()
            .map(|(i, _)| i)
            .nth(self.cursor_position)
            .unwrap_or(self.input.len())
    }

    /// Highest scroll offset the input viewport supports at the last drawn
    /// size: wrapped rows minus visible rows. `None` when no composer rect
    /// is known (nothing drawn yet, or an overlay owns the surface).
    pub fn input_scroll_max(&self) -> Option<usize> {
        let rect = self.ui.bounds(crate::ui::UiKey::Composer)?;
        let text_width = crate::composer::composer_text_width(rect.width as usize);
        let rows = crate::composer::input_row_count(&self.input, text_width, self.input.len());
        let visible = (rect.height as usize)
            .saturating_sub(crate::design::COMPOSER_VERTICAL_CHROME_ROWS as usize)
            .max(1);
        Some(rows.saturating_sub(visible))
    }

    /// Step the input viewport by `lines` wrapped rows (wheel ticks pass 4,
    /// matching the transcript's wheel cadence). Returns the new offset, or
    /// `None` when the box isn't scrollable. Manual scrolling is a *reading
    /// excursion*: the caret-follow clamp in `cursor_screen_pos` does not
    /// chase the caret until the next caret-moving key, so browsing the draft
    /// never yanks the view back.
    pub fn step_input_scroll(&mut self, up: bool, lines: usize) -> Option<usize> {
        let max = self.input_scroll_max()?;
        if max == 0 {
            return None;
        }
        let target = if up {
            self.input_scroll.saturating_sub(lines)
        } else {
            (self.input_scroll + lines).min(max)
        };
        if target != self.input_scroll {
            self.input_scroll = target;
            self.input_scroll_follow_cursor = false;
        }
        Some(self.input_scroll)
    }

    /// Whether a selection-drag pointer at row `y` should arm composer
    /// edge-autoscroll: the drag must be active with its anchor in the input
    /// box, the box must be on screen with hidden rows on the side being
    /// crossed, and the pointer must sit beyond the input's text rows — above
    /// the first text row, or at/below the last one (the panel's own chrome
    /// rows and the area clean outside the box both count as "past the edge",
    /// exactly like every GUI text surface). Returns the direction (`true` =
    /// up) or `None` when the pointer is back inside the text rows and the
    /// drag should follow it normally.
    pub fn input_drag_scroll_edge(&self, y: u16) -> Option<bool> {
        let anchored_in_input = self
            .drag
            .anchor
            .as_ref()
            .is_some_and(|a| a.message_idx == crate::render::INPUT_MSG_IDX);
        if !(self.drag.active && anchored_in_input) {
            return None;
        }
        let rect = self.ui.bounds(crate::ui::UiKey::Composer)?;
        let max = self.input_scroll_max()?;
        if max == 0 {
            return None; // every wrapped row is visible; nothing to scroll to
        }
        let text_top = rect.y + crate::design::COMPOSER_TEXT_ROW_OFFSET;
        let text_bottom_exclusive = rect.y
            + rect
                .height
                .saturating_sub(crate::design::COMPOSER_VERTICAL_CHROME_ROWS);
        if y < text_top && self.input_scroll > 0 {
            Some(true)
        } else if y >= text_bottom_exclusive && self.input_scroll < max {
            Some(false)
        } else {
            None
        }
    }

    /// Advance an armed edge-autoscroll by one wrapped row, extending the
    /// selection head to the viewport edge the scroll just exposed. Returns
    /// whether anything moved (the caller redraws only then). Called from
    /// both the pointer's move events (immediate feedback) and the event
    /// loop's heartbeat tick (so holding the pointer still at the edge keeps
    /// scrolling); lazily disarms itself if the drag is no longer active.
    pub fn step_input_drag_scroll(&mut self) -> bool {
        let Some(up) = self.input_drag_scroll else {
            return false;
        };
        if !self.drag.active {
            self.input_drag_scroll = None;
            return false;
        }
        let Some(max) = self.input_scroll_max() else {
            return false;
        };
        let target = if up {
            self.input_scroll.saturating_sub(1)
        } else {
            (self.input_scroll + 1).min(max)
        };
        if target == self.input_scroll {
            return false;
        }
        self.input_scroll = target;
        self.input_scroll_follow_cursor = false;

        // Resolve the newly-exposed viewport edge through the same wrap the
        // renderer uses, and pin the selection head to its byte — hidden rows
        // are not in the layout map, so the head cannot be resolved from the
        // pointer; the edge *is* the pointer as far as the selection cares.
        let Some(rect) = self.ui.bounds(crate::ui::UiKey::Composer) else {
            return false;
        };
        let text_width = crate::composer::composer_text_width(rect.width as usize);
        let visible = (rect.height as usize)
            .saturating_sub(crate::design::COMPOSER_VERTICAL_CHROME_ROWS as usize)
            .max(1);
        let wrapped = crate::composer::composer_wrapped(&self.input, text_width, self.input.len());
        if wrapped.is_empty() {
            return false;
        }
        let edge_row = if up {
            target.min(wrapped.len() - 1)
        } else {
            (target + visible - 1).min(wrapped.len() - 1)
        };
        let byte = if up {
            wrapped[edge_row].start_byte
        } else {
            wrapped[edge_row].end_byte.min(self.input.len())
        };
        self.drag.update_to_cursor(
            &mut self.selection,
            crate::model::layout::SemanticCursor::new(crate::render::INPUT_MSG_IDX, 0, byte),
        );
        if target == 0 || target == max {
            // The pointer is still held, but no hidden row remains in this
            // direction. Stop the animation heartbeat now; a later mouse-drag
            // report will re-arm it if geometry or direction changes.
            self.input_drag_scroll = None;
        }
        true
    }

    /// Set the logical input caret position. Physical cursor placement has one
    /// writer only: the next frame's terminal commit transaction.
    pub fn set_cursor(&mut self, pos: usize) {
        self.cursor_position = pos;
        self.input_scroll_follow_cursor = true;
    }

    /// Set the input caret to the end of `self.input` (common case after a
    /// programmatic input replacement: history navigation, modal restore,
    /// paste). Equivalent to `set_cursor(self.input.chars().count())` but
    /// reads as intent at the call site.
    pub fn set_cursor_end(&mut self) {
        let end = self.input.chars().count();
        self.set_cursor(end);
    }

    /// Whether the active selection covers a piece of the composer's text —
    /// the precondition for the caret-relay and delete-selection behaviours.
    /// Supports whole-input selections (`Block` on `INPUT_MSG_IDX`) and
    /// drag-selected ranges (`Range` on `INPUT_MSG_IDX`).
    pub fn has_input_selection(&self) -> bool {
        if !self.selection.is_active() {
            return false;
        }
        match &self.selection {
            SelectionState::Block { message_idx, .. } => {
                *message_idx == crate::render::INPUT_MSG_IDX
            }
            SelectionState::TableCell { message_idx, .. } => {
                *message_idx == crate::render::INPUT_MSG_IDX
            }
            SelectionState::Range { anchor, head } => {
                anchor.message_idx == crate::render::INPUT_MSG_IDX
                    && head.message_idx == crate::render::INPUT_MSG_IDX
            }
            SelectionState::InputRange {
                anchor_byte,
                head_byte,
            } => anchor_byte != head_byte,
            SelectionState::None => false,
        }
    }

    /// Adopt the caret to the given edge of the input selection and drop
    /// the selection, restoring the (previously hidden) caret at that edge.
    /// `Head` is the release point where the mouse drag finished, while `Tail`
    /// is the anchor point where the drag began.
    ///
    /// No-op (returns `false`) unless [`Self::has_input_selection`].
    pub fn adopt_caret_from_input_selection(&mut self, edge: SelectionEdge) -> bool {
        if !self.has_input_selection() {
            return false;
        }
        let pos = match &self.selection {
            SelectionState::Block { .. } => match edge {
                SelectionEdge::Tail => 0,
                SelectionEdge::Head => self.input.chars().count(),
            },
            SelectionState::InputRange {
                anchor_byte,
                head_byte,
            } => {
                let byte = match edge {
                    SelectionEdge::Tail => *anchor_byte,
                    SelectionEdge::Head => *head_byte,
                };
                let byte = crate::model::selection::floor_grapheme_boundary(&self.input, byte)
                    .min(self.input.len());
                self.input[..byte].chars().count()
            }
            SelectionState::Range { anchor, head } => {
                let cursor = match edge {
                    SelectionEdge::Tail => *anchor,
                    SelectionEdge::Head => *head,
                };
                let byte = crate::model::selection::floor_grapheme_boundary(
                    &self.input,
                    cursor.byte_offset,
                )
                .min(self.input.len());
                self.input[..byte].chars().count()
            }
            _ => match edge {
                SelectionEdge::Tail => 0,
                SelectionEdge::Head => self.cursor_position,
            },
        };
        self.selection = SelectionState::None;
        self.drag.cancel();
        self.set_cursor(pos.min(self.input.chars().count()));
        true
    }

    /// Whether the next direction-key press should relay from the hidden
    /// caret position instead of acting on the *visible* (stale) caret:
    /// `true` while a whole-input selection is active on the composer and
    /// the composer owns the caret. Callers run this check *after* the
    /// direction key has been mapped through `route_event` but before its
    /// cursor mutation takes effect for the user — see the event loop's key
    /// relay for the exact sequencing.
    pub fn input_selection_relays_arrows(&self) -> bool {
        self.has_input_selection() && self.caret_owner() == CaretOwner::Composer
    }

    /// Delete the composer text the active input selection covers (the standard
    /// editor behaviour: Backspace/Del over a selection replaces it).
    /// No-op (returns `false`) unless [`Self::has_input_selection`].
    pub fn delete_input_selection(&mut self) -> bool {
        if !self.has_input_selection() {
            return false;
        }
        match &self.selection {
            SelectionState::Block { message_idx, .. }
                if *message_idx == crate::render::INPUT_MSG_IDX =>
            {
                self.input.clear();
                self.selection = SelectionState::None;
                self.drag.cancel();
                self.set_cursor(0);
                true
            }
            SelectionState::InputRange {
                anchor_byte,
                head_byte,
            } => {
                let start = (*anchor_byte).min(*head_byte).min(self.input.len());
                let end = (*anchor_byte).max(*head_byte).min(self.input.len());
                if start < end {
                    self.input.replace_range(start..end, "");
                }
                let new_cursor = self.input[..start].chars().count();
                self.selection = SelectionState::None;
                self.drag.cancel();
                self.set_cursor(new_cursor);
                true
            }
            SelectionState::Range { .. } => {
                if let Some((start, end)) = self.selection.active_normalized_range() {
                    let start_byte = crate::model::selection::floor_grapheme_boundary(
                        &self.input,
                        start.byte_offset,
                    )
                    .min(self.input.len());
                    let end_byte = crate::model::selection::inclusive_grapheme_end(
                        &self.input,
                        end.byte_offset,
                    )
                    .min(self.input.len());
                    if start_byte < end_byte {
                        self.input.replace_range(start_byte..end_byte, "");
                    }
                    let new_cursor = self.input[..start_byte].chars().count();
                    self.selection = SelectionState::None;
                    self.drag.cancel();
                    self.set_cursor(new_cursor);
                    true
                } else {
                    self.selection = SelectionState::None;
                    self.drag.cancel();
                    false
                }
            }
            _ => {
                self.selection = SelectionState::None;
                self.drag.cancel();
                false
            }
        }
    }

    /// The single source of truth for which surface owns the terminal cursor
    /// this frame. See [`CaretOwner`].
    ///
    /// This is a pure function of active surface and edit mode — never of the
    /// selection, which is folded in separately by [`Self::caret_visible`].
    /// Authoritative Stage-Scene-Overlay caret ownership (ADR-0205).
    ///
    /// The input hierarchy strictly dictates which surface owns the physical cursor:
    /// 1. If an Overlay (Dialog/Sheet) is mounted in the overlay stack:
    ///    - If it accepts text input (Palette, search bar, editor form), it owns `CaretOwner::Overlay`.
    ///    - If it delegates to the composer (`HistorySearch`), it owns `CaretOwner::Composer`.
    ///    - Otherwise (browse dialogs, list navigation, delete confirmation), cursor is suppressed (`CaretOwner::None`).
    /// 2. If no overlay is mounted, the active root Scene owns the terminal:
    ///    - In `ConversationScene`, the composer owns `CaretOwner::Composer` unless a transcript step
    ///      or browse focus has blurred the composer (`CaretOwner::None`).
    ///    - Non-conversational scenes without composer claim `CaretOwner::None`.
    pub fn caret_owner(&self) -> CaretOwner {
        use crate::surfaces::{DialogKind, SheetKind};
        if self.surfaces.active_overlay().is_some() || self.active_sheet().is_some() {
            // Dialog key reference overlay suppresses caret
            if self.dialog_keys() {
                return CaretOwner::None;
            }
            // Confirmation sub-layers suppress caret
            if self.pending_provider_delete.is_some() {
                return CaretOwner::None;
            }
            // The history panel owns its own embedded query field (ADR-0035):
            // it never borrows the composer line.
            if self.active_dialog() == Some(DialogKind::HistorySearch) {
                // The panel is anchored to the composer: browsing keeps the
                // composer's normal palette (the panel is an extension of it),
                // while the search sub-layer owns the visible cursor in its own
                // embedded field.
                return if self.surfaces.dlg::<crate::surfaces::HistorySearchDialog>().search {
                    CaretOwner::Overlay
                } else if self.in_subagent_view() {
                    CaretOwner::None
                } else {
                    CaretOwner::Composer
                };
            }
            // Models and Connections search rows claim overlay caret
            if matches!(
                self.active_dialog(),
                Some(DialogKind::Models | DialogKind::Connections)
            ) {
                return if self.picker_search() {
                    CaretOwner::Overlay
                } else {
                    CaretOwner::None
                };
            }
            // Provider/Model key editor form
            if self.surfaces.contains_sheet(SheetKind::ModelEditor) {
                return if !self.editor_model_settings_only && self.editor_field == 0 {
                    CaretOwner::Overlay
                } else {
                    CaretOwner::None
                };
            }
            if self.active_sheet() == Some(crate::sheet::SheetKind::InputInjection)
                && self.surfaces.active_overlay().is_none()
            {
                return CaretOwner::Overlay;
            }
            // Unified Command Palette (Switcher) and Custom Provider forms
            if matches!(self.active_dialog(), Some(DialogKind::Switcher))
                || self.surfaces.contains_sheet(SheetKind::CustomProvider)
            {
                return CaretOwner::Overlay;
            }
            // Question Sheet's "Other" free-text entry
            if self.active_sheet() == Some(crate::sheet::SheetKind::Question)
                && self.surfaces.active_overlay().is_none()
                && self
                    .question
                    .as_ref()
                    .is_some_and(|q| q.is_other_highlighted())
            {
                return CaretOwner::Overlay;
            }
            return CaretOwner::None;
        }

        // Scene layer: the composer owns the caret unless blurred by step/browse focus
        if self.current_scene() == crate::surfaces::SceneKind::Dashboard && self.host_prompting {
            // The Dashboard's inline prompt borrows the composer buffer but
            // renders in the scene's own footer band, so the scene — not the
            // composer — owns the cursor while it is open.
            return CaretOwner::Scene;
        }
        if self.current_scene() != crate::surfaces::SceneKind::Conversation
            || self.focused_target.is_some()
            || self.transcript_focused
            || self.in_subagent_view()
        {
            CaretOwner::None
        } else {
            CaretOwner::Composer
        }
    }

    /// Whether the terminal cursor should be visible right now —
    /// [`Self::caret_owner`] plus the one extra rule that an active text
    /// selection hides the cursor (a block cursor would clash with the
    /// selection background). This is what every cursor site consults; no
    /// call site should re-derive visibility from raw fields.
    pub fn caret_visible(&self) -> bool {
        !self.selection.is_active() && self.caret_owner() != CaretOwner::None
    }

    /// Mint the correlation id for an in-flight busy-Enter steer.
    ///
    /// The insert is **transcript-owned** (ADR-0126): it becomes a
    /// `DeliveryStatus::Queued` entry the moment it is sent and never enters
    /// the outbox — so this helper only mints the correlation id the loop
    /// uses to settle that entry when the harness admits it
    /// (`UserInputInserted`) or hands it back (`UserInputUnavailable` →
    /// [`Self::requeue_dispatch`]).
    pub fn new_insert_id(&self) -> String {
        uuid::Uuid::new_v4().to_string()
    }

    /// Adopt `text` (plus its staged attachments) as the new **draft** — the
    /// live, editable, remembered input slot — entering draft mode
    /// (`history_index = None`). This is the single entry point for every
    /// path that places input into the composer as "the newest unsent input":
    /// the Phase-1 unsend restore, the Ctrl+R history insert, and the queue
    /// recall. With [`DraftAdoption::Replace`], whatever the draft held
    /// before is replaced (that content was either sent or superseded), so ↓
    /// past the newest history row later restores *this* input, never a
    /// stale one.
    ///
    /// [`DraftAdoption::OnlyIfIdle`] guards the one path that is not a user
    /// gesture: the unsend restore arrives asynchronously and must not eat a
    /// half-typed draft the user was composing while the round ran.
    ///
    /// The staged attachments are stored both in `pending_*` (what ships on
    /// send) and mirrored into the `history_draft_*` slots (what ↓ restores).
    pub fn adopt_as_draft(
        &mut self,
        text: String,
        images: Vec<ImagePart>,
        text_pastes: Vec<String>,
        policy: DraftAdoption,
    ) {
        if policy == DraftAdoption::OnlyIfIdle
            && (!self.input.is_empty()
                || !self.pending_images.is_empty()
                || !self.pending_text_pastes.is_empty())
        {
            return;
        }
        self.history_index = None;
        self.input = text;
        self.set_cursor_end();
        if !images.is_empty() {
            self.pending_images = images;
        }
        if !text_pastes.is_empty() {
            self.pending_text_pastes = text_pastes;
        }
        self.history_draft = self.input.clone();
        self.history_draft_images = self.pending_images.clone();
        self.history_draft_text_pastes = self.pending_text_pastes.clone();
        // Programmatic input replacement: latch the completion dismissal so
        // the popup doesn't flash until the next real edit.
        self.suggestion_index = None;
        self.completion_dismissed = true;
    }

    /// The currently active `ComposerExtensionKind`, if any.
    ///
    /// Floating overlays that attach directly to the Composer (History Search,
    /// Slash Commands, Mention completions) are extensions of the composer surface,
    /// borrowing its text buffer and projecting their candidates without self-contained footers.
    pub fn active_composer_extension(
        &self,
    ) -> Option<crate::composer_extension::ComposerExtensionKind> {
        if self.surfaces.active_overlay().is_none() && !self.completion_dismissed {
            match self.completion_kind() {
                crate::completion::CompletionKind::Slash => {
                    let completions = self.completions();
                    let exact_match = completions.iter().any(|c| {
                        c.replace_start == 0
                            && c.replace_end == self.input.len()
                            && c.label == self.input
                    });
                    if !completions.is_empty() && !exact_match {
                        Some(crate::composer_extension::ComposerExtensionKind::SlashCompletion)
                    } else {
                        None
                    }
                }
                crate::completion::CompletionKind::Path => {
                    let completions = self.completions();
                    let exact_match = completions.iter().any(|c| {
                        c.replace_start == 0
                            && c.replace_end == self.input.len()
                            && c.label == self.input
                    });
                    if !completions.is_empty() && !exact_match {
                        Some(crate::composer_extension::ComposerExtensionKind::PathCompletion)
                    } else {
                        None
                    }
                }
                crate::completion::CompletionKind::None => None,
            }
        } else {
            None
        }
    }

    /// The editor chain's "end at chat" teardown (ADR-0139):
    /// whatever picker the chain started from (the nav frame the opener
    /// just popped) hides with its parked composer draft handed back — the
    /// user resumes typing what they were typing before Ctrl+M. The stack
    /// is cleared: nothing between chat and here is reachable via Esc.
    pub(crate) fn restore_chat_after_editor_chain(&mut self) {
        while self.active_dialog().is_none() && self.surfaces.active_overlay().is_some() {
            self.pop_transient_surface();
        }
        if let Some(id) = self.active_dialog() {
            self.deactivate_dialog(id);
        }
        // The editor chain ends at the dialog it started from; if even that
        // is gone, reveal the full-screen scene beneath (ADR-0205).
        if self.active_dialog().is_none() && self.surfaces.active_dialog().is_none() {
            self.surfaces.dismiss_all_overlays();
        }
    }

    /// Park the composer draft into `stashed_input` and clear the live line so
    /// the input-injection modal (L3.5 β) can borrow it for free-text entry.
    /// Mirrors the stash half of the provider/history pickers.
    pub fn park_input_draft(&mut self) {
        self.injection_stashed_input = std::mem::take(&mut self.input);
        self.set_cursor(0);
        self.input_scroll = 0;
        self.suggestion_index = None;
    }

    /// Tear down the input-injection modal's borrowed state: hand the parked
    /// composer draft back. Does **not** touch the overlay stack.
    pub fn restore_input_draft(&mut self) {
        self.input = std::mem::take(&mut self.injection_stashed_input);
        self.set_cursor_end();
        self.input_scroll = 0;
        self.suggestion_index = None;
        self.modal_index = 0;
    }

    /// The active fuzzy query for the picker: the picker entity's own embedded
    /// field while its search sub-layer is active, else empty (browse mode
    /// shows every row). Never the composer line (ADR-0035).
    pub(crate) fn picker_query(&self) -> &str {
        match self.top_dialog() {
            Some(crate::surfaces::DialogKind::Models)
                if self.surfaces.dlg::<crate::surfaces::ModelsDialog>().search =>
            {
                self.surfaces.dlg::<crate::surfaces::ModelsDialog>().query.text.trim()
            }
            Some(crate::surfaces::DialogKind::Connections)
                if self.surfaces.dlg::<crate::surfaces::ConnectionsDialog>().search =>
            {
                self.surfaces.dlg::<crate::surfaces::ConnectionsDialog>().query.text.trim()
            }
            _ => "",
        }
    }
}
