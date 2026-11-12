//! Surface and scene navigation handlers.
use super::host;
use crate::shell::App;
use crate::shell::event_loop::runtime::UiRuntime;
use crate::shell::event_loop::sync::show_local_toast;
use nuo_protocol::AgentRequest;
use nuo_view::model::selection::SelectionState;

pub(crate) fn enter_panel(
    app: &mut App,
    id: impl Into<crate::shell::surfaces::DialogKind>,
    runtime: &UiRuntime,
    viewed_session_id: &str,
) -> bool {
    use crate::shell::surfaces::DialogKind;

    let id = id.into();
    if !app.dialog_available(id) {
        return false;
    }
    let first = app.open_dialog(id);
    app.selection = SelectionState::None;
    app.focus.browse();
    app.drag.cancel();

    if first {
        match id {
            DialogKind::Models => {
                let rows = app.models_flat_filtered();
                let index = rows
                    .iter()
                    .position(|row| {
                        row.provider_id == app.current_provider
                            && row.model == app.current_model
                            && row.usable
                    })
                    .or_else(|| rows.iter().position(|r| r.usable))
                    .unwrap_or(0);
                let m = &mut app
                    .surfaces
                    .dlg_mut::<crate::shell::surfaces::ModelsDialog>();
                m.search = false;
                m.follow = true;
                m.index = index;
                app.suggestion_index = None;
            }
            DialogKind::Connections => {
                let ranked = app.providers_filtered();
                let default_id = app.provider_picker.default_id.clone();
                let index = ranked
                    .iter()
                    .position(|row| row.id == app.current_provider)
                    .or_else(|| ranked.iter().position(|row| row.id == default_id))
                    .unwrap_or(0);
                let c = &mut app
                    .surfaces
                    .dlg_mut::<crate::shell::surfaces::ConnectionsDialog>();
                c.search = false;
                c.follow = true;
                c.index = index;
                app.suggestion_index = None;
            }
            DialogKind::HistorySearch => {
                app.surfaces
                    .dlg_mut::<crate::shell::surfaces::HistorySearchDialog>()
                    .index = 0;
                app.surfaces
                    .dlg_mut::<crate::shell::surfaces::HistorySearchDialog>()
                    .scroll = 0;
                app.surfaces
                    .dlg_mut::<crate::shell::surfaces::HistorySearchDialog>()
                    .follow = true;
            }
            _ => {}
        }
    }

    if id == DialogKind::Queue {
        // M4: the editing-safety auto-pause is mirrored to the server queue
        // authority (resume on close, via `queue_exit_session`). Ownership is
        // recorded so the exit hook releases *only* this pause: entering an
        // already-paused session needs no release, because the pause the user
        // set is not ours to undo (`[INV-OUTBOX-04]`).
        let was_paused = app.is_queue_blocked(viewed_session_id);
        app.set_queue_blocked(viewed_session_id, true);
        if !was_paused {
            app.send_intent(AgentRequest::QueuePaused {
                thread_id: viewed_session_id.to_string(),
                paused: true,
            });
        }
        app.queue_exit_session = Some((
            viewed_session_id.to_string(),
            if was_paused {
                crate::shell::app::QueuePauseOrigin::UserOwned
            } else {
                crate::shell::app::QueuePauseOrigin::ModalAutoPause
            },
        ));
    }

    let request = match id {
        DialogKind::Permissions | DialogKind::Tools | DialogKind::Mcp | DialogKind::Skills => {
            Some(AgentRequest::QueryThreadContext)
        }
        // ADR-0129 [INV-USAGE-01]: the usage report is an installation-wide
        // aggregate, so it is fetched over the session-free control plane rather
        // than this session's duplex. Always re-query: the report is a 400-day
        // historical fold, and the previous numbers stay on screen until the
        // fresh reply lands, so reopening never flashes the loading state.
        DialogKind::UsageStats => {
            host::refresh_usage(app, runtime);
            None
        }
        DialogKind::Quotas => {
            let filter = app
                .provider_quotas
                .as_ref()
                .and_then(|q| q.provider_filter.clone());
            Some(AgentRequest::QueryProviderQuotas {
                provider: filter,
                force_refresh: false,
            })
        }
        DialogKind::Models => Some(AgentRequest::QueryProviderQuotas {
            provider: None,
            force_refresh: false,
        }),
        DialogKind::ThreadStats | DialogKind::SessionTrace if app.token_ledger.is_none() => {
            if app.token_report.is_none() {
                Some(AgentRequest::QueryTokenUsage {
                    thread_id: viewed_session_id.to_string(),
                })
            } else {
                None
            }
        }
        DialogKind::Threads => Some(AgentRequest::QueryThreadsOverview),
        DialogKind::SessionTree => Some(AgentRequest::QueryThreadTree),
        _ => None,
    };
    if let Some(request) = request
        && !app.send_intent(request)
    {
        show_local_toast(
            app,
            format!("Could not refresh {}: backend disconnected.", id.label()),
            true,
            std::time::Duration::from_millis(3200),
        );
    }

    let _ = runtime;
    first
}

/// Activate a tab at `index`: updates the surfaces selection and MRU history,
/// entering the appropriate scene or arming a cross-session switch.

pub(crate) fn enter_scene(
    app: &mut App,
    scene: impl Into<crate::shell::surfaces::SceneKind>,
    runtime: &UiRuntime,
) {
    use crate::shell::surfaces::SceneKind;

    let scene = scene.into();
    let previous = app.current_scene();
    if previous != scene {
        if previous == SceneKind::Thread {
            app.checkpoint_active_tab_context();
        }
        app.leave_scene_for_navigation(previous);
    }
    match scene {
        SceneKind::Dashboard => {
            app.open_tab(crate::shell::surfaces::ClientTab::dashboard());
        }
        SceneKind::Settings => {
            app.open_tab(crate::shell::surfaces::ClientTab::settings());
        }
        _ => {
            app.switch_scene(scene);
        }
    }
    app.selection = SelectionState::None;
    app.focus.enter_composer();
    app.drag.cancel();

    let request = match scene {
        SceneKind::Settings => {
            app.config_focus = crate::shell::overlays::ConfigFocus::Categories;
            app.config_category = 0;
            app.config_hover_index = None;
            let active_category =
                crate::shell::overlays::ConfigCategory::from_index(app.config_category);
            if active_category == crate::shell::overlays::ConfigCategory::Appearance {
                app.config_detail_index = crate::shell::config::color_scheme_index(
                    &app.color_scheme,
                    if app.current_workspace.is_empty() {
                        None
                    } else {
                        Some(std::path::Path::new(&app.current_workspace))
                    },
                );
            } else {
                app.config_detail_index = 0;
            }
            app.config_scroll = 0;
            app.config_detail_scroll = 0;
            app.config_dropdown = None;
            Some(AgentRequest::QueryWebSearchConfig)
        }
        SceneKind::Dashboard => {
            app.host_modal_follow = true;
            app.host_focus = crate::shell::overlays::DashboardFocus::Detail;
            app.host_console_log.clear();
            app.host_kill_confirm = None;
            app.host_kill_confirm_id = None;
            None
        }
        // The log scene reads state the transcript already holds; entering it
        // asks the backend for nothing.
        SceneKind::CommandLog | SceneKind::Thread | SceneKind::Subagent => None,
    };
    if let Some(request) = request
        && !app.send_intent(request)
    {
        show_local_toast(
            app,
            "Could not refresh view: backend disconnected.".to_string(),
            true,
            std::time::Duration::from_millis(3200),
        );
    }
    let _ = runtime;
}
