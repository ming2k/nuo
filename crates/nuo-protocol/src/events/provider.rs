//! Provider/model picker snapshots and catalog sync state.

use super::*;

/// One row of provider-picker state sent from the harness to the TUI. Carries
/// everything the picker renders for a provider — display name, the served model
/// ids and the active one, plus the dynamic signals (key readiness, favorite,
/// last-used) — keyed by canonical provider id. The TUI renders directly from
/// these rows (built-in and user-defined providers share one path), so no static
/// per-provider table is consulted. See ADR-0002. Recency exists at BOTH
/// levels: the provider row's `last_used_ms` orders the Connections list, and
/// each `model_info` entry's `last_used_ms` orders the flat Models picker's
/// "recent" section.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderPickerRow {
    pub id: String,
    /// Display name (e.g. `"OpenAI"`, `"Anthropic"`, or a custom connection's name).
    pub name: String,
    /// Wire id of the currently-active model on this connection.
    pub model: String,
    /// Every model id this connection serves, in catalog order.
    pub models: Vec<String>,
    /// Per-model/channel settings in the same order as `models`.
    #[serde(default)]
    pub model_info: Vec<ProviderModelInfo>,
    /// `true` for built-in presets, `false` for user-defined connections.
    pub builtin: bool,
    /// Wire protocol id of the default channel (`"openai"` | `"anthropic"` |
    /// `"google"`), used to pre-fill the edit form.
    pub protocol: String,
    /// Base URL of the default channel, used to pre-fill the edit form.
    pub base_url: String,
    pub key_ready: bool,
    /// The model provider this connection points at (`"openai"`,
    /// `"anthropic"`, `"deepseek"`, …).
    #[serde(default)]
    pub provider: String,
    /// Custom headers configured for this connection.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub headers: std::collections::BTreeMap<String, String>,
    /// Unix epoch milliseconds of the last activation. `None` if never activated.
    pub last_used_ms: Option<u64>,
    /// How the connection authenticates.
    #[serde(default)]
    pub auth: crate::ConnectionAuth,
}

pub type ConnectionPickerRow = ProviderPickerRow;

/// Why a live catalog refresh did not deliver a fresh model list.
///
/// The distinction is what the user can *do*: a refusal means the account was
/// told it may not use the provider, so retrying is pointless and the remedy is
/// an entitlement change; a transient failure means retrying is the remedy. The
/// default is [`Self::Transient`] because it claims less.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CatalogSyncFailure {
    /// The upstream failed to serve the list: network, timeout, `429`, `5xx`.
    #[default]
    Transient,
    /// The upstream refused the request as unauthorized or forbidden
    /// (`401`/`403`): the connection is not usable with these credentials or
    /// this plan.
    Refused,
}

