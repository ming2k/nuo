//! Modal-surface handlers for the input dispatch match: the model/provider
//! editors' submit paths, the generic close-modal routing, and the shared
//! ↑/↓ modal navigation. Extracted verbatim from the corresponding arms of
//! `dispatch_action`'s match; only `SubmitModelEditor`'s arm-level `continue`
//! became an [`ActionFlow`] value (it already was, inside `dispatch_action`).
use std::sync::atomic::Ordering;

use nuo_protocol::AgentRequest;

use crate::shell::App;
use crate::shell::surfaces::{DialogKind, SceneKind, SheetKind};

use super::ActionFlow;

/// Loop stage (input dispatch): the `SubmitCustomProvider` arm.
#[allow(clippy::expect_used)] // The editor only exposes registered protocol choices.
pub(crate) fn handle_submit_custom_provider(app: &mut App) {
    if app.surfaces.contains_sheet(SheetKind::CustomProvider) {
        // Commit the focused text field's live value first.
        app.stash_custom_field();
        let name = app.custom_name.trim().to_string();
        let protocol = app
            .custom_protocol_wire
            .parse::<nuo_protocol::WireProtocol>()
            .expect("provider editor must carry a registered wire protocol");
        let base_url = app.custom_base_url.trim().to_string();
        let api_key = nuo_protocol::SecretString::from(app.custom_token.trim());
        if let Some(key) = app.custom_edit_id.clone() {
            // Edit mode: update meta (models stay managed in
            // the Models picker). A name is still required.
            // ADR-0046: effort/thinking are no longer
            // provider-level.
            if name.is_empty() {
                app.load_custom_field();
            } else {
                // The connection's identity is its `name` (INV-3).
                // A changed Name is the separate atomic `RenameConnection`
                // transaction; the metadata edit itself stays keyed by the
                // current name.
                let provider = app
                    .custom_provider_id
                    .clone()
                    .or_else(|| {
                        app.provider_picker
                            .rows
                            .iter()
                            .find(|r| r.id == key)
                            .map(|r| r.provider.clone())
                    })
                    .unwrap_or_default();
                let is_custom = provider == nuo_view::providers::CUSTOM_TEMPLATE.id;
                // Only a custom provider owns its wire/endpoint; a curated
                // provider derives both from its spec.
                let base_url = app
                    .custom_fields
                    .contains(&crate::CustomField::BaseUrl)
                    .then(|| base_url.clone())
                    .filter(|url| !url.is_empty());
                if name != key {
                    app.send_intent(AgentRequest::RenameConnection {
                        from: key.clone(),
                        to: name.clone(),
                    });
                }
                if is_custom && let Some(url) = base_url {
                    app.send_intent(AgentRequest::RegisterProvider {
                        id: provider.clone(),
                        label: Some(name.clone()),
                        root_url: url,
                        protocol: Some(protocol),
                        user_agent: None,
                        headers: None,
                        catalog_format: None,
                        dialect: None,
                    });
                }
                app.send_intent(AgentRequest::EditConnection {
                    name,
                    provider,
                    api_key,
                    headers: None,
                    budget: app.custom_budget.clone(),
                    entitlement: None,
                });
                // Phase 3: the chain ends at chat. Pop the nav
                // frame (the picker this editor was opened over) and hand
                // the composer draft back from that view's per-view slot.
                app.restore_chat_after_editor_chain();
                app.custom_field = 0;
                app.custom_edit_id = None;
            }
        } else {
            // Create mode: the model list comes from the template's
            // seeded models, or the single typed Model field when
            // the template exposes one.
            // ADR-0046: new channels start with thinking off;
            // reasoning is opted in per model from the Models
            // picker.
            let models: Vec<String> = if app.custom_fields.contains(&crate::CustomField::Model) {
                app.custom_model
                    .split(',')
                    .map(|m| m.trim().to_string())
                    .filter(|m| !m.is_empty())
                    .collect()
            } else {
                app.custom_models
                    .iter()
                    .map(|m| m.trim().to_string())
                    .collect()
            };
            let usable = models.iter().any(|m| !m.is_empty());
            if name.is_empty() || !usable {
                app.load_custom_field();
            } else {
                // The template's id IS the model provider id: the
                // connection is created against that service surface. Only the
                // `custom` provider takes a protocol/endpoint override; a
                // curated provider owns its wire. The name is sent raw and
                // trimmed — the server rejects a duplicate with a suggested
                // alternative (surfaced as `AgentResponse::Error`) instead of
                // the client silently suffixing it.
                let provider = app.custom_provider_id.take().unwrap_or_default();
                let is_custom = provider == nuo_view::providers::CUSTOM_TEMPLATE.id;
                let base_url = app
                    .custom_fields
                    .contains(&crate::CustomField::BaseUrl)
                    .then(|| base_url.clone())
                    .filter(|url| !url.is_empty());
                let provider_id = if is_custom {
                    let pid = format!("custom-{}", name.to_lowercase().replace(' ', "-"));
                    if let Some(url) = base_url {
                        app.send_intent(AgentRequest::RegisterProvider {
                            id: pid.clone(),
                            label: Some(name.clone()),
                            root_url: url,
                            protocol: Some(protocol),
                            user_agent: app.custom_user_agent.clone(),
                            headers: None,
                            catalog_format: None,
                            dialect: None,
                        });
                    }
                    pid
                } else {
                    provider
                };
                let auth = app.custom_auth.clone();
                app.send_intent(AgentRequest::AddConnection {
                    name,
                    provider: provider_id,
                    api_key,
                    models,
                    auth,
                    headers: None,
                    budget: app.custom_budget.clone(),
                    entitlement: None,
                });
                app.restore_chat_after_editor_chain();
                app.custom_field = 0;
            }
        }
    }
}

