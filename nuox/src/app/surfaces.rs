//! Surface, Scene, and Dialog navigation under the Stage-Scene-Overlay architecture (ADR-0205).

use super::*;
use crate::surfaces::{DialogKind, DialogState, OverlaySurface, SceneKind, SheetKind};

#[allow(dead_code)]
impl App {
    /// Which interaction sheet currently occupies the composer slot, if any (ADR-0173 §3).
    pub(crate) fn active_sheet(&self) -> Option<crate::sheet::SheetKind> {
        self.active_sheet
    }

    /// Mount an interaction sheet into the composer slot, replacing the draft editor.
    pub(crate) fn push_sheet_surface(&mut self, kind: crate::sheet::SheetKind) {
        self.active_sheet = Some(kind);
    }

    /// Unmount the current sheet, handing the slot back to the draft editor.
    pub(crate) fn dismiss_sheet(&mut self) {
        self.active_sheet = None;
    }

    /// Park the transcript-focus states while an agent-driven sheet mounts.
    pub(crate) fn park_transcript_focus_for_sheet(&mut self) {
        self.focused_target = None;
        self.transcript_focused = false;
    }

    /// Exact identity of the focused dialog, if the top overlay is a dialog (ADR-0205).
    pub(crate) fn active_dialog(&self) -> Option<DialogKind> {
        self.surfaces.active_dialog()
    }

    /// The root scene the user stands in (ADR-0205: Conversation, Dashboard, Settings, etc.).
    pub(crate) fn current_scene(&self) -> SceneKind {
        self.surfaces.active_scene()
    }

    /// Navigate to a root scene.
    pub(crate) fn switch_scene(&mut self, scene: SceneKind) {
        self.surfaces.switch_scene(scene);
    }

    /// Hard reset to Conversation home scene: clear all overlays and history.
    pub(crate) fn reset_to_conversation(&mut self) {
        self.surfaces.reset_to_conversation();
    }

    /// Pop one overlay and restore the underlying surface.
    pub(crate) fn pop_transient_surface(&mut self) {
        self.surfaces.pop_overlay();
        if let Some(id) = self.active_dialog() {
            self.restore_dialog_state(id);
        }
    }

    pub(crate) fn modal_scroll_field(&mut self) -> Option<(&mut usize, Option<&mut bool>)> {
        if self.dialog_keys && self.surfaces.active_overlay().is_some() {
            return Some((&mut self.dialog_keys_scroll, None));
        }
        if self.active_sheet() == Some(crate::sheet::SheetKind::Question)
            && self.surfaces.active_overlay().is_none()
        {
            return Some((
                &mut self.question_scroll,
                Some(&mut self.question_modal_follow),
            ));
        }
        if let Some(overlay) = self.surfaces.active_overlay() {
            match overlay {
                OverlaySurface::Dialog(d) => match d {
                    DialogKind::Permissions => Some((&mut self.permissions_scroll, None)),
                    DialogKind::Telemetry => Some((&mut self.telemetry_scroll, None)),
                    DialogKind::UsageStats => Some((&mut self.usage_stats_scroll, None)),
                    DialogKind::Tools
                    | DialogKind::Mcp
                    | DialogKind::Skills
                    | DialogKind::Sessions => Some((
                        &mut self.session_scroll,
                        Some(&mut self.session_modal_follow),
                    )),
                    DialogKind::Queue => {
                        Some((&mut self.queue_scroll, Some(&mut self.queue_modal_follow)))
                    }
                    DialogKind::Asides => {
                        Some((&mut self.btw_scroll, Some(&mut self.btw_modal_follow)))
                    }
                    DialogKind::HistorySearch => Some((
                        &mut self.history_scroll,
                        Some(&mut self.history_modal_follow),
                    )),
                    DialogKind::Connections | DialogKind::Models => {
                        Some((&mut self.model_scroll, Some(&mut self.model_modal_follow)))
                    }
                    DialogKind::SessionTree => {
                        Some((&mut self.tree_scroll, Some(&mut self.tree_modal_follow)))
                    }
                    DialogKind::Switcher => Some((&mut self.command_palette_scroll, None)),
                },
                OverlaySurface::Sheet(s) => match s {
                    SheetKind::OAuthPending => Some((&mut self.oauth_scroll, None)),
                    SheetKind::ProviderPreset => Some((&mut self.preset_scroll, None)),
                    SheetKind::CustomProvider => Some((&mut self.custom_scroll, None)),
                    _ => None,
                },
            }
        } else {
            match self.current_scene() {
                SceneKind::Settings => match self.config_focus {
                    crate::overlays::ConfigFocus::Categories => {
                        Some((&mut self.config_scroll, None))
                    }
                    crate::overlays::ConfigFocus::Detail => {
                        Some((&mut self.config_detail_scroll, None))
                    }
                },
                SceneKind::Dashboard => {
                    if self.host_preview.is_some() {
                        Some((&mut self.host_preview_scroll, None))
                    } else {
                        match self.host_focus {
                            crate::overlays::DashboardFocus::List => {
                                Some((&mut self.host_scroll, Some(&mut self.host_modal_follow)))
                            }
                            crate::overlays::DashboardFocus::Detail => {
                                Some((&mut self.host_detail_scroll, None))
                            }
                        }
                    }
                }
                _ => None,
            }
        }
    }

