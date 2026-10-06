//! Builtin seed/baseline model lists for the shipped model providers.
//!
//! Pure static data shared by the daemon (catalog reconciliation) and the
//! frontend (add-connection chooser). Single source of truth.

pub const ANTHROPIC_BUILTIN_MODELS: &[&str] = &[
    "claude-fable-5",
    "claude-sonnet-5",
    "claude-opus-4-8",
    "claude-sonnet-4-6",
    "claude-haiku-4-5-20251001",
];

pub const CHATGPT_BUILTIN_MODELS: &[&str] = &[];

pub const DEEPSEEK_BUILTIN_MODELS: &[&str] = &[
    "deepseek-v4-flash",
    "deepseek-v4-pro",
    "deepseek-v4-flash-vision-exp",
];

pub const COPILOT_SEED_MODELS: &[&str] = &["gpt-4o-mini"];

pub const GOOGLE_BUILTIN_MODELS: &[&str] = &[
    // Gemini 3.x
    "gemini-3.8-flash",
    "gemini-3.7-flash",
    "gemini-3.5-flash",
    "gemini-3-pro-preview",
    "gemini-3-flash-preview",
    "gemini-3.1-pro-preview",
    "gemini-3.1-pro-preview-customtools",
    // Gemini 2.5
    "gemini-2.5-flash",
    "gemini-2.5-pro",
    "gemini-2.5-flash-lite",
    // Gemini 2.0 (still widely served by relays)
    "gemini-2.0-flash",
];

pub const ANTIGRAVITY_OAUTH_MODELS: &[&str] = &[
    "gemini-3.8-flash-tiered",
    "gemini-3.7-flash-tiered",
    "gemini-pro-agent",
    "gemini-3.1-pro-low",
    "gemini-3.1-flash-lite",
    "gemini-2.5-flash",
    "gemini-2.5-pro",
];

pub const KIMI_CODE_MODELS: &[&str] = &["k3", "kimi-k2.7-code"];

pub const OPENAI_BUILTIN_MODELS: &[&str] = &[
    "gpt-5.6-sol",
    "gpt-5.6-terra",
    "gpt-5.6-luna",
    "gpt-5.5",
    "gpt-5.4",
    "gpt-5.4-mini",
];

/// Offline seed for OpenRouter. Its live `/models` catalog is authoritative;
/// this keeps the primary Nex coding model selectable before the first refresh.
pub const OPENROUTER_BUILTIN_MODELS: &[&str] = &["nex-agi/nex-n2.5-pro:free"];

/// Seed models for Command Code Provider API. Its live `/models` endpoint is authoritative;
/// this keeps the flagship models selectable before the first refresh.
///
/// The ids are CommandCode's own `vendor/model` wire ids; the DeepSeek siblings
/// are listed because the server resolves them per turn (ADR-0014) and each
/// carries a registered effort ladder, so the picker exposes its depth control
/// offline as well.
pub const COMMANDCODE_BUILTIN_MODELS: &[&str] = &[
    "claude-sonnet-5-5",
    "gpt-5.6-sol",
    "deepseek/deepseek-v4-flash",
    "deepseek/deepseek-v4.1-flash",
    "deepseek/deepseek-v4-pro",
];

pub const OPENCODE_GO_MODELS: &[&str] = &[
    "glm-5.2",
    "kimi-k2.7-code",
    "deepseek-v4-flash",
    "deepseek-v4.1-flash",
];

pub const OPENCODE_CONSOLE_MODELS: &[&str] = &["claude-sonnet-4-6", "deepseek-v4-flash", "glm-5.2"];

/// OpenCode Zen relay seeds. The key-authenticated `/zen/v1` surface publishes
/// a public `/zen/v1/models` catalog, which is authoritative; this is the
/// offline seed before the first refresh.
pub const OPENCODE_ZEN_MODELS: &[&str] = &["claude-sonnet-4-6", "deepseek-v4-flash", "glm-5.2"];

pub const ZAI_CODE_MODELS: &[&str] = &["glm-5.3", "glm-5.3-flash", "glm-5.2"];

