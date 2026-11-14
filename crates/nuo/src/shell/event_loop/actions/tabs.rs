//! Tab management handlers (activation, close).
use super::ActionFlow;
use super::navigation::enter_scene;
use crate::shell::App;
use crate::shell::event_loop::runtime::UiRuntime;
use nuo_view::model::selection::SelectionState;
use std::sync::atomic::Ordering;

pub(crate) fn activate_tab_at_index(
    app: &mut App,
    index: usize,
    runtime: &UiRuntime,
    viewed_session_id: &str,
) -> ActionFlow {
    let current_idx = app.surfaces.surfaces.active_tab_index();
    if index == current_idx {
        // [INV-TAB-11] Idempotent container gesture: Re-selecting the already active tab
        // is an inert container operation that preserves the active scene context.
        //
        // It still *repaints*. A tab click resolves against the presented
        // (hit-test) scene, and `active_tab_index()` is live state: if the two
        // ever drift — anything that moves the surface selection without
        // publishing a frame — the click lands on the tab the user sees as
        // inactive while app state already considers it active. Returning
        // without a repaint would then leave the stale highlight on screen and
        // swallow every further click on that tab, i.e. a permanently dead
        // tab. Painting on the idempotent path re-synchronises the two.
        runtime.dirty.store(true, Ordering::SeqCst);
        runtime.dirty_notify.notify_waiters();
        return ActionFlow::Handled;
    }

    if let Some(target_tab) = app.surfaces.surfaces.tabs().get(index).cloned() {
        // Checkpoint departing tab context (ADR-0091 [INV-TAB-12])
        app.checkpoint_active_tab_context();

        let was_in_thread = app.current_scene() == crate::shell::surfaces::SceneKind::Thread;
        app.surfaces.surfaces.select_tab(index);
        match &target_tab.kind {
            crate::shell::surfaces::TabKind::Thread(id) => {
                if id != viewed_session_id {
                    app.surfaces.switch_to_target = Some(id.clone());
                    app.link.should_quit.store(true, Ordering::SeqCst);
                    return ActionFlow::Exit;
                } else {
                    if !was_in_thread {
                        enter_scene(app, crate::shell::surfaces::SceneKind::Thread, runtime);
                    }
                    app.restore_active_tab_context();
                    app.transcript.selection = SelectionState::None;
                }
            }
            crate::shell::surfaces::TabKind::Dashboard => {
                enter_scene(app, crate::shell::surfaces::SceneKind::Dashboard, runtime);
            }
            crate::shell::surfaces::TabKind::Settings => {
                enter_scene(app, crate::shell::surfaces::SceneKind::Settings, runtime);
            }
        }
    }

    runtime.dirty.store(true, Ordering::SeqCst);
    runtime.dirty_notify.notify_waiters();

    ActionFlow::Handled
}

/// Close active tab and automatically activate the next tab according to MRU history,
/// re-attaching or refreshing the focused scene.
pub(crate) fn handle_close_tab(
    app: &mut App,
    runtime: &UiRuntime,
    viewed_session_id: &str,
) -> ActionFlow {
    let active_idx = app.surfaces.surfaces.active_tab_index();
    app.close_tab(active_idx);
    if app.surfaces.surfaces.tabs().is_empty() {
        tracing::info!(reason = "close_last_tab", "client detaching");
        return ActionFlow::Exit;
    }

    if let Some(target_tab) = app.surfaces.surfaces.active_tab().cloned() {
        match &target_tab.kind {
            crate::shell::surfaces::TabKind::Thread(id) => {
                if id != viewed_session_id {
                    app.surfaces.switch_to_target = Some(id.clone());
                    app.link.should_quit.store(true, Ordering::SeqCst);
                    return ActionFlow::Exit;
                } else {
                    enter_scene(app, crate::shell::surfaces::SceneKind::Thread, runtime);
                    app.restore_active_tab_context();
                    app.transcript.selection = SelectionState::None;
                }
            }
            crate::shell::surfaces::TabKind::Dashboard => {
                enter_scene(app, crate::shell::surfaces::SceneKind::Dashboard, runtime);
            }
            crate::shell::surfaces::TabKind::Settings => {
                enter_scene(app, crate::shell::surfaces::SceneKind::Settings, runtime);
            }
        }
    }

    runtime.dirty.store(true, Ordering::SeqCst);
    runtime.dirty_notify.notify_waiters();

    ActionFlow::Handled
}