    pub fn on_viewed_session_changed(&mut self) {
        self.history_index = None;
        self.clear_history_draft();
        if let Some(sid) = self.queue_exit_session.take() {
            self.resume_queue(&sid);
        }
        if self.input_history_persist {
            self.send_intent(nuo_contracts::AgentRequest::QueryInputHistory);
        }
        self.reset_to_conversation();
        self.surface_store.close_all();
        for id in DialogKind::ALL {
            self.reset_dialog_payload(id);
        }
        self.session_context = None;
        self.command_palette_query.clear();
        self.command_palette_selected = 0;
        self.command_palette_scroll = 0;
        self.esc_armed_until = None;
        self.input.clear();
        self.pending_images.clear();
        self.pending_text_pastes.clear();
        self.cursor_position = 0;
        self.input_scroll = 0;
        self.input_drag_scroll = None;
        self.suggestion_index = None;
        self.completion_dismissed = true;
        self.session_history_backfill.clear();
        self.session_history_backfill_cursor = 0;
    }

    pub(crate) fn can_open_switcher(&self) -> bool {
        self.can_accept_navigation_signal()
    }

    /// Whether asynchronous presentation intent may replace the foreground.
    pub(crate) fn can_accept_navigation_signal(&self) -> bool {
        let no_transient_sheet = self.surfaces.active_sheet().is_none();
        let active_dialog = self.active_dialog();
        let scene = self.current_scene();
        no_transient_sheet
            && !(scene == SceneKind::Dashboard
                && (self.host_prompting || self.host_preview.is_some()))
            && !(active_dialog == Some(DialogKind::Sessions) && self.session_info_detail)
            && !(active_dialog == Some(DialogKind::Telemetry)
                && (self.telemetry_detail || self.telemetry_turn.is_some()))
            && !(scene == SceneKind::Settings
                && (self.config_dropdown.is_some()
                    || self.config_focus == crate::overlays::ConfigFocus::Detail))
    }

    /// Test-only: put an interaction sheet in the composer slot.
    #[cfg(test)]
    pub(crate) fn set_active_sheet_for_test(&mut self, kind: crate::sheet::SheetKind) {
        self.active_sheet = Some(kind);
    }

    /// Clear all session-scoped view states on session change.
    pub(crate) fn reset_session_views(&mut self) {
        self.reset_view_state();
        self.reset_to_conversation();
        self.surface_store.close_all();
        self.focus_stack.clear();
        self.in_side_view = false;
        self.side_session_id = None;
        self.session_detail = None;
        self.session_info_detail = false;
        self.session_info_scroll = 0;
        self.session_history_backfill_cursor = 0;
    }

