//! Dialog pointer/scroll routing.

use super::entities::*;
use super::*;

#[derive(Debug, Clone, Copy)]
pub(super) enum ScrollDelta {
    Lines(isize),
    ToStart,
    ToEnd,
}

fn apply_delta(scroll: &mut usize, delta: ScrollDelta) {
    match delta {
        ScrollDelta::Lines(d) if d < 0 => {
            *scroll = scroll.saturating_sub((-d) as usize);
        }
        ScrollDelta::Lines(d) => {
            *scroll = scroll.saturating_add(d as usize);
        }
        ScrollDelta::ToStart => {
            *scroll = 0;
        }
        ScrollDelta::ToEnd => {
            *scroll = usize::MAX;
        }
    }
}

pub(super) fn scroll_delta(action: &InputAction, app: &crate::shell::App) -> Option<ScrollDelta> {
    let page_step = if app.transcript.modal_body_height > 0 {
        app.transcript.modal_body_height.saturating_sub(1).max(1) as isize
    } else {
        app.transcript.view_height.saturating_sub(1).max(1) as isize
    };
    match action {
        InputAction::ScrollUp => Some(ScrollDelta::Lines(-1)),
        InputAction::ScrollDown => Some(ScrollDelta::Lines(1)),
        InputAction::Wheel { up, .. } => Some(ScrollDelta::Lines(if *up { -1 } else { 1 })),
        InputAction::ScrollPageUp => Some(ScrollDelta::Lines(-page_step)),
        InputAction::ScrollPageDown => Some(ScrollDelta::Lines(page_step)),
        InputAction::ScrollTop => Some(ScrollDelta::ToStart),
        InputAction::ScrollBottom => Some(ScrollDelta::ToEnd),
        _ => None,
    }
}

/// Scroll one dialog entity. The type-erasing downcast cannot fail: each
/// `DialogView` implementation reports exactly one `DialogKind`.
#[allow(clippy::expect_used)]
pub(super) fn handle_dialog_scroll(view: &mut dyn DialogView, delta: ScrollDelta) -> DialogOutcome {
    if view.keys_open() {
        apply_delta(view.keys_scroll_mut(), delta);
        return DialogOutcome::Consumed;
    }
    match view.kind() {
        DialogKind::Threads => {
            let e = view
                .as_any_mut()
                .downcast_mut::<ThreadsDialog>()
                .expect("kind");
            if e.info_detail {
                apply_delta(&mut e.info_scroll, delta);
            } else {
                apply_delta(&mut e.scroll, delta);
                e.follow = false;
            }
            DialogOutcome::Consumed
        }
        DialogKind::Connections => {
            let e = view
                .as_any_mut()
                .downcast_mut::<ConnectionsDialog>()
                .expect("kind");
            if e.info_detail {
                apply_delta(&mut e.info_scroll, delta);
            } else {
                apply_delta(&mut e.scroll, delta);
                e.follow = false;
            }
            DialogOutcome::Consumed
        }
        DialogKind::Tools => {
            let e = view
                .as_any_mut()
                .downcast_mut::<ToolsDialog>()
                .expect("kind");
            if e.info_detail {
                apply_delta(&mut e.info_scroll, delta);
            } else {
                apply_delta(&mut e.scroll, delta);
                e.follow = false;
            }
            DialogOutcome::Consumed
        }
        DialogKind::Mcp => {
            let e = view.as_any_mut().downcast_mut::<McpDialog>().expect("kind");
            apply_delta(&mut e.scroll, delta);
            e.follow = false;
            DialogOutcome::Consumed
        }
        DialogKind::Skills => {
            let e = view
                .as_any_mut()
                .downcast_mut::<SkillsDialog>()
                .expect("kind");
            apply_delta(&mut e.scroll, delta);
            e.follow = false;
            DialogOutcome::Consumed
        }
        DialogKind::Models => {
            let e = view
                .as_any_mut()
                .downcast_mut::<ModelsDialog>()
                .expect("kind");
            if e.info_detail {
                apply_delta(&mut e.info_scroll, delta);
            } else {
                apply_delta(&mut e.scroll, delta);
                e.follow = false;
            }
            DialogOutcome::Consumed
        }
        DialogKind::HistorySearch => {
            let e = view
                .as_any_mut()
                .downcast_mut::<HistorySearchDialog>()
                .expect("kind");
            apply_delta(&mut e.scroll, delta);
            e.follow = false;
            DialogOutcome::Consumed
        }
        DialogKind::Queue => {
            let e = view
                .as_any_mut()
                .downcast_mut::<QueueDialog>()
                .expect("kind");
            apply_delta(&mut e.scroll, delta);
            e.follow = false;
            DialogOutcome::Consumed
        }
        DialogKind::SessionTree => {
            let e = view
                .as_any_mut()
                .downcast_mut::<SessionTreeDialog>()
                .expect("kind");
            apply_delta(&mut e.scroll, delta);
            e.follow = false;
            DialogOutcome::Consumed
        }
        DialogKind::Quotas => {
            let e = view
                .as_any_mut()
                .downcast_mut::<QuotasDialog>()
                .expect("kind");
            apply_delta(&mut e.scroll, delta);
            e.follow = false;
            DialogOutcome::Consumed
        }
        DialogKind::Permissions => {
            let e = view
                .as_any_mut()
                .downcast_mut::<PermissionsDialog>()
                .expect("kind");
            apply_delta(&mut e.scroll, delta);
            DialogOutcome::Consumed
        }
        DialogKind::UsageStats => {
            let e = view
                .as_any_mut()
                .downcast_mut::<UsageStatsDialog>()
                .expect("kind");
            apply_delta(&mut e.scroll, delta);
            DialogOutcome::Consumed
        }
        DialogKind::ThreadStats => {
            let e = view
                .as_any_mut()
                .downcast_mut::<ThreadStatsDialog>()
                .expect("kind");
            apply_delta(&mut e.scroll, delta);
            DialogOutcome::Consumed
        }
        DialogKind::SessionTrace => {
            let e = view
                .as_any_mut()
                .downcast_mut::<SessionTraceDialog>()
                .expect("kind");
            apply_delta(&mut e.scroll, delta);
            DialogOutcome::Consumed
        }
        DialogKind::Switcher => {
            let e = view
                .as_any_mut()
                .downcast_mut::<SwitcherDialog>()
                .expect("kind");
            apply_delta(&mut e.scroll, delta);
            DialogOutcome::Consumed
        }
        DialogKind::PortShare => DialogOutcome::Consumed,
        DialogKind::Help => {
            let e = view
                .as_any_mut()
                .downcast_mut::<HelpDialog>()
                .expect("kind");
            apply_delta(&mut e.scroll, delta);
            DialogOutcome::Consumed
        }
        DialogKind::Notices => {
            let e = view
                .as_any_mut()
                .downcast_mut::<NoticesDialog>()
                .expect("kind");
            apply_delta(&mut e.scroll, delta);
            DialogOutcome::Consumed
        }
    }
}

