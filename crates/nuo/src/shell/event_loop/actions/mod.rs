//! Input-action dispatch for the TUI event loop: [`dispatch_action`] classifies
//! an [`input::InputAction`] into an action family and delegates to the
//! family handler. The family handlers hold the arms that `run_app_loop`'s
//! input-drain stage ran inline, moved here verbatim with only the
//! loop-control statements rewritten as [`ActionFlow`] values.
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

use nuotc::Terminal;
use tokio::sync::mpsc;

use nuo_protocol::AgentRequest;

use crate::shell::App;
use crate::shell::clipboard;
use crate::shell::clipboard_ops;
use crate::shell::input;
use crate::shell::render;
use crate::shell::surfaces::{DialogKind, SceneKind};
use nuo_view::model::selection::SelectionState;

use super::runtime::UiRuntime;
use super::sync::show_local_toast;

pub(crate) mod commands;
mod composer;
pub(crate) mod config;
mod host;
mod modals;
pub(crate) mod mouse;
pub(crate) mod navigation;
pub(crate) mod palette;
mod providers;
mod questions;
mod queue;
pub(crate) mod scroll;
mod sessions;
pub(crate) mod tabs;

#[cfg(test)]
mod test_shims;
#[cfg(test)]
mod tests;

pub(crate) use config::{handle_config_activate, open_active_connection_detail};
#[allow(unused_imports)]
pub(crate) use modals::{
    activate_picked_model, apply_pre_attach_decision, effective_reasoning_effort,
    handle_open_model_detail, handle_permission_submit, handle_submit_model_editor,
    modal_page_step, question_effects,
};
pub(crate) use navigation::{enter_panel, enter_scene};
pub(crate) use palette::execute_command_by_id;
pub(crate) use scroll::{
    handle_wheel, scroll_tick, scroll_transcript_page, scroll_transcript_to_edge,
};
pub(crate) use tabs::{activate_tab_at_index, handle_close_tab};

#[allow(unused_imports)]
pub(super) use commands::split_command_word;
#[allow(unused_imports)]
pub(crate) use commands::{
    InterruptTarget, handle_esc_interrupt, handle_esc_interrupt_with_runtime,
};

#[cfg(test)]
pub(crate) use commands::{handle_ctrl_c, handle_send_slash};
#[cfg(test)]
pub(crate) use modals::{
    handle_close_modal, handle_modal_down, handle_modal_up, handle_open_model_editor,
    handle_submit_custom_provider,
};
#[cfg(test)]
pub(crate) use test_shims::{dispatch_action_for_test, dispatch_command_for_test, host_test_shims};

/// How the event loop proceeds after a dispatched action. Arms that ended in
/// `continue` (skip to the next drained input event) or `return Ok(())` (exit
/// the loop) when the match was inline in `run_app_loop` return these instead;
/// the call site maps them back onto the same control flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ActionFlow {
    /// Action handled; the drain loop proceeds to the next statement.
    Handled,
    /// `continue` the input-drain loop.
    NextEvent,
    /// `return Ok(())` from `run_app_loop`.
    Exit,
}

pub(crate) struct ActionContext<'a> {
    pub runtime: &'a UiRuntime,
    pub session: &'a crate::shell::HookStartSource,
    pub viewed_session_id: &'a str,
    pub copy_tx: &'a mpsc::UnboundedSender<Result<clipboard::CopyOutcome, String>>,
    pub copy_pending: &'a Arc<AtomicUsize>,
    pub paste_tx: &'a mpsc::UnboundedSender<clipboard::ClipboardRead>,
    pub sgr_guard: &'a mut input::SgrLeakGuard,
    /// Whether the resolved action came from a **keystroke** (as opposed to a
    /// pointer, wheel, hover, or resize event).
    ///
    /// The multi-stroke confirmation policy (the `C-x` scene namespace, the
    /// double Ctrl+C quit window, the dashboard's `k` kill confirm) is defined
    /// over *keystrokes*: "the armed state survives exactly one keystroke".
    /// Gating the disarm on this flag keeps pointer noise out of that state
    /// machine — a terminal that reports motion (`MouseEventKind::Moved` →
    /// `InputAction::Hover`, which the WinAPI backend does natively regardless
    /// of the DEC tracking modes) must not silently cancel a half-typed chord,
    /// or `C-x w` would intermittently need two attempts.
    pub key_event: bool,
}

/// Shared scroll step for keyboard scroll keys and out-of-panel wheel
/// ticks: a modal body (including the permission sheet's details) takes the
/// tick one line at a time; with no modal the transcript scrolls by four
/// lines per tick so browsing feels fast instead of crawling line-by-line.
/// Wheel ticks landing inside the composer panel never reach this — the
/// `Wheel` arm routes them to the input's own viewport first.
pub(super) async fn dispatch_action<W: std::io::Write>(
    app: &mut App,
    terminal: &mut Terminal<W>,
    action: input::InputAction,
    ctx: &mut ActionContext<'_>,
) -> ActionFlow {
    let ActionContext {
        runtime,
        session,
        viewed_session_id,
        copy_tx,
        copy_pending,
        paste_tx,
        sgr_guard,
        key_event,
    } = ctx;
    let runtime = *runtime;
    let session = *session;
    let viewed_session_id = *viewed_session_id;
    let copy_tx = *copy_tx;
    let copy_pending = *copy_pending;
    let paste_tx = *paste_tx;
    let key_event = *key_event;
    let sgr_guard = &mut **sgr_guard;
    // While the dashboard's kill confirm is armed, only the confirming `k`
    // (or the confirm-cancelling paths inside the dashboard arms) keeps it
    // alive: any other keystroke — navigation, prompt, focus toggle, Esc —
    // disarms it. The armed state lives exactly one keystroke, so pointer
    // noise (motion → `Hover`, wheel, resize) can never cancel it.
    if key_event
        && app.surfaces.host_kill_confirm.is_some()
        && app.current_scene() == SceneKind::Dashboard
        && !matches!(
            action,
            input::InputAction::HostKillSelected | input::InputAction::None
        )
    {
        host::cancel_kill_confirm(app);
    }

    // While the Ctrl+C quit window is armed, any keystroke other than Ctrl+C
    // cancels the armed state so subsequent typing does not inadvertently exit
    // the program. The window is wall-clock bounded, so a pointer event is
    // neither here nor there — it must not consume the operator's first press.
    if key_event
        && app.ctrl_c_armed()
        && !matches!(action, input::InputAction::CtrlC | input::InputAction::None)
    {
        app.arm_ctrl_c(None);
    }
    match classify_action(&action) {
        ActionFamily::None => ActionFlow::Handled,
        ActionFamily::Terminal => handle_terminal_tab_action(
            app,
            terminal,
            action,
            runtime,
            viewed_session_id,
            &mut *sgr_guard,
        ),
        ActionFamily::Composer => {
            handle_composer_input_action(app, action, runtime, session, viewed_session_id, paste_tx)
                .await
        }
        ActionFamily::ProviderConfig => handle_provider_config_action(
            app,
            runtime,
            viewed_session_id,
            copy_tx,
            copy_pending,
            action,
        ),
        ActionFamily::Session => {
            handle_session_thread_action(app, viewed_session_id, ctx, action).await
        }
        ActionFamily::Host => {
            host::handle_host_action(app, runtime, viewed_session_id, action).await
        }
        ActionFamily::Navigation => {
            handle_navigation_focus_action(app, runtime, viewed_session_id, action)
        }
        ActionFamily::Modal => {
            handle_overlay_modal_action(app, runtime, viewed_session_id, action).await
        }
        ActionFamily::Telemetry => {
            handle_telemetry_usage_action(app, runtime, viewed_session_id, action)
        }
        ActionFamily::Scroll => handle_scroll_action(app, runtime, viewed_session_id, action),
        ActionFamily::Clipboard => {
            handle_clipboard_export_action(app, viewed_session_id, copy_tx, copy_pending, action)
        }
        ActionFamily::Notices => {
            handle_notice_action(app, viewed_session_id, copy_tx, copy_pending, action)
        }
        ActionFamily::Queue => {
            handle_queue_dispatch_action(app, runtime, viewed_session_id, action)
        }
        ActionFamily::Interrupt => {
            handle_interrupt_action(
                app,
                runtime,
                viewed_session_id,
                copy_tx,
                copy_pending,
                action,
            )
            .await
        }
        ActionFamily::Mouse => handle_mouse_action(app, runtime, viewed_session_id, action).await,
    }
}

