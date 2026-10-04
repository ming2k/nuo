//! Mouse-action handlers for the input dispatch match — selection drags,
//! block select, right-click, and hover affordance. Extracted verbatim from
//! the corresponding arms of `dispatch_action`'s match.

use crate::input;
use crate::interaction::{self, ClickTarget};
use crate::model::document::{NoticeSeverity, TranscriptMessage};
use crate::model::layout::{InteractiveTarget, SemanticCursor};
use crate::model::selection::{CellDragInfo, SelectionState, floor_grapheme_boundary};
use crate::step_interaction::StepKind;
use crate::{App, CaretOwner, ProviderDeleteChoice, SelectionEdge};

use super::super::runtime::UiRuntime;
use super::super::transcript::resolve_focused_mut;
use super::modals::handle_permission_submit;

/// Loop stage (input dispatch): the `SelectionStart` arm of the action match.
pub(super) async fn handle_selection_start(
    app: &mut App,
    runtime: &UiRuntime,
    viewed_session_id: &str,
    x: u16,
    y: u16,
) {
    use crate::ui::UiKey;
    app.input_drag_scroll = None;
    app.ui.runtime.release_pointer();
    let target = app.ui.target(x, y);
    if let Some(key) = target
        && let Some(id) = app.ui.scene().id(&key)
    {
        // A non-focusable decoration cannot steal focus. Capture belongs to
        // the component that received the press and is released on unmount.
        let _ = app.ui.runtime.focus(id);
        let _ = app.ui.runtime.capture_pointer(id);
    }
    match target {
        Some(UiKey::ProviderDelete) if !app.ui.contains(UiKey::ProviderDelete, x, y) => {
            app.pending_provider_delete = None;
            app.provider_delete_focus = ProviderDeleteChoice::default();
        }
        Some(UiKey::QuestionOption(index)) => {
            if let Some(question) = app.question.take() {
                app.question = Some(
                    question
                        .update(crate::question_model::QuestionAction::Select(index + 1))
                        .0,
                );
                app.question_modal_follow = true;
            }
        }
        Some(UiKey::PermissionAction(index)) => {
            app.modal_index = index;
            handle_permission_submit(app, runtime).await;
        }
        Some(UiKey::Sheet(crate::sheet::SheetKind::Permission)) => {
            if let Some(cursor) = app
                .ui
                .document
                .cursor_at(x, y)
                .filter(|cursor| cursor.message_idx == crate::model::layout::MODAL_DOC_MSG_IDX)
            {
                app.drag.begin_range(&mut app.selection, cursor);
                return;
            }
        }
        Some(UiKey::SettingsOption(index)) => {
            app.config_focus = crate::overlays::ConfigFocus::Detail;
            app.config_detail_index = index;
        }
        Some(UiKey::Overlay(_) | UiKey::OauthUrl | UiKey::OauthCode) => {
            if let Some(cursor) = app
                .ui
                .document
                .cursor_at(x, y)
                .filter(|cursor| cursor.message_idx == crate::model::layout::MODAL_DOC_MSG_IDX)
            {
                app.drag.begin_range(&mut app.selection, cursor);
                return;
            }
            if let Some(active_overlay) = app.surfaces.active_overlay()
                && app.click_outside_dismiss
                && !app.ui.contains(UiKey::Overlay(active_overlay), x, y)
            {
                super::modals::handle_close_modal(app, viewed_session_id);
            }
        }
        Some(UiKey::CompletionItem(index)) => {
            app.accept_completion(index);
            app.suggestion_index = None;
            app.completion_dismissed = true;
        }
        Some(UiKey::Queue) => {
            super::enter_panel(
                app,
                crate::surfaces::DialogKind::Queue,
                runtime,
                viewed_session_id,
            );
        }
        Some(UiKey::Context | UiKey::Performance) => {
            super::enter_panel(
                app,
                crate::surfaces::DialogKind::Telemetry,
                runtime,
                viewed_session_id,
            );
        }
        Some(UiKey::Connection) => {
            super::open_active_connection_detail(app, runtime, viewed_session_id);
        }
        Some(UiKey::Sticky) => {
            if let Some(mi) = app.sticky_step {
                app.focused_target = app.focused_messages().get(mi).and_then(|message| {
                    if message.is_reasoning() {
                        Some(InteractiveTarget::reasoning(mi))
                    } else if message.is_tool_step() || message.is_subagent_task() {
                        Some(InteractiveTarget::tool_step(mi))
                    } else {
                        None
                    }
                });
                let mut messages = std::mem::take(&mut app.messages);
                app.toggle_step_pinned(&mut messages, mi);
                app.messages = messages;
                app.layout_height_cache.clear();
                app.transcript_changed_pending = true;
            }
            app.selection = SelectionState::None;
            app.drag.cancel();
            return;
        }
        Some(UiKey::Transcript | UiKey::Composer) => {
            handle_document_press(app, runtime, x, y).await;
            return;
        }
        _ => {}
    }
    app.selection = SelectionState::None;
    app.focused_target = None;
    app.drag.cancel();
    app.ui.runtime.release_pointer();
}

