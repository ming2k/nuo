//! Command palette execution (RFC-0005 §2.7) and the registry executor.
//!
//! [`run_selected`] implements no verb: it resolves the highlighted entry from
//! the same projection the palette drew, pops the palette, and hands the
//! entry's command to [`execute_command_by_id`] or its action back to the
//! ordinary dispatcher (`[INV-PAL-CTX-03]`).
use super::commands::handle_esc_interrupt_with_runtime;
use super::config::open_active_connection_detail;
use super::navigation::{enter_panel, enter_scene};
use super::tabs::handle_close_tab;
use super::{ActionContext, ActionFlow, commands, modals};
use crate::shell::App;
use crate::shell::clipboard_ops;
use crate::shell::event_loop::sync::show_local_toast;
use protocol::AgentRequest;
use std::sync::atomic::Ordering;

/// Run the palette's highlighted entry.
///
/// A blocked entry explains itself and leaves the palette open: Enter on it is
/// a question ("why can't I?"), and the answer is the reason, not silence.
pub(crate) async fn run_selected<W: std::io::Write>(
    app: &mut App,
    terminal: &mut graticule::Terminal<W>,
    ctx: &mut ActionContext<'_>,
) -> ActionFlow {
    use crate::shell::keymap::Availability;
    use crate::shell::palette::Effect;
    use crate::shell::surfaces::{DialogKind, PaletteDialog};

    if app.active_dialog() != Some(DialogKind::Palette) {
        return ActionFlow::Handled;
    }
    let dialog = app.surfaces.surfaces.dlg::<PaletteDialog>();
    let (query, selected) = (dialog.query.text.clone(), dialog.selected);
    let projection = crate::shell::palette::project(app, &query);
    let Some(entry) = projection.selected(selected).map(|r| r.entry.clone()) else {
        return ActionFlow::Handled;
    };
    if let Availability::Unavailable(reason) = entry.availability {
        show_local_toast(
            app,
            format!("{}: {reason}", entry.title),
            true,
            std::time::Duration::from_millis(2500),
        );
        return ActionFlow::Handled;
    }
    // Pop first, so the surface under the palette is the active one again and
    // the entry acts on it exactly as its key would.
    app.pop_transient_surface();
    app.surfaces.palette_recents.touch(&entry.key);
    match entry.effect {
        Effect::Command(id) => execute_command_by_id(app, ctx, id).await,
        Effect::Action(action) => {
            Box::pin(super::dispatch_action(app, terminal, action, ctx)).await
        }
    }
}

fn handle_tab_command(
    app: &mut App,
    runtime: &crate::shell::event_loop::UiRuntime,
    viewed_session_id: &str,
    cmd_id: crate::shell::keymap::CommandId,
) -> ActionFlow {
    use crate::shell::keymap::CommandId;
    match cmd_id {
        CommandId::Quit => {
            // ADR-0038 [INV-ACTION-02]: Quitting the TUI client is strictly a non-destructive Detach.
            tracing::info!(reason = "command_quit", "client detaching");
            ActionFlow::Exit
        }
        CommandId::CloseTab => handle_close_tab(app, runtime, viewed_session_id),
        CommandId::NewTab => {
            app.surfaces.startup_overlay = crate::shell::StartupOverlay::None;
            app.dismiss_active_dialog();
            if app.is_current_thread_empty() {
                app.send_intent(AgentRequest::SlashCommand("/new".to_string()));
            } else {
                app.surfaces.switch_to_new = true;
                app.link.should_quit.store(true, Ordering::SeqCst);
            }
            ActionFlow::Handled
        }
        _ => unreachable!("handle_tab_command is only called for tab commands"),
    }
}