/// Loop stage (input dispatch): the `OpenModelDetail` arm.
pub(crate) fn handle_open_model_detail(app: &mut App) {
    if app.active_dialog() == Some(DialogKind::Models) {
        let dlg = app
            .surfaces
            .dlg_mut::<crate::shell::surfaces::ModelsDialog>();
        dlg.info_detail = true;
        dlg.info_scroll = 0;
        dlg.search = false;
    }
}

/// Loop stage (input dispatch): the `OpenModelEditor` arm.
pub(crate) fn handle_open_model_editor(app: &mut App) {
    if app.active_dialog() == Some(DialogKind::Models) {
        // `e` on a flat model row (from browse list or detail sub-view).
        // The per-model settings popup opens for any usable model to configure
        // effort, thinking switch, and capability overrides.
        let rows = app.models_flat_filtered();
        if let Some(row) = rows
            .get(app.active_index())
            .or_else(|| rows.iter().find(|r| r.usable))
            && row.usable
        {
            let is_builtin = !app.provider_is_custom(&row.provider_id);
            // Phase 3: the picker that opened this editor goes on
            // the navigation stack; its Esc/submit pops back to it.
            app.surfaces.present_sheet(SheetKind::ModelEditor);
            app.editor_target = Some(row.provider_id.clone());
            app.editor_model = row.model.clone();
            app.editor_model_settings_only = true;
            app.editor_target_is_builtin = is_builtin;
            app.editor_key.clear();
            // Load the stored capability overrides (layer 1) so
            // the editor shows what is already forced, if anything. The read
            // is a server round-trip now: open with the defaults
            // cleared and prefill when the `RouteSettings` answer lands.
            app.editor_vision_override = None;
            app.editor_tool_override = None;
            app.send_intent(nuo_protocol::AgentRequest::QueryRouteSettings {
                provider_id: row.provider_id.clone(),
                model: row.model.clone(),
            });
            // The rungs, the current value and the switch are all the
            // **server's resolved answer** (the snapshot row), never a
            // client-side re-derivation. This client does not link the
            // provider registry, so it has no baseline ladders and no
            // knowledge of a provider's declared default — re-deriving either
            // here is precisely how the editor once opened on a tier the wire
            // would not carry (ADR-0126).
            //
            // `row.effort` is therefore already clamped onto `row.effort_levels`
            // by the server; the open value is a plain read.
            app.editor_effort_levels = row.effort_levels.clone();
            app.editor_effort = row.effort.clone().unwrap_or_default();
            app.editor_thinking_available = row.thinking.is_some();
            app.editor_thinking = row.thinking.unwrap_or(false);
            app.editor_field = if !app.editor_effort_levels.is_empty() || row.effort.is_some() {
                1
            } else if app.editor_thinking_available {
                2
            } else {
                3
            };
            app.input = app.editor_effort.clone();
            app.set_cursor_end();
            app.surfaces
                .dlg_mut::<crate::shell::surfaces::ModelsDialog>()
                .search = false;
        }
    } else if app.active_dialog() == Some(DialogKind::Connections) {
        // `e` in the Connections list. A built-in provider opens
        // the API-key editor (only its auth changes; the model is
        // chosen from the Models picker). A user-defined provider
        // opens the full meta edit form (Name/Protocol/Base
        // URL/Token); its models stay managed in the Models
        // picker.
        let ranked = app.providers_filtered();
        let target = ranked
            .get(app.active_index())
            .or_else(|| ranked.first())
            .map(|row| (row.id.clone(), row.model.clone(), row.builtin));
        if let Some((id, model, builtin)) = target {
            if builtin {
                app.surfaces.present_sheet(SheetKind::ModelEditor);
                app.editor_target = Some(id);
                app.editor_field = 0;
                app.editor_key.clear();
                app.editor_model = model;
                app.editor_model_settings_only = false;
                app.editor_target_is_builtin = false;
                app.editor_effort = "high".to_string();
                app.editor_effort_levels.clear();
                app.editor_thinking_available = false;
                app.editor_vision_override = None;
                app.editor_tool_override = None;
                app.editor_thinking = true;
                app.input.clear();
                app.set_cursor(0);
                app.surfaces
                    .dlg_mut::<crate::shell::surfaces::ConnectionsDialog>()
                    .search = false;
            } else {
                // Pre-fill the edit form from the snapshot row.
                let row = app
                    .provider_picker
                    .rows
                    .iter()
                    .find(|r| r.id == id)
                    .cloned();
                let (name, protocol, base_url, auth, curated) = row
                    .map(|r| {
                        (
                            r.name,
                            r.protocol,
                            r.base_url,
                            r.auth,
                            r.provider != nuo_view::providers::CUSTOM_TEMPLATE.id,
                        )
                    })
                    .unwrap_or((
                        String::new(),
                        String::new(),
                        String::new(),
                        nuo_protocol::ConnectionAuth::Static,
                        true,
                    ));
                app.surfaces
                    .dlg_mut::<crate::shell::surfaces::ConnectionsDialog>()
                    .search = false;
                app.open_edit_provider_editor(
                    id,
                    name,
                    protocol,
                    base_url,
                    auth,
                    curated,
                    app.connection_detail
                        .as_ref()
                        .map(|d| d.budget.clone())
                        .unwrap_or_default(),
                );
            }
        }
    }
}

