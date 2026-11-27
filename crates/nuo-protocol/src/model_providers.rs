//! Builtin seed/baseline model lists for the shipped model providers.
//!
//! Pure static data shared by the server (catalog reconciliation) and the
//! frontend (add-connection chooser). Single source of truth.
pub const ANTHROPIC_BUILTIN_MODELS: &[&str] = &[
    "claude-fable-5",
    "claude-sonnet-5",
    "claude-opus-4-8",
    "claude-sonnet-4-6",
    "claude-haiku-4-5-20251001",
];

pub const CHATGPT_BUILTIN_MODELS: &[&str] = &[];

/// The two wire ids `api.deepseek.com` serves (ADR-0083). The retired V4 Flash
/// generation (`deepseek-v4-flash`, `deepseek-v4-flash-vision-exp`) is still
/// accepted upstream and stays registered as a baseline, but it is no longer
/// seeded for new connections.
pub const DEEPSEEK_BUILTIN_MODELS: &[&str] = &["deepseek-flash", "deepseek-v4-pro"];

pub const COPILOT_SEED_MODELS: &[&str] = &["gpt-4o-mini"];

pub const GOOGLE_BUILTIN_MODELS: &[&str] = &[
    // Gemini 4
    "gemini-4-neon",
    "gemini-4-argon",
    "gemini-4-hydrogen",
    "gemini-4-carbon",
    // Gemini 3.x
    "gemini-3.8-flash",
    "gemini-3.7-flash",
    "gemini-3.5-flash",
    "gemini-3-pro-preview",
    "gemini-3-flash-preview",
    "gemini-3.1-pro-preview",
    "gemini-3.1-pro-preview-customtools",
    "gemini-3.1-flash-lite",
    // Gemini 2.5
    "gemini-2.5-flash",
    "gemini-2.5-pro",
    "gemini-2.5-flash-lite",
    // Gemini 2.0 (still widely served by relays)
    "gemini-2.0-flash",
];

pub const ANTIGRAVITY_OAUTH_MODELS: &[&str] = &[
    "gemini-4-neon",
    "gemini-4-argon",
    "gemini-4-hydrogen",
    "gemini-3.8-flash",
    "claude-sonnet-4-6",
    "claude-opus-4-6",
    "claude-sonnet-5-5",
    "claude-opus-5-5",
    "gpt-oss-120b-medium",
    "gemini-3.5-flash-lite",
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
    "deepseek/deepseek-v4-flash-fast",
    "deepseek/deepseek-v4.1-flash",
    "deepseek/deepseek-v4.1-flash-fast",
    "deepseek/deepseek-v4-pro",
];

/// Seed models for the OpenCode Go subscription surface, in activation order.
///
/// The surface speaks the `zen/go` endpoint family and publishes ids only, so
/// these seeds are what the picker shows before the first catalog refresh; the
/// live `/models` payload remains authoritative for what the account may run.
pub const OPENCODE_GO_MODELS: &[&str] = &[
    "deepseek-v4.1-flash",
    "deepseek-v4-pro",
    "glm-5.3-flash",
    "glm-5.3",
    "glm-5.2",
    "qwen3.8-max",
    "qwen3.8-flash",
    "kimi-k3",
    "deepseek-v4.1-flash-free",
    "glm-5.3-flash-free",
];

pub const OPENCODE_ZEN_MODELS: &[&str] = &[
    "deepseek-v4.1-flash",
    "glm-5.3-flash",
    "claude-sonnet-4-6",
    "deepseek-v4.1-flash-free",
    "glm-5.3-flash-free",
];

/// Seed models for the OpenDesign Cloud API surface, in activation order. The
/// gateway's live `/models` endpoint remains authoritative; these keep the
/// Design Plan's flagship rows selectable before the first refresh.
pub const OPENDESIGN_GO_BUILTIN_MODELS: &[&str] = &[
    "deepseek-v4.1-flash",
    "deepseek-v4.1-flash-eco",
    "deepseek-v4-flash",
    "deepseek-v4-flash-vision-exp",
    "deepseek-v4-pro",
    "claude-haiku-5.5",
    "gpt-6-luna",
    "glm-5.3-flash",
    "glm-5.3-flashx",
    "kimi-k2.7-code",
    "mimo-v2.6-flash",
    "mimo-v2.6-pro",
    "minimax-m3.1-flash",
    "hy4-preview",
];