/// QianwenAI Platform Token Plan seeds, in activation order. The live
/// `/models` endpoint is authoritative for what this account's plan may run
/// (the whitelist is enforced upstream); this keeps the plan's Qwen-native
/// flagship models selectable before the first refresh. The plan's GLM /
/// DeepSeek / Kimi third-party rows are not seeded — their capability
/// baselines are owned by their home providers and the live catalog serves
/// them.
pub const QIANWEN_BUILTIN_MODELS: &[&str] = &["qwen3.8-max", "qwen3.8-flash", "qwen3.6-flash"];

pub const XAI_BUILTIN_MODELS: &[&str] = &["grok-4.5", "grok-4.20", "grok-4.3", "grok-build-0.1"];

use serde::{Deserialize, Serialize};

/// A first-class user-declared model provider surface (ADR-0258).
///
/// Declares the physical transport endpoint, default protocol, catalog discovery,
/// dialect, and client identity preset for a custom LLM service surface (e.g.
/// corporate relay, local vLLM, or self-hosted gateway).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserDeclaredProvider {
    /// Optional human-readable display label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Root API URL (e.g. `https://relay.example.com/v1`, without trailing slash).
    pub root_url: String,
    /// Default wire transport protocol (e.g. `chat-completions`, `responses`, `anthropic-messages`, `google-gemini`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_protocol: Option<crate::WireProtocol>,
    /// Default client profile preset for User-Agent / client headers emulation (ADR-0164, ADR-0258).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_profile: Option<crate::ClientPreset>,
    /// Optional User-Agent override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_agent: Option<String>,
    /// Catalog discovery format: `openai`, `anthropic`, `google`, `none`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog: Option<crate::provider_surface::RemoteCatalogSource>,
    /// Typed service dialect, inherited independently of model protocol.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dialect: Option<crate::catalog::ProviderDialect>,
    /// Optional explicit transport endpoints, keyed by model wire protocol.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub protocol_roots: Vec<(crate::WireProtocol, String)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog_root_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_cache: Option<crate::provider_surface::ProviderPromptCache>,
    #[serde(default)]
    pub client_profile_sensitive: bool,
}

impl UserDeclaredProvider {
    pub fn validate(&self, id: &str) -> Result<(), String> {
        if id.is_empty() || id.trim() != id {
            return Err("provider id must be nonempty and trimmed".into());
        }
        if is_known_model_provider(id) {
            return Err(format!("provider `{id}` collides with a built-in provider"));
        }
        crate::provider_surface::ApiRoot::parse(&self.root_url)?;
        if let Some(cache) = &self.prompt_cache {
            cache.validate()?;
        }
        if let Some(root) = &self.catalog_root_url {
            crate::provider_surface::ApiRoot::parse(root)?;
        }
        let mut wires = std::collections::HashSet::new();
        for (wire, root) in &self.protocol_roots {
            if !wires.insert(*wire) {
                return Err(format!("duplicate protocol root for {wire}"));
            }
            crate::provider_surface::ApiRoot::parse(root)?;
        }
        if !self.dialect.unwrap_or_default().supports(
            self.default_protocol
                .unwrap_or(crate::WireProtocol::ChatCompletions),
        ) {
            return Err(format!(
                "provider `{id}` has an incompatible default protocol"
            ));
        }
        Ok(())
    }
}

/// Qoder subscription seed models, in activation order.
///
/// This is the **offline seed** — the ids a connection starts with before the
/// live scene catalog has been fetched. The catalog is the authority once it
/// answers; the seed only keeps the provider usable offline and gives the
/// picker an activation order (`qmodel_38max` first, matching the vendor's own
/// `is_default` entry).
pub const QODER_MODELS: &[&str] = &["qmodel_38max", "qfmodel"];

// ═════════════════════════════════════════════════════════════════════════════
// Model provider ids — contract vocabulary
// ═════════════════════════════════════════════════════════════════════════════

/// The closed set of model provider ids.
///
/// Every connection persists one of these, so the vocabulary is contract data
/// rather than an implementation detail.
///
/// A provider id names a **service surface** — (endpoint family, wire dialect,
/// model universe). It never encodes a wire protocol or an authentication mode.
pub const MODEL_PROVIDER_IDS: &[&str] = &[
    "openai",
    "chatgpt-plan",
    "anthropic",
    "google",
    "google-antigravity",
    "github-copilot",
    "xai",
    "deepseek",
    "glm-cn",
    "kimi-code",
    "openrouter",
    "opencode",
    "opencode-zen",
    "opencode-plan",
    "qoder",
    "qianwen",
    "commandcode-plan",
];