    /// Focus a browse dialog under the ADR-0205 lifecycle.
    pub(crate) fn open_dialog(&mut self, id: DialogKind) -> bool {
        if let Some(current) = self.active_dialog()
            && current != id
        {
            self.deactivate_dialog(current);
        }
        if id == DialogKind::HistorySearch && self.input_history_persist {
            self.send_intent(nuo_contracts::AgentRequest::QueryInputHistory);
        }
        let first = self.surface_store.open(id).is_none();
        self.surfaces.present_dialog(id);
        self.restore_dialog_state(id);
        first
    }

    /// Persist current TUI presentation preferences into `$XDG_CONFIG_HOME/mutx/config.toml`.
    pub fn save_tui_config(&self) {
        let mut cfg = crate::config::TuiConfig::load();
        cfg.color_scheme = self.color_scheme.clone();
        cfg.custom_color_scheme = self.custom_color_scheme.clone();
        cfg.click_outside_dismiss = self.click_outside_dismiss;
        cfg.expand_auto_scroll = self.expand_auto_scroll;
        cfg.transcript_layout = self.transcript_layout.as_str().to_string();
        let _ = cfg.save();
    }

    /// Snapshot the current field values of a dialog into `SurfaceStore`.
    pub(crate) fn save_dialog_state(&mut self, id: DialogKind) {
        let scroll = self.dialog_scroll(id);
        let follow = self.dialog_follow(id);
        let draft = self.surface_store.state(&id).and_then(|s| s.draft.clone());
        let query = if self.owns_composer_draft(id) {
            self.input.clone()
        } else {
            self.surface_store
                .state(&id)
                .map(|state| state.query.clone())
                .unwrap_or_default()
        };
        let query_active = match id {
            DialogKind::Models | DialogKind::Connections => self.model_search,
            DialogKind::HistorySearch => self.history_search,
            _ => false,
        };
        self.surface_store.save(
            id,
            DialogState {
                index: self.modal_index,
                scroll,
                follow,
                draft,
                query,
                query_active,
            },
        );
    }

    /// Restore the live fields projected by a retained dialog.
    pub(crate) fn restore_dialog_state(&mut self, id: DialogKind) {
        let state = self.surface_store.state(&id).cloned().unwrap_or_default();
        self.modal_index = state.index;
        self.apply_dialog_scroll(id, state.scroll);
        self.apply_dialog_follow(id, state.follow);
        if self.owns_composer_draft(id) {
            if state.draft.is_none() {
                self.park_draft_into(id);
            }
            self.input = state.query;
            self.set_cursor_end();
            self.input_scroll = 0;
            self.input_drag_scroll = None;
            self.suggestion_index = None;
            match id {
                DialogKind::Models | DialogKind::Connections => {
                    self.model_search = state.query_active;
                }
                DialogKind::HistorySearch => {
                    self.history_search = state.query_active;
                }
                _ => {}
            }
        }
    }

    /// Exit hook for a root scene.
    pub(crate) fn leave_scene_for_navigation(&mut self, scene: SceneKind) {
        self.deactivate_scene(scene)
    }

    pub(crate) fn deactivate_scene(&mut self, scene: SceneKind) {
        match scene {
            SceneKind::Dashboard => {
                self.host_prompting = false;
                self.host_prompt_new = false;
                self.host_preview = None;
                self.host_preview_scroll = 0;
            }
            SceneKind::Settings => {
                self.config_dropdown = None;
            }
            SceneKind::Conversation | SceneKind::TaskInspection | SceneKind::Aside => {}
        }
    }

    /// Run the exit hook for one exact dialog.
    pub(crate) fn deactivate_dialog(&mut self, id: DialogKind) {
        self.dialog_keys = false;
        self.dialog_keys_scroll = 0;
        self.save_dialog_state(id);
        if self.owns_composer_draft(id) {
            self.restore_draft_from(id);
            if id == DialogKind::HistorySearch {
                self.history_search = false;
            } else {
                self.model_search = false;
            }
        }
        if id == DialogKind::Sessions {
            self.sessions_loading = false;
            self.session_info_detail = false;
            self.session_detail = None;
            self.session_info_scroll = 0;
        }
        if id == DialogKind::Connections {
            self.connection_info_detail = false;
            self.connection_info_standalone = false;
            self.connection_detail = None;
            self.connection_info_scroll = 0;
            self.connection_models_expanded = false;
        }
        if id == DialogKind::Telemetry {
            self.telemetry_tab = crate::overlays::telemetry::TelemetryTab::Overview;
            self.telemetry_detail = false;
            self.telemetry_turn = None;
            self.telemetry_turn_cursor = 0;
        }
        if id == DialogKind::Queue
            && let Some(sid) = self.queue_exit_session.take()
        {
            self.resume_queue(&sid);
        }
    }