pub const GLM_CN_MODELS: &[&str] = &["glm-5.3", "glm-5.3-flash", "glm-5.2"];

/// QianwenAI Platform Token Plan seeds, in activation order. The live
/// `/models` endpoint is authoritative for what this account's plan may run
/// (the whitelist is enforced upstream); this keeps the plan's Qwen-native
/// flagship models selectable before the first refresh. The plan's GLM /
/// DeepSeek / Kimi third-party rows are not seeded — their capability
/// baselines are owned by their home providers and the live catalog serves
/// them.
pub const QIANWEN_BUILTIN_MODELS: &[&str] = &["qwen3.8-max", "qwen3.8-flash", "qwen3.6-flash"];

pub const XAI_BUILTIN_MODELS: &[&str] = &["grok-4.5", "grok-4.20", "grok-4.3", "grok-build-0.1"];

/// Seed models for the Tencent CodeBuddy domestic line (CN: Hunyuan / DeepSeek /
/// Kimi / GLM / MiniMix), in activation order. The international line seeds a
/// disjoint catalog — see `CODEBUDDY_INTL_BUILTIN_MODELS`.
pub const CODEBUDDY_CN_BUILTIN_MODELS: &[&str] = &[
    "hy4-preview",
    "deepseek-v4-flash",
    "deepseek-v4-pro",
    "kimi-k3-1",
    "glm-5.3-flash",
    "minimax-m2.5",
    "default",
];

/// Seed models for the Tencent CodeBuddy AI international line — disjoint from
/// the domestic catalog (ADR-0097 physical channel bifurcation).
pub const CODEBUDDY_INTL_BUILTIN_MODELS: &[&str] = &["gpt-5.6-sol", "default"];

use serde::{Deserialize, Serialize};

/// A first-class user-declared model provider surface.
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
    pub default_protocol: Option<nuo_inference_wire::WireProtocol>,
    /// Optional User-Agent override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_agent: Option<String>,
    /// Optional custom headers to send on requests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<std::collections::BTreeMap<String, String>>,
    /// Catalog discovery format: `openai`, `anthropic`, `google`, `none`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog: Option<crate::surface::RemoteCatalogSource>,
    /// Typed service dialect, inherited independently of model protocol.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dialect: Option<crate::dialects::ProviderDialect>,
    /// Optional explicit transport endpoints, keyed by model wire protocol.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub protocol_roots: Vec<(nuo_inference_wire::WireProtocol, String)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog_root_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_cache: Option<crate::surface::ProviderPromptCache>,
}