/// The action family used by [`dispatch_action`] to pick a handler.
///
/// Classification is pure and happens before the handler runs; the action
/// itself is moved into the selected handler unchanged.
#[derive(Clone, Copy)]
enum ActionFamily {
    None,
    Terminal,
    Composer,
    ProviderConfig,
    Session,
    Host,
    Navigation,
    Modal,
    Telemetry,
    Scroll,
    Clipboard,
    Notices,
    Queue,
    Interrupt,
    Mouse,
}

fn classify_action(action: &input::InputAction) -> ActionFamily {
    match action {
        input::InputAction::None => ActionFamily::None,
        input::InputAction::TerminalResized { .. }
        | input::InputAction::Quit
        | input::InputAction::SelectTab(_)
        | input::InputAction::NextTab
        | input::InputAction::PrevTab
        | input::InputAction::CloseTab
        | input::InputAction::TabHistoryBack
        | input::InputAction::TabHistoryForward => ActionFamily::Terminal,
        input::InputAction::SendChat(_)
        | input::InputAction::SteerImmediate(_)
        | input::InputAction::QueueFollowUp(_)
        | input::InputAction::SendSlash(_)
        | input::InputAction::HistoryInsert
        | input::InputAction::HistoryDeleteSelected
        | input::InputAction::CancelHistoryRecall
        | input::InputAction::Paste
        | input::InputAction::BracketedPaste(_)
        | input::InputAction::InsertChar(_)
        | input::InputAction::Backspace
        | input::InputAction::DeleteForward
        | input::InputAction::SuggestNext
        | input::InputAction::SuggestPrev
        | input::InputAction::AcceptSuggestion(_)
        | input::InputAction::CommitSuggestion(_)
        | input::InputAction::ReopenCompletion
        | input::InputAction::CloseCompletion
        | input::InputAction::HistoryPrev
        | input::InputAction::HistoryNext => ActionFamily::Composer,
        input::InputAction::ProviderPickerActivate
        | input::InputAction::OpenPresetChooser
        | input::InputAction::OpenCustomConnection
        | input::InputAction::RefreshProviderModels
        | input::InputAction::ConfigFocusToggle
        | input::InputAction::ConfigActivate
        | input::InputAction::ConfigSegmentPrev
        | input::InputAction::ConfigSegmentNext
        | input::InputAction::McpToggle
        | input::InputAction::McpReconnect
        | input::InputAction::PermissionsActivate
        | input::InputAction::PermissionsClearAll
        | input::InputAction::OpenConnectionDetail
        | input::InputAction::OpenModelDetail
        | input::InputAction::OpenActiveConnectionDetail
        | input::InputAction::BrowseConnectionModels
        | input::InputAction::CustomProviderNextField
        | input::InputAction::CustomProviderPrevField
        | input::InputAction::ScrollCustomProvider { .. }
        | input::InputAction::CycleCustomProviderChoice { .. }
        | input::InputAction::MovePresetChoice { .. }
        | input::InputAction::SelectPreset
        | input::InputAction::SelectPresetWithOauthMethod { .. }
        | input::InputAction::CancelOauthPending
        | input::InputAction::CycleOauthSelection
        | input::InputAction::CopyOauthContent { .. }
        | input::InputAction::CancelPresetChooser
        | input::InputAction::DeleteProvider
        | input::InputAction::DeleteProviderConfirm
        | input::InputAction::DeleteProviderCancel
        | input::InputAction::CancelCustomProvider
        | input::InputAction::SubmitCustomProvider
        | input::InputAction::ModelEnterSearch
        | input::InputAction::ModelExitSearch
        | input::InputAction::ProviderPickerToggleFavorite
        | input::InputAction::ProviderPickerBlockModel
        | input::InputAction::OpenModelEditor
        | input::InputAction::ModelEditorNextField
        | input::InputAction::ModelEditorEffortCycle { .. }
        | input::InputAction::ModelEditorEffortJump { .. }
        | input::InputAction::ModelEditorThinkingToggle
        | input::InputAction::ModelEditorVisionCycle
        | input::InputAction::ModelEditorToolCycle
        | input::InputAction::SubmitModelEditor => ActionFamily::ProviderConfig,
        input::InputAction::SessionSelect { .. }
        | input::InputAction::SessionActivate
        | input::InputAction::OpenSelectedSession
        | input::InputAction::DeleteSelectedSession
        | input::InputAction::CreateNewSession
        | input::InputAction::OpenSessionInfo
        | input::InputAction::ToggleSessionTimelineExpand
        | input::InputAction::ViewSwitcherFilter { .. }
        | input::InputAction::ViewSwitcherBackspace
        | input::InputAction::OpenPortPalette
        | input::InputAction::TogglePortListener
        | input::InputAction::CopyPortConnectInfo
        | input::InputAction::ViewSwitcherToggle
        | input::InputAction::ViewSwitchActivate
        | input::InputAction::ViewCloseSelected => ActionFamily::Session,
        input::InputAction::HostPreviewSelected
        | input::InputAction::HostSwitchSelected
        | input::InputAction::HostToggleUsage
        | input::InputAction::HostFilterOpen
        | input::InputAction::HostFocusToggle
        | input::InputAction::HostInterruptSelected
        | input::InputAction::HostKillSelected
        | input::InputAction::HostSuspendSelected
        | input::InputAction::HostPromptOpen
        | input::InputAction::HostNewSession
        | input::InputAction::HostPromptSeed(_)
        | input::InputAction::HostPromptSubmit => ActionFamily::Host,
        input::InputAction::OpenSessions
        | input::InputAction::NavigateDashboard
        | input::InputAction::NavigateSettings
        | input::InputAction::OpenModels
        | input::InputAction::OpenConnections
        | input::InputAction::OpenHistory
        | input::InputAction::OpenPermissions
        | input::InputAction::OpenTools
        | input::InputAction::OpenUsage
        | input::InputAction::OpenQuotas
        | input::InputAction::OpenMcp
        | input::InputAction::OpenSkills
        | input::InputAction::SkillsToggleDetail
        | input::InputAction::OpenConfig
        | input::InputAction::OpenToolDetail
        | input::InputAction::SceneBack
        | input::InputAction::ViewFocusedTargetOutput
        | input::InputAction::FocusNextTarget
        | input::InputAction::FocusPrevTarget
        | input::InputAction::ToggleFocusPlane
        | input::InputAction::ActivateFocusedTarget
        | input::InputAction::FocusedTargetKey(_) => ActionFamily::Navigation,
        input::InputAction::CloseModal
        | input::InputAction::OpenHelp
        | input::InputAction::ToggleDialogKeys
        | input::InputAction::DialogKeysScroll { .. }
        | input::InputAction::ModalUp
        | input::InputAction::ModalDown
        | input::InputAction::QuestionUp
        | input::InputAction::QuestionDown
        | input::InputAction::QuestionToggle
        | input::InputAction::QuestionSelect(_)
        | input::InputAction::QuestionSubmit
        | input::InputAction::QuestionPrevious
        | input::InputAction::QuestionNext
        | input::InputAction::QuestionCancel
        | input::InputAction::PreAttachUp
        | input::InputAction::PreAttachDown
        | input::InputAction::PreAttachToggle
        | input::InputAction::PreAttachSubmit
        | input::InputAction::PreAttachCancel
        | input::InputAction::InputSubmit
        | input::InputAction::InputCancel
        | input::InputAction::QuestionInsertChar(_)
        | input::InputAction::QuestionBackspace
        | input::InputAction::PermissionPrevOption
        | input::InputAction::PermissionNextOption
        | input::InputAction::PermissionSubmit
        | input::InputAction::PermissionReject
        | input::InputAction::PermissionBack => ActionFamily::Modal,
        input::InputAction::QuotasActivate
        | input::InputAction::RefreshQuotas
        | input::InputAction::QuotasToggleDetail
        | input::InputAction::TraceInspectDetail
        | input::InputAction::ThreadStatsOpenTrace
        | input::InputAction::OpenThreadStats
        | input::InputAction::OpenSessionTrace => ActionFamily::Telemetry,
        input::InputAction::ScrollUp
        | input::InputAction::ScrollDown
        | input::InputAction::Wheel { .. }
        | input::InputAction::ScrollPageUp
        | input::InputAction::ScrollPageDown
        | input::InputAction::ScrollTop
        | input::InputAction::ScrollBottom
        | input::InputAction::PermissionDetailsUp
        | input::InputAction::PermissionDetailsDown => ActionFamily::Scroll,
        input::InputAction::CopySelection
        | input::InputAction::CopyFocusedTarget
        | input::InputAction::CommandLogCopy
        | input::InputAction::CommandLogToggleScope
        | input::InputAction::CopyFocusedDiagnostic => ActionFamily::Clipboard,
        input::InputAction::OpenNotices
        | input::InputAction::NoticesSelect { .. }
        | input::InputAction::NoticesPage { .. }
        | input::InputAction::NoticesToggleDetail
        | input::InputAction::NoticesClear
        | input::InputAction::NoticesCopy => ActionFamily::Notices,
        input::InputAction::ToggleSendMode
        | input::InputAction::OpenQueue
        | input::InputAction::RecallQueuedSelected
        | input::InputAction::QueueToggleBlock
        | input::InputAction::QueueDelete
        | input::InputAction::QueueMoveItem { .. } => ActionFamily::Queue,
        input::InputAction::Interrupt
        | input::InputAction::CtrlC
        | input::InputAction::InterruptSubagent => ActionFamily::Interrupt,
        input::InputAction::SelectionStart { .. }
        | input::InputAction::RightClick { .. }
        | input::InputAction::SelectionUpdate { .. }
        | input::InputAction::SelectionEnd
        | input::InputAction::SelectBlock { .. }
        | input::InputAction::Hover { .. } => ActionFamily::Mouse,
    }
}

