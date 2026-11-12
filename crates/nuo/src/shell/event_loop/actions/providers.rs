//! Provider picker, OAuth setup, and model editor action dispatch.
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use tokio::sync::mpsc;

use nuo_protocol::AgentRequest;

use crate::shell::App;
use crate::shell::clipboard;
use crate::shell::clipboard_ops;
use crate::shell::input::{self, InputAction};
use crate::shell::surfaces::{DialogKind, SheetKind};

use super::super::transcript::extract_selection_text;
use super::ActionFlow;
use super::config::{cycle_tri_state, select_connection_preset};
use super::modals;

pub(super) fn handle_provider_action(
    app: &mut App,
    copy_tx: &mpsc::UnboundedSender<Result<clipboard::CopyOutcome, String>>,
    copy_pending: &Arc<AtomicUsize>,
    action: InputAction,
) -> Option<ActionFlow> {
    match action {
        InputAction::ProviderPickerActivate => {
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
                            return Some(ActionFlow::Handled);
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
                        return Some(ActionFlow::Handled);
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
                modals::activate_picked_model(app, id, model, ready);
            }
            Some(ActionFlow::Handled)
        }
        InputAction::CustomProviderNextField => {
            if app.surfaces.contains_sheet(SheetKind::CustomProvider) {
                app.cycle_custom_field(true);
            }
            Some(ActionFlow::Handled)
        }
        InputAction::CustomProviderPrevField => {
            if app.surfaces.contains_sheet(SheetKind::CustomProvider) {
                app.cycle_custom_field(false);
            }
            Some(ActionFlow::Handled)
        }
        InputAction::ScrollCustomProvider { forward } => {
            if app.surfaces.contains_sheet(SheetKind::CustomProvider) {
                app.scroll_custom_provider(forward);
            }
            Some(ActionFlow::Handled)
        }
        InputAction::CycleCustomProviderChoice { forward } => {
            if app.surfaces.contains_sheet(SheetKind::CustomProvider) {
                app.cycle_custom_choice(forward);
            }
            Some(ActionFlow::Handled)
        }
        InputAction::MovePresetChoice { forward } => {
            if app.surfaces.contains_sheet(SheetKind::ProviderPreset) {
                app.move_preset_choice(forward);
            }
            Some(ActionFlow::Handled)
        }
        InputAction::SelectPreset => {
            select_connection_preset(app, None);
            Some(ActionFlow::Handled)
        }
        InputAction::SelectPresetWithOauthMethod { method } => {
            select_connection_preset(app, Some(method));
            Some(ActionFlow::Handled)
        }
        InputAction::CancelOauthPending => {
            app.cancel_oauth_pending();
            Some(ActionFlow::Handled)
        }
        InputAction::CycleOauthSelection => {
            if app.surfaces.contains_sheet(SheetKind::OAuthPending) {
                app.cycle_oauth_selection();
            }
            Some(ActionFlow::Handled)
        }
        InputAction::CopyOauthContent { target } => {
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
                return Some(ActionFlow::Handled);
            }
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
            Some(ActionFlow::Handled)
        }
        InputAction::CancelPresetChooser => {
            app.cancel_preset_chooser();
            Some(ActionFlow::Handled)
        }
        InputAction::DeleteProvider => {
            app.stage_provider_delete();
            Some(ActionFlow::Handled)
        }
        InputAction::DeleteProviderConfirm => {
            if let Some(req) = app.confirm_provider_delete() {
                app.send_intent(req);
            }
            Some(ActionFlow::Handled)
        }
        InputAction::DeleteProviderCancel => {
            app.cancel_provider_delete();
            Some(ActionFlow::Handled)
        }
        InputAction::CancelCustomProvider => {
            app.cancel_custom_provider();
            Some(ActionFlow::Handled)
        }
        InputAction::SubmitCustomProvider => {
            modals::handle_submit_custom_provider(app);
            Some(ActionFlow::Handled)
        }
        InputAction::ModelEnterSearch => {
            if matches!(
                app.active_dialog(),
                Some(DialogKind::Connections | DialogKind::Models)
            ) {
                app.set_picker_search(true);
                app.reset_picker_nav();
            }
            Some(ActionFlow::Handled)
        }
        InputAction::ModelExitSearch => {
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
            Some(ActionFlow::Handled)
        }
        InputAction::ProviderPickerToggleFavorite => {
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
            Some(ActionFlow::Handled)
        }
        InputAction::ProviderPickerBlockModel => {
            if app.active_dialog() == Some(DialogKind::Models) {
                let ranked = app.models_flat_filtered();
                if let Some(row) = ranked.get(app.active_index()).or_else(|| ranked.first()) {
                    app.send_intent(AgentRequest::ExcludeModel {
                        scope: nuo_protocol::model::ModelTargetScope::Connection(
                            row.provider_id.clone(),
                        ),
                        model_id: row.model.clone(),
                    });
                }
            }
            Some(ActionFlow::Handled)
        }
        InputAction::OpenModelEditor => {
            modals::handle_open_model_editor(app);
            Some(ActionFlow::Handled)
        }
        InputAction::ModelEditorNextField => {
            if app.editor_model_settings_only {
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
            Some(ActionFlow::Handled)
        }
        InputAction::ModelEditorEffortCycle { delta } => {
            let levels: &[String] = &app.editor_effort_levels;
            if levels.is_empty() {
                return Some(ActionFlow::NextEvent);
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
            Some(ActionFlow::Handled)
        }
        InputAction::ModelEditorEffortJump { index } => {
            if let Some(level) = app.editor_effort_levels.get(index).cloned() {
                app.editor_effort = level;
                app.input = app.editor_effort.clone();
                app.set_cursor_end();
            }
            Some(ActionFlow::Handled)
        }
        InputAction::ModelEditorThinkingToggle => {
            if app.editor_thinking_available {
                app.editor_thinking = !app.editor_thinking;
            }
            Some(ActionFlow::Handled)
        }
        InputAction::ModelEditorVisionCycle => {
            app.editor_vision_override = cycle_tri_state(app.editor_vision_override);
            Some(ActionFlow::Handled)
        }
        InputAction::ModelEditorToolCycle => {
            app.editor_tool_override = cycle_tri_state(app.editor_tool_override);
            Some(ActionFlow::Handled)
        }
        InputAction::SubmitModelEditor => Some(modals::handle_submit_model_editor(app)),
        _ => None,
    }
}