    /// Dismiss the active dialog overlay, restoring the dialog underneath (if
    /// any). Strictly **overlay-scoped**: it never leaves the Scene. A Scene's
    /// exit is `close_scene`, a deliberate verb — never the universal dismiss
    /// chord (ADR-0205 `[INV-TUI-CLEAN-02]`, ADR-0298 §2).
    pub(crate) fn dismiss_active_dialog(&mut self) -> bool {
        if let Some(id) = self.active_dialog() {
            self.deactivate_dialog(id);
            self.surfaces.pop_overlay();
            if let Some(underlying) = self.active_dialog() {
                self.restore_dialog_state(underlying);
            }
            true
        } else {
            false
        }
    }

    /// Leave a root scene back to the scene it came from (ADR-0205 lifecycle:
    /// the scene history the router keeps for TaskInspection and Aside, and a
    /// direct return to Conversation for the peer scenes). Returns `false`
    /// when already on the Conversation scene.
    ///
    /// This is the *only* scene-leaving path besides the aside/subagent exits
    /// in [`App::close_scene`]; nothing on the Esc chord calls it.
    pub(crate) fn leave_scene(&mut self) -> bool {
        let leaving = self.current_scene();
        if leaving == SceneKind::Conversation {
            return false;
        }
        self.surfaces.back_scene();
        if leaving == SceneKind::TaskInspection {
            self.focus_stack.clear();
            self.reset_view_state();
        }
        if leaving == SceneKind::Aside {
            self.in_side_view = false;
            self.side_session_id = None;
            self.reset_view_state();
        }
        self.deactivate_scene(leaving);
        true
    }

    /// Actively leave the current Scene: the `C-x` scene namespace's exit
    /// (`C-x w` / `C-x k`, ADR-0298 §1) and each scene's own `q`.
    ///
    /// Overlay-agnostic by design — an overlay floating above the scene is the
    /// visual foreground, so the dispatcher dismisses it *first* and the scene
    /// is left on the next press. Returns `true` when a scene was actually
    /// left (i.e. the view was not already the home Conversation).
    pub(crate) fn close_scene(&mut self) -> bool {
        self.scene_namespace_armed = false;
        match self.current_scene() {
            SceneKind::Aside => {
                // `/btw`: detach from the aside view and return to the primary
                // transcript (ADR-0103). Detach is non-destructive — the aside
                // keeps running. The interrupt arm is cleared so the main
                // view's next Esc starts a fresh confirmation.
                self.exit_side_view();
                self.arm_esc(None);
                self.send_intent(nuo_contracts::AgentRequest::ExitSideView);
                true
            }
            SceneKind::TaskInspection => {
                if self.exit_subagent() {
                    true
                } else {
                    self.leave_scene()
                }
            }
            SceneKind::Dashboard | SceneKind::Settings => self.leave_scene(),
            SceneKind::Conversation => false,
        }
    }

