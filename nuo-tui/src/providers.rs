//! Snapshot-driven provider/model picker filter & sort logic.
//!
//! The pickers render directly from [`nuo_wire::ProviderPickerSnapshot`] — one
//! [`nuo_wire::ProviderPickerRow`] per provider the harness knows how to
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

use nuo_wire::{ConnectionAuth, ProviderModelInfo, ProviderPickerSnapshot, WireProtocol};

use crate::fuzzy;

/// One editable field of the provider editor. The visible set is chosen by the
/// active [`ConnectionTemplate`] (create) or the edited connection's provider
/// (edit), rather than a fixed five-field form. Provider-owned model collections are
/// imported from `nuo_wire`; this view layer only selects and renders
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
    ClientIdentity,
}

/// A curated starting point for adding a connection — the **creation-time
/// template** of ADR-0201. A template is consumed when the connection is
/// created and never persisted as identity; the created connection records
/// only the [`Self::id`] as its `provider` (a model provider names a service
/// surface, ADR-0201 INV-1). Curated templates lock the wire protocol and seed
/// their model list; the standalone `custom` template exposes protocol, model,
/// and request identity in its editor. Modelled as *data* — one table entry per
/// template — mirroring `nuo_wire::model_providers`.
pub struct ConnectionTemplate {
    /// The **model provider id** this template creates a connection for
    /// (`"openai"`, `"openai-subscription"`, `"custom"`, …). MUST match the
    /// matching `nuo_provider_adapters::model_provider_spec` id 1:1 and never change
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
    pub auth: nuo_wire::ConnectionAuth,
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
        if self.auth.is_oauth() {
            return vec![CustomField::Name];
        }
        if self.id == CUSTOM_TEMPLATE.id {
            return vec![
                CustomField::Name,
                CustomField::BaseUrl,
                CustomField::Token,
                CustomField::Model,
                CustomField::Protocol,
                CustomField::ClientIdentity,
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
        self.auth.is_oauth()
    }
}

/// The connection templates offered when adding a connection, **sorted
/// alphabetically by title**. The chooser renders rows in this order and keys
/// `↑/↓` movement to it, so the declared order here IS the display order —
/// insert new entries at their sorted position, not at the end.
///
/// Every `id` is a **model provider id** (ADR-0201 §5) and is persisted on the
/// created connection as `provider`.
pub const PROVIDER_PRESETS: &[ConnectionTemplate] = &[
    ConnectionTemplate {
        id: "anthropic",
        label: "Anthropic",
        description: "Anthropic's official API for flagship Claude models with advanced reasoning; sign in with an Anthropic API key.",
        protocol: WireProtocol::AnthropicMessages,
        models: nuo_wire::model_providers::ANTHROPIC_BUILTIN_MODELS,
        needs_url: false,
        url_hint: "https://api.anthropic.com/v1/messages",
        needs_model: false,
        default_url: Some("https://api.anthropic.com/v1/messages"),
        user_agent: None,
        auth: nuo_wire::ConnectionAuth::ApiKey,
    },
    ConnectionTemplate {
        id: "chatgpt-plan",
        label: "ChatGPT Plan",
        description: "Uses your ChatGPT Plus or Pro subscription for Codex and flagship GPT models; authorizes in the browser, no API key.",
        protocol: WireProtocol::Responses,
        models: nuo_wire::model_providers::CHATGPT_BUILTIN_MODELS,
        needs_url: false,
        url_hint: "https://chatgpt.com/backend-api/codex/responses",
        needs_model: false,
        default_url: Some("https://chatgpt.com/backend-api/codex/responses"),
        user_agent: None,
        auth: nuo_wire::ConnectionAuth::subscription_const("chatgpt-plan"),
    },
    ConnectionTemplate {
        id: "commandcode-plan",
        label: "CommandCode Plan",
        description: "Command Code Provider API serving Claude, GPT, DeepSeek, and top open models; sign in with your Command Code key.",
        protocol: WireProtocol::ChatCompletions,
        models: nuo_wire::model_providers::COMMANDCODE_BUILTIN_MODELS,
        needs_url: false,
        url_hint: "https://api.commandcode.ai/provider/v1/chat/completions",
        needs_model: false,
        default_url: Some("https://api.commandcode.ai/provider/v1/chat/completions"),
        user_agent: None,
        auth: nuo_wire::ConnectionAuth::ApiKey,
    },
    ConnectionTemplate {
        id: "deepseek",
        label: "DeepSeek",
        description: "DeepSeek's platform API with high-performance reasoning and coding models; sign in with a DeepSeek API key.",
        protocol: WireProtocol::Responses,
        models: nuo_wire::model_providers::DEEPSEEK_BUILTIN_MODELS,
        needs_url: false,
        url_hint: "https://api.deepseek.com/v1/responses",
        needs_model: false,
        default_url: Some("https://api.deepseek.com/v1/responses"),
        user_agent: None,
        auth: nuo_wire::ConnectionAuth::ApiKey,
    },
    ConnectionTemplate {
        id: "github-copilot",
        label: "GitHub Copilot",
        description: "Your GitHub Copilot subscription, serving multi-vendor coding and reasoning models; authorizes on the device via GitHub.",
        protocol: WireProtocol::ChatCompletions,
        models: nuo_wire::model_providers::COPILOT_SEED_MODELS,
        needs_url: false,
        url_hint: "https://api.githubcopilot.com/chat/completions",
        needs_model: false,
        default_url: Some("https://api.githubcopilot.com/chat/completions"),
        user_agent: None,
        auth: nuo_wire::ConnectionAuth::subscription_const("copilot"),
    },
    ConnectionTemplate {
        id: "google",
        label: "Google AI Studio",
        description: "Google AI Studio / developer API covering the full Gemini range; sign in with a Google API key.",
        protocol: WireProtocol::GoogleGemini,
        models: nuo_wire::model_providers::GOOGLE_BUILTIN_MODELS,
        needs_url: false,
        url_hint: "https://generativelanguage.googleapis.com/v1beta",
        needs_model: false,
        default_url: Some("https://generativelanguage.googleapis.com/v1beta"),
        user_agent: None,
        auth: nuo_wire::ConnectionAuth::ApiKey,
    },
    ConnectionTemplate {
        id: "google-antigravity",
        label: "Google Antigravity",
        description: "Your Google One AI Premium subscription for flagship Gemini plus companion Claude models; authorizes in the browser.",
        protocol: WireProtocol::GoogleGemini,
        models: nuo_wire::model_providers::ANTIGRAVITY_OAUTH_MODELS,
        needs_url: false,
        url_hint: "https://daily-cloudcode-pa.googleapis.com",
        needs_model: false,
        default_url: Some("https://daily-cloudcode-pa.googleapis.com"),
        user_agent: Some(nuo_wire::client_identity::ANTIGRAVITY_USER_AGENT),
        auth: nuo_wire::ConnectionAuth::subscription_const("google-antigravity"),
    },
    ConnectionTemplate {
        id: "kimi-code",
        label: "Kimi Code",
        description: "Moonshot's Kimi Coding Plan with long-context coding and reasoning models; sign in with a plan API key.",
        protocol: WireProtocol::ChatCompletions,
        models: nuo_wire::model_providers::KIMI_CODE_MODELS,
        needs_url: false,
        url_hint: "https://api.kimi.com/coding/v1/chat/completions",
        needs_model: false,
        default_url: Some("https://api.kimi.com/coding/v1/chat/completions"),
        user_agent: Some(nuo_wire::client_identity::OPENCODE_USER_AGENT),
        auth: nuo_wire::ConnectionAuth::ApiKey,
    },
    ConnectionTemplate {
        id: "openai",
        label: "OpenAI Platform",
        description: "OpenAI's platform API for official flagship GPT and frontier reasoning models; sign in with an OpenAI API key.",
        protocol: WireProtocol::ChatCompletions,
        models: nuo_wire::model_providers::OPENAI_BUILTIN_MODELS,
        needs_url: false,
        url_hint: "https://api.openai.com/v1/chat/completions",
        needs_model: false,
        default_url: Some("https://api.openai.com/v1/chat/completions"),
        user_agent: None,
        auth: nuo_wire::ConnectionAuth::ApiKey,
    },
    ConnectionTemplate {
        id: "opencode",
        label: "OpenCode",
        description: "OpenCode Console account for coding and agent models; sign in with your OpenCode account.",
        protocol: WireProtocol::ChatCompletions,
        models: nuo_wire::model_providers::OPENCODE_CONSOLE_MODELS,
        needs_url: false,
        url_hint: "https://opencode.ai/inference/openai/v1/chat/completions",
        needs_model: false,
        default_url: Some("https://opencode.ai/inference/openai/v1/chat/completions"),
        user_agent: Some(nuo_wire::client_identity::OPENCODE_USER_AGENT),
        auth: nuo_wire::ConnectionAuth::subscription_const("opencode"),
    },
    ConnectionTemplate {
        id: "opencode-plan",
        label: "OpenCode Plan",
        description: "OpenCode Plan subscription for open coding models; sign in with your OpenCode Plan API key.",
        protocol: WireProtocol::ChatCompletions,
        models: nuo_wire::model_providers::OPENCODE_GO_MODELS,
        needs_url: false,
        url_hint: "https://opencode.ai/zen/go/v1/chat/completions",
        needs_model: false,
        default_url: Some("https://opencode.ai/zen/go/v1/chat/completions"),
        user_agent: Some(nuo_wire::client_identity::OPENCODE_USER_AGENT),
        auth: nuo_wire::ConnectionAuth::ApiKey,
    },
    ConnectionTemplate {
        id: "opencode-zen",
        label: "OpenCode Zen",
        description: "OpenCode Zen relay with pay-as-you-go billing for frontier coding models; sign in with your OpenCode API key.",
        protocol: WireProtocol::ChatCompletions,
        models: nuo_wire::model_providers::OPENCODE_ZEN_MODELS,
        needs_url: false,
        url_hint: "https://opencode.ai/zen/v1/chat/completions",
        needs_model: false,
        default_url: Some("https://opencode.ai/zen/v1/chat/completions"),
        user_agent: Some(nuo_wire::client_identity::OPENCODE_USER_AGENT),
        auth: nuo_wire::ConnectionAuth::ApiKey,
    },
    ConnectionTemplate {
        id: "openrouter",
        label: "OpenRouter",
        description: "OpenRouter's unified gateway for Nex and hundreds of other models; sign in with an OpenRouter API key.",
        protocol: WireProtocol::ChatCompletions,
        models: nuo_wire::model_providers::OPENROUTER_BUILTIN_MODELS,
        needs_url: false,
        url_hint: "https://openrouter.ai/api/v1/chat/completions",
        needs_model: false,
        default_url: Some("https://openrouter.ai/api/v1/chat/completions"),
        user_agent: None,
        auth: nuo_wire::ConnectionAuth::ApiKey,
    },
    ConnectionTemplate {
        id: "qianwen",
        label: "QianwenAI Token Plan",
        description: "Alibaba's QianwenAI Platform Token Plan serving Qwen, DeepSeek, GLM, and Kimi over one Credits-billed key; sign in with your plan API key (sk-sp-…).",
        protocol: WireProtocol::ChatCompletions,
        models: nuo_wire::model_providers::QIANWEN_BUILTIN_MODELS,
        needs_url: false,
        url_hint: "https://token-plan.maas.qianwenaiapi.com/compatible-mode/v1/chat/completions",
        needs_model: false,
        default_url: Some(
            "https://token-plan.maas.qianwenaiapi.com/compatible-mode/v1/chat/completions",
        ),
        user_agent: None,
        auth: nuo_wire::ConnectionAuth::ApiKey,
    },
    ConnectionTemplate {
        id: "qoder",
        label: "Qoder",
        description: "Alibaba's Qoder subscription for Qoder3 and Qwen coding models with COSY-signed inference; paste a personal access token (pt-…) or authorize via device flow.",
        protocol: WireProtocol::ChatCompletions,
        models: nuo_wire::model_providers::QODER_MODELS,
        needs_url: false,
        url_hint: "https://api3.qoder.sh/algo/api/v2/service/pro/sse/agent_chat_generation",
        needs_model: false,
        default_url: Some("https://api3.qoder.sh"),
        user_agent: None,
        auth: nuo_wire::ConnectionAuth::subscription_const("qoder"),
    },
    ConnectionTemplate {
        id: "glm-cn",
        label: "ZAI Code (CN)",
        description: "Zhipu's Z.AI Coding Plan with flagship GLM and code-enhanced models; sign in with a plan API key.",
        protocol: WireProtocol::ChatCompletions,
        models: nuo_wire::model_providers::ZAI_CODE_MODELS,
        needs_url: false,
        url_hint: "https://open.bigmodel.cn/api/coding/paas/v4/chat/completions",
        needs_model: false,
        default_url: Some("https://open.bigmodel.cn/api/coding/paas/v4/chat/completions"),
        user_agent: Some(nuo_wire::client_identity::ZCODE_USER_AGENT),
        auth: nuo_wire::ConnectionAuth::ApiKey,
    },
    ConnectionTemplate {
        id: "xai",
        label: "xAI",
        description: "Your SuperGrok or X Premium subscription for flagship Grok reasoning models; authorizes in the browser.",
        protocol: WireProtocol::ChatCompletions,
        models: nuo_wire::model_providers::XAI_BUILTIN_MODELS,
        needs_url: false,
        url_hint: "https://api.x.ai/v1/chat/completions",
        needs_model: false,
        default_url: Some("https://api.x.ai/v1/chat/completions"),
        user_agent: None,
        auth: nuo_wire::ConnectionAuth::subscription_const("xai"),
    },
];

/// The `custom` provider's template — the generic bring-your-own-endpoint
/// definition (ADR-0201 §6). It is intentionally separate from
/// [`PROVIDER_PRESETS`]: the Connections surface exposes "Add curated
/// connection" and "Add custom connection" as sibling actions, so a custom
/// endpoint is never presented as though it were a curated service.
///
/// `custom` is an ordinary model provider with an open model universe; its
/// default wire is `chat-completions` and a connection may override
/// protocol, base URL, and user agent.
pub const CUSTOM_TEMPLATE: ConnectionTemplate = ConnectionTemplate {
    id: "custom",
    label: "Custom connection",
    description: "Any endpoint you bring — a custom gateway, local runtime, or relay; you set the base URL, protocol, and key.",
    protocol: WireProtocol::ChatCompletions,
    models: &[],
    needs_url: true,
    url_hint: "https://relay.example.com/v1/chat/completions",
    needs_model: true,
    default_url: None,
    user_agent: None,
    auth: nuo_wire::ConnectionAuth::ApiKey,
};

/// Resolve either a curated template or the standalone `custom` template by
/// its **provider id**.
pub fn connection_definition(provider: &str) -> Option<&'static ConnectionTemplate> {
    if provider == CUSTOM_TEMPLATE.id {
        return Some(&CUSTOM_TEMPLATE);
    }
    PROVIDER_PRESETS.iter().find(|t| t.id == provider)
}

