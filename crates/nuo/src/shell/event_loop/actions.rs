//! Input-action dispatch for the TUI event loop: the `match` over
//! [`input::InputAction`] that `run_app_loop`'s input-drain stage ran inline,
//! moved here verbatim (one arm per variant) with only the loop-control
//! statements rewritten as [`ActionFlow`] values.
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use nuotc::Terminal;
use tokio::sync::mpsc;

use nuo_wire::{AgentRequest, PermissionDecision, PermissionRequest};

use crate::shell::App;
use crate::shell::clipboard;
use crate::shell::clipboard_ops;
use crate::shell::input;
use crate::shell::render;
use crate::shell::surfaces::{DialogKind, SceneKind, SheetKind};
use nuo_view::model::selection::SelectionState;

use super::runtime::UiRuntime;
use super::sync::show_local_toast;
use super::transcript::extract_selection_text;

mod commands;
mod host;
mod modals;
pub(crate) mod mouse;

#[allow(unused_imports)]
pub(crate) use modals::{
    activate_picked_model, effective_reasoning_effort, handle_open_model_detail,
    handle_permission_submit, handle_submit_model_editor, modal_page_step, question_effects,
};

pub(super) use commands::split_command_word;
#[allow(unused_imports)]
pub(crate) use commands::{
    InterruptTarget, handle_esc_interrupt, handle_esc_interrupt_with_runtime,
};

#[cfg(test)]
pub(crate) use commands::handle_ctrl_c;

#[cfg(test)]
pub(crate) use commands::handle_send_slash;
#[cfg(test)]
pub(crate) use modals::{
    handle_close_modal, handle_modal_down, handle_modal_up, handle_open_model_editor,
    handle_submit_custom_provider,
};

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

pub(super) struct ActionContext<'a> {
    pub runtime: &'a UiRuntime,
    pub session: &'a crate::shell::SessionSource,
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
fn scroll_tick(app: &mut App, down: bool) {
    if let Some((scroll, follow)) = app.modal_scroll_field() {
        if let Some(f) = follow {
            *f = false;
        }
        *scroll = if down {
            scroll.saturating_add(1)
        } else {
            scroll.saturating_sub(1)
        };
    } else if down {
        app.pin_summary_line = None;
        app.scroll = app.scroll.saturating_add(4).min(app.max_scroll);
        if app.scroll >= app.max_scroll {
            app.follow_bottom = true;
        }
    } else {
        // While a permission sheet is open the transcript stays scrollable,
        // so the wheel / page keys drive the thread behind it, not the
        // sheet's own body.
        app.follow_bottom = false;
        app.pin_summary_line = None;
        app.scroll = app.scroll.saturating_sub(4);
    }
}

fn scroll_transcript_page(app: &mut App, down: bool) {
    let step = app.view_height.saturating_sub(1).max(1);
    app.pin_summary_line = None;
    if down {
        app.scroll = app.scroll.saturating_add(step).min(app.max_scroll);
        if app.scroll >= app.max_scroll {
            app.follow_bottom = true;
        }
    } else {
        app.follow_bottom = false;
        app.scroll = app.scroll.saturating_sub(step);
    }
}

fn scroll_transcript_to_edge(app: &mut App, bottom: bool) {
    app.pin_summary_line = None;
    if bottom {
        app.scroll = app.max_scroll;
        app.follow_bottom = true;
    } else {
        app.scroll = 0;
        app.follow_bottom = false;
    }
}

fn select_connection_preset(app: &mut App, forced_method: Option<nuo_wire::LoginMethod>) {
    if !app.surfaces.contains_sheet(SheetKind::ProviderPreset) {
        return;
    }
    let Some(preset) = crate::PROVIDER_PRESETS.get(app.preset_choice) else {
        return;
    };
    if !preset.oauth_first() {
        if forced_method.is_some() {
            show_local_toast(
                app,
                "This connection signs in with an API key; there is no OAuth method to choose.",
                true,
                std::time::Duration::from_millis(2600),
            );
        } else {
            app.open_custom_provider_editor(preset);
        }
        return;
    }

    let method = forced_method.or_else(|| preset.auth.default_login_method());
    let Some(method) = method else {
        show_local_toast(
            app,
            "This connection has no available OAuth login method.",
            true,
            std::time::Duration::from_millis(2600),
        );
        return;
    };
    if !preset.auth.supports_login_method(method) {
        show_local_toast(
            app,
            match method {
                nuo_wire::LoginMethod::Browser => {
                    "Browser PKCE login is not supported by this connection."
                }
                nuo_wire::LoginMethod::Device => {
                    "Device login is not supported by this connection."
                }
            },
            true,
            std::time::Duration::from_millis(2600),
        );
        return;
    }

    app.begin_oauth_add(preset, method);
    app.send_intent(AgentRequest::AuthorizeOAuth {
        method,
        auth: preset.auth.clone(),
    });
}