    /// Step back one level **inside** the active Scene: a dropdown, a drill-in
    /// pane, the dashboard's preview or inline prompt. Esc produces this on the
    /// Dashboard and Settings scenes (ADR-0298 §2). It is scene-local by
    /// construction — there is no arm that leaves the Scene, so Esc can never
    /// navigate between scenes. The Conversation, TaskInspection and Aside
    /// scenes own their Esc in their own schemes and never reach this.
    pub(crate) fn scene_back(&mut self) {
        if self.current_scene() == SceneKind::Settings {
            if self.config_dropdown.is_some() {
                self.config_dropdown = None;
                return;
            }
            if self.config_focus == crate::overlays::ConfigFocus::Detail {
                if self.config_category == 0 {
                    // Leaving the theme pane reverts the live preview to the
                    // persisted color scheme.
                    let ws_path = if self.current_workspace.is_empty() {
                        None
                    } else {
                        Some(std::path::Path::new(&self.current_workspace))
                    };
                    self.theme = Theme::from_color_scheme_with_workspace(
                        &self.color_scheme,
                        &self.custom_color_scheme,
                        ws_path,
                    );
                    self.config_detail_index =
                        Theme::color_scheme_index_with_workspace(&self.color_scheme, ws_path);
                }
                self.config_focus = crate::overlays::ConfigFocus::Categories;
            }
            return;
        }
        if self.current_scene() == SceneKind::Dashboard {
            if self.host_preview.is_some() {
                self.host_preview = None;
                self.host_preview_scroll = 0;
                return;
            }
            if self.host_prompting {
                self.host_prompting = false;
                self.host_prompt_new = false;
                self.input.clear();
                self.set_cursor(0);
            }
        }
    }

    /// Explicitly close a retained dialog, dropping both its state and UI payload.
    pub(crate) fn close_dialog(&mut self, id: DialogKind) {
        if self.active_dialog() == Some(id) {
            self.deactivate_dialog(id);
            self.surfaces.pop_overlay();
        }
        self.surface_store.close(id);
        self.reset_dialog_payload(id);
    }

    fn reset_dialog_payload(&mut self, id: DialogKind) {
        match id {
            DialogKind::Tools | DialogKind::Mcp => {
                self.session_scroll = 0;
                self.session_modal_follow = true;
            }
            DialogKind::Skills => {
                self.session_scroll = 0;
                self.session_modal_follow = true;
                self.skills_expanded = None;
            }
            DialogKind::Permissions => self.permissions_scroll = 0,
            DialogKind::UsageStats => {
                self.usage_stats_scroll = 0;
            }
            DialogKind::Telemetry => {
                self.telemetry_tab = crate::overlays::telemetry::TelemetryTab::Overview;
                self.telemetry_scroll = 0;
                self.telemetry_detail = false;
                self.telemetry_turn = None;
                self.telemetry_turn_cursor = 0;
            }
            DialogKind::Asides => {
                self.btw_list.clear();
                self.btw_scroll = 0;
                self.btw_modal_follow = true;
            }
            DialogKind::Models | DialogKind::Connections => {
                self.model_search = false;
                self.model_scroll = 0;
                self.model_modal_follow = true;
                self.models_refreshing = false;
            }
            DialogKind::HistorySearch => {
                self.history_search = false;
            }
            DialogKind::Queue => {
                self.queue_scroll = 0;
                self.queue_modal_follow = true;
            }
            DialogKind::Sessions => {
                self.sessions_loading = true;
                self.session_info_detail = false;
                self.session_detail = None;
                self.session_info_scroll = 0;
            }
            DialogKind::SessionTree => {
                self.session_tree = nuo_contracts::SessionTree::default();
                self.tree_scroll = 0;
                self.tree_modal_follow = true;
            }
            DialogKind::Switcher => {
                self.command_palette_query.clear();
                self.command_palette_selected = 0;
                self.command_palette_scroll = 0;
            }
        }
    }