/// The editor header title for a create-mode connection — the label of the
/// template the flow was seeded from, falling back to a generic header. The
/// lookup is by **provider id**, not wire protocol: several providers share the
/// `openai` wire, and a first-match-by-protocol lookup would mislabel the
/// editor (e.g. "ChatGPT Subscription" for the OpenAI Platform flow).
pub fn provider_label_for(provider: Option<&str>) -> String {
    provider
        .and_then(connection_definition)
        .map(|t| t.label.to_string())
        .unwrap_or_else(|| "＋ Add connection".to_string())
}

/// Resolve the provider **type** label for a Connections row from its
/// `provider` id — e.g. `provider = "openai"` → `"OpenAI Platform"`. This is
/// the service surface shown beside the user-given connection name (distinct
/// from the connection name itself). Returns `None` for a provider the local
/// template table does not know, in which case the row renders the connection
/// name alone.
pub fn provider_type_label(provider: &str) -> Option<&'static str> {
    if provider.is_empty() {
        return None;
    }
    connection_definition(provider).map(|t| t.label)
}

/// The ordered editor fields shown when **editing** an existing connection.
/// For the `custom` provider the form offers Name, Base URL, and Token (the
/// Model field is omitted — models, and their per-model reasoning, ADR-0046,
/// are managed in the Models picker). For a curated provider (where endpoint
/// and models are owned by the provider spec), Base URL is fixed by the
/// provider, so only Name and Token are offered. For an OAuth connection
/// (ChatGPT/Codex, xAI, Copilot, Antigravity) only Name is editable: the Base
/// URL and Token are fixed by the auth flow and must not be hand-edited, so a
/// rename is the only safe operation.
pub fn edit_fields(curated: bool, auth: ConnectionAuth) -> Vec<CustomField> {
    if auth.is_oauth() {
        vec![CustomField::Name]
    } else if curated {
        vec![CustomField::Name, CustomField::Token]
    } else {
        vec![
            CustomField::Name,
            CustomField::BaseUrl,
            CustomField::Token,
            CustomField::Protocol,
            CustomField::ClientIdentity,
        ]
    }
}

/// Whether a protocol's model set is *closed*: the candidate list is the full,
/// fixed set and the add-model overlay must NOT offer a free-text fallback.
/// OpenAI and Anthropic relays serve an open, evolving model set, so
/// typing an unlisted id is legitimate; native Google is a closed family — its
/// models are enumerated by Google and forwarded verbatim by relays, so an
/// arbitrary id is almost certainly a typo or hallucination, not a real model.
#[cfg(test)]
pub fn protocol_model_set_closed(protocol_wire: &str) -> bool {
    protocol_wire == WireProtocol::GoogleGemini.as_str()
}

