//! Provider modals state: template chooser, custom editor fields, the OAuth flow, and delete staging.

use super::*;

impl App {
    /// Open the curated template chooser — the "Add curated connection" entry
    /// point.
    /// The chat draft is already parked in `stashed_input` (the Connections list
    /// stashed it on open); the chooser is a pure list, so the composer line
    /// stays clear.
    pub fn open_preset_chooser(&mut self) {
        self.surfaces
            .present_sheet(crate::surfaces::SheetKind::ProviderPreset);
        self.preset_choice = 0;
        self.preset_scroll = 0;
        self.input.clear();
        self.set_cursor(0);
    }

    /// Open the standalone custom-connection editor directly from
    /// Connections. Unlike a curated template selection, this is a sibling
    /// branch of the add flow, so the Connections panel stays below it on the
    /// transient navigation stack.
    pub fn open_custom_connection_editor(&mut self) {
        self.seed_custom_provider_from_template(&crate::providers::CUSTOM_TEMPLATE);
        self.surfaces
            .present_sheet(crate::surfaces::SheetKind::CustomProvider);
        self.input.clear();
        self.set_cursor(0);
    }

    /// Move the template-chooser selection, wrapping at the ends.
    pub fn move_preset_choice(&mut self, forward: bool) {
        let n = crate::PROVIDER_PRESETS.len();
        if n == 0 {
            return;
        }
        self.preset_choice = if forward {
            (self.preset_choice + 1) % n
        } else {
            (self.preset_choice + n - 1) % n
        };
    }

    /// Seed create-mode buffers from `template` without opening the editor.
    pub fn seed_custom_provider_from_template(&mut self, template: &ConnectionTemplate) {
        self.custom_edit_id = None;
        self.custom_fields = template.fields();
        self.custom_field = 0;
        self.custom_protocol_wire = template.protocol.to_string();
        self.custom_client_identity = template
            .user_agent
            .map(nuo_wire::ClientIdentity::from_user_agent)
            .unwrap_or(nuo_wire::ClientIdentity::Native);
        self.custom_models = template.models.iter().map(|m| m.to_string()).collect();
        self.custom_url_hint = template.url_hint.to_string();
        self.custom_user_agent = template.user_agent.map(str::to_string);
        self.custom_auth = template.auth.clone();
        self.custom_provider_id = Some(template.id.to_string());
        self.custom_name.clear();
        self.custom_base_url = template.default_url.map(str::to_string).unwrap_or_default();
        self.custom_token.clear();
        self.custom_model.clear();
    }

    /// Open the provider editor seeded from `template` (create mode) on the Name
    /// field. The composer line is borrowed for the focused Name field.
    pub fn open_custom_provider_editor(&mut self, template: &ConnectionTemplate) {
        self.seed_custom_provider_from_template(template);
        self.surfaces
            .present_sheet(crate::surfaces::SheetKind::CustomProvider);
        self.input.clear();
        self.set_cursor(0);
    }

    /// Open the OAuth waiting sheet and seed create buffers from `template`.
    pub fn begin_oauth_add(
        &mut self,
        template: &ConnectionTemplate,
        method: nuo_wire::LoginMethod,
    ) {
        self.seed_custom_provider_from_template(template);
        self.awaiting_oauth_add = true;
        // The default message mirrors the selected login method: the device
        // flow prints a URL + user code,
        // while the browser flow opens a loopback callback. The auth subagent
        // overwrites this with the live URL/code as soon as the device-code
        // request returns.
        self.oauth_pending_message = match method {
            nuo_wire::LoginMethod::Device => "Requesting device code…".to_string(),
            nuo_wire::LoginMethod::Browser => {
                "Complete authorization in your browser (or open the link below).".to_string()
            }
        };
        self.oauth_pending_url.clear();
        self.oauth_pending_user_code.clear();
        self.oauth_pending_error = None;
        self.oauth_scroll = 0;
        self.surfaces
            .present_sheet(crate::surfaces::SheetKind::OAuthPending);
        self.input.clear();
        self.set_cursor(0);
    }