fn handle_terminal_tab_action<W: std::io::Write>(
    app: &mut App,
    terminal: &mut Terminal<W>,
    action: input::InputAction,
    runtime: &UiRuntime,
    viewed_session_id: &str,
    sgr_guard: &mut input::SgrLeakGuard,
) -> ActionFlow {
    match action {
        input::InputAction::TerminalResized { cols, rows } => {
            // A resize is the prime trigger for crossterm splitting an
            // in-flight SGR mouse sequence across reads (issue #854).
            // Re-arm mouse capture so both crossterm's parser and the
            // terminal's mouse-tracking state start from a clean slate.
            terminal.resize_to(cols, rows);
            app.transcript.layout_height_cache.clear();
            if !app.transcript.follow_bottom {
                app.transcript.scroll_settle_pending = true;
            }
            if app.surfaces.profile.supports_mouse {
                let _ = crossterm::execute!(
                    std::io::stdout(),
                    crate::shell::mouse_mode::EnableButtonMouse
                );
            }
            sgr_guard.reset();
        }
        input::InputAction::Quit => {
            // ADR-0038 [INV-ACTION-02]: Quitting the TUI client is strictly a non-destructive Detach.
            tracing::info!(reason = "slash_exit", "client detaching");
            return ActionFlow::Exit;
        }
        input::InputAction::SelectTab(idx) => {
            return activate_tab_at_index(app, idx, runtime, viewed_session_id);
        }
        input::InputAction::NextTab => {
            let next_idx = (app.surfaces.surfaces.active_tab_index() + 1)
                % app.surfaces.surfaces.tabs().len().max(1);
            return activate_tab_at_index(app, next_idx, runtime, viewed_session_id);
        }
        input::InputAction::PrevTab => {
            let total = app.surfaces.surfaces.tabs().len().max(1);
            let prev_idx = if app.surfaces.surfaces.active_tab_index() == 0 {
                total - 1
            } else {
                app.surfaces.surfaces.active_tab_index() - 1
            };
            return activate_tab_at_index(app, prev_idx, runtime, viewed_session_id);
        }
        input::InputAction::CloseTab => {
            if app.active_dialog().is_some() {
                app.dismiss_surface();
            } else if !modals::quit_standalone_scene_at_startup(app) {
                return handle_close_tab(app, runtime, viewed_session_id);
            }
        }
        input::InputAction::TabHistoryBack => {
            app.tab_history_back();
        }
        input::InputAction::TabHistoryForward => {
            app.tab_history_forward();
        }
        _ => {}
    }
    ActionFlow::Handled
}

async fn handle_composer_input_action(
    app: &mut App,
    action: input::InputAction,
    runtime: &UiRuntime,
    session: &crate::shell::HookStartSource,
    viewed_session_id: &str,
    paste_tx: &mpsc::UnboundedSender<clipboard::ClipboardRead>,
) -> ActionFlow {
    match action {
        input::InputAction::SendChat(text) => {
            commands::handle_send_chat(app, runtime, viewed_session_id, text).await;
        }
        input::InputAction::SteerImmediate(text) => {
            commands::handle_send_steer(app, runtime, viewed_session_id, text).await;
        }
        input::InputAction::QueueFollowUp(text) => {
            commands::handle_queue_follow_up(app, runtime, viewed_session_id, text).await;
        }
        input::InputAction::SendSlash(cmd) => {
            return commands::handle_send_slash(app, runtime, session, cmd).await;
        }
        input::InputAction::HistoryInsert => {
            // Enter / Tab inside the Ctrl+R panel: pull the focused entry out
            // of `history_rows` (the filtered matches) and drop it into
            // the input box for further editing / sending. The message
            // is not shipped here — the user hits Enter again to send.
            let ranked = app.history_rows();
            let pick = ranked
                .get(
                    app.surfaces
                        .surfaces
                        .dlg_mut::<crate::shell::surfaces::HistorySearchDialog>()
                        .index,
                )
                .or_else(|| ranked.first());
            let Some((orig_idx, _)) = pick else {
                return ActionFlow::Handled;
            };
            let original = *orig_idx;
            let text = app.composer.input_history[original].text.clone();
            // Restore the attachments cached behind this entry (if
            // any) so a re-send ships the real image / paste
            // payloads rather than a bare chip label; with no
            // cache the staged vectors are cleared.
            app.restore_history_attachments(original);
            // The inserted entry becomes the new draft: it is the
            // newest *unsent* input, so ↓ past the newest history
            // row restores it, never a stale remembered draft.
            app.adopt_as_draft(
                text,
                app.composer.pending_images.clone(),
                app.composer.pending_text_pastes.clone(),
                crate::shell::app::DraftAdoption::Replace,
            );
            // Clear the stashed draft capsule since user committed a historical entry
            app.clear_stashed_draft();
            // The selection replaces the in-progress draft, and history search completes.
            app.surfaces.surfaces.dismiss_all_overlays();
            app.surfaces
                .surfaces
                .dlg_mut::<crate::shell::surfaces::HistorySearchDialog>()
                .index = 0;
            app.composer.input_scroll = 0;
            app.composer.suggestion_index = None;
            // A programmatic input replacement — latch the dismissal so
            // a slash-command selection doesn't flash its completion
            // popup until the next real edit.
            app.composer.completion_dismissed = true;
            app.reset_to_thread();
        }
        input::InputAction::HistoryDeleteSelected => {
            if app.active_composer_extension()
                == Some(crate::shell::composer_extension::ComposerExtensionKind::HistorySearch)
            {
                app.delete_selected_history_entry();
            }
        }
        input::InputAction::CancelHistoryRecall => {
            // Esc while the inline ↑/↓ pointer sits on a history row: cancel
            // the recall and restore the stashed draft (text + attachments).
            // `cancel_history_recall` is a no-op when the pointer is already
            // `None`, so a key race between the context snapshot and dispatch
            // cannot clobber a draft.
            app.cancel_history_recall();
        }
        input::InputAction::Paste => {
            // Ctrl+V: read the system clipboard off the event loop.
            // The result is delivered back through `paste_rx` and
            // applied on a later frame (image -> attach, text ->
            // insert on the main prompt, or inline splice into the
            // focused modal field). `apply_clipboard_paste` branches
            // on the active modal at apply time, so a paste spawned
            // inside a modal that the user closed before the read
            // returned lands in the main prompt rather than being
            // dropped.
            clipboard_ops::spawn_clipboard_paste(paste_tx);
        }
        input::InputAction::BracketedPaste(text) => {
            // Terminal-level paste (bracketed paste mode). The payload
            // is already in hand, so route it directly through the same
            // chip-or-inline logic as Ctrl+V without an async hop.
            clipboard_ops::apply_clipboard_paste(app, clipboard::ClipboardRead::Text(text));
        }
        input::InputAction::InsertChar(_)
        | input::InputAction::Backspace
        | input::InputAction::DeleteForward
        | input::InputAction::SuggestNext
        | input::InputAction::SuggestPrev
        | input::InputAction::AcceptSuggestion(_)
        | input::InputAction::CommitSuggestion(_)
        | input::InputAction::ReopenCompletion
        | input::InputAction::CloseCompletion
        | input::InputAction::HistoryPrev
        | input::InputAction::HistoryNext => {
            composer::handle_composer_action(app, action);
        }
        _ => {}
    }
    ActionFlow::Handled
}