/// Loop stage (input dispatch): the `SubmitModelEditor` arm.
pub(crate) fn handle_submit_model_editor(app: &mut App) -> ActionFlow {
    if (app.surfaces.contains_sheet(SheetKind::ModelEditor) || app.editor_model_settings_only)
        && let Some(target) = app.editor_target.clone()
    {
        if let Some(payload) = target.strip_prefix("web_credential:") {
            let Some(expected_revision) =
                app.websearch_config.as_ref().map(|config| config.revision)
            else {
                return ActionFlow::NextEvent;
            };
            let mut parts = payload.splitn(2, ':');
            let axis = match parts.next() {
                Some("search") => nuo_protocol::WebProviderAxis::Search,
                Some("reader") => nuo_protocol::WebProviderAxis::Reader,
                _ => return ActionFlow::NextEvent,
            };
            let provider_id = parts.next().unwrap_or_default().to_string();
            app.send_intent(AgentRequest::UpdateWebSearchConfig(Box::new(
                nuo_protocol::WebSearchConfigUpdate {
                    expected_revision,
                    credential: Some(nuo_protocol::WebCredentialUpdate {
                        axis,
                        provider_id,
                        value: app.input.trim().to_string(),
                    }),
                    ..Default::default()
                },
            )));
            app.input.clear();
            app.set_cursor(0);
            app.editor_target = None;
            app.pop_transient_surface();
            return ActionFlow::NextEvent;
        }

        if target == "web_endpoint:searxng" {
            let Some(expected_revision) =
                app.websearch_config.as_ref().map(|config| config.revision)
            else {
                return ActionFlow::NextEvent;
            };
            app.send_intent(AgentRequest::UpdateWebSearchConfig(Box::new(
                nuo_protocol::WebSearchConfigUpdate {
                    expected_revision,
                    searxng_url: Some(app.input.trim().to_string()),
                    ..Default::default()
                },
            )));
            app.input.clear();
            app.set_cursor(0);
            app.editor_target = None;
            app.pop_transient_surface();
            return ActionFlow::NextEvent;
        }

        let id = target;
        let model = if app.editor_model.trim().is_empty() {
            app.provider_picker
                .rows
                .iter()
                .find(|r| r.id == id)
                .map(|r| r.model.clone())
                .unwrap_or_default()
        } else {
            app.editor_model.trim().to_string()
        };
        if app.editor_model_settings_only {
            // Per-model settings editor (opened from the Models
            // picker). Flush the focused field's
            // live text into its buffer before reading, so a
            // submit while effort is focused captures the value.
            // Field 2 (thinking) is a toggle with no text.
            if app.editor_field == 1 {
                app.editor_effort = app.input.clone();
            }
            let effort = app.editor_effort.clone();
            // Built-in models persist to `[model_reasoning]` (no
            // user-editable channel); user-defined models persist
            // to their channel. ADR-0045.
            if app.editor_target_is_builtin {
                app.send_intent(AgentRequest::EditModelReasoning {
                    model,
                    effort: Some(effort),
                    thinking: app.editor_thinking_available.then_some(app.editor_thinking),
                    overrides: Some(nuo_protocol::CapabilityOverrides {
                        vision: app.editor_vision_override,
                        tool_call: app.editor_tool_override,
                        ..Default::default()
                    }),
                });
            } else {
                app.send_intent(AgentRequest::EditConnectionModel {
                    connection: id,
                    model,
                    effort: Some(effort),
                    thinking: app.editor_thinking_available.then_some(app.editor_thinking),
                    overrides: Some(nuo_protocol::CapabilityOverrides {
                        vision: app.editor_vision_override,
                        tool_call: app.editor_tool_override,
                        ..Default::default()
                    }),
                });
            }
            app.input.clear();
            app.set_cursor(0);
            app.editor_target = None;
            app.editor_model_settings_only = false;
            app.editor_target_is_builtin = false;
            app.editor_thinking_available = false;
            app.editor_vision_override = None;
            app.editor_tool_override = None;
            app.set_picker_search(false);
            app.set_active_follow(true);
            if app.surfaces.contains_sheet(SheetKind::ModelEditor) {
                app.pop_transient_surface();
            }
            return ActionFlow::NextEvent;
        }
        // Key editor (not model-settings-only): this is a
        // built-in provider's API-key edit or a first-key entry.
        // ADR-0046 removed effort/thinking from the provider
        // level, so switching now carries only the key
        // (effort/thinking are set per model from the Models
        // picker `e` editor).
        let key = app.input.trim().to_string();
        app.send_intent(AgentRequest::SwitchConnection {
            provider: id,
            model,
            api_key: if key.is_empty() {
                None
            } else {
                Some(key.into())
            },
            base_url: None,
        });
        // Close to chat: the chain ends here (phase 3). Pop the
        // nav frame (the picker the editor was opened over) and hand the
        // composer draft back from that view's per-view slot.
        app.restore_chat_after_editor_chain();
        app.editor_target = None;
        app.editor_model_settings_only = false;
        app.editor_target_is_builtin = false;
    }
    ActionFlow::Handled
}