    /// Pop the deepest sub-layer of a view or dialog.
    pub(crate) fn pop_sublayer(&mut self) -> bool {
        if self.dialog_keys {
            self.dialog_keys = false;
            self.dialog_keys_scroll = 0;
            return true;
        }
        if self.current_scene() == SceneKind::Settings {
            if self.config_dropdown.is_some() {
                self.config_dropdown = None;
                return true;
            }
            if self.config_focus == crate::overlays::ConfigFocus::Detail {
                self.config_focus = crate::overlays::ConfigFocus::Categories;
                return true;
            }
        }
        if self.current_scene() == SceneKind::Dashboard {
            if self.host_preview.is_some() {
                self.host_preview = None;
                self.host_preview_scroll = 0;
                return true;
            }
            if self.host_prompting {
                self.host_prompting = false;
                self.host_prompt_new = false;
                self.input.clear();
                self.set_cursor(0);
                return true;
            }
        }
        if let Some(dialog) = self.active_dialog() {
            match dialog {
                DialogKind::Telemetry => {
                    if self.telemetry_turn.is_some() {
                        self.telemetry_turn = None;
                        self.telemetry_scroll = 0;
                        return true;
                    }
                    if self.telemetry_detail {
                        self.telemetry_detail = false;
                        self.telemetry_turn_cursor = 0;
                        self.telemetry_scroll = 0;
                        return true;
                    }
                }
                DialogKind::Sessions if self.session_info_detail => {
                    self.session_info_detail = false;
                    self.session_detail = None;
                    self.session_info_scroll = 0;
                    return true;
                }
                DialogKind::Connections if self.connection_info_detail => {
                    let standalone = self.connection_info_standalone;
                    self.connection_info_detail = false;
                    self.connection_info_standalone = false;
                    self.connection_detail = None;
                    self.connection_info_scroll = 0;
                    self.connection_models_expanded = false;
                    return !standalone;
                }
                _ => {}
            }
        }
        false
    }

    /// The dispatcher-facing dismiss verb.
    pub(crate) fn dismiss_surface(&mut self) -> bool {
        if self.active_dialog() == Some(DialogKind::Switcher) {
            self.pop_transient_surface();
            return true;
        }
        self.dismiss_active_dialog()
    }

    fn dialog_scroll(&self, id: DialogKind) -> usize {
        match id {
            DialogKind::Tools | DialogKind::Mcp | DialogKind::Skills => self.session_scroll,
            DialogKind::Permissions => self.permissions_scroll,
            DialogKind::UsageStats => self.usage_stats_scroll,
            DialogKind::Telemetry => self.telemetry_scroll,
            DialogKind::Asides => self.btw_scroll,
            DialogKind::HistorySearch => self.history_scroll,
            DialogKind::Models | DialogKind::Connections => self.model_scroll,
            DialogKind::Queue => self.queue_scroll,
            DialogKind::Sessions => self.session_scroll,
            DialogKind::SessionTree => self.tree_scroll,
            DialogKind::Switcher => self.command_palette_scroll,
        }
    }

    fn apply_dialog_scroll(&mut self, id: DialogKind, scroll: usize) {
        match id {
            DialogKind::Tools | DialogKind::Mcp | DialogKind::Skills => {
                self.session_scroll = scroll;
            }
            DialogKind::Permissions => self.permissions_scroll = scroll,
            DialogKind::UsageStats => self.usage_stats_scroll = scroll,
            DialogKind::Telemetry => self.telemetry_scroll = scroll,
            DialogKind::Asides => self.btw_scroll = scroll,
            DialogKind::HistorySearch => self.history_scroll = scroll,
            DialogKind::Models | DialogKind::Connections => {
                self.model_scroll = scroll;
            }
            DialogKind::Queue => self.queue_scroll = scroll,
            DialogKind::Sessions => self.session_scroll = scroll,
            DialogKind::SessionTree => self.tree_scroll = scroll,
            DialogKind::Switcher => self.command_palette_scroll = scroll,
        }
    }

    fn dialog_follow(&self, id: DialogKind) -> bool {
        match id {
            DialogKind::Tools | DialogKind::Mcp | DialogKind::Skills => self.session_modal_follow,
            DialogKind::Asides => self.btw_modal_follow,
            DialogKind::HistorySearch => self.history_modal_follow,
            DialogKind::Models | DialogKind::Connections => self.model_modal_follow,
            DialogKind::Queue => self.queue_modal_follow,
            DialogKind::Sessions => self.session_modal_follow,
            DialogKind::SessionTree => self.tree_modal_follow,
            _ => true,
        }
    }

