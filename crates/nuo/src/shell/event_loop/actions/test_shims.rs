//! Test shims for actions dispatch.
use super::palette::execute_command_by_id;
use super::{ActionContext, ActionFlow, dispatch_action};
use crate::shell::App;
use crate::shell::event_loop::runtime::UiRuntime;
use crate::shell::input;
use std::sync::Arc;
use tokio::sync::mpsc;

#[cfg(test)]
pub(crate) async fn dispatch_action_for_test(
    app: &mut App,
    runtime: &UiRuntime,
    action: input::InputAction,
    viewed_session_id: &str,
) -> ActionFlow {
    let (copy_tx, _copy_rx) = mpsc::unbounded_channel();
    let copy_pending = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (paste_tx, _paste_rx) = mpsc::unbounded_channel();
    let mut sgr_guard = input::SgrLeakGuard::default();
    let session = crate::shell::SessionSource::Remote {
        session_id: viewed_session_id.to_string(),
    };
    let mut ctx = ActionContext {
        runtime,
        session: &session,
        viewed_session_id,
        copy_tx: &copy_tx,
        copy_pending: &copy_pending,
        paste_tx: &paste_tx,
        sgr_guard: &mut sgr_guard,
        // The test shim dispatches a bare action with no originating event;
        // treat it as a keystroke, the conservative default for the armed-window
        // confirmation policy.
        key_event: true,
    };
    let backend = nuotc::Backend::new(Vec::new()).with_bce(nuotc::Bce::No);
    let mut terminal = nuotc::Terminal::new(backend);
    dispatch_action(app, &mut terminal, action, &mut ctx).await
}

/// Dispatch a registry command by id, exactly as the palette / a bound chord
/// does. Exists so tests can pin the *wire* consequence of a command (the
/// `/queue block` and `/queue clear` verbs are registry commands, not input
/// actions, and their intent traffic is the property under test).
#[cfg(test)]
pub(crate) async fn dispatch_command_for_test(
    app: &mut App,
    runtime: &UiRuntime,
    command: crate::shell::keymap::CommandId,
    viewed_session_id: &str,
) -> ActionFlow {
    let (copy_tx, _copy_rx) = mpsc::unbounded_channel();
    let copy_pending = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (paste_tx, _paste_rx) = mpsc::unbounded_channel();
    let mut sgr_guard = input::SgrLeakGuard::default();
    let session = crate::shell::SessionSource::Remote {
        session_id: viewed_session_id.to_string(),
    };
    let ctx = ActionContext {
        runtime,
        session: &session,
        viewed_session_id,
        copy_tx: &copy_tx,
        copy_pending: &copy_pending,
        paste_tx: &paste_tx,
        sgr_guard: &mut sgr_guard,
        key_event: true,
    };
    execute_command_by_id(app, &ctx, command).await
}

/// Test shims for the dashboard console dispatcher: the internal helpers the
/// console tests drive directly (they need no terminal or clipboard plumbing
/// — just `App` + `UiRuntime`).
#[cfg(test)]
pub(crate) mod host_test_shims {
    use super::super::host;
    use super::UiRuntime;
    use crate::shell::App;

    pub(crate) async fn dispatch(
        app: &mut App,
        runtime: &UiRuntime,
        line: &str,
        create_when_bare: bool,
    ) {
        host::dispatch_console_command(app, runtime, line, create_when_bare).await;
    }

    pub(crate) fn kill(app: &mut App, runtime: &UiRuntime) {
        host::kill_selected(app, runtime);
    }

    pub(crate) fn kill_cancel(app: &mut App) {
        host::cancel_kill_confirm(app);
    }

    /// The scene-exit verb's standalone-startup carve-out (see
    /// `modals::quit_standalone_scene_at_startup`).
    pub(crate) fn quit_standalone_scene_at_startup(app: &mut App) -> bool {
        crate::shell::event_loop::actions::modals::quit_standalone_scene_at_startup(app)
    }
}