pub(super) fn dialog_body_bounds(kind: DialogKind, modal_rect: Rect) -> (u16, u16) {
    match kind {
        DialogKind::HistorySearch => (
            modal_rect.y + 2,
            modal_rect.y + modal_rect.height.saturating_sub(1),
        ),
        DialogKind::Switcher => (
            modal_rect.y + 5,
            modal_rect.y + modal_rect.height.saturating_sub(3),
        ),
        DialogKind::Notices => (
            modal_rect.y + 2,
            modal_rect.y + modal_rect.height.saturating_sub(2),
        ),
        _ => (
            modal_rect.y + 3,
            modal_rect.y + modal_rect.height.saturating_sub(3),
        ),
    }
}

/// Route a click to one dialog entity. The type-erasing downcast cannot fail:
/// each `DialogView` implementation reports exactly one `DialogKind`.
#[allow(clippy::expect_used)]
pub(super) fn handle_dialog_click(
    view: &mut dyn DialogView,
    visual_row: usize,
    app: &mut crate::shell::App,
    viewed_session_id: &str,
) -> DialogOutcome {
    match view.kind() {
        DialogKind::Threads => {
            let e = view
                .as_any_mut()
                .downcast_mut::<ThreadsDialog>()
                .expect("kind");
            if e.info_detail {
                return DialogOutcome::Consumed;
            }
            let projected = crate::shell::overlays::session::project_session_rows(
                &app.session.sessions_overview,
                Some(&e.expanded),
            );
            let visible = if app.transcript.modal_body_height > 0 {
                app.transcript.modal_body_height as usize
            } else {
                10
            };
            let (start, _) = nuo_ui_primitives::primitives::resolve_scroll(
                &mut e.scroll,
                visible,
                projected.len(),
                if e.follow { Some(e.index) } else { None },
                nuo_ui_primitives::primitives::SCROLL_EDGE_MARGIN,
            );
            let target_idx = start + visual_row;
            if target_idx < projected.len() {
                let was_selected = e.index == target_idx;
                e.index = target_idx;
                e.follow = true;
                if was_selected {
                    let target_session = projected[target_idx].session();
                    let target_id = target_session.id.clone();
                    let target_title = target_session.title.clone();
                    if target_id != viewed_session_id {
                        let is_empty = app.is_current_thread_empty();
                        let tab = crate::shell::surfaces::ClientTab::thread(
                            &target_id,
                            target_title.as_deref(),
                        );
                        if is_empty {
                            app.send_intent(nuo_protocol::AgentRequest::DeleteThread {
                                id: viewed_session_id.to_string(),
                            });
                            let active_idx = app.surfaces.surfaces.active_tab_index();
                            if let Some(existing_pos) = app
                                .surfaces
                                .surfaces
                                .tabs()
                                .iter()
                                .position(|t| match &t.kind {
                                    crate::shell::surfaces::TabKind::Thread(tid) => {
                                        tid == &target_id
                                    }
                                    _ => false,
                                })
                            {
                                app.close_tab(active_idx);
                                let new_pos = if existing_pos > active_idx {
                                    existing_pos - 1
                                } else {
                                    existing_pos
                                };
                                app.surfaces.surfaces.select_tab(new_pos);
                            } else {
                                app.replace_tab(active_idx, tab);
                            }
                        } else {
                            app.open_tab(tab);
                        }
                        app.surfaces.switch_to_target = Some(target_id);
                        app.link
                            .should_quit
                            .store(true, std::sync::atomic::Ordering::SeqCst);
                    } else {
                        return DialogOutcome::Dismiss;
                    }
                }
            }
            DialogOutcome::Consumed
        }
        DialogKind::Models => {
            let e = view
                .as_any_mut()
                .downcast_mut::<ModelsDialog>()
                .expect("kind");
            if e.info_detail {
                return DialogOutcome::Consumed;
            }
            let models = app.models_flat_filtered();
            let (geometry, _) = nuo_view::providers::models_body_lines(&models);
            let target_line = e.scroll + visual_row;
            if let Some(nuo_view::providers::ModelBodyLine::Row(row_idx)) =
                geometry.get(target_line)
            {
                if let Some(rm) = models.get(*row_idx)
                    && !rm.usable
                {
                    return DialogOutcome::Consumed;
                }
                let was_selected = e.index == *row_idx;
                e.index = *row_idx;
                e.follow = true;
                if was_selected
                    && let Some(rm) = models.get(*row_idx)
                    && rm.usable
                {
                    let ready = app
                        .surfaces
                        .key_status
                        .get(&rm.provider_id)
                        .copied()
                        .unwrap_or(true);
                    crate::shell::event_loop::actions::activate_picked_model(
                        app,
                        rm.provider_id.clone(),
                        rm.model.clone(),
                        ready,
                    );
                }
            }
            DialogOutcome::Consumed
        }
        DialogKind::Connections => {
            let e = view
                .as_any_mut()
                .downcast_mut::<ConnectionsDialog>()
                .expect("kind");
            if e.info_detail {
                return DialogOutcome::Consumed;
            }
            let providers = app.providers_filtered();
            let target_idx = e.scroll + visual_row;
            if target_idx < providers.len() {
                e.index = target_idx;
                e.follow = true;
            }
            DialogOutcome::Consumed
        }
        DialogKind::Tools => {
            let e = view
                .as_any_mut()
                .downcast_mut::<ToolsDialog>()
                .expect("kind");
            if e.info_detail {
                return DialogOutcome::Consumed;
            }
            let count = app.session_tools_len();
            let target_idx = e.scroll + visual_row;
            if target_idx < count {
                let was_selected = e.index == target_idx;
                e.index = target_idx;
                e.follow = true;
                if was_selected {
                    e.info_detail = true;
                    e.info_scroll = 0;
                }
            }
            DialogOutcome::Consumed
        }
        DialogKind::Mcp => {
            let e = view.as_any_mut().downcast_mut::<McpDialog>().expect("kind");
            let count = app
                .session
                .session_context
                .as_ref()
                .map_or(0, |s| s.mcp.len());
            let target_idx = e.scroll + visual_row;
            if target_idx < count {
                e.index = target_idx;
                e.follow = true;
            }
            DialogOutcome::Consumed
        }
        DialogKind::Skills => {
            let e = view
                .as_any_mut()
                .downcast_mut::<SkillsDialog>()
                .expect("kind");
            let count = app
                .session
                .session_context
                .as_ref()
                .map_or(0, |s| s.skills.len());
            let target_idx = e.scroll + visual_row;
            if target_idx < count {
                let was_selected = e.index == target_idx;
                e.index = target_idx;
                e.follow = true;
                if was_selected {
                    e.expanded = if e.expanded == Some(target_idx) {
                        None
                    } else {
                        Some(target_idx)
                    };
                }
            }
            DialogOutcome::Consumed
        }
        DialogKind::Permissions => {
            let e = view
                .as_any_mut()
                .downcast_mut::<PermissionsDialog>()
                .expect("kind");
            let count = app
                .session
                .session_context
                .as_ref()
                .map_or(0, |s| s.permissions.len());
            let target_idx = e.scroll + visual_row;
            if target_idx < count {
                e.index = target_idx;
            }
            DialogOutcome::Consumed
        }
        DialogKind::HistorySearch => {
            let e = view
                .as_any_mut()
                .downcast_mut::<HistorySearchDialog>()
                .expect("kind");
            let count = app.history_rows().len();
            let target_idx = e.scroll + visual_row;
            if target_idx < count {
                e.index = target_idx;
                e.follow = true;
            }
            DialogOutcome::Consumed
        }
        DialogKind::SessionTree => {
            let e = view
                .as_any_mut()
                .downcast_mut::<SessionTreeDialog>()
                .expect("kind");
            let count = crate::shell::overlays::tree::flatten_tree(&app.session.session_tree).len();
            let target_idx = e.scroll + visual_row;
            if target_idx < count {
                e.index = target_idx;
                e.follow = true;
            }
            DialogOutcome::Consumed
        }
        DialogKind::Quotas => {
            let e = view
                .as_any_mut()
                .downcast_mut::<QuotasDialog>()
                .expect("kind");
            let count = app
                .telemetry
                .provider_quotas
                .as_ref()
                .map_or(0, |q| q.entries.len());
            let target_idx = e.scroll + visual_row;
            if target_idx < count {
                let was_selected = e.index == target_idx;
                e.index = target_idx;
                e.follow = true;
                if was_selected {
                    e.expanded = if e.expanded == Some(target_idx) {
                        None
                    } else {
                        Some(target_idx)
                    };
                }
            }
            DialogOutcome::Consumed
        }
        DialogKind::Queue => {
            let e = view
                .as_any_mut()
                .downcast_mut::<QueueDialog>()
                .expect("kind");
            let count = app.pending_count(viewed_session_id);
            let target_idx = e.scroll + visual_row;
            if target_idx < count {
                e.index = target_idx;
                e.follow = true;
            }
            DialogOutcome::Consumed
        }
        DialogKind::Switcher => {
            let e = view
                .as_any_mut()
                .downcast_mut::<SwitcherDialog>()
                .expect("kind");
            let app_ctx = crate::shell::keymap::AppContext {
                has_overlay: true,
                active_dialog: app
                    .surfaces
                    .surfaces
                    .underlying_dialog()
                    .or_else(|| app.active_dialog()),
                is_responding: app.viewed_chrome().responding,
                has_selection: !matches!(app.transcript.selection, SelectionState::None),
                has_running_task: app.viewed_chrome().responding,
                has_notices: !!app.has_notices(),
                has_focused_entry: app.transcript.focus.entry().is_some(),
                queue_count: app.outbox_total(),
                has_session: app.has_session(),
                scene: app.current_scene(),
            };
            let entries = crate::shell::overlays::command_palette::filter_palette_commands(
                &e.query.text,
                &app.composer.recent_commands,
                &app_ctx,
            );
            let target_idx = e.scroll + visual_row;
            if target_idx < entries.len() {
                e.selected = target_idx;
                e.follow = true;
            }
            DialogOutcome::Consumed
        }
        DialogKind::Help => {
            let e = view
                .as_any_mut()
                .downcast_mut::<HelpDialog>()
                .expect("kind");
            let target_line = e.scroll + visual_row;
            if let Some(Some(action_kind)) = e.actions.get(target_line) {
                app.surfaces.pending_help_action = Some(*action_kind);
                return DialogOutcome::Dismiss;
            }
            DialogOutcome::Consumed
        }
        _ => DialogOutcome::Consumed,
    }
}