/// The registry model ids matching a wire protocol. Kept as a test helper for
/// registry and template consistency; the custom connection editor
/// intentionally does not use this list.
#[cfg(test)]
fn protocol_model_candidates(protocol_wire: &str) -> Vec<&'static str> {
    let Ok(protocol) = protocol_wire.parse::<WireProtocol>() else {
        return Vec::new();
    };
    let mut seen = std::collections::HashSet::new();
    nuo_wire::baseline_models()
        .filter(|m| m.protocol == protocol)
        .map(|m| m.id)
        // Deduplicate: a model id can appear in multiple provider tables (e.g.
        // gpt-4o-mini in both `openai` and `copilot`), and inventory iteration
        // order is not guaranteed, so without dedup the candidate list — and
        // thus the first-match the picker commits — would be non-deterministic.
        .filter(|id| seen.insert(*id))
        .collect()
}

/// One selectable row in the **flat model picker** (the `Models` dialog,
/// ADR-0205; equivalent): a single (provider, model) pair drawn from anywhere
/// in the snapshot. Built by [`models_flat_filtered_from`]; the picker
/// browses, searches, and activates these directly — there is no drill-in
/// stage.
#[derive(Clone, Debug)]
pub struct RankedModel {
    /// The section this row belongs to (Favorites, Recent, or All).
    pub section: ModelSection,
    /// Canonical id of the provider serving this model (its snapshot row id).
    pub provider_id: String,
    /// Wire model id to activate — the identity, and the string that goes on
    /// the wire, appears in `hidden_models`/favorites/route settings, and is
    /// what the user types in config. It is the *fallback* label: a row leads
    /// with [`Self::name`] when the provider publishes one, and with this id
    /// when it does not. Either way the id stays searchable.
    pub model: String,
    /// The provider's own human-readable label for this model (`DeepSeek V4.1
    /// Flash` for the wire id `deepseek-flash`), when the catalog advertises
    /// one.
    ///
    /// Optional by construction and **never** the identity: it is the row's
    /// primary label when present (so the list reads in the provider's own
    /// vocabulary), while the wire id rides along beside it so the string the
    /// user must actually type stays learnable. `None` — the common case, since
    /// stock OpenAI-compatible and Gemini catalogues publish nothing — renders
    /// the bare id exactly as before.
    pub name: Option<String>,
    /// The provider's display name, rendered as the dim `· <provider>` suffix
    /// so identical model ids served by different instances stay
    /// distinguishable in the flat list.
    pub provider_label: String,
    /// Model-specific controls surfaced by the picker snapshot. OpenAI rows
    /// can expose effort; Anthropic rows can expose effort plus thinking.
    pub effort: Option<String>,
    pub thinking: Option<bool>,
    /// Reasoning effort tiers this model supports, in ascending order
    /// (mirrors `ProviderModelInfo.effort_levels`).
    pub effort_levels: Vec<String>,
    /// Whether this model is favorited (mirrors the snapshot's per-model
    /// `favorite` flag; ADR-0046). A starred daily-driver model sorts into
    /// the leading **Favorites** section of the flat list wherever it is
    /// served and shows a `★` glyph.
    pub favorite: bool,
    /// Unix epoch ms of this model's last activation (`None` = never used).
    /// A model with usage history sorts into the **Recent** section
    /// (most-recently-used first).
    pub last_used_ms: Option<u64>,
    /// Context window limit in tokens (ADR-0182).
    pub context_window: usize,
    /// Whether this model may be run on this account right now (ADR-0273).
    /// `false` renders the row greyed-out and refuses activation. Undeclared
    /// availability — the common case — is always usable.
    pub usable: bool,
    /// The provider's own reason for an unusable verdict, verbatim. `None`
    /// means the provider declared none; the surface then says only that the
    /// model is unavailable rather than guessing a cause. Never parsed.
    pub locked_reason: Option<String>,
    /// The provider declared the model unusable but the user's own scope
    /// overrode it: the row is usable and must disclose the contradiction
    /// (`[INV-AVAIL-05]`).
    pub availability_overridden: bool,
    /// The availability verdict was observed before a refresh that has since
    /// failed, so it may already be out of date upstream. The row says so
    /// rather than presenting a stale verdict as freshly confirmed (ADR-0273).
    pub availability_stale: bool,
    /// Subsequence match against the wire model ID (`model`). The renderer
    /// highlights the ID column with these positions, and uses them for the
    /// leading label column only when the row has no separate name to draw.
    pub match_id: Option<fuzzy::FuzzyMatch>,
    /// Subsequence match against the human-readable model name (`name`), if
    /// present — the positions that highlight the leading label column.
    pub match_name: Option<fuzzy::FuzzyMatch>,
    /// Subsequence match against the provider connection label
    /// (`provider_label`).
    pub match_connection: Option<fuzzy::FuzzyMatch>,
}

impl RankedModel {
    /// The row's list section.
    pub fn section(&self) -> ModelSection {
        self.section
    }
}

/// The three sections of the flat Models picker list, in display order.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum ModelSection {
    /// ★-favorited models (ADR-0046) — pinned user intent leads the list.
    Favorites,
    /// Models with usage history, most recently used first.
    Recent,
    /// Every remaining (provider, model) pair, ASCII by model id.
    All,
}

impl ModelSection {
    /// The section's label row: `FAVORITES`, `RECENT`, `ALL MODELS`. Rendered
    /// as a dim uppercase tag (the same section-tag voice as the chrome
    /// labels, e.g. the todo bar's `TODOS`).
    pub fn label(self) -> &'static str {
        match self {
            ModelSection::Favorites => "FAVORITES",
            ModelSection::Recent => "RECENT",
            ModelSection::All => "ALL MODELS",
        }
    }
}

/// One selectable row in the **Connections** provider list. Carries everything
/// the renderer and input handler need (copied out of the snapshot row), so
/// neither re-indexes the snapshot.
pub struct RankedProvider {
    /// Canonical provider id.
    pub id: String,
    /// Display name (the fuzzy target; mirrors `label`).
    pub name: String,
    /// Active model wire id.
    pub model: String,
    /// Every model id this provider serves.
    pub models: Vec<String>,
    /// `true` for curated providers, `false` for user-defined connections. Drives
    /// the built-in/custom grouping and whether `e` opens the full meta editor.
    pub builtin: bool,
    /// The model provider this connection points at (`"openai"`, …),
    /// surfaced so the Connections list can show the provider *type* beside
    /// the connection name (ADR-0201).
    pub provider: String,
    /// Client identity configured for this connection.
    pub client_identity: nuo_wire::ClientIdentity,
    /// The rendered label — the provider's display name (the instance name).
    pub label: String,
    /// The fuzzy match against `label`, or `None` in browse mode (empty query).
    pub m: Option<fuzzy::FuzzyMatch>,
}

impl RankedProvider {
    /// Whether the provider hosts more than one model. Informational for the
    /// Connections list (the flat Models picker lists each pair individually,
    /// so no drill-in remains).
    #[cfg(test)]
    pub fn is_multi_model(&self) -> bool {
        self.models.len() > 1
    }
}

/// The last-used-desc → name ordering of the Connections provider list. Pulls
/// each provider's recency signal from its snapshot row. (Favorite is
/// model-level now — ADR-0046 — so the Connections list no longer sorts by it.)
fn provider_order(
    picker: &ProviderPickerSnapshot,
    a_id: &str,
    b_id: &str,
    a_name: &str,
    b_name: &str,
) -> std::cmp::Ordering {
    let used = |id: &str| {
        picker
            .rows
            .iter()
            .find(|r| r.id == id)
            .and_then(|r| r.last_used_ms)
    };
    let a_used = used(a_id);
    let b_used = used(b_id);
    b_used.cmp(&a_used).then_with(|| a_name.cmp(b_name))
}

/// Build the **Connections** provider rows: one per snapshot row,
/// fuzzy-filtered by `query` against the provider (instance) name and sorted
/// last-used → name. An empty `query` (browse mode) keeps every provider with
/// no match positions.
pub fn providers_filtered_from(
    picker: &ProviderPickerSnapshot,
    query: &str,
) -> Vec<RankedProvider> {
    let mut rows: Vec<RankedProvider> = Vec::new();
    for prow in picker.rows.iter() {
        let label = prow.name.clone();
        let m = if query.is_empty() {
            None
        } else {
            match fuzzy::fuzzy_match(&label, query) {
                Some(m) => Some(m),
                None => continue,
            }
        };
        rows.push(RankedProvider {
            id: prow.id.clone(),
            name: prow.name.clone(),
            model: prow.model.clone(),
            models: prow.models.clone(),
            builtin: prow.builtin,
            provider: prow.provider.clone(),
            client_identity: prow.client_identity.clone(),
            label,
            m,
        });
    }
    rows.sort_by(|a, b| provider_order(picker, &a.id, &b.id, &a.name, &b.name));
    rows
}