async fn handle_document_press(app: &mut App, runtime: &UiRuntime, x: u16, y: u16) {
    {
        // Unified content hit-test cascade
        // interaction::classify_click runs the full priority
        // chain (input box → step summary → table cell →
        // generic content → gap → dead) so the event loop
        // only needs a single match.
        match interaction::classify_click(&app.ui.document, x, y) {
            ClickTarget::InputBox { cursor } => {
                // Click inside the live input box: clear any
                // focused step so the next keypress edits rather
                // than acting on a step, and return keyboard
                // attention to the composer (ADR-0174).
                app.focused_target = None;
                app.transcript_focused = false;
                // Relay hand-off: place the (possibly hidden) caret at the
                // clicked character so any pending whole-input selection is
                // broken exactly where the user clicked, and the next
                // direction key continues from there. Without this, clicking
                // into a selected input and pressing ← would jump from the
                // stale pre-selection caret instead of the click point.
                app.adopt_caret_from_input_selection(SelectionEdge::Tail);
                app.selection = SelectionState::None;
                let byte = floor_grapheme_boundary(&app.input, cursor.byte_offset);
                app.set_cursor(app.input[..byte].chars().count());
                // Arm a fresh drag from the click point; a plain click (no
                // drag) collapses to a zero-length range that paints nothing,
                // exactly like the previous behaviour.
                app.drag.begin_range(
                    &mut app.selection,
                    SemanticCursor::new(crate::render::INPUT_MSG_IDX, 0, byte),
                );
            }
            ClickTarget::StepSummary { message_idx, kind } => {
                // Clicked a step summary: navigate into a subagent
                // task, otherwise toggle that step's disclosure.
                let mi = message_idx;
                app.focused_target = Some(kind.focus_target(mi));
                let mut messages = std::mem::take(&mut app.messages);
                match kind {
                    StepKind::ToolStep => {
                        let enter_id = resolve_focused_mut(&mut messages, &app.focus_stack, mi)
                            .and_then(|message| {
                                if message.is_subagent_task() {
                                    message.tool_step_call_id().map(String::from)
                                } else {
                                    None
                                }
                            });
                        if let Some(id) = enter_id {
                            app.messages = messages;
                            app.enter_subagent(id);
                        } else {
                            app.toggle_step_pinned(&mut messages, mi);
                            app.messages = messages;
                            app.layout_height_cache.clear();
                            app.transcript_changed_pending = true;
                        }
                    }
                    StepKind::Reasoning
                    | StepKind::ProviderRetry
                    | StepKind::CommandResult
                    | StepKind::Notice => {
                        app.toggle_step_pinned(&mut messages, mi);
                        app.messages = messages;
                        app.layout_height_cache.clear();
                        app.transcript_changed_pending = true;
                    }
                }
                app.selection = SelectionState::None;
                app.transcript_focused = true;
                app.drag.cancel();
            }
            ClickTarget::TableCell {
                message_idx,
                block_idx,
                cursor,
                cell_text,
                cell_segments,
                ..
            } => {
                // A cell drag is clamped to `│` boundaries: the
                // pointer may wander anywhere but the selection
                // can never cross a `│` border into an adjacent
                // cell.  Within the cell the user has free
                // substring selection — no auto-full-select.
                app.drag.begin_cell(
                    &mut app.selection,
                    cursor,
                    CellDragInfo {
                        message_idx,
                        block_idx,
                        cell_text,
                        segments: cell_segments,
                    },
                );
                app.focused_target = None;
                app.transcript_focused = true;
            }
            ClickTarget::Link { url, .. } => {
                app.selection = SelectionState::None;
                app.focused_target = None;
                app.drag.cancel();
                let url_for_open = url.clone();
                let mutations = runtime.mutations.clone();
                tokio::task::spawn_blocking(move || {
                    if let Err(err) = crate::browser::open_browser(&url_for_open) {
                        let notice = TranscriptMessage::notice(
                            NoticeSeverity::Warning,
                            format!("Failed to open link {url_for_open}: {err}"),
                        );
                        mutations.blocking_send(crate::event_loop::AppMutation::Transcript {
                            buffer: crate::event_loop::Buffer::Primary,
                            edit: crate::event_loop::TranscriptEdit::Append { message: notice },
                        });
                    }
                });
            }
            ClickTarget::Content { cursor } => {
                // A plain click does NOT select — it only arms a
                // drag. A zero-length range is created so an
                // immediate drag extends it normally. Message-text
                // clicks also park attention on the transcript
                // (ADR-0174 browse focus).
                app.drag.begin_range(&mut app.selection, cursor);
                app.focused_target = None;
                app.transcript_focused = true;
            }
            ClickTarget::ContentGap => {
                // Click inside the content band but not on a
                // region: clear any step focus and selection
                // without starting a text selection. A blank-space
                // click parks attention on the transcript (ADR-0174
                // browse focus): the composer dims until a composer
                // click or a keystroke hands focus back.
                app.selection = SelectionState::None;
                app.focused_target = None;
                app.transcript_focused = true;
                app.drag.cancel();
            }
            ClickTarget::Dead => {
                // Click outside all known areas (outer gutters,
                // below content). Fully inert.
                app.selection = SelectionState::None;
                app.focused_target = None;
                app.drag.cancel();
            }
        }
    }
}