/// Loop stage: dispatch one drained [`input::InputAction`]. The match body is
/// verbatim from `run_app_loop`; only `continue` / `return Ok(())` inside arms
/// became [`ActionFlow`] values, and the clipboard senders / viewed session id
/// are passed explicitly instead of captured.
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
        && app.host_kill_confirm.is_some()
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

    // An armed scene namespace survives exactly one keystroke: any keystroke
    // other than re-arming it disarms it, so a half-typed chord cannot linger
    // and change the meaning of a later, unrelated key. Pointer events are
    // deliberately excluded from this state machine: a `C-x` typed while the
    // pointer reports motion (or a spontaneous resize lands mid-chord) used to
    // be cancelled by that non-keystroke event, which is why `C-x w` could
    // appear to need two attempts.
    if key_event
        && app.scene_namespace_armed
        && !matches!(action, input::InputAction::SetSceneNamespaceArmed(_))
    {
        app.scene_namespace_armed = false;
    }

    match action {
        input::InputAction::None => {}
        input::InputAction::TerminalResized { cols, rows } => {
            // A resize is the prime trigger for crossterm splitting an
            // in-flight SGR mouse sequence across reads (issue #854).
            // Re-arm mouse capture so both crossterm's parser and the
            // terminal's mouse-tracking state start from a clean slate.
            terminal.resize_to(cols, rows);
            app.layout_height_cache.clear();
            if !app.follow_bottom {
                app.scroll_settle_pending = true;
            }
            if app.profile.supports_mouse {
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
            let next_idx = (app.surfaces.active_tab_index() + 1) % app.surfaces.tabs().len().max(1);
            return activate_tab_at_index(app, next_idx, runtime, viewed_session_id);
        }
        input::InputAction::PrevTab => {
            let total = app.surfaces.tabs().len().max(1);
            let prev_idx = if app.surfaces.active_tab_index() == 0 {
                total - 1
            } else {
                app.surfaces.active_tab_index() - 1
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
        input::InputAction::SendChat(text) => {
            commands::handle_send_chat(app, runtime, viewed_session_id, text).await;
        }
        input::InputAction::SteerImmediate(text) => {
            commands::handle_send_steer(app, runtime, viewed_session_id, text).await;
        }
        input::InputAction::QueueFollowUp(text) => {
            commands::handle_queue_follow_up(app, runtime, viewed_session_id, text).await;
        }
        input::InputAction::ToggleSendMode => {
            app.composer_send_mode = match app.composer_send_mode {
                crate::shell::app::ComposerSendMode::Steer => {
                    crate::shell::app::ComposerSendMode::FollowUp
                }
                crate::shell::app::ComposerSendMode::FollowUp => {
                    crate::shell::app::ComposerSendMode::Steer
                }
            };
        }
        input::InputAction::WorkspaceGuardrailNotice(notice) => {
            show_local_toast(app, notice, false, std::time::Duration::from_millis(3500));
        }
        input::InputAction::SendSlash(cmd) => {
            return commands::handle_send_slash(app, runtime, session, cmd).await;
        }
        input::InputAction::ProviderPickerActivate => {
            // Activate is a Models-only action: the flat (provider,
            // model) pair under the highlight. The Connections list has
            // no activate concept — it only manages instances, so Enter
            // never produces this action there. Both the Models picker
            // and the key editor share one activation path (key-ready /
            // OAuth / key editor) via `activate_picked_model`.
            let key_ready = |app: &App, id: &str| app.key_status.get(id).copied().unwrap_or(true);
            let in_detail = app
                .surfaces
                .dlg::<crate::shell::surfaces::ModelsDialog>()
                .info_detail;
            let target = if app.active_dialog() == Some(DialogKind::Models) {
                let rows = app.models_flat_filtered();
                let picked = rows
                    .get(app.active_index())
                    .or_else(|| rows.iter().find(|r| r.usable));
                if in_detail {
                    if let Some(row) = picked {
                        if !row.usable {
                            return ActionFlow::Handled;
                        }
                        app.surfaces
                            .dlg_mut::<crate::shell::surfaces::ModelsDialog>()
                            .info_detail = false;
                        Some((row.provider_id.clone(), row.model.clone()))
                    } else {
                        None
                    }
                } else if let Some(row) = picked {
                    if !row.usable {
                        modals::handle_open_model_detail(app);
                        return ActionFlow::Handled;
                    }
                    Some((row.provider_id.clone(), row.model.clone()))
                } else {
                    None
                }
            } else {
                None
            };
            if let Some((id, model)) = target {
                let ready = key_ready(app, &id);
                activate_picked_model(app, id, model, ready);
            }
        }
        input::InputAction::CustomProviderNextField => {
            if app.surfaces.contains_sheet(SheetKind::CustomProvider) {
                app.cycle_custom_field(true);
            }
        }
        input::InputAction::CustomProviderPrevField => {
            if app.surfaces.contains_sheet(SheetKind::CustomProvider) {
                app.cycle_custom_field(false);
            }
        }
        input::InputAction::ScrollCustomProvider { forward } => {
            if app.surfaces.contains_sheet(SheetKind::CustomProvider) {
                app.scroll_custom_provider(forward);
            }
        }
        input::InputAction::CycleCustomProviderChoice { forward } => {
            if app.surfaces.contains_sheet(SheetKind::CustomProvider) {
                app.cycle_custom_choice(forward);
            }
        }
        input::InputAction::MovePresetChoice { forward } => {
            if app.surfaces.contains_sheet(SheetKind::ProviderPreset) {
                app.move_preset_choice(forward);
            }
        }
        input::InputAction::SelectPreset => {
            select_connection_preset(app, None);
        }
        input::InputAction::SelectPresetWithOauthMethod { method } => {
            select_connection_preset(app, Some(method));
        }
        input::InputAction::CancelOauthPending => {
            app.cancel_oauth_pending();
        }
        input::InputAction::CycleOauthSelection => {
            if app.surfaces.contains_sheet(SheetKind::OAuthPending) {
                app.cycle_oauth_selection();
            }
        }
        input::InputAction::CopyOauthContent { target } => {
            // If the user has active text selected in the modal, copy that selection first
            if let Some(text) = extract_selection_text(
                &app.selection,
                app.focused_messages(),
                &app.input,
                &app.ui.document,
                app.drag.cell_info.as_ref(),
            ) {
                clipboard_ops::spawn_clipboard_copy(copy_tx, copy_pending.clone(), text);
                app.copy_toast_message = "Selection copied to clipboard".to_string();
                app.copy_toast_failed = false;
                app.copy_toast_until =
                    Some(std::time::Instant::now() + std::time::Duration::from_millis(1500));
                return ActionFlow::Handled;
            }
            // Copy the OAuth pending sheet's primary field to the
            // system clipboard.
            let actual_target = match target {
                input::OauthCopyTarget::Selected => app.oauth_selected_target(),
                input::OauthCopyTarget::UserCode => input::OauthCopyTarget::UserCode,
                input::OauthCopyTarget::Url => input::OauthCopyTarget::Url,
            };
            let (text, label) = match actual_target {
                input::OauthCopyTarget::UserCode => (
                    app.oauth_pending_user_code.clone(),
                    "Code copied to clipboard",
                ),
                input::OauthCopyTarget::Url | input::OauthCopyTarget::Selected => {
                    (app.oauth_pending_url.clone(), "Link copied to clipboard")
                }
            };
            if !text.is_empty() {
                clipboard_ops::spawn_clipboard_copy(copy_tx, copy_pending.clone(), text);
                app.copy_toast_message = label.to_string();
                app.copy_toast_failed = false;
                app.copy_toast_until =
                    Some(std::time::Instant::now() + std::time::Duration::from_millis(1500));
            }
        }
        input::InputAction::CancelPresetChooser => {
            app.cancel_preset_chooser();
        }
        input::InputAction::DeleteProvider => {
            // Connections `Shift+D`: stage the highlighted custom
            // provider for deletion and open the confirm overlay over
            // the list (dimmed backdrop + centered panel). The actual
            // `AgentRequest::DeleteConnection` only fires once the user
            // confirms inside the overlay. Built-in providers and the
            // synthetic "＋ Add connection" row are ignored by the
            // helper.
            app.stage_provider_delete();
        }
        input::InputAction::DeleteProviderConfirm => {
            // The confirm overlay's Enter-on-Delete: dispatch the
            // staged deletion and tear the overlay down.
            if let Some(req) = app.confirm_provider_delete() {
                app.send_intent(req);
            }
        }
        input::InputAction::DeleteProviderCancel => {
            // Esc / Ctrl+C / Enter-on-Cancel inside the confirm
            // overlay: drop the staged provider id and return keyboard
            // focus to the Connections list. The modal itself stays
            // open.
            app.cancel_provider_delete();
        }
        input::InputAction::CancelCustomProvider => {
            app.cancel_custom_provider();
        }
        input::InputAction::SubmitCustomProvider => {
            modals::handle_submit_custom_provider(app);
        }
        input::InputAction::ModelEnterSearch => {
            // `/` in browse mode: enter the search sub-layer. The input
            // line is already empty (held in `stashed_input`); typing now
            // builds the fuzzy query and re-ranks the active picker's
            // rows. Shared by the Connections and Models pickers.
            if matches!(
                app.active_dialog(),
                Some(DialogKind::Connections | DialogKind::Models)
            ) {
                app.set_picker_search(true);
                app.reset_picker_nav();
            }
        }
        input::InputAction::ModelExitSearch => {
            // First Esc while searching: drop the query and return to the
            // full browse list. The chat draft stays parked in
            // `stashed_input` until the modal closes for real.
            if matches!(
                app.active_dialog(),
                Some(DialogKind::Connections | DialogKind::Models)
            ) {
                app.set_picker_search(false);
                app.input.clear();
                app.set_cursor(0);
                app.input_scroll = 0;
                app.suggestion_index = None;
                app.reset_picker_nav();
            }
        }
        input::InputAction::ProviderPickerToggleFavorite => {
            // Models only (gated in input): toggle the favorite on the
            // highlighted MODEL (falling back to the first visible usable row).
            // Favorite is model-level (ADR-0046), so the id is the
            // model wire id. Sending the request is enough; the backend
            // pushes a fresh snapshot that flips the ★ next frame.
            if app.active_dialog() == Some(DialogKind::Models) {
                let ranked = app.models_flat_filtered();
                if let Some(row) = ranked
                    .get(app.active_index())
                    .or_else(|| ranked.iter().find(|r| r.usable))
                    && row.usable
                {
                    app.send_intent(AgentRequest::ToggleFavorite {
                        id: row.model.clone(),
                    });
                }
            }
        }
        input::InputAction::ProviderPickerBlockModel => {
            // 'x' blocks/intercepts the highlighted model from the connection pipe.
            if app.active_dialog() == Some(DialogKind::Models) {
                let ranked = app.models_flat_filtered();
                if let Some(row) = ranked.get(app.active_index()).or_else(|| ranked.first()) {
                    app.send_intent(AgentRequest::ExcludeModel {
                        scope: nuo_wire::model::ModelTargetScope::Connection(
                            row.provider_id.clone(),
                        ),
                        model_id: row.model.clone(),
                    });
                }
            }
        }
        input::InputAction::OpenModelEditor => {
            modals::handle_open_model_editor(app);
        }
        input::InputAction::ModelEditorNextField => {
            // Cycle focus through the per-model editor's fields: effort
            // (1) ↔ thinking (2). ADR-0046: the provider key editor has
            // only an API-key field, so Tab is a no-op there
            // (it never reaches this branch — `editor_model_settings_only`
            // gates it). The focused text field owns the composer line;
            // the thinking field is a toggle (no text), so it clears the
            // line while focused.
            if app.editor_model_settings_only {
                // The settings editor owns fields 1..=4 (effort, thinking
                // when available, then the capability overrides 3/4).
                // Tab wraps 1 ↔ 4 when the override rows are shown.
                let has_overrides = app.editor_model_settings_only;
                match app.editor_field {
                    1 if app.editor_thinking_available => {
                        app.editor_effort = app.input.clone();
                        app.input.clear();
                        app.set_cursor(0);
                        app.editor_field = 2;
                    }
                    2 if app.editor_thinking_available => {
                        app.input = app.editor_effort.clone();
                        app.set_cursor_end();
                        if has_overrides {
                            app.editor_field = 3;
                        } else {
                            app.editor_field = 1;
                        }
                    }
                    3 => {
                        app.editor_field = 4;
                    }
                    4 => {
                        app.input = app.editor_effort.clone();
                        app.set_cursor_end();
                        app.editor_field = 1;
                    }
                    _ => {
                        app.input = app.editor_effort.clone();
                        app.set_cursor_end();
                        if has_overrides {
                            app.editor_field = 3;
                        } else {
                            app.editor_field = 1;
                        }
                    }
                }
            }
        }
        input::InputAction::ModelEditorEffortCycle { delta } => {
            // Cycle the effort selector through the selected model's
            // supported wire levels, wrapping at both ends. Mirrored
            // into app.input so the renderer shows the live value.
            //
            // The ladder is the one captured from the snapshot when the editor
            // opened — this binary does not link `nuo-providers`, so
            // `resolve_model` cannot see the provider baseline tables and would
            // return an empty ladder, making the cycle a no-op.
            let levels: &[String] = &app.editor_effort_levels;
            if levels.is_empty() {
                return ActionFlow::NextEvent;
            }
            let cur = levels
                .iter()
                .position(|l| l == &app.editor_effort)
                .unwrap_or_else(|| {
                    levels
                        .iter()
                        .position(|l| l == "medium")
                        .or_else(|| levels.iter().position(|l| l == "high"))
                        .unwrap_or(0)
                }) as isize;
            let n = levels.len() as isize;
            let next = ((cur + delta as isize).rem_euclid(n)) as usize;
            app.editor_effort = levels[next].clone();
            app.input = app.editor_effort.clone();
            app.set_cursor_end();
        }
        input::InputAction::ModelEditorEffortJump { index } => {
            // Jump straight to a ladder rung (digit on the effort
            // field). Out-of-range digits are ignored so a 7-rung key
            // on a 3-rung ladder is a no-op, never a clamp that would
            // surprise. Mirrors `editor_effort` into `app.input` like
            // the cycle path so the renderer shows the live value.
            if let Some(level) = app.editor_effort_levels.get(index).cloned() {
                app.editor_effort = level;
                app.input = app.editor_effort.clone();
                app.set_cursor_end();
            }
        }
        input::InputAction::ModelEditorThinkingToggle => {
            // Toggle extended thinking on/off (Space). Orthogonal to
            // effort — the two knobs are independent on the wire.
            if app.editor_thinking_available {
                app.editor_thinking = !app.editor_thinking;
            }
        }
        input::InputAction::ModelEditorVisionCycle => {
            // Cycle the vision capability override (layer 1):
            // inherit → force on → force off → inherit.
            app.editor_vision_override = cycle_tri_state(app.editor_vision_override);
        }
        input::InputAction::ModelEditorToolCycle => {
            // Cycle the tool-call capability override, same tri-state.
            app.editor_tool_override = cycle_tri_state(app.editor_tool_override);
        }
        input::InputAction::SubmitModelEditor => {
            return modals::handle_submit_model_editor(app);
        }
        input::InputAction::Interrupt => {
            // Mirror Ctrl+C's quit pattern: the first Esc only arms a
            // wall-clock 2s window (and shows a toast); the second Esc
            // within that window actually interrupts the running task. A
            // press after the window lapsed starts a fresh window rather
            // than firing a stale confirmation.
            handle_esc_interrupt_with_runtime(app, runtime, commands::InterruptTarget::Primary)
                .await;
        }
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
                    .dlg_mut::<crate::shell::surfaces::ConnectionsDialog>()
                    .info_detail
            {
                let id = app
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
                    if let Some(detail) = app.connection_detail.as_mut() {
                        detail.usage = nuo_wire::ConnectionUsageState::Fetching;
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
                    .dlg_mut::<crate::shell::surfaces::ModelsDialog>()
                    .refreshing
                    || app
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
                        .dlg_mut::<crate::shell::surfaces::ModelsDialog>()
                        .refreshing = true;
                    app.surfaces
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
        input::InputAction::HistoryInsert => {
            // Enter / Tab inside the Ctrl+R panel: pull the focused entry out
            // of `history_rows` (the filtered matches) and drop it into
            // the input box for further editing / sending. The message
            // is not shipped here — the user hits Enter again to send.
            let ranked = app.history_rows();
            let pick = ranked
                .get(
                    app.surfaces
                        .dlg_mut::<crate::shell::surfaces::HistorySearchDialog>()
                        .index,
                )
                .or_else(|| ranked.first());
            let Some((orig_idx, _)) = pick else {
                return ActionFlow::Handled;
            };
            let original = *orig_idx;
            let text = app.input_history[original].text.clone();
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
                app.pending_images.clone(),
                app.pending_text_pastes.clone(),
                crate::shell::app::DraftAdoption::Replace,
            );
            // Clear the stashed draft capsule since user committed a historical entry
            app.clear_stashed_draft();
            // The selection replaces the in-progress draft, and history search completes.
            app.surfaces.dismiss_all_overlays();
            app.surfaces
                .dlg_mut::<crate::shell::surfaces::HistorySearchDialog>()
                .index = 0;
            app.input_scroll = 0;
            app.suggestion_index = None;
            // A programmatic input replacement — latch the dismissal so
            // a slash-command selection doesn't flash its completion
            // popup until the next real edit.
            app.completion_dismissed = true;
            app.reset_to_thread();
        }
        input::InputAction::HistoryDeleteSelected => {
            if app.active_composer_extension()
                == Some(crate::shell::composer_extension::ComposerExtensionKind::HistorySearch)
            {
                app.delete_selected_history_entry();
            }
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
        input::InputAction::QuotasActivate => {
            if let Some(snap) = &app.provider_quotas {
                let idx = app.active_index();
                if let Some(entry) = snap.entries.get(idx) {
                    let conn_name = entry.name.clone();
                    app.send_intent(nuo_wire::AgentRequest::SwitchConnection {
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
                .provider_quotas
                .as_ref()
                .and_then(|q| q.provider_filter.clone());
            app.send_intent(nuo_wire::AgentRequest::QueryProviderQuotas {
                provider: filter,
                force_refresh: true,
            });
        }
        input::InputAction::QuotasToggleDetail => {
            // Toggle the detail block of the selected quota row.
            let idx = app.active_index();
            app.surfaces
                .dlg_mut::<crate::shell::surfaces::QuotasDialog>()
                .expanded = if app
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
                .dlg_mut::<crate::shell::surfaces::SkillsDialog>()
                .expanded = if app
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
        input::InputAction::ConfigFocusToggle => {
            if app.current_scene() == SceneKind::Settings {
                app.config_focus = match app.config_focus {
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
                && crate::shell::overlays::ConfigCategory::from_index(app.config_category)
                    == crate::shell::overlays::ConfigCategory::WebReader
            {
                app.config_category = crate::shell::overlays::ConfigCategory::WebSearch as usize;
                app.config_detail_index = 0;
                app.config_detail_scroll = 0;
                app.config_hover_index = None;
            }
        }
        input::InputAction::ConfigSegmentNext => {
            if app.current_scene() == SceneKind::Settings
                && crate::shell::overlays::ConfigCategory::from_index(app.config_category)
                    == crate::shell::overlays::ConfigCategory::WebSearch
            {
                app.config_category = crate::shell::overlays::ConfigCategory::WebReader as usize;
                app.config_detail_index = 0;
                app.config_detail_scroll = 0;
                app.config_hover_index = None;
            }
        }
        input::InputAction::McpToggle => {
            // Connect/disconnect the selected server for the session.
            // The "enabled intent" is the inverse of its disabled flag;
            // the harness replies with a fresh snapshot.
            if let Some(server) = app
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
            if let Some(snapshot) = app.session_context.as_ref()
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
        input::InputAction::SessionSelect { forward } => {
            // List navigation is owned by the active dialog entity
            // (`DialogView::handle_input`, ADR-0035 §1).
            if let Some(mut ent) = app.surfaces.take_active_view() {
                let _ = ent.handle_input(
                    &input::InputAction::SessionSelect { forward },
                    app,
                    viewed_session_id,
                );
                app.surfaces.put_active_view(ent);
            }
        }
        input::InputAction::SessionActivate => {
            // Toggle the selected tool. The request is sent through the
            // normal agent channel; the harness replies with a fresh
            // snapshot that re-renders the dashboard.
            if let Some(req) = app.session_activate_request() {
                app.send_intent(req);
            }
        }
        input::InputAction::OpenSelectedSession => {
            let target = {
                let rows = crate::shell::overlays::session::project_session_rows(
                    &app.sessions_overview,
                    Some(
                        &app.surfaces
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
                app.startup_overlay = crate::shell::StartupOverlay::None;

                if id == viewed_session_id {
                    // Re-selecting the currently viewed thread is an idempotent focus recovery.
                    // Keep messages intact. If running, pin scroll to bottom for live output.
                    if app.running_sessions.contains(&id) {
                        app.scroll = 0;
                    }
                } else {
                    let is_empty = app.is_current_thread_empty();
                    let tab = crate::shell::surfaces::ClientTab::thread(&id, title.as_deref());
                    if is_empty {
                        // Current tab is an empty thread: discard it and overwrite the tab.
                        app.send_intent(AgentRequest::DeleteSession {
                            id: viewed_session_id.to_string(),
                        });
                        let active_idx = app.surfaces.active_tab_index();
                        if let Some(existing_pos) =
                            app.surfaces.tabs().iter().position(|t| match &t.kind {
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
                            app.surfaces.select_tab(new_pos);
                        } else {
                            app.replace_tab(active_idx, tab);
                        }
                    } else {
                        // Project into ClientTab workspace topology (ADR-0039, ADR-0041, ADR-0043)
                        app.open_tab(tab);
                    }

                    // Switching to another thread (running or idle):
                    // Purge legacy slash-command RPC (/sessions <id>).
                    // Trigger clean client viewport attach (ADR-0038 [INV-ACTION-02]).
                    app.switch_to_target = Some(id);
                    app.should_quit.store(true, Ordering::SeqCst);
                }
            }
        }
        input::InputAction::HostPreviewSelected => {
            // Enter on a dock selection opens the read-only preview
            // modal. Selection alone never opens it; Esc closes.
            let idx = app
                .modal_index
                .min(app.host_sessions.len().saturating_sub(1));
            let order = crate::shell::overlays::creation_order(&app.host_sessions);
            if let Some(row) = order.get(idx).map(|&i| &app.host_sessions[i]) {
                app.host_preview = Some(row.id.clone());
                app.host_preview_scroll = 0;
            }
        }
        input::InputAction::HostSwitchSelected => {
            let idx = app
                .modal_index
                .min(app.host_sessions.len().saturating_sub(1));
            // The dock renders sessions in creation order (`#seq`);
            // the selection indexes that sequence, not the raw
            // newest-first snapshot.
            let order = crate::shell::overlays::creation_order(&app.host_sessions);
            let target_info = order
                .get(idx)
                .and_then(|&i| app.host_sessions.get(i))
                .map(|r| (r.id.clone(), r.title.clone()));
            if let Some((target_id, target_title)) = target_info {
                // Switching to the current session is a no-op.
                let switchable = target_id != viewed_session_id;
                if switchable {
                    let is_empty = app.is_current_thread_empty();
                    let tab = crate::shell::surfaces::ClientTab::thread(
                        &target_id,
                        target_title.as_deref(),
                    );
                    if is_empty {
                        app.send_intent(AgentRequest::DeleteSession {
                            id: viewed_session_id.to_string(),
                        });
                        let active_idx = app.surfaces.active_tab_index();
                        if let Some(existing_pos) =
                            app.surfaces.tabs().iter().position(|t| match &t.kind {
                                crate::shell::surfaces::TabKind::Thread(tid) => tid == &target_id,
                                _ => false,
                            })
                        {
                            app.close_tab(active_idx);
                            let new_pos = if existing_pos > active_idx {
                                existing_pos - 1
                            } else {
                                existing_pos
                            };
                            app.surfaces.select_tab(new_pos);
                        } else {
                            app.replace_tab(active_idx, tab);
                        }
                    } else {
                        app.open_tab(tab);
                    }
                    app.switch_to_target = Some(target_id);
                    app.should_quit.store(true, Ordering::SeqCst);
                }
                app.dismiss_active_dialog();
                app.set_active_index(0);
                app.host_prompting = false;
            }
        }
        input::InputAction::HostFocusToggle => {
            app.host_focus = match app.host_focus {
                crate::shell::overlays::DashboardFocus::List => {
                    crate::shell::overlays::DashboardFocus::Detail
                }
                crate::shell::overlays::DashboardFocus::Detail => {
                    crate::shell::overlays::DashboardFocus::List
                }
            };
        }
        input::InputAction::HostInterruptSelected => {
            // `i` on the dock: interrupt the selection. Routed through the
            // console dispatcher so the dispatch line + receipt land in the
            // cockpit log alongside `/interrupt`.
            host::dispatch_console_command(app, runtime, "/interrupt", false).await;
        }
        input::InputAction::HostKillSelected => {
            // `k` on the dock: two-press confirm, then the kill verb.
            host::kill_selected(app, runtime);
        }
        input::InputAction::HostSuspendSelected => {
            // `s` on the dock: suspend the selection (park in memory).
            host::suspend_selected(app, runtime);
        }
        input::InputAction::HostPromptOpen => {
            // `p`: prompt the selected session. The composer buffer
            // becomes the task text.
            app.host_prompting = true;
            app.host_prompt_new = false;
            app.input.clear();
            app.set_cursor(0);
        }
        input::InputAction::HostNewSession => {
            // `n`: create a new session with the text as opening task.
            app.host_prompting = true;
            app.host_prompt_new = true;
            app.input.clear();
            app.set_cursor(0);
        }
        input::InputAction::HostPromptSeed(c) => {
            // A printable key on the dashboard opens the console composer
            // with that key as the first character — typing is opening.
            // The seeded role is "prompt the selection" (the `p` default):
            // an explicit `@N`/`/verb` in the line routes itself anyway.
            app.host_prompting = true;
            app.host_prompt_new = false;
            app.input.clear();
            app.input.insert(0, c);
            app.set_cursor(1);
        }
        input::InputAction::HostPromptSubmit => {
            let text = app.input.trim().to_string();
            // The `n`-opened prompt's default role is "create"; an explicit
            // address or verb in the text overrides it (`@3 …` still routes
            // to #3, `/kill` still kills).
            let create_new = app.host_prompt_new;
            app.host_prompting = false;
            app.host_prompt_new = false;
            app.input.clear();
            app.set_cursor(0);
            if text.is_empty() {
                return ActionFlow::Handled;
            }
            // The composer is a command line now (grammar plus
            // slash verbs): `@3 text` addresses, `/kill`-family manages,
            // bare text keeps the legacy role — prompt the selection, or
            // create when the prompt was opened with `n`.
            host::dispatch_console_command(app, runtime, &text, create_new).await;
        }
        input::InputAction::DeleteSelectedSession => {
            let rows = crate::shell::overlays::session::project_session_rows(
                &app.sessions_overview,
                Some(
                    &app.surfaces
                        .dlg_mut::<crate::shell::surfaces::SessionsDialog>()
                        .expanded,
                ),
            );
            let idx = app.active_index().min(rows.len().saturating_sub(1));
            if let Some(item) = rows.get(idx) {
                let id = item.session().id.clone();
                app.sessions_overview.retain(|s| s.id != id);
                app.surfaces
                    .dlg_mut::<crate::shell::surfaces::SessionsDialog>()
                    .expanded
                    .remove(&id);
                let new_rows = crate::shell::overlays::session::project_session_rows(
                    &app.sessions_overview,
                    Some(
                        &app.surfaces
                            .dlg_mut::<crate::shell::surfaces::SessionsDialog>()
                            .expanded,
                    ),
                );
                app.set_active_index(app.active_index().min(new_rows.len().saturating_sub(1)));
                app.send_intent(AgentRequest::DeleteSession { id });
            }
        }
        input::InputAction::CreateNewSession => {
            app.startup_overlay = crate::shell::StartupOverlay::None;
            app.dismiss_active_dialog();
            if app.is_current_thread_empty() {
                // If already in an empty thread, reset in place without adding a new tab.
                app.send_intent(AgentRequest::SlashCommand("/new".to_string()));
            } else {
                // Current thread has content: create a fresh thread in a new tab.
                app.switch_to_new = true;
                app.should_quit.store(true, Ordering::SeqCst);
            }
        }
        input::InputAction::OpenSessionInfo => {
            // Drill into the session-info sub-view for the highlighted
            // row. Request the full detail (complete last prompt,
            // timestamps) on demand — the picker rows only carry a
            // truncated preview. While the round-trip is in flight the
            // body shows a loading state.
            let rows = crate::shell::overlays::session::project_session_rows(
                &app.sessions_overview,
                Some(
                    &app.surfaces
                        .dlg_mut::<crate::shell::surfaces::SessionsDialog>()
                        .expanded,
                ),
            );
            if let Some(item) = rows.get(app.active_index().min(rows.len().saturating_sub(1))) {
                let session = item.session();
                app.surfaces
                    .dlg_mut::<crate::shell::surfaces::SessionsDialog>()
                    .info_detail = true;
                app.session_detail = None;
                app.surfaces
                    .dlg_mut::<crate::shell::surfaces::SessionsDialog>()
                    .info_scroll = 0;
                app.send_intent(AgentRequest::QuerySessionDetail {
                    id: session.id.clone(),
                });
            }
        }
        input::InputAction::ToggleSessionTimelineExpand => {
            let rows = crate::shell::overlays::session::project_session_rows(
                &app.sessions_overview,
                Some(
                    &app.surfaces
                        .dlg_mut::<crate::shell::surfaces::SessionsDialog>()
                        .expanded,
                ),
            );
            let idx = app.active_index().min(rows.len().saturating_sub(1));
            if let Some(item) = rows.get(idx) {
                // Any row with children toggles, at any depth: a fork of a fork
                // is a first-class branch and must expand like its parent.
                if item.child_count > 0 {
                    let id = item.session().id.clone();
                    let collapsed = app
                        .surfaces
                        .dlg_mut::<crate::shell::surfaces::SessionsDialog>()
                        .expanded
                        .remove(&id);
                    if !collapsed {
                        app.surfaces
                            .dlg_mut::<crate::shell::surfaces::SessionsDialog>()
                            .expanded
                            .insert(id);
                    }
                }
            }
        }
        input::InputAction::OpenConnectionDetail => {
            let providers = app.providers_filtered();
            if let Some(ranked) =
                providers.get(app.active_index().min(providers.len().saturating_sub(1)))
            {
                app.surfaces
                    .dlg_mut::<crate::shell::surfaces::ConnectionsDialog>()
                    .info_detail = true;
                app.surfaces
                    .dlg_mut::<crate::shell::surfaces::ConnectionsDialog>()
                    .info_standalone = false;
                app.connection_detail = None;
                app.surfaces
                    .dlg_mut::<crate::shell::surfaces::ConnectionsDialog>()
                    .info_scroll = 0;
                app.send_intent(AgentRequest::QueryConnectionDetail {
                    id: ranked.id.clone(),
                    force_refresh: false,
                });
            }
        }
        input::InputAction::OpenToolDetail => {
            let count = app.session_tools_len();
            if count > 0 {
                let tools = app
                    .surfaces
                    .dlg_mut::<crate::shell::surfaces::ToolsDialog>();
                tools.info_detail = true;
                tools.info_scroll = 0;
            }
        }
        input::InputAction::OpenModelDetail => {
            modals::handle_open_model_detail(app);
        }
        input::InputAction::OpenActiveConnectionDetail => {
            open_active_connection_detail(app, runtime, viewed_session_id);
        }
        input::InputAction::BrowseConnectionModels => {
            if let Some(detail) = &app.connection_detail {
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
        input::InputAction::CloseModal => {
            modals::handle_close_modal(app, viewed_session_id);
        }
        input::InputAction::SceneBack => {
            app.scene_back();
        }
        input::InputAction::SetSceneNamespaceArmed(armed) => {
            app.scene_namespace_armed = armed;
        }
        input::InputAction::CancelSceneNamespace => {
            app.scene_namespace_armed = false;
        }
        input::InputAction::TraceInspectDetail => {
            if app.active_dialog() == Some(DialogKind::SessionTrace) {
                if !app
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
                            .dlg_mut::<crate::shell::surfaces::SessionTraceDialog>()
                            .detail = true;
                        app.surfaces
                            .dlg_mut::<crate::shell::surfaces::SessionTraceDialog>()
                            .turn_cursor = 0;
                        app.surfaces
                            .dlg_mut::<crate::shell::surfaces::SessionTraceDialog>()
                            .scroll = 0;
                    }
                } else if app
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
                                .dlg_mut::<crate::shell::surfaces::SessionTraceDialog>()
                                .turn_cursor,
                        )
                    }) {
                        app.surfaces
                            .dlg_mut::<crate::shell::surfaces::SessionTraceDialog>()
                            .turn = Some(key);
                        app.surfaces
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
        input::InputAction::ToggleDialogKeys => {
            let open = !app.dialog_keys();
            app.set_dialog_keys(open);
        }
        input::InputAction::DialogKeysScroll { delta } => {
            if let Some(mut ent) = app.surfaces.take_active_view() {
                let outcome = ent.handle_input(
                    &input::InputAction::DialogKeysScroll { delta },
                    app,
                    viewed_session_id,
                );
                app.surfaces.put_active_view(ent);
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
        input::InputAction::ScrollUp => {
            if let Some(mut ent) = app.surfaces.take_active_view() {
                let outcome =
                    ent.handle_input(&input::InputAction::ScrollUp, app, viewed_session_id);
                app.surfaces.put_active_view(ent);
                if outcome.is_consumed() {
                    return ActionFlow::Handled;
                }
            }
            scroll_tick(app, false);
        }
        input::InputAction::ScrollDown => {
            if let Some(mut ent) = app.surfaces.take_active_view() {
                let outcome =
                    ent.handle_input(&input::InputAction::ScrollDown, app, viewed_session_id);
                app.surfaces.put_active_view(ent);
                if outcome.is_consumed() {
                    return ActionFlow::Handled;
                }
            }
            scroll_tick(app, true);
        }
        input::InputAction::Wheel { up, x, y } => {
            handle_wheel(app, up, x, y, viewed_session_id);
        }
        input::InputAction::ScrollPageUp => {
            if let Some(mut ent) = app.surfaces.take_active_view() {
                let outcome =
                    ent.handle_input(&input::InputAction::ScrollPageUp, app, viewed_session_id);
                app.surfaces.put_active_view(ent);
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
            if let Some(mut ent) = app.surfaces.take_active_view() {
                let outcome =
                    ent.handle_input(&input::InputAction::ScrollPageDown, app, viewed_session_id);
                app.surfaces.put_active_view(ent);
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
            if let Some(mut ent) = app.surfaces.take_active_view() {
                let outcome =
                    ent.handle_input(&input::InputAction::ScrollTop, app, viewed_session_id);
                app.surfaces.put_active_view(ent);
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
            if let Some(mut ent) = app.surfaces.take_active_view() {
                let outcome =
                    ent.handle_input(&input::InputAction::ScrollBottom, app, viewed_session_id);
                app.surfaces.put_active_view(ent);
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
            app.permission_scroll = app.permission_scroll.saturating_sub(1);
        }
        input::InputAction::PermissionDetailsDown => {
            app.permission_scroll = app
                .permission_scroll
                .saturating_add(1)
                .min(app.permission_max_scroll);
        }
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
            if let Some(target) = app.focused_target {
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
        input::InputAction::CopyFocusedDiagnostic => {
            if let Some(request) = crate::shell::copy::diagnostic_for_current_focus(app) {
                clipboard_ops::copy(app, request, viewed_session_id, copy_tx, copy_pending);
            }
        }
        input::InputAction::CtrlC => {
            return commands::handle_ctrl_c(app, viewed_session_id, copy_tx, copy_pending);
        }
        input::InputAction::OpenNotices => {
            // Ctrl-o / `/notices`: the notice log viewer. Opening marks the
            // log read, so the badge stops claiming there is something unseen
            // the moment the user is actually looking at it.
            app.toggle_notice_viewer();
        }
        input::InputAction::NoticesSelect { forward } => {
            let count = app.notice_count();
            if let Some(viewer) = app.notice_viewer.as_mut() {
                viewer.step(forward, count);
            }
        }
        input::InputAction::NoticesPage { forward } => {
            // One page is the panel's own body height, which its renderer wrote
            // back this frame; the fallback keeps the verb usable on the very
            // first keypress, before any paint has happened.
            let page = app
                .notice_viewer
                .as_ref()
                .map_or(1, |v| v.body_height.max(1)) as usize;
            if let Some(viewer) = app.notice_viewer.as_mut() {
                viewer.scroll = if forward {
                    viewer.scroll.saturating_add(page)
                } else {
                    viewer.scroll.saturating_sub(page)
                };
                viewer.follow = false;
            }
        }
        input::InputAction::NoticesToggleDetail => {
            let count = app.notice_count();
            if let Some(viewer) = app.notice_viewer.as_mut() {
                viewer.toggle_detail(count);
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
        input::InputAction::OpenQueue => {
            // F2 opens the queue overview — the full outbox list that
            // the persistent queue bar previews. The selection starts
            // at the front (the next item to pop). This mirrors a
            // click on the queue bar.
            //
            // A retained view (phase 4): the cursor/scroll
            // survive hide; the auto-block runs on EVERY entry (not just
            // first open) because it is an editing safety latch — the
            // matching resume is the view's exit hook (hide). A
            // persistent user block is a different thing (`F3` /
            // Ctrl+P), unaffected here.
            enter_panel(
                app,
                crate::shell::surfaces::DialogKind::Queue,
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
        input::InputAction::FocusNextTarget => {
            app.focus_interactive_target(1);
        }
        input::InputAction::FocusPrevTarget => {
            app.focus_interactive_target(-1);
        }
        input::InputAction::ClearFocusedTarget => {
            app.focused_target = None;
            app.transcript_focused = false;
        }
        input::InputAction::CancelHistoryRecall => {
            // Esc while the inline ↑/↓ pointer sits on a history row: cancel
            // the recall and restore the stashed draft (text + attachments).
            // `cancel_history_recall` is a no-op when the pointer is already
            // `None`, so a key race between the context snapshot and dispatch
            // cannot clobber a draft.
            app.cancel_history_recall();
        }
        input::InputAction::ActivateFocusedTarget => {
            // Stack-top focus dispatch: delegate to the focused component's own
            // interactive key handler (Enter activates: expand/collapse, subagent zoom, etc.).
            app.dispatch_focused_target_key(crate::shell::keymap::Key::ENTER);
        }
        input::InputAction::FocusedTargetKey(key) => {
            app.dispatch_focused_target_key(key);
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
        input::InputAction::InterruptSubagent => {
            // Esc inside the Subagent scene: interrupt only the
            // viewed child, with the same armed press-twice contract. It never
            // reaches the primary round, so the outer turn keeps running.
            handle_esc_interrupt_with_runtime(app, runtime, commands::InterruptTarget::Subagent)
                .await;
        }
        input::InputAction::ViewSwitcherFilter { ch } => {
            if app.active_dialog() == Some(DialogKind::Switcher) {
                app.surfaces
                    .dlg_mut::<crate::shell::surfaces::SwitcherDialog>()
                    .query
                    .text
                    .push(ch);
                app.surfaces
                    .dlg_mut::<crate::shell::surfaces::SwitcherDialog>()
                    .query
                    .cursor = app
                    .surfaces
                    .dlg_mut::<crate::shell::surfaces::SwitcherDialog>()
                    .query
                    .text
                    .len();
                app.surfaces
                    .dlg_mut::<crate::shell::surfaces::SwitcherDialog>()
                    .selected = 0;
                app.surfaces
                    .dlg_mut::<crate::shell::surfaces::SwitcherDialog>()
                    .scroll = 0;
            }
        }
        input::InputAction::ViewSwitcherBackspace => {
            if app.active_dialog() == Some(DialogKind::Switcher) {
                {
                    let q = &mut app
                        .surfaces
                        .dlg_mut::<crate::shell::surfaces::SwitcherDialog>()
                        .query;
                    if !q.is_empty() {
                        let start = nuotc::floor_grapheme_boundary(&q.text, q.text.len() - 1);
                        q.text.truncate(start);
                    }
                    q.cursor = q.text.len();
                }
                let d = app
                    .surfaces
                    .dlg_mut::<crate::shell::surfaces::SwitcherDialog>();
                d.selected = 0;
                d.scroll = 0;
            }
        }
        input::InputAction::OpenPortPalette => {
            if app.active_dialog() == Some(DialogKind::PortShare) {
                app.dismiss_surface();
            } else {
                app.surfaces
                    .dlg_mut::<crate::shell::surfaces::PortShareDialog>()
                    .begin();
                app.surfaces
                    .dlg_mut::<crate::shell::surfaces::PortShareDialog>()
                    .loading = true;
                app.open_dialog(DialogKind::PortShare);
                app.send_intent(nuo_wire::AgentRequest::QueryServerEndpoints);
            }
        }
        input::InputAction::TogglePortListener => {
            if app.active_dialog() == Some(DialogKind::PortShare) {
                let current_active = app
                    .surfaces
                    .dlg::<crate::shell::surfaces::PortShareDialog>()
                    .active;
                let d = app
                    .surfaces
                    .dlg_mut::<crate::shell::surfaces::PortShareDialog>();
                d.loading = true;
                d.notice = None;
                app.send_intent(nuo_wire::AgentRequest::ToggleServerPort {
                    port: None,
                    enable: !current_active,
                });
            }
        }
        input::InputAction::CopyPortConnectInfo => {
            if app.active_dialog() == Some(DialogKind::PortShare) {
                let d = app
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
        }
        input::InputAction::ViewSwitcherToggle => {
            if app.active_dialog() == Some(DialogKind::Switcher) {
                app.dismiss_surface();
            } else if app.can_open_switcher() {
                app.open_dialog(DialogKind::Switcher);
                app.surfaces
                    .dlg_mut::<crate::shell::surfaces::SwitcherDialog>()
                    .begin();
            }
        }
        input::InputAction::ViewSwitchActivate => {
            if app.active_dialog() != Some(DialogKind::Switcher) {
                return ActionFlow::Handled;
            }
            let is_busy = app.running_sessions.contains(viewed_session_id);
            let app_ctx = crate::shell::keymap::AppContext {
                has_overlay: app.surfaces.active_overlay().is_some(),
                active_dialog: app
                    .surfaces
                    .underlying_dialog()
                    .or_else(|| app.active_dialog()),
                is_responding: is_busy,
                has_selection: !matches!(
                    app.selection,
                    nuo_view::model::selection::SelectionState::None
                ),
                has_running_task: is_busy,
                has_notices: !!app.has_notices(),
                has_focused_entry: app.focused_target.is_some(),
                queue_count: app.pending_dispatch.len(),
                has_session: app.has_session(),
                scene: app.current_scene(),
            };
            let entries = crate::shell::overlays::command_palette::filter_palette_commands(
                &app.surfaces
                    .dlg_mut::<crate::shell::surfaces::SwitcherDialog>()
                    .query
                    .text,
                &app.recent_commands,
                &app_ctx,
            );
            if let Some(entry) = entries
                .get(
                    app.surfaces
                        .dlg_mut::<crate::shell::surfaces::SwitcherDialog>()
                        .selected,
                )
                .or_else(|| entries.first())
                && matches!(
                    entry.availability,
                    crate::shell::keymap::Availability::Available
                )
            {
                let cmd_key = entry.slash.clone().unwrap_or_else(|| entry.label.clone());
                if let Some(pos) = app.recent_commands.iter().position(|id| id == &cmd_key) {
                    app.recent_commands.remove(pos);
                }
                app.recent_commands.insert(0, cmd_key);
                if app.recent_commands.len() > 10 {
                    app.recent_commands.truncate(10);
                }

                app.pop_transient_surface();
                return execute_command_by_id(app, ctx, entry.id).await;
            }
        }
        input::InputAction::ViewCloseSelected => {
            if app.active_dialog() != Some(DialogKind::Switcher) {
                return ActionFlow::Handled;
            }
        }
        input::InputAction::InsertChar(c) => {
            // Already handled by route_event mutating app.input
            let _ = c;
            app.suggestion_index = None;
            // The user is editing again, so live completions are
            // once again useful — clear the Enter-commit dismissal.
            app.completion_dismissed = false;
            // Typing into the input box reclaims it as the active
            // surface: drop any transcript-step focus so the composer
            // re-brightens and the next arrow key resumes caret movement
            // rather than step navigation.
            app.focused_target = None;
            // Reconcile attachments: if the user typed inside a chip
            // (breaking its syntax) the backing staged entry must be
            // dropped, and surviving chips relabeled.
            app.reconcile_attachments();
        }
        input::InputAction::Backspace => {
            app.suggestion_index = None;
            app.completion_dismissed = false;
            // Same as InsertChar: editing the input box reclaims focus
            // from any transcript step.
            app.focused_target = None;
            // Reconcile attachments: a chip-aware backspace has
            // already spliced the chip out of `app.input`; this
            // drops the orphaned entry from `pending_images` /
            // `pending_text_pastes` and relabels survivors.
            app.reconcile_attachments();
        }
        input::InputAction::DeleteForward => {
            // Forward delete runs the same post-edit passes as Backspace: the
            // text mutation already happened in `route_event`; this arm
            // only keeps the completion latch, focus ownership, and staged
            // attachments consistent with the new buffer (a chip-aware
            // forward delete may have orphaned a staged entry).
            app.suggestion_index = None;
            app.completion_dismissed = false;
            // Editing the input box reclaims focus from any transcript step,
            // mirroring Backspace.
            app.focused_target = None;
            app.reconcile_attachments();
        }
        input::InputAction::SuggestNext => {
            let count = app.completions().len();
            if count > 0 {
                let next = match app.suggestion_index {
                    Some(i) => (i + 1) % count,
                    None => 0,
                };
                app.suggestion_index = Some(next);
            }
        }
        input::InputAction::SuggestPrev => {
            let count = app.completions().len();
            if count > 0 {
                let prev = match app.suggestion_index {
                    Some(i) => {
                        if i == 0 {
                            count - 1
                        } else {
                            i - 1
                        }
                    }
                    None => count - 1,
                };
                app.suggestion_index = Some(prev);
            }
        }
        input::InputAction::AcceptSuggestion(idx_str) => {
            if let Ok(idx) = idx_str.parse::<usize>() {
                app.accept_completion(idx);
            }
            // Legacy accept-without-closing arm (no longer bound to a key at
            // the top level — Tab now commits like Enter). Kept for callers
            // that want a live splice; the popup stays open only for
            // directory descents, which accept_completion decides by kind.
        }
        input::InputAction::CommitSuggestion(idx_str) => {
            if let Ok(idx) = idx_str.parse::<usize>() {
                app.accept_completion(idx);
            }
        }
        input::InputAction::ReopenCompletion => {
            // The other half of the Esc/Tab toggle: bring a dismissed
            // completion menu back without accepting anything. The next
            // anchor pass (post-dispatch, same iteration) seeds the
            // highlight onto the first candidate, so the reopened menu
            // lands already selected with its details flyout showing.
            app.completion_dismissed = false;
        }
        input::InputAction::CloseCompletion => {
            // Esc dismisses the popup without accepting anything.
            // Same latch as Enter-commit so the popup stays hidden
            // until the next edit clears `completion_dismissed` — or
            // until Tab re-opens it (ReopenCompletion).
            app.suggestion_index = None;
            app.completion_dismissed = true;
        }
        input::InputAction::HistoryPrev => {
            // Inline ↑ walks the **current session's** history only
            // (newest-first), not the whole cross-session log — Ctrl+R
            // is the global search surface. We recompute the session
            // slice each press so newly-recorded entries appear
            // without a restart; `history_index` is a position into
            // that slice. `App::history_prev` advances toward older
            // entries and stashes the in-progress draft on the first
            // press (so ↓ can restore it).
            let session_rows = app.current_session_history();
            app.history_prev(&session_rows);
        }
        input::InputAction::RecallQueuedSelected => {
            // The queue modal's `Enter` recalls the *selected* item
            // (the `↑/↓` highlight, not always the newest) into the
            // composer and closes the modal. Closing resumes the
            // auto-block the modal set on open.
            let idx = app.active_index();
            app.dismiss_active_dialog();
            if let Some(crate::shell::app::RecallQueued::Restored(dispatch)) =
                app.recall_queued_at(viewed_session_id, idx)
            {
                app.restore_dispatch(dispatch);
            }
        }
        input::InputAction::QueueToggleBlock => {
            // `Ctrl+P` inside the queue modal: toggle the hard block on the
            // viewed session's outbox. M4: the pause is the
            // *server's* queue flag — the local toggle is the optimistic
            // projection and the verb is authoritative.
            let paused = !app.is_queue_blocked(viewed_session_id);
            app.set_queue_blocked(viewed_session_id, paused);
            app.send_intent(AgentRequest::QueuePaused {
                session_id: viewed_session_id.to_string(),
                paused,
            });
        }
        input::InputAction::QueueDelete => {
            // `D` in the queue modal: remove the highlighted
            // item outright. The queue is auto-blocked on open, so the
            // index can't drift under us. Clamp the selection to the
            // now-shorter list.
            if app.active_dialog() == Some(DialogKind::Queue) {
                let idx = app.active_index();
                let _removed = app.remove_queued_at(viewed_session_id, idx);
                let count = app.pending_count(viewed_session_id);
                if count == 0 {
                    app.set_active_index(0);
                } else if app.active_index() >= count {
                    app.set_active_index(count - 1);
                }
                app.surfaces
                    .dlg_mut::<crate::shell::surfaces::QueueDialog>()
                    .follow = true;
            }
        }
        input::InputAction::QueueMoveItem { delta } => {
            // `K`/`J` in the queue modal: reorder the highlighted item
            // toward the front (next to pop) or the tail. Clamp at the
            // session slice boundaries so it can't escape into another
            // session's items.
            if app.active_dialog() == Some(DialogKind::Queue) {
                let idx = app.active_index();
                if let Some(item) = app.queued_at(viewed_session_id, idx) {
                    app.send_intent(AgentRequest::QueueReorder {
                        session_id: viewed_session_id.to_string(),
                        input_id: item.id.clone(),
                        delta,
                    });
                }
                app.move_queued(viewed_session_id, idx, delta);
                // Follow the moved item if it changed position.
                let count = app.pending_count(viewed_session_id);
                if count > 0 {
                    app.set_active_index((idx as i32 + delta).clamp(0, count as i32 - 1) as usize);
                    app.surfaces
                        .dlg_mut::<crate::shell::surfaces::QueueDialog>()
                        .follow = true;
                }
            }
        }
        input::InputAction::HistoryNext => {
            // Inline ↓ walks the current session's history forward
            // (toward the newest), mirroring HistoryPrev. Walking past
            // the newest entry restores the stashed draft.
            let session_rows = app.current_session_history();
            app.history_next(&session_rows);
        }
        input::InputAction::ModalUp => {
            modals::handle_modal_up(app, viewed_session_id);
        }
        input::InputAction::ModalDown => {
            modals::handle_modal_down(app, viewed_session_id);
        }
        input::InputAction::QuestionUp => {
            if app.active_sheet() == Some(crate::shell::sheet::SheetKind::Question)
                && let Some(qm) = app.question.take()
            {
                app.question = Some(
                    qm.update(crate::shell::question_model::QuestionAction::Up)
                        .0,
                );
                // Moving the highlight re-enables follow so the body
                // scrolls to keep the cursor visible.
                app.question_modal_follow = true;
            }
        }
        input::InputAction::QuestionDown => {
            if app.active_sheet() == Some(crate::shell::sheet::SheetKind::Question)
                && let Some(qm) = app.question.take()
            {
                app.question = Some(
                    qm.update(crate::shell::question_model::QuestionAction::Down)
                        .0,
                );
                app.question_modal_follow = true;
            }
        }
        input::InputAction::QuestionToggle => {
            if app.active_sheet() == Some(crate::shell::sheet::SheetKind::Question)
                && let Some(qm) = app.question.take()
            {
                app.question = Some(
                    qm.update(crate::shell::question_model::QuestionAction::Toggle)
                        .0,
                );
            }
        }
        input::InputAction::QuestionSelect(n) => {
            if app.active_sheet() == Some(crate::shell::sheet::SheetKind::Question)
                && let Some(qm) = app.question.take()
            {
                app.question = Some(
                    qm.update(crate::shell::question_model::QuestionAction::Select(n))
                        .0,
                );
                // A digit jump moves the highlight, so follow it.
                app.question_modal_follow = true;
            }
        }
        input::InputAction::QuestionSubmit => {
            if app.active_sheet() == Some(crate::shell::sheet::SheetKind::Question)
                && let Some(qm) = app.question.take()
            {
                let (qm, effects) = qm.update(crate::shell::question_model::QuestionAction::Submit);
                // Keep the model until the per-frame queue sync clears
                // it; the Closed effect drives the channel reply + drain.
                app.question = Some(qm);
                question_effects::apply(&effects, app, runtime).await;
                app.question_scroll = 0;
                app.question_modal_follow = true;
            }
        }
        input::InputAction::QuestionPrevious => {
            if app.active_sheet() == Some(crate::shell::sheet::SheetKind::Question)
                && let Some(qm) = app.question.take()
            {
                app.question = Some(
                    qm.update(crate::shell::question_model::QuestionAction::Previous)
                        .0,
                );
                app.question_scroll = 0;
                app.question_modal_follow = true;
            }
        }
        input::InputAction::QuestionNext => {
            if app.active_sheet() == Some(crate::shell::sheet::SheetKind::Question)
                && let Some(qm) = app.question.take()
            {
                app.question = Some(
                    qm.update(crate::shell::question_model::QuestionAction::Next)
                        .0,
                );
                app.question_scroll = 0;
                app.question_modal_follow = true;
            }
        }
        input::InputAction::QuestionCancel => {
            if app.active_sheet() == Some(crate::shell::sheet::SheetKind::Question)
                && let Some(qm) = app.question.take()
            {
                let (_qm, effects) =
                    qm.update(crate::shell::question_model::QuestionAction::Cancel);
                // Cancel discards the model immediately; the Closed
                // effect drives the (empty-answers) reply + drain.
                question_effects::apply(&effects, app, runtime).await;
            }
        }
        // PreAttach interstitial actions. The surface owns
        // the keyboard when mounted; routing is disjoint from the
        // Question sheet so neither flow can interact with the other.
        // Quit decisions flip `should_quit` so the loop terminates on
        // the next iteration; trust decisions dispatch the canonical
        // `/trust` slash command and let the per-frame sync clear
        // `pre_attach` once the server republishes a Trusted snapshot.
        input::InputAction::PreAttachUp => {
            if let Some(pa) = app.pre_attach.as_mut() {
                let _ = pa.apply(crate::shell::question_model::QuestionAction::Up);
            }
        }
        input::InputAction::PreAttachDown => {
            if let Some(pa) = app.pre_attach.as_mut() {
                let _ = pa.apply(crate::shell::question_model::QuestionAction::Down);
            }
        }
        input::InputAction::PreAttachToggle => {
            if let Some(pa) = app.pre_attach.as_mut() {
                let _ = pa.apply(crate::shell::question_model::QuestionAction::Toggle);
            }
        }
        input::InputAction::PreAttachSubmit => {
            if let Some(pa) = app.pre_attach.as_mut() {
                let decision = pa.apply(crate::shell::question_model::QuestionAction::Submit);
                apply_pre_attach_decision(decision, app, runtime).await;
            }
        }
        input::InputAction::PreAttachCancel => {
            if let Some(pa) = app.pre_attach.as_mut() {
                let decision = pa.apply(crate::shell::question_model::QuestionAction::Cancel);
                apply_pre_attach_decision(decision, app, runtime).await;
            }
        }
        input::InputAction::InputSubmit => {
            if app.active_sheet() == Some(crate::shell::sheet::SheetKind::InputInjection) {
                let text = std::mem::take(&mut app.input);
                if let Some(req) = app.pending_input.take() {
                    // Drain the matching front so the per-frame sync
                    // closes the modal and restores the composer draft.
                    app.pending_inputs.pop_front();
                    let parent_call_id = app.subagent_question_parent.remove(&req.id);
                    app.send_intent(AgentRequest::StdinReply {
                        request_id: req.id.clone(),
                        text,
                        parent_call_id,
                    });
                }
                let next = app.pending_inputs.front().cloned();
                if let Some(next) = next {
                    app.pending_input = Some(next);
                    app.input.clear();
                    app.set_cursor(0);
                } else {
                    app.restore_input_draft();
                    app.dismiss_sheet();
                }
            }
        }
        input::InputAction::InputCancel => {
            if app.active_sheet() == Some(crate::shell::sheet::SheetKind::InputInjection)
                && let Some(req) = app.pending_input.take()
            {
                // Empty reply = cancel → the command runs with closed
                // stdin and fails fast with a non-interactive remedy.
                app.pending_inputs.pop_front();
                let next = app.pending_inputs.front().cloned();
                let parent_call_id = app.subagent_question_parent.remove(&req.id);
                app.send_intent(AgentRequest::StdinReply {
                    request_id: req.id.clone(),
                    text: String::new(),
                    parent_call_id,
                });
                if let Some(next) = next {
                    app.pending_input = Some(next);
                    app.input.clear();
                    app.set_cursor(0);
                } else {
                    app.restore_input_draft();
                    app.dismiss_sheet();
                }
            }
        }
        input::InputAction::QuestionInsertChar(c) => {
            if app.active_sheet() == Some(crate::shell::sheet::SheetKind::Question)
                && let Some(qm) = app.question.take()
            {
                app.question = Some(
                    qm.update(crate::shell::question_model::QuestionAction::InsertChar(c))
                        .0,
                );
                // Typing into the "Other" field may grow it onto a new
                // wrapped line, pushing the caret below the viewport.
                // Re-arm follow so the body scrolls to track the
                // caret (not just the "Other" label row).
                app.question_modal_follow = true;
            }
        }
        input::InputAction::QuestionBackspace => {
            if app.active_sheet() == Some(crate::shell::sheet::SheetKind::Question)
                && let Some(qm) = app.question.take()
            {
                app.question = Some(
                    qm.update(crate::shell::question_model::QuestionAction::Backspace)
                        .0,
                );
                // Backspace can collapse the field back up a line;
                // re-arm follow so the caret stays on screen.
                app.question_modal_follow = true;
            }
        }
        input::InputAction::PermissionPrevOption => {
            if app.active_sheet() == Some(crate::shell::sheet::SheetKind::Permission) {
                let one_off = app.pending_permission.as_ref().is_some_and(|r| r.one_off);
                let total = crate::shell::overlays::permission_action_count(
                    app.permission_confirm_always,
                    one_off,
                );
                if total > 0 {
                    app.modal_index = if app.modal_index == 0 {
                        total - 1
                    } else {
                        app.modal_index - 1
                    };
                }
            }
        }
        input::InputAction::PermissionNextOption => {
            if app.active_sheet() == Some(crate::shell::sheet::SheetKind::Permission) {
                let one_off = app.pending_permission.as_ref().is_some_and(|r| r.one_off);
                let total = crate::shell::overlays::permission_action_count(
                    app.permission_confirm_always,
                    one_off,
                );
                if total > 0 {
                    app.modal_index = (app.modal_index + 1) % total;
                }
            }
        }
        input::InputAction::PermissionSubmit => {
            handle_permission_submit(app, runtime).await;
        }
        input::InputAction::PermissionReject => {
            // Rejecting settles the whole concurrent permission batch;
            // resolve every queued request so its tool futures finish.
            let queued: Vec<PermissionRequest> = app.pending_permissions.drain(..).collect();
            app.pending_permission = None;
            app.dismiss_sheet();
            app.modal_index = 0;
            app.permission_confirm_always = false;
            app.permission_show_details = false;
            for pending in queued {
                let parent_call_id = app.subagent_permission_parent.remove(&pending.id);
                app.send_intent(AgentRequest::PermissionReply {
                    request_id: pending.id,
                    decision: PermissionDecision::Reject,
                    parent_call_id,
                });
            }
        }
        input::InputAction::PermissionBack => {
            app.permission_confirm_always = false;
            app.modal_index = 1;
        }
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
    }
    ActionFlow::Handled
}

/// The sole retained-panel entry transaction (panels are
/// retained modals). It focuses/restores the panel, runs one-time
/// initialization, then applies the panel's refresh-on-show and enter-hook
/// policy. Dedicated shortcuts, mouse targets, backend open signals and the
/// quick switcher all route here. Root scenes use [`enter_scene`].
pub(crate) fn open_active_connection_detail(
    app: &mut App,
    runtime: &UiRuntime,
    viewed_session_id: &str,
) {
    enter_panel(
        app,
        crate::shell::surfaces::DialogKind::Connections,
        runtime,
        viewed_session_id,
    );
    let target_id = if !app.current_provider.is_empty() {
        app.current_provider.clone()
    } else {
        app.providers_filtered()
            .first()
            .map(|p| p.id.clone())
            .unwrap_or_default()
    };
    if let Some(pos) = app
        .providers_filtered()
        .iter()
        .position(|p| p.id == target_id)
    {
        app.set_active_index(pos);
    }
    app.surfaces
        .dlg_mut::<crate::shell::surfaces::ConnectionsDialog>()
        .info_detail = true;
    app.surfaces
        .dlg_mut::<crate::shell::surfaces::ConnectionsDialog>()
        .info_standalone = true;
    app.connection_detail = None;
    app.surfaces
        .dlg_mut::<crate::shell::surfaces::ConnectionsDialog>()
        .info_scroll = 0;
    if !target_id.is_empty() {
        app.send_intent(AgentRequest::QueryConnectionDetail {
            id: target_id,
            force_refresh: false,
        });
    }
}

pub(super) fn enter_panel(
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
    app.focused_target = None;
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
        // M4: the editing-safety auto-pause is mirrored to the
        // server queue authority (resume on close, via `queue_exit_session`).
        app.set_queue_blocked(viewed_session_id, true);
        app.send_intent(AgentRequest::QueuePaused {
            session_id: viewed_session_id.to_string(),
            paused: true,
        });
        app.queue_exit_session = Some(viewed_session_id.to_string());
    }

    let request = match id {
        DialogKind::Permissions | DialogKind::Tools | DialogKind::Mcp | DialogKind::Skills => {
            Some(AgentRequest::QuerySessionContext)
        }
        // Always re-query. The report is a 400-day historical aggregate and
        // is no longer pushed at round boundaries (the fold cost sat on the
        // critical path to the round's idle snapshot), so reusing a cached
        // copy would show the first snapshot forever. The previous numbers
        // stay on screen until the fresh reply lands, so reopening does not
        // flash the loading state.
        DialogKind::UsageStats => Some(AgentRequest::QueryUsageStats { event_cap: 200 }),
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
        DialogKind::ThreadStats | DialogKind::SessionTrace if app.token_ledger.is_none() => {
            if app.token_report.is_none() {
                Some(AgentRequest::QueryTokenUsage {
                    session_id: viewed_session_id.to_string(),
                })
            } else {
                None
            }
        }
        DialogKind::Threads => Some(AgentRequest::QuerySessionsOverview),
        DialogKind::SessionTree => Some(AgentRequest::QuerySessionTree),
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
pub(super) fn activate_tab_at_index(
    app: &mut App,
    index: usize,
    runtime: &UiRuntime,
    viewed_session_id: &str,
) -> ActionFlow {
    let current_idx = app.surfaces.active_tab_index();
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

    if let Some(target_tab) = app.surfaces.tabs().get(index).cloned() {
        // Checkpoint departing tab context (ADR-0091 [INV-TAB-12])
        app.checkpoint_active_tab_context();

        let was_in_thread = app.current_scene() == crate::shell::surfaces::SceneKind::Thread;
        app.surfaces.select_tab(index);
        match &target_tab.kind {
            crate::shell::surfaces::TabKind::Thread(id) => {
                if id != viewed_session_id {
                    app.switch_to_target = Some(id.clone());
                    app.should_quit.store(true, Ordering::SeqCst);
                    return ActionFlow::Exit;
                } else {
                    if !was_in_thread {
                        enter_scene(app, crate::shell::surfaces::SceneKind::Thread, runtime);
                    }
                    app.restore_active_tab_context();
                    app.selection = SelectionState::None;
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
pub(super) fn handle_close_tab(
    app: &mut App,
    runtime: &UiRuntime,
    viewed_session_id: &str,
) -> ActionFlow {
    let active_idx = app.surfaces.active_tab_index();
    app.close_tab(active_idx);
    if app.surfaces.tabs().is_empty() {
        tracing::info!(reason = "close_last_tab", "client detaching");
        return ActionFlow::Exit;
    }

    if let Some(target_tab) = app.surfaces.active_tab().cloned() {
        match &target_tab.kind {
            crate::shell::surfaces::TabKind::Thread(id) => {
                if id != viewed_session_id {
                    app.switch_to_target = Some(id.clone());
                    app.should_quit.store(true, Ordering::SeqCst);
                    return ActionFlow::Exit;
                } else {
                    enter_scene(app, crate::shell::surfaces::SceneKind::Thread, runtime);
                    app.restore_active_tab_context();
                    app.selection = SelectionState::None;
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

/// Enter a full-screen scene: navigate the router, run the scene's
/// every-show UI refresh, and fire its data-refresh request.
pub(super) fn enter_scene(
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
    app.focused_target = None;
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
        SceneKind::Thread | SceneKind::Subagent => None,
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

pub(crate) fn handle_config_activate(app: &mut App) {
    if app.current_scene() != SceneKind::Settings {
        return;
    }
    let ws_path = if app.current_workspace.is_empty() {
        None
    } else {
        Some(std::path::Path::new(&app.current_workspace))
    };
    let active_category = crate::shell::overlays::ConfigCategory::from_index(app.config_category);
    match app.config_focus {
        crate::shell::overlays::ConfigFocus::Categories => {
            app.config_focus = crate::shell::overlays::ConfigFocus::Detail;
            if active_category == crate::shell::overlays::ConfigCategory::Appearance {
                app.config_detail_index =
                    crate::shell::config::color_scheme_index(&app.color_scheme, ws_path);
            } else {
                app.config_detail_index = 0;
            }
        }
        crate::shell::overlays::ConfigFocus::Detail => match active_category {
            crate::shell::overlays::ConfigCategory::Appearance => {
                if app.profile.supports_color_themes() {
                    let schemes = crate::shell::config::available_color_schemes(ws_path);
                    let sel_idx = app.config_detail_index % schemes.len().max(1);
                    if let Some(scheme) = schemes.get(sel_idx) {
                        let name = &scheme.id;
                        app.color_scheme = name.to_string();
                        app.theme = crate::shell::config::resolve_theme_with_profile(
                            name.as_ref(),
                            &app.custom_color_scheme,
                            ws_path,
                            &app.profile,
                        );
                        app.send_intent(AgentRequest::UpdateTuiColorScheme {
                            name: app.color_scheme.clone(),
                            custom: app.custom_color_scheme.clone(),
                        });
                        app.save_tui_config();
                    }
                }
            }
            crate::shell::overlays::ConfigCategory::Components => {
                use crate::shell::views::settings::components::{ComponentRowId, row_for_index};
                match row_for_index(app.config_detail_index) {
                    Some(ComponentRowId::Reasoning) => {
                        let next =
                            !crate::shell::config::reasoning_default_expanded(&app.tui_config);
                        if next {
                            app.tui_config
                                .default_expanded
                                .insert(crate::shell::config::THINKING_KEY.to_string(), true);
                        } else {
                            app.tui_config
                                .default_expanded
                                .remove(crate::shell::config::THINKING_KEY);
                        }
                        app.reasoning_default_expanded = next;
                        app.save_tui_config();
                    }
                    Some(ComponentRowId::Tool(component)) => {
                        let next = !crate::shell::config::tool_default_expanded(
                            &app.tui_config,
                            component.primary_name(),
                        );
                        crate::shell::config::set_component_default_expanded(
                            &mut app.tui_config,
                            component,
                            next,
                        );
                        app.save_tui_config();
                    }
                    Some(ComponentRowId::AutoScroll) => {
                        app.expand_auto_scroll = !app.expand_auto_scroll;
                        app.tui_config.expand_auto_scroll = app.expand_auto_scroll;
                        app.save_tui_config();
                    }
                    None => {}
                }
            }
            crate::shell::overlays::ConfigCategory::WebSearch
            | crate::shell::overlays::ConfigCategory::WebReader => {
                let Some(revision) = app.websearch_config.as_ref().map(|config| config.revision)
                else {
                    return;
                };
                let is_search =
                    active_category == crate::shell::overlays::ConfigCategory::WebSearch;
                match app.config_detail_index {
                    0 => {
                        let anchor = if let Some(target_rect) = app.config_selected_rect {
                            nuo_ui_primitives::components::dropdown::DropdownAnchor::anchored(
                                target_rect,
                                nuo_ui_primitives::components::dropdown::DropdownPlacement::Auto,
                            )
                        } else {
                            nuo_ui_primitives::components::dropdown::DropdownAnchor::center_screen()
                        };
                        if is_search {
                            let current = app
                                .websearch_config
                                .as_ref()
                                .map(|ws| ws.provider.as_str())
                                .unwrap_or("exa");
                            let dropdown =
                                crate::shell::views::settings::build_websearch_provider_dropdown(
                                    current,
                                    app.websearch_config.as_ref(),
                                );
                            app.config_dropdown = Some((dropdown, anchor));
                        } else {
                            let current = app
                                .websearch_config
                                .as_ref()
                                .map(|ws| ws.reader.as_str())
                                .unwrap_or("disabled");
                            let dropdown =
                                crate::shell::views::settings::build_websearch_reader_dropdown(
                                    current,
                                    app.websearch_config.as_ref(),
                                );
                            app.config_dropdown = Some((dropdown, anchor));
                        }
                    }
                    1 => {
                        let current = app
                            .websearch_config
                            .as_ref()
                            .map(|ws| ws.timeout_secs)
                            .unwrap_or(20);
                        let next = if current >= 120 {
                            5
                        } else {
                            (current + 5).max(5)
                        };
                        app.send_intent(AgentRequest::UpdateWebSearchConfig(Box::new(
                            nuo_wire::WebSearchConfigUpdate {
                                expected_revision: revision,
                                timeout_secs: Some(next),
                                ..Default::default()
                            },
                        )));
                    }
                    2 => {
                        let editor = app.websearch_config.as_ref().and_then(|ws| {
                            let (axis, id) = if is_search {
                                ("search", ws.provider.id())
                            } else {
                                ("reader", ws.reader.id())
                            };
                            let capability = ws.capabilities.iter().find(|capability| {
                                capability.id == id
                                    && capability.axis
                                        == if is_search {
                                            nuo_wire::WebProviderAxis::Search
                                        } else {
                                            nuo_wire::WebProviderAxis::Reader
                                        }
                            })?;
                            let endpoint = capability.endpoint
                                == nuo_wire::WebEndpointRequirement::UserSupplied;
                            let target = if endpoint {
                                format!("web_endpoint:{id}")
                            } else {
                                format!("web_credential:{axis}:{id}")
                            };
                            let initial = if endpoint {
                                ws.searxng_url.clone().unwrap_or_default()
                            } else {
                                String::new()
                            };
                            Some((target, capability.display_name.clone(), initial))
                        });
                        if let Some((target, display_name, initial)) = editor {
                            app.surfaces.present_sheet(SheetKind::ModelEditor);
                            app.editor_target = Some(target);
                            app.editor_model = display_name;
                            app.editor_key.clear();
                            app.editor_field = 0;
                            app.input = initial;
                            app.set_cursor(app.input.len());
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        },
    }
}

pub(crate) fn handle_wheel(app: &mut App, up: bool, x: u16, y: u16, viewed_session_id: &str) {
    use crate::shell::ui::UiKey;
    match app.ui.scene().hit_test(x, y).copied() {
        Some(UiKey::Overlay(overlay)) if app.ui.contains(UiKey::Overlay(overlay), x, y) => {
            if let Some(mut ent) = app.surfaces.take_active_view() {
                let action = if up {
                    input::InputAction::ScrollUp
                } else {
                    input::InputAction::ScrollDown
                };
                let outcome = ent.handle_input(&action, app, viewed_session_id);
                app.surfaces.put_active_view(ent);
                if outcome.is_consumed() {
                    return;
                }
            }
            scroll_tick(app, !up);
        }
        Some(UiKey::SettingsCategory(_)) => {
            app.config_scroll = if up {
                app.config_scroll.saturating_sub(1)
            } else {
                app.config_scroll.saturating_add(1)
            };
        }
        Some(
            UiKey::OauthUrl | UiKey::OauthCode | UiKey::SettingsOption(_) | UiKey::SettingsRow(_),
        ) => scroll_tick(app, !up),
        Some(UiKey::ProviderDelete | UiKey::PreAttach) => {}
        Some(
            UiKey::Sheet(crate::shell::sheet::SheetKind::Permission) | UiKey::PermissionAction(_),
        ) if app.permission_show_details => {
            app.permission_scroll = if up {
                app.permission_scroll.saturating_sub(1)
            } else {
                app.permission_scroll
                    .saturating_add(1)
                    .min(app.permission_max_scroll)
            };
        }
        Some(UiKey::Sheet(crate::shell::sheet::SheetKind::Question) | UiKey::QuestionOption(_)) => {
            app.question_modal_follow = false;
            app.question_scroll = if up {
                app.question_scroll.saturating_sub(1)
            } else {
                app.question_scroll.saturating_add(1)
            };
        }
        Some(UiKey::Completion | UiKey::CompletionItem(_)) => {
            let count = app.completions().len();
            if count > 0 {
                app.suggestion_index = Some(if up {
                    match app.suggestion_index {
                        Some(0) | None => count.saturating_sub(1),
                        Some(i) => i.saturating_sub(1),
                    }
                } else {
                    match app.suggestion_index {
                        Some(i) if i + 1 < count => i + 1,
                        _ => 0,
                    }
                });
            }
        }
        Some(UiKey::Composer) if app.step_input_scroll(up, 4).is_none() => {
            scroll_tick(app, !up);
        }
        Some(
            UiKey::Transcript
            | UiKey::Sticky
            | UiKey::Queue
            | UiKey::Activity
            | UiKey::ModelBar
            | UiKey::Context
            | UiKey::Performance
            | UiKey::Connection
            | UiKey::NoticesBadge,
        ) => {
            scroll_tick(app, !up);
        }
        _ => {}
    }
}

#[cfg(test)]
mod transcript_scroll_tests {
    use super::*;
    use crate::shell::surfaces::DialogView;

    fn scrollable_app() -> App {
        let mut app = crate::shell::tests::new_app_for_relay_tests();
        app.max_scroll = 100;
        app.scroll = 100;
        app.view_height = 20;
        app.follow_bottom = true;
        app
    }

    #[test]
    fn wheel_ticks_move_the_transcript_and_rearm_bottom_follow() {
        let mut app = scrollable_app();

        scroll_tick(&mut app, false);
        assert_eq!(app.scroll, 96);
        assert!(!app.follow_bottom);

        scroll_tick(&mut app, true);
        assert_eq!(app.scroll, 100);
        assert!(app.follow_bottom);
    }

    #[test]
    fn page_navigation_uses_the_measured_viewport() {
        let mut app = scrollable_app();

        scroll_transcript_page(&mut app, false);
        assert_eq!(app.scroll, 81);
        assert!(!app.follow_bottom);

        scroll_transcript_page(&mut app, true);
        assert_eq!(app.scroll, 100);
        assert!(app.follow_bottom);
    }

    #[test]
    fn edge_navigation_updates_position_and_follow_mode_together() {
        let mut app = scrollable_app();

        scroll_transcript_to_edge(&mut app, false);
        assert_eq!(app.scroll, 0);
        assert!(!app.follow_bottom);

        scroll_transcript_to_edge(&mut app, true);
        assert_eq!(app.scroll, 100);
        assert!(app.follow_bottom);
    }

    #[test]
    fn wheel_spatial_routing_under_permission_modal() {
        let mut app = scrollable_app();
        app.set_active_sheet_for_test(crate::shell::sheet::SheetKind::Permission);
        app.ui.begin(nuotc::Rect::new(0, 0, 80, 24));
        app.ui
            .mount_permission_sheet(nuotc::Rect::new(0, 15, 80, 5));
        app.ui.commit();
        app.permission_show_details = true;
        app.permission_max_scroll = 10;
        app.permission_scroll = 2;

        // 1. Wheel over permission sheet (y=16) scrolls details down
        handle_wheel(&mut app, false, 10, 16, "");
        assert_eq!(app.permission_scroll, 3);
        assert_eq!(app.scroll, 100, "transcript scroll untouched");

        // 2. Wheel over permission sheet (y=16) scrolls details up
        handle_wheel(&mut app, true, 10, 16, "");
        assert_eq!(app.permission_scroll, 2);
        assert_eq!(app.scroll, 100, "transcript scroll untouched");

        // 3. Wheel above permission sheet (y=5) scrolls transcript
        handle_wheel(&mut app, true, 10, 5, "");
        assert_eq!(app.scroll, 96, "transcript scrolled up");
        assert_eq!(app.permission_scroll, 2, "permission scroll untouched");
    }

    #[test]
    fn wheel_spatial_routing_under_overlay_modal_isolates_backdrop() {
        let mut app = scrollable_app();
        app.open_dialog(crate::shell::surfaces::DialogKind::UsageStats);
        app.ui.begin(nuotc::Rect::new(0, 0, 80, 24));
        app.ui.mount(
            crate::shell::ui::UiKey::Overlay(crate::shell::surfaces::OverlaySurface::Dialog(
                crate::shell::surfaces::DialogKind::UsageStats,
            )),
            nuotc::Rect::new(10, 5, 60, 10),
        );
        app.ui.commit();
        app.surfaces
            .dlg_mut::<crate::shell::surfaces::UsageStatsDialog>()
            .scroll = 5;

        // 1. Wheel on backdrop (x=2, y=2) outside modal_rect: absorbed, neither modal nor transcript scrolls
        handle_wheel(&mut app, false, 2, 2, "");
        assert_eq!(
            app.surfaces
                .dlg_mut::<crate::shell::surfaces::UsageStatsDialog>()
                .scroll,
            5,
            "modal scroll untouched on backdrop"
        );
        assert_eq!(app.scroll, 100, "transcript scroll untouched on backdrop");

        // 2. Wheel inside modal (x=20, y=8): scrolls modal body
        handle_wheel(&mut app, false, 20, 8, "");
        assert_eq!(
            app.surfaces
                .dlg_mut::<crate::shell::surfaces::UsageStatsDialog>()
                .scroll,
            6,
            "modal scrolled inside modal_rect"
        );
        assert_eq!(app.scroll, 100, "transcript scroll untouched");
    }

    #[test]
    fn wheel_spatial_routing_under_dialog_sub_views_scrolls_info_detail() {
        let mut app = scrollable_app();
        app.open_dialog(crate::shell::surfaces::DialogKind::Threads);
        app.ui.begin(nuotc::Rect::new(0, 0, 80, 24));
        app.ui.mount(
            crate::shell::ui::UiKey::Overlay(crate::shell::surfaces::OverlaySurface::Dialog(
                crate::shell::surfaces::DialogKind::Threads,
            )),
            nuotc::Rect::new(10, 5, 60, 10),
        );
        app.ui.commit();

        let threads = app
            .surfaces
            .dlg_mut::<crate::shell::surfaces::ThreadsDialog>();
        threads.scroll = 2;
        threads.info_detail = true;
        threads.info_scroll = 0;

        // Wheel inside Threads dialog in info_detail mode:
        // Must increment info_scroll and leave main list scroll untouched! [INV-SURFACE-08]
        handle_wheel(&mut app, false, 20, 8, "");
        assert_eq!(
            app.surfaces
                .dlg_mut::<crate::shell::surfaces::ThreadsDialog>()
                .info_scroll,
            1,
            "info_detail scrolled down"
        );
        assert_eq!(
            app.surfaces
                .dlg_mut::<crate::shell::surfaces::ThreadsDialog>()
                .scroll,
            2,
            "main list scroll untouched"
        );

        handle_wheel(&mut app, true, 20, 8, "");
        assert_eq!(
            app.surfaces
                .dlg_mut::<crate::shell::surfaces::ThreadsDialog>()
                .info_scroll,
            0,
            "info_detail scrolled back up"
        );
        assert_eq!(
            app.surfaces
                .dlg_mut::<crate::shell::surfaces::ThreadsDialog>()
                .scroll,
            2,
            "main list scroll untouched"
        );
    }

    #[test]
    fn wheel_spatial_routing_under_dialog_keys_scrolls_keys_sublayer() {
        let mut app = scrollable_app();
        app.open_dialog(crate::shell::surfaces::DialogKind::UsageStats);
        app.ui.begin(nuotc::Rect::new(0, 0, 80, 24));
        app.ui.mount(
            crate::shell::ui::UiKey::Overlay(crate::shell::surfaces::OverlaySurface::Dialog(
                crate::shell::surfaces::DialogKind::UsageStats,
            )),
            nuotc::Rect::new(10, 5, 60, 10),
        );
        app.ui.commit();

        let stats = app
            .surfaces
            .dlg_mut::<crate::shell::surfaces::UsageStatsDialog>();
        stats.set_keys_open(true);
        *stats.keys_scroll_mut() = 0;

        // Wheel inside UsageStats while dialog_keys is open:
        // Must increment keys_scroll on the dialog entity, not the body scroll!
        handle_wheel(&mut app, false, 20, 8, "");
        assert_eq!(
            *app.surfaces
                .dlg_mut::<crate::shell::surfaces::UsageStatsDialog>()
                .keys_scroll_mut(),
            1,
            "dialog keys sublayer scrolled down"
        );
        assert_eq!(
            app.surfaces
                .dlg_mut::<crate::shell::surfaces::UsageStatsDialog>()
                .scroll,
            0,
            "main body scroll untouched"
        );
    }

    #[tokio::test]
    async fn completion_menu_mouse_wheel_and_click() {
        let mut app = scrollable_app();
        app.input = "/m".to_string();
        app.cursor_position = 2;
        app.ui.begin(nuotc::Rect::new(0, 0, 80, 24));
        app.ui.mount_completion(nuotc::Rect::new(0, 8, 30, 2));
        app.ui
            .mount_completion_item(0, nuotc::Rect::new(0, 8, 30, 1));
        app.ui
            .mount_completion_item(1, nuotc::Rect::new(0, 9, 30, 1));
        app.ui.commit();

        let runtime = UiRuntime::minimal_for_test();

        // Wheel over menu cycles suggestions
        handle_wheel(&mut app, false, 5, 8, "");
        assert!(app.suggestion_index.is_some());

        // Click on completion item 0 accepts completion
        mouse::handle_selection_start(&mut app, &runtime, "s1", 5, 8).await;
        assert!(app.completion_dismissed);
        assert_eq!(app.suggestion_index, None);
    }

    #[tokio::test]
    async fn mouse_click_on_tab_bar_switches_active_tab() {
        let mut app = crate::shell::tests::new_app_for_relay_tests();
        let runtime = UiRuntime::minimal_for_test();

        // Mount tabs
        app.surfaces
            .open_tab(crate::shell::surfaces::ClientTab::thread(
                "s1",
                Some("Thread 1"),
            ));
        app.surfaces
            .open_tab(crate::shell::surfaces::ClientTab::dashboard());
        app.surfaces
            .open_tab(crate::shell::surfaces::ClientTab::settings());
        app.surfaces.select_tab(0);
        assert_eq!(app.surfaces.active_tab_index(), 0);

        // Mount TabBarItem(1) (Dashboard tab) at (14, 0)
        app.ui.begin(nuotc::Rect::new(0, 0, 80, 24));
        app.ui.mount(
            crate::shell::ui::UiKey::TabBarItem(1),
            nuotc::Rect::new(14, 0, 14, 1),
        );
        app.ui.mount(
            crate::shell::ui::UiKey::TabBarItem(2),
            nuotc::Rect::new(30, 0, 13, 1),
        );
        app.ui.commit();

        // Click on Tab 1
        mouse::handle_selection_start(&mut app, &runtime, "s1", 16, 0).await;
        assert_eq!(app.surfaces.active_tab_index(), 1);
        assert_eq!(
            app.surfaces.active_scene(),
            crate::shell::surfaces::SceneKind::Dashboard
        );

        // Click on Tab 2
        mouse::handle_selection_start(&mut app, &runtime, "s1", 32, 0).await;
        assert_eq!(app.surfaces.active_tab_index(), 2);
        assert_eq!(
            app.surfaces.active_scene(),
            crate::shell::surfaces::SceneKind::Settings
        );
    }

    #[tokio::test]
    async fn mouse_click_on_tab_bar_background_is_inert_and_preserves_composer_focus() {
        let mut app = crate::shell::tests::new_app_for_relay_tests();
        let runtime = UiRuntime::minimal_for_test();

        assert!(!app.transcript_focused, "composer active by default");

        // Mount TabBar across full width (0..80) and TabBarItem(0) at (0..14)
        app.ui.begin(nuotc::Rect::new(0, 0, 80, 24));
        app.ui.mount(
            crate::shell::ui::UiKey::TabBar,
            nuotc::Rect::new(0, 0, 80, 1),
        );
        app.ui.mount(
            crate::shell::ui::UiKey::TabBarItem(0),
            nuotc::Rect::new(0, 0, 14, 1),
        );
        app.ui.commit();

        // Click outside tabs on the TabBar background (col 50)
        mouse::handle_selection_start(&mut app, &runtime, "s1", 50, 0).await;
        // ADR-0091 [INV-TAB-11]: TabBar background clicks are inert container gestures
        // and must NOT leak side-effects into active scene context.
        assert!(
            !app.transcript_focused,
            "clicking TabBar background must not blur composer"
        );
    }

    #[tokio::test]
    async fn mouse_click_on_active_thread_tab_is_idempotent_and_preserves_composer_focus() {
        let mut app = crate::shell::tests::new_app_for_relay_tests();
        let runtime = UiRuntime::minimal_for_test();

        app.surfaces
            .open_tab(crate::shell::surfaces::ClientTab::thread(
                "s1",
                Some("Thread 1"),
            ));
        app.surfaces.select_tab(0);
        assert!(!app.transcript_focused, "composer active by default");

        app.ui.begin(nuotc::Rect::new(0, 0, 80, 24));
        app.ui.mount(
            crate::shell::ui::UiKey::TabBar,
            nuotc::Rect::new(0, 0, 80, 1),
        );
        app.ui.mount(
            crate::shell::ui::UiKey::TabBarItem(0),
            nuotc::Rect::new(0, 0, 14, 1),
        );
        app.ui.commit();

        // Click on already-active Thread tab (col 5)
        mouse::handle_selection_start(&mut app, &runtime, "s1", 5, 0).await;
        assert_eq!(app.surfaces.active_tab_index(), 0);
        // ADR-0091 [INV-TAB-11]: Re-selecting the already active tab is an idempotent
        // container gesture that preserves active context.
        assert!(
            !app.transcript_focused,
            "clicking active tab must not blur composer"
        );
    }

    #[tokio::test]
    async fn mouse_click_on_scene_history_back_unwinds_subagent_to_thread() {
        let mut app = crate::shell::tests::new_app_for_relay_tests();
        let runtime = UiRuntime::minimal_for_test();

        app.open_tab(crate::shell::surfaces::ClientTab::thread(
            "conv-1",
            None::<String>,
        ));
        app.enter_subagent("call_sub_1".to_string());
        assert_eq!(
            app.current_scene(),
            crate::shell::surfaces::SceneKind::Subagent
        );
        assert!(app.in_subagent_view());

        // Mount SceneBar at row 1 and SceneHistoryBack at (50..58, 1)
        app.ui.begin(nuotc::Rect::new(0, 0, 80, 24));
        app.ui.mount(
            crate::shell::ui::UiKey::SceneBar,
            nuotc::Rect::new(0, 1, 80, 1),
        );
        app.ui.mount(
            crate::shell::ui::UiKey::SceneHistoryBack,
            nuotc::Rect::new(50, 1, 8, 1),
        );
        app.ui.commit();

        // Click on the SceneHistoryBack button (col 52, row 1)
        mouse::handle_selection_start(&mut app, &runtime, "conv-1", 52, 1).await;
        assert_eq!(
            app.current_scene(),
            crate::shell::surfaces::SceneKind::Thread
        );
        assert!(!app.in_subagent_view());
    }

    #[tokio::test]
    async fn mouse_click_on_scene_breadcrumb_jumps_to_ancestor_depth() {
        let mut app = crate::shell::tests::new_app_for_relay_tests();
        let runtime = UiRuntime::minimal_for_test();

        app.open_tab(crate::shell::surfaces::ClientTab::thread(
            "conv-1",
            None::<String>,
        ));
        app.enter_subagent("call_sub_1".to_string());
        assert_eq!(
            app.current_scene(),
            crate::shell::surfaces::SceneKind::Subagent
        );
        assert!(app.in_subagent_view());

        // Mount SceneBreadcrumb(0) (thread ancestor) at (60..66, 1)
        app.ui.begin(nuotc::Rect::new(0, 0, 80, 24));
        app.ui.mount(
            crate::shell::ui::UiKey::SceneBreadcrumb(0),
            nuotc::Rect::new(60, 1, 6, 1),
        );
        app.ui.commit();

        // Click on the thread ancestor crumb (col 62, row 1)
        mouse::handle_selection_start(&mut app, &runtime, "conv-1", 62, 1).await;
        assert_eq!(
            app.current_scene(),
            crate::shell::surfaces::SceneKind::Thread
        );
        assert!(!app.in_subagent_view());
    }

    #[tokio::test]
    async fn tab_switching_preserves_and_restores_sovereign_tab_context() {
        let mut app = crate::shell::tests::new_app_for_relay_tests();
        let runtime = UiRuntime::minimal_for_test();

        app.surfaces
            .open_tab(crate::shell::surfaces::ClientTab::thread(
                "s1",
                Some("Thread 1"),
            ));
        app.surfaces
            .open_tab(crate::shell::surfaces::ClientTab::dashboard());
        app.surfaces.select_tab(0);

        // Customize Thread 1 context: browse mode, scrolled up, draft input text
        app.transcript_focused = true;
        app.scroll = 42;
        app.input = "draft question for thread 1".to_string();
        app.cursor_position = 5;

        // Switch to Dashboard (Tab 1)
        activate_tab_at_index(&mut app, 1, &runtime, "s1");
        assert_eq!(app.surfaces.active_tab_index(), 1);
        assert_eq!(
            app.surfaces.active_scene(),
            crate::shell::surfaces::SceneKind::Dashboard
        );

        // Switch back to Thread 1 (Tab 0)
        activate_tab_at_index(&mut app, 0, &runtime, "s1");
        assert_eq!(app.surfaces.active_tab_index(), 0);
        assert_eq!(
            app.surfaces.active_scene(),
            crate::shell::surfaces::SceneKind::Thread
        );

        // ADR-0091 [INV-TAB-12]: Restores sovereign tab context exactly as left
        assert!(
            app.transcript_focused,
            "browse focus mode must be preserved across tab switch"
        );
        assert_eq!(app.scroll, 42, "scroll offset must be preserved");
        assert_eq!(
            app.input, "draft question for thread 1",
            "draft text must be preserved"
        );
        assert_eq!(app.cursor_position, 5, "caret position must be preserved");
    }

    #[tokio::test]
    async fn mouse_click_outside_composer_on_unmounted_zone_is_inert() {
        let mut app = crate::shell::tests::new_app_for_relay_tests();
        let runtime = UiRuntime::minimal_for_test();

        assert!(!app.transcript_focused, "composer active by default");

        // Click in outer unmounted zone / dead zone
        mouse::handle_selection_start(&mut app, &runtime, "s1", 79, 10).await;
        // ADR-0091 [INV-TAB-11]: Unmounted zones outside the active transcript content rect
        // are inert and must not leak focus mutations into the composer.
        assert!(
            !app.transcript_focused,
            "clicking outside composer on unmounted zone must be inert"
        );
    }

    #[tokio::test]
    async fn mouse_click_on_dialog_entry_selects_row() {
        let mut app = crate::shell::tests::new_app_for_relay_tests();
        let runtime = UiRuntime::minimal_for_test();

        app.sessions_overview = (0..5)
            .map(|i| crate::shell::tests::overview_row(&format!("s{i}")))
            .collect();
        app.open_dialog(crate::shell::surfaces::DialogKind::Threads);
        let threads = app
            .surfaces
            .dlg_mut::<crate::shell::surfaces::ThreadsDialog>();
        threads.index = 0;
        threads.scroll = 0;

        // Modal bounding box: y=5..20 (height=15).
        // Body starts at y = 5 + 3 = 8.
        // Row 0 is at y=8, Row 1 is at y=9, Row 2 is at y=10.
        app.ui.begin(nuotc::Rect::new(0, 0, 80, 24));
        app.ui.mount(
            crate::shell::ui::UiKey::Overlay(crate::shell::surfaces::OverlaySurface::Dialog(
                crate::shell::surfaces::DialogKind::Threads,
            )),
            nuotc::Rect::new(10, 5, 60, 15),
        );
        app.ui.commit();

        // 1. Click on Row 1 (y = 9) inside modal: selects row 1
        mouse::handle_selection_start(&mut app, &runtime, "s1", 20, 9).await;
        assert_eq!(
            app.surfaces
                .dlg_mut::<crate::shell::surfaces::ThreadsDialog>()
                .index,
            1,
            "clicking row 1 sets index to 1"
        );
        assert!(
            app.surfaces
                .dlg_mut::<crate::shell::surfaces::ThreadsDialog>()
                .follow
        );

        // 2. Click on Row 2 (y = 10) inside modal: selects row 2
        mouse::handle_selection_start(&mut app, &runtime, "s1", 20, 10).await;
        assert_eq!(
            app.surfaces
                .dlg_mut::<crate::shell::surfaces::ThreadsDialog>()
                .index,
            2,
            "clicking row 2 sets index to 2"
        );

        // 3. Sub-view parity [INV-SURFACE-08]: in info_detail mode, clicking inside list area does not corrupt index
        app.surfaces
            .dlg_mut::<crate::shell::surfaces::ThreadsDialog>()
            .info_detail = true;
        mouse::handle_selection_start(&mut app, &runtime, "s1", 20, 8).await;
        assert_eq!(
            app.surfaces
                .dlg_mut::<crate::shell::surfaces::ThreadsDialog>()
                .index,
            2,
            "info_detail mode protects list index"
        );
        app.surfaces
            .dlg_mut::<crate::shell::surfaces::ThreadsDialog>()
            .info_detail = false;

        // 4. Re-clicking an already-selected thread row for a different session switches to target
        mouse::handle_selection_start(&mut app, &runtime, "s1", 20, 10).await;
        assert_eq!(app.switch_to_target, Some("s2".to_string()));

        // 5. Re-clicking the already-selected row for the current session dismisses the dialog
        app.surfaces
            .dlg_mut::<crate::shell::surfaces::ThreadsDialog>()
            .index = 1;
        mouse::handle_selection_start(&mut app, &runtime, "s1", 20, 9).await;
        assert!(
            app.active_dialog().is_none(),
            "re-clicking current session dismisses dialog"
        );
    }

    #[tokio::test]
    async fn mouse_click_on_config_dropdown_selects_and_outside_dismisses() {
        let mut app = crate::shell::tests::new_app_for_relay_tests();
        let runtime = UiRuntime::minimal_for_test();

        app.switch_scene(crate::shell::surfaces::SceneKind::Settings);
        app.websearch_config = Some(nuo_wire::WebSearchConfigView {
            revision: 42,
            provider: nuo_wire::WebSearchProvider::Exa,
            reader: nuo_wire::WebReaderProvider::Disabled,
            timeout_secs: 20,
            searxng_url: None,
            search_credential: nuo_wire::WebCredentialStatus::Stored,
            reader_credential: nuo_wire::WebCredentialStatus::NotRequired,
            capabilities: nuo_wire::web_provider_capabilities(),
        });

        // 1. Open dropdown
        let dropdown = crate::shell::views::settings::build_websearch_provider_dropdown(
            "exa",
            app.websearch_config.as_ref(),
        );
        let anchor = nuo_ui_primitives::components::dropdown::DropdownAnchor::center_screen();
        app.config_dropdown = Some((dropdown, anchor.clone()));
        assert!(app.config_dropdown.is_some());

        // Mount dropdown popup at (20, 5, 40, 10)
        app.ui.begin(nuotc::Rect::new(0, 0, 80, 24));
        app.ui.mount(
            crate::shell::ui::UiKey::ConfigDropdown,
            nuotc::Rect::new(20, 5, 40, 10),
        );
        app.ui.commit();

        // Click outside at (5, 2): dismisses dropdown
        mouse::handle_selection_start(&mut app, &runtime, "s1", 5, 2).await;
        assert!(
            app.config_dropdown.is_none(),
            "clicking outside dismisses dropdown"
        );

        // 2. Open dropdown again and click item 1 (Tavily) at (25, 8)
        let dropdown = crate::shell::views::settings::build_websearch_provider_dropdown(
            "exa",
            app.websearch_config.as_ref(),
        );
        app.config_dropdown = Some((dropdown, anchor));
        app.ui.begin(nuotc::Rect::new(0, 0, 80, 24));
        app.ui.mount(
            crate::shell::ui::UiKey::ConfigDropdown,
            nuotc::Rect::new(20, 5, 40, 10),
        );
        app.ui.commit();

        // Items start at y = 5 + 2 = 7. Row 0 (Exa) at y=7, Row 1 (Tavily) at y=8.
        mouse::handle_selection_start(&mut app, &runtime, "s1", 25, 8).await;
        assert!(
            app.config_dropdown.is_none(),
            "selecting item closes dropdown"
        );
    }

    #[tokio::test]
    async fn mouse_click_on_settings_category_and_row_toggle() {
        let mut app = crate::shell::tests::new_app_for_relay_tests();
        let runtime = UiRuntime::minimal_for_test();

        app.switch_scene(crate::shell::surfaces::SceneKind::Settings);
        app.config_category = 0; // Appearance
        app.config_focus = crate::shell::overlays::ConfigFocus::Categories;

        // Mount Category 1 (Components) at (2, 4, 18, 2) and Detail Row 0 at (24, 4, 40, 2)
        app.ui.begin(nuotc::Rect::new(0, 0, 80, 24));
        app.ui.mount(
            crate::shell::ui::UiKey::SettingsCategory(1),
            nuotc::Rect::new(2, 4, 18, 2),
        );
        app.ui.mount(
            crate::shell::ui::UiKey::SettingsRow(0),
            nuotc::Rect::new(24, 4, 40, 2),
        );
        app.ui.commit();

        // 1. Click Category 1: switches to Components category
        mouse::handle_selection_start(&mut app, &runtime, "s1", 5, 4).await;
        assert_eq!(app.config_category, 1, "switched to Components category");
        assert_eq!(
            app.config_focus,
            crate::shell::overlays::ConfigFocus::Categories
        );

        // 2. Click Detail Row 0 (Components row 0 = Reasoning Traces): toggles reasoning expanded!
        let initial_reasoning = app.reasoning_default_expanded;
        mouse::handle_selection_start(&mut app, &runtime, "s1", 30, 4).await;
        assert_eq!(
            app.config_focus,
            crate::shell::overlays::ConfigFocus::Detail
        );
        assert_eq!(app.config_detail_index, 0);
        assert_eq!(
            app.reasoning_default_expanded, !initial_reasoning,
            "clicking components row toggles setting"
        );

        // 3. Click Detail Row 0 again: toggles back
        mouse::handle_selection_start(&mut app, &runtime, "s1", 30, 4).await;
        assert_eq!(
            app.reasoning_default_expanded, initial_reasoning,
            "clicking components row again toggles setting back"
        );
    }

    #[tokio::test]
    async fn mouse_click_outside_dialog_dismisses_dialog() {
        let mut app = crate::shell::tests::new_app_for_relay_tests();
        let runtime = UiRuntime::minimal_for_test();

        app.open_dialog(crate::shell::surfaces::DialogKind::Tools);
        assert!(app.active_dialog().is_some());
        app.click_outside_dismiss = true;

        // Modal mounted at (10, 5, 60, 15)
        app.ui.begin(nuotc::Rect::new(0, 0, 80, 24));
        app.ui.mount(
            crate::shell::ui::UiKey::Overlay(crate::shell::surfaces::OverlaySurface::Dialog(
                crate::shell::surfaces::DialogKind::Tools,
            )),
            nuotc::Rect::new(10, 5, 60, 15),
        );
        app.ui.commit();

        // Click outside at (2, 2) dismisses the dialog
        mouse::handle_selection_start(&mut app, &runtime, "s1", 2, 2).await;
        assert!(
            app.active_dialog().is_none(),
            "dialog dismissed on backdrop click"
        );
    }

    #[tokio::test]
    async fn mouse_click_on_overlay_modal_does_not_set_transcript_focused() {
        let mut app = scrollable_app();
        let runtime = UiRuntime::minimal_for_test();
        let antigravity = crate::PROVIDER_PRESETS
            .iter()
            .find(|p| p.id == "google-antigravity")
            .expect("antigravity preset exists");

        app.open_preset_chooser();
        app.begin_oauth_add(antigravity, nuo_wire::LoginMethod::Browser);
        assert!(!app.transcript_focused);

        // Mount the OAuth overlay area in UI so pointer target falls within overlay
        app.ui.begin(nuotc::Rect::new(0, 0, 80, 24));
        app.ui.mount(
            crate::shell::ui::UiKey::Overlay(crate::shell::surfaces::OverlaySurface::Sheet(
                crate::shell::surfaces::SheetKind::OAuthPending,
            )),
            nuotc::Rect::new(10, 5, 60, 14),
        );
        app.ui.commit();

        // Mouse click at an arbitrary coordinate inside the overlay where no text cursor exists
        mouse::handle_selection_start(&mut app, &runtime, "s1", 12, 6).await;

        assert!(
            !app.transcript_focused,
            "Click on overlay modal must NOT set transcript_focused to true"
        );
    }
}

#[cfg(test)]
mod view_entry_tests {
    use super::*;

    #[test]
    fn every_show_refreshes_remote_view_data() {
        let mut app = crate::shell::tests::new_app_for_relay_tests();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        app.tx = tx;
        let runtime = UiRuntime::minimal_for_test();

        assert!(enter_panel(
            &mut app,
            crate::shell::surfaces::DialogKind::SessionTree,
            &runtime,
            "s1"
        ));
        assert!(matches!(rx.try_recv(), Ok(AgentRequest::QuerySessionTree)));
        app.dismiss_surface();
        assert!(!enter_panel(
            &mut app,
            crate::shell::surfaces::DialogKind::SessionTree,
            &runtime,
            "s1"
        ));
        assert!(matches!(rx.try_recv(), Ok(AgentRequest::QuerySessionTree)));
    }

    #[test]
    fn sessions_and_skills_have_complete_query_paths() {
        let mut app = crate::shell::tests::new_app_for_relay_tests();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        app.tx = tx;
        let runtime = UiRuntime::minimal_for_test();

        enter_panel(
            &mut app,
            crate::shell::surfaces::DialogKind::Threads,
            &runtime,
            "s1",
        );
        assert!(matches!(
            rx.try_recv(),
            Ok(AgentRequest::QuerySessionsOverview)
        ));
        enter_panel(
            &mut app,
            crate::shell::surfaces::DialogKind::Skills,
            &runtime,
            "s1",
        );
        assert!(matches!(
            rx.try_recv(),
            Ok(AgentRequest::QuerySessionContext)
        ));
    }
}

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

/// Test shims for the dashboard console dispatcher: the internal helpers the
/// console tests drive directly (they need no terminal or clipboard plumbing
/// — just `App` + `UiRuntime`).
#[cfg(test)]
pub(crate) mod host_test_shims {
    use super::{UiRuntime, host};
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
        super::modals::quit_standalone_scene_at_startup(app)
    }
}

/// Cycle a capability-override tri-state (layer 1): unset (inherit
/// from the lower layers) → forced on → forced off → unset. Used by the
/// settings editor's Space cycling on fields 3/4.
fn cycle_tri_state(v: Option<bool>) -> Option<bool> {
    match v {
        None => Some(true),
        Some(true) => Some(false),
        Some(false) => None,
    }
}

async fn execute_command_by_id(
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
            // same scope: dismiss the overlay if one is up, else clear focus,
            // else step back one level *inside* the Scene. It never navigates
            // between Scenes.
            if app.surfaces.active_overlay().is_some() {
                modals::handle_close_modal(app, viewed_session_id);
            } else if app.focused_target.is_some() {
                app.focused_target = None;
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
        CommandId::Quit => {
            // ADR-0038 [INV-ACTION-02]: Quitting the TUI client is strictly a non-destructive Detach.
            tracing::info!(reason = "command_quit", "client detaching");
            return ActionFlow::Exit;
        }
        CommandId::CloseTab => {
            return handle_close_tab(app, runtime, viewed_session_id);
        }
        CommandId::KillThread => {
            // ADR-0038: Explicit kill terminates the thread on the server.
            app.send_intent(AgentRequest::EndSession);
            return ActionFlow::Exit;
        }
        CommandId::CopyFocusedEntry => {
            // The palette's verb form of the focused-entry chord (`y`). Same
            // resolution, same receipt, same failure message — the palette is
            // a discoverability path, not a second copy implementation.
            if let Some(target) = app.focused_target {
                clipboard_ops::copy(
                    app,
                    crate::shell::copy::CopyRequest::Entry {
                        message_idx: target.message_idx,
                    },
                    viewed_session_id,
                    copy_tx,
                    copy_pending,
                );
            } else {
                clipboard_ops::show_copy_failure(
                    app,
                    crate::shell::copy::CopyError::MissingEntry.message(),
                );
            }
        }
        CommandId::CopyRecentDiagnostics => {
            // The escape hatch for everything that never had (or no longer has)
            // a transcript row: transient bubbles, failed local actions, the
            // "could not reach the backend" family. Rendered through the same
            // diagnostic shape so one paste carries the whole sequence.
            match app.render_notices(Some(viewed_session_id)) {
                Some(text) => {
                    clipboard_ops::spawn_clipboard_copy(copy_tx, copy_pending.clone(), text);
                    app.copy_toast_message = "notices copied".to_string();
                    app.copy_toast_failed = false;
                    app.copy_toast_until =
                        Some(std::time::Instant::now() + std::time::Duration::from_millis(1500));
                }
                None => clipboard_ops::show_copy_failure(app, "no notices retained yet"),
            }
        }
        CommandId::CopyDiagnostic => {
            // The palette's verb form of `Shift-y`.
            if let Some(request) = crate::shell::copy::diagnostic_for_current_focus(app) {
                clipboard_ops::copy(app, request, viewed_session_id, copy_tx, copy_pending);
            } else {
                clipboard_ops::show_copy_failure(
                    app,
                    crate::shell::copy::CopyError::MissingEntry.message(),
                );
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
            let text = std::mem::take(&mut app.input);
            app.set_cursor(0);
            commands::handle_send_chat(app, runtime, viewed_session_id, text).await;
        }
        CommandId::QueueFollowUp => {
            let text = std::mem::take(&mut app.input);
            app.set_cursor(0);
            commands::handle_queue_follow_up(app, runtime, viewed_session_id, text).await;
        }
        CommandId::SteerImmediate => {
            let text = std::mem::take(&mut app.input);
            app.set_cursor(0);
            commands::handle_send_steer(app, runtime, viewed_session_id, text).await;
        }
        CommandId::ToggleSendMode => {
            app.composer_send_mode = match app.composer_send_mode {
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
        CommandId::ThreadStatsOpenTrace => {
            enter_panel(
                app,
                crate::shell::surfaces::DialogKind::SessionTrace,
                runtime,
                viewed_session_id,
            );
        }
        CommandId::TraceInspectDetail => {
            if app.active_dialog() == Some(crate::shell::surfaces::DialogKind::SessionTrace) {
                let has_rounds = app
                    .token_source_report(viewed_session_id)
                    .map(|report| render::telemetry_round_count(&report) > 0)
                    .unwrap_or(false);
                if has_rounds {
                    let d = app
                        .surfaces
                        .dlg_mut::<crate::shell::surfaces::SessionTraceDialog>();
                    d.detail = true;
                    d.turn_cursor = 0;
                    d.scroll = 0;
                }
            }
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
                    .dlg_mut::<crate::shell::surfaces::PortShareDialog>()
                    .begin();
                app.surfaces
                    .dlg_mut::<crate::shell::surfaces::PortShareDialog>()
                    .loading = true;
                app.open_dialog(crate::shell::surfaces::DialogKind::PortShare);
                app.send_intent(nuo_wire::AgentRequest::QueryServerEndpoints);
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
            app.send_intent(nuo_wire::AgentRequest::SlashCommand("/fork".to_string()));
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
            if app.is_queue_blocked(&sid) {
                app.resume_queue(&sid);
                show_local_toast(
                    app,
                    "Queue resumed",
                    false,
                    std::time::Duration::from_millis(2000),
                );
            } else {
                app.block_queue(&sid);
                show_local_toast(
                    app,
                    "Queue paused",
                    false,
                    std::time::Duration::from_millis(2000),
                );
            }
        }
        CommandId::ClearQueue => {
            app.pending_dispatch.clear();
            show_local_toast(
                app,
                "Queue cleared",
                false,
                std::time::Duration::from_millis(2000),
            );
        }
        CommandId::PermissionsClearAll => {
            let revocations = match app.session_context.as_ref() {
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

/// Apply a PreAttach surface decision.
///
/// `TrustCommand` routes through the canonical `/trust` slash command
/// — the same path the server's `/trust` handler uses, so persistence
/// AND the live reload stay owned by the one code path. The per-frame
/// sync observes the subsequent `Trusted` snapshot and clears
/// `App::pre_attach` on the next frame.
///
/// `Quit` flips `should_quit` so the loop terminates on its next
/// iteration; the cleanup path (EndSession wire message, terminal
/// restore) runs in the existing `run_tui` epilogue. There is no
/// chat surface to fall through to — dismissing is
/// quitting.
async fn apply_pre_attach_decision(
    decision: Option<crate::shell::PreAttachDecision>,
    app: &mut App,
    _runtime: &UiRuntime,
) {
    let Some(decision) = decision else {
        return;
    };
    match decision {
        crate::shell::PreAttachDecision::Trust { domains } => {
            tracing::info!(
                ?domains,
                "nuo: PreAttach decision — granting workspace trust directly"
            );
            app.send_intent(AgentRequest::TrustWorkspace { domains });
            // The per-frame sync clears `pre_attach` once the
            // republished snapshot reports Trusted. The PreAttach surface
            // shows a `Trusting workspace...` state while awaiting the snapshot.
        }
        crate::shell::PreAttachDecision::Quit => {
            tracing::info!("nuo: PreAttach decision — quitting (keep quarantined)");
            // Drop the PreAttach state so the listener's
            // `trust_gate_dismissed` latch (set below) is what stops
            // a subsequent snapshot from re-mounting within this
            // terminating run.
            app.pre_attach = None;
            // Mark the gate dismissed so any in-flight snapshot the
            // listener publishes before the loop terminates cannot
            // re-mount PreAttach on the dying frame.
            app.should_quit.store(true, Ordering::SeqCst);
            app.send_intent(AgentRequest::EndSession);
        }
    }
}