/// Progress / outcome of an OAuth connect flow (xAI SuperGrok).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ConnectStatus {
    /// User must complete authorization out-of-band. `url` is the authorize /
    /// verification URL; `user_code` is set for device-code (empty for browser).
    Pending {
        provider: String,
        url: String,
        user_code: String,
        message: String,
    },
    /// Authorization succeeded; tokens persisted (and provider activated when
    /// this followed [`AgentRequest::ConnectConnection`]).
    Done { provider: String },
    /// Authorization succeeded but the follow-up live catalog sync failed,
    /// so the provider keeps its previous (often seed-only) model list. The
    /// UI surfaces this as a warning so the user does not mistake a stale list
    /// for the account's real entitlements. `kind` says whether the upstream
    /// refused the account or merely failed to answer.
    CatalogSyncWarning {
        provider: String,
        message: String,
        #[serde(default)]
        kind: CatalogSyncFailure,
    },
    /// Authorization failed or was denied.
    Failed { provider: String, message: String },
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderModelInfo {
    /// Wire model id. Mirrors an entry in [`ProviderPickerRow::models`].
    pub model: String,
    /// Human-readable label the provider advertises for this model (models.dev
    /// `name`, Anthropic/Kimi `display_name`), when it advertises one. This is
    /// the label the model pickers lead with, falling back to `model` when it is
    /// absent. Purely presentational: it is never the identity, so frontends
    /// must keep the wire id visible beside it and key every config surface on
    /// `model`. `None` — the common case — means show the bare wire id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Wire protocol id of the channel serving this model (`"openai-chat-completions"`
    /// | `"anthropic-messages"` | `"google-generate-content"` | …).
    ///
    /// **Display/diagnostics only.** No surface may branch on this string to
    /// decide which knobs exist — whether a route exposes depth or a thinking
    /// switch is carried by [`Self::effort`] / [`Self::thinking`] being `None`,
    /// resolved server-side from the provider's own declaration (ADR-0126).
    pub protocol: String,
    /// The **effective reasoning depth** this route would stamp, already
    /// resolved server-side (stored choice clamped onto the declared ladder,
    /// else the provider's declared default). `None` when the route exposes no
    /// depth knob.
    ///
    /// This is authoritative: a frontend renders it and never re-clamps or
    /// invents a fallback of its own.
    pub effort: Option<String>,
    /// The **effective extended-thinking switch**: `Some` only when the
    /// provider declares a separate on/off knob. `None` means the route has no
    /// switch, so no switch is rendered.
    pub thinking: Option<bool>,
    /// Reasoning effort tiers this channel/model supports, in ascending order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effort_levels: Vec<String>,
    /// Whether this model is favorited in the **Models** picker (ADR-0046 moved
    /// favorite from provider-level to per-model). A starred daily-driver model
    /// sorts into the second priority tier of the flat list wherever it is
    /// served. Added late, so it defaults to `false` on deserialize for older
    /// snapshots.
    #[serde(default)]
    pub favorite: bool,
    /// Unix epoch milliseconds of this model's last activation, or `None` if
    /// never activated. Drives the flat Models picker's "recent" section
    /// (recency-desc, most recently used first). `None`-defaulted so older
    /// snapshots deserialize as "never used".
    #[serde(default)]
    pub last_used_ms: Option<u64>,
    /// Effective image-input support for this route — the **full**
    /// capability resolution (user overrides over the remote advertisement
    /// over the static baseline), resolved server-side in
    /// `Channel::capabilities()`. Frontends gate image affordances (composer
    /// paste, vision-only tools) on this field instead of re-resolving the
    /// model in their own process: the client's static registry cannot see
    /// the server's fitted-model overlay or per-route overrides, so a
    /// client-side resolution would disagree with what the server actually
    /// routes.
    ///
    /// **Three-valued**: `Some(false)` means a layer declared that
    /// this route rejects images — the only value a frontend may gate on;
    /// `None` means *undeclared*, which older snapshots (and every route whose
    /// vendor advertises no vision field) deserialize to, so a frontend must
    /// treat it as "try it", never as text-only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vision: Option<bool>,
    /// Effective context window in tokens for this route — the **full**
    /// Capability resolution. Guaranteed > 0 for valid
    /// channels; 0 indicates an unresolved fallback.
    #[serde(default)]
    pub context_window: usize,
    /// Maximum output generation tokens for this route when declared or overridden.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    /// The **effective** availability verdict for this route:
    /// the provider's declaration, after any sovereign user override has been
    /// applied server-side. `None` means undeclared — a frontend must treat it
    /// as usable, never as disabled. `Some(usable:false)` is the one value a
    /// picker may dim and refuse.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub availability: Option<Availability>,
    /// Set when the provider declared the model unusable but the user's own
    /// scope (inject) overrode it. The row is usable yet must disclose the
    /// upstream verdict it contradicts (`[INV-AVAIL-05]`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub availability_overridden: bool,
    /// The provider's listing intent. `Some(false)` means the
    /// provider does not want this model offered in a listing; `None` means
    /// undeclared and therefore listed. Independent of `availability`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub advertised: Option<bool>,
    /// Set when this row's `availability` was observed **before a refresh that
    /// has since failed**, so the verdict may already have been reversed
    /// upstream. The declaration is still enforced (a failed refresh never
    /// widens access), but the surface must not present it as freshly
    /// confirmed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub availability_stale: bool,
}

/// Full snapshot of provider-picker state: which provider is the current
/// default plus one row per known provider. Sent on startup and after any
/// mutation (favorite toggle, default change, provider switch) so the TUI
/// always renders from a fresh, consistent picture rather than merging
/// incremental updates.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProviderPickerSnapshot {
    /// Canonical id of the active/default connection. Matches
    /// `config.default_connection`.
    pub default_id: String,
    pub rows: Vec<ProviderPickerRow>,
}

pub type ConnectionPickerSnapshot = ProviderPickerSnapshot;
