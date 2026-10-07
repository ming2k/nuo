//! Subagent and side views: focus targets, step pinning, enter/exit transitions, sibling cycling.

use super::*;

impl App {
    /// Splice the `idx`-th live completion's label into [`App::input`] over
    /// its `[replace_start, replace_end)` byte range, landing the cursor
    /// just past the inserted text. Shared by `Tab` cycling and `Enter`
    /// commit.
    ///
    /// **Slash commands are terminal accepts.** Accepting a `/command` is a
    /// commit: no trailing space is appended, the highlight is cleared, and
    /// [`App::completion_dismissed`] is latched so the popup stays hidden
    /// until the next edit. This unifies Tab and Enter — a `/pursue ` (with
    /// the space) would immediately match the subcommand prefix and
    /// re-trigger the menu (defeating the point of accepting), and once a
    /// slash label replaces the whole input the candidate list collapses to
    /// the single exact match anyway, so cycling has nothing to cycle
    /// through. The user opts back into completion by editing the input
    /// (clearing the latch) or, for subcommand discovery, by typing a space.
    ///
    /// **`@path` mentions keep cycling.** Files splice inline, so multiple
    /// candidates survive an accept and Tab is meant to walk them; the popup
    /// therefore re-opens for path accepts and no latch is set. Directories
    /// end in `/` and also skip the trailing space so the popup re-triggers
    /// on the dir's contents.
    pub fn accept_completion(&mut self, idx: usize) {
        let completions = self.completions();
        let Some(comp) = completions.get(idx) else {
            return;
        };
        // Replacement range and inserted bytes are backend-owned completion
        // semantics. The TUI only translates the wire offsets and applies the
        // edit; it does not decide how `@` or trailing whitespace behave.
        let replace_start = comp.replace_start;
        let replace_end = comp.replace_end;
        let insert_text = &comp.insert_text;
        let mut new_input = String::with_capacity(self.input.len() + insert_text.len());
        new_input.push_str(&self.input[..replace_start]);
        new_input.push_str(insert_text);
        let cursor_byte = replace_start + insert_text.len();
        new_input.push_str(&self.input[replace_end..]);
        self.input = new_input;
        self.set_cursor(self.input[..cursor_byte].chars().count());
        // A terminal accept is a commit: exit completion so the popup does
        // not re-open on the just-spliced label (which would collapse to a
        // single exact match and, for slash commands, with a trailing space
        // fire the subcommand menu). Applies equally to Tab and Enter since
        // both route through here. Directory and namespace accepts (PathDir)
        // stay live (ADR-0256 DrillDown) so Tab/Enter keeps descending the
        // tree or exploring sub-entities.
        if !matches!(comp.kind, CompletionItemKind::PathDir) {
            self.suggestion_index = None;
            self.completion_dismissed = true;
        } else {
            self.suggestion_index = Some(0);
            self.completion_dismissed = false;
            self.backend_completions.clear();
            self.completion_response_input = None;
            self.completion_response_cursor = 0;
        }
    }