impl UserDeclaredProvider {
    pub fn validate(&self, id: &str) -> Result<(), String> {
        if id.is_empty() || id.trim() != id {
            return Err("provider id must be nonempty and trimmed".into());
        }
        if is_known_model_provider(id) {
            return Err(format!("provider `{id}` collides with a built-in provider"));
        }
        crate::surface::ApiRoot::parse(&self.root_url)?;
        if let Some(cache) = &self.prompt_cache {
            cache.validate()?;
        }
        if let Some(root) = &self.catalog_root_url {
            crate::surface::ApiRoot::parse(root)?;
        }
        let mut wires = std::collections::HashSet::new();
        for (wire, root) in &self.protocol_roots {
            if !wires.insert(*wire) {
                return Err(format!("duplicate protocol root for {wire}"));
            }
            crate::surface::ApiRoot::parse(root)?;
        }
        if !self.dialect.unwrap_or_default().supports(
            self.default_protocol
                .unwrap_or(nuo_inference_wire::WireProtocol::OpenAiChatCompletions),
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
    "opendesign-go",
    "opencode-zen",
    "opencode-go",
    "qoder",
    "qianwen",
    "commandcode-goat",
    "codebuddy-cn",
    "codebuddy-intl",
];

/// Whether `id` names a registered model provider.
pub fn is_known_model_provider(id: &str) -> bool {
    BUILTIN_PROVIDER_DEFAULTS.iter().any(|p| p.id == id)
}

/// Map a provider id from any era to its canonical id, or `None` when the
/// value names no provider at all.
///
/// Legacy spellings appended `-oauth` to encode an authentication mode. These
/// aliases exist **only** to migrate an existing `connections.toml` on load —
/// nothing serializes them back.
pub fn canonical_provider_id(id: &str) -> Option<String> {
    // The OpenCode Console surface (`opencode`), legacy `zai-code`,
    // and retired plan aliases (`commandcode-plan`, `opencode-plan`) were
    // retired outright: their ids deliberately do not canonicalize, so a stale
    // `connections.toml` row fails loudly as an unknown provider.
    let canonical = match id {
        "chatgpt" | "chatgpt-oauth" | "openai-subscription" => "chatgpt-plan",
        "commandcode" | "command-code" => "commandcode-goat",
        "antigravity-oauth" => "google-antigravity",
        "copilot-oauth" => "github-copilot",
        "xai-oauth" => "xai",
        "kimi" => "kimi-code",
        "google-ai-studio" | "ai-studio" | "google-studio" | "gemini" => "google",
        other => other,
    };
    is_known_model_provider(canonical).then(|| canonical.to_string())
}

/// Canonical User-Agent strings for providers whose gateways require a
/// specific client header. Single source of truth shared by provider specs and
/// UI connection presets.
pub const CODEX_USER_AGENT: &str = "codex_cli_rs/0.153.4";
pub const CODEBUDDY_USER_AGENT: &str = "CLI/2.172.1 CodeBuddy/2.172.1";
pub const ZCODE_USER_AGENT: &str = "ZCode/3.5.3";
pub const OPENCODE_USER_AGENT: &str = "opencode/2.0.32";
pub const ANTIGRAVITY_USER_AGENT: &str = "antigravity/cli/1.5.12 \
(aidev_client; os_type=linux; arch=amd64; cl=1024904911; auth_method=consumer)";

/// Single source of truth for a built-in provider's static identity, default
/// protocol, endpoints, User-Agent, and seed model list.
///
/// Both `crates/nuo-provider-*` specs and `crates/nuo-view/src/providers/presets.rs`
/// project these fields directly from here so adding or updating a provider
/// never requires hand-copying URLs, User-Agents, labels, or protocols across
/// layers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuiltinProviderDefaults {
    pub id: &'static str,
    pub label: &'static str,
    pub protocol: nuo_inference_wire::WireProtocol,
    pub root_url: &'static str,
    pub default_endpoint: &'static str,
    pub user_agent: Option<&'static str>,
    pub models: &'static [&'static str],
}

impl BuiltinProviderDefaults {
    /// Borrow `user_agent` as a static `Cow` for `const` `ModelProviderSpec`
    /// definitions.
    pub const fn user_agent_cow(&self) -> Option<std::borrow::Cow<'static, str>> {
        match self.user_agent {
            Some(ua) => Some(std::borrow::Cow::Borrowed(ua)),
            None => None,
        }
    }
}

pub const PROVIDER_ANTHROPIC: BuiltinProviderDefaults = BuiltinProviderDefaults {
    id: "anthropic",
    label: "Anthropic",
    protocol: nuo_inference_wire::WireProtocol::AnthropicMessages,
    root_url: "https://api.anthropic.com/v1",
    default_endpoint: "https://api.anthropic.com/v1/messages",
    user_agent: None,
    models: ANTHROPIC_BUILTIN_MODELS,
};

pub const PROVIDER_CHATGPT_PLAN: BuiltinProviderDefaults = BuiltinProviderDefaults {
    id: "chatgpt-plan",
    label: "ChatGPT Plan",
    protocol: nuo_inference_wire::WireProtocol::OpenAiResponses,
    root_url: "https://chatgpt.com/backend-api/codex",
    default_endpoint: "https://chatgpt.com/backend-api/codex/responses",
    user_agent: Some(CODEX_USER_AGENT),
    models: CHATGPT_BUILTIN_MODELS,
};

