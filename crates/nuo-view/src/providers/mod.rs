//! Snapshot-driven provider/model picker filter & sort logic.
//!
//! The pickers render directly from [`nuo_protocol::ProviderPickerSnapshot`] — one
//! [`nuo_protocol::ProviderPickerRow`] per provider the harness knows how to
//! drive, carrying the display name, the served model ids, the active model, and
//! the live per-user signals (favorite, key-ready, last-used). Built-in and
//! user-defined providers share this single path, so a custom provider added via
//! the editor shows up like any built-in (there is no separate static table).
//!
//! Two surfaces read the same snapshot:
//!
//! - **Connections** (`/connections`): [`providers_filtered_from`] builds the
//!   provider-instance list — the management surface (favorite, edit, delete,
//!   add). Activating a provider activates its current model.
//! - **Models** (`/models`, `Ctrl+M`): [`models_flat_filtered_from`] builds a
//!   **flat** list of every (provider, model) pair — the daily-driver switch
//!   surface. There is no drilling: one row per pair, Enter activates. The
//!   list is grouped into three labeled sections (Favorites → Recent → All
//!   models; see [`ModelSection`]), and [`models_body_lines`] maps the flat
//!   row indices onto the body's line geometry for the renderer.
use nuo_protocol::{ConnectionAuth, ProviderPickerSnapshot, WireProtocol};

use nuo_ui_primitives::fuzzy;

/// One editable field of the provider editor. The visible set is chosen by the
/// active [`ConnectionTemplate`] (create) or the edited connection's provider
/// (edit), rather than a fixed five-field form. Provider-owned model collections are
/// imported from `nuo_protocol`; this view layer only selects and renders
/// those curated values.
///
/// Reasoning (effort/thinking) is intentionally NOT a provider-editor field —
/// ADR-0046 moved it to the per-model `e` editor in the Models picker, so a
/// provider is
/// created/authed here and its models are reasoned with (or not) individually.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CustomField {
    Name,
    BaseUrl,
    Token,
    Model,
    Protocol,
    Budget,
}

/// A curated starting point for adding a connection — the creation-time
/// template (ADR-0014, ADR-0027). A template is consumed when the connection is
/// created and never persisted as identity; the created connection records
/// only the [`Self::id`] as its `provider` (a model provider names a service
/// surface). Curated templates lock the wire protocol and seed
/// their model list; the standalone `custom` template exposes protocol, model,
/// and request identity in its editor. Modelled as *data* — one table entry per
/// template.
pub struct ConnectionTemplate {
    /// The **model provider id** this template creates a connection for
    /// (`"openai"`, `"openai-subscription"`, `"custom"`, …). MUST match the
    /// matching `nuo_provider::model_provider_spec` id 1:1 and never change
    /// once shipped: it is persisted as the connection's `provider` and is the
    /// join key the catalog resolves models with.
    pub id: &'static str,
    /// List label, e.g. `"Custom Anthropic (Claude relay)"`.
    pub label: &'static str,
    /// One-sentence description shown wrapped under the label in the chooser's
    /// focused row. It must read as prose and cover what the user acts on:
    /// what the service is, what it serves, and how it authenticates ("sign
    /// in with an API key" vs "authorizes in the browser").
    pub description: &'static str,
    /// Default wire protocol for the created connection, sent in
    /// `AgentRequest::AddConnection` as `protocol: Some(..)` **only** for the
    /// `custom` provider; a curated provider owns its wire and is created with
    /// `protocol: None`.
    pub protocol: WireProtocol,
    /// Models seeded as channels. Empty means the user enters one via the Model
    /// field (templates can opt in when they need one).
    pub models: &'static [&'static str],
    /// Whether the editor shows a Base URL field (false for native Google).
    pub needs_url: bool,
    /// Placeholder shown in the Base URL field — the full endpoint shape.
    pub url_hint: &'static str,
    /// Whether the editor exposes a free-text Model field. Most templates seed
    /// `models`; open protocols can still add arbitrary model ids later.
    pub needs_model: bool,
    /// A concrete endpoint pre-filled into the Base URL field on open
    /// (create mode), so a service-specific template works without the user
    /// typing the host. `None` for templates whose `url_hint` is a placeholder
    /// only and whose field starts empty, since the user supplies their own
    /// relay host. When set, the user can still edit the value.
    pub default_url: Option<&'static str>,
    /// Template-specific user agent. Most providers use the default agent, but
    /// the coding-plan endpoints validate this header.
    pub user_agent: Option<&'static str>,
    /// How connections created from this template authenticate. `XaiOAuth`
    /// starts browser OAuth before the name editor (OAuth-first add flow).
    pub auth: nuo_protocol::ConnectionAuth,
}

impl ConnectionTemplate {
    /// The title the template chooser sorts and keys rows by. Every template
    /// renders its [`Self::label`] alone as the row title — any suffix is part
    /// of the label, so this accessor exists to name that rule and give the
    /// sort a single home rather than to project a second spelling of the
    /// label.
    pub fn display_title(&self) -> &'static str {
        self.label
    }

    /// The ordered, visible editor fields for this template (create mode).
    /// OAuth templates only ask for the connection name (auth already completed).
    pub fn fields(&self) -> Vec<CustomField> {
        if self.auth.is_negotiated() {
            return vec![CustomField::Name];
        }
        if self.id == CUSTOM_TEMPLATE.id {
            return vec![
                CustomField::Name,
                CustomField::BaseUrl,
                CustomField::Token,
                CustomField::Model,
                CustomField::Protocol,
            ];
        }
        let mut fields = vec![CustomField::Name];
        if self.needs_url {
            fields.push(CustomField::BaseUrl);
        }
        fields.push(CustomField::Token);
        if self.needs_model {
            fields.push(CustomField::Model);
        }
        // No Effort/Thinking: ADR-0046 made reasoning a per-model concern.
        fields
    }

    /// Whether selecting this template starts OAuth before the name editor.
    pub fn oauth_first(&self) -> bool {
        self.auth.is_negotiated()
    }
}

/// The connection templates offered when adding a connection, **sorted
/// alphabetically by title**. The chooser renders rows in this order and keys
/// `↑/↓` movement to it, so the declared order here IS the display order —
/// insert new entries at their sorted position, not at the end.
///
/// Every `id` is a **model provider id** and is persisted on the
/// created connection as `provider`.
mod filtering;
mod lookup;
mod presets;
mod ranking;

#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) use self::filtering::BONUS_CROSS_FIELD;
pub use self::filtering::*;
#[cfg(test)]
pub(crate) use self::lookup::protocol_model_candidates;
pub use self::lookup::*;
pub use self::presets::*;
pub use self::ranking::*;