pub(crate) async fn execute_command_by_id(
    app: &mut App,
    ctx: &ActionContext<'_>,
    cmd_id: crate::shell::keymap::CommandId,
) -> ActionFlow {
    let runtime = ctx.runtime;
    let viewed_session_id = ctx.viewed_session_id;
    let copy_tx = ctx.copy_tx;
    let copy_pending = ctx.copy_pending;
    use crate::shell::keymap::CommandId;
    match cmd_id {
        CommandId::CommandPalette => {}
        CommandId::CancelOrBack => {
            // The palette-invoked twin of the Esc chord, and therefore the
            // same scope: dismiss the overlay if one is up, else step back one
            // level *inside* the Scene. It never navigates between Scenes — and
            // since ADR-0145 it no longer crosses focus planes either: a
            // running round is still interruptible from either plane, and the
            // plane is the user's, not the chord's.
            if app.surfaces.surfaces.active_overlay().is_some() {
                modals::handle_close_modal(app, viewed_session_id);
            } else if app.has_settled_background_tasks() {
                app.dismiss_settled_background_tasks();
            } else {
                app.scene_back();
            }
        }
        CommandId::InterruptTask => {
            handle_esc_interrupt_with_runtime(app, runtime, commands::InterruptTarget::Primary)
                .await;
        }
        CommandId::Quit | CommandId::CloseTab | CommandId::NewTab => {
            return handle_tab_command(app, runtime, viewed_session_id, cmd_id);
        }
        CommandId::KillThread => {
            // ADR-0038: Explicit kill terminates the thread on the server.
            app.send_intent(AgentRequest::EndThread);
            return ActionFlow::Exit;
        }
        CommandId::CopyRecentDiagnostics => {
            // The escape hatch for everything that never had (or no longer has)
            // a transcript row: transient bubbles, failed local actions, the
            // "could not reach the backend" family. Rendered through the same
            // diagnostic shape so one paste carries the whole sequence.
            match app.render_notices(Some(viewed_session_id)) {
                Some(text) => {
                    clipboard_ops::spawn_clipboard_copy(copy_tx, copy_pending.clone(), text);
                    app.surfaces.copy_toast_message = "notices copied".to_string();
                    app.surfaces.copy_toast_failed = false;
                    app.surfaces.copy_toast_until =
                        Some(std::time::Instant::now() + std::time::Duration::from_millis(1500));
                }
                None => clipboard_ops::show_copy_failure(app, "no notices retained yet"),
            }
        }
        CommandId::CopySelection => {
            // The palette entry resolves through the same rule as the chord it
            // mirrors: one command, one meaning, whichever surface fired it.
            match crate::shell::copy::CopyRequest::for_current_focus(app) {
                Some(request) => {
                    clipboard_ops::copy(app, request, viewed_session_id, copy_tx, copy_pending)
                }
                None => clipboard_ops::show_copy_failure(
                    app,
                    crate::shell::copy::CopyError::NoSelection.message(),
                ),
            }
        }
        CommandId::SendPrompt => {
            let text = std::mem::take(&mut app.composer.input);
            app.set_cursor(0);
            commands::handle_send_chat(app, runtime, viewed_session_id, text).await;
        }
        CommandId::QueueFollowUp => {
            let text = std::mem::take(&mut app.composer.input);
            app.set_cursor(0);
            commands::handle_queue_follow_up(app, runtime, viewed_session_id, text).await;
        }
        CommandId::SteerImmediate => {
            let text = std::mem::take(&mut app.composer.input);
            app.set_cursor(0);
            commands::handle_send_steer(app, runtime, viewed_session_id, text).await;
        }
        CommandId::ToggleSendMode => {
            app.composer.composer_send_mode = match app.composer.composer_send_mode {
                crate::shell::app::ComposerSendMode::Steer => {
                    crate::shell::app::ComposerSendMode::FollowUp
                }
                crate::shell::app::ComposerSendMode::FollowUp => {
                    crate::shell::app::ComposerSendMode::Steer
                }
            };
        }
        CommandId::HistorySearch => {
            if !app.is_composer_focused() {
                return ActionFlow::Handled;
            }
            enter_panel(
                app,
                crate::shell::surfaces::DialogKind::HistorySearch,
                runtime,
                viewed_session_id,
            );
        }
        CommandId::NavigateSession => {
            enter_scene(app, crate::shell::surfaces::SceneKind::Thread, runtime);
        }
        CommandId::NavigateDashboard => {
            enter_scene(app, crate::shell::surfaces::SceneKind::Dashboard, runtime);
        }
        CommandId::NavigateSettings => {
            enter_scene(app, crate::shell::surfaces::SceneKind::Settings, runtime);
        }
        CommandId::OpenNotices => {
            app.toggle_notice_viewer();
        }
        CommandId::OpenQueue => {
            enter_panel(
                app,
                crate::shell::surfaces::DialogKind::Queue,
                runtime,
                viewed_session_id,
            );
        }
        CommandId::OpenThreadStats => {
            enter_panel(
                app,
                crate::shell::surfaces::DialogKind::ThreadStats,
                runtime,
                viewed_session_id,
            );
        }
        CommandId::OpenSessionTrace => {
            enter_panel(
                app,
                crate::shell::surfaces::DialogKind::SessionTrace,
                runtime,
                viewed_session_id,
            );
        }
        CommandId::OpenModels => {
            enter_panel(
                app,
                crate::shell::surfaces::DialogKind::Models,
                runtime,
                viewed_session_id,
            );
        }
        CommandId::OpenConnections => {
            enter_panel(
                app,
                crate::shell::surfaces::DialogKind::Connections,
                runtime,
                viewed_session_id,
            );
        }
        CommandId::OpenActiveConnectionDetail => {
            open_active_connection_detail(app, runtime, viewed_session_id);
        }
        CommandId::OpenPortShare => {
            if app.active_dialog() == Some(crate::shell::surfaces::DialogKind::PortShare) {
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
                app.open_dialog(crate::shell::surfaces::DialogKind::PortShare);
                app.send_intent(protocol::AgentRequest::QueryServerEndpoints);
            }
        }
        CommandId::OpenTools => {
            enter_panel(
                app,
                crate::shell::surfaces::DialogKind::Tools,
                runtime,
                viewed_session_id,
            );
        }
        CommandId::OpenMcp => {
            enter_panel(
                app,
                crate::shell::surfaces::DialogKind::Mcp,
                runtime,
                viewed_session_id,
            );
        }
        CommandId::OpenSkills => {
            enter_panel(
                app,
                crate::shell::surfaces::DialogKind::Skills,
                runtime,
                viewed_session_id,
            );
        }
        CommandId::OpenPermissions => {
            enter_panel(
                app,
                crate::shell::surfaces::DialogKind::Permissions,
                runtime,
                viewed_session_id,
            );
        }
        CommandId::OpenUsage => {
            enter_panel(
                app,
                crate::shell::surfaces::DialogKind::UsageStats,
                runtime,
                viewed_session_id,
            );
        }
        CommandId::OpenQuotas => {
            enter_panel(
                app,
                crate::shell::surfaces::DialogKind::Quotas,
                runtime,
                viewed_session_id,
            );
        }
        CommandId::OpenTree => {
            enter_panel(
                app,
                crate::shell::surfaces::DialogKind::SessionTree,
                runtime,
                viewed_session_id,
            );
        }
        CommandId::ForkThread => {
            app.send_intent(protocol::AgentRequest::SlashCommand("/fork".to_string()));
        }
        CommandId::OpenSessions => {
            enter_panel(
                app,
                crate::shell::surfaces::DialogKind::Threads,
                runtime,
                viewed_session_id,
            );
        }
        CommandId::ToggleQueueBlock => {
            let sid = viewed_session_id.to_string();
            let paused = !app.is_queue_blocked(&sid);
            app.set_queue_blocked(&sid, paused);
            // The `/queue block` slash command is the *same* verb as the
            // modal's `Ctrl+P`; without the intent it only repainted the local
            // latch, so the toast said "paused" while the driver kept shipping
            // the queue. The verb is the authority (M4).
            app.send_intent(AgentRequest::QueuePaused {
                thread_id: sid,
                paused,
            });
            show_local_toast(
                app,
                if paused {
                    "Queue paused"
                } else {
                    "Queue resumed"
                },
                false,
                std::time::Duration::from_millis(2000),
            );
        }
        CommandId::ClearQueue => {
            // Scoped to the viewed session and mirrored to the server:
            // `pending_dispatch.clear()` was global (dropping every other
            // session's local rows) *and* invisible to the driver, which kept
            // the messages queued and sent them at the next round boundary —
            // then restored the rows with the next snapshot.
            //
            // The two row classes need two verbs. `QueueClear` empties the
            // driver's follow-up queue; a live `Steering` row is not in that
            // queue at all, so clearing the list without cancelling it would
            // leave the steer to land *after* the user emptied the queue — the
            // one outcome "clear" most obviously promises not to happen. Its
            // restore is suppressed: a clear consumes the drafts, it does not
            // hand them back.
            let sid = viewed_session_id.to_string();
            let steers: Vec<String> = app
                .composer
                .pending_dispatch
                .iter()
                .filter(|item| {
                    item.session_id == sid
                        && item.state == crate::shell::app::QueuedDispatchState::Steering
                })
                .map(|item| item.id.clone())
                .collect();
            for input_id in steers {
                app.send_intent(AgentRequest::CancelSteer {
                    thread_id: sid.clone(),
                    input_id: input_id.clone(),
                });
                app.composer
                    .suppress_steer_restore
                    .insert((sid.clone(), input_id));
            }
            app.composer
                .pending_dispatch
                .retain(|item| item.session_id != sid);
            app.send_intent(AgentRequest::QueueClear { thread_id: sid });
            show_local_toast(
                app,
                "Queue cleared",
                false,
                std::time::Duration::from_millis(2000),
            );
        }
        CommandId::PermissionsClearAll => {
            let revocations = match app.session.session_context.as_ref() {
                Some(ctx) => ctx
                    .permissions
                    .iter()
                    .map(|perm| AgentRequest::RevokePermission {
                        tool: perm.tool.clone(),
                        scope: perm.scope.clone(),
                    })
                    .collect::<Vec<_>>(),
                None => Vec::new(),
            };
            for revocation in revocations {
                app.send_intent(revocation);
            }
            show_local_toast(
                app,
                "All permissions revoked",
                false,
                std::time::Duration::from_millis(2000),
            );
        }
        CommandId::ProviderAddConnection => {
            app.open_preset_chooser();
        }
        CommandId::RedrawScreen => {
            show_local_toast(
                app,
                "Screen redrawn",
                false,
                std::time::Duration::from_millis(1000),
            );
        }
        _ => {}
    }
    ActionFlow::Handled
}
