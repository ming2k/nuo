//! Tab management handlers (activation, close, create).
use super::ActionFlow;
use super::navigation::enter_scene;
use crate::shell::App;
use crate::shell::event_loop::runtime::UiRuntime;
use std::sync::atomic::Ordering;
use view::model::selection::SelectionState;

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
                // ADR-0251 `[INV-MUX-02]`: switch resident workspaces in O(1)
                // over the multiplexed connection without tearing down `App`
                // or reconnecting. Enter the Thread scene first so its scene
                // entry hook never clobbers the restored focus plane.
                if !was_in_thread {
                    enter_scene(app, crate::shell::surfaces::SceneKind::Thread, runtime);
                }
                if id != viewed_session_id {
                    app.activate_thread_workspace(id);
                } else {
                    app.restore_active_tab_context();
                }
                app.transcript.selection = SelectionState::None;
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

/// Close the tab at `index` and automatically activate the next tab according to
/// MRU history, re-attaching or refreshing the focused scene.
///
/// `index` is stated by the caller rather than assumed to be the active tab: the
/// TabBar's hover close mark (ADR-0235) names the pill it belongs to, and a
/// pointer affordance that closes "whatever happens to be active" would close
/// the wrong workspace whenever the strip and the selection disagree — which is
/// exactly the frame in which a user clicks a mark they can see.
pub(crate) fn handle_close_tab_at(
    app: &mut App,
    index: usize,
    runtime: &UiRuntime,
    viewed_session_id: &str,
) -> ActionFlow {
    // An index the strip no longer paints (a click racing a close) is a no-op,
    // never a panic and never a close of a different tab.
    if index >= app.surfaces.surfaces.tabs().len() {
        return ActionFlow::Handled;
    }
    let closed_tab = app.close_tab(index);
    if let Some(crate::shell::surfaces::ClientTab {
        kind: crate::shell::surfaces::TabKind::Thread(closed_id),
        ..
    }) = closed_tab
    {
        // ADR-0251: release the closed tab's resident workspace and
        // unsubscribe it on the multiplexed connection.
        app.close_thread_workspace(&closed_id);
    }
    if app.surfaces.surfaces.tabs().is_empty() {
        tracing::info!(reason = "close_last_tab", "client detaching");
        return ActionFlow::Exit;
    }

    if let Some(target_tab) = app.surfaces.surfaces.active_tab().cloned() {
        match &target_tab.kind {
            crate::shell::surfaces::TabKind::Thread(id) => {
                enter_scene(app, crate::shell::surfaces::SceneKind::Thread, runtime);
                if id != viewed_session_id {
                    app.activate_thread_workspace(id);
                } else {
                    app.restore_active_tab_context();
                }
                app.transcript.selection = SelectionState::None;
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

/// Close the active tab (the keyboard and palette path).
pub(crate) fn handle_close_tab(
    app: &mut App,
    runtime: &UiRuntime,
    viewed_session_id: &str,
) -> ActionFlow {
    let active_idx = app.surfaces.surfaces.active_tab_index();
    handle_close_tab_at(app, active_idx, runtime, viewed_session_id)
}

/// The strip's own creation verb: open a brand-new thread tab (ADR-0235).
///
/// This is the same resolution `Alt-t` / `NewTab` uses — `/new` in place while
/// the current thread is still empty, otherwise a fresh tab — because two
/// spellings of "new tab" that disagree about when an empty thread is recycled
/// would leave the user guessing which one they pressed.
pub(crate) fn handle_add_tab(
    app: &mut App,
    runtime: &UiRuntime,
    viewed_session_id: &str,
) -> ActionFlow {
    let _ = (runtime, viewed_session_id);
    app.surfaces.startup_overlay = crate::shell::StartupOverlay::None;
    app.dismiss_active_dialog();
    if app.is_current_thread_empty() {
        app.send_intent(protocol::AgentRequest::SlashCommand("/new".to_string()));
    } else {
        // ADR-0251: create and subscribe a new thread tab directly over the
        // live multiplexed connection without tearing down `App`.
        let cwd = app.session.cwd.clone();
        app.subscribe_thread(protocol::AttachAction::New(None), Some(cwd));
    }
    ActionFlow::Handled
}