fn handle_provider_config_action(
    app: &mut App,
    runtime: &UiRuntime,
    viewed_session_id: &str,
    copy_tx: &mpsc::UnboundedSender<Result<clipboard::CopyOutcome, String>>,
    copy_pending: &Arc<AtomicUsize>,
    action: input::InputAction,
) -> ActionFlow {
    match action {
        input::InputAction::ProviderPickerActivate
        | input::InputAction::CustomProviderNextField
        | input::InputAction::CustomProviderPrevField
        | input::InputAction::ScrollCustomProvider { .. }
        | input::InputAction::CycleCustomProviderChoice { .. }
        | input::InputAction::MovePresetChoice { .. }
        | input::InputAction::SelectPreset
        | input::InputAction::SelectPresetWithOauthMethod { .. }
        | input::InputAction::CancelOauthPending
        | input::InputAction::CycleOauthSelection
        | input::InputAction::CopyOauthContent { .. }
        | input::InputAction::CancelPresetChooser
        | input::InputAction::DeleteProvider
        | input::InputAction::DeleteProviderConfirm
        | input::InputAction::DeleteProviderCancel
        | input::InputAction::CancelCustomProvider
        | input::InputAction::SubmitCustomProvider
        | input::InputAction::ModelEnterSearch
        | input::InputAction::ModelExitSearch
        | input::InputAction::ProviderPickerToggleFavorite
        | input::InputAction::ProviderPickerBlockModel
        | input::InputAction::OpenModelEditor
        | input::InputAction::ModelEditorNextField
        | input::InputAction::ModelEditorEffortCycle { .. }
        | input::InputAction::ModelEditorEffortJump { .. }
        | input::InputAction::ModelEditorThinkingToggle
        | input::InputAction::ModelEditorVisionCycle
        | input::InputAction::ModelEditorToolCycle
        | input::InputAction::SubmitModelEditor => {
            if let Some(flow) =
                providers::handle_provider_action(app, copy_tx, copy_pending, action)
            {
                return flow;
            }
        }
        input::InputAction::OpenPresetChooser => {
            // `a` in Connections opens the curated preset branch.
            // Only meaningful from Connections; ignored otherwise.
            if app.active_dialog() == Some(DialogKind::Connections) {
                app.open_preset_chooser();
            }
        }
        input::InputAction::OpenCustomConnection => {
            // `c` in Connections opens the custom branch directly. Custom is
            // deliberately not one of the curated preset rows.
            if app.active_dialog() == Some(DialogKind::Connections) {
                app.open_custom_connection_editor();
            }
        }
        input::InputAction::RefreshProviderModels => {
            if app.active_dialog() == Some(DialogKind::Connections)
                && app
                    .surfaces
                    .surfaces
                    .dlg_mut::<crate::shell::surfaces::ConnectionsDialog>()
                    .info_detail
            {
                let id = app
                    .surfaces
                    .connection_detail
                    .as_ref()
                    .map(|d| d.name.clone())
                    .or_else(|| {
                        let providers = app.providers_filtered();
                        providers
                            .get(app.active_index().min(providers.len().saturating_sub(1)))
                            .map(|p| p.id.clone())
                    });
                if let Some(id) = id {
                    if let Some(detail) = app.surfaces.connection_detail.as_mut() {
                        detail.usage = nuo_protocol::ConnectionUsageState::Fetching;
                    }
                    show_local_toast(
                        app,
                        "Refreshing connection usage…",
                        false,
                        std::time::Duration::from_millis(1500),
                    );
                    app.send_intent(AgentRequest::QueryConnectionDetail {
                        id,
                        force_refresh: true,
                    });
                }
            } else if matches!(
                app.active_dialog(),
                Some(DialogKind::Models | DialogKind::Connections)
            ) {
                if app
                    .surfaces
                    .surfaces
                    .dlg_mut::<crate::shell::surfaces::ModelsDialog>()
                    .refreshing
                    || app
                        .surfaces
                        .surfaces
                        .dlg_mut::<crate::shell::surfaces::ConnectionsDialog>()
                        .refreshing
                {
                    show_local_toast(
                        app,
                        "Model refresh already in progress…",
                        false,
                        std::time::Duration::from_millis(1500),
                    );
                } else {
                    app.surfaces
                        .surfaces
                        .dlg_mut::<crate::shell::surfaces::ModelsDialog>()
                        .refreshing = true;
                    app.surfaces
                        .surfaces
                        .dlg_mut::<crate::shell::surfaces::ConnectionsDialog>()
                        .refreshing = true;
                    show_local_toast(
                        app,
                        if app.active_dialog() == Some(DialogKind::Connections) {
                            "Refreshing connections and models…"
                        } else {
                            "Refreshing models…"
                        },
                        false,
                        std::time::Duration::from_millis(1500),
                    );
                    app.send_intent(AgentRequest::RefreshProviderModels);
                    if app.active_dialog() == Some(DialogKind::Connections) {
                        app.send_intent(AgentRequest::QueryAllConnectionsUsage {
                            force_refresh: true,
                        });
                    }
                }
            }
        }
        input::InputAction::ConfigFocusToggle => {
            if app.current_scene() == SceneKind::Settings {
                app.surfaces.config_focus = match app.surfaces.config_focus {
                    crate::shell::overlays::ConfigFocus::Categories => {
                        crate::shell::overlays::ConfigFocus::Detail
                    }
                    crate::shell::overlays::ConfigFocus::Detail => {
                        crate::shell::overlays::ConfigFocus::Categories
                    }
                };
            }
        }
        input::InputAction::ConfigActivate => {
            handle_config_activate(app);
        }
        input::InputAction::ConfigSegmentPrev => {
            if app.current_scene() == SceneKind::Settings
                && crate::shell::overlays::ConfigCategory::from_index(app.surfaces.config_category)
                    == crate::shell::overlays::ConfigCategory::WebReader
            {
                app.surfaces.config_category =
                    crate::shell::overlays::ConfigCategory::WebSearch as usize;
                app.surfaces.config_detail_index = 0;
                app.surfaces.config_detail_scroll = 0;
                app.surfaces.config_hover_index = None;
            }
        }
        input::InputAction::ConfigSegmentNext => {
            if app.current_scene() == SceneKind::Settings
                && crate::shell::overlays::ConfigCategory::from_index(app.surfaces.config_category)
                    == crate::shell::overlays::ConfigCategory::WebSearch
            {
                app.surfaces.config_category =
                    crate::shell::overlays::ConfigCategory::WebReader as usize;
                app.surfaces.config_detail_index = 0;
                app.surfaces.config_detail_scroll = 0;
                app.surfaces.config_hover_index = None;
            }
        }
        input::InputAction::McpToggle => {
            // Connect/disconnect the selected server for the session.
            // The "enabled intent" is the inverse of its disabled flag;
            // the harness replies with a fresh snapshot.
            if let Some(server) = app
                .session
                .session_context
                .as_ref()
                .and_then(|s| s.mcp.get(app.active_index()))
            {
                app.send_intent(AgentRequest::ToggleMcpServer {
                    name: server.name.clone(),
                    enabled: server.disabled,
                });
            }
        }
        input::InputAction::McpReconnect => {
            // Reconnect the selected server on demand. The harness
            // replies with a fresh snapshot reflecting the new status.
            if let Some(server) = app
                .session
                .session_context
                .as_ref()
                .and_then(|s| s.mcp.get(app.active_index()))
            {
                app.send_intent(AgentRequest::ReconnectMcpServer {
                    name: server.name.clone(),
                });
            }
        }
        input::InputAction::PermissionsActivate => {
            // Revoke the selected "always allow" rule. The harness
            // replies with a fresh snapshot so the list re-renders.
            if let Some(snapshot) = app.session.session_context.as_ref()
                && let Some(rule) = snapshot.permissions.get(app.active_index())
            {
                app.send_intent(AgentRequest::RevokePermission {
                    tool: rule.tool.clone(),
                    scope: rule.scope.clone(),
                });
            }
        }
        input::InputAction::PermissionsClearAll => {
            // Clear every cached rule. The harness replies with a fresh
            // (empty) snapshot.
            app.send_intent(AgentRequest::ClearAllPermissions);
            app.set_active_index(0);
        }
        input::InputAction::OpenConnectionDetail => {
            let providers = app.providers_filtered();
            if let Some(ranked) =
                providers.get(app.active_index().min(providers.len().saturating_sub(1)))
            {
                app.surfaces
                    .surfaces
                    .dlg_mut::<crate::shell::surfaces::ConnectionsDialog>()
                    .info_detail = true;
                app.surfaces
                    .surfaces
                    .dlg_mut::<crate::shell::surfaces::ConnectionsDialog>()
                    .info_standalone = false;
                app.surfaces.connection_detail = None;
                app.surfaces
                    .surfaces
                    .dlg_mut::<crate::shell::surfaces::ConnectionsDialog>()
                    .info_scroll = 0;
                app.send_intent(AgentRequest::QueryConnectionDetail {
                    id: ranked.id.clone(),
                    force_refresh: false,
                });
            }
        }
        input::InputAction::OpenModelDetail => {
            modals::handle_open_model_detail(app);
        }
        input::InputAction::OpenActiveConnectionDetail => {
            open_active_connection_detail(app, runtime, viewed_session_id);
        }
        input::InputAction::BrowseConnectionModels => {
            if let Some(detail) = &app.surfaces.connection_detail {
                let filter = if !detail.provider_label.is_empty() {
                    detail.provider_label.clone()
                } else if !detail.name.is_empty() {
                    detail.name.clone()
                } else {
                    detail.provider.clone()
                };
                app.open_dialog(DialogKind::Models);
                let models_dlg = app
                    .surfaces
                    .surfaces
                    .dlg_mut::<crate::shell::surfaces::ModelsDialog>();
                models_dlg.info_detail = false;
                models_dlg.search = true;
                models_dlg.query.text = filter;
                models_dlg.query.cursor = models_dlg.query.text.len();
                models_dlg.index = 0;
                models_dlg.scroll = 0;
                models_dlg.follow = true;
            }
        }
        _ => {}
    }
    ActionFlow::Handled
}