    /// After OAuth succeeds: name-only editor (default name derived from the
    /// in-flight auth — "xAI" for SuperGrok, "ChatGPT Subscription" for the
    /// ChatGPT plan).
    pub fn open_oauth_instance_name_editor(&mut self) {
        self.awaiting_oauth_add = false;
        self.oauth_pending_url.clear();
        self.oauth_pending_user_code.clear();
        self.oauth_pending_message.clear();
        self.oauth_pending_error = None;
        self.oauth_scroll = 0;
        self.surfaces
            .present_sheet(crate::surfaces::SheetKind::CustomProvider);
        self.custom_fields = vec![CustomField::Name];
        self.custom_field = 0;
        self.custom_edit_id = None;
        let default_name = match &self.custom_auth {
            nuo_wire::ConnectionAuth::Subscription { provider } => match provider.as_ref() {
                "chatgpt" => "ChatGPT Subscription",
                "copilot" => "Copilot",
                "google-antigravity" => "Google Antigravity",
                "qoder" => "Qoder",
                "opencode" | "opencode-go" => "OpenCode Go",
                _ => "xAI",
            },
            nuo_wire::ConnectionAuth::ApiKey => "Custom",
        };
        self.custom_name = default_name.to_string();
        self.input = default_name.to_string();
        self.set_cursor_end();
    }

    /// Return which OAuth target is currently selected.
    pub fn oauth_selected_target(&self) -> crate::input::OauthCopyTarget {
        if self.oauth_selected_item == 1 && !self.oauth_pending_user_code.is_empty() {
            crate::input::OauthCopyTarget::UserCode
        } else {
            crate::input::OauthCopyTarget::Url
        }
    }

    /// Cycle selection between URL (0) and Code (1) in OAuth Pending sheet.
    pub fn cycle_oauth_selection(&mut self) {
        if !self.oauth_pending_user_code.is_empty() {
            self.oauth_selected_item = if self.oauth_selected_item == 0 { 1 } else { 0 };
        } else {
            self.oauth_selected_item = 0;
        }
    }

    /// Auth mode of a provider picker row (for OAuth re-connect routing).
    pub fn provider_row_auth(&self, id: &str) -> nuo_wire::ConnectionAuth {
        self.provider_picker
            .rows
            .iter()
            .find(|r| r.id == id)
            .map(|r| r.auth.clone())
            .unwrap_or_default()
    }

    /// Open the provider editor in **edit** mode for an existing connection,
    /// pre-filling its metadata. The visible fields depend on whether it is a
    /// curated or `custom` connection, and its auth type: a custom API-key
    /// connection shows Name / Base URL / Token, a curated API-key connection
    /// shows Name / Token, and an OAuth connection (ChatGPT/Codex, xAI, Copilot,
    /// Antigravity) shows Name only. The Model field is always hidden (models
    /// are managed in the Models picker).
    ///
    /// `key` is the connection's identity (ADR-0201 INV-3) — the request that
    /// saves this form is keyed by it, and a changed Name is submitted as the
    /// separate `RenameConnection` transaction.
    #[allow(clippy::too_many_arguments)] // Editor state is seeded from one connection snapshot.
    pub fn open_edit_provider_editor(
        &mut self,
        key: String,
        name: String,
        protocol: String,
        base_url: String,
        auth: ConnectionAuth,
        curated: bool,
        client_identity: nuo_wire::ClientIdentity,
    ) {
        self.surfaces
            .present_sheet(crate::surfaces::SheetKind::CustomProvider);
        self.custom_edit_id = Some(key);
        self.custom_fields = edit_fields(curated, auth.clone());
        self.custom_field = 0;
        self.custom_protocol_wire = protocol;
        self.custom_client_identity = client_identity;
        self.custom_models.clear();
        self.custom_url_hint.clear();
        self.custom_user_agent = None;
        self.custom_auth = auth;
        // Edit mode never carries a provider id: edits to an existing
        // connection are sent as `EditConnection`, which is keyed by the
        // connection name, and a stray id here must not leak into a later
        // create flow.
        self.custom_provider_id = None;
        self.custom_name = name.clone();
        self.custom_base_url = base_url;
        self.custom_token.clear();
        self.custom_model.clear();
        self.input = name;
        self.set_cursor_end();
    }

    /// Whether the provider editor is editing an existing provider.
    pub fn custom_is_editing(&self) -> bool {
        self.custom_edit_id.is_some()
    }

    /// The currently focused editor field, or `None` when no editor is open.
    pub fn current_custom_field(&self) -> Option<CustomField> {
        self.custom_fields.get(self.custom_field as usize).copied()
    }

    /// Number of visible fields the editor exposes for the active template.
    fn custom_field_count(&self) -> u8 {
        self.custom_fields.len().max(1) as u8
    }

    /// Scroll the custom editor body with `↑` / `↓`.
    pub fn scroll_custom_provider(&mut self, forward: bool) {
        if forward {
            self.custom_scroll = self.custom_scroll.saturating_add(1);
        } else {
            self.custom_scroll = self.custom_scroll.saturating_sub(1);
        }
    }

