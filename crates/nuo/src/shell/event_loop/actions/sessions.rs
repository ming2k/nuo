//! Session overview, thread switching, port palette, and view switcher actions.
use std::sync::atomic::Ordering;

use protocol::AgentRequest;

use crate::shell::App;
use crate::shell::clipboard_ops;
use crate::shell::input::InputAction;
use crate::shell::surfaces::DialogKind;

use super::{ActionContext, ActionFlow};

pub(super) async fn handle_session_action(
    app: &mut App,
    viewed_session_id: &str,
    ctx: &mut ActionContext<'_>,
    action: InputAction,
) -> Option<ActionFlow> {
    match action {
        InputAction::SessionSelect { forward } => {
            if let Some(mut ent) = app.surfaces.surfaces.take_active_view() {
                let _ = ent.handle_input(
                    &InputAction::SessionSelect { forward },
                    app,
                    viewed_session_id,
                );
                app.surfaces.surfaces.put_active_view(ent);
            }
            Some(ActionFlow::Handled)
        }
        InputAction::SessionActivate => {
            if let Some(req) = app.session_activate_request() {
                app.send_intent(req);
            }
            Some(ActionFlow::Handled)
        }
        InputAction::OpenSelectedSession => {
            let target = {
                let rows = crate::shell::overlays::session::project_session_rows(
                    &app.session.sessions_overview,
                    Some(
                        &app.surfaces
                            .surfaces
                            .dlg_mut::<crate::shell::surfaces::SessionsDialog>()
                            .expanded,
                    ),
                );
                rows.get(app.active_index().min(rows.len().saturating_sub(1)))
                    .map(|item| {
                        let session = item.session();
                        (session.id.clone(), session.title.clone())
                    })
            };
            if let Some((id, title)) = target {
                app.dismiss_active_dialog();
                app.set_active_index(0);
                app.surfaces.startup_overlay = crate::shell::StartupOverlay::None;

                if id == viewed_session_id {
                    if app.session.running_sessions.contains(&id) {
                        app.transcript.scroll = 0;
                    }
                } else {
                    let is_empty = app.is_current_thread_empty();
                    let tab = crate::shell::surfaces::ClientTab::thread(&id, title.as_deref());
                    if is_empty {
                        app.send_intent(AgentRequest::DeleteThread {
                            id: viewed_session_id.to_string(),
                        });
                        let active_idx = app.surfaces.surfaces.active_tab_index();
                        if let Some(existing_pos) =
                            app.surfaces
                                .surfaces
                                .tabs()
                                .iter()
                                .position(|t| match &t.kind {
                                    crate::shell::surfaces::TabKind::Thread(tid) => tid == &id,
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

                    app.surfaces.switch_to_target = Some(id);
                    app.link.should_quit.store(true, Ordering::SeqCst);
                }
            }
            Some(ActionFlow::Handled)
        }
        InputAction::DeleteSelectedSession => {
            let rows = crate::shell::overlays::session::project_session_rows(
                &app.session.sessions_overview,
                Some(
                    &app.surfaces
                        .surfaces
                        .dlg_mut::<crate::shell::surfaces::SessionsDialog>()
                        .expanded,
                ),
            );
            let idx = app.active_index().min(rows.len().saturating_sub(1));
            if let Some(item) = rows.get(idx) {
                let id = item.session().id.clone();
                app.session.sessions_overview.retain(|s| s.id != id);
                app.surfaces
                    .surfaces
                    .dlg_mut::<crate::shell::surfaces::SessionsDialog>()
                    .expanded
                    .remove(&id);
                let new_rows = crate::shell::overlays::session::project_session_rows(
                    &app.session.sessions_overview,
                    Some(
                        &app.surfaces
                            .surfaces
                            .dlg_mut::<crate::shell::surfaces::SessionsDialog>()
                            .expanded,
                    ),
                );
                app.set_active_index(app.active_index().min(new_rows.len().saturating_sub(1)));
                app.send_intent(AgentRequest::DeleteThread { id });
            }
            Some(ActionFlow::Handled)
        }
        InputAction::CreateNewSession => {
            app.surfaces.startup_overlay = crate::shell::StartupOverlay::None;
            app.dismiss_active_dialog();
            if app.is_current_thread_empty() {
                app.send_intent(AgentRequest::SlashCommand("/new".to_string()));
            } else {
                app.surfaces.switch_to_new = true;
                app.link.should_quit.store(true, Ordering::SeqCst);
            }
            Some(ActionFlow::Handled)
        }
        InputAction::OpenSessionInfo => {
            let rows = crate::shell::overlays::session::project_session_rows(
                &app.session.sessions_overview,
                Some(
                    &app.surfaces
                        .surfaces
                        .dlg_mut::<crate::shell::surfaces::SessionsDialog>()
                        .expanded,
                ),
            );
            if let Some(item) = rows.get(app.active_index().min(rows.len().saturating_sub(1))) {
                let session = item.session();
                app.surfaces
                    .surfaces
                    .dlg_mut::<crate::shell::surfaces::SessionsDialog>()
                    .info_detail = true;
                app.session.session_detail = None;
                app.surfaces
                    .surfaces
                    .dlg_mut::<crate::shell::surfaces::SessionsDialog>()
                    .info_scroll = 0;
                app.send_intent(AgentRequest::QueryThreadDetail {
                    id: session.id.clone(),
                });
            }
            Some(ActionFlow::Handled)
        }
        InputAction::ToggleSessionTimelineExpand => {
            let rows = crate::shell::overlays::session::project_session_rows(
                &app.session.sessions_overview,
                Some(
                    &app.surfaces
                        .surfaces
                        .dlg_mut::<crate::shell::surfaces::SessionsDialog>()
                        .expanded,
                ),
            );
            let idx = app.active_index().min(rows.len().saturating_sub(1));
            if let Some(item) = rows.get(idx)
                && item.child_count > 0
            {
                let id = item.session().id.clone();
                let collapsed = app
                    .surfaces
                    .surfaces
                    .dlg_mut::<crate::shell::surfaces::SessionsDialog>()
                    .expanded
                    .remove(&id);
                if !collapsed {
                    app.surfaces
                        .surfaces
                        .dlg_mut::<crate::shell::surfaces::SessionsDialog>()
                        .expanded
                        .insert(id);
                }
            }
            Some(ActionFlow::Handled)
        }
        InputAction::PaletteInsert { ch } => {
            if app.active_dialog() == Some(DialogKind::Palette) {
                app.surfaces
                    .surfaces
                    .dlg_mut::<crate::shell::surfaces::PaletteDialog>()
                    .query
                    .text
                    .push(ch);
                app.surfaces
                    .surfaces
                    .dlg_mut::<crate::shell::surfaces::PaletteDialog>()
                    .query
                    .cursor = app
                    .surfaces
                    .surfaces
                    .dlg_mut::<crate::shell::surfaces::PaletteDialog>()
                    .query
                    .text
                    .len();
                app.surfaces
                    .surfaces
                    .dlg_mut::<crate::shell::surfaces::PaletteDialog>()
                    .selected = 0;
                app.surfaces
                    .surfaces
                    .dlg_mut::<crate::shell::surfaces::PaletteDialog>()
                    .scroll = 0;
            }
            Some(ActionFlow::Handled)
        }
        InputAction::PaletteBackspace => {
            if app.active_dialog() == Some(DialogKind::Palette) {
                {
                    let q = &mut app
                        .surfaces
                        .surfaces
                        .dlg_mut::<crate::shell::surfaces::PaletteDialog>()
                        .query;
                    if !q.is_empty() {
                        let start = graticule::floor_grapheme_boundary(&q.text, q.text.len() - 1);
                        q.text.truncate(start);
                    }
                    q.cursor = q.text.len();
                }
                let d = app
                    .surfaces
                    .surfaces
                    .dlg_mut::<crate::shell::surfaces::PaletteDialog>();
                d.selected = 0;
                d.scroll = 0;
            }
            Some(ActionFlow::Handled)
        }
        InputAction::OpenPortPalette => {
            if app.active_dialog() == Some(DialogKind::PortShare) {
                app.dismiss_surface();
            } else {
                app.surfaces
                    .surfaces
                    .dlg_mut::<crate::shell::surfaces::PortShareDialog>()
                    .begin();
                app.surfaces
                    .surfaces
                    .dlg_mut::<crate::shell::surfaces::PortShareDialog>()
                    .loading = true;
                app.open_dialog(DialogKind::PortShare);
                app.send_intent(AgentRequest::QueryServerEndpoints);
            }
            Some(ActionFlow::Handled)
        }
        InputAction::TogglePortListener => {
            if app.active_dialog() == Some(DialogKind::PortShare) {
                let current_active = app
                    .surfaces
                    .surfaces
                    .dlg::<crate::shell::surfaces::PortShareDialog>()
                    .active;
                let d = app
                    .surfaces
                    .surfaces
                    .dlg_mut::<crate::shell::surfaces::PortShareDialog>();
                d.loading = true;
                d.notice = None;
                app.send_intent(AgentRequest::ToggleServerPort {
                    port: None,
                    enable: !current_active,
                });
            }
            Some(ActionFlow::Handled)
        }
        InputAction::CopyPortConnectInfo => {
            if app.active_dialog() == Some(DialogKind::PortShare) {
                let d = app
                    .surfaces
                    .surfaces
                    .dlg_mut::<crate::shell::surfaces::PortShareDialog>();
                if d.active && d.port > 0 {
                    let cmd = if let Some(token) = &d.token {
                        format!("nuo --remote 127.0.0.1:{} --token {}", d.port, token)
                    } else {
                        format!("nuo --remote 127.0.0.1:{}", d.port)
                    };
                    clipboard_ops::spawn_clipboard_copy(ctx.copy_tx, ctx.copy_pending.clone(), cmd);
                    d.notice = Some("Copied remote connect command to clipboard!".to_string());
                }
            }
            Some(ActionFlow::Handled)
        }
        InputAction::PaletteToggle => {
            if app.active_dialog() == Some(DialogKind::Palette) {
                app.dismiss_surface();
            } else if app.can_open_palette() {
                app.open_dialog(DialogKind::Palette);
                app.surfaces
                    .surfaces
                    .dlg_mut::<crate::shell::surfaces::PaletteDialog>()
                    .begin();
            }
            Some(ActionFlow::Handled)
        }
        _ => None,
    }
}