/// Loop stage (input dispatch): the `CloseModal` arm — the generic overlay
/// dismiss. **Overlay-scoped**: every branch below closes, backs out of, or
/// quits *from* an overlay. No branch leaves a Scene (
/// `[INV-TUI-CLEAN-02]`): Esc over a Scene's own chrome produces
/// [`InputAction::SceneBack`] instead, and tab closure is
/// [`InputAction::CloseTab`] (`C-x w`, ADR-0043, ADR-0077).
pub(crate) fn handle_close_modal(app: &mut App, _viewed_session_id: &str) {
    // Sub-page back-out is checked FIRST (deepest level wins),
    // so Esc from a drill-in always returns to its parent view
    // before any close/quit logic runs — otherwise pressing Esc
    // in e.g. the Sessions › Info sub-view at startup would quit
    // the program instead of dropping back to the sessions list.
    // One step back through any drill-in sub-layer (phase 4):
    // the single shared pop — Esc here and the outside-click mirror below
    // can no longer drift apart. A view with a sub-layer open stays up.
    if app.pop_sublayer() {
        // Sub-layer closed; the parent view keeps the surface.
    } else if app.startup_overlay == crate::shell::StartupOverlay::SessionsPicker
        && app.active_dialog() == Some(DialogKind::Threads)
    {
        // `nuo attach` (no id) opened the picker at startup
        // instead of loading any session: there is no real
        // thread behind the modal, so closing the *list*
        // (not a sub-view — those are handled above) must quit
        // the program rather than drop into an empty chat.
        tracing::info!(reason = "startup_picker_cancelled", "app exiting");
        app.should_quit.store(true, Ordering::SeqCst);
    } else {
        // Retained browse dialogs hide instead of closing, and the
        // quick switcher cancels back to its origin surface — both via the
        // shared dismiss verb.
        if app.dismiss_surface() {
            return;
        }
        if app.surfaces.contains_sheet(SheetKind::ModelEditor) {
            app.editor_target = None;
            app.editor_model_settings_only = false;
            app.editor_target_is_builtin = false;
            app.input.clear();
            app.set_cursor(0);
            app.set_picker_search(false);
            app.set_active_follow(true);
            app.pop_transient_surface();
        } else if app.surfaces.contains_sheet(SheetKind::CustomProvider) {
            app.cancel_custom_provider();
        } else if app.surfaces.contains_sheet(SheetKind::OAuthPending) {
            app.cancel_oauth_pending();
        } else if app.surfaces.contains_sheet(SheetKind::ProviderPreset) {
            app.cancel_preset_chooser();
        }
        // Nothing left to dismiss: the gesture is spent. The Scene beneath is
        // deliberately left exactly as it was — a dismiss never navigates
        // (A trailing `reset_to_thread` used to demote
        // the Scene here, which made every overlay-dismiss on the Dashboard or
        // Settings scene a back-door scene exit.)
    }
}