    /// Cycle an inline selector in the custom editor with `←` / `→`.
    pub fn cycle_custom_choice(&mut self, forward: bool) {
        match self.current_custom_field() {
            Some(CustomField::Protocol) => {
                const PROTOCOLS: &[nuo_wire::WireProtocol] = &[
                    nuo_wire::WireProtocol::ChatCompletions,
                    nuo_wire::WireProtocol::Responses,
                    nuo_wire::WireProtocol::AnthropicMessages,
                    nuo_wire::WireProtocol::GoogleGemini,
                ];
                let current = self
                    .custom_protocol_wire
                    .parse::<nuo_wire::WireProtocol>()
                    .unwrap_or_default();
                let index = PROTOCOLS
                    .iter()
                    .position(|value| *value == current)
                    .unwrap_or(0);
                let next = if forward {
                    (index + 1) % PROTOCOLS.len()
                } else {
                    (index + PROTOCOLS.len() - 1) % PROTOCOLS.len()
                };
                self.custom_protocol_wire = PROTOCOLS[next].to_string();
            }
            Some(CustomField::ClientIdentity) => {
                let choices = nuo_wire::ClientIdentity::PRESETS;
                let index = choices
                    .iter()
                    .position(|value| value == &self.custom_client_identity)
                    .unwrap_or(0);
                let next = if forward {
                    (index + 1) % choices.len()
                } else {
                    (index + choices.len() - 1) % choices.len()
                };
                self.custom_client_identity = choices[next].clone();
            }
            _ => {}
        }
    }

    /// Whether the focused provider field owns the composer text buffer.
    pub fn custom_text_field_focused(&self) -> bool {
        matches!(
            self.current_custom_field(),
            Some(
                CustomField::Name | CustomField::BaseUrl | CustomField::Token | CustomField::Model
            )
        )
    }

    /// Save the composer line into the focused text field's buffer (Name / Base
    /// URL / Token / Model). Selector fields do not own a text value.
    pub fn stash_custom_field(&mut self) {
        let value = std::mem::take(&mut self.input);
        match self.current_custom_field() {
            Some(CustomField::Name) => self.custom_name = value,
            Some(CustomField::BaseUrl) => self.custom_base_url = value,
            Some(CustomField::Token) => self.custom_token = value,
            Some(CustomField::Model) => self.custom_model = value,
            _ => {}
        }
    }

    /// Load the focused field into the composer line: the buffer for a text
    /// field. Inline selector fields leave the composer empty.
    pub fn load_custom_field(&mut self) {
        self.input = match self.current_custom_field() {
            Some(CustomField::Name) => self.custom_name.clone(),
            Some(CustomField::BaseUrl) => self.custom_base_url.clone(),
            Some(CustomField::Token) => self.custom_token.clone(),
            Some(CustomField::Model) => self.custom_model.clone(),
            _ => String::new(),
        };
        self.set_cursor_end();
    }

    /// Move the provider editor focus (`Tab` / `BackTab`), wrapping across the
    /// active template's visible fields.
    pub fn cycle_custom_field(&mut self, forward: bool) {
        self.stash_custom_field();
        let n = self.custom_field_count();
        self.custom_field = if forward {
            (self.custom_field + 1) % n
        } else {
            (self.custom_field + n - 1) % n
        };
        self.load_custom_field();
    }

    /// Compute the **Connections** provider rows. Delegates to
    /// `providers_filtered_from` so the input handler and the renderer share
    /// one filter+sort implementation.
    pub fn providers_filtered(&self) -> Vec<RankedProvider> {
        providers_filtered_from(&self.provider_picker, self.picker_query())
    }

    /// Compute the **flat Models** rows — every (provider, model) pair in the
    /// snapshot, filtered by the current picker query. Delegates to
    /// `models_flat_filtered_from` so the input handler and the renderer
    /// share one filter+sort implementation.
    pub fn models_flat_filtered(&self) -> Vec<RankedModel> {
        models_flat_filtered_from(
            &self.provider_picker,
            &self.current_provider,
            &self.current_model,
            self.picker_query(),
        )
    }

    /// Return the authoritative model info for the currently active route
    /// (current_provider, current_model) as projected by the daemon (ADR-0182).
    pub fn active_model_info(&self) -> Option<&nuo_wire::ProviderModelInfo> {
        self.provider_picker
            .rows
            .iter()
            .find(|row| row.id == self.current_provider)
            .and_then(|row| {
                row.model_info
                    .iter()
                    .find(|info| info.model == self.current_model)
            })
    }

