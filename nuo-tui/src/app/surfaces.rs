//! Surface, Scene, and Dialog navigation under the Stage-Scene-Overlay
//! architecture (ADR-0035).
//!
//! Dialog state lives in the encapsulated entities owned by
//! `SurfaceRouter::dialogs`; this module owns the App-side lifecycle glue:
//! opening (with precondition gating), dismissal, scene/session unwinding, and
//! the wheel-routing scroll resolution.

use super::*;
use crate::surfaces::{DialogKind, OverlaySurface, SceneKind, SheetKind};

#[allow(dead_code)]
impl App {
    /// Whether the ambient session context is present for session-scoped
    /// dialogs (`[INV-SURFACE-03]`).
    pub(crate) fn has_session(&self) -> bool {
        !self.current_session_id.is_empty()
    }

    /// Whether `id` may be opened in the current environment.
    pub(crate) fn dialog_available(&self, id: DialogKind) -> bool {
        id.is_available(self.current_scene(), self.has_session())
    }

    /// Which interaction sheet currently occupies the composer slot, if any.
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

    /// Exact identity of the focused dialog, if the top overlay is a dialog.
    pub(crate) fn active_dialog(&self) -> Option<DialogKind> {
        self.surfaces.active_dialog()
    }

    /// The root scene the user stands in.
    pub(crate) fn current_scene(&self) -> SceneKind {
        self.surfaces.active_scene()
    }

    /// The topmost dialog in the overlay stack, even when a sheet floats
    /// above it (the picker-under-editor case).
    pub(crate) fn top_dialog(&self) -> Option<DialogKind> {
        self.surfaces.top_dialog()
    }

    /// The active dialog's selection cursor.
    pub(crate) fn active_index(&self) -> usize {
        self.top_dialog()
            .map_or(0, |id| self.surfaces.nav_index(id))
    }

    /// Set the active dialog's selection cursor.
    pub(crate) fn set_active_index(&mut self, value: usize) {
        if let Some(id) = self.top_dialog() {
            self.surfaces.set_nav_index(id, value);
        }
    }

    /// Rotate the active dialog's selection cursor over `count` rows.
    pub(crate) fn rotate_active_index(&mut self, count: usize, forward: bool) {
        if count == 0 {
            self.set_active_index(0);
            return;
        }
        let cur = self.active_index();
        let next = if forward {
            (cur + 1) % count
        } else if cur == 0 {
            count - 1
        } else {
            cur - 1
        };
        self.set_active_index(next);
    }

    /// Whether the topmost model/provider picker is in search mode.
    pub(crate) fn picker_search(&self) -> bool {
        match self.top_dialog() {
            Some(DialogKind::Models) => self.surfaces.dlg::<crate::surfaces::ModelsDialog>().search,
            Some(DialogKind::Connections) => self.surfaces.dlg::<crate::surfaces::ConnectionsDialog>().search,
            _ => false,
        }
    }

    /// Toggle the topmost model/provider picker's search mode.
    pub(crate) fn set_picker_search(&mut self, value: bool) {
        match self.top_dialog() {
            Some(DialogKind::Models) => self.surfaces.dlg_mut::<crate::surfaces::ModelsDialog>().search = value,
            Some(DialogKind::Connections) => self.surfaces.dlg_mut::<crate::surfaces::ConnectionsDialog>().search = value,
            _ => {}
        }
    }

    /// Reset the topmost picker's cursor, scroll, and follow to first-open.
    pub(crate) fn reset_picker_nav(&mut self) {
        match self.top_dialog() {
            Some(DialogKind::Models) => {
                let m = &mut self.surfaces.dlg_mut::<crate::surfaces::ModelsDialog>();
                m.index = 0;
                m.scroll = 0;
                m.follow = true;
            }
            Some(DialogKind::Connections) => {
                let c = &mut self.surfaces.dlg_mut::<crate::surfaces::ConnectionsDialog>();
                c.index = 0;
                c.scroll = 0;
                c.follow = true;
            }
            _ => {}
        }
    }