/// Whether `id` names a registered model provider.
pub fn is_known_model_provider(id: &str) -> bool {
    MODEL_PROVIDER_IDS.contains(&id)
}

/// Map a provider id from any era to its canonical id, or `None` when the
/// value names no provider at all.
///
/// Legacy spellings appended `-oauth` to encode an authentication
/// mode; `zai-code` named the CN endpoint with the international brand. These
/// aliases exist **only** to migrate an existing `connections.toml` on load —
/// nothing serializes them back.
pub fn canonical_provider_id(id: &str) -> Option<String> {
    let canonical = match id {
        "chatgpt" | "chatgpt-oauth" | "openai-subscription" => "chatgpt-plan",
        "commandcode" | "command-code" => "commandcode-plan",
        "opencode-go" => "opencode-plan",
        "antigravity-oauth" => "google-antigravity",
        "copilot-oauth" => "github-copilot",
        "xai-oauth" => "xai",
        "zai-code" => "glm-cn",
        "kimi" => "kimi-code",
        other => other,
    };
    is_known_model_provider(canonical).then(|| canonical.to_string())
}

/// Canonical human-readable display label for a model provider ID.
pub fn model_provider_label(id: &str) -> &'static str {
    match id {
        "anthropic" => "Anthropic",
        "chatgpt-plan" => "ChatGPT Plan",
        "commandcode-plan" => "CommandCode Plan",
        "deepseek" => "DeepSeek",
        "github-copilot" => "GitHub Copilot",
        "google" => "Google AI Studio",
        "google-antigravity" => "Google Antigravity",
        "kimi-code" => "Kimi Code",
        "openai" => "OpenAI Platform",
        "opencode-zen" => "OpenCode Zen",
        "opencode-plan" => "OpenCode Plan",
        "openrouter" => "OpenRouter",
        "glm-cn" => "ZAI Code (CN)",
        "qoder" => "Qoder",
        "qianwen" => "QianwenAI Token Plan",
        "xai" => "xAI",
        _ => "Custom Provider",
    }
}

#[cfg(test)]
mod provider_id_tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_never_encode_protocol_or_auth() {
        let mut ids = MODEL_PROVIDER_IDS.to_vec();
        ids.sort_unstable();
        let dups: Vec<&str> = ids
            .windows(2)
            .filter(|pair| pair[0] == pair[1])
            .map(|pair| pair[0])
            .collect();
        assert!(dups.is_empty(), "duplicate provider ids: {dups:?}");
        for id in MODEL_PROVIDER_IDS {
            assert!(
                !id.ends_with("-oauth") && !id.ends_with("-compatible"),
                "{id} encodes an auth mode or a protocol"
            );
        }
    }

    #[test]
    fn legacy_ids_canonicalize_and_unknown_ids_are_rejected() {
        assert_eq!(
            canonical_provider_id("chatgpt-oauth").as_deref(),
            Some("chatgpt-plan")
        );
        assert_eq!(
            canonical_provider_id("openai-subscription").as_deref(),
            Some("chatgpt-plan")
        );
        assert_eq!(
            canonical_provider_id("opencode-go").as_deref(),
            Some("opencode-plan")
        );
        assert_eq!(
            canonical_provider_id("commandcode").as_deref(),
            Some("commandcode-plan")
        );
        assert_eq!(
            canonical_provider_id("deepseek").as_deref(),
            Some("deepseek")
        );
        assert_eq!(canonical_provider_id("zai-code").as_deref(), Some("glm-cn"));
        assert!(canonical_provider_id("does-not-exist").is_none());
        assert!(canonical_provider_id("").is_none());
    }

    #[test]
    fn known_providers_have_labels() {
        for id in MODEL_PROVIDER_IDS {
            let label = model_provider_label(id);
            assert!(!label.is_empty(), "provider {id} has empty label");
        }
        assert_eq!(
            model_provider_label("google-antigravity"),
            "Google Antigravity"
        );
        assert_eq!(
            model_provider_label("chatgpt-plan"),
            "ChatGPT Plan"
        );
        assert_eq!(
            model_provider_label("commandcode-plan"),
            "CommandCode Plan"
        );
        assert_eq!(
            model_provider_label("opencode-plan"),
            "OpenCode Plan"
        );
    }
}