    /// Return the authoritative route capabilities for the active route (ADR-0182).
    pub fn active_route_capabilities(&self) -> nuo_wire::RouteCapabilities {
        self.active_model_info()
            .map(|info| info.route_capabilities())
            .unwrap_or_else(|| {
                // No daemon row yet (startup, or a route the snapshot has not
                // refreshed): the baseline layer alone, resolved through the
                // same three-valued merge the daemon uses, so an id no layer
                // knows stays *undeclared* rather than being reported as a
                // text-only claim (ADR-0230).
                nuo_wire::ModelCapabilities::for_channel(&self.current_model, None)
                    .to_route_capabilities()
            })
    }

    /// Return the authoritative context window (in tokens) for the active route.
    /// Evaluated daemon-side via ADR-0149 (ADR-0182). Falls back to static
    /// baseline only when the snapshot has not mounted.
    pub fn active_model_context_window(&self) -> usize {
        let cw = self.active_route_capabilities().context_window;
        if cw > 0 {
            cw
        } else {
            nuo_wire::model::resolve(&self.current_model).context_window
        }
    }

    /// Whether images would travel to the active route.
    ///
    /// **Permissive on undeclared** (ADR-0230): `true` unless a layer declared
    /// that the route rejects images. This answers the *request* policy, not
    /// "was vision advertised" — a gate that must be certain should read
    /// [`Self::active_route_capabilities`]`.vision` and treat `None` as
    /// unknown. Uses daemon-projected capabilities from `ProviderModelInfo`,
    /// falling back to the baseline layers when the snapshot has not mounted.
    pub fn active_model_supports_vision(&self) -> bool {
        self.active_route_capabilities().accepts_images()
    }

    /// Whether the provider with this snapshot id is user-defined (not a
    /// curated provider). Drives the Connections `e`/`Shift+D` routing and the
    /// Models `d` (remove-model) gate.
    pub fn provider_is_custom(&self, id: &str) -> bool {
        self.provider_picker
            .rows
            .iter()
            .find(|row| row.id == id)
            .map(|row| !row.builtin)
            .unwrap_or(false)
    }

    /// Number of selectable rows in the active picker. Connections counts only
    /// the provider rows (adding a connection is a footer shortcut now, not a
    /// synthetic list row); Models counts the flat (provider, model) rows. Used
    /// to clamp the ↑/↓ selection cursor. Returns 0 when no picker is open.
    pub fn picker_row_count(&self) -> usize {
        match self.active_dialog() {
            Some(crate::surfaces::DialogKind::Connections) => self.providers_filtered().len(),
            Some(crate::surfaces::DialogKind::Models) => self.models_flat_filtered().len(),
            _ => 0,
        }
    }

    /// Stage the highlighted user-defined connection for deletion: open the
    /// confirm overlay ([`App::pending_provider_delete`]) over the Connections
    /// list without destroying anything yet. No-op for curated providers or when
    /// an overlay is already open (prevents re-staging). Driven by the `Shift+D`
    /// → `DeleteProvider` input action.
    pub fn stage_provider_delete(&mut self) {
        if self.active_dialog() != Some(crate::surfaces::DialogKind::Connections)
            || self.pending_provider_delete.is_some()
        {
            return;
        }
        let ranked = self.providers_filtered();
        if let Some(row) = ranked.get(self.active_index()).or_else(|| ranked.first())
            && !row.builtin
        {
            self.pending_provider_delete = Some(row.id.clone());
            self.provider_delete_focus = ProviderDeleteChoice::default();
        }
    }

    /// Confirm the staged deletion: dispatch `AgentRequest::DeleteConnection`
    /// for the staged connection name and tear the overlay down. Returns
    /// `Some(request)` when a deletion was staged (the harness applies it),
    /// `None` when the overlay was not open. Driven by the overlay's
    /// Enter-on-Delete. Decrementing `modal_index` mirrors the picker's other
    /// removal paths so the cursor lands on a valid row once this row vanishes.
    pub fn confirm_provider_delete(&mut self) -> Option<AgentRequest> {
        let name = self.pending_provider_delete.take()?;
        let idx = self.active_index().saturating_sub(1);
        self.set_active_index(idx);
        self.provider_delete_focus = ProviderDeleteChoice::default();
        Some(AgentRequest::DeleteConnection { name })
    }

    /// Cancel the staged deletion: drop the staged id and return keyboard
    /// focus to the Connections list. The modal itself stays open.
    /// Driven by the overlay's Esc / Ctrl+C / Enter-on-Cancel.
    pub fn cancel_provider_delete(&mut self) {
        self.pending_provider_delete = None;
        self.provider_delete_focus = ProviderDeleteChoice::default();
    }
}