    /// Toggle the expansion of the tool step / reasoning trace at `mi`,
    /// keeping its header pinned to the screen position the user interacted with.
    ///
    /// A toggle inserts or removes the body lines that sit *below* the header,
    /// so the header's own content-line never moves. That gives a simple rule
    /// for keeping the header where the user clicked:
    ///
    /// - Visible (in-stream) header: leave `scroll` untouched and the header
    ///   stays on the same row; the body grows or shrinks beneath it.
    /// - Sticky-overlay header (its real header is scrolled off the top): point
    ///   `scroll` at the recorded header content-line so the real header lands
    ///   at row 0 where the overlay sat. The line is also recorded in
    ///   `pin_summary_line` so the per-frame clamp does not pull it back down
    ///   once the collapsed body shortens the stream.
    /// - Either way `follow_bottom` is cleared: the user is now pinning their
    ///   attention on this header, so the next frame's auto-follow must not
    ///   yank it away (this is what previously let an expand push the header
    ///   off-screen while the view was following the bottom).
    ///
    /// Returns `true` when a step was actually toggled, so callers can gate
    /// side effects like clearing the text selection.
    pub(crate) fn toggle_step_pinned(
        &mut self,
        messages: &mut [TranscriptMessage],
        mi: usize,
    ) -> bool {
        let pinned_to_top = self.sticky_step == Some(mi);
        let sticky_summary_line = self.sticky_summary_line;

        let transcript_top_y = self
            .ui
            .document
            .transcript_content_rect()
            .map(|r| r.y)
            .unwrap_or(0);
        let prev_region = self.ui.document.first_region_for_message(mi);
        let summary_screen_y = prev_region.map(|r| r.rect.y);
        let msg_line_index = summary_screen_y
            .map(|y| self.scroll as usize + (y.saturating_sub(transcript_top_y) as usize));

        let toggled = resolve_focused_mut(messages, &self.focus_stack, mi).and_then(|message| {
            use crate::model::interactive::InteractiveEntry;
            if message.toggle_expanded() {
                message.is_expanded()
            } else {
                None
            }
        });

        let Some(newly_expanded) = toggled else {
            return false;
        };

        self.follow_bottom = false;

        // `[tui] expand_auto_scroll` (default off): a toggle is a read
        // interaction, so by default the scroll offset is the user's and
        // stays put — the card grows or shrinks in place. Only the sticky
        // header's collapse still re-anchors, because that overlay's row
        // must land where the summary it covered sits. The settle request
        // latches either way: the toggle changed the stream's height, so the
        // clamp must validate the (untouched) offset against the *new*
        // measurement — a hard collapse can shrink the tail below it.
        if !self.expand_auto_scroll {
            if !newly_expanded && pinned_to_top {
                if let Some(summary_line) = sticky_summary_line {
                    self.scroll = summary_line.min(u16::MAX as usize) as u16;
                    self.pin_summary_line = Some(summary_line);
                }
            } else {
                self.pin_summary_line = None;
            }
            self.scroll_settle_pending = true;
            return true;
        }

        if newly_expanded {
            // When expanding, if the summary line was not already at the top of the viewport,
            // scroll down so that the summary line shifts up toward the top of the viewport (row 0 or 1),
            // giving maximum vertical space for the newly revealed body content to be visible.
            if let Some(y) = summary_screen_y
                && let Some(line_idx) = msg_line_index
            {
                let rel_y = y.saturating_sub(transcript_top_y);
                if rel_y > 1 {
                    self.scroll = line_idx.saturating_sub(1).min(u16::MAX as usize) as u16;
                }
            }
            self.pin_summary_line = None;
        } else if pinned_to_top {
            if let Some(summary_line) = sticky_summary_line {
                self.scroll = summary_line.min(u16::MAX as usize) as u16;
                self.pin_summary_line = Some(summary_line);
            }
        } else if let Some(line_idx) = msg_line_index {
            // If collapsing a step that was scrolled above the viewport, keep the collapsed summary visible
            if line_idx < self.scroll as usize {
                self.scroll = line_idx.min(u16::MAX as usize) as u16;
                self.pin_summary_line = Some(line_idx);
            } else {
                self.pin_summary_line = None;
            }
        } else {
            self.pin_summary_line = None;
        }

        // The toggle changed the transcript's height, so the scroll target
        // computed above is only valid against the *new* layout — which does
        // not exist until the next frame renders. Latch the settle request so
        // the event loop stages that frame (measure first, paint the final
        // offset second) instead of painting an intermediate viewport that
        // the post-draw clamp then has to correct.
        self.scroll_settle_pending = true;

        true
    }

    /// Dispatch a key event to the focused transcript component (stack-top focus).
    pub(crate) fn dispatch_focused_target_key(
        &mut self,
        key: crate::keymap::Key,
    ) -> crate::model::interactive::EntryKeyOutcome {
        use crate::model::interactive::{EntryKeyOutcome, InteractiveEntry};
        let Some(target) = self.focused_target else {
            return EntryKeyOutcome::Unhandled;
        };

        let mut messages = std::mem::take(&mut self.messages);
        let outcome = if let Some(message) =
            resolve_focused_mut(&mut messages, &self.focus_stack, target.message_idx)
        {
            message.handle_focused_key(key)
        } else {
            EntryKeyOutcome::Unhandled
        };

        self.messages = messages;

        match &outcome {
            EntryKeyOutcome::EnterSubagent(id) => {
                let id = id.clone();
                self.enter_subagent(id);
            }
            EntryKeyOutcome::Handled { changed } => {
                if *changed {
                    self.follow_bottom = false;
                    self.scroll_settle_pending = true;
                    self.layout_height_cache.clear();
                    self.transcript_changed_pending = true;
                    self.selection = SelectionState::None;
                }
            }
            EntryKeyOutcome::Unhandled => {}
        }

        outcome
    }