async fn handle_session_thread_action(
    app: &mut App,
    viewed_session_id: &str,
    ctx: &mut ActionContext<'_>,
    action: input::InputAction,
) -> ActionFlow {
    match action {
        input::InputAction::SessionSelect { .. }
        | input::InputAction::SessionActivate
        | input::InputAction::OpenSelectedSession => {
            if let Some(flow) =
                sessions::handle_session_action(app, viewed_session_id, ctx, action).await
            {
                return flow;
            }
        }
        input::InputAction::DeleteSelectedSession
        | input::InputAction::CreateNewSession
        | input::InputAction::OpenSessionInfo
        | input::InputAction::ToggleSessionTimelineExpand => {
            if let Some(flow) =
                sessions::handle_session_action(app, viewed_session_id, ctx, action).await
            {
                return flow;
            }
        }
        input::InputAction::ViewSwitcherFilter { .. }
        | input::InputAction::ViewSwitcherBackspace
        | input::InputAction::OpenPortPalette
        | input::InputAction::TogglePortListener
        | input::InputAction::CopyPortConnectInfo
        | input::InputAction::ViewSwitcherToggle
        | input::InputAction::ViewSwitchActivate
        | input::InputAction::ViewCloseSelected => {
            if let Some(flow) =
                sessions::handle_session_action(app, viewed_session_id, ctx, action).await
            {
                return flow;
            }
        }
        _ => {}
    }
    ActionFlow::Handled
}