/// Build the **flat Models** rows: [`RankedModel`] rows sectioned into three labeled groups:
///
/// 1. **Favorites** — ★-marked models (ADR-0046), ASCII by model id;
/// 2. **Recent** — models with usage history, most recently used first
///    (recency-desc, ASCII id as the tiebreaker);
/// 3. **All models** — ALL available models across ready providers (including favorites
///    and recent models), ASCII by model id (provider label as the stable tiebreaker).
///
/// Fuzzy filtering matches `query` against the model's **rendered label** — the
/// provider's own name for it when the catalog advertises one, else the wire id.
/// When the label does not match but the wire id behind it does, the row is kept
/// unhighlighted (`m = None`); likewise when the PROVIDER name fuzzy-matches, all
/// of that provider's models are included unhighlighted, so "show me everything
/// Anthropic serves" works from the same search box. Match positions always index
/// onto the rendered label's characters only, because that is what is drawn. The
/// sectioned ordering is applied in
/// search mode too, so filtered results keep the same visual grouping.
pub fn models_flat_filtered_from(
    picker: &ProviderPickerSnapshot,
    current_provider: &str,
    current_model: &str,
    query: &str,
) -> Vec<RankedModel> {
    let _ = (current_provider, current_model);
    let mut candidates: Vec<RankedModel> = Vec::new();
    for prow in &picker.rows {
        // Daily-driver model picker only shows models from ready/authenticated connections.
        if !prow.key_ready {
            continue;
        }
        // Connection/provider match for this provider row
        let conn_match = if query.is_empty() {
            None
        } else {
            fuzzy::fuzzy_match(&prow.name, query)
        };
        for model in &prow.models {
            let info = prow
                .model_info
                .iter()
                .find(|info| info.model == *model)
                .cloned()
                .unwrap_or_else(|| ProviderModelInfo {
                    model: model.clone(),
                    ..ProviderModelInfo::default()
                });
            // Multi-field fuzzy matching: model id, model display name, and connection label.
            let (match_id, match_name, match_connection) = if query.is_empty() {
                (None, None, None)
            } else {
                let id_m = fuzzy::fuzzy_match(model, query);
                let name_m = info
                    .name
                    .as_deref()
                    .and_then(|name| fuzzy::fuzzy_match(name, query));
                let conn_m = conn_match.clone();

                if id_m.is_none() && name_m.is_none() && conn_m.is_none() {
                    continue;
                }
                (id_m, name_m, conn_m)
            };

            candidates.push(RankedModel {
                section: ModelSection::All,
                provider_id: prow.id.clone(),
                model: model.clone(),
                name: info.name.clone(),
                provider_label: prow.name.clone(),
                effort: info.effort,
                thinking: info.thinking,
                // The daemon already resolved this route's ladder (ADR-0149:
                // baseline ⊕ remote ⊕ user overrides) and shipped it on the
                // snapshot. It must ride through to the editor: the client
                // cannot re-resolve it, because `nuo-providers` (which owns
                // the baseline tables) is not linked into this binary — the
                // registries live in `nuo-wire`, whose baselines are
                // populated by the daemon's provider crates.
                effort_levels: info.effort_levels,
                favorite: info.favorite,
                last_used_ms: info.last_used_ms,
                context_window: info.context_window,
                usable: info
                    .availability
                    .as_ref()
                    .is_none_or(|availability| availability.usable),
                locked_reason: info
                    .availability
                    .as_ref()
                    .filter(|availability| !availability.usable)
                    .and_then(|availability| availability.reason.clone()),
                availability_overridden: info.availability_overridden,
                availability_stale: info.availability_stale,
                match_id,
                match_name,
                match_connection,
            });
        }
    }

    // 1. Favorites section: starred models, sorted by ASCII model id then provider label.
    let mut favorites: Vec<RankedModel> = candidates
        .iter()
        .filter(|r| r.favorite)
        .cloned()
        .map(|mut r| {
            r.section = ModelSection::Favorites;
            r
        })
        .collect();
    favorites.sort_by(|a, b| {
        a.model
            .cmp(&b.model)
            .then_with(|| a.provider_label.cmp(&b.provider_label))
    });

    // 2. Recent section: models with usage history, sorted by recency desc, then ASCII model id, then provider label.
    let mut recent: Vec<RankedModel> = candidates
        .iter()
        .filter(|r| r.last_used_ms.is_some())
        .cloned()
        .map(|mut r| {
            r.section = ModelSection::Recent;
            r
        })
        .collect();
    recent.sort_by(|a, b| {
        let a_used = a.last_used_ms.unwrap_or(0);
        let b_used = b.last_used_ms.unwrap_or(0);
        b_used
            .cmp(&a_used)
            .then_with(|| a.model.cmp(&b.model))
            .then_with(|| a.provider_label.cmp(&b.provider_label))
    });

    // 3. All models section: contains EVERY candidate model across ready providers.
    let mut all: Vec<RankedModel> = candidates
        .into_iter()
        .map(|mut r| {
            r.section = ModelSection::All;
            r
        })
        .collect();
    all.sort_by(|a, b| {
        a.model
            .cmp(&b.model)
            .then_with(|| a.provider_label.cmp(&b.provider_label))
    });

    let mut rows = Vec::with_capacity(favorites.len() + recent.len() + all.len());
    rows.extend(favorites);
    rows.extend(recent);
    rows.extend(all);
    rows
}

/// One **body line** of the flat Models list: either a selectable row or a
/// dim section label. The body the renderer paints is
/// [`models_body_lines`] — `row_index` addresses into it skip the label rows
/// (the selection cursor is a *row* cursor, not a *line* cursor, so ↑/↓ can
/// never land on a label).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ModelBodyLine {
    /// A selectable row — the payload is the row's index into the flat
    /// [`RankedModel`] slice (the same slice `modal_index` addresses).
    Row(usize),
    /// A dim section label — the payload is the section being announced.
    /// Never selectable.
    Section(ModelSection),
}

/// Map every selectable row index to its body-line index, inserting a
/// [`ModelBodyLine::Section`] label before each non-empty section in display
/// order. Rows whose section is empty produce no lines at all, so an empty
/// Favorites section, for instance, renders no `FAVORITES` header. The
/// returned vector's length is the body's total line count (what the modal's
/// scroll math must use); `row_line[i]` is where flat row `i` paints.
///
/// Blank row between sections is *not* included here — the renderer adds one
/// spacer line before every section label after the first (see
/// `model_list_body`), keeping this mapping pure row/label geometry.
pub fn models_body_lines(models: &[RankedModel]) -> (Vec<ModelBodyLine>, Vec<usize>) {
    let mut lines: Vec<ModelBodyLine> = Vec::with_capacity(models.len() + 3);
    let mut row_line: Vec<usize> = Vec::with_capacity(models.len());
    // Sections arrive in display order because `models_flat_filtered_from`
    // sorts by `ModelSection` first — walk the boundary transitions.
    let mut current: Option<ModelSection> = None;
    for (i, rm) in models.iter().enumerate() {
        let section = rm.section();
        if current != Some(section) {
            lines.push(ModelBodyLine::Section(section));
            current = Some(section);
        }
        row_line.push(lines.len());
        lines.push(ModelBodyLine::Row(i));
    }
    (lines, row_line)
}

#[cfg(test)]
mod tests {
    extern crate nuo_provider_adapters;
    use super::*;
    use nuo_wire::ProviderPickerRow;

    fn row(id: &str, name: &str, models: &[&str], builtin: bool) -> ProviderPickerRow {
        ProviderPickerRow {
            id: id.to_string(),
            name: name.to_string(),
            model: models.first().copied().unwrap_or("").to_string(),
            models: models.iter().map(|m| m.to_string()).collect(),
            model_info: Vec::new(),
            builtin,
            protocol: String::new(),
            base_url: String::new(),
            key_ready: true,
            provider: String::new(),
            client_identity: Default::default(),
            last_used_ms: None,
            auth: Default::default(),
        }
    }

    fn sample() -> ProviderPickerSnapshot {
        ProviderPickerSnapshot {
            default_id: "openai".to_string(),
            rows: vec![
                row("kimi-code", "Kimi Code", &["kimi-k2.7-code"], true),
                row("openai", "OpenAI", &["gpt-4o", "gpt-4o-mini"], true),
                row(
                    "anthropic",
                    "Anthropic",
                    &[
                        "claude-fable-5",
                        "claude-sonnet-5",
                        "claude-opus-4-8",
                        "claude-sonnet-4-6",
                    ],
                    true,
                ),
                row("my-relay", "My Relay", &["glm-5.2", "glm-5.1"], false),
            ],
        }
    }

    /// Build a `ProviderModelInfo` with everything neutral — no favorite, no
    /// recency, no reasoning knobs.
    fn info(model: &str) -> ProviderModelInfo {
        ProviderModelInfo {
            model: model.to_string(),
            name: None,
            protocol: String::new(),
            effort: None,
            thinking: None,
            effort_levels: Vec::new(),
            favorite: false,
            last_used_ms: None,
            vision: Some(false),
            context_window: 128_000,
            max_output_tokens: None,
            availability: None,

            availability_overridden: false,

            advertised: None,
            availability_stale: false,
        }
    }