/// Loop stage (input dispatch): the `RightClick` arm of the action match.
pub(super) async fn handle_right_click(app: &mut App, _runtime: &UiRuntime, x: u16, y: u16) {
    // Right-click on a tool-step summary toggles its inline
    // disclosure (same as left-click / Enter). For
    // permission-denied steps the inline body surfaces the
    // "Permission denied" message directly.
    if let ClickTarget::StepSummary {
        message_idx,
        kind: StepKind::ToolStep,
    } = interaction::classify_click(&app.ui.document, x, y)
    {
        app.focused_target = Some(InteractiveTarget::tool_step(message_idx));
        let mut messages = std::mem::take(&mut app.messages);
        app.toggle_step_pinned(&mut messages, message_idx);
        app.messages = messages;
        app.layout_height_cache.clear();
        app.transcript_changed_pending = true;
    }
    app.selection = SelectionState::None;
    app.drag.cancel();
}

/// Loop stage (input dispatch): the `SelectionUpdate` arm of the action match.
pub(super) fn handle_selection_update(app: &mut App, x: u16, y: u16) {
    // Edge autoscroll (the GUI-standard "drag past the edge" affordance): a
    // drag anchored in the composer whose pointer has left the input's text
    // rows drives the input viewport — and the selection head with it —
    // instead of resolving the pointer through the layout map, which only
    // knows visible rows. The event loop's heartbeat keeps stepping while the
    // pointer rests past the edge (`App::step_input_drag_scroll`).
    if let Some(up) = app.input_drag_scroll_edge(y) {
        app.input_drag_scroll = Some(up);
        app.step_input_drag_scroll();
        return;
    }
    app.input_drag_scroll = None;
    app.drag
        .update_from_point(&mut app.selection, &app.ui.document, x, y);
}

