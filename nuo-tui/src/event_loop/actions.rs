//! Input-action dispatch for the TUI event loop: the `match` over
//! [`input::InputAction`] that `run_app_loop`'s input-drain stage ran inline,
//! moved here verbatim (one arm per variant) with only the loop-control
//! statements rewritten as [`ActionFlow`] values.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use nuotc::Terminal;
use tokio::sync::mpsc;

use nuo_wire::{AgentRequest, PermissionDecision, PermissionRequest};

use crate::App;
use crate::clipboard;
use crate::clipboard_ops;
use crate::input;
use crate::model::selection::SelectionState;
use crate::render;
use crate::render::Theme;
use crate::surfaces::{DialogKind, SceneKind, SheetKind};

use super::runtime::UiRuntime;
use super::sync::show_local_toast;
use super::transcript::{extract_focused_target_text, extract_selection_text};

mod commands;
mod host;
mod modals;
mod mouse;

pub(crate) use modals::{
    activate_picked_model, effective_reasoning_effort, handle_permission_submit, modal_page_step,
    question_effects,
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
    pub session: &'a crate::SessionSource,
    pub viewed_session_id: &'a str,
    pub copy_tx: &'a mpsc::UnboundedSender<Result<clipboard::CopyOutcome, String>>,
    pub copy_pending: &'a Arc<AtomicUsize>,
    pub paste_tx: &'a mpsc::UnboundedSender<clipboard::ClipboardRead>,
    pub sgr_guard: &'a mut input::SgrLeakGuard,
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
        // so the wheel / page keys drive the conversation behind it, not the
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
    } = ctx;
    let runtime = *runtime;
    let session = *session;
    let viewed_session_id = *viewed_session_id;
    let copy_tx = *copy_tx;
    let copy_pending = *copy_pending;
    let paste_tx = *paste_tx;
    let sgr_guard = &mut **sgr_guard;
    // While the dashboard's kill confirm is armed, only the confirming `k`
    // (or the confirm-cancelling paths inside the dashboard arms) keeps it
    // alive: any other action — navigation, prompt, focus toggle, Esc —
    // disarms it. The armed state lives exactly one keystroke.
    if app.host_kill_confirm.is_some()
        && app.current_scene() == SceneKind::Dashboard
        && !matches!(
            action,
            input::InputAction::HostKillSelected | input::InputAction::None
        )
    {
        host::cancel_kill_confirm(app);
    }

    // While the Ctrl+C quit window is armed, any user action other than
    // Ctrl+C cancels the armed state so subsequent typing or interaction
    // does not inadvertently exit the program.
    if app.ctrl_c_armed() && !matches!(action, input::InputAction::CtrlC | input::InputAction::None)
    {
        app.arm_ctrl_c(None);
    }

    // An armed scene namespace survives exactly one keystroke: any action other
    // than re-arming it disarms it, so a half-typed chord cannot linger and
    // change the meaning of a later, unrelated key (ADR-0298 §1).
    if app.scene_namespace_armed && !matches!(action, input::InputAction::SetSceneNamespaceArmed(_))
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
            use crossterm::event::EnableMouseCapture;
            let _ = crossterm::execute!(std::io::stdout(), EnableMouseCapture);
            sgr_guard.reset();
        }
        input::InputAction::Quit => {
            app.send_intent(AgentRequest::EndSession);
            tracing::info!(reason = "slash_exit", "app exiting");
            return ActionFlow::Exit;
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
                crate::app::ComposerSendMode::Steer => crate::app::ComposerSendMode::FollowUp,
                crate::app::ComposerSendMode::FollowUp => crate::app::ComposerSendMode::Steer,
            };
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
            let target = if app.active_dialog() == Some(DialogKind::Models) {
                let rows = app.models_flat_filtered();
                let picked = rows.get(app.active_index()).or_else(|| rows.first());
                // A model the provider declared unavailable mirrors the official
                // CLI's greyed-out menu row: refuse activation with a toast
                // stating the provider's OWN reason when it gave one, instead of
                // sending a request the server must refuse. When the provider
                // declared no reason, say only that (ADR-0273) — never invent
                // "your plan", which is a different, unstated diagnosis.
                if let Some(row) = picked.filter(|row| !row.usable) {
                    let message = row.locked_reason.as_deref().map_or_else(
                        || "This model is unavailable on your account".to_string(),
                        |reason| format!("This model is unavailable: {reason}"),
                    );
                    crate::event_loop::sync::show_local_toast(
                        app,
                        message,
                        true,
                        std::time::Duration::from_millis(2000),
                    );
                    return ActionFlow::Handled;
                }
                picked.map(|row| (row.provider_id.clone(), row.model.clone()))
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
            if app.surfaces.contains_sheet(SheetKind::OAuthPending) {
                app.send_intent(AgentRequest::CancelAuthorizeOAuth);
                app.awaiting_oauth_add = false;
                app.oauth_pending_url.clear();
                app.oauth_pending_user_code.clear();
                app.oauth_pending_message.clear();
                app.oauth_pending_error = None;
                app.open_preset_chooser();
            }
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
            // Return to the Connections list the chooser was opened
            // from; the chat draft stays parked in stashed_input.
            if app.surfaces.contains_sheet(SheetKind::ProviderPreset) {
                app.input.clear();
                app.set_cursor(0);
                app.pop_transient_surface();
                app.set_picker_search(false);
                app.reset_picker_nav();
            }
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
            // Return to the Connections list the editor was opened
            // from; the chat draft stays parked in stashed_input.
            if app.surfaces.contains_sheet(SheetKind::CustomProvider) {
                app.input.clear();
                app.set_cursor(0);
                app.custom_field = 0;
                app.custom_edit_id = None;
                app.pop_transient_surface();
                app.set_picker_search(false);
                app.reset_picker_nav();
            }
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
            // highlighted MODEL (falling back to the first visible row).
            // Favorite is model-level (ADR-0046), so the id is the
            // model wire id. Sending the request is enough; the backend
            // pushes a fresh snapshot that flips the ★ next frame.
            if app.active_dialog() == Some(DialogKind::Models) {
                let ranked = app.models_flat_filtered();
                if let Some(row) = ranked.get(app.active_index()).or_else(|| ranked.first()) {
                    app.send_intent(AgentRequest::ToggleFavorite {
                        id: row.model.clone(),
                    });
                }
            }
        }
        input::InputAction::ProviderPickerBlockModel => {
            // ADR-0203 §10: 'x' blocks/intercepts the highlighted model from the connection pipe.
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
            // Cycle the vision capability override (ADR-0149 layer 1):
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
                crate::surfaces::DialogKind::Sessions,
                runtime,
                viewed_session_id,
            );
        }
        input::InputAction::NavigateDashboard => {
            enter_scene(app, crate::surfaces::SceneKind::Dashboard, runtime);
        }
        input::InputAction::OpenModels => {
            enter_panel(
                app,
                crate::surfaces::DialogKind::Models,
                runtime,
                viewed_session_id,
            );
        }
        input::InputAction::OpenConnections => {
            enter_panel(
                app,
                crate::surfaces::DialogKind::Connections,
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
            if app.active_dialog() == Some(DialogKind::Connections) && app.surfaces.dlg_mut::<crate::surfaces::ConnectionsDialog>().info_detail {
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
                if app.surfaces.dlg_mut::<crate::surfaces::ModelsDialog>().refreshing
                    || app.surfaces.dlg_mut::<crate::surfaces::ConnectionsDialog>().refreshing
                {
                    show_local_toast(
                        app,
                        "Model refresh already in progress…",
                        false,
                        std::time::Duration::from_millis(1500),
                    );
                } else {
                    app.surfaces.dlg_mut::<crate::surfaces::ModelsDialog>().refreshing = true;
                    app.surfaces.dlg_mut::<crate::surfaces::ConnectionsDialog>().refreshing = true;
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
            enter_panel(
                app,
                crate::surfaces::DialogKind::HistorySearch,
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
                .get(app.surfaces.dlg_mut::<crate::surfaces::HistorySearchDialog>().index)
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
                crate::app::DraftAdoption::Replace,
            );
            // The selection replaces the in-progress draft, and the search
            // filter query's task is completed, so query and cursor reset.
            app.surfaces.dismiss_all_overlays();
            app.surfaces.dlg_mut::<crate::surfaces::HistorySearchDialog>().search = false;
            app.surfaces.dlg_mut::<crate::surfaces::HistorySearchDialog>().index = 0;
            app.surfaces.dlg_mut::<crate::surfaces::HistorySearchDialog>().query.clear();
            app.surfaces.dlg_mut::<crate::surfaces::HistorySearchDialog>().query.cursor = 0;
            app.input_scroll = 0;
            app.suggestion_index = None;
            // A programmatic input replacement — latch the dismissal so
            // a slash-command selection doesn't flash its completion
            // popup until the next real edit.
            app.completion_dismissed = true;
            app.reset_to_conversation();
        }
        input::InputAction::HistoryDeleteSelected => {
            if app.active_composer_extension()
                == Some(crate::composer_extension::ComposerExtensionKind::HistorySearch)
            {
                app.delete_selected_history_entry();
            }
        }
        input::InputAction::OpenPermissions => {
            enter_panel(
                app,
                crate::surfaces::DialogKind::Permissions,
                runtime,
                viewed_session_id,
            );
        }
        input::InputAction::OpenTools => {
            enter_panel(
                app,
                crate::surfaces::DialogKind::Tools,
                runtime,
                viewed_session_id,
            );
        }
        input::InputAction::OpenUsage => {
            enter_panel(
                app,
                crate::surfaces::DialogKind::UsageStats,
                runtime,
                viewed_session_id,
            );
        }
        input::InputAction::OpenMcp => {
            enter_panel(
                app,
                crate::surfaces::DialogKind::Mcp,
                runtime,
                viewed_session_id,
            );
        }
        input::InputAction::OpenSkills => {
            enter_panel(
                app,
                crate::surfaces::DialogKind::Skills,
                runtime,
                viewed_session_id,
            );
        }
        input::InputAction::SkillsToggleDetail => {
            // Toggle the detail block of the selected skill row. Re-pressing
            // Enter on an already-expanded row collapses it.
            let idx = app.active_index();
            app.surfaces.dlg_mut::<crate::surfaces::SkillsDialog>().expanded = if app.surfaces.dlg_mut::<crate::surfaces::SkillsDialog>().expanded == Some(idx) {
                None
            } else {
                Some(idx)
            };
            app.set_active_follow(true);
        }
        input::InputAction::OpenConfig => {
            enter_scene(app, crate::surfaces::SceneKind::Settings, runtime);
        }
        input::InputAction::ConfigFocusToggle => {
            if app.current_scene() == SceneKind::Settings {
                app.config_focus = match app.config_focus {
                    crate::overlays::ConfigFocus::Categories => {
                        crate::overlays::ConfigFocus::Detail
                    }
                    crate::overlays::ConfigFocus::Detail => {
                        crate::overlays::ConfigFocus::Categories
                    }
                };
            }
        }
        input::InputAction::ConfigActivate => {
            if app.current_scene() == SceneKind::Settings {
                let ws_path = if app.current_workspace.is_empty() {
                    None
                } else {
                    Some(std::path::Path::new(&app.current_workspace))
                };
                let active_category =
                    crate::overlays::ConfigCategory::from_index(app.config_category);
                match app.config_focus {
                    crate::overlays::ConfigFocus::Categories => {
                        app.config_focus = crate::overlays::ConfigFocus::Detail;
                        if active_category == crate::overlays::ConfigCategory::Appearance {
                            app.config_detail_index = Theme::color_scheme_index_with_workspace(
                                &app.color_scheme,
                                ws_path,
                            );
                        } else {
                            app.config_detail_index = 0;
                        }
                    }
                    crate::overlays::ConfigFocus::Detail => {
                        match active_category {
                            crate::overlays::ConfigCategory::Appearance => {
                                if app.profile.supports_color_themes() {
                                    // Appearance category:
                                    let schemes =
                                        Theme::available_color_schemes_with_workspace(ws_path);
                                    let sel_idx = app.config_detail_index % schemes.len().max(1);
                                    if let Some(scheme) = schemes.get(sel_idx) {
                                        let name = &scheme.id;
                                        app.color_scheme = name.to_string();
                                        app.theme = Theme::resolve_with_profile(
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
                            crate::overlays::ConfigCategory::Components => {
                                // Rows are resolved by identity, never by a
                                // literal index: the panel's ordering lives in
                                // `settings::components::row_for_index`, so a
                                // new tool component needs no arm here
                                // (ADR-0020).
                                use crate::views::settings::components::{
                                    ComponentRowId, row_for_index,
                                };
                                match row_for_index(app.config_detail_index) {
                                    Some(ComponentRowId::Reasoning) => {
                                        // Reasoning Traces (thinking)
                                        let next = !crate::config::reasoning_default_expanded(&app.tui_config);
                                        app.tui_config
                                            .default_expanded
                                            .insert(crate::config::THINKING_KEY.to_string(), next);
                                        app.reasoning_default_expanded = next;
                                        app.save_tui_config();
                                    }
                                    Some(ComponentRowId::Tool(component)) => {
                                        // One row per declared component; the
                                        // setter fans the choice out to every
                                        // name the component owns.
                                        let next = !crate::config::tool_default_expanded(
                                            &app.tui_config,
                                            component.primary_name(),
                                        );
                                        crate::config::set_component_default_expanded(
                                            &mut app.tui_config,
                                            component,
                                            next,
                                        );
                                        app.save_tui_config();
                                    }
                                    Some(ComponentRowId::AutoScroll) => {
                                        // Auto-Scroll on Expand
                                        app.expand_auto_scroll = !app.expand_auto_scroll;
                                        app.tui_config.expand_auto_scroll = app.expand_auto_scroll;
                                        app.save_tui_config();
                                    }
                                    None => {}
                                }
                            }
                            crate::overlays::ConfigCategory::WebSearch | crate::overlays::ConfigCategory::WebReader => {
                                let Some(revision) =
                                    app.websearch_config.as_ref().map(|config| config.revision)
                                else {
                                    return ActionFlow::NextEvent;
                                };
                                let is_search = active_category == crate::overlays::ConfigCategory::WebSearch;
                                match app.config_detail_index {
                                    0 => {
                                        let anchor = if let Some(target_rect) =
                                            app.config_selected_rect
                                        {
                                            crate::components::dropdown::DropdownAnchor::anchored(
                                                target_rect,
                                                crate::components::dropdown::DropdownPlacement::Auto,
                                            )
                                        } else {
                                            crate::components::dropdown::DropdownAnchor::center_screen()
                                        };
                                        if is_search {
                                            let current = app
                                                .websearch_config
                                                .as_ref()
                                                .map(|ws| ws.provider.as_str())
                                                .unwrap_or("exa");
                                            let dropdown =
                                                crate::views::settings::build_websearch_provider_dropdown(
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
                                                crate::views::settings::build_websearch_reader_dropdown(
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
                                        app.send_intent(AgentRequest::UpdateWebSearchConfig(
                                            Box::new(nuo_wire::WebSearchConfigUpdate {
                                                expected_revision: revision,
                                                timeout_secs: Some(next),
                                                ..Default::default()
                                            }),
                                        ));
                                    }
                                    2 => {
                                        // Collect the editor prefill while the config
                                        // borrow is live, then drop it before mutating `app`.
                                        let editor = app.websearch_config.as_ref().and_then(|ws| {
                                            let (axis, id) = if is_search {
                                                ("search", ws.provider.id())
                                            } else {
                                                ("reader", ws.reader.id())
                                            };
                                            let capability = ws.capabilities.iter().find(|capability| {
                                                capability.id == id
                                                    && capability.axis == if is_search {
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
                        }
                    }
                }
            }
        }
        input::InputAction::ConfigSegmentPrev => {
            if app.current_scene() == SceneKind::Settings {
                if crate::overlays::ConfigCategory::from_index(app.config_category)
                    == crate::overlays::ConfigCategory::WebReader
                {
                    app.config_category = crate::overlays::ConfigCategory::WebSearch as usize;
                    app.config_detail_index = 0;
                    app.config_detail_scroll = 0;
                    app.config_hover_index = None;
                }
            }
        }
        input::InputAction::ConfigSegmentNext => {
            if app.current_scene() == SceneKind::Settings {
                if crate::overlays::ConfigCategory::from_index(app.config_category)
                    == crate::overlays::ConfigCategory::WebSearch
                {
                    app.config_category = crate::overlays::ConfigCategory::WebReader as usize;
                    app.config_detail_index = 0;
                    app.config_detail_scroll = 0;
                    app.config_hover_index = None;
                }
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
            let rows = crate::overlays::session::project_session_rows(
                &app.sessions_overview,
                Some(&app.surfaces.dlg_mut::<crate::surfaces::SessionsDialog>().expanded),
            );
            if let Some(item) = rows.get(app.active_index().min(rows.len().saturating_sub(1))) {
                let session = item.session();
                let id = session.id.clone();
                let short_id = crate::session::short_session_id(&id);
                app.dismiss_active_dialog();
                app.set_active_index(0);
                // A session was chosen from the startup picker, so a
                // real conversation now backs the view: subsequent
                // `/sessions` modals should behave as ordinary
                // transient overlays (Esc = dismiss, not quit).
                app.startup_overlay = crate::StartupOverlay::None;
                app.switching_session = Some(short_id.clone());
                let _ = short_id;
                app.messages.clear();
                app.scroll = 0;
                app.send_intent(AgentRequest::SlashCommand(format!("/sessions {}", id)));
            }
        }
        input::InputAction::HostPreviewSelected => {
            // Enter on a dock selection opens the read-only preview
            // modal. Selection alone never opens it; Esc closes.
            let idx = app
                .modal_index
                .min(app.host_sessions.len().saturating_sub(1));
            let order = crate::overlays::creation_order(&app.host_sessions);
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
            let order = crate::overlays::creation_order(&app.host_sessions);
            if let Some(row) = order.get(idx).map(|&i| &app.host_sessions[i]) {
                // Switching to the current session is a no-op.
                let switchable = row.id != viewed_session_id;
                if switchable {
                    app.switch_to_target = Some(row.id.clone());
                    app.should_quit.store(true, Ordering::SeqCst);
                }
                app.dismiss_active_dialog();
                app.set_active_index(0);
                app.host_prompting = false;
            }
        }
        input::InputAction::HostFocusToggle => {
            app.host_focus = match app.host_focus {
                crate::overlays::DashboardFocus::List => crate::overlays::DashboardFocus::Detail,
                crate::overlays::DashboardFocus::Detail => crate::overlays::DashboardFocus::List,
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
            // The composer is a command line now (ADR-0097 §2 grammar plus
            // slash verbs): `@3 text` addresses, `/kill`-family manages,
            // bare text keeps the legacy role — prompt the selection, or
            // create when the prompt was opened with `n`.
            host::dispatch_console_command(app, runtime, &text, create_new).await;
        }
        input::InputAction::DeleteSelectedSession => {
            let rows = crate::overlays::session::project_session_rows(
                &app.sessions_overview,
                Some(&app.surfaces.dlg_mut::<crate::surfaces::SessionsDialog>().expanded),
            );
            let idx = app.active_index().min(rows.len().saturating_sub(1));
            if let Some(item) = rows.get(idx) {
                let id = item.session().id.clone();
                app.sessions_overview.retain(|s| s.id != id);
                app.surfaces.dlg_mut::<crate::surfaces::SessionsDialog>().expanded.remove(&id);
                let new_rows = crate::overlays::session::project_session_rows(
                    &app.sessions_overview,
                    Some(&app.surfaces.dlg_mut::<crate::surfaces::SessionsDialog>().expanded),
                );
                app.set_active_index(app.active_index().min(new_rows.len().saturating_sub(1)));
                app.send_intent(AgentRequest::DeleteSession { id });
            }
        }
        input::InputAction::CreateNewSession => {
            app.startup_overlay = crate::StartupOverlay::None;
            app.dismiss_active_dialog();
            app.send_intent(AgentRequest::SlashCommand("/new".to_string()));
        }
        input::InputAction::OpenSessionInfo => {
            // Drill into the session-info sub-view for the highlighted
            // row. Request the full detail (complete last prompt,
            // timestamps) on demand — the picker rows only carry a
            // truncated preview. While the round-trip is in flight the
            // body shows a loading state.
            let rows = crate::overlays::session::project_session_rows(
                &app.sessions_overview,
                Some(&app.surfaces.dlg_mut::<crate::surfaces::SessionsDialog>().expanded),
            );
            if let Some(item) = rows.get(app.active_index().min(rows.len().saturating_sub(1))) {
                let session = item.session();
                app.surfaces.dlg_mut::<crate::surfaces::SessionsDialog>().info_detail = true;
                app.session_detail = None;
                app.surfaces.dlg_mut::<crate::surfaces::SessionsDialog>().info_scroll = 0;
                app.send_intent(AgentRequest::QuerySessionDetail {
                    id: session.id.clone(),
                });
            }
        }
        input::InputAction::ToggleSessionTimelineExpand => {
            let rows = crate::overlays::session::project_session_rows(
                &app.sessions_overview,
                Some(&app.surfaces.dlg_mut::<crate::surfaces::SessionsDialog>().expanded),
            );
            let idx = app.active_index().min(rows.len().saturating_sub(1));
            if let Some(item) = rows.get(idx) {
                match item {
                    crate::overlays::session::SessionPickerItem::Trunk {
                        session,
                        child_count,
                        ..
                    } if *child_count > 0 => {
                        let id = session.id.clone();
                        if app.surfaces.dlg_mut::<crate::surfaces::SessionsDialog>().expanded.contains(&id) {
                            app.surfaces.dlg_mut::<crate::surfaces::SessionsDialog>().expanded.remove(&id);
                        } else {
                            app.surfaces.dlg_mut::<crate::surfaces::SessionsDialog>().expanded.insert(id);
                        }
                    }
                    _ => {}
                }
            }
        }
        input::InputAction::OpenConnectionDetail => {
            let providers = app.providers_filtered();
            if let Some(ranked) =
                providers.get(app.active_index().min(providers.len().saturating_sub(1)))
            {
                app.surfaces.dlg_mut::<crate::surfaces::ConnectionsDialog>().info_detail = true;
                app.surfaces.dlg_mut::<crate::surfaces::ConnectionsDialog>().info_standalone = false;
                app.connection_detail = None;
                app.surfaces.dlg_mut::<crate::surfaces::ConnectionsDialog>().info_scroll = 0;
                app.surfaces.dlg_mut::<crate::surfaces::ConnectionsDialog>().models_expanded = false;
                app.send_intent(AgentRequest::QueryConnectionDetail {
                    id: ranked.id.clone(),
                    force_refresh: false,
                });
            }
        }
        input::InputAction::OpenActiveConnectionDetail => {
            open_active_connection_detail(app, runtime, viewed_session_id);
        }
        input::InputAction::ToggleConnectionModelsExpanded => {
            if app.active_dialog() == Some(DialogKind::Connections) && app.surfaces.dlg_mut::<crate::surfaces::ConnectionsDialog>().info_detail {
                app.surfaces.dlg_mut::<crate::surfaces::ConnectionsDialog>().models_expanded = !app.surfaces.dlg_mut::<crate::surfaces::ConnectionsDialog>().models_expanded;
            }
        }
        input::InputAction::CloseModal => {
            modals::handle_close_modal(app, viewed_session_id);
        }
        input::InputAction::CloseScene => {
            // An overlay floating above the scene is the visual foreground, so
            // it is dismissed first; the scene itself is left on the next
            // press (ADR-0298 §1). Leaving a *standalone* scene (a startup
            // `nuo dashboard` / `nuo settings` with no requested
            // conversation) is a program exit instead: there is no
            // conversation to return to. The leader arm itself is cleared by
            // the shared pre-dispatch reset, so neither branch repeats it.
            if app.active_dialog().is_some() {
                app.dismiss_surface();
            } else if !modals::quit_standalone_scene_at_startup(app) {
                app.close_scene();
            }
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
        input::InputAction::TelemetryActivate => {
            if app.active_dialog() == Some(DialogKind::Telemetry) {
                if app.surfaces.dlg_mut::<crate::surfaces::TelemetryDialog>().tab == crate::overlays::telemetry::TelemetryTab::Overview {
                    app.surfaces.dlg_mut::<crate::surfaces::TelemetryDialog>().tab = crate::overlays::telemetry::TelemetryTab::Activity;
                    app.surfaces.dlg_mut::<crate::surfaces::TelemetryDialog>().scroll = 0;
                } else if !app.surfaces.dlg_mut::<crate::surfaces::TelemetryDialog>().detail {
                    let has_rounds = app
                        .token_source_report(viewed_session_id)
                        .map(|report| render::telemetry_round_count(&report) > 0)
                        .unwrap_or(false);
                    if has_rounds {
                        app.surfaces.dlg_mut::<crate::surfaces::TelemetryDialog>().detail = true;
                        app.surfaces.dlg_mut::<crate::surfaces::TelemetryDialog>().turn_cursor = 0;
                        app.surfaces.dlg_mut::<crate::surfaces::TelemetryDialog>().scroll = 0;
                    }
                } else if app.surfaces.dlg_mut::<crate::surfaces::TelemetryDialog>().turn.is_none() {
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
                            app.surfaces.dlg_mut::<crate::surfaces::TelemetryDialog>().turn_cursor,
                        )
                    }) {
                        app.surfaces.dlg_mut::<crate::surfaces::TelemetryDialog>().turn = Some(key);
                        app.surfaces.dlg_mut::<crate::surfaces::TelemetryDialog>().scroll = 0;
                    }
                }
            }
        }
        input::InputAction::TelemetryNextTab => {
            if app.active_dialog() == Some(DialogKind::Telemetry) {
                app.surfaces.dlg_mut::<crate::surfaces::TelemetryDialog>().tab = match app.surfaces.dlg_mut::<crate::surfaces::TelemetryDialog>().tab {
                    crate::overlays::telemetry::TelemetryTab::Overview => {
                        crate::overlays::telemetry::TelemetryTab::Activity
                    }
                    crate::overlays::telemetry::TelemetryTab::Activity => {
                        crate::overlays::telemetry::TelemetryTab::Overview
                    }
                };
                app.surfaces.dlg_mut::<crate::surfaces::TelemetryDialog>().scroll = 0;
            }
        }
        input::InputAction::TelemetryPrevTab => {
            if app.active_dialog() == Some(DialogKind::Telemetry) {
                app.surfaces.dlg_mut::<crate::surfaces::TelemetryDialog>().tab = match app.surfaces.dlg_mut::<crate::surfaces::TelemetryDialog>().tab {
                    crate::overlays::telemetry::TelemetryTab::Overview => {
                        crate::overlays::telemetry::TelemetryTab::Activity
                    }
                    crate::overlays::telemetry::TelemetryTab::Activity => {
                        crate::overlays::telemetry::TelemetryTab::Overview
                    }
                };
                app.surfaces.dlg_mut::<crate::surfaces::TelemetryDialog>().scroll = 0;
            }
        }
        input::InputAction::TelemetrySetTab(tab) => {
            if app.active_dialog() == Some(DialogKind::Telemetry) && app.surfaces.dlg_mut::<crate::surfaces::TelemetryDialog>().tab != tab {
                app.surfaces.dlg_mut::<crate::surfaces::TelemetryDialog>().tab = tab;
                app.surfaces.dlg_mut::<crate::surfaces::TelemetryDialog>().scroll = 0;
            }
        }
        input::InputAction::ToggleDialogKeys => {
            let open = !app.dialog_keys();
            app.set_dialog_keys(open);
        }
        input::InputAction::DialogKeysScroll { delta } => {
            if let Some(scroll) = app.dialog_keys_scroll() {
                if delta < 0 {
                    *scroll = scroll.saturating_sub((-delta) as usize);
                } else {
                    *scroll = scroll.saturating_add(delta as usize);
                }
            }
        }
        input::InputAction::ScrollUp => {
            scroll_tick(app, false);
        }
        input::InputAction::ScrollDown => {
            scroll_tick(app, true);
        }
        input::InputAction::Wheel { up, x, y } => {
            handle_wheel(app, up, x, y);
        }
        input::InputAction::ScrollPageUp => {
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
            if let Some(text) = extract_selection_text(
                &app.selection,
                app.focused_messages(),
                &app.input,
                &app.ui.document,
                app.drag.cell_info.as_ref(),
            ) {
                clipboard_ops::spawn_clipboard_copy(copy_tx, copy_pending.clone(), text);
            } else if let Some(target) = app.focused_target
                && let Some(text) = extract_focused_target_text(app.focused_messages(), target)
            {
                clipboard_ops::spawn_clipboard_copy(copy_tx, copy_pending.clone(), text);
            }
        }
        input::InputAction::CopyFocusedTarget => {
            if let Some(target) = app.focused_target
                && let Some(text) = extract_focused_target_text(app.focused_messages(), target)
            {
                clipboard_ops::spawn_clipboard_copy(copy_tx, copy_pending.clone(), text);
            }
        }
        input::InputAction::CtrlC => {
            return commands::handle_ctrl_c(app, viewed_session_id, copy_tx, copy_pending);
        }
        input::InputAction::OpenQueue => {
            // F2 opens the queue overview — the full outbox list that
            // the persistent queue bar previews. The selection starts
            // at the front (the next item to pop). This mirrors a
            // click on the queue bar.
            //
            // A retained view (ADR-0133 phase 4): the cursor/scroll
            // survive hide; the auto-block runs on EVERY entry (not just
            // first open) because it is an editing safety latch — the
            // matching resume is the view's exit hook (hide). A
            // persistent user block is a different thing (`F3` /
            // Ctrl+P), unaffected here.
            enter_panel(
                app,
                crate::surfaces::DialogKind::Queue,
                runtime,
                viewed_session_id,
            );
        }
        input::InputAction::OpenTelemetry => {
            // Ctrl+O opens the session telemetry report (Context & Performance).
            enter_panel(
                app,
                crate::surfaces::DialogKind::Telemetry,
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
            // cannot clobber a draft (ADR-0192).
            app.cancel_history_recall();
        }
        input::InputAction::ActivateFocusedTarget => {
            // Stack-top focus dispatch: delegate to the focused component's own
            // interactive key handler (Enter activates: expand/collapse, subagent zoom, etc.).
            app.dispatch_focused_target_key(crate::keymap::Key::ENTER);
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
        input::InputAction::InterruptSide => {
            // Esc inside an aside view (ADR-0103 §2): interrupt the viewed
            // aside's round with the same armed press-twice contract as the
            // main view's Esc interrupt. Never leaves the view, never closes
            // the aside.
            handle_esc_interrupt_with_runtime(app, runtime, commands::InterruptTarget::Aside).await;
        }
        input::InputAction::InterruptSubagent => {
            // Esc inside the Subagent scene (ADR-0205): interrupt only the
            // viewed child, with the same armed press-twice contract. It never
            // reaches the primary round, so the outer turn keeps running.
            handle_esc_interrupt_with_runtime(
                app,
                runtime,
                commands::InterruptTarget::Subagent,
            )
            .await;
        }
        input::InputAction::OpenBtwList => {
            // F5 / `/btw list` (ADR-0103 §5): ask the harness for a fresh
            // list and pop the modal once the rows land. The open signal is
            // consumed by the loop's sync stage, so a slow harness reply
            // simply opens with the last known rows and refreshes in place.
            enter_panel(
                app,
                crate::surfaces::DialogKind::Asides,
                runtime,
                viewed_session_id,
            );
        }
        input::InputAction::ViewSwitcherFilter { ch } => {
            if app.active_dialog() == Some(DialogKind::Switcher) {
                app.surfaces.dlg_mut::<crate::surfaces::SwitcherDialog>().query.text.push(ch);
                app.surfaces.dlg_mut::<crate::surfaces::SwitcherDialog>().query.cursor = app.surfaces.dlg_mut::<crate::surfaces::SwitcherDialog>().query.text.len();
                app.surfaces.dlg_mut::<crate::surfaces::SwitcherDialog>().selected = 0;
                app.surfaces.dlg_mut::<crate::surfaces::SwitcherDialog>().scroll = 0;
            }
        }
        input::InputAction::ViewSwitcherBackspace => {
            if app.active_dialog() == Some(DialogKind::Switcher) {
                {
                    let q = &mut app.surfaces.dlg_mut::<crate::surfaces::SwitcherDialog>().query;
                    if !q.is_empty() {
                        let start = nuotc::text::floor_grapheme_boundary(
                            &q.text,
                            q.text.len() - 1,
                        );
                        q.text.truncate(start);
                    }
                    q.cursor = q.text.len();
                }
                let d = app.surfaces.dlg_mut::<crate::surfaces::SwitcherDialog>();
                d.selected = 0;
                d.scroll = 0;
            }
        }
        input::InputAction::ViewSwitcherToggle => {
            if app.active_dialog() == Some(DialogKind::Switcher) {
                app.dismiss_surface();
            } else if app.can_open_switcher() {
                app.open_dialog(DialogKind::Switcher);
                app.surfaces.dlg_mut::<crate::surfaces::SwitcherDialog>().begin();
            }
        }
        input::InputAction::ViewSwitchActivate => {
            if app.active_dialog() != Some(DialogKind::Switcher) {
                return ActionFlow::Handled;
            }
            let is_busy = app.running_sessions.contains(viewed_session_id);
            let app_ctx = crate::keymap::AppContext {
                has_overlay: app.surfaces.active_overlay().is_some(),
                active_dialog: app
                    .surfaces
                    .underlying_dialog()
                    .or_else(|| app.active_dialog()),
                is_responding: is_busy,
                has_selection: !matches!(
                    app.selection,
                    crate::model::selection::SelectionState::None
                ),
                has_running_task: is_busy,
                queue_count: app.pending_dispatch.len(),
                has_session: app.has_session(),
                scene: app.current_scene(),
            };
            let entries = crate::overlays::command_palette::filter_palette_commands(
                &app.surfaces.dlg_mut::<crate::surfaces::SwitcherDialog>().query.text,
                &app.command_catalog,
                &app.recent_commands,
                &app_ctx,
            );
            if let Some(entry) = entries
                .get(app.surfaces.dlg_mut::<crate::surfaces::SwitcherDialog>().selected)
                .or_else(|| entries.first())
                && matches!(entry.availability, crate::keymap::Availability::Available)
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
                match entry.action.clone() {
                    crate::overlays::command_palette::PaletteAction::Client(cmd_id) => {
                        return execute_command_by_id(app, ctx, cmd_id).await;
                    }
                    crate::overlays::command_palette::PaletteAction::Harness {
                        slash,
                        requires_args,
                    } => {
                        if requires_args {
                            app.reset_to_conversation();
                            app.input = format!("{} ", slash);
                            app.set_cursor_end();
                            app.completion_dismissed = false;
                            app.suggestion_index = None;
                            return ActionFlow::Handled;
                        } else {
                            return commands::handle_send_slash(app, runtime, session, slash).await;
                        }
                    }
                }
            }
        }
        input::InputAction::ViewCloseSelected => {
            if app.active_dialog() != Some(DialogKind::Switcher) {
                return ActionFlow::Handled;
            }
        }
        input::InputAction::BtwFocusSelected => {
            // Asides modal Enter (ADR-0103 §5): jump back into the selected
            // aside. The harness replies with `SideViewOpened` carrying the
            // full transcript back-fill; the modal closes on arrival.
            if let Some(row) = app.btw_list.get(app.active_index()) {
                let side_id = row.id.clone();
                app.dismiss_active_dialog();
                app.send_intent(AgentRequest::FocusSide { side_id });
            }
        }
        input::InputAction::BtwCloseSelected => {
            // Asides modal `D` (ADR-0103 §5): close + discard the selected
            // aside (cancel its round, drop it from the list, delete its
            // session files). The modal stays open on the refreshed list.
            if let Some(row) = app.btw_list.get(app.active_index()) {
                let side_id = row.id.clone();
                app.send_intent(AgentRequest::CloseSide { side_id });
                // Optimistically drop the row so the selection does not
                // point at a stale entry before the fresh list lands; clamp
                // the cursor in case the last row was removed.
                app.btw_list.remove(app.active_index());
                if app.active_index() >= app.btw_list.len() {
                    app.set_active_index(app.btw_list.len().saturating_sub(1));
                }
            }
        }
        input::InputAction::PrevSibling => {
            app.cycle_sibling(-1);
        }
        input::InputAction::NextSibling => {
            app.cycle_sibling(1);
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
            if let Some(crate::app::RecallQueued::Restored(dispatch)) =
                app.recall_queued_at(viewed_session_id, idx)
            {
                app.restore_dispatch(dispatch);
            }
        }
        input::InputAction::QueueToggleBlock => {
            // `Ctrl+P` inside the queue modal: toggle the hard block on the
            // viewed session's outbox. ADR-0197 M4: the pause is the
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
                app.surfaces.dlg_mut::<crate::surfaces::QueueDialog>().follow = true;
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
                    app.surfaces.dlg_mut::<crate::surfaces::QueueDialog>().follow = true;
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
            if app.active_sheet() == Some(crate::sheet::SheetKind::Question)
                && let Some(qm) = app.question.take()
            {
                app.question = Some(qm.update(crate::question_model::QuestionAction::Up).0);
                // Moving the highlight re-enables follow so the body
                // scrolls to keep the cursor visible.
                app.question_modal_follow = true;
            }
        }
        input::InputAction::QuestionDown => {
            if app.active_sheet() == Some(crate::sheet::SheetKind::Question)
                && let Some(qm) = app.question.take()
            {
                app.question = Some(qm.update(crate::question_model::QuestionAction::Down).0);
                app.question_modal_follow = true;
            }
        }
        input::InputAction::QuestionToggle => {
            if app.active_sheet() == Some(crate::sheet::SheetKind::Question)
                && let Some(qm) = app.question.take()
            {
                app.question = Some(qm.update(crate::question_model::QuestionAction::Toggle).0);
            }
        }
        input::InputAction::QuestionSelect(n) => {
            if app.active_sheet() == Some(crate::sheet::SheetKind::Question)
                && let Some(qm) = app.question.take()
            {
                app.question = Some(
                    qm.update(crate::question_model::QuestionAction::Select(n))
                        .0,
                );
                // A digit jump moves the highlight, so follow it.
                app.question_modal_follow = true;
            }
        }
        input::InputAction::QuestionSubmit => {
            if app.active_sheet() == Some(crate::sheet::SheetKind::Question)
                && let Some(qm) = app.question.take()
            {
                let (qm, effects) = qm.update(crate::question_model::QuestionAction::Submit);
                // Keep the model until the per-frame queue sync clears
                // it; the Closed effect drives the channel reply + drain.
                app.question = Some(qm);
                question_effects::apply(&effects, app, runtime).await;
                app.question_scroll = 0;
                app.question_modal_follow = true;
            }
        }
        input::InputAction::QuestionPrevious => {
            if app.active_sheet() == Some(crate::sheet::SheetKind::Question)
                && let Some(qm) = app.question.take()
            {
                app.question = Some(qm.update(crate::question_model::QuestionAction::Previous).0);
                app.question_scroll = 0;
                app.question_modal_follow = true;
            }
        }
        input::InputAction::QuestionNext => {
            if app.active_sheet() == Some(crate::sheet::SheetKind::Question)
                && let Some(qm) = app.question.take()
            {
                app.question = Some(qm.update(crate::question_model::QuestionAction::Next).0);
                app.question_scroll = 0;
                app.question_modal_follow = true;
            }
        }
        input::InputAction::QuestionCancel => {
            if app.active_sheet() == Some(crate::sheet::SheetKind::Question)
                && let Some(qm) = app.question.take()
            {
                let (_qm, effects) = qm.update(crate::question_model::QuestionAction::Cancel);
                // Cancel discards the model immediately; the Closed
                // effect drives the (empty-answers) reply + drain.
                question_effects::apply(&effects, app, runtime).await;
            }
        }
        // ADR-0175: PreAttach interstitial actions. The surface owns
        // the keyboard when mounted; routing is disjoint from the
        // Question sheet so neither flow can interact with the other.
        // Quit decisions flip `should_quit` so the loop terminates on
        // the next iteration; trust decisions dispatch the canonical
        // `/trust` slash command and let the per-frame sync clear
        // `pre_attach` once the server republishes a Trusted snapshot.
        input::InputAction::PreAttachUp => {
            if let Some(pa) = app.pre_attach.as_mut() {
                let _ = pa.apply(crate::question_model::QuestionAction::Up);
            }
        }
        input::InputAction::PreAttachDown => {
            if let Some(pa) = app.pre_attach.as_mut() {
                let _ = pa.apply(crate::question_model::QuestionAction::Down);
            }
        }
        input::InputAction::PreAttachToggle => {
            if let Some(pa) = app.pre_attach.as_mut() {
                let _ = pa.apply(crate::question_model::QuestionAction::Toggle);
            }
        }
        input::InputAction::PreAttachSubmit => {
            if let Some(pa) = app.pre_attach.as_mut() {
                let decision = pa.apply(crate::question_model::QuestionAction::Submit);
                apply_pre_attach_decision(decision, app, runtime).await;
            }
        }
        input::InputAction::PreAttachCancel => {
            if let Some(pa) = app.pre_attach.as_mut() {
                let decision = pa.apply(crate::question_model::QuestionAction::Cancel);
                apply_pre_attach_decision(decision, app, runtime).await;
            }
        }
        input::InputAction::InputSubmit => {
            if app.active_sheet() == Some(crate::sheet::SheetKind::InputInjection) {
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
            if app.active_sheet() == Some(crate::sheet::SheetKind::InputInjection)
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
            if app.active_sheet() == Some(crate::sheet::SheetKind::Question)
                && let Some(qm) = app.question.take()
            {
                app.question = Some(
                    qm.update(crate::question_model::QuestionAction::InsertChar(c))
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
            if app.active_sheet() == Some(crate::sheet::SheetKind::Question)
                && let Some(qm) = app.question.take()
            {
                app.question = Some(
                    qm.update(crate::question_model::QuestionAction::Backspace)
                        .0,
                );
                // Backspace can collapse the field back up a line;
                // re-arm follow so the caret stays on screen.
                app.question_modal_follow = true;
            }
        }
        input::InputAction::PermissionPrevOption => {
            if app.active_sheet() == Some(crate::sheet::SheetKind::Permission) {
                let one_off = app.pending_permission.as_ref().is_some_and(|r| r.one_off);
                let total = crate::overlays::permission_action_count(
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
            if app.active_sheet() == Some(crate::sheet::SheetKind::Permission) {
                let one_off = app.pending_permission.as_ref().is_some_and(|r| r.one_off);
                let total = crate::overlays::permission_action_count(
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

/// The sole retained-panel entry transaction (ADR-0141: panels are
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
        crate::surfaces::DialogKind::Connections,
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
    app.surfaces.dlg_mut::<crate::surfaces::ConnectionsDialog>().info_detail = true;
    app.surfaces.dlg_mut::<crate::surfaces::ConnectionsDialog>().info_standalone = true;
    app.connection_detail = None;
    app.surfaces.dlg_mut::<crate::surfaces::ConnectionsDialog>().info_scroll = 0;
    app.surfaces.dlg_mut::<crate::surfaces::ConnectionsDialog>().models_expanded = false;
    if !target_id.is_empty() {
        app.send_intent(AgentRequest::QueryConnectionDetail {
            id: target_id,
            force_refresh: false,
        });
    }
}

pub(super) fn enter_panel(
    app: &mut App,
    id: impl Into<crate::surfaces::DialogKind>,
    runtime: &UiRuntime,
    viewed_session_id: &str,
) -> bool {
    use crate::surfaces::DialogKind;

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
                        row.provider_id == app.current_provider && row.model == app.current_model
                    })
                    .unwrap_or(0);
                let m = &mut app.surfaces.dlg_mut::<crate::surfaces::ModelsDialog>();
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
                let c = &mut app.surfaces.dlg_mut::<crate::surfaces::ConnectionsDialog>();
                c.search = false;
                c.follow = true;
                c.index = index;
                app.suggestion_index = None;
            }
            DialogKind::HistorySearch => {
                app.surfaces.dlg_mut::<crate::surfaces::HistorySearchDialog>().index = 0;
                app.surfaces.dlg_mut::<crate::surfaces::HistorySearchDialog>().scroll = 0;
                app.surfaces.dlg_mut::<crate::surfaces::HistorySearchDialog>().follow = true;
            }
            _ => {}
        }
    }

    if id == DialogKind::HistorySearch {
        app.surfaces.dlg_mut::<crate::surfaces::HistorySearchDialog>().search = true;
        app.surfaces.dlg_mut::<crate::surfaces::HistorySearchDialog>().query.cursor =
            app.surfaces.dlg_mut::<crate::surfaces::HistorySearchDialog>().query.text.len();
    }
    if id == DialogKind::Queue {
        // ADR-0197 M4: the editing-safety auto-pause is mirrored to the
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
        DialogKind::Telemetry if app.token_ledger.is_none() => {
            if app.token_report.is_none() {
                Some(AgentRequest::QueryTokenUsage {
                    session_id: viewed_session_id.to_string(),
                })
            } else {
                None
            }
        }
        DialogKind::Asides => Some(AgentRequest::QueryBtwList),
        DialogKind::Sessions => Some(AgentRequest::QuerySessionsOverview),
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

/// Enter a full-screen scene (ADR-0205): navigate the router, run the scene's
/// every-show UI refresh, and fire its data-refresh request.
pub(super) fn enter_scene(
    app: &mut App,
    scene: impl Into<crate::surfaces::SceneKind>,
    runtime: &UiRuntime,
) {
    use crate::surfaces::SceneKind;

    let scene = scene.into();
    let previous = app.current_scene();
    if previous != scene {
        app.leave_scene_for_navigation(previous);
    }
    app.switch_scene(scene);
    app.selection = SelectionState::None;
    app.focused_target = None;
    app.drag.cancel();

    let request = match scene {
        SceneKind::Settings => {
            app.config_focus = crate::overlays::ConfigFocus::Categories;
            app.config_category = 0;
            app.config_hover_index = None;
            let active_category =
                crate::overlays::ConfigCategory::from_index(app.config_category);
            if active_category == crate::overlays::ConfigCategory::Appearance {
                app.config_detail_index = Theme::color_scheme_index_with_workspace(
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
            app.host_focus = crate::overlays::DashboardFocus::Detail;
            app.host_console_log.clear();
            app.host_kill_confirm = None;
            app.host_kill_confirm_id = None;
            None
        }
        SceneKind::Conversation | SceneKind::TaskInspection | SceneKind::Aside => None,
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

pub(crate) fn handle_wheel(app: &mut App, up: bool, x: u16, y: u16) {
    use crate::ui::UiKey;
    match app.ui.scene().hit_test(x, y).copied() {
        Some(UiKey::Overlay(overlay)) if app.ui.contains(UiKey::Overlay(overlay), x, y) => {
            scroll_tick(app, !up);
        }
        Some(
            UiKey::OauthUrl
            | UiKey::OauthCode
            | UiKey::SettingsOption(_)
            | UiKey::SettingsRow(_),
        ) => scroll_tick(app, !up),
        Some(UiKey::ProviderDelete | UiKey::PreAttach) => {}
        Some(UiKey::Sheet(crate::sheet::SheetKind::Permission) | UiKey::PermissionAction(_))
            if app.permission_show_details =>
        {
            app.permission_scroll = if up {
                app.permission_scroll.saturating_sub(1)
            } else {
                app.permission_scroll
                    .saturating_add(1)
                    .min(app.permission_max_scroll)
            };
        }
        Some(UiKey::Sheet(crate::sheet::SheetKind::Question) | UiKey::QuestionOption(_)) => {
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
            | UiKey::Connection,
        ) => {
            scroll_tick(app, !up);
        }
        _ => {}
    }
}

#[cfg(test)]
mod transcript_scroll_tests {
    use super::*;

    fn scrollable_app() -> App {
        let mut app = crate::tests::new_app_for_relay_tests();
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
        app.set_active_sheet_for_test(crate::sheet::SheetKind::Permission);
        app.ui.begin(nuotc::Rect::new(0, 0, 80, 24));
        app.ui
            .mount_permission_sheet(nuotc::Rect::new(0, 15, 80, 5));
        app.ui.commit();
        app.permission_show_details = true;
        app.permission_max_scroll = 10;
        app.permission_scroll = 2;

        // 1. Wheel over permission sheet (y=16) scrolls details down
        handle_wheel(&mut app, false, 10, 16);
        assert_eq!(app.permission_scroll, 3);
        assert_eq!(app.scroll, 100, "transcript scroll untouched");

        // 2. Wheel over permission sheet (y=16) scrolls details up
        handle_wheel(&mut app, true, 10, 16);
        assert_eq!(app.permission_scroll, 2);
        assert_eq!(app.scroll, 100, "transcript scroll untouched");

        // 3. Wheel above permission sheet (y=5) scrolls transcript
        handle_wheel(&mut app, true, 10, 5);
        assert_eq!(app.scroll, 96, "transcript scrolled up");
        assert_eq!(app.permission_scroll, 2, "permission scroll untouched");
    }

    #[test]
    fn wheel_spatial_routing_under_overlay_modal_isolates_backdrop() {
        let mut app = scrollable_app();
        app.open_dialog(crate::surfaces::DialogKind::UsageStats);
        app.ui.begin(nuotc::Rect::new(0, 0, 80, 24));
        app.ui.mount(
            crate::ui::UiKey::Overlay(crate::surfaces::OverlaySurface::Dialog(
                crate::surfaces::DialogKind::UsageStats,
            )),
            nuotc::Rect::new(10, 5, 60, 10),
        );
        app.ui.commit();
        app.surfaces.dlg_mut::<crate::surfaces::UsageStatsDialog>().scroll = 5;

        // 1. Wheel on backdrop (x=2, y=2) outside modal_rect: absorbed, neither modal nor transcript scrolls
        handle_wheel(&mut app, false, 2, 2);
        assert_eq!(
            app.surfaces.dlg_mut::<crate::surfaces::UsageStatsDialog>().scroll, 5,
            "modal scroll untouched on backdrop"
        );
        assert_eq!(app.scroll, 100, "transcript scroll untouched on backdrop");

        // 2. Wheel inside modal (x=20, y=8): scrolls modal body
        handle_wheel(&mut app, false, 20, 8);
        assert_eq!(
            app.surfaces.dlg_mut::<crate::surfaces::UsageStatsDialog>().scroll, 6,
            "modal scrolled inside modal_rect"
        );
        assert_eq!(app.scroll, 100, "transcript scroll untouched");
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
        handle_wheel(&mut app, false, 5, 8);
        assert!(app.suggestion_index.is_some());

        // Click on completion item 0 accepts completion
        mouse::handle_selection_start(&mut app, &runtime, "s1", 5, 8).await;
        assert!(app.completion_dismissed);
        assert_eq!(app.suggestion_index, None);
    }
}

#[cfg(test)]
mod view_entry_tests {
    use super::*;

    #[test]
    fn every_show_refreshes_remote_view_data() {
        let mut app = crate::tests::new_app_for_relay_tests();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        app.tx = tx;
        let runtime = UiRuntime::minimal_for_test();

        assert!(enter_panel(
            &mut app,
            crate::surfaces::DialogKind::SessionTree,
            &runtime,
            "s1"
        ));
        assert!(matches!(rx.try_recv(), Ok(AgentRequest::QuerySessionTree)));
        app.dismiss_surface();
        assert!(!enter_panel(
            &mut app,
            crate::surfaces::DialogKind::SessionTree,
            &runtime,
            "s1"
        ));
        assert!(matches!(rx.try_recv(), Ok(AgentRequest::QuerySessionTree)));
    }

    #[test]
    fn sessions_and_skills_have_complete_query_paths() {
        let mut app = crate::tests::new_app_for_relay_tests();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        app.tx = tx;
        let runtime = UiRuntime::minimal_for_test();

        enter_panel(
            &mut app,
            crate::surfaces::DialogKind::Sessions,
            &runtime,
            "s1",
        );
        assert!(matches!(
            rx.try_recv(),
            Ok(AgentRequest::QuerySessionsOverview)
        ));
        enter_panel(
            &mut app,
            crate::surfaces::DialogKind::Skills,
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
    let session = crate::SessionSource::Remote {
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
    };
    let backend = nuotc::Backend::with_bce(Vec::new(), nuotc::backend::Bce::No);
    let mut terminal = nuotc::Terminal::new(backend);
    dispatch_action(app, &mut terminal, action, &mut ctx).await
}

/// Test shims for the dashboard console dispatcher: the internal helpers the
/// console tests drive directly (they need no terminal or clipboard plumbing
/// — just `App` + `UiRuntime`).
#[cfg(test)]
pub(crate) mod host_test_shims {
    use super::{UiRuntime, host};
    use crate::App;

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

/// Cycle a capability-override tri-state (ADR-0149 layer 1): unset (inherit
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
    cmd_id: crate::keymap::CommandId,
) -> ActionFlow {
    let runtime = ctx.runtime;
    let viewed_session_id = ctx.viewed_session_id;
    let copy_tx = ctx.copy_tx;
    let copy_pending = ctx.copy_pending;
    use crate::keymap::CommandId;
    match cmd_id {
        CommandId::CommandPalette => {}
        CommandId::CancelOrBack => {
            // The palette-invoked twin of the Esc chord, and therefore the
            // same scope: dismiss the overlay if one is up, else clear focus,
            // else step back one level *inside* the Scene. It never navigates
            // between Scenes (ADR-0298 §2).
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
            handle_esc_interrupt_with_runtime(
                app,
                runtime,
                commands::InterruptTarget::Primary,
            )
            .await;
        }
        CommandId::Quit => {
            app.send_intent(AgentRequest::EndSession);
            return ActionFlow::Exit;
        }
        CommandId::CopySelection => {
            if let Some(text) = extract_selection_text(
                &app.selection,
                app.focused_messages(),
                &app.input,
                &app.ui.document,
                app.drag.cell_info.as_ref(),
            ) {
                clipboard_ops::spawn_clipboard_copy(copy_tx, copy_pending.clone(), text);
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
                crate::app::ComposerSendMode::Steer => crate::app::ComposerSendMode::FollowUp,
                crate::app::ComposerSendMode::FollowUp => crate::app::ComposerSendMode::Steer,
            };
        }
        CommandId::HistorySearch => {
            enter_panel(
                app,
                crate::surfaces::DialogKind::HistorySearch,
                runtime,
                viewed_session_id,
            );
        }
        CommandId::NavigateSession => {
            enter_scene(app, crate::surfaces::SceneKind::Conversation, runtime);
        }
        CommandId::NavigateDashboard => {
            enter_scene(app, crate::surfaces::SceneKind::Dashboard, runtime);
        }
        CommandId::NavigateSettings => {
            enter_scene(app, crate::surfaces::SceneKind::Settings, runtime);
        }
        CommandId::OpenQueue => {
            enter_panel(
                app,
                crate::surfaces::DialogKind::Queue,
                runtime,
                viewed_session_id,
            );
        }
        CommandId::OpenTelemetry => {
            enter_panel(
                app,
                crate::surfaces::DialogKind::Telemetry,
                runtime,
                viewed_session_id,
            );
        }
        CommandId::OpenModels => {
            enter_panel(
                app,
                crate::surfaces::DialogKind::Models,
                runtime,
                viewed_session_id,
            );
        }
        CommandId::OpenConnections => {
            enter_panel(
                app,
                crate::surfaces::DialogKind::Connections,
                runtime,
                viewed_session_id,
            );
        }
        CommandId::OpenActiveConnectionDetail => {
            open_active_connection_detail(app, runtime, viewed_session_id);
        }
        CommandId::OpenTools => {
            enter_panel(
                app,
                crate::surfaces::DialogKind::Tools,
                runtime,
                viewed_session_id,
            );
        }
        CommandId::OpenMcp => {
            enter_panel(
                app,
                crate::surfaces::DialogKind::Mcp,
                runtime,
                viewed_session_id,
            );
        }
        CommandId::OpenSkills => {
            enter_panel(
                app,
                crate::surfaces::DialogKind::Skills,
                runtime,
                viewed_session_id,
            );
        }
        CommandId::OpenPermissions => {
            enter_panel(
                app,
                crate::surfaces::DialogKind::Permissions,
                runtime,
                viewed_session_id,
            );
        }
        CommandId::OpenUsage => {
            enter_panel(
                app,
                crate::surfaces::DialogKind::UsageStats,
                runtime,
                viewed_session_id,
            );
        }
        CommandId::OpenTree => {
            enter_panel(
                app,
                crate::surfaces::DialogKind::SessionTree,
                runtime,
                viewed_session_id,
            );
        }
        CommandId::OpenBtw => {
            enter_panel(
                app,
                crate::surfaces::DialogKind::Asides,
                runtime,
                viewed_session_id,
            );
        }
        CommandId::OpenSessions => {
            enter_panel(
                app,
                crate::surfaces::DialogKind::Sessions,
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

/// ADR-0175: apply a PreAttach surface decision.
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
/// chat surface to fall through to under ADR-0175 §4 — dismissing is
/// quitting.
async fn apply_pre_attach_decision(
    decision: Option<crate::PreAttachDecision>,
    app: &mut App,
    _runtime: &UiRuntime,
) {
    let Some(decision) = decision else {
        return;
    };
    match decision {
        crate::PreAttachDecision::Trust { domains } => {
            tracing::info!(
                ?domains,
                "nuo: PreAttach decision — granting workspace trust directly"
            );
            app.send_intent(AgentRequest::TrustWorkspace { domains });
            // The per-frame sync clears `pre_attach` once the
            // republished snapshot reports Trusted. The PreAttach surface
            // shows a `Trusting workspace...` state while awaiting the snapshot.
        }
        crate::PreAttachDecision::Quit => {
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
