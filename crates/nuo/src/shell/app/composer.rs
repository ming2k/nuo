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
    pub fn token_source_report(&self, session_id: &str) -> Option<nuo_wire::TokenSourceReport> {
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
    /// one the scene's Esc resolves to (scene scope): the viewed
    /// session on the chat scenes (`running_sessions`), or the
    /// focused child's own liveness on the Subagent scene. Never the runtime's
    /// global primary-only `is_responding` flag: a background view armed from its
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
        let target_running = if self.current_scene() == crate::shell::surfaces::SceneKind::Subagent
        {
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
        self.cursor_position.min(self.input.len())
    }

    /// The composer caret in **wire units**: a Unicode scalar (char) index into
    /// `input`.
    ///
    /// `cursor_position` is a *byte* offset — the editor motions
    /// (`crate::shell::input::readline`), the render grid
    /// ([`crate::shell::composer_grid`]), and the `@`-mention scanner all index
    /// by bytes. The control-plane contract is different:
    /// [`nuo_wire::AgentRequest::CompleteComposer`] and
    /// [`nuo_wire::InputCompletion`]'s `replace_start`/`replace_end` are
    /// **Unicode scalar indices** (`nuo-server`'s completion engine converts
    /// them with `char_to_byte`). Sending bytes where the backend reads chars
    /// silently dead-ends every completion whose prefix is not pure ASCII: the
    /// backend cannot resolve the offset, returns no items, and a mention typed
    /// mid-paragraph in (say) Chinese never opens its menu. Convert here, at the
    /// one boundary where local byte offsets meet the wire contract.
    pub fn wire_cursor(&self) -> usize {
        let mut byte = self.cursor_position.min(self.input.len());
        while byte > 0 && !self.input.is_char_boundary(byte) {
            byte -= 1;
        }
        self.input[..byte].chars().count()
    }

    /// The width the composer lays its text out at, or `0` before the first
    /// frame has published a composer rect.
    ///
    /// Caret ↑/↓ walk the *wrapped* rows, so the resolver needs the geometry
    /// the panel is actually drawn at. `0` is the "not measured yet" sentinel:
    /// the grid then falls back to [`TextGrid::unwrapped`], one row per
    /// logical line, instead of guessing a width.
    pub fn composer_grid_width(&self) -> usize {
        self.ui
            .bounds(crate::shell::ui::UiKey::Composer)
            .map_or(0, |rect| {
                crate::shell::composer::composer_text_width(rect.width as usize)
            })
    }

    /// The composer's visual text grid at the last drawn geometry. Every
    /// caller (caret motion, selection extension, scroll bookkeeping) resolves
    /// its rows here, so none of them can disagree with the renderer.
    pub fn composer_grid(&self) -> TextGrid {
        match self.composer_grid_width() {
            0 => TextGrid::unwrapped(&self.input),
            width => TextGrid::new(&self.input, width),
        }
    }

    /// Highest scroll offset the input viewport supports at the last drawn
    /// size: wrapped rows minus visible rows. `None` when no composer rect
    /// is known (nothing drawn yet, or an overlay owns the surface).
    pub fn input_scroll_max(&self) -> Option<usize> {
        let rect = self.ui.bounds(crate::shell::ui::UiKey::Composer)?;
        let text_width = crate::shell::composer::composer_text_width(rect.width as usize);
        let rows = TextGrid::new(&self.input, text_width).row_count();
        let visible = (rect.height as usize)
            .saturating_sub(nuo_ui_primitives::design::COMPOSER_VERTICAL_CHROME_ROWS as usize)
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
            .is_some_and(|a| a.message_idx == crate::shell::render::INPUT_MSG_IDX);
        if !(self.drag.active && anchored_in_input) {
            return None;
        }
        let rect = self.ui.bounds(crate::shell::ui::UiKey::Composer)?;
        let max = self.input_scroll_max()?;
        if max == 0 {
            return None; // every wrapped row is visible; nothing to scroll to
        }
        let text_top = rect.y + nuo_ui_primitives::design::COMPOSER_TEXT_ROW_OFFSET;
        let text_bottom_exclusive = rect.y
            + rect
                .height
                .saturating_sub(nuo_ui_primitives::design::COMPOSER_VERTICAL_CHROME_ROWS);
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
        let Some(rect) = self.ui.bounds(crate::shell::ui::UiKey::Composer) else {
            return false;
        };
        let text_width = crate::shell::composer::composer_text_width(rect.width as usize);
        let visible = (rect.height as usize)
            .saturating_sub(nuo_ui_primitives::design::COMPOSER_VERTICAL_CHROME_ROWS as usize)
            .max(1);
        let grid = TextGrid::new(&self.input, text_width);
        let wrapped = grid.rows();
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
            nuo_view::model::layout::SemanticCursor::new(
                crate::shell::render::INPUT_MSG_IDX,
                0,
                byte,
            ),
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
        let end = self.input.len();
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
                *message_idx == crate::shell::render::INPUT_MSG_IDX
            }
            SelectionState::TableCell { message_idx, .. } => {
                *message_idx == crate::shell::render::INPUT_MSG_IDX
            }
            SelectionState::Range { anchor, head } => {
                anchor.message_idx == crate::shell::render::INPUT_MSG_IDX
                    && head.message_idx == crate::shell::render::INPUT_MSG_IDX
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
                SelectionEdge::Head => self.input.len(),
            },
            SelectionState::InputRange {
                anchor_byte,
                head_byte,
            } => {
                let byte = match edge {
                    SelectionEdge::Tail => *anchor_byte,
                    SelectionEdge::Head => *head_byte,
                };
                nuo_view::model::selection::floor_grapheme_boundary(&self.input, byte)
                    .min(self.input.len())
            }
            SelectionState::Range { anchor, head } => {
                let cursor = match edge {
                    SelectionEdge::Tail => *anchor,
                    SelectionEdge::Head => *head,
                };
                nuo_view::model::selection::floor_grapheme_boundary(&self.input, cursor.byte_offset)
                    .min(self.input.len())
            }
            _ => match edge {
                SelectionEdge::Tail => 0,
                SelectionEdge::Head => self.cursor_position,
            },
        };
        self.selection = SelectionState::None;
        self.drag.cancel();
        self.set_cursor(pos.min(self.input.len()));
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
        self.has_input_selection() && self.is_composer_focused()
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
                if *message_idx == crate::shell::render::INPUT_MSG_IDX =>
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
                let new_cursor = start;
                self.selection = SelectionState::None;
                self.drag.cancel();
                self.set_cursor(new_cursor);
                true
            }
            SelectionState::Range { .. } => {
                if let Some((start, end)) = self.selection.active_normalized_range() {
                    let start_byte = nuo_view::model::selection::floor_grapheme_boundary(
                        &self.input,
                        start.byte_offset,
                    )
                    .min(self.input.len());
                    let end_byte = nuo_view::model::selection::inclusive_grapheme_end(
                        &self.input,
                        end.byte_offset,
                    )
                    .min(self.input.len());
                    if start_byte < end_byte {
                        self.input.replace_range(start_byte..end_byte, "");
                    }
                    let new_cursor = start_byte;
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
    /// Authoritative Stage-Scene-Overlay caret ownership.
    ///
    /// The input hierarchy strictly dictates which surface owns the physical cursor:
    /// 1. If an Overlay (Dialog/Sheet) is mounted in the overlay stack:
    ///    - If it accepts text input (Palette, search bar, editor form), it owns `CaretOwner::Overlay`.
    ///    - If it delegates to the composer (`HistorySearch`), it owns `CaretOwner::Composer`.
    ///    - Otherwise (browse dialogs, list navigation, delete confirmation), cursor is suppressed (`CaretOwner::None`).
    /// 2. If no overlay is mounted, the active root Scene owns the terminal:
    ///    - In `ThreadScene`, the composer owns `CaretOwner::Composer` unless a transcript step
    ///      or browse focus has blurred the composer (`CaretOwner::None`).
    ///    - Non-thread scenes without composer claim `CaretOwner::None`.
    pub fn caret_owner(&self) -> CaretOwner {
        use crate::shell::surfaces::{DialogKind, SheetKind};
        if self.surfaces.active_overlay().is_some() || self.active_sheet().is_some() {
            // Dialog key reference overlay suppresses caret
            if self.dialog_keys() {
                return CaretOwner::None;
            }
            // Confirmation sub-layers suppress caret
            if self.pending_provider_delete.is_some() {
                return CaretOwner::None;
            }
            // The history panel is a composer extension (ADR-0044):
            // the composer is the single input and physical caret owner.
            if self.active_dialog() == Some(DialogKind::HistorySearch) {
                return if self.in_subagent_view() {
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
            if self.active_sheet() == Some(crate::shell::sheet::SheetKind::InputInjection)
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
            if self.active_sheet() == Some(crate::shell::sheet::SheetKind::Question)
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
        if self.current_scene() == crate::shell::surfaces::SceneKind::Dashboard
            && self.host_prompting
        {
            // The Dashboard's inline prompt borrows the composer buffer but
            // renders in the scene's own footer band, so the scene — not the
            // composer — owns the cursor while it is open.
            return CaretOwner::Scene;
        }
        if self.current_scene() != crate::shell::surfaces::SceneKind::Thread
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

    /// Whether the composer currently has input focus (owns the caret).
    pub fn is_composer_focused(&self) -> bool {
        self.caret_owner() == CaretOwner::Composer
    }

    /// Whether the composer has any draft text, active history recall, or staged attachments.
    pub fn has_composer_content(&self) -> bool {
        !self.input.is_empty()
            || self.history_index.is_some()
            || !self.draft_capsule.is_empty()
            || !self.pending_images.is_empty()
            || !self.pending_text_pastes.is_empty()
    }

    /// Clear the live composer buffer, attachments, scroll offset, and active history recall state.
    pub fn clear_composer(&mut self) {
        self.input.clear();
        self.set_cursor(0);
        self.input_scroll = 0;
        self.history_index = None;
        self.clear_stashed_draft();
        self.suggestion_index = None;
        self.completion_dismissed = true;
        self.pending_images.clear();
        self.pending_text_pastes.clear();
        self.selection = SelectionState::None;
        self.arm_ctrl_c(None);
    }

    /// Mint the correlation id for an in-flight busy-Enter steer.
    ///
    /// The insert is **transcript-owned**: it becomes a
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
        self.draft_capsule = nuo_view::composer_mode::DraftCapsule::capture(
            self.input.clone(),
            self.cursor_position,
            self.pending_images.clone(),
            self.pending_text_pastes.clone(),
        );
        // Programmatic input replacement: latch the completion dismissal so
        // the popup doesn't flash until the next real edit.
        self.suggestion_index = None;
        self.completion_dismissed = true;
    }

    /// Atomic capture of current prompt and attachments into `draft_capsule` (ADR-0051 [INV-COMP-03]).
    pub fn stash_draft(&mut self) {
        self.draft_capsule = nuo_view::composer_mode::DraftCapsule::capture(
            std::mem::take(&mut self.input),
            self.cursor_position,
            std::mem::take(&mut self.pending_images),
            std::mem::take(&mut self.pending_text_pastes),
        );
        self.cursor_position = 0;
    }

    /// Atomic restoration of `draft_capsule` into live prompt and attachments (ADR-0051 [INV-COMP-03]).
    pub fn restore_stashed_draft(&mut self) {
        self.draft_capsule.restore_into(
            &mut self.input,
            &mut self.pending_images,
            &mut self.pending_text_pastes,
            &mut self.cursor_position,
        );
    }

    /// Clear the stashed draft capsule (ADR-0051 [INV-COMP-03]).
    pub fn clear_stashed_draft(&mut self) {
        self.draft_capsule.clear();
    }

    /// Whether an atomic draft capsule is currently held.
    pub fn has_stashed_draft(&self) -> bool {
        !self.draft_capsule.is_empty()
    }

    /// The currently active `ComposerExtensionKind`, if any.
    ///
    /// Floating overlays that attach directly to the Composer (History Search,
    /// Slash Commands, Mention completions) are extensions of the composer surface,
    /// borrowing its text buffer and projecting their candidates without self-contained footers.
    pub fn active_composer_extension(
        &self,
    ) -> Option<crate::shell::composer_extension::ComposerExtensionKind> {
        if self.active_dialog() == Some(crate::shell::surfaces::DialogKind::HistorySearch) {
            return Some(crate::shell::composer_extension::ComposerExtensionKind::HistorySearch);
        }
        if self.surfaces.active_overlay().is_none() && !self.completion_dismissed {
            match self.completion_kind() {
                crate::shell::completion::CompletionKind::Slash => {
                    let completions = self.completions();
                    let exact_match = completions.iter().any(|c| {
                        c.replace_start == 0
                            && c.replace_end == self.input.len()
                            && c.label == self.input
                    });
                    if !completions.is_empty() && !exact_match {
                        Some(crate::shell::composer_extension::ComposerExtensionKind::SlashCompletion)
                    } else {
                        None
                    }
                }
                crate::shell::completion::CompletionKind::Path => {
                    let completions = self.completions();
                    let exact_match = completions.iter().any(|c| {
                        c.replace_start == 0
                            && c.replace_end == self.input.len()
                            && c.label == self.input
                    });
                    if !completions.is_empty() && !exact_match {
                        Some(
                            crate::shell::composer_extension::ComposerExtensionKind::PathCompletion,
                        )
                    } else {
                        None
                    }
                }
                crate::shell::completion::CompletionKind::None => None,
            }
        } else {
            None
        }
    }

    /// Authoritative Canonical Quadrant Mode of the Composer (ADR-0051 [INV-COMP-01]).
    pub fn composer_mode(&self) -> nuo_view::composer_mode::ComposerMode {
        use nuo_view::composer_mode::{CompleteKind, ComposerMode};
        if self.active_dialog() == Some(crate::shell::surfaces::DialogKind::HistorySearch) {
            let selected_index = self
                .surfaces
                .dlg::<crate::shell::surfaces::HistorySearchDialog>()
                .index;
            return ComposerMode::Search {
                query: self.input.clone(),
                selected_index,
            };
        }
        if let Some(history_index) = self.history_index {
            return ComposerMode::Recall { history_index };
        }
        if let Some(ext) = self.active_composer_extension() {
            match ext {
                crate::shell::composer_extension::ComposerExtensionKind::SlashCompletion => {
                    return ComposerMode::Complete {
                        kind: CompleteKind::Command,
                        needle: self.input.clone(),
                        selected_index: self.suggestion_index.unwrap_or(0),
                    };
                }
                crate::shell::composer_extension::ComposerExtensionKind::PathCompletion => {
                    return ComposerMode::Complete {
                        kind: CompleteKind::Context,
                        needle: self.input.clone(),
                        selected_index: self.suggestion_index.unwrap_or(0),
                    };
                }
                crate::shell::composer_extension::ComposerExtensionKind::HistorySearch => {
                    let selected_index = self
                        .surfaces
                        .dlg::<crate::shell::surfaces::HistorySearchDialog>()
                        .index;
                    return ComposerMode::Search {
                        query: self.input.clone(),
                        selected_index,
                    };
                }
            }
        }
        ComposerMode::Edit
    }

    /// The editor chain's "end at chat" teardown:
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
        // is gone, reveal the full-screen scene beneath.
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
            Some(crate::shell::surfaces::DialogKind::Models)
                if self
                    .surfaces
                    .dlg::<crate::shell::surfaces::ModelsDialog>()
                    .search =>
            {
                self.surfaces
                    .dlg::<crate::shell::surfaces::ModelsDialog>()
                    .query
                    .text
                    .trim()
            }
            Some(crate::shell::surfaces::DialogKind::Connections)
                if self
                    .surfaces
                    .dlg::<crate::shell::surfaces::ConnectionsDialog>()
                    .search =>
            {
                self.surfaces
                    .dlg::<crate::shell::surfaces::ConnectionsDialog>()
                    .query
                    .text
                    .trim()
            }
            _ => "",
        }
    }
}