/// Loop stage (input dispatch): the `SelectionEnd` arm of the action match.
pub(super) fn handle_selection_end(app: &mut App) {
    app.ui.runtime.release_pointer();
    app.drag.finish(&mut app.selection);
    // An edge-autoscroll armed by this drag stops with it: holding the
    // pointer still after release must not keep scrolling the input.
    app.input_drag_scroll = None;
    // Caret relay: when the finished drag selected (part of) the live input,
    // the caret is hidden for as long as the selection paints — but its
    // position is defined to be the drag's head, the point where the mouse
    // button was released. Record that position now, so the first direction
    // key after the drag relays from the release point instead of the stale
    // pre-drag caret (the composer's scene-routed selection handler resolves
    // it when the selection is next touched).
    if let SelectionState::InputRange { head_byte, .. } = app.selection
        && app.caret_owner() == CaretOwner::Composer
    {
        let byte = floor_grapheme_boundary(&app.input, head_byte);
        app.set_cursor(app.input[..byte].chars().count());
    }
    if let SelectionState::Range { head, .. } = app.selection
        && head.message_idx == crate::render::INPUT_MSG_IDX
        && app.caret_owner() == CaretOwner::Composer
    {
        let byte = floor_grapheme_boundary(&app.input, head.byte_offset);
        app.set_cursor(app.input[..byte].chars().count());
    }
    // Middle-click-style whole-block select on the input selects the entire
    // buffer; the caret's hidden position is defined as the end (head).
    if let SelectionState::Block {
        message_idx: crate::render::INPUT_MSG_IDX,
        ..
    } = app.selection
    {
        app.set_cursor(app.input.chars().count());
    }
}

/// Loop stage (input dispatch): the `SelectBlock` arm of the action match.
pub(super) fn handle_select_block(app: &mut App, x: u16, y: u16) {
    if let Some((mi, bi)) = input::resolve_block(&app.ui.document, x, y) {
        app.selection = SelectionState::Block {
            message_idx: mi,
            block_idx: bi,
        };
        // Whole-input select (middle-click on the composer): the hidden
        // caret's position is defined as the buffer's end, so a following
        // ←/Backspace relays from there once the selection breaks.
        if mi == crate::render::INPUT_MSG_IDX {
            app.set_cursor(app.input.chars().count());
        }
    }
}

/// Loop stage (input dispatch): the `Hover` arm of the action match.
pub(super) async fn handle_hover(app: &mut App, _runtime: &UiRuntime, x: u16, y: u16) {
    // Every step summary (tool step, subagent task, reasoning
    // trace) carries the same hover affordance. When the pointer
    // rests on one — either the inline summary or the sticky
    // pinned variant — record its message index so the next draw
    // lights it up to the intermediate hover tone; otherwise
    // clear it.
    if app.ui.target(x, y) == Some(crate::ui::UiKey::Sticky) {
        if let Some(mi) = app.sticky_step {
            let is_step = app
                .messages
                .get(mi)
                .map(|m| m.is_reasoning() || m.is_tool_step() || m.is_subagent_task())
                .unwrap_or(false);
            app.hovered_step = is_step.then_some(mi);
        }
    } else if app.ui.target(x, y) == Some(crate::ui::UiKey::Transcript) {
        app.hovered_step = match interaction::classify_click(&app.ui.document, x, y) {
            ClickTarget::StepSummary { message_idx, .. } => Some(message_idx),
            _ => None,
        };
    } else {
        app.hovered_step = None;
    }
}