/// Loop stage (input dispatch): leaving a root scene that has no other way
/// out, when that scene was opened *standalone* at startup (`nuo dashboard`,
/// `nuo settings` with no carrier session the user asked to converse with).
///
/// This is a **program exit**, not a scene transition: with no thread
/// ever requested, "returning" to the carrier chat would trap the user in an
/// empty session (the trap startup carve-outs exist to prevent).
/// Called from the scene-exit verbs (`C-x w`, each scene's own `q`)
/// after the scene itself declined to navigate. Returns `true` when it handled
/// the exit by quiting.
pub(crate) fn quit_standalone_scene_at_startup(app: &mut App) -> bool {
    let reason = match app.startup_overlay {
        crate::shell::StartupOverlay::Dashboard if app.current_scene() == SceneKind::Dashboard => {
            "startup_dashboard_cancelled"
        }
        crate::shell::StartupOverlay::Settings { .. }
            if app.current_scene() == SceneKind::Settings =>
        {
            "startup_settings_cancelled"
        }
        _ => return false,
    };
    tracing::info!(reason, "app exiting");
    app.should_quit.store(true, Ordering::SeqCst);
    true
}

/// Loop stage (input dispatch): the `ModalUp` arm (per-modal ↑ navigation).
pub(crate) fn handle_modal_up(app: &mut App, viewed_session_id: &str) {
    if app.active_dialog().is_some() {
        if let Some(mut ent) = app.surfaces.take_active_view() {
            let _ = ent.handle_input(
                &crate::shell::input::InputAction::ModalUp,
                app,
                viewed_session_id,
            );
            app.surfaces.put_active_view(ent);
        }
    } else {
        match app.current_scene() {
            SceneKind::Dashboard => {
                if app.host_focus == crate::shell::overlays::DashboardFocus::List {
                    super::super::actions::host::cancel_kill_confirm(app);
                    let count = app.host_sessions.len();
                    app.modal_index = if count == 0 {
                        0
                    } else if app.modal_index == 0 {
                        count - 1
                    } else {
                        app.modal_index - 1
                    };
                    app.host_modal_follow = true;
                } else {
                    app.host_detail_scroll = app.host_detail_scroll.saturating_sub(1);
                }
            }
            SceneKind::Settings => match app.config_focus {
                crate::shell::overlays::ConfigFocus::Categories => {
                    let count = crate::shell::overlays::ConfigCategory::ALL.len();
                    app.config_category = (app.config_category + count - 1) % count;
                    app.config_detail_index = 0;
                    app.config_detail_scroll = 0;
                    app.config_hover_index = None;
                }
                crate::shell::overlays::ConfigFocus::Detail => {
                    let ws_path = if app.current_workspace.is_empty() {
                        None
                    } else {
                        Some(std::path::Path::new(&app.current_workspace))
                    };
                    let active_category =
                        crate::shell::overlays::ConfigCategory::from_index(app.config_category);
                    let count = active_category.detail_item_count(
                        ws_path,
                        app.websearch_config.as_ref(),
                        &app.profile,
                    );
                    if count > 0 {
                        app.config_detail_index = (app.config_detail_index + count - 1) % count;
                    }
                }
            },
            SceneKind::Thread | SceneKind::Subagent => {}
        }
    }
}