    pub(crate) fn visible_interactive_targets(&self) -> Vec<InteractiveTarget> {
        use crate::model::interactive::InteractiveEntry;
        use crate::model::layout::InteractiveTargetKind;
        let mut targets = self.ui.document.interactive_targets();
        if let Some(message_idx) = self.sticky_step
            && let Some(message) = self.focused_messages().get(message_idx)
            && message.is_focusable()
            && let Some(kind) = message.target_kind()
        {
            let target = match kind {
                InteractiveTargetKind::ToolStep => InteractiveTarget::tool_step(message_idx),
                InteractiveTargetKind::Reasoning => InteractiveTarget::reasoning(message_idx),
                InteractiveTargetKind::Notice => InteractiveTarget::notice(message_idx),
                InteractiveTargetKind::CommandResult => {
                    InteractiveTarget::command_result(message_idx)
                }
                InteractiveTargetKind::CompactedCard => {
                    InteractiveTarget::compacted_card(message_idx)
                }
            };
            if !targets.contains(&target) {
                targets.insert(0, target);
            }
        }
        targets
    }

    pub(crate) fn retain_visible_focused_target(&mut self) {
        if self.surfaces.active_overlay().is_some() || self.active_sheet().is_some() {
            self.focused_target = None;
            return;
        }
        if let Some(target) = self.focused_target
            && !self.visible_interactive_targets().contains(&target)
        {
            self.focused_target = None;
        }
    }

    pub(crate) fn focus_interactive_target(&mut self, direction: i8) {
        let targets = self.visible_interactive_targets();
        if targets.is_empty() {
            self.focused_target = None;
            return;
        }

        let current = self
            .focused_target
            .and_then(|target| targets.iter().position(|candidate| *candidate == target));
        let next = match (current, direction < 0) {
            (Some(0), true) => {
                if self.scroll > 0 {
                    self.scroll = self.scroll.saturating_sub(4);
                    self.follow_bottom = false;
                    self.pin_summary_line = None;
                    return;
                } else {
                    targets.len() - 1
                }
            }
            (Some(idx), true) => idx - 1,
            (Some(idx), false) => {
                if idx == targets.len() - 1 {
                    if self.scroll < self.max_scroll {
                        self.scroll = self.scroll.saturating_add(4).min(self.max_scroll);
                        if self.scroll >= self.max_scroll {
                            self.follow_bottom = true;
                        }
                        self.pin_summary_line = None;
                        return;
                    } else {
                        self.focused_target = None;
                        self.transcript_focused = false;
                        self.selection = SelectionState::None;
                        self.drag.cancel();
                        return;
                    }
                } else {
                    idx + 1
                }
            }
            (None, true) => targets.len() - 1,
            (None, false) => 0,
        };

        self.focused_target = Some(targets[next]);
        self.selection = SelectionState::None;
        self.drag.cancel();
    }

    /// Whether the view is currently zoomed into a subagent task.
    /// Whether the view is currently zoomed into a subagent task. Derived
    /// from the router (ADR-0141), not from zoom-stack emptiness: a
    /// dashboard opened over the zoom keeps the zoom alive underneath.
    pub fn in_subagent_view(&self) -> bool {
        self.current_scene() == crate::surfaces::SceneKind::TaskInspection
    }

    /// The parent tool-call id of the subagent the zoom is focused on — the top
    /// of the focus stack, which is also the addressable identity the
    /// `SubagentRegistry` keys the live child by. `None` when not zoomed.
    pub fn focused_subagent_call_id(&self) -> Option<&str> {
        self.focus_stack.last().map(|frame| frame.call_id.as_str())
    }

    /// Whether the focused subagent task's own round is still live — the
    /// scene-local liveness that gates the Subagent scene's `Esc` interrupt
    /// (ADR-0205). Distinct from the primary's `is_responding`: a finished
    /// child under a still-running parent reports `false`, so the zoom never
    /// advertises (or fires) a primary interrupt. Derived from the step's
    /// status, not a session-scoped mirror, because a subagent has no entry in
    /// `running_sessions` (it runs *inside* the parent's round).
    pub fn focused_subagent_running(&self) -> bool {
        let Some(call_id) = self.focused_subagent_call_id() else {
            return false;
        };
        self.messages.iter().any(|message| {
            message.is_subagent_task()
                && message.tool_step_call_id() == Some(call_id)
                && message.tool_step_status() == Some(crate::model::document::ToolStepStatus::Running)
        })
    }