    /// Set the active dialog's body-follow flag.
    pub(crate) fn set_active_follow(&mut self, follow: bool) {
        if let Some(id) = self.top_dialog() {
            match id {
                DialogKind::Models => self.surfaces.dlg_mut::<crate::surfaces::ModelsDialog>().follow = follow,
                DialogKind::Connections => self.surfaces.dlg_mut::<crate::surfaces::ConnectionsDialog>().follow = follow,
                DialogKind::Tools => self.surfaces.dlg_mut::<crate::surfaces::ToolsDialog>().follow = follow,
                DialogKind::Mcp => self.surfaces.dlg_mut::<crate::surfaces::McpDialog>().follow = follow,
                DialogKind::Skills => self.surfaces.dlg_mut::<crate::surfaces::SkillsDialog>().follow = follow,
                DialogKind::Sessions => self.surfaces.dlg_mut::<crate::surfaces::SessionsDialog>().follow = follow,
                DialogKind::HistorySearch => self.surfaces.dlg_mut::<crate::surfaces::HistorySearchDialog>().follow = follow,
                DialogKind::SessionTree => self.surfaces.dlg_mut::<crate::surfaces::SessionTreeDialog>().follow = follow,
                DialogKind::Queue => self.surfaces.dlg_mut::<crate::surfaces::QueueDialog>().follow = follow,
                DialogKind::Asides => self.surfaces.dlg_mut::<crate::surfaces::AsidesDialog>().follow = follow,
                DialogKind::Permissions
                | DialogKind::UsageStats
                | DialogKind::Telemetry
                | DialogKind::Switcher => {}
            }
        }
    }

    /// Whether the in-dialog key-reference sub-layer is open on the active
    /// dialog.
    pub(crate) fn dialog_keys(&self) -> bool {
        self.surfaces.active_view().is_some_and(|v| v.keys_open())
    }

    /// Toggle the active dialog's key-reference sub-layer.
    pub(crate) fn set_dialog_keys(&mut self, open: bool) {
        if let Some(v) = self.surfaces.active_view_mut() {
            v.set_keys_open(open);
        }
    }

    /// The active dialog's key-reference body scroll offset.
    pub(crate) fn dialog_keys_scroll(&mut self) -> Option<&mut usize> {
        self.surfaces.active_view_mut().map(|v| v.keys_scroll_mut())
    }

    /// Run the App-side teardown a sheet owns when it leaves the overlay stack
    /// (the sheet equivalent of [`DialogView::on_dismiss`], `[INV-SURFACE-04]`).
    pub(crate) fn on_sheet_dismissed(&mut self, kind: SheetKind) {
        match kind {
            SheetKind::ModelEditor => {
                self.editor_target = None;
                self.editor_model_settings_only = false;
                self.editor_target_is_builtin = false;
                self.input.clear();
                self.set_cursor(0);
            }
            SheetKind::CustomProvider => {
                self.custom_field = 0;
                self.custom_edit_id = None;
            }
            SheetKind::ProviderPreset => {
                self.preset_choice = 0;
                self.preset_scroll = 0;
            }
            SheetKind::OAuthPending => {
                self.oauth_scroll = 0;
            }
            SheetKind::ProviderDeleteConfirm => {
                self.cancel_provider_delete();
            }
            SheetKind::Permission | SheetKind::Question | SheetKind::InputInjection => {}
        }
    }

    /// Navigate to a root scene, running each dropped sheet's teardown.
    pub(crate) fn switch_scene(&mut self, scene: SceneKind) {
        let sheets = self.surfaces.take_sheets();
        self.surfaces.switch_scene(scene);
        for s in sheets {
            self.on_sheet_dismissed(s);
        }
    }

    /// Hard reset to Conversation home scene: unwind all overlays and history.
    pub(crate) fn reset_to_conversation(&mut self) {
        let sheets = self.surfaces.take_sheets();
        self.surfaces.reset_to_conversation();
        for s in sheets {
            self.on_sheet_dismissed(s);
        }
    }

    /// Pop one overlay and restore the underlying surface.
    pub(crate) fn pop_transient_surface(&mut self) {
        self.surfaces.pop_overlay();
    }