/// Loop stage (input dispatch): the `ModalDown` arm (per-modal ↓ navigation).
pub(crate) fn handle_modal_down(app: &mut App, viewed_session_id: &str) {
    if app.active_dialog().is_some() {
        if let Some(mut ent) = app.surfaces.take_active_view() {
            let _ = ent.handle_input(
                &crate::shell::input::InputAction::ModalDown,
                app,
                viewed_session_id,
            );
            app.surfaces.put_active_view(ent);
        }
    } else {
        match app.current_scene() {
            SceneKind::Dashboard => {
                if app.host_focus == crate::shell::overlays::DashboardFocus::List {
                    super::super::actions::host::cancel_kill_confirm(app);
                    let count = app.host_sessions.len().max(1);
                    app.modal_index = (app.modal_index + 1) % count;
                    app.host_modal_follow = true;
                } else {
                    app.host_detail_scroll = app.host_detail_scroll.saturating_add(1);
                }
            }
            SceneKind::Settings => match app.config_focus {
                crate::shell::overlays::ConfigFocus::Categories => {
                    let count = crate::shell::overlays::ConfigCategory::ALL.len();
                    app.config_category = (app.config_category + 1) % count;
                    app.config_detail_index = 0;
                    app.config_detail_scroll = 0;
                    app.config_hover_index = None;
                }
                crate::shell::overlays::ConfigFocus::Detail => {
                    let ws_path = if app.current_workspace.is_empty() {
                        None
                    } else {
                        Some(std::path::Path::new(&app.current_workspace))
                    };
                    let active_category =
                        crate::shell::overlays::ConfigCategory::from_index(app.config_category);
                    let count = active_category.detail_item_count(
                        ws_path,
                        app.websearch_config.as_ref(),
                        &app.profile,
                    );
                    if count > 0 {
                        app.config_detail_index = (app.config_detail_index + 1) % count;
                    }
                }
            },
            SceneKind::Thread | SceneKind::Subagent => {}
        }
    }
}

/// The reasoning depth to advertise in the composer/footer for the active
/// model, as the **server already resolved it**.
///
/// No protocol sniffing: whether depth shows at all is the provider's
/// declaration (`ProviderModelInfo::effort` is `None` for a route with no depth
/// knob), and which tier applies is the server's clamp, not this client's. The
/// old `match protocol { "anthropic" => …, _ => … }` here re-derived a rule the
/// server owns, which is how the footer once disagreed with the wire
/// (ADR-0126).
pub(crate) fn effective_reasoning_effort(app: &App) -> Option<&str> {
    app.provider_picker
        .rows
        .iter()
        .find(|row| row.id == app.current_provider)
        .and_then(|row| row.model_info.iter().find(|m| m.model == app.current_model))
        .and_then(|m| m.effort.as_deref())
}

pub(crate) fn activate_picked_model(app: &mut App, id: String, model: String, key_ready: bool) {
    if key_ready {
        app.send_intent(AgentRequest::SwitchConnection {
            provider: id,
            model,
            api_key: None,
            base_url: None,
        });
        app.dismiss_surface();
    } else if app.provider_row_auth(&id).is_negotiated() {
        let auth = app.provider_row_auth(&id);
        let method = auth
            .default_login_method()
            .unwrap_or(nuo_protocol::LoginMethod::Device);
        app.send_intent(AgentRequest::ConnectConnection { name: id, method });
        app.dismiss_surface();
    } else {
        app.surfaces.present_sheet(SheetKind::ModelEditor);
        app.editor_target = Some(id);
        app.editor_field = 0;
        app.editor_key.clear();
        app.editor_model = model;
        app.editor_model_settings_only = false;
        app.editor_target_is_builtin = false;
        app.editor_effort = "high".to_string();
        app.editor_effort_levels.clear();
        app.editor_thinking = true;
        app.input.clear();
        app.set_cursor(0);
        app.set_picker_search(false);
    }
}