    /// The sample with one favorited model (`claude-sonnet-5`) and two with
    /// usage history — `glm-5.1` used most recently (t=2000), `gpt-4o` older
    /// (t=1000) — so all three sections are populated and the RECENT section
    /// has a meaningful internal order.
    fn sectioned() -> ProviderPickerSnapshot {
        let mut snapshot = sample();
        for prow in &mut snapshot.rows {
            let (id, info) = match prow.id.as_str() {
                "anthropic" => ("anthropic", info("claude-sonnet-5")),
                "openai" => {
                    let mut i = info("gpt-4o");
                    i.last_used_ms = Some(1_000);
                    ("openai", i)
                }
                "my-relay" => {
                    let mut i = info("glm-5.1");
                    i.last_used_ms = Some(2_000);
                    ("my-relay", i)
                }
                _ => continue,
            };
            assert_eq!(id, prow.id);
            prow.model_info = vec![info];
        }
        // The favorite flag lands after the match above (the borrow ends).
        for prow in &mut snapshot.rows {
            if prow.id == "anthropic" {
                prow.model_info[0].favorite = true;
            }
        }
        snapshot
    }

    #[test]
    fn flat_rows_show_the_raw_wire_id() {
        // The id-fallback half of the policy: with no provider label the row
        // leads with the raw wire id, and there is no client-side name mapping
        // to drift. (The label half is
        // `flat_rows_carry_the_provider_label_only_when_advertised`; either way
        // `model` is what gets activated.)
        let snapshot = sample();
        let rows = models_flat_filtered_from(&snapshot, "", "", "");
        let glm = rows
            .iter()
            .find(|r| r.model == "glm-5.2")
            .expect("relay pair present");
        assert_eq!(glm.provider_label, "My Relay");
        assert_eq!(glm.name, None, "nothing was advertised to label with");
        // The rendered label is the id itself (verified via the fuzzy match
        // target): matching "glm-5.2" hits positions inside the id.
        assert!(
            fuzzy::fuzzy_match(&glm.model, "glm52").is_some(),
            "the match target is the id"
        );
    }

    #[test]
    fn flat_sections_favorites_then_recent_then_all() {
        // Three-section ordering: Favorites lead, Recent (usage history,
        // most-recent-first) follow, All models contains all candidate models in ASCII order.
        // The current pair no longer pins to the top — it keeps its natural
        // section position in ALL MODELS.
        let snapshot = sectioned();
        let rows = models_flat_filtered_from(&snapshot, "my-relay", "glm-5.2", "");

        // Section boundaries: collect the section of each row and assert the
        // sequence is the display order with no interleaving.
        let sections: Vec<ModelSection> = rows.iter().map(|r| r.section()).collect();
        let mut first_all = sections.len();
        for (i, s) in sections.iter().enumerate() {
            match s {
                ModelSection::Favorites if i < 1 => {}
                ModelSection::Recent if (1..3).contains(&i) => {}
                ModelSection::All => {
                    first_all = first_all.min(i);
                }
                _ => panic!("unexpected section {s:?} at index {i}: {sections:?}"),
            }
        }
        assert_eq!(first_all, 3, "ALL MODELS starts after both lead sections");

        // Favorites section: exactly the starred model, first.
        assert!(rows[0].favorite);
        assert_eq!(rows[0].model, "claude-sonnet-5");
        assert_eq!(rows[0].section(), ModelSection::Favorites);

        // Recent section: glm-5.1 (t=2000) before gpt-4o (t=1000).
        assert_eq!(rows[1].model, "glm-5.1", "most recent first");
        assert_eq!(rows[2].model, "gpt-4o");
        assert_eq!(rows[1].last_used_ms, Some(2_000));
        assert_eq!(rows[2].last_used_ms, Some(1_000));
        assert_eq!(rows[1].section(), ModelSection::Recent);
        assert_eq!(rows[2].section(), ModelSection::Recent);

        // All models: plain ASCII, containing ALL 9 models (including favorites, recent, and current pair).
        assert_eq!(rows[first_all..].len(), 9);
        assert!(
            rows[first_all..]
                .iter()
                .all(|r| r.section() == ModelSection::All)
        );
        let rest: Vec<&str> = rows[first_all..].iter().map(|r| r.model.as_str()).collect();
        let mut sorted = rest.clone();
        sorted.sort();
        assert_eq!(rest, sorted);
        assert!(rest.contains(&"claude-sonnet-5"));
        assert!(rest.contains(&"glm-5.1"));
        assert!(rest.contains(&"gpt-4o"));
    }

    #[test]
    fn flat_recent_orders_by_recency_desc_ascii_tiebreak() {
        // Two models with the SAME recency fall back to ASCII id order, so
        // the section stays deterministic when timestamps collide.
        let mut snapshot = sample();
        for prow in &mut snapshot.rows {
            let ids: Vec<String> = prow.models.clone();
            prow.model_info = ids
                .iter()
                .map(|m| {
                    let mut i = info(m);
                    i.last_used_ms = Some(5_000);
                    i
                })
                .collect();
        }
        let rows = models_flat_filtered_from(&snapshot, "", "", "");
        // Every model is recent → RECENT section contains all models, and ALL MODELS section contains all models.
        let recent_rows: Vec<&RankedModel> = rows
            .iter()
            .filter(|r| r.section() == ModelSection::Recent)
            .collect();
        let all_rows: Vec<&RankedModel> = rows
            .iter()
            .filter(|r| r.section() == ModelSection::All)
            .collect();
        assert_eq!(recent_rows.len(), 9);
        assert_eq!(all_rows.len(), 9);
        let ids: Vec<&str> = recent_rows.iter().map(|r| r.model.as_str()).collect();
        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(ids, sorted);
    }

    #[test]
    fn flat_recent_isolates_same_model_across_different_connections() {
        // Two connection instances offer the exact same model (e.g. gpt-4o).
        // When only one connection has usage history for it, ONLY that
        // (connection, model) row must appear in RECENT. The other instance
        // must NOT be pulled into RECENT or elevated.
        let mut row_a = row("openai-official", "OpenAI Official", &["gpt-4o"], true);
        let mut info_a = info("gpt-4o");
        info_a.last_used_ms = Some(5_000);
        row_a.model_info = vec![info_a];

        let mut row_b = row("openai-proxy", "OpenAI Proxy", &["gpt-4o"], false);
        let info_b = info("gpt-4o");
        row_b.model_info = vec![info_b];

        let snapshot = ProviderPickerSnapshot {
            default_id: "openai-official".to_string(),
            rows: vec![row_a, row_b],
        };

        let rows = models_flat_filtered_from(&snapshot, "", "", "");
        let recent_rows: Vec<&RankedModel> = rows
            .iter()
            .filter(|r| r.section() == ModelSection::Recent)
            .collect();

        assert_eq!(
            recent_rows.len(),
            1,
            "only the activated connection row should be in RECENT"
        );
        assert_eq!(recent_rows[0].provider_id, "openai-official");
        assert_eq!(recent_rows[0].model, "gpt-4o");

        // In ALL MODELS, both instances still exist
        let all_rows: Vec<&RankedModel> = rows
            .iter()
            .filter(|r| r.section() == ModelSection::All)
            .collect();
        assert_eq!(all_rows.len(), 2);
    }

    #[test]
    fn flat_favorite_outranks_recency() {
        // Precedence: a favorite always wins over the recency signal —
        // favorites are pinned user intent, recency is emergent. A starred
        // model with NO usage history still leads a used-but-unstarred one.
        let snapshot = sectioned();
        let rows = models_flat_filtered_from(&snapshot, "", "", "");
        assert_eq!(
            rows[0].section(),
            ModelSection::Favorites,
            "the unstarred-but-recent glm-5.1 must not lead"
        );
        assert_eq!(rows[0].model, "claude-sonnet-5");
        assert_eq!(rows[1].section(), ModelSection::Recent);
    }

    #[test]
    fn flat_current_pair_keeps_its_section_not_the_top() {
        // The live (provider, model) pair is identified by its ● glyph, not
        // by list position any more: make the never-used glm-5.2 the current
        // pair — it stays in ALL MODELS at its ASCII position while the
        // favorite keeps the lead.
        let snapshot = sectioned();
        let rows = models_flat_filtered_from(&snapshot, "my-relay", "glm-5.2", "");
        assert_eq!(rows[0].model, "claude-sonnet-5", "favorite still leads");
        let current = rows
            .iter()
            .find(|r| r.provider_id == "my-relay" && r.model == "glm-5.2")
            .expect("current pair present");
        assert_eq!(
            current.section(),
            ModelSection::All,
            "current pair is not pinned to the top"
        );
    }

    #[test]
    fn flat_sections_survive_a_fuzzy_query() {
        // Search mode keeps the same grouping: filtered rows stay ordered
        // Favorites → Recent → All.
        let snapshot = sectioned();
        let rows = models_flat_filtered_from(&snapshot, "", "", "g");
        // Matches gpt-4o (recent) and glm-5.1/glm-5.2 (recent/plain).
        let sections: Vec<ModelSection> = rows.iter().map(|r| r.section()).collect();
        let mut ordered = sections.clone();
        ordered.sort();
        assert_eq!(sections, ordered, "sections never regress under a query");
    }