    fn apply_dialog_follow(&mut self, id: DialogKind, follow: bool) {
        match id {
            DialogKind::Tools | DialogKind::Mcp | DialogKind::Skills | DialogKind::Sessions => {
                self.session_modal_follow = follow
            }
            DialogKind::Asides => self.btw_modal_follow = follow,
            DialogKind::HistorySearch => self.history_modal_follow = follow,
            DialogKind::Models | DialogKind::Connections => {
                self.model_modal_follow = follow;
            }
            DialogKind::Queue => self.queue_modal_follow = follow,
            DialogKind::SessionTree => self.tree_modal_follow = follow,
            _ => {}
        }
    }

    pub(crate) fn reset_view_state(&mut self) {
        self.scroll = 0;
        self.follow_bottom = true;
        self.selection = SelectionState::None;
        self.drag.cancel();
        self.sticky_step = None;
        self.sticky_summary_line = None;
        self.pin_summary_line = None;
        self.scroll_settle_pending = false;
        self.focused_target = None;
    }

    pub fn viewed_chrome(&self) -> SessionChrome {
        if self.in_side_view
            && let Some(side_id) = self.side_session_id.as_deref()
            && let Some(chrome) = self.session_chrome.get(side_id)
        {
            return chrome.clone();
        }
        SessionChrome {
            phase: self.phase.clone(),
            responding: self.round_started_at.is_some() || self.phase.is_some(),
            round_count: self.round_count,
            current_turn: self.current_turn,
            round_started_at: self.round_started_at,
            can_retry: self.loop_status.is_idle() && self.harness_retry_pending,
            last_turn_performance: self
                .session_chrome
                .get(&self.current_session_id)
                .and_then(|chrome| chrome.last_turn_performance),
            // The primary's own setback slot. Unlike `phase` this mirror is
            // never swapped into the App fields by `apply_chrome`/the parked
            // primary chrome: the primary view reads it directly, the aside
            // view reads the aside's entry, and a view change therefore has
            // nothing to restore (ADR-0235).
            transport_setback: self.provider_retry.clone(),
        }
    }

    /// Write the primary session's activity phase, retiring its
    /// transport-setback clause by the same rule as
    /// [`SessionChrome::set_phase`] — this is the primary's half of the single
    /// clause lifetime (ADR-0235).
    pub fn set_phase(&mut self, phase: Option<crate::phase::Phase>) {
        if crate::phase::ends_transport_setback(phase.as_ref()) {
            self.provider_retry = None;
        }
        self.phase = phase;
    }

    /// Whether any session currently carries a live transport setback. The
    /// event loop's animation predicate and the renderer both read this, so a
    /// countdown nobody is looking at still ticks, and a retired one stops
    /// costing frames.
    pub fn has_live_transport_setback(&self) -> bool {
        self.provider_retry.is_some()
            || self
                .session_chrome
                .values()
                .any(|chrome| chrome.transport_setback.is_some())
    }

    /// Copy a viewed session's chrome into the App-level mirrors. Deliberately
    /// partial: it carries the display slots a view swap must restore
    /// (`phase`, counters, timer origin) and *not* the setback clause, which is
    /// parked by construction — the primary keeps its own in
    /// [`App::provider_retry`] while an aside is on screen (ADR-0235).
    pub(super) fn apply_chrome(&mut self, chrome: &SessionChrome) {
        self.phase = chrome.phase.clone();
        self.round_started_at = chrome.round_started_at;
        self.round_count = chrome.round_count;
        self.current_turn = chrome.current_turn;
    }

    pub fn clear_responding(&mut self) {
        self.set_phase(None);
        self.round_started_at = None;
        self.loop_status = nuo_contracts::LoopStatus::Idle;
        if self.in_side_view {
            if let Some(side_id) = self.side_session_id.as_deref()
                && let Some(chrome) = self.session_chrome.get_mut(side_id)
            {
                chrome.responding = false;
                chrome.set_phase(None);
                chrome.round_started_at = None;
            }
        } else if let Some(chrome) = self.session_chrome.get_mut(&self.current_session_id) {
            chrome.responding = false;
            chrome.set_phase(None);
            chrome.round_started_at = None;
        }
    }
}