pub(crate) async fn handle_permission_submit(
    app: &mut App,
    _runtime: &crate::shell::event_loop::runtime::UiRuntime,
) {
    let one_off = app.pending_permission.as_ref().is_some_and(|r| r.one_off);
    let reject_idx = if one_off { 1 } else { 2 };
    let details_idx = if one_off { 2 } else { 3 };
    if app.permission_confirm_always {
        if app.modal_index == 1 {
            app.permission_confirm_always = false;
            app.modal_index = 1;
            return;
        }
    } else {
        if app.modal_index == details_idx {
            app.permission_show_details = !app.permission_show_details;
            app.permission_scroll = 0;
            return;
        }
        if !one_off && app.modal_index == 1 {
            app.permission_confirm_always = true;
            app.permission_show_details = false;
            app.modal_index = 0;
            return;
        }
    }
    if let Some(request) = app.pending_permission.take() {
        let decision = if app.permission_confirm_always {
            nuo_protocol::PermissionDecision::Always
        } else {
            match app.modal_index {
                0 => nuo_protocol::PermissionDecision::Once,
                i if i == reject_idx => nuo_protocol::PermissionDecision::Reject,
                _ => nuo_protocol::PermissionDecision::Reject,
            }
        };
        let request_id = request.id;
        let parent_call_id = app.subagent_permission_parent.remove(&request_id);
        app.send_intent(AgentRequest::PermissionReply {
            request_id: request_id.clone(),
            decision,
            parent_call_id,
        });
        if decision == nuo_protocol::PermissionDecision::Reject {
            let queued: Vec<nuo_protocol::PermissionRequest> =
                app.pending_permissions.drain(..).collect();
            for pending in queued {
                let parent_call_id = app.subagent_permission_parent.remove(&pending.id);
                app.send_intent(AgentRequest::PermissionReply {
                    request_id: pending.id,
                    decision: nuo_protocol::PermissionDecision::Reject,
                    parent_call_id,
                });
            }
            app.pending_permission = None;
            app.pop_transient_surface();
        } else {
            app.pending_permissions.retain(|r| r.id != request_id);
            app.pending_permission = app.pending_permissions.front().cloned();
            if app.pending_permission.is_none() {
                app.pop_transient_surface();
            }
        }
        app.modal_index = 0;
        app.permission_scroll = 0;
        app.permission_max_scroll = 0;
        app.permission_confirm_always = false;
        app.permission_show_details = false;
    }
}

pub(crate) fn modal_page_step(app: &App) -> usize {
    let h = if app.modal_body_height > 0 {
        app.modal_body_height
    } else {
        app.view_height
    };
    h.saturating_sub(1).max(1) as usize
}

pub(crate) mod question_effects {
    use super::{AgentRequest, App};
    use crate::shell::event_loop::runtime::UiRuntime;
    use std::sync::atomic::Ordering;