    #[test]
    fn body_lines_interleave_labels_and_rows() {
        // The body geometry: a section label precedes each non-empty section,
        // rows keep their flat index, and empty sections emit nothing.
        let snapshot = sectioned();
        let rows = models_flat_filtered_from(&snapshot, "", "", "");
        let (lines, row_line) = models_body_lines(&rows);

        // One label per non-empty section (all three are populated here).
        let labels: Vec<&str> = lines
            .iter()
            .filter_map(|l| match l {
                ModelBodyLine::Section(s) => Some(s.label()),
                ModelBodyLine::Row(_) => None,
            })
            .collect();
        assert_eq!(labels, vec!["FAVORITES", "RECENT", "ALL MODELS"]);

        // Labels come from the display-ordered section enum.
        assert!(lines[0] == ModelBodyLine::Section(ModelSection::Favorites));

        // Row 0 (the favorite) paints one line below its label; the row map
        // is strictly increasing and within the body.
        assert_eq!(row_line[0], 1);
        assert!(row_line.windows(2).all(|w| w[0] < w[1]));
        assert_eq!(
            row_line.last().copied(),
            Some(lines.len() - 1),
            "the last row paints the last line"
        );
        // Every Row(i) entry's mapped line actually holds that row.
        for (i, line) in row_line.iter().enumerate() {
            assert_eq!(lines[*line], ModelBodyLine::Row(i));
        }
    }

    #[test]
    fn body_lines_skip_empty_sections() {
        // A snapshot with neither favorites nor usage renders ONE label
        // (ALL MODELS) — no empty FAVORITES/RECENT headers.
        let snapshot = sample();
        let rows = models_flat_filtered_from(&snapshot, "", "", "");
        let (lines, _) = models_body_lines(&rows);
        let labels: Vec<&str> = lines
            .iter()
            .filter_map(|l| match l {
                ModelBodyLine::Section(s) => Some(s.label()),
                ModelBodyLine::Row(_) => None,
            })
            .collect();
        assert_eq!(labels, vec!["ALL MODELS"]);
    }