    /// The message slice currently in view: the `/btw` side transcript when
    /// the side view is active (ADR-0017), the focused subagent task's child
    /// messages when zoomed, or the root conversation otherwise.
    pub fn focused_messages(&self) -> &[TranscriptMessage] {
        if self.in_side_view {
            return &self.side_messages;
        }
        let Some(frame) = self.focus_stack.last() else {
            return &self.messages;
        };
        self.messages
            .iter()
            .find_map(|message| {
                if message.is_subagent_task()
                    && message.tool_step_call_id() == Some(frame.call_id.as_str())
                {
                    message.subagent_children()
                } else {
                    None
                }
            })
            .unwrap_or(&[])
    }

    /// Zoom into a subagent task's child messages. The zoom frame (call id +
    /// saved scroll) stays on `App` as data; the surface is the router's
    /// `SceneKind::TaskInspection` (ADR-0205).
    pub fn enter_subagent(&mut self, call_id: String) {
        let saved_scroll = ScrollSnapshot {
            offset: self.scroll,
            follow_bottom: self.follow_bottom,
        };
        self.focus_stack.push(ZoomFrame {
            call_id,
            saved_scroll,
        });
        if self.current_scene() != crate::surfaces::SceneKind::TaskInspection {
            self.surfaces
                .switch_scene(crate::surfaces::SceneKind::TaskInspection);
        }
        self.reset_view_state();
    }

    /// Return from the current subagent view to its parent. Returns true if a
    /// frame was actually popped. When the last frame pops, the surface
    /// leaves `SceneKind::TaskInspection` through the router's return path
    /// (which also drains a destination scene opened over the zoom, e.g. the
    /// dashboard).
    pub fn exit_subagent(&mut self) -> bool {
        if let Some(frame) = self.focus_stack.pop() {
            self.reset_view_state();
            self.scroll = frame.saved_scroll.offset;
            self.follow_bottom = frame.saved_scroll.follow_bottom;
            if self.focus_stack.is_empty() && self.in_subagent_view() {
                self.surfaces.back_scene();
            }
            true
        } else {
            false
        }
    }

    /// Enter the `/btw` aside view (ADR-0017, ADR-0103). The side transcript
    /// ([`App::side_messages`]) becomes the viewed stream and the aside page
    /// header reports the primary session's coarse status. The buffer itself
    /// was already back-filled from `SideViewOpened`'s payload by the
    /// listener (ADR-0103 §6), so entering never clears it. Reuses the
    /// subagent zoom's `reset_view_state` so the swap feels identical to
    /// focusing a task step.
    pub fn enter_side_view(&mut self, side_id: String) {
        self.side_session_id = Some(side_id.clone());
        // The surface is the router's `SceneKind::Aside` (ADR-0205); the
        // flag below remains as cheap payload for the input context and
        // tests.
        self.in_side_view = true;
        if self.current_scene() != crate::surfaces::SceneKind::Aside {
            self.surfaces
                .switch_scene(crate::surfaces::SceneKind::Aside);
        }
        self.parent_status = ParentStatus::Idle;
        // An armed Esc confirmation is view-scoped: entering the aside must
        // not inherit the primary's arm (a second Esc here would otherwise
        // fire the *aside's* interrupt off a confirmation aimed at the
        // primary's round).
        self.esc_armed_until = None;
        // View-scoped chrome (the aside-view activity-bar fix): snapshot the
        // primary's live chrome, then swap the displayed chrome to the
        // aside's own `SessionChrome` entry. A primary round still streaming
        // in the background keeps its activity text, elapsed timer, and
        // counters parked in `saved_primary_chrome`; the aside view shows
        // only the aside's state — typically idle on entry ("new aside, no
        // round"), or streaming if re-entering a running aside.
        //
        // The snapshot is taken only when none is parked: jumping between
        // asides (A → B, or re-entering A) must not re-snapshot, because the
        // displayed chrome at that moment is the *previous aside's* —
        // overwriting would silently destroy the primary's parked state.
        if self.saved_primary_chrome.is_none() {
            self.saved_primary_chrome = Some(SessionChrome {
                phase: self.phase.clone(),
                responding: self.round_started_at.is_some() || self.phase.is_some(),
                round_count: self.round_count,
                current_turn: self.current_turn,
                round_started_at: self.round_started_at,
                can_retry: self.loop_status.is_idle() && self.harness_retry_pending,
                last_turn_performance: self
                    .session_chrome
                    .get(&self.current_session_id)
                    .and_then(|chrome| chrome.last_turn_performance),
                // Deliberately empty: the primary's setback clause is *not* a
                // displayed-mirror slot. It stays in `App::provider_retry`
                // (the primary's own, `viewed_chrome`'s primary branch reads
                // it there), so leaving the aside view has nothing to restore
                // and entering one cannot clobber it (ADR-0235).
                transport_setback: None,
            });
        }
        if let Some(chrome) = self.session_chrome.get(&side_id).cloned() {
            self.apply_chrome(&chrome);
        } else {
            // First entry: the aside has no chrome history yet — a fresh,
            // idle surface. Clearing rather than inheriting is the point.
            //
            // Written straight into the displayed mirror rather than through
            // `set_phase`: the primary's setback clause is not part of what a
            // view swap displays (ADR-0235), so a fresh aside showing an idle
            // bar must not retire the primary's live countdown.
            self.phase = None;
            self.round_started_at = None;
            self.round_count = 0;
            self.current_turn = 0;
        }
        self.reset_view_state();
    }