    pub(crate) fn modal_scroll_field(&mut self) -> Option<(&mut usize, Option<&mut bool>)> {
        if self.dialog_keys() {
            return self.dialog_keys_scroll().map(|s| (s, None));
        }
        if self.active_sheet() == Some(crate::sheet::SheetKind::Question)
            && self.surfaces.active_overlay().is_none()
        {
            return Some((
                &mut self.question_scroll,
                Some(&mut self.question_modal_follow),
            ));
        }
        if let Some(id) = self.active_dialog() {
            let v = self.surfaces.view_by_kind_mut(id)?;
            let (scroll, follow) = v.nav_fields();
            return match id {
                DialogKind::Permissions
                | DialogKind::UsageStats
                | DialogKind::Telemetry
                | DialogKind::Switcher => Some((scroll, None)),
                _ => Some((scroll, follow)),
            };
        }
        if let Some(overlay) = self.surfaces.active_overlay() {
            match overlay {
                OverlaySurface::Sheet(s) => match s {
                    SheetKind::OAuthPending => Some((&mut self.oauth_scroll, None)),
                    SheetKind::ProviderPreset => Some((&mut self.preset_scroll, None)),
                    SheetKind::CustomProvider => Some((&mut self.custom_scroll, None)),
                    _ => None,
                },
                OverlaySurface::Dialog(_) => None,
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
            self.send_intent(nuo_wire::AgentRequest::QueryInputHistory);
        }
        // Session transitions isolate their effects to session-scoped dialogs
        // (`[INV-SURFACE-05]`): archive the outgoing session's modal state,
        // reinstate the incoming session's, and leave global dialog state (and
        // the open global dialogs themselves) untouched.
        self.surfaces.unwind_session();
        let incoming = self.current_session_id.clone();
        self.surfaces.dialogs.switch_session(&incoming);
        self.surfaces
            .retain_available(self.current_scene(), self.has_session());
        self.session_context = None;
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
            && !(active_dialog == Some(DialogKind::Sessions)
                && self.surfaces.dlg::<crate::surfaces::SessionsDialog>().info_detail)
            && !(active_dialog == Some(DialogKind::Telemetry)
                && (self.surfaces.dlg::<crate::surfaces::TelemetryDialog>().detail
                    || self.surfaces.dlg::<crate::surfaces::TelemetryDialog>().turn.is_some()))
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
        self.surfaces.unwind_session();
        let incoming = self.current_session_id.clone();
        self.surfaces.dialogs.switch_session(&incoming);
        self.focus_stack.clear();
        self.in_side_view = false;
        self.side_session_id = None;
        self.session_detail = None;
        self.surfaces.dlg_mut::<crate::surfaces::SessionsDialog>().info_detail = false;
        self.surfaces.dlg_mut::<crate::surfaces::SessionsDialog>().info_scroll = 0;
        self.session_history_backfill_cursor = 0;
    }

    /// Focus a browse dialog. Returns `true` on first open, and refuses to open
    /// a dialog whose preconditions are unsatisfied (`[INV-SURFACE-03]`).
    pub(crate) fn open_dialog(&mut self, id: DialogKind) -> bool {
        if !self.dialog_available(id) {
            return false;
        }
        if let Some(current) = self.active_dialog()
            && current != id
        {
            self.deactivate_dialog(current);
        }
        if id == DialogKind::HistorySearch && self.input_history_persist {
            self.send_intent(nuo_wire::AgentRequest::QueryInputHistory);
        }
        let first = !self.surface_store.is_open(id);
        self.surface_store.open(id);
        self.surfaces.present_dialog(id);
        first
    }

    /// Persist current TUI presentation preferences into `$XDG_CONFIG_HOME/nuo/tui.toml`.
    pub fn save_tui_config(&self) {
        let mut cfg = crate::config::TuiConfig::load();
        cfg.color_scheme = self.color_scheme.clone();
        cfg.custom_color_scheme = self.custom_color_scheme.clone();
        cfg.click_outside_dismiss = self.click_outside_dismiss;
        cfg.expand_auto_scroll = self.expand_auto_scroll;
        cfg.default_expanded = self.tui_config.default_expanded.clone();
        let _ = cfg.save();
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

    /// Run the App-side exit hook for one exact dialog. The dialog entity's own
    /// dismissal hook runs in `SurfaceRouter::pop_overlay`.
    pub(crate) fn deactivate_dialog(&mut self, id: DialogKind) {
        self.set_dialog_keys(false);
        if id == DialogKind::Sessions {
            self.surfaces.dlg_mut::<crate::surfaces::SessionsDialog>().loading = false;
            self.surfaces.dlg_mut::<crate::surfaces::SessionsDialog>().info_detail = false;
            self.session_detail = None;
            self.surfaces.dlg_mut::<crate::surfaces::SessionsDialog>().info_scroll = 0;
        }
        if id == DialogKind::Connections {
            self.surfaces.dlg_mut::<crate::surfaces::ConnectionsDialog>().info_detail = false;
            self.surfaces.dlg_mut::<crate::surfaces::ConnectionsDialog>().info_standalone = false;
            self.connection_detail = None;
            self.surfaces.dlg_mut::<crate::surfaces::ConnectionsDialog>().info_scroll = 0;
            self.surfaces.dlg_mut::<crate::surfaces::ConnectionsDialog>().models_expanded = false;
        }
        if id == DialogKind::Telemetry {
            self.surfaces.dlg_mut::<crate::surfaces::TelemetryDialog>().tab = crate::overlays::telemetry::TelemetryTab::Overview;
            self.surfaces.dlg_mut::<crate::surfaces::TelemetryDialog>().detail = false;
            self.surfaces.dlg_mut::<crate::surfaces::TelemetryDialog>().turn = None;
            self.surfaces.dlg_mut::<crate::surfaces::TelemetryDialog>().turn_cursor = 0;
        }
        if id == DialogKind::Queue
            && let Some(sid) = self.queue_exit_session.take()
        {
            self.resume_queue(&sid);
        }
    }

    /// Dismiss the active dialog overlay, restoring the dialog underneath (if
    /// any). Strictly **overlay-scoped**: it never leaves the Scene.
    pub(crate) fn dismiss_active_dialog(&mut self) -> bool {
        if let Some(id) = self.active_dialog() {
            self.deactivate_dialog(id);
            self.surfaces.pop_overlay();
            true
        } else {
            false
        }
    }

    /// Leave a root scene back to the scene it came from.
    pub(crate) fn leave_scene(&mut self) -> bool {
        let leaving = self.current_scene();
        if leaving == SceneKind::Conversation {
            return false;
        }
        let sheets = self.surfaces.take_sheets();
        self.surfaces.back_scene();
        for s in sheets {
            self.on_sheet_dismissed(s);
        }
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

    /// Actively leave the current Scene: the `C-x` scene namespace's exit and
    /// each scene's own `q`.
    pub(crate) fn close_scene(&mut self) -> bool {
        self.scene_namespace_armed = false;
        match self.current_scene() {
            SceneKind::Aside => {
                self.exit_side_view();
                self.arm_esc(None);
                self.send_intent(nuo_wire::AgentRequest::ExitSideView);
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

    /// Step back one level **inside** the active Scene.
    pub(crate) fn scene_back(&mut self) {
        if self.current_scene() == SceneKind::Settings {
            if self.config_dropdown.is_some() {
                self.config_dropdown = None;
                return;
            }
            if self.config_focus == crate::overlays::ConfigFocus::Detail {
                if crate::overlays::ConfigCategory::from_index(self.config_category)
                    == crate::overlays::ConfigCategory::Appearance
                {
                    let ws_path = if self.current_workspace.is_empty() {
                        None
                    } else {
                        Some(std::path::Path::new(&self.current_workspace))
                    };
                    self.theme = Theme::resolve_with_profile(
                        &self.color_scheme,
                        &self.custom_color_scheme,
                        ws_path,
                        &self.profile,
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
        self.surfaces.dialogs.reset(id);
    }

    /// Pop the deepest sub-layer of a view or dialog.
    pub(crate) fn pop_sublayer(&mut self) -> bool {
        if self.dialog_keys() {
            self.set_dialog_keys(false);
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
                    let t = &mut self.surfaces.dlg_mut::<crate::surfaces::TelemetryDialog>();
                    if t.turn.is_some() {
                        t.turn = None;
                        t.scroll = 0;
                        return true;
                    }
                    if t.detail {
                        t.detail = false;
                        t.turn_cursor = 0;
                        t.scroll = 0;
                        return true;
                    }
                }
                DialogKind::Sessions if self.surfaces.dlg_mut::<crate::surfaces::SessionsDialog>().info_detail => {
                    self.surfaces.dlg_mut::<crate::surfaces::SessionsDialog>().info_detail = false;
                    self.session_detail = None;
                    self.surfaces.dlg_mut::<crate::surfaces::SessionsDialog>().info_scroll = 0;
                    return true;
                }
                DialogKind::Connections if self.surfaces.dlg_mut::<crate::surfaces::ConnectionsDialog>().info_detail => {
                    let standalone = self.surfaces.dlg_mut::<crate::surfaces::ConnectionsDialog>().info_standalone;
                    self.surfaces.dlg_mut::<crate::surfaces::ConnectionsDialog>().info_detail = false;
                    self.surfaces.dlg_mut::<crate::surfaces::ConnectionsDialog>().info_standalone = false;
                    self.connection_detail = None;
                    self.surfaces.dlg_mut::<crate::surfaces::ConnectionsDialog>().info_scroll = 0;
                    self.surfaces.dlg_mut::<crate::surfaces::ConnectionsDialog>().models_expanded = false;
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
            transport_setback: self.provider_retry.clone(),
        }
    }

    /// Write the primary session's activity phase, retiring its
    /// transport-setback clause by the same rule as
    /// [`SessionChrome::set_phase`].
    pub fn set_phase(&mut self, phase: Option<crate::phase::Phase>) {
        if crate::phase::ends_transport_setback(phase.as_ref()) {
            self.provider_retry = None;
        }
        self.phase = phase;
    }

    /// Whether any session currently carries a live transport setback.
    pub fn has_live_transport_setback(&self) -> bool {
        self.provider_retry.is_some()
            || self
                .session_chrome
                .values()
                .any(|chrome| chrome.transport_setback.is_some())
    }

    /// Copy a viewed session's chrome into the App-level mirrors.
    pub(super) fn apply_chrome(&mut self, chrome: &SessionChrome) {
        self.phase = chrome.phase.clone();
        self.round_started_at = chrome.round_started_at;
        self.round_count = chrome.round_count;
        self.current_turn = chrome.current_turn;
    }

    pub fn clear_responding(&mut self) {
        self.set_phase(None);
        self.round_started_at = None;
        self.loop_status = nuo_wire::LoopStatus::Idle;
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
