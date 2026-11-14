//! Mouse-action handlers for the input dispatch match — selection drags,
//! block select, right-click, and hover affordance. Extracted verbatim from
//! the corresponding arms of `dispatch_action`'s match.
use crate::shell::input;
use crate::shell::interaction::{self, ClickTarget};
use crate::shell::step_interaction::StepKind;
use crate::shell::{App, CaretOwner, ProviderDeleteChoice, SelectionEdge};
use nuo_view::model::layout::{InteractiveTarget, SemanticCursor};
use nuo_view::model::selection::{CellDragInfo, SelectionState, floor_grapheme_boundary};

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
    use crate::shell::ui::UiKey;
    app.composer.input_drag_scroll = None;
    app.transcript.ui.runtime.release_pointer();
    let target = app.transcript.ui.target(x, y);

    // Dismiss open dropdown if clicking outside it
    if app.surfaces.config_dropdown.is_some() && target != Some(UiKey::ConfigDropdown) {
        app.surfaces.config_dropdown = None;
    }

    if let Some(key) = target
        && let Some(id) = app.transcript.ui.scene().id(&key)
    {
        // A non-focusable decoration cannot steal focus. Capture belongs to
        // the component that received the press and is released on unmount.
        let _ = app.transcript.ui.runtime.focus(id);
        let _ = app.transcript.ui.runtime.capture_pointer(id);
    }
    match target {
        Some(UiKey::TabBar | UiKey::SceneBar) => {
            // [INV-TAB-11] TabBar / SceneBar Container Purity: Clicks on container-level chrome
            // (empty background, margins) are inert container gestures. They cancel drags
            // and release pointer capture, but MUST NOT mutate intra-scene context (such as
            // blurring the composer, altering transcript focus, or clearing selection).
            app.transcript.drag.cancel();
            app.transcript.ui.runtime.release_pointer();
            return;
        }
        Some(UiKey::TabBarItem(index)) => {
            super::activate_tab_at_index(app, index, runtime, viewed_session_id);
            app.transcript.drag.cancel();
            app.transcript.ui.runtime.release_pointer();
            return;
        }
        Some(UiKey::TabBarPrev) => {
            let total = app.surfaces.surfaces.tabs().len().max(1);
            let prev_idx = if app.surfaces.surfaces.active_tab_index() == 0 {
                total - 1
            } else {
                app.surfaces.surfaces.active_tab_index() - 1
            };
            super::activate_tab_at_index(app, prev_idx, runtime, viewed_session_id);
            app.transcript.drag.cancel();
            app.transcript.ui.runtime.release_pointer();
            return;
        }
        Some(UiKey::TabBarNext) => {
            let next_idx = (app.surfaces.surfaces.active_tab_index() + 1)
                % app.surfaces.surfaces.tabs().len().max(1);
            super::activate_tab_at_index(app, next_idx, runtime, viewed_session_id);
            app.transcript.drag.cancel();
            app.transcript.ui.runtime.release_pointer();
            return;
        }
        Some(UiKey::TabBarOverview) => {
            if app.active_dialog() == Some(crate::shell::surfaces::DialogKind::Switcher) {
                app.dismiss_surface();
            } else if app.can_open_switcher() {
                app.open_dialog(crate::shell::surfaces::DialogKind::Switcher);
                app.surfaces
                    .surfaces
                    .dlg_mut::<crate::shell::surfaces::SwitcherDialog>()
                    .begin();
            }
            app.transcript.drag.cancel();
            app.transcript.ui.runtime.release_pointer();
            return;
        }
        Some(UiKey::SceneHistoryBack) => {
            app.tab_history_back();
            app.transcript.drag.cancel();
            app.transcript.ui.runtime.release_pointer();
            return;
        }
        Some(UiKey::SceneHistoryForward) => {
            app.tab_history_forward();
            app.transcript.drag.cancel();
            app.transcript.ui.runtime.release_pointer();
            return;
        }
        Some(UiKey::SceneBreadcrumb(target_depth)) => {
            if let Some(tab) = app.surfaces.surfaces.active_tab() {
                let current_cursor = tab.cursor;
                if target_depth < current_cursor {
                    let steps = current_cursor - target_depth;
                    for _ in 0..steps {
                        if !app.tab_history_back() {
                            break;
                        }
                    }
                } else if target_depth > current_cursor {
                    let steps = target_depth - current_cursor;
                    for _ in 0..steps {
                        if !app.tab_history_forward() {
                            break;
                        }
                    }
                }
            }
            app.transcript.drag.cancel();
            app.transcript.ui.runtime.release_pointer();
            return;
        }
        Some(UiKey::ProviderDelete) if !app.transcript.ui.contains(UiKey::ProviderDelete, x, y) => {
            app.surfaces.pending_provider_delete = None;
            app.surfaces.provider_delete_focus = ProviderDeleteChoice::default();
        }
        Some(UiKey::QuestionOption(index)) => {
            if let Some(question) = app.human.question.take() {
                app.human.question = Some(
                    question
                        .update(crate::shell::question_model::QuestionAction::Select(
                            index + 1,
                        ))
                        .0,
                );
                app.human.question_modal_follow = true;
            }
        }
        Some(UiKey::PermissionAction(index)) => {
            app.surfaces.modal_index = index;
            handle_permission_submit(app, runtime).await;
        }
        Some(UiKey::Sheet(crate::shell::sheet::SheetKind::Permission)) => {
            if let Some(cursor) =
                app.transcript.ui.document.cursor_at(x, y).filter(|cursor| {
                    cursor.message_idx == nuo_view::model::layout::MODAL_DOC_MSG_IDX
                })
            {
                app.transcript
                    .drag
                    .begin_range(&mut app.transcript.selection, cursor);
                return;
            }
        }
        Some(UiKey::SettingsCategory(index)) => {
            app.surfaces.config_category = index;
            app.surfaces.config_focus = crate::shell::overlays::ConfigFocus::Categories;
            app.surfaces.config_detail_index = 0;
            app.surfaces.config_detail_scroll = 0;
            app.surfaces.config_hover_index = None;
        }
        Some(UiKey::ConfigDropdown) => {
            if let Some((dropdown, _anchor)) = app.surfaces.config_dropdown.take() {
                if let Some(popup_area) = app.transcript.ui.bounds(UiKey::ConfigDropdown) {
                    let has_descriptions = dropdown.items.iter().any(|it| it.description.is_some());
                    let rows_per_item = if has_descriptions { 2 } else { 1 };
                    let items_top = popup_area.y + 2;
                    let items_bottom = popup_area.y + popup_area.height.saturating_sub(1);
                    if y >= items_top && y < items_bottom {
                        let rel_row = (y - items_top) as usize / rows_per_item;
                        let item_idx = dropdown.scroll_top + rel_row;
                        if item_idx < dropdown.filtered_indices.len() {
                            let raw_idx = dropdown.filtered_indices[item_idx];
                            if !dropdown.items[raw_idx].disabled {
                                let payload = dropdown.items[raw_idx].payload.clone();
                                let ctx = dropdown.context.as_deref().unwrap_or("");
                                let revision = app
                                    .surfaces
                                    .websearch_config
                                    .as_ref()
                                    .map(|config| config.revision);
                                match ctx {
                                    "websearch_provider" => {
                                        if let (Some(revision), Ok(provider)) =
                                            (revision, payload.parse())
                                        {
                                            app.send_intent(
                                                nuo_protocol::AgentRequest::UpdateWebSearchConfig(
                                                    Box::new(nuo_protocol::WebSearchConfigUpdate {
                                                        expected_revision: revision,
                                                        provider: Some(provider),
                                                        ..Default::default()
                                                    }),
                                                ),
                                            );
                                        }
                                    }
                                    "websearch_reader" => {
                                        if let (Some(revision), Ok(reader)) =
                                            (revision, payload.parse())
                                        {
                                            app.send_intent(
                                                nuo_protocol::AgentRequest::UpdateWebSearchConfig(
                                                    Box::new(nuo_protocol::WebSearchConfigUpdate {
                                                        expected_revision: revision,
                                                        reader: Some(reader),
                                                        ..Default::default()
                                                    }),
                                                ),
                                            );
                                        }
                                    }
                                    _ => {}
                                }
                                app.surfaces.config_dropdown = None;
                                return;
                            }
                        }
                    }
                }
                app.surfaces.config_dropdown = None;
            }
        }
        Some(UiKey::SettingsOption(index)) => {
            app.surfaces.config_focus = crate::shell::overlays::ConfigFocus::Detail;
            app.surfaces.config_detail_index = index;
            super::handle_config_activate(app);
        }
        Some(UiKey::SettingsRow(index)) => {
            let was_selected = app.surfaces.config_focus
                == crate::shell::overlays::ConfigFocus::Detail
                && app.surfaces.config_detail_index == index;
            app.surfaces.config_focus = crate::shell::overlays::ConfigFocus::Detail;
            app.surfaces.config_detail_index = index;
            let active_category =
                crate::shell::overlays::ConfigCategory::from_index(app.surfaces.config_category);
            // Click-to-toggle for Components, click-to-apply for Appearance,
            // or re-clicking an already-selected row activates:
            if was_selected
                || active_category == crate::shell::overlays::ConfigCategory::Components
                || active_category == crate::shell::overlays::ConfigCategory::Appearance
            {
                super::handle_config_activate(app);
            }
        }
        Some(UiKey::Overlay(_overlay)) => {
            if let Some(cursor) =
                app.transcript.ui.document.cursor_at(x, y).filter(|cursor| {
                    cursor.message_idx == nuo_view::model::layout::MODAL_DOC_MSG_IDX
                })
            {
                app.transcript
                    .drag
                    .begin_range(&mut app.transcript.selection, cursor);
                return;
            }
            if let Some(active_overlay) = app.surfaces.surfaces.active_overlay()
                && app.surfaces.click_outside_dismiss
                && !app
                    .transcript
                    .ui
                    .contains(UiKey::Overlay(active_overlay), x, y)
            {
                super::modals::handle_close_modal(app, viewed_session_id);
                return;
            }
            if let Some(active_overlay) = app.surfaces.surfaces.active_overlay()
                && app
                    .transcript
                    .ui
                    .contains(UiKey::Overlay(active_overlay), x, y)
                && let Some(mut ent) = app.surfaces.surfaces.take_active_view()
            {
                let action = input::InputAction::SelectionStart { x, y };
                let outcome = ent.handle_input(&action, app, viewed_session_id);
                app.surfaces.surfaces.put_active_view(ent);
                if outcome == crate::shell::surfaces::DialogOutcome::Dismiss {
                    app.surfaces.surfaces.pop_overlay();
                    if let Some(help_action) = app.surfaces.pending_help_action.take() {
                        execute_help_action(app, runtime, help_action, viewed_session_id).await;
                    }
                    return;
                }
                if outcome.is_consumed() {
                    return;
                }
            }
        }
        Some(UiKey::OauthUrl | UiKey::OauthCode) => {
            if let Some(cursor) =
                app.transcript.ui.document.cursor_at(x, y).filter(|cursor| {
                    cursor.message_idx == nuo_view::model::layout::MODAL_DOC_MSG_IDX
                })
            {
                app.transcript
                    .drag
                    .begin_range(&mut app.transcript.selection, cursor);
                return;
            }
            if let Some(active_overlay) = app.surfaces.surfaces.active_overlay()
                && app.surfaces.click_outside_dismiss
                && !app
                    .transcript
                    .ui
                    .contains(UiKey::Overlay(active_overlay), x, y)
            {
                super::modals::handle_close_modal(app, viewed_session_id);
            }
        }
        Some(UiKey::CompletionItem(index)) => {
            app.accept_completion(index);
            app.composer.suggestion_index = None;
            app.composer.completion_dismissed = true;
        }
        Some(UiKey::Queue) => {
            super::enter_panel(
                app,
                crate::shell::surfaces::DialogKind::Queue,
                runtime,
                viewed_session_id,
            );
        }
        Some(UiKey::Context) => {
            super::enter_panel(
                app,
                crate::shell::surfaces::DialogKind::ThreadStats,
                runtime,
                viewed_session_id,
            );
        }
        Some(UiKey::Performance) => {
            super::enter_panel(
                app,
                crate::shell::surfaces::DialogKind::SessionTrace,
                runtime,
                viewed_session_id,
            );
        }
        Some(UiKey::Connection) => {
            super::open_active_connection_detail(app, runtime, viewed_session_id);
        }
        Some(UiKey::NoticesBadge) => {
            // The badge is the notice log's persistent entry point: clicking it
            // opens the notice center dialog, exactly as the chord it advertises does.
            app.open_notice_viewer();
        }
        Some(UiKey::Sticky) => {
            if let Some(mi) = app.transcript.sticky_step {
                // Clicking the pinned summary is a pointer acceleration onto the
                // transcript plane: it highlights the entry it names
                // (ADR-0145). A sticky summary that maps to no focusable kind
                // lands on browse focus rather than leaving the plane
                // unspecified.
                let target = app.focused_messages().get(mi).and_then(|message| {
                    if message.is_reasoning() {
                        Some(InteractiveTarget::reasoning(mi))
                    } else if message.is_tool_step() || message.is_subagent_task() {
                        Some(InteractiveTarget::tool_step(mi))
                    } else {
                        None
                    }
                });
                app.transcript.focus.enter_transcript(match target {
                    Some(target) => crate::shell::focus::TranscriptFocus::Entry(target),
                    None => crate::shell::focus::TranscriptFocus::Browse,
                });
                let mut messages = std::mem::take(&mut app.transcript.messages);
                app.toggle_step_pinned(&mut messages, mi);
                app.transcript.messages = messages;
                app.transcript.layout_height_cache.clear();
                app.transcript.transcript_changed_pending = true;
            }
            app.transcript.selection = SelectionState::None;
            app.transcript.drag.cancel();
            return;
        }
        Some(UiKey::Transcript | UiKey::Composer) => {
            handle_document_press(app, runtime, x, y).await;
            return;
        }
        _ => {}
    }
    app.transcript.selection = SelectionState::None;
    app.transcript.focus.enter_composer();
    app.transcript.drag.cancel();
    app.transcript.ui.runtime.release_pointer();
}