    /// Leave the `/btw` aside view and return to the primary transcript
    /// (ADR-0103). Detach is non-destructive: the aside keeps running and its
    /// buffer is **retained** (clipped out of view), so re-entering shows the
    /// full history without a refetch. The aside session stays live on the
    /// harness side until explicitly closed.
    pub fn exit_side_view(&mut self) {
        // Restore the primary's parked chrome (the aside-view activity-bar
        // fix): whatever the primary was doing when the user entered the
        // aside — idle, or a round still streaming in the background — its
        // activity bar, elapsed timer, and counters come back exactly as
        // they were. Without this, exiting into a running primary would show
        // the aside's (or a cleared) bar until the next primary event.
        if let Some(primary) = self.saved_primary_chrome.take() {
            self.apply_chrome(&primary);
        } else {
            // No snapshot exists only in a legacy in-process state that
            // predates the snapshot write; clear to a neutral surface and
            // let the next frame's per-session bookkeeping rebuild it.
            self.set_phase(None);
            self.round_started_at = None;
        }
        self.in_side_view = false;
        self.side_session_id = None;
        if self.current_scene() == crate::surfaces::SceneKind::Aside {
            self.surfaces.reset_to_conversation();
        }
        // Dropping any armed Esc confirmation is part of leaving: the arm
        // targeted the aside's round, and a carried arm would fire the
        // *primary's* interrupt on the next Esc. Covers the Ctrl+C detach
        // and the `SideViewSignal::Closed` backstop alike.
        self.esc_armed_until = None;
        self.reset_view_state();
    }

    /// Cycle to the previous (`dir < 0`) or next (`dir > 0`) sibling subagent
    /// task at the current focus level. No-op when not in a subagent view or
    /// when there are no siblings.
    pub fn cycle_sibling(&mut self, dir: i8) {
        let Some(current) = self.focus_stack.last() else {
            return;
        };
        let current_id = current.call_id.clone();
        let task_ids: Vec<String> = self
            .messages
            .iter()
            .filter_map(|message| {
                if message.is_subagent_task() {
                    message.tool_step_call_id().map(String::from)
                } else {
                    None
                }
            })
            .collect();
        let Some(idx) = task_ids.iter().position(|id| *id == current_id) else {
            return;
        };
        if task_ids.len() < 2 {
            return;
        }
        let n = task_ids.len() as isize;
        let next = ((idx as isize + dir as isize).rem_euclid(n)) as usize;
        if let Some(frame) = self.focus_stack.last_mut() {
            frame.call_id = task_ids[next].clone();
        }
        self.reset_view_state();
    }

    /// Number of selectable rows in the Tools modal — the tool list, the
    /// only interactive surface. Used to clamp the Up/Down selection cursor.
    pub fn session_tools_len(&self) -> usize {
        self.session_context
            .as_ref()
            .map(|s| s.tools.len())
            .unwrap_or(0)
    }

    /// Build the mutation request implied by toggling the selected tool in the
    /// Tools modal, or `None` when there is no snapshot or the selection
    /// is out of range. The harness applies it and replies with a fresh
    /// snapshot that re-renders the modal.
    pub fn session_activate_request(&self) -> Option<AgentRequest> {
        let tool = self
            .session_context
            .as_ref()?
            .tools
            .get(self.active_index())?;
        Some(AgentRequest::ToggleTool {
            name: tool.name.clone(),
            enabled: !tool.enabled,
        })
    }
}