pub const PROVIDER_CODEBUDDY_CN: BuiltinProviderDefaults = BuiltinProviderDefaults {
    id: "codebuddy-cn",
    label: "Tencent CodeBuddy CN",
    protocol: nuo_inference_wire::WireProtocol::OpenAiChatCompletions,
    root_url: "https://copilot.tencent.com/v2",
    default_endpoint: "https://copilot.tencent.com/v2/chat/completions",
    user_agent: Some(CODEBUDDY_USER_AGENT),
    models: CODEBUDDY_CN_BUILTIN_MODELS,
};

pub const PROVIDER_CODEBUDDY_INTL: BuiltinProviderDefaults = BuiltinProviderDefaults {
    id: "codebuddy-intl",
    label: "Tencent CodeBuddy Global",
    protocol: nuo_inference_wire::WireProtocol::OpenAiChatCompletions,
    root_url: "https://www.codebuddy.ai/v2",
    default_endpoint: "https://www.codebuddy.ai/v2/chat/completions",
    user_agent: Some(CODEBUDDY_USER_AGENT),
    models: CODEBUDDY_INTL_BUILTIN_MODELS,
};

pub const PROVIDER_COMMANDCODE_GOAT: BuiltinProviderDefaults = BuiltinProviderDefaults {
    id: "commandcode-goat",
    label: "CommandCode GOAT",
    protocol: nuo_inference_wire::WireProtocol::OpenAiResponses,
    root_url: "https://api.commandcode.ai/provider/v1",
    default_endpoint: "https://api.commandcode.ai/provider/v1/responses",
    user_agent: None,
    models: COMMANDCODE_BUILTIN_MODELS,
};

pub const PROVIDER_DEEPSEEK: BuiltinProviderDefaults = BuiltinProviderDefaults {
    id: "deepseek",
    label: "DeepSeek",
    protocol: nuo_inference_wire::WireProtocol::OpenAiResponses,
    root_url: "https://api.deepseek.com/v1",
    default_endpoint: "https://api.deepseek.com/v1/responses",
    user_agent: None,
    models: DEEPSEEK_BUILTIN_MODELS,
};

pub const PROVIDER_GLM_CN: BuiltinProviderDefaults = BuiltinProviderDefaults {
    id: "glm-cn",
    label: "GLM Coding Plan CN",
    protocol: nuo_inference_wire::WireProtocol::OpenAiChatCompletions,
    root_url: "https://open.bigmodel.cn/api/coding/paas/v4",
    default_endpoint: "https://open.bigmodel.cn/api/coding/paas/v4/chat/completions",
    user_agent: Some(ZCODE_USER_AGENT),
    models: GLM_CN_MODELS,
};

pub const PROVIDER_GITHUB_COPILOT: BuiltinProviderDefaults = BuiltinProviderDefaults {
    id: "github-copilot",
    label: "GitHub Copilot",
    protocol: nuo_inference_wire::WireProtocol::OpenAiChatCompletions,
    root_url: "https://api.githubcopilot.com",
    default_endpoint: "https://api.githubcopilot.com/chat/completions",
    user_agent: None,
    models: COPILOT_SEED_MODELS,
};

pub const PROVIDER_GOOGLE: BuiltinProviderDefaults = BuiltinProviderDefaults {
    id: "google",
    label: "Google AI Studio",
    protocol: nuo_inference_wire::WireProtocol::GoogleGenerateContent,
    root_url: "https://generativelanguage.googleapis.com/v1beta",
    default_endpoint: "https://generativelanguage.googleapis.com/v1beta",
    user_agent: None,
    models: GOOGLE_BUILTIN_MODELS,
};