fn handle_navigation_focus_action(
    app: &mut App,
    runtime: &UiRuntime,
    viewed_session_id: &str,
    action: input::InputAction,
) -> ActionFlow {
    match action {
        input::InputAction::OpenSessions => {
            enter_panel(
                app,
                crate::shell::surfaces::DialogKind::Threads,
                runtime,
                viewed_session_id,
            );
        }
        input::InputAction::NavigateDashboard => {
            enter_scene(app, crate::shell::surfaces::SceneKind::Dashboard, runtime);
        }
        input::InputAction::NavigateSettings => {
            enter_scene(app, crate::shell::surfaces::SceneKind::Settings, runtime);
        }
        input::InputAction::OpenModels => {
            enter_panel(
                app,
                crate::shell::surfaces::DialogKind::Models,
                runtime,
                viewed_session_id,
            );
        }
        input::InputAction::OpenConnections => {
            enter_panel(
                app,
                crate::shell::surfaces::DialogKind::Connections,
                runtime,
                viewed_session_id,
            );
        }
        input::InputAction::OpenHistory => {
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
        input::InputAction::OpenPermissions => {
            enter_panel(
                app,
                crate::shell::surfaces::DialogKind::Permissions,
                runtime,
                viewed_session_id,
            );
        }
        input::InputAction::OpenTools => {
            enter_panel(
                app,
                crate::shell::surfaces::DialogKind::Tools,
                runtime,
                viewed_session_id,
            );
        }
        input::InputAction::OpenUsage => {
            enter_panel(
                app,
                crate::shell::surfaces::DialogKind::UsageStats,
                runtime,
                viewed_session_id,
            );
        }
        input::InputAction::OpenQuotas => {
            enter_panel(
                app,
                crate::shell::surfaces::DialogKind::Quotas,
                runtime,
                viewed_session_id,
            );
        }
        input::InputAction::OpenMcp => {
            enter_panel(
                app,
                crate::shell::surfaces::DialogKind::Mcp,
                runtime,
                viewed_session_id,
            );
        }
        input::InputAction::OpenSkills => {
            enter_panel(
                app,
                crate::shell::surfaces::DialogKind::Skills,
                runtime,
                viewed_session_id,
            );
        }
        input::InputAction::SkillsToggleDetail => {
            // Toggle the detail block of the selected skill row. Re-pressing
            // Enter on an already-expanded row collapses it.
            let idx = app.active_index();
            app.surfaces
                .surfaces
                .dlg_mut::<crate::shell::surfaces::SkillsDialog>()
                .expanded = if app
                .surfaces
                .surfaces
                .dlg_mut::<crate::shell::surfaces::SkillsDialog>()
                .expanded
                == Some(idx)
            {
                None
            } else {
                Some(idx)
            };
            app.set_active_follow(true);
        }
        input::InputAction::OpenConfig => {
            enter_scene(app, crate::shell::surfaces::SceneKind::Settings, runtime);
        }
        input::InputAction::OpenToolDetail => {
            let count = app.session_tools_len();
            if count > 0 {
                let tools = app
                    .surfaces
                    .surfaces
                    .dlg_mut::<crate::shell::surfaces::ToolsDialog>();
                tools.info_detail = true;
                tools.info_scroll = 0;
            }
        }
        input::InputAction::SceneBack => {
            app.scene_back();
        }
        input::InputAction::ViewFocusedTargetOutput => {
            if let Some(target) = app.transcript.focus.entry() {
                app.open_command_log_scene(target.message_idx);
            }
        }
        input::InputAction::FocusNextTarget => {
            app.focus_interactive_target(1);
        }
        input::InputAction::FocusPrevTarget => {
            app.focus_interactive_target(-1);
        }
        input::InputAction::ToggleFocusPlane => {
            // `Shift-Tab`: cross to the other peer plane, remembering (and
            // restoring) the transcript's entry highlight so the crossing is
            // lossless (ADR-0145 `[INV-FOCUS-01]`).
            app.transcript.focus.toggle();
            // A crossing is a deliberate act on the surface, so any text
            // selection or in-flight drag on the plane being left is dropped —
            // a highlight that survives a plane change reads as "both planes
            // are live", which is precisely what the model forbids.
            app.transcript.selection = SelectionState::None;
            app.transcript.drag.cancel();
        }
        input::InputAction::ActivateFocusedTarget => {
            // Stack-top focus dispatch: delegate to the focused component's own
            // interactive key handler (Enter activates: expand/collapse, subagent zoom, etc.).
            app.dispatch_focused_target_key(crate::shell::keymap::Key::ENTER);
        }
        input::InputAction::FocusedTargetKey(key) => {
            app.dispatch_focused_target_key(key);
        }
        _ => {}
    }
    ActionFlow::Handled
}

async fn handle_overlay_modal_action(
    app: &mut App,
    runtime: &UiRuntime,
    viewed_session_id: &str,
    action: input::InputAction,
) -> ActionFlow {
    match action {
        input::InputAction::CloseModal => {
            modals::handle_close_modal(app, viewed_session_id);
        }
        input::InputAction::OpenHelp => {
            if app.active_dialog() == Some(DialogKind::Help) {
                app.dismiss_surface();
            } else {
                app.open_dialog(DialogKind::Help);
            }
        }
        input::InputAction::ToggleDialogKeys => {
            let open = !app.dialog_keys();
            app.set_dialog_keys(open);
        }
        input::InputAction::DialogKeysScroll { delta } => {
            if let Some(mut ent) = app.surfaces.surfaces.take_active_view() {
                let outcome = ent.handle_input(
                    &input::InputAction::DialogKeysScroll { delta },
                    app,
                    viewed_session_id,
                );
                app.surfaces.surfaces.put_active_view(ent);
                if outcome.is_consumed() {
                    return ActionFlow::Handled;
                }
            }
            if let Some(scroll) = app.dialog_keys_scroll() {
                if delta < 0 {
                    *scroll = scroll.saturating_sub((-delta) as usize);
                } else {
                    *scroll = scroll.saturating_add(delta as usize);
                }
            }
        }
        input::InputAction::ModalUp => {
            modals::handle_modal_up(app, viewed_session_id);
        }
        input::InputAction::ModalDown => {
            modals::handle_modal_down(app, viewed_session_id);
        }
        input::InputAction::QuestionUp
        | input::InputAction::QuestionDown
        | input::InputAction::QuestionToggle
        | input::InputAction::QuestionSelect(_)
        | input::InputAction::QuestionSubmit
        | input::InputAction::QuestionPrevious
        | input::InputAction::QuestionNext
        | input::InputAction::QuestionCancel
        | input::InputAction::PreAttachUp
        | input::InputAction::PreAttachDown
        | input::InputAction::PreAttachToggle
        | input::InputAction::PreAttachSubmit
        | input::InputAction::PreAttachCancel
        | input::InputAction::InputSubmit
        | input::InputAction::InputCancel
        | input::InputAction::QuestionInsertChar(_)
        | input::InputAction::QuestionBackspace
        | input::InputAction::PermissionPrevOption
        | input::InputAction::PermissionNextOption
        | input::InputAction::PermissionSubmit
        | input::InputAction::PermissionReject
        | input::InputAction::PermissionBack => {
            questions::handle_question_action(app, runtime, action).await;
        }
        _ => {}
    }
    ActionFlow::Handled
}

fn handle_telemetry_usage_action(
    app: &mut App,
    runtime: &UiRuntime,
    viewed_session_id: &str,
    action: input::InputAction,
) -> ActionFlow {
    match action {
        input::InputAction::QuotasActivate => {
            if let Some(snap) = &app.telemetry.provider_quotas {
                let idx = app.active_index();
                if let Some(entry) = snap.entries.get(idx) {
                    let conn_name = entry.name.clone();
                    app.send_intent(nuo_protocol::AgentRequest::SwitchConnection {
                        provider: conn_name.clone(),
                        model: String::new(),
                        api_key: None,
                        base_url: None,
                    });
                    show_local_toast(
                        app,
                        format!("Switched active connection to `{conn_name}`"),
                        false,
                        std::time::Duration::from_millis(1500),
                    );
                }
            }
        }
        input::InputAction::RefreshQuotas => {
            show_local_toast(
                app,
                "Refreshing provider quotas…",
                false,
                std::time::Duration::from_millis(1500),
            );
            let filter = app
                .telemetry
                .provider_quotas
                .as_ref()
                .and_then(|q| q.provider_filter.clone());
            app.send_intent(nuo_protocol::AgentRequest::QueryProviderQuotas {
                provider: filter,
                force_refresh: true,
            });
        }
        input::InputAction::QuotasToggleDetail => {
            // Toggle the detail block of the selected quota row.
            let idx = app.active_index();
            app.surfaces
                .surfaces
                .dlg_mut::<crate::shell::surfaces::QuotasDialog>()
                .expanded = if app
                .surfaces
                .surfaces
                .dlg_mut::<crate::shell::surfaces::QuotasDialog>()
                .expanded
                == Some(idx)
            {
                None
            } else {
                Some(idx)
            };
            app.set_active_follow(true);
        }
        input::InputAction::TraceInspectDetail => {
            if app.active_dialog() == Some(DialogKind::SessionTrace) {
                if !app
                    .surfaces
                    .surfaces
                    .dlg_mut::<crate::shell::surfaces::SessionTraceDialog>()
                    .detail
                {
                    let has_rounds = app
                        .token_source_report(viewed_session_id)
                        .map(|report| render::telemetry_round_count(&report) > 0)
                        .unwrap_or(false);
                    if has_rounds {
                        app.surfaces
                            .surfaces
                            .dlg_mut::<crate::shell::surfaces::SessionTraceDialog>()
                            .detail = true;
                        app.surfaces
                            .surfaces
                            .dlg_mut::<crate::shell::surfaces::SessionTraceDialog>()
                            .turn_cursor = 0;
                        app.surfaces
                            .surfaces
                            .dlg_mut::<crate::shell::surfaces::SessionTraceDialog>()
                            .scroll = 0;
                    }
                } else if app
                    .surfaces
                    .surfaces
                    .dlg_mut::<crate::shell::surfaces::SessionTraceDialog>()
                    .turn
                    .is_none()
                {
                    let report = app.token_source_report(viewed_session_id);
                    let round_index = app.active_index().min(
                        report
                            .as_ref()
                            .map(|report| render::telemetry_round_count(report).saturating_sub(1))
                            .unwrap_or(0),
                    );
                    if let Some(key) = report.as_ref().and_then(|report| {
                        render::telemetry_attempt_key(
                            report,
                            round_index,
                            app.surfaces
                                .surfaces
                                .dlg_mut::<crate::shell::surfaces::SessionTraceDialog>()
                                .turn_cursor,
                        )
                    }) {
                        app.surfaces
                            .surfaces
                            .dlg_mut::<crate::shell::surfaces::SessionTraceDialog>()
                            .turn = Some(key);
                        app.surfaces
                            .surfaces
                            .dlg_mut::<crate::shell::surfaces::SessionTraceDialog>()
                            .scroll = 0;
                    }
                }
            }
        }
        input::InputAction::ThreadStatsOpenTrace => {
            enter_panel(
                app,
                crate::shell::surfaces::DialogKind::SessionTrace,
                runtime,
                viewed_session_id,
            );
        }
        input::InputAction::OpenThreadStats => {
            enter_panel(
                app,
                crate::shell::surfaces::DialogKind::ThreadStats,
                runtime,
                viewed_session_id,
            );
        }
        input::InputAction::OpenSessionTrace => {
            enter_panel(
                app,
                crate::shell::surfaces::DialogKind::SessionTrace,
                runtime,
                viewed_session_id,
            );
        }
        _ => {}
    }
    ActionFlow::Handled
}

fn handle_scroll_action(
    app: &mut App,
    runtime: &UiRuntime,
    viewed_session_id: &str,
    action: input::InputAction,
) -> ActionFlow {
    match action {
        input::InputAction::ScrollUp => {
            if let Some(mut ent) = app.surfaces.surfaces.take_active_view() {
                let outcome =
                    ent.handle_input(&input::InputAction::ScrollUp, app, viewed_session_id);
                app.surfaces.surfaces.put_active_view(ent);
                if outcome.is_consumed() {
                    return ActionFlow::Handled;
                }
            }
            scroll_tick(app, false);
        }
        input::InputAction::ScrollDown => {
            if let Some(mut ent) = app.surfaces.surfaces.take_active_view() {
                let outcome =
                    ent.handle_input(&input::InputAction::ScrollDown, app, viewed_session_id);
                app.surfaces.surfaces.put_active_view(ent);
                if outcome.is_consumed() {
                    return ActionFlow::Handled;
                }
            }
            scroll_tick(app, true);
        }
        input::InputAction::Wheel { up, x, y } => {
            handle_wheel(app, up, x, y, runtime, viewed_session_id);
        }
        input::InputAction::ScrollPageUp => {
            if let Some(mut ent) = app.surfaces.surfaces.take_active_view() {
                let outcome =
                    ent.handle_input(&input::InputAction::ScrollPageUp, app, viewed_session_id);
                app.surfaces.surfaces.put_active_view(ent);
                if outcome.is_consumed() {
                    return ActionFlow::Handled;
                }
            }
            // Read the (Copy) page step up front so the subsequent
            // mutable borrow of the scroll field doesn't conflict.
            let step = modal_page_step(app);
            if let Some((scroll, follow)) = app.modal_scroll_field() {
                if let Some(f) = follow {
                    *f = false;
                }
                *scroll = scroll.saturating_sub(step);
            } else {
                scroll_transcript_page(app, false);
            }
        }
        input::InputAction::ScrollPageDown => {
            if let Some(mut ent) = app.surfaces.surfaces.take_active_view() {
                let outcome =
                    ent.handle_input(&input::InputAction::ScrollPageDown, app, viewed_session_id);
                app.surfaces.surfaces.put_active_view(ent);
                if outcome.is_consumed() {
                    return ActionFlow::Handled;
                }
            }
            // Read the (Copy) page step up front so the subsequent
            // mutable borrow of the scroll field doesn't conflict.
            let step = modal_page_step(app);
            if let Some((scroll, follow)) = app.modal_scroll_field() {
                if let Some(f) = follow {
                    *f = false;
                }
                *scroll = scroll.saturating_add(step);
            } else {
                scroll_transcript_page(app, true);
            }
        }
        input::InputAction::ScrollTop => {
            if let Some(mut ent) = app.surfaces.surfaces.take_active_view() {
                let outcome =
                    ent.handle_input(&input::InputAction::ScrollTop, app, viewed_session_id);
                app.surfaces.surfaces.put_active_view(ent);
                if outcome.is_consumed() {
                    return ActionFlow::Handled;
                }
            }
            if let Some((scroll, follow)) = app.modal_scroll_field() {
                if let Some(f) = follow {
                    *f = false;
                }
                *scroll = 0;
            } else {
                scroll_transcript_to_edge(app, false);
            }
        }
        input::InputAction::ScrollBottom => {
            if let Some(mut ent) = app.surfaces.surfaces.take_active_view() {
                let outcome =
                    ent.handle_input(&input::InputAction::ScrollBottom, app, viewed_session_id);
                app.surfaces.surfaces.put_active_view(ent);
                if outcome.is_consumed() {
                    return ActionFlow::Handled;
                }
            }
            // Modal scroll bounds are clamped by render_body each
            // frame, so a large number here just means "go to end".
            if let Some((scroll, follow)) = app.modal_scroll_field() {
                if let Some(f) = follow {
                    *f = false;
                }
                *scroll = usize::MAX;
            } else {
                scroll_transcript_to_edge(app, true);
            }
        }
        input::InputAction::PermissionDetailsUp => {
            app.human.permission_scroll = app.human.permission_scroll.saturating_sub(1);
        }
        input::InputAction::PermissionDetailsDown => {
            app.human.permission_scroll = app
                .human
                .permission_scroll
                .saturating_add(1)
                .min(app.human.permission_max_scroll);
        }
        _ => {}
    }
    ActionFlow::Handled
}

fn handle_clipboard_export_action(
    app: &mut App,
    viewed_session_id: &str,
    copy_tx: &mpsc::UnboundedSender<Result<clipboard::CopyOutcome, String>>,
    copy_pending: &Arc<AtomicUsize>,
    action: input::InputAction,
) -> ActionFlow {
    match action {
        input::InputAction::CopySelection => {
            // The global chord: copy whatever the keyboard is on, with one
            // resolution rule (`CopyRequest::for_current_focus`) so a drag and
            // a focused entry can never disagree about precedence. When nothing
            // is aimed at, say so instead of writing an empty string.
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
        input::InputAction::CopyFocusedTarget => {
            // On a folded command step, `y` is the divider's `copy` verb: the
            // user is pointing at a row that says "N lines hidden", so the answer
            // is those N rows. Every other focused entry keeps the verbatim-entry
            // meaning, because there is nothing hidden for the label to promise.
            if let Some(target) = app.transcript.focus.entry() {
                if let Some((text, feedback)) =
                    crate::shell::copy::fold_hidden_rows_copy(app, target.message_idx)
                {
                    clipboard_ops::spawn_clipboard_copy(copy_tx, copy_pending.clone(), text);
                    app.surfaces.copy_toast_message = feedback;
                    app.surfaces.copy_toast_failed = false;
                    app.surfaces.copy_toast_until =
                        Some(std::time::Instant::now() + std::time::Duration::from_millis(1500));
                } else {
                    clipboard_ops::copy(
                        app,
                        crate::shell::copy::CopyRequest::Entry {
                            message_idx: target.message_idx,
                        },
                        viewed_session_id,
                        copy_tx,
                        copy_pending,
                    );
                }
            }
        }
        input::InputAction::CommandLogCopy => {
            // The receipt is derived from the same value the bytes came from, so
            // the toast cannot describe a different slice than the clipboard got.
            let Some(log) = app.transcript.command_log.as_ref() else {
                return ActionFlow::Handled;
            };
            let text = log.copy_text();
            if text.is_empty() {
                show_local_toast(
                    app,
                    "this log has no rows to copy",
                    true,
                    std::time::Duration::from_millis(1600),
                );
                return ActionFlow::Handled;
            }
            let feedback = log.copy_feedback();
            clipboard_ops::spawn_clipboard_copy(copy_tx, copy_pending.clone(), text);
            app.surfaces.copy_toast_message = feedback;
            app.surfaces.copy_toast_failed = false;
            app.surfaces.copy_toast_until =
                Some(std::time::Instant::now() + std::time::Duration::from_millis(1500));
        }
        input::InputAction::CommandLogToggleScope => {
            if let Some(log) = app.transcript.command_log.as_mut()
                && log.can_scope_toggle()
            {
                log.scope = log.scope.toggled();
                // The two scopes have different lengths, so a scroll offset that
                // was valid in one is out of range in the other. Re-anchor at the
                // top rather than clamping into the middle: the reader just asked
                // to see a different slice, and the first row of it is the answer.
                app.surfaces.command_log_scroll = 0;
            }
        }
        input::InputAction::CopyFocusedDiagnostic => {
            if let Some(request) = crate::shell::copy::diagnostic_for_current_focus(app) {
                clipboard_ops::copy(app, request, viewed_session_id, copy_tx, copy_pending);
            }
        }
        _ => {}
    }
    ActionFlow::Handled
}

fn handle_notice_action(
    app: &mut App,
    viewed_session_id: &str,
    copy_tx: &mpsc::UnboundedSender<Result<clipboard::CopyOutcome, String>>,
    copy_pending: &Arc<AtomicUsize>,
    action: input::InputAction,
) -> ActionFlow {
    match action {
        input::InputAction::OpenNotices => {
            // Ctrl-o / `/notices`: the notice log viewer. Opening marks the
            // log read, so the badge stops claiming there is something unseen
            // the moment the user is actually looking at it.
            app.toggle_notice_viewer();
        }
        input::InputAction::NoticesSelect { forward } => {
            let count = app.notice_count();
            if count > 0 && app.notice_viewer_open() {
                let dlg = app
                    .surfaces
                    .surfaces
                    .dlg_mut::<crate::shell::surfaces::NoticesDialog>();
                if forward {
                    dlg.index = (dlg.index + 1).min(count - 1);
                } else {
                    dlg.index = dlg.index.saturating_sub(1);
                }
                dlg.follow = false;
                dlg.detail = false;
            }
        }
        input::InputAction::NoticesPage { forward } => {
            if app.notice_viewer_open() {
                let page = app
                    .surfaces
                    .surfaces
                    .dlg::<crate::shell::surfaces::NoticesDialog>()
                    .body_height
                    .max(1) as usize;
                let dlg = app
                    .surfaces
                    .surfaces
                    .dlg_mut::<crate::shell::surfaces::NoticesDialog>();
                dlg.scroll = if forward {
                    dlg.scroll.saturating_add(page)
                } else {
                    dlg.scroll.saturating_sub(page)
                };
                dlg.follow = false;
            }
        }
        input::InputAction::NoticesToggleDetail => {
            let count = app.notice_count();
            if count > 0 && app.notice_viewer_open() {
                let dlg = app
                    .surfaces
                    .surfaces
                    .dlg_mut::<crate::shell::surfaces::NoticesDialog>();
                dlg.detail = !dlg.detail;
            }
        }
        input::InputAction::NoticesClear => {
            app.clear_notices();
        }
        input::InputAction::NoticesCopy => match app.render_notices(Some(viewed_session_id)) {
            Some(text) => {
                clipboard_ops::spawn_clipboard_copy(copy_tx, copy_pending.clone(), text);
                show_local_toast(
                    app,
                    "notices copied",
                    false,
                    std::time::Duration::from_millis(1500),
                );
            }
            None => show_local_toast(
                app,
                "no notices retained yet",
                true,
                std::time::Duration::from_millis(1500),
            ),
        },
        _ => {}
    }
    ActionFlow::Handled
}

fn handle_queue_dispatch_action(
    app: &mut App,
    runtime: &UiRuntime,
    viewed_session_id: &str,
    action: input::InputAction,
) -> ActionFlow {
    match action {
        input::InputAction::ToggleSendMode => {
            queue::handle_queue_action(app, viewed_session_id, action);
        }
        input::InputAction::OpenQueue => {
            // The queue chord opens the queue overview — the full outbox list
            // that the activity strip previews (one line, below the
            // transcript). The selection starts at the front (the next item to
            // pop). This mirrors a click on the strip's queue affordance.
            //
            // A retained view (phase 4): the cursor/scroll survive hide. The
            // pause runs on EVERY entry (not just first open) because it is an
            // editing safety latch — the matching resume is the view's exit
            // hook, which also tells the server. A user pause set with `Ctrl+P`
            // / `/queue block` is the same flag; nothing else is layered here.
            enter_panel(
                app,
                crate::shell::surfaces::DialogKind::Queue,
                runtime,
                viewed_session_id,
            );
        }
        input::InputAction::RecallQueuedSelected
        | input::InputAction::QueueToggleBlock
        | input::InputAction::QueueDelete
        | input::InputAction::QueueMoveItem { .. } => {
            queue::handle_queue_action(app, viewed_session_id, action);
        }
        _ => {}
    }
    ActionFlow::Handled
}

async fn handle_interrupt_action(
    app: &mut App,
    runtime: &UiRuntime,
    viewed_session_id: &str,
    copy_tx: &mpsc::UnboundedSender<Result<clipboard::CopyOutcome, String>>,
    copy_pending: &Arc<AtomicUsize>,
    action: input::InputAction,
) -> ActionFlow {
    match action {
        input::InputAction::Interrupt => {
            // Mirror Ctrl+C's quit pattern: the first Esc only arms a
            // wall-clock 2s window (and shows a toast); the second Esc
            // within that window actually interrupts the running task. A
            // press after the window lapsed starts a fresh window rather
            // than firing a stale confirmation.
            handle_esc_interrupt_with_runtime(app, runtime, commands::InterruptTarget::Primary)
                .await;
        }
        input::InputAction::CtrlC => {
            return commands::handle_ctrl_c(app, viewed_session_id, copy_tx, copy_pending);
        }
        input::InputAction::InterruptSubagent => {
            // Esc inside the Subagent scene: interrupt only the
            // viewed child, with the same armed press-twice contract. It never
            // reaches the primary round, so the outer turn keeps running.
            handle_esc_interrupt_with_runtime(app, runtime, commands::InterruptTarget::Subagent)
                .await;
        }
        _ => {}
    }
    ActionFlow::Handled
}

async fn handle_mouse_action(
    app: &mut App,
    runtime: &UiRuntime,
    viewed_session_id: &str,
    action: input::InputAction,
) -> ActionFlow {
    match action {
        input::InputAction::SelectionStart { x, y } => {
            mouse::handle_selection_start(app, runtime, viewed_session_id, x, y).await;
        }
        input::InputAction::RightClick { x, y } => {
            mouse::handle_right_click(app, runtime, x, y).await;
        }
        input::InputAction::SelectionUpdate { x, y } => {
            mouse::handle_selection_update(app, x, y);
        }
        input::InputAction::SelectionEnd => {
            mouse::handle_selection_end(app);
        }
        input::InputAction::SelectBlock { x, y } => {
            mouse::handle_select_block(app, x, y);
        }
        input::InputAction::Hover { x, y } => {
            mouse::handle_hover(app, runtime, x, y).await;
        }
        _ => {}
    }
    ActionFlow::Handled
}