    pub(crate) async fn apply(
        effects: &[crate::shell::question_model::QuestionEffect],
        app: &mut App,
        runtime: &UiRuntime,
    ) {
        for effect in effects {
            match effect {
                crate::shell::question_model::QuestionEffect::Reply {
                    request_id,
                    answers,
                } => {
                    // Trust gate requests no longer route
                    // through the pending_question queue (they mount
                    // the PreAttach interstitial instead). This guard
                    // is defensive: should a trust_gate request ever
                    // reach the Question sheet (legacy path, test
                    // fixture, or malformed wire), intercept it here
                    // instead of forwarding to the server — the
                    // server has no parked round waiting for a
                    // TRUST_GATE_REQUEST_ID reply.
                    if request_id == nuo_view::trust_gate::TRUST_GATE_REQUEST_ID {
                        runtime.trust_gate_dismissed.store(true, Ordering::SeqCst);
                        let selected = nuo_view::trust_gate::answer_to_domains(answers);
                        // A refusal must name what to refuse. The offered set is
                        // the authority, not the selection: "the user picked
                        // nothing" means "refuse everything that was asked
                        // about", and sending an empty list with `Deny` would
                        // record no refusal at all and let the gate re-open.
                        let offered = app.workspace_security.domains_needing_review();
                        let decision = if selected.is_empty() {
                            nuo_protocol::TrustDecision::Deny
                        } else {
                            nuo_protocol::TrustDecision::Grant
                        };
                        let domains = if selected.is_empty() {
                            offered
                        } else {
                            selected
                        };
                        app.send_intent(AgentRequest::TrustWorkspace { domains, decision });
                        continue;
                    }
                    let parent_call_id = app.subagent_question_parent.remove(request_id);
                    app.send_intent(AgentRequest::UserQuestionReply {
                        request_id: request_id.clone(),
                        answers: answers.clone(),
                        parent_call_id,
                    });
                }
                crate::shell::question_model::QuestionEffect::Cancelled { request_id } => {
                    // Defensive guard — see the Reply arm.
                    if request_id == nuo_view::trust_gate::TRUST_GATE_REQUEST_ID {
                        runtime.trust_gate_dismissed.store(true, Ordering::SeqCst);
                        continue;
                    }
                    let parent_call_id = app.subagent_question_parent.remove(request_id);
                    app.send_intent(AgentRequest::UserQuestionReply {
                        request_id: request_id.clone(),
                        answers: Vec::new(),
                        parent_call_id,
                    });
                }
                crate::shell::question_model::QuestionEffect::Closed { request_id } => {
                    app.pending_questions.retain(|r| r.id != *request_id);
                    if app.pending_questions.is_empty() {
                        app.question = None;
                        // The sheet is composer-slot state,
                        // not router foreground identity: unmount it via
                        // `dismiss_sheet`, never `pop_transient_surface`.
                        // Popping the surface router here consumed an
                        // unbalanced return frame (the sync never pushed a
                        // transient for the sheet), leaving the router's
                        // `transcript_focused`-equivalent state desynced —
                        // the composer rendered its inactive palette and
                        // never recovered.
                        if app.active_sheet() == Some(crate::shell::sheet::SheetKind::Question) {
                            app.dismiss_sheet();
                        }
                        app.modal_index = 0;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn custom_template_creates_a_custom_provider_connection() {
        // The `custom` template is the generic bring-your-own-endpoint
        // provider; the seeded provider id is the one persisted on the
        // connection, and it is NOT part of the curated chooser table.
        assert_eq!(nuo_view::providers::CUSTOM_TEMPLATE.id, "custom");
        assert!(
            nuo_view::providers::PROVIDER_PRESETS
                .iter()
                .all(|t| t.id != nuo_view::providers::CUSTOM_TEMPLATE.id),
            "custom connections have their own Connections-level branch"
        );
    }

    /// Regression (composer stuck inactive after the question sheet is
    /// answered): `Closed` must unmount the sheet as composer-slot state
    /// (`dismiss_sheet`) — never `pop_transient_surface`, which
    /// consumed an unbalanced return frame and left the composer dimmed.
    #[tokio::test]
    async fn closed_effect_unmounts_question_sheet_not_router_transient() {
        use crate::shell::question_model::{QuestionAction, QuestionEffect, QuestionModel};
        use crate::shell::sheet::SheetKind;

        let mut app = crate::shell::tests::new_app_for_relay_tests();
        let runtime = crate::shell::event_loop::runtime::UiRuntime::minimal_for_test();

        // Simulate the mounted sheet exactly as the per-frame sync leaves it:
        // slot state set, no router transient pushed on top of the session view.
        let request = {
            use nuo_protocol::{UserQuestion, UserQuestionOption, UserQuestionRequest};
            UserQuestionRequest {
                id: "q1".into(),
                questions: vec![UserQuestion {
                    header: Some("Style".into()),
                    question: "Which error handling crate?".into(),
                    options: vec![
                        UserQuestionOption {
                            label: "anyhow".into(),
                            description: None,
                        },
                        UserQuestionOption {
                            label: "eyre".into(),
                            description: None,
                        },
                    ],
                    multi_select: false,
                }],
                origin: None,
            }
        };
        app.question = Some(QuestionModel::open(request));
        app.push_sheet_surface(SheetKind::Question);

        let effects = {
            let qm = app.question.take().expect("question mounted");
            let (_qm, effects) = qm.update(QuestionAction::Submit);
            effects
        };
        assert!(effects.contains(&QuestionEffect::Closed {
            request_id: "q1".into()
        }));

        super::question_effects::apply(&effects, &mut app, &runtime).await;

        assert!(app.question.is_none(), "model dropped");
        assert_eq!(
            app.active_sheet(),
            None,
            "sheet must unmount so the composer slot is handed back"
        );
        assert_eq!(
            app.surfaces.active_overlay(),
            None,
            "the session scene must stay mounted underneath"
        );
        assert_eq!(
            app.caret_owner(),
            crate::shell::CaretOwner::Composer,
            "the composer must own the caret again after the sheet closes"
        );
    }
}