async fn handle_document_press(app: &mut App, runtime: &UiRuntime, x: u16, y: u16) {
    {
        // Unified content hit-test cascade
        // interaction::classify_click runs the full priority
        // chain (input box → step summary → table cell →
        // generic content → gap → dead) so the event loop
        // only needs a single match.
        match interaction::classify_click(&app.transcript.ui.document, x, y) {
            // The fold block's two verb clauses. `press v view full log` is pure
            // navigation into the step's own log scene. `press y copy hidden`
            // needs the clipboard channel, which the pointer handler does not
            // hold, so it is published as a pending intent the action loop serves
            // on its next step — the same channel the keyboard uses for the same
            // verb.
            ClickTarget::FoldViewLog { message_idx } => {
                app.open_command_log_scene(message_idx);
            }
            ClickTarget::FoldCopyRows { message_idx } => {
                app.surfaces.pending_fold_copy = Some(message_idx);
                app.transcript
                    .focus
                    .enter_transcript(crate::shell::focus::TranscriptFocus::Entry(
                        InteractiveTarget::tool_step(message_idx),
                    ));
            }
            ClickTarget::InputBox { cursor } => {
                // Click inside the live input box: hand the keyboard to the
                // composer plane so the next keypress edits rather than acting
                // on a transcript entry (ADR-0145).
                app.transcript.focus.enter_composer();
                // Relay hand-off: place the (possibly hidden) caret at the
                // clicked character so any pending whole-input selection is
                // broken exactly where the user clicked, and the next
                // direction key continues from there. Without this, clicking
                // into a selected input and pressing ← would jump from the
                // stale pre-selection caret instead of the click point.
                app.adopt_caret_from_input_selection(SelectionEdge::Tail);
                app.transcript.selection = SelectionState::None;
                let byte = floor_grapheme_boundary(&app.composer.input, cursor.byte_offset);
                app.set_cursor(byte);
                // Arm a fresh drag from the click point; a plain click (no
                // drag) collapses to a zero-length range that paints nothing,
                // exactly like the previous behaviour.
                app.transcript.drag.begin_range(
                    &mut app.transcript.selection,
                    SemanticCursor::new(crate::shell::render::INPUT_MSG_IDX, 0, byte),
                );
            }
            ClickTarget::StepSummary { message_idx, kind } => {
                // Clicked a step summary: navigate into a subagent
                // task, otherwise toggle that step's disclosure.
                let mi = message_idx;
                app.transcript
                    .focus
                    .enter_transcript(crate::shell::focus::TranscriptFocus::Entry(
                        kind.focus_target(mi),
                    ));
                let mut messages = std::mem::take(&mut app.transcript.messages);
                match kind {
                    StepKind::ToolStep => {
                        let enter_id =
                            resolve_focused_mut(&mut messages, &app.transcript.focus_stack, mi)
                                .and_then(|message| {
                                    if message.is_subagent_task() {
                                        message.tool_step_call_id().map(String::from)
                                    } else {
                                        None
                                    }
                                });
                        if let Some(id) = enter_id {
                            app.transcript.messages = messages;
                            app.enter_subagent(id);
                        } else {
                            app.toggle_step_pinned(&mut messages, mi);
                            app.transcript.messages = messages;
                            app.transcript.layout_height_cache.clear();
                            app.transcript.transcript_changed_pending = true;
                        }
                    }
                    StepKind::Reasoning
                    | StepKind::Notice
                    | StepKind::CommandResult
                    | StepKind::CompactedCard => {
                        app.toggle_step_pinned(&mut messages, mi);
                        app.transcript.messages = messages;
                        app.transcript.layout_height_cache.clear();
                        app.transcript.transcript_changed_pending = true;
                    }
                }
                app.transcript.selection = SelectionState::None;
                app.transcript.drag.cancel();
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
                app.transcript.drag.begin_cell(
                    &mut app.transcript.selection,
                    cursor,
                    CellDragInfo {
                        message_idx,
                        block_idx,
                        cell_text,
                        segments: cell_segments,
                    },
                );
                app.transcript
                    .focus
                    .enter_transcript(crate::shell::focus::TranscriptFocus::Browse);
            }
            ClickTarget::Link { url, .. } => {
                // Opening a link is not an act on the transcript's content, so
                // it neither claims nor releases the plane: the plane the user
                // was in is the one the next keypress lands in.
                app.transcript.selection = SelectionState::None;
                app.transcript.drag.cancel();
                let url_for_open = url.clone();
                let mutations = runtime.mutations.clone();
                tokio::task::spawn_blocking(move || {
                    if let Err(err) = crate::shell::browser::open_browser(&url_for_open) {
                        // A failed launch is an operation receipt, not
                        // dialogue: it must not enter the conversation.
                        mutations.blocking_send(
                            crate::shell::event_loop::AppMutation::NoticeDisclosure(
                                crate::shell::app::notice_catalog::ClientNotice::LinkOpenFailed {
                                    url: url_for_open,
                                    error: err.to_string(),
                                }
                                .render(),
                            ),
                        );
                    }
                });
            }
            ClickTarget::Content { cursor } => {
                // A plain click does NOT select — it only arms a
                // drag. A zero-length range is created so an
                // immediate drag extends it normally. Message-text
                // clicks also park attention on the transcript
                // (browse focus).
                app.transcript
                    .drag
                    .begin_range(&mut app.transcript.selection, cursor);
                app.transcript
                    .focus
                    .enter_transcript(crate::shell::focus::TranscriptFocus::Browse);
            }
            ClickTarget::ContentGap => {
                // Click inside the content band but not on a
                // region: clear any step focus and selection
                // without starting a text selection. A blank-space
                // click parks attention on the transcript (
                // browse focus): the composer dims until a composer
                // click or a keystroke hands focus back.
                app.transcript.selection = SelectionState::None;
                app.transcript
                    .focus
                    .enter_transcript(crate::shell::focus::TranscriptFocus::Browse);
                app.transcript.drag.cancel();
            }
            ClickTarget::Dead => {
                // Click outside all known areas (outer gutters,
                // below content). Parks attention on browse focus so the composer
                // becomes inactive.
                app.transcript.selection = SelectionState::None;
                if app.current_scene() == crate::shell::surfaces::SceneKind::Thread
                    && app.surfaces.surfaces.active_overlay().is_none()
                    && app.active_sheet().is_none()
                {
                    app.transcript
                        .focus
                        .enter_transcript(crate::shell::focus::TranscriptFocus::Browse);
                } else {
                    app.transcript.focus.enter_composer();
                }
                app.transcript.drag.cancel();
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
    } = interaction::classify_click(&app.transcript.ui.document, x, y)
    {
        app.transcript
            .focus
            .enter_transcript(crate::shell::focus::TranscriptFocus::Entry(
                InteractiveTarget::tool_step(message_idx),
            ));
        let mut messages = std::mem::take(&mut app.transcript.messages);
        app.toggle_step_pinned(&mut messages, message_idx);
        app.transcript.messages = messages;
        app.transcript.layout_height_cache.clear();
        app.transcript.transcript_changed_pending = true;
    }
    app.transcript.selection = SelectionState::None;
    app.transcript.drag.cancel();
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
        app.composer.input_drag_scroll = Some(up);
        app.step_input_drag_scroll();
        return;
    }
    app.composer.input_drag_scroll = None;
    app.transcript.drag.update_from_point(
        &mut app.transcript.selection,
        &app.transcript.ui.document,
        x,
        y,
    );
}

/// Loop stage (input dispatch): the `SelectionEnd` arm of the action match.
pub(super) fn handle_selection_end(app: &mut App) {
    app.transcript.ui.runtime.release_pointer();
    app.transcript.drag.finish(&mut app.transcript.selection);
    // An edge-autoscroll armed by this drag stops with it: holding the
    // pointer still after release must not keep scrolling the input.
    app.composer.input_drag_scroll = None;
    // Caret relay: when the finished drag selected (part of) the live input,
    // the caret is hidden for as long as the selection paints — but its
    // position is defined to be the drag's head, the point where the mouse
    // button was released. Record that position now, so the first direction
    // key after the drag relays from the release point instead of the stale
    // pre-drag caret (the composer's scene-routed selection handler resolves
    // it when the selection is next touched).
    if let SelectionState::InputRange { head_byte, .. } = app.transcript.selection
        && app.caret_owner() == CaretOwner::Composer
    {
        let byte = floor_grapheme_boundary(&app.composer.input, head_byte);
        app.set_cursor(byte);
    }
    if let SelectionState::Range { head, .. } = app.transcript.selection
        && head.message_idx == crate::shell::render::INPUT_MSG_IDX
        && app.caret_owner() == CaretOwner::Composer
    {
        let byte = floor_grapheme_boundary(&app.composer.input, head.byte_offset);
        app.set_cursor(byte);
    }
    // Middle-click-style whole-block select on the input selects the entire
    // buffer; the caret's hidden position is defined as the end (head).
    if let SelectionState::Block {
        message_idx: crate::shell::render::INPUT_MSG_IDX,
        ..
    } = app.transcript.selection
    {
        app.set_cursor(app.composer.input.len());
    }
}

/// Loop stage (input dispatch): the `SelectBlock` arm of the action match.
pub(super) fn handle_select_block(app: &mut App, x: u16, y: u16) {
    if let Some((mi, bi)) = input::resolve_block(&app.transcript.ui.document, x, y) {
        app.transcript.selection = SelectionState::Block {
            message_idx: mi,
            block_idx: bi,
        };
        // Whole-input select (middle-click on the composer): the hidden
        // caret's position is defined as the buffer's end, so a following
        // ←/Backspace relays from there once the selection breaks.
        if mi == crate::shell::render::INPUT_MSG_IDX {
            app.set_cursor(app.composer.input.len());
        }
    }
}

/// Loop stage (input dispatch): the `Hover` arm of the action match.
pub(super) async fn handle_hover(app: &mut App, _runtime: &UiRuntime, x: u16, y: u16) {
    // Settings detail rows are pointer targets too: resting on a row lights it
    // up to the shared palette-derived hover band so the mouse and the keyboard
    // cursor read as one affordance. Recomputed on every motion — moving off a
    // row (or off the pane) clears it.
    app.surfaces.config_hover_index = match app.transcript.ui.target(x, y) {
        Some(crate::shell::ui::UiKey::SettingsOption(index))
        | Some(crate::shell::ui::UiKey::SettingsRow(index)) => Some(index),
        _ => None,
    };

    // Every step summary (tool step, subagent task, reasoning
    // trace) carries the same hover affordance. When the pointer
    // rests on one — either the inline summary or the sticky
    // pinned variant — record its message index so the next draw
    // lights it up to the intermediate hover tone; otherwise
    // clear it.
    if app.transcript.ui.target(x, y) == Some(crate::shell::ui::UiKey::Sticky) {
        if let Some(mi) = app.transcript.sticky_step {
            let is_step = app
                .transcript
                .messages
                .get(mi)
                .map(|m| m.is_reasoning() || m.is_tool_step() || m.is_subagent_task())
                .unwrap_or(false);
            app.transcript.hovered_step = is_step.then_some(mi);
        }
    } else if app.transcript.ui.target(x, y) == Some(crate::shell::ui::UiKey::Transcript) {
        app.transcript.hovered_step =
            match interaction::classify_click(&app.transcript.ui.document, x, y) {
                ClickTarget::StepSummary { message_idx, .. } => Some(message_idx),
                _ => None,
            };
    } else {
        app.transcript.hovered_step = None;
    }
}

async fn execute_help_action(
    app: &mut App,
    runtime: &UiRuntime,
    action: crate::shell::overlays::help::HelpActionKind,
    viewed_session_id: &str,
) {
    use crate::shell::overlays::help::HelpActionKind as K;
    match action {
        K::NewTab => {
            app.surfaces.startup_overlay = crate::shell::StartupOverlay::None;
            if app.is_current_thread_empty() {
                app.send_intent(nuo_protocol::AgentRequest::SlashCommand("/new".to_string()));
            } else {
                app.surfaces.switch_to_new = true;
                app.link
                    .should_quit
                    .store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }
        K::CloseTab => {
            let idx = app.surfaces.surfaces.active_tab_index();
            app.close_tab(idx);
        }
        K::Dashboard => {
            app.switch_scene(crate::shell::surfaces::SceneKind::Dashboard);
        }
        K::Settings => {
            app.switch_scene(crate::shell::surfaces::SceneKind::Settings);
        }
        K::Switcher => {
            super::enter_panel(
                app,
                crate::shell::surfaces::DialogKind::Switcher,
                runtime,
                viewed_session_id,
            );
        }
        K::Sessions => {
            super::enter_panel(
                app,
                crate::shell::surfaces::DialogKind::Threads,
                runtime,
                viewed_session_id,
            );
        }
        K::Notices => {
            app.open_notice_viewer();
        }
        K::ThreadStats => {
            super::enter_panel(
                app,
                crate::shell::surfaces::DialogKind::ThreadStats,
                runtime,
                viewed_session_id,
            );
        }
        K::Connections => {
            super::open_active_connection_detail(app, runtime, viewed_session_id);
        }
        K::CloseHelp => {}
    }
}

#[cfg(test)]
#[allow(dead_code)]
pub(crate) async fn click_at_for_test(app: &mut App, x: u16, y: u16, viewed_session_id: &str) {
    let runtime = UiRuntime::minimal_for_test();
    handle_selection_start(app, &runtime, viewed_session_id, x, y).await;
}