pub const PROVIDER_GOOGLE_ANTIGRAVITY: BuiltinProviderDefaults = BuiltinProviderDefaults {
    id: "google-antigravity",
    label: "Google Antigravity",
    protocol: nuo_inference_wire::WireProtocol::GoogleGenerateContent,
    root_url: "https://daily-cloudcode-pa.googleapis.com",
    default_endpoint: "https://daily-cloudcode-pa.googleapis.com",
    user_agent: Some(ANTIGRAVITY_USER_AGENT),
    models: ANTIGRAVITY_OAUTH_MODELS,
};

pub const PROVIDER_KIMI_CODE: BuiltinProviderDefaults = BuiltinProviderDefaults {
    id: "kimi-code",
    label: "Kimi Code",
    protocol: nuo_inference_wire::WireProtocol::OpenAiChatCompletions,
    root_url: "https://api.kimi.com/coding/v1",
    default_endpoint: "https://api.kimi.com/coding/v1/chat/completions",
    user_agent: Some(OPENCODE_USER_AGENT),
    models: KIMI_CODE_MODELS,
};

pub const PROVIDER_OPENAI: BuiltinProviderDefaults = BuiltinProviderDefaults {
    id: "openai",
    label: "OpenAI Platform",
    protocol: nuo_inference_wire::WireProtocol::OpenAiChatCompletions,
    root_url: "https://api.openai.com/v1",
    default_endpoint: "https://api.openai.com/v1/chat/completions",
    user_agent: None,
    models: OPENAI_BUILTIN_MODELS,
};

pub const PROVIDER_OPENCODE_GO: BuiltinProviderDefaults = BuiltinProviderDefaults {
    id: "opencode-go",
    label: "OpenCode Go",
    protocol: nuo_inference_wire::WireProtocol::OpenAiResponses,
    root_url: "https://opencode.ai/zen/go/v1",
    default_endpoint: "https://opencode.ai/zen/go/v1/responses",
    user_agent: Some(OPENCODE_USER_AGENT),
    models: OPENCODE_GO_MODELS,
};

pub const PROVIDER_OPENCODE_ZEN: BuiltinProviderDefaults = BuiltinProviderDefaults {
    id: "opencode-zen",
    label: "OpenCode Zen",
    protocol: nuo_inference_wire::WireProtocol::OpenAiChatCompletions,
    root_url: "https://opencode.ai/zen/v1",
    default_endpoint: "https://opencode.ai/zen/v1/chat/completions",
    user_agent: Some(OPENCODE_USER_AGENT),
    models: OPENCODE_ZEN_MODELS,
};

pub const PROVIDER_OPENDESIGN_GO: BuiltinProviderDefaults = BuiltinProviderDefaults {
    id: "opendesign-go",
    label: "OpenDesign Go",
    protocol: nuo_inference_wire::WireProtocol::OpenAiChatCompletions,
    root_url: "https://amr-link.open-design.ai/v1",
    default_endpoint: "https://amr-link.open-design.ai/v1/chat/completions",
    user_agent: None,
    models: OPENDESIGN_GO_BUILTIN_MODELS,
};

pub const PROVIDER_OPENROUTER: BuiltinProviderDefaults = BuiltinProviderDefaults {
    id: "openrouter",
    label: "OpenRouter",
    protocol: nuo_inference_wire::WireProtocol::OpenAiChatCompletions,
    root_url: "https://openrouter.ai/api/v1",
    default_endpoint: "https://openrouter.ai/api/v1/chat/completions",
    user_agent: None,
    models: OPENROUTER_BUILTIN_MODELS,
};

pub const PROVIDER_QIANWEN: BuiltinProviderDefaults = BuiltinProviderDefaults {
    id: "qianwen",
    label: "QianwenAI Token Plan",
    protocol: nuo_inference_wire::WireProtocol::OpenAiChatCompletions,
    root_url: "https://token-plan.maas.qianwenaiapi.com/compatible-mode/v1",
    default_endpoint: "https://token-plan.maas.qianwenaiapi.com/compatible-mode/v1/chat/completions",
    user_agent: None,
    models: QIANWEN_BUILTIN_MODELS,
};

