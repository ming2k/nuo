//! Configuration, presets and settings handlers.
use crate::shell::App;
use crate::shell::event_loop::actions::navigation::enter_panel;
use crate::shell::event_loop::runtime::UiRuntime;
use crate::shell::event_loop::sync::show_local_toast;
use crate::shell::surfaces::{SceneKind, SheetKind};
use nuo_protocol::AgentRequest;

pub(crate) fn select_connection_preset(
    app: &mut App,
    forced_method: Option<nuo_protocol::LoginMethod>,
) {
    if !app
        .surfaces
        .surfaces
        .contains_sheet(SheetKind::ProviderPreset)
    {
        return;
    }
    let Some(preset) = crate::PROVIDER_PRESETS.get(app.provider_editor.preset_choice) else {
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
                nuo_protocol::LoginMethod::Browser => {
                    "Browser PKCE login is not supported by this connection."
                }
                nuo_protocol::LoginMethod::Device => {
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
    let target_id = if !app.session.current_provider.is_empty() {
        app.session.current_provider.clone()
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
        .surfaces
        .dlg_mut::<crate::shell::surfaces::ConnectionsDialog>()
        .info_detail = true;
    app.surfaces
        .surfaces
        .dlg_mut::<crate::shell::surfaces::ConnectionsDialog>()
        .info_standalone = true;
    app.surfaces.connection_detail = None;
    app.surfaces
        .surfaces
        .dlg_mut::<crate::shell::surfaces::ConnectionsDialog>()
        .info_scroll = 0;
    if !target_id.is_empty() {
        app.send_intent(AgentRequest::QueryConnectionDetail {
            id: target_id,
            force_refresh: false,
        });
    }
}

pub(crate) fn handle_config_activate(app: &mut App) {
    if app.current_scene() != SceneKind::Settings {
        return;
    }
    let ws_path = if app.session.current_workspace.is_empty() {
        None
    } else {
        Some(std::path::Path::new(&app.session.current_workspace))
    };
    let active_category =
        crate::shell::overlays::ConfigCategory::from_index(app.surfaces.config_category);
    match app.surfaces.config_focus {
        crate::shell::overlays::ConfigFocus::Categories => {
            app.surfaces.config_focus = crate::shell::overlays::ConfigFocus::Detail;
            if active_category == crate::shell::overlays::ConfigCategory::Appearance {
                app.surfaces.config_detail_index =
                    crate::shell::config::color_scheme_index(&app.surfaces.color_scheme, ws_path);
            } else {
                app.surfaces.config_detail_index = 0;
            }
        }
        crate::shell::overlays::ConfigFocus::Detail => match active_category {
            crate::shell::overlays::ConfigCategory::Appearance => {
                if app.surfaces.profile.supports_color_themes() {
                    let schemes = crate::shell::config::available_color_schemes(ws_path);
                    let sel_idx = app.surfaces.config_detail_index % schemes.len().max(1);
                    if let Some(scheme) = schemes.get(sel_idx) {
                        let name = &scheme.id;
                        app.surfaces.color_scheme = name.to_string();
                        app.surfaces.theme = crate::shell::config::resolve_theme_with_profile(
                            name.as_ref(),
                            &app.surfaces.custom_color_scheme,
                            ws_path,
                            &app.surfaces.profile,
                        );
                        app.send_intent(AgentRequest::UpdateTuiColorScheme {
                            name: app.surfaces.color_scheme.clone(),
                            custom: serde_json::to_value(&app.surfaces.custom_color_scheme)
                                .unwrap_or_default(),
                        });
                        app.save_tui_config();
                    }
                }
            }
            crate::shell::overlays::ConfigCategory::Components => {
                use crate::shell::views::settings::components::{ComponentRowId, row_for_index};
                match row_for_index(app.surfaces.config_detail_index) {
                    Some(ComponentRowId::Reasoning) => {
                        let next = !crate::shell::config::reasoning_default_expanded(
                            &app.session.tui_config,
                        );
                        if next {
                            app.session
                                .tui_config
                                .default_expanded
                                .insert(crate::shell::config::THINKING_KEY.to_string(), true);
                        } else {
                            app.session
                                .tui_config
                                .default_expanded
                                .remove(crate::shell::config::THINKING_KEY);
                        }
                        app.transcript.reasoning_default_expanded = next;
                        app.save_tui_config();
                    }
                    Some(ComponentRowId::Tool(component)) => {
                        let next = !crate::shell::config::tool_default_expanded(
                            &app.session.tui_config,
                            component.primary_name(),
                        );
                        crate::shell::config::set_component_default_expanded(
                            &mut app.session.tui_config,
                            component,
                            next,
                        );
                        app.save_tui_config();
                    }
                    Some(ComponentRowId::AutoScroll) => {
                        app.transcript.expand_auto_scroll = !app.transcript.expand_auto_scroll;
                        app.session.tui_config.expand_auto_scroll =
                            app.transcript.expand_auto_scroll;
                        app.save_tui_config();
                    }
                    None => {}
                }
            }
            crate::shell::overlays::ConfigCategory::WebSearch
            | crate::shell::overlays::ConfigCategory::WebReader => {
                let Some(revision) = app
                    .surfaces
                    .websearch_config
                    .as_ref()
                    .map(|config| config.revision)
                else {
                    return;
                };
                let is_search =
                    active_category == crate::shell::overlays::ConfigCategory::WebSearch;
                match app.surfaces.config_detail_index {
                    0 => {
                        let anchor = if let Some(target_rect) = app.surfaces.config_selected_rect {
                            nuo_ui_primitives::components::dropdown::DropdownAnchor::anchored(
                                target_rect,
                                nuo_ui_primitives::components::dropdown::DropdownPlacement::Auto,
                            )
                        } else {
                            nuo_ui_primitives::components::dropdown::DropdownAnchor::center_screen()
                        };
                        if is_search {
                            let current = app
                                .surfaces
                                .websearch_config
                                .as_ref()
                                .map(|ws| ws.provider.as_str())
                                .unwrap_or("exa");
                            let dropdown =
                                crate::shell::views::settings::build_websearch_provider_dropdown(
                                    current,
                                    app.surfaces.websearch_config.as_ref(),
                                );
                            app.surfaces.config_dropdown = Some((dropdown, anchor));
                        } else {
                            let current = app
                                .surfaces
                                .websearch_config
                                .as_ref()
                                .map(|ws| ws.reader.as_str())
                                .unwrap_or("disabled");
                            let dropdown =
                                crate::shell::views::settings::build_websearch_reader_dropdown(
                                    current,
                                    app.surfaces.websearch_config.as_ref(),
                                );
                            app.surfaces.config_dropdown = Some((dropdown, anchor));
                        }
                    }
                    1 => {
                        let current = app
                            .surfaces
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
                            nuo_protocol::WebSearchConfigUpdate {
                                expected_revision: revision,
                                timeout_secs: Some(next),
                                ..Default::default()
                            },
                        )));
                    }
                    2 => {
                        let editor = app.surfaces.websearch_config.as_ref().and_then(|ws| {
                            let (axis, id) = if is_search {
                                ("search", ws.provider.id())
                            } else {
                                ("reader", ws.reader.id())
                            };
                            let capability = ws.capabilities.iter().find(|capability| {
                                capability.id == id
                                    && capability.axis
                                        == if is_search {
                                            nuo_protocol::WebProviderAxis::Search
                                        } else {
                                            nuo_protocol::WebProviderAxis::Reader
                                        }
                            })?;
                            let endpoint = capability.endpoint
                                == nuo_protocol::WebEndpointRequirement::UserSupplied;
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
                            app.surfaces.surfaces.present_sheet(SheetKind::ModelEditor);
                            app.provider_editor.editor_target = Some(target);
                            app.provider_editor.editor_model = display_name;
                            app.provider_editor.editor_key.clear();
                            app.provider_editor.editor_field = 0;
                            app.composer.input = initial;
                            app.set_cursor(app.composer.input.len());
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        },
    }
}

pub(crate) fn cycle_tri_state(v: Option<bool>) -> Option<bool> {
    match v {
        None => Some(true),
        Some(true) => Some(false),
        Some(false) => None,
    }
}