    #[test]
    fn flat_sorts_ascii_with_provider_label_tiebreak() {
        // Full-list invariant inside each section: rows never increase across
        // ASCII model id, then provider label. Run against the plain sample's
        // every adjacent pair.
        let snapshot = sample();
        let rows = models_flat_filtered_from(&snapshot, "", "", "");
        let keys: Vec<(ModelSection, String, String)> = rows
            .iter()
            .map(|r| (r.section(), r.model.clone(), r.provider_label.clone()))
            .collect();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted);
    }

    #[test]
    fn flat_fuzzy_filters_by_model_id() {
        // A query matching a model id keeps that pair with highlight
        // positions indexing onto the id's characters.
        let snapshot = sample();
        let rows = models_flat_filtered_from(&snapshot, "", "", "opus");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].model, "claude-opus-4-8");
        assert!(
            rows[0].match_id.is_some(),
            "an id match carries the id column's highlight"
        );
    }

    #[test]
    fn protocol_candidates_filter_by_wire_format() {
        let openai = protocol_model_candidates(WireProtocol::ChatCompletions.as_str());
        assert!(openai.contains(&"gpt-4o"));
        // Anthropic-format models are excluded from the OpenAI candidate list.
        assert!(!openai.contains(&"claude-opus-4-8"));
        let anthropic = protocol_model_candidates(WireProtocol::AnthropicMessages.as_str());
        assert!(anthropic.contains(&"claude-opus-4-8"));
        assert!(!anthropic.contains(&"gpt-4o"));
    }

    #[test]
    fn google_candidate_set_is_the_canonical_family() {
        // The native-Google candidate list mirrors the ids Google plus common
        // relays/中转站 serve — so a Custom Google provider offers real models,
        // not hallucinated preview ids. Image/embedding/video-only models are
        // excluded (an agent only consumes the text generateContent surface).
        let google = protocol_model_candidates(WireProtocol::GoogleGemini.as_str());
        for id in [
            "gemini-3.8-flash",
            "gemini-3.7-flash",
            "gemini-3.5-flash",
            "gemini-3-pro-preview",
            "gemini-3-flash-preview",
            "gemini-3.1-pro-preview",
            "gemini-2.5-flash",
            "gemini-2.5-pro",
            "gemini-2.0-flash",
        ] {
            assert!(google.contains(&id), "google candidate set missing {id}");
        }
        // Image-generation variants must NOT be in the text agent's candidate set.
        assert!(
            !google.contains(&"gemini-2.5-flash-image"),
            "image-only model leaked into google candidates"
        );
    }

    #[test]
    fn google_candidate_set_includes_antigravity_relay_models() {
        // The Antigravity (sub2api) relay ids are registered as native-Google
        // baselines, so the add-model overlay for a Google provider offers
        // them (the closed-set policy has real candidates to pick from).
        let google = protocol_model_candidates(WireProtocol::GoogleGemini.as_str());
        for id in [
            "gemini-3.1-pro-high",
            "gemini-3.1-pro-low",
            "gemini-3-flash",
        ] {
            assert!(
                google.contains(&id),
                "antigravity relay model {id} missing from google candidates"
            );
        }
    }

    #[test]
    fn antigravity_template_is_offered_with_prefilled_url_and_seeded_models() {
        let tmpl = PROVIDER_PRESETS
            .iter()
            .find(|t| t.id == "google-antigravity")
            .expect("antigravity template offered in the chooser");
        assert_eq!(tmpl.label, "Google Antigravity");
        assert_eq!(tmpl.protocol, WireProtocol::GoogleGemini);
        assert_eq!(
            tmpl.models,
            nuo_wire::model_providers::ANTIGRAVITY_OAUTH_MODELS
        );
        assert_eq!(
            tmpl.default_url,
            Some("https://daily-cloudcode-pa.googleapis.com")
        );
        assert!(!tmpl.needs_url, "OAuth template hides Base URL field");
        assert!(
            !tmpl.needs_model,
            "no free-text Model field — models are seeded"
        );
        assert_eq!(tmpl.fields(), vec![CustomField::Name]);
    }

    #[test]
    fn openai_template_seeds_openai_text_models() {
        let tmpl = PROVIDER_PRESETS
            .iter()
            .find(|t| t.id == "openai")
            .expect("openai template offered in the chooser");
        assert_eq!(tmpl.protocol, WireProtocol::ChatCompletions);
        assert_eq!(
            tmpl.models,
            nuo_wire::model_providers::OPENAI_BUILTIN_MODELS
        );
        assert!(
            !tmpl.needs_url,
            "official endpoint URL is prefilled and hidden"
        );
        assert!(
            !tmpl.needs_model,
            "model list is seeded; add-model handles custom ids"
        );
        assert_eq!(tmpl.fields(), vec![CustomField::Name, CustomField::Token]);
        for id in ["gpt-5.5", "gpt-5.4", "gpt-5.6-sol"] {
            assert!(
                protocol_model_candidates(WireProtocol::ChatCompletions.as_str()).contains(&id),
                "OpenAI candidate set missing {id}"
            );
        }
    }

    #[test]
    fn builtin_templates_prefill_official_urls_generic_relays_do_not() {
        let builtin_labels = [
            "OpenAI Platform",
            "Anthropic",
            "Google AI Studio",
            "CommandCode Plan",
            "DeepSeek",
            "xAI",
            "ChatGPT Plan",
            "GitHub Copilot",
            "Google Antigravity",
            "Kimi Code",
            "ZAI Code (CN)",
            "OpenCode",
            "OpenCode Plan",
            "OpenCode Zen",
            "OpenRouter",
            "QianwenAI Token Plan",
            "Qoder",
        ];
        for t in PROVIDER_PRESETS {
            if builtin_labels.contains(&t.label) {
                assert!(t.default_url.is_some(), "{:?} should pre-fill", t.label);
            } else {
                assert!(
                    t.default_url.is_none(),
                    "{:?} generic relay must not pre-fill",
                    t.label
                );
            }
        }
    }

    #[test]
    fn opencode_zen_template_is_key_authenticated_on_the_zen_relay() {
        let tmpl = PROVIDER_PRESETS
            .iter()
            .find(|t| t.id == "opencode-zen")
            .expect("opencode-zen template offered in the chooser");
        assert_eq!(tmpl.label, "OpenCode Zen");
        assert_eq!(tmpl.protocol, WireProtocol::ChatCompletions);
        assert_eq!(
            tmpl.models,
            nuo_wire::model_providers::OPENCODE_ZEN_MODELS
        );
        assert_eq!(
            tmpl.default_url,
            Some("https://opencode.ai/zen/v1/chat/completions")
        );
        assert_eq!(tmpl.auth, ConnectionAuth::ApiKey);
        assert!(
            !tmpl.oauth_first(),
            "the Zen relay signs in with an API key"
        );
        assert_eq!(tmpl.fields(), vec![CustomField::Name, CustomField::Token]);
    }

    #[test]
    fn google_model_set_is_closed_others_open() {
        // A closed set means the add-model overlay offers no free-text fallback:
        // the candidate list is the complete family, so an unmatched id is a
        // typo. OpenAI/Anthropic relays serve an open, evolving set, so typing
        // an unlisted id stays legitimate there.
        assert!(
            protocol_model_set_closed(WireProtocol::GoogleGemini.as_str()),
            "native Google must be a closed model set"
        );
        assert!(
            !protocol_model_set_closed(WireProtocol::ChatCompletions.as_str()),
            "OpenAI relays keep an open model set"
        );
        assert!(
            !protocol_model_set_closed(WireProtocol::AnthropicMessages.as_str()),
            "Anthropic relays keep an open model set"
        );
    }

    #[test]
    fn connections_lists_one_row_per_provider_including_custom() {
        let snapshot = sample();
        let rows = providers_filtered_from(&snapshot, "");
        assert_eq!(rows.len(), snapshot.rows.len());
        // The user-defined provider shows up like any built-in.
        assert!(rows.iter().any(|r| r.id == "my-relay"));
    }

    #[test]
    fn connections_fuzzy_filters_by_provider_name() {
        let snapshot = sample();
        let rows = providers_filtered_from(&snapshot, "anthro");
        assert!(rows.iter().all(|r| r.id == "anthropic"));
        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn connections_orders_by_last_used_then_name_no_favorite() {
        // Favorite is model-level now (ADR-0046), so the Connections list no
        // longer sorts by it — only last-used desc, then instance name.
        let mut snapshot = sample();
        // Give kimi-code a recent activation so it leads.
        for r in &mut snapshot.rows {
            r.last_used_ms = (r.id == "kimi-code").then_some(1_000);
        }
        let rows = providers_filtered_from(&snapshot, "");
        assert_eq!(rows[0].id, "kimi-code");
        // The rest fall back to name order.
        let rest: Vec<&str> = rows[1..].iter().map(|r| r.id.as_str()).collect();
        assert_eq!(rest, vec!["anthropic", "my-relay", "openai"]);
    }

    #[test]
    fn connections_no_longer_groups_builtins_before_custom() {
        let snapshot = sample();
        let rows = providers_filtered_from(&snapshot, "");
        let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["anthropic", "kimi-code", "my-relay", "openai"]);
    }

    #[test]
    fn is_multi_model_tracks_model_count() {
        let snapshot = sample();
        let rows = providers_filtered_from(&snapshot, "");
        let kimi = rows.iter().find(|r| r.id == "kimi-code").unwrap();
        assert!(!kimi.is_multi_model());
        let openai = rows.iter().find(|r| r.id == "openai").unwrap();
        assert!(openai.is_multi_model());
    }

    #[test]
    fn flat_lists_every_provider_model_pair() {
        // The flat Models picker has one row per (provider, model) pair across
        // ALL snapshot rows — no drilling, no per-provider scoping.
        let snapshot = sample();
        let rows = models_flat_filtered_from(&snapshot, "", "", "");
        let pair_count: usize = snapshot.rows.iter().map(|r| r.models.len()).sum();
        assert_eq!(rows.len(), pair_count);
        // Pairs from different providers coexist, each carrying its provider
        // id AND the provider display name for the `· <provider>` row suffix.
        let openai = rows
            .iter()
            .find(|r| r.provider_id == "openai" && r.model == "gpt-4o-mini")
            .expect("openai pair present");
        assert_eq!(openai.provider_label, "OpenAI");
        assert!(rows.iter().any(|r| r.provider_id == "my-relay"));
        assert!(
            rows.iter()
                .any(|r| r.provider_id == "anthropic" && r.model == "claude-opus-4-8")
        );
    }

    #[test]
    fn flat_sorts_favorite_model_first_then_ascii() {
        // Favorite is model-level (ADR-0046): a starred model sorts into the
        // leading section of the flat list wherever it is served. Give one
        // anthropic model recency and favorite another: the favorited model
        // leads in FAVORITES, the used model is in RECENT, and ALL MODELS contains all 8 models.
        let mut snapshot = sample();
        let anthropic = snapshot
            .rows
            .iter_mut()
            .find(|r| r.id == "anthropic")
            .unwrap();
        let mut starred = info("claude-sonnet-5");
        starred.favorite = true;
        let mut used = info("claude-fable-5");
        used.last_used_ms = Some(100);
        anthropic.model_info = vec![starred, used];
        let rows = models_flat_filtered_from(&snapshot, "", "", "");
        // The favorited model leads the FAVORITES section.
        assert!(rows[0].favorite);
        assert_eq!(rows[0].model, "claude-sonnet-5");
        assert_eq!(rows[0].section(), ModelSection::Favorites);
        // The used model is in the RECENT section.
        assert_eq!(rows[1].model, "claude-fable-5");
        assert_eq!(rows[1].section(), ModelSection::Recent);
        // Everything from the third row on is the ALL MODELS section (all 9 models).
        assert_eq!(rows[2..].len(), 9);
        assert!(rows[2..].iter().all(|r| r.section() == ModelSection::All));
        let rest: Vec<&str> = rows[2..].iter().map(|r| r.model.as_str()).collect();
        let mut sorted = rest.clone();
        sorted.sort();
        assert_eq!(rest, sorted);
    }

    #[test]
    fn flat_fuzzy_by_provider_name_includes_its_models_unhighlighted() {
        // "relay" matches no model id but DOES match the "My Relay"
        // provider name: that provider's models are included with no model
        // match at all (rendered without highlight), while other providers
        // drop out.
        let snapshot = sample();
        let rows = models_flat_filtered_from(&snapshot, "", "", "relay");
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r.provider_id == "my-relay"));
        assert!(
            rows.iter()
                .all(|r| r.match_id.is_none() && r.match_name.is_none()),
            "provider-name fallback rows are unhighlighted"
        );
    }

    #[test]
    fn flat_fuzzy_matches_the_name_label_and_keeps_the_id_as_an_alias() {
        // A relay's wire id (`deepseek-flash`) says nothing about "V4.1", and a
        // name-first list leads with the name — so a name query highlights the
        // label column. The id stays reachable as an alias, and because the id
        // is drawn too (its own column), an id query highlights the ID column
        // rather than leaving the row unhighlighted.
        let mut snapshot = sample();
        for prow in &mut snapshot.rows {
            if prow.id == "my-relay" {
                let mut i = info("deepseek-flash");
                i.name = Some("DeepSeek V4.1 Flash".to_string());
                prow.models = vec!["deepseek-flash".to_string()];
                prow.model_info = vec![i];
            }
        }

        // The marketed name matches the leading label column → highlighted.
        let rows = models_flat_filtered_from(&snapshot, "", "", "v4.1");
        assert_eq!(rows.len(), 1, "only the labelled model matches: {rows:?}");
        assert_eq!(rows[0].model, "deepseek-flash", "the id stays the identity");
        assert_eq!(rows[0].name.as_deref(), Some("DeepSeek V4.1 Flash"));
        assert!(
            rows[0].match_name.is_some() && rows[0].match_id.is_none(),
            "a name query highlights the label column only"
        );

        // The wire id finds the row and highlights the ID column.
        let rows = models_flat_filtered_from(&snapshot, "", "", "deepseek-flash");
        assert_eq!(rows.len(), 1, "the id matches: {rows:?}");
        assert!(
            rows[0].match_id.is_some() && rows[0].match_name.is_none(),
            "an id query highlights the id column only"
        );

        let rows = models_flat_filtered_from(&snapshot, "", "", "v9.9");
        assert!(rows.is_empty(), "no row matches: {rows:?}");
    }

    #[test]
    fn flat_rows_carry_the_provider_label_only_when_advertised() {
        // `name` is optional by construction: a model with no advertised label
        // surfaces `None` (the renderer then leads with the bare id), one with a
        // label surfaces it verbatim.
        let mut snapshot = sample();
        for prow in &mut snapshot.rows {
            let ids: Vec<String> = prow.models.clone();
            prow.model_info = ids.iter().map(|m| info(m)).collect();
        }
        let rows = models_flat_filtered_from(&snapshot, "", "", "");
        assert!(rows.iter().all(|r| r.name.is_none()));

        if let Some(prow) = snapshot.rows.iter_mut().find(|p| p.id == "my-relay") {
            let mut i = info("glm-5.2");
            i.name = Some("GLM-5.2 (Agentic)".to_string());
            prow.model_info = vec![i];
        }
        let rows = models_flat_filtered_from(&snapshot, "", "", "");
        let glm = rows.iter().find(|r| r.model == "glm-5.2").expect("row");
        assert_eq!(glm.name.as_deref(), Some("GLM-5.2 (Agentic)"));
    }

    #[test]
    fn flat_rows_order_ascii_without_current_or_favorite() {
        // With no favorites and no usage history, the whole list is the ALL
        // MODELS section in pure ASCII order (provider label as the
        // tiebreak) — deterministic regardless of provider order.
        let snapshot = sample();
        let rows = models_flat_filtered_from(&snapshot, "", "", "");
        let ids: Vec<&str> = rows.iter().map(|r| r.model.as_str()).collect();
        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(ids, sorted);
    }

    #[test]
    fn each_template_models_reference_the_shared_constants() {
        // The template `id` IS the model provider id, persisted on the created
        // connection as `provider`. The daemon's provider specs and this UI
        // table must share the *same* model-list constant (single source of
        // truth in `nuo_wire::model_providers`) — otherwise the catalog's
        // reconciliation could not re-seed a connection from its provider. This
        // test catches a UI table that inlined a drifted copy of the list.
        for t in PROVIDER_PRESETS {
            let referenced = match t.id {
                "anthropic" => Some(nuo_wire::model_providers::ANTHROPIC_BUILTIN_MODELS),
                "chatgpt-plan" | "openai-subscription" => {
                    Some(nuo_wire::model_providers::CHATGPT_BUILTIN_MODELS)
                }
                "commandcode-plan" | "commandcode" => {
                    Some(nuo_wire::model_providers::COMMANDCODE_BUILTIN_MODELS)
                }
                "deepseek" => Some(nuo_wire::model_providers::DEEPSEEK_BUILTIN_MODELS),
                "github-copilot" => Some(nuo_wire::model_providers::COPILOT_SEED_MODELS),
                "google" => Some(nuo_wire::model_providers::GOOGLE_BUILTIN_MODELS),
                "google-antigravity" => {
                    Some(nuo_wire::model_providers::ANTIGRAVITY_OAUTH_MODELS)
                }
                "kimi-code" => Some(nuo_wire::model_providers::KIMI_CODE_MODELS),
                "openai" => Some(nuo_wire::model_providers::OPENAI_BUILTIN_MODELS),
                "openrouter" => Some(nuo_wire::model_providers::OPENROUTER_BUILTIN_MODELS),
                "opencode-plan" | "opencode-go" => Some(nuo_wire::model_providers::OPENCODE_GO_MODELS),
                "opencode-zen" => Some(nuo_wire::model_providers::OPENCODE_ZEN_MODELS),
                "opencode" => Some(nuo_wire::model_providers::OPENCODE_CONSOLE_MODELS),
                "glm-cn" => Some(nuo_wire::model_providers::ZAI_CODE_MODELS),
                "xai" => Some(nuo_wire::model_providers::XAI_BUILTIN_MODELS),
                _ => None,
            };
            if let Some(expected) = referenced {
                assert_eq!(
                    t.models, expected,
                    "template {} model list diverged from the shared constant",
                    t.id
                );
            }
        }
    }

    #[test]
    fn template_ids_are_unique() {
        let mut ids: Vec<&str> = PROVIDER_PRESETS
            .iter()
            .chain(std::iter::once(&CUSTOM_TEMPLATE))
            .map(|t| t.id)
            .collect();
        ids.sort_unstable();
        let dups: Vec<&[&str]> = ids.windows(2).filter(|pair| pair[0] == pair[1]).collect();
        assert!(dups.is_empty(), "duplicate template ids: {dups:?}");
    }

    #[test]
    fn openai_platform_template_is_labeled_to_distinguish_chatgpt() {
        // The `openai` template is the platform/API-key billing plan, distinct
        // from the ChatGPT Subscription template that shares its wire protocol.
        // The label must say so — a bare "OpenAI" reads as the company and
        // matches the subscription plan users actually have.
        let openai = PROVIDER_PRESETS.iter().find(|t| t.id == "openai").unwrap();
        assert_eq!(openai.label, "OpenAI Platform");
        let chatgpt = PROVIDER_PRESETS
            .iter()
            .find(|t| t.id == "chatgpt-plan")
            .unwrap();
        assert_eq!(openai.protocol, WireProtocol::ChatCompletions);
        assert_eq!(chatgpt.protocol, WireProtocol::Responses);
        assert!(
            chatgpt.models.is_empty(),
            "the subscription template must not hardcode a seed — Codex /backend-api/codex/models is authoritative"
        );
    }

    #[test]
    fn commandcode_template_uses_provider_route_and_seeds() {
        let cmd = PROVIDER_PRESETS
            .iter()
            .find(|template| template.id == "commandcode-plan")
            .unwrap();
        assert_eq!(cmd.label, "CommandCode Plan");
        assert_eq!(cmd.protocol, WireProtocol::ChatCompletions);
        assert_eq!(
            cmd.default_url,
            Some("https://api.commandcode.ai/provider/v1/chat/completions")
        );
        assert_eq!(
            cmd.models,
            nuo_wire::model_providers::COMMANDCODE_BUILTIN_MODELS
        );
    }

    #[test]
    fn openrouter_template_uses_gateway_route_and_nex_seed() {
        let openrouter = PROVIDER_PRESETS
            .iter()
            .find(|template| template.id == "openrouter")
            .unwrap();
        assert_eq!(openrouter.label, "OpenRouter");
        assert_eq!(openrouter.protocol, WireProtocol::ChatCompletions);
        assert_eq!(
            openrouter.default_url,
            Some("https://openrouter.ai/api/v1/chat/completions")
        );
        assert_eq!(
            openrouter.models,
            nuo_wire::model_providers::OPENROUTER_BUILTIN_MODELS
        );
    }

    #[test]
    fn editor_title_resolves_by_provider_id_not_protocol() {
        // Several providers share the `openai` wire (openai-subscription is
        // declared first). A create-mode editor title must resolve from the
        // seeded provider id, otherwise every openai-wire flow would be
        // headed "ChatGPT Subscription".
        assert_eq!(provider_label_for(Some("openai")), "OpenAI Platform");
        assert_eq!(
            provider_label_for(Some("chatgpt-plan")),
            "ChatGPT Plan"
        );
        assert_eq!(provider_label_for(Some("custom")), "Custom connection");
        assert_eq!(provider_label_for(Some("deepseek")), "DeepSeek");
        // Unknown / unseeded ids fall back to the generic header.
        assert_eq!(provider_label_for(None), "＋ Add connection");
        assert_eq!(
            provider_label_for(Some("no-such-provider")),
            "＋ Add connection"
        );
    }

    #[test]
    fn custom_template_is_not_a_chooser_row() {
        assert!(
            PROVIDER_PRESETS.iter().all(|t| t.id != "custom"),
            "custom connections have their own Connections-level branch"
        );
        assert_eq!(
            connection_definition("custom").map(|definition| definition.id),
            Some("custom")
        );
    }

    #[test]
    fn edit_fields_api_key_shows_transport_and_identity_for_custom_only() {
        // A pure-custom API-key provider also exposes its transport and
        // request identity.
        let custom_fields = edit_fields(false, ConnectionAuth::ApiKey);
        assert_eq!(
            custom_fields,
            vec![
                CustomField::Name,
                CustomField::BaseUrl,
                CustomField::Token,
                CustomField::Protocol,
                CustomField::ClientIdentity,
            ]
        );

        // A curated API-key provider derives its Base URL from the provider
        // spec, so it only exposes Name and Token.
        let curated_fields = edit_fields(true, ConnectionAuth::ApiKey);
        assert_eq!(curated_fields, vec![CustomField::Name, CustomField::Token]);
    }

    #[test]
    fn edit_fields_oauth_shows_name_only() {
        // An OAuth connection's endpoint and bearer are owned by the auth flow
        // (xAI `https://api.x.ai/...`, ChatGPT
        // `https://chatgpt.com/backend-api/codex/...`). The editor must expose
        // only a rename, so the server-side guard is never the lone defense
        // against wiping them.
        let xai = edit_fields(true, ConnectionAuth::subscription("xai"));
        assert_eq!(xai, vec![CustomField::Name]);

        let chatgpt = edit_fields(true, ConnectionAuth::subscription("chatgpt"));
        assert_eq!(chatgpt, vec![CustomField::Name]);
    }

    #[test]
    fn flat_rows_exclude_unready_providers() {
        let mut snapshot = sample();
        // Mark openai as not key_ready
        snapshot.rows[1].key_ready = false;
        let rows = models_flat_filtered_from(&snapshot, "", "", "");
        assert!(!rows.iter().any(|r| r.provider_id == "openai"));
        assert!(rows.iter().any(|r| r.provider_id == "kimi-code"));
    }

    /// The flat picker row must carry the route's effort ladder straight from
    /// the snapshot. `RankedModel.effort_levels` was hardcoded empty, so the
    /// editor opened from a Models row always fell back to the value-only
    /// control — the node slider never appeared. The daemon is the only
    /// authority for the ladder (it owns the provider baseline tables, which
    /// this client does not link), so the field must be a pass-through.
    #[test]
    fn flat_rows_carry_the_snapshot_effort_ladder() {
        let mut snapshot = sample();
        for prow in &mut snapshot.rows {
            if prow.id == "kimi-code" {
                let mut i = info("kimi-k2.7-code");
                i.effort = Some("high".to_string());
                i.effort_levels = vec![
                    "low".to_string(),
                    "high".to_string(),
                    "max".to_string(),
                ];
                prow.model_info = vec![i];
            }
        }
        let rows = models_flat_filtered_from(&snapshot, "", "", "kimi-k2.7-code");
        let row = rows
            .iter()
            .find(|r| r.model == "kimi-k2.7-code")
            .expect("row");
        assert_eq!(
            row.effort_levels,
            vec!["low".to_string(), "high".to_string(), "max".to_string()],
            "the row must forward the snapshot ladder, not drop it"
        );
    }
}