pub const PROVIDER_QODER: BuiltinProviderDefaults = BuiltinProviderDefaults {
    id: "qoder",
    label: "Qoder",
    protocol: nuo_inference_wire::WireProtocol::OpenAiChatCompletions,
    root_url: "https://api3.qoder.sh",
    default_endpoint: "https://api3.qoder.sh",
    user_agent: None,
    models: QODER_MODELS,
};

pub const PROVIDER_XAI: BuiltinProviderDefaults = BuiltinProviderDefaults {
    id: "xai",
    label: "xAI",
    protocol: nuo_inference_wire::WireProtocol::OpenAiChatCompletions,
    root_url: "https://api.x.ai/v1",
    default_endpoint: "https://api.x.ai/v1/chat/completions",
    user_agent: None,
    models: XAI_BUILTIN_MODELS,
};

/// All shipped built-in provider defaults.
pub const BUILTIN_PROVIDER_DEFAULTS: &[&BuiltinProviderDefaults] = &[
    &PROVIDER_OPENAI,
    &PROVIDER_CHATGPT_PLAN,
    &PROVIDER_ANTHROPIC,
    &PROVIDER_GOOGLE,
    &PROVIDER_GOOGLE_ANTIGRAVITY,
    &PROVIDER_GITHUB_COPILOT,
    &PROVIDER_XAI,
    &PROVIDER_DEEPSEEK,
    &PROVIDER_GLM_CN,
    &PROVIDER_KIMI_CODE,
    &PROVIDER_OPENROUTER,
    &PROVIDER_OPENDESIGN_GO,
    &PROVIDER_OPENCODE_ZEN,
    &PROVIDER_OPENCODE_GO,
    &PROVIDER_QODER,
    &PROVIDER_QIANWEN,
    &PROVIDER_COMMANDCODE_GOAT,
    &PROVIDER_CODEBUDDY_CN,
    &PROVIDER_CODEBUDDY_INTL,
];

/// Canonical human-readable display label for a model provider ID.
pub fn model_provider_label(id: &str) -> &'static str {
    let canonical = canonical_provider_id(id);
    let effective = canonical.as_deref().unwrap_or(id);
    BUILTIN_PROVIDER_DEFAULTS
        .iter()
        .find(|p| p.id == effective)
        .map(|p| p.label)
        .unwrap_or("Custom Provider")
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
            canonical_provider_id("commandcode").as_deref(),
            Some("commandcode-goat")
        );
        assert_eq!(
            canonical_provider_id("deepseek").as_deref(),
            Some("deepseek")
        );
        // Retired legacy ids must not resurrect.
        assert!(canonical_provider_id("zai-code").is_none());
        assert!(canonical_provider_id("commandcode-plan").is_none());
        assert!(canonical_provider_id("opencode-plan").is_none());
        assert_eq!(
            canonical_provider_id("google-ai-studio").as_deref(),
            Some("google")
        );
        assert_eq!(
            canonical_provider_id("ai-studio").as_deref(),
            Some("google")
        );
        assert_eq!(canonical_provider_id("gemini").as_deref(), Some("google"));
        // The retired OpenCode Console id must not resurrect (ADR-0079).
        assert!(canonical_provider_id("opencode").is_none());
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
        assert_eq!(model_provider_label("chatgpt-plan"), "ChatGPT Plan");
        assert_eq!(model_provider_label("commandcode-goat"), "CommandCode GOAT");
        assert_eq!(model_provider_label("opencode-go"), "OpenCode Go");
    }

    #[test]
    fn model_provider_ids_match_builtin_defaults() {
        let default_ids: Vec<&str> = BUILTIN_PROVIDER_DEFAULTS.iter().map(|p| p.id).collect();
        assert_eq!(
            default_ids.as_slice(),
            MODEL_PROVIDER_IDS,
            "MODEL_PROVIDER_IDS and BUILTIN_PROVIDER_DEFAULTS must stay 1:1 identical"
        );
        for defaults in BUILTIN_PROVIDER_DEFAULTS {
            assert!(!defaults.id.is_empty());
            assert!(!defaults.label.is_empty());
            assert!(!defaults.root_url.is_empty());
            assert!(!defaults.default_endpoint.is_empty());
        }
    }
}
