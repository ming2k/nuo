//! Curated connection templates and their preset rows.
//!
//! Technical defaults (`id`, `label`, `protocol`, `default_url`, `user_agent`,
//! seed `models`) are projected directly from
//! [`nuo_protocol::model_providers::BuiltinProviderDefaults`] so this table owns
//! only UI presentation prose and auth flow selection.

use nuo_protocol::model_providers as mp;

use super::*;

pub const PROVIDER_PRESETS: &[ConnectionTemplate] = &[
    ConnectionTemplate::curated(
        &mp::PROVIDER_ANTHROPIC,
        "Anthropic's official API for flagship Claude models with advanced reasoning; sign in with an Anthropic API key.",
        nuo_protocol::ConnectionAuth::Static,
    ),
    ConnectionTemplate::curated(
        &mp::PROVIDER_CHATGPT_PLAN,
        "Uses your ChatGPT Plus or Pro subscription for Codex and flagship GPT models; authorizes in the browser, no API key.",
        nuo_protocol::ConnectionAuth::negotiated_const("chatgpt-plan"),
    ),
    ConnectionTemplate::curated(
        &mp::PROVIDER_COMMANDCODE_GOAT,
        "Command Code Provider API serving Claude, GPT, DeepSeek, and top open models; sign in with your Command Code key.",
        nuo_protocol::ConnectionAuth::Static,
    ),
    ConnectionTemplate::curated(
        &mp::PROVIDER_DEEPSEEK,
        "DeepSeek's platform API with high-performance reasoning and coding models; sign in with a DeepSeek API key.",
        nuo_protocol::ConnectionAuth::Static,
    ),
    ConnectionTemplate::curated(
        &mp::PROVIDER_GLM_CN,
        "Zhipu's GLM Coding Plan on the domestic BigModel platform, serving the flagship GLM-5 family; sign in with a plan API key.",
        nuo_protocol::ConnectionAuth::Static,
    ),
    ConnectionTemplate::curated(
        &mp::PROVIDER_GITHUB_COPILOT,
        "Your GitHub Copilot subscription, serving multi-vendor coding and reasoning models; authorizes on the device via GitHub.",
        nuo_protocol::ConnectionAuth::negotiated_const("copilot"),
    ),
    ConnectionTemplate::curated(
        &mp::PROVIDER_GOOGLE,
        "Google AI Studio / developer API covering the full Gemini range; sign in with a Google API key.",
        nuo_protocol::ConnectionAuth::Static,
    ),
    ConnectionTemplate::curated(
        &mp::PROVIDER_GOOGLE_ANTIGRAVITY,
        "Your Google One AI Premium subscription for flagship Gemini plus companion Claude models; authorizes in the browser.",
        nuo_protocol::ConnectionAuth::negotiated_const("google-antigravity"),
    ),
    ConnectionTemplate::curated(
        &mp::PROVIDER_KIMI_CODE,
        "Moonshot's Kimi Coding Plan with long-context coding and reasoning models; sign in with a plan API key.",
        nuo_protocol::ConnectionAuth::Static,
    ),
    ConnectionTemplate::curated(
        &mp::PROVIDER_OPENAI,
        "OpenAI's platform API for official flagship GPT and frontier reasoning models; sign in with an OpenAI API key.",
        nuo_protocol::ConnectionAuth::Static,
    ),
    ConnectionTemplate::curated(
        &mp::PROVIDER_OPENCODE_GO,
        "OpenCode Go subscription for open coding models; sign in with your OpenCode Go API key.",
        nuo_protocol::ConnectionAuth::Static,
    ),
    ConnectionTemplate::curated(
        &mp::PROVIDER_OPENCODE_ZEN,
        "OpenCode Zen relay with pay-as-you-go billing for frontier coding models; sign in with your OpenCode API key.",
        nuo_protocol::ConnectionAuth::Static,
    ),
    ConnectionTemplate::curated(
        &mp::PROVIDER_OPENDESIGN_GO,
        "OpenDesign Cloud API subscription serving the Design Plan's DeepSeek, Claude, GPT, GLM, Kimi, MiMo, MiniMax, and Hunyuan models; sign in with an OpenDesign API key (sk-amr-…).",
        nuo_protocol::ConnectionAuth::Static,
    ),
    ConnectionTemplate::curated(
        &mp::PROVIDER_OPENROUTER,
        "OpenRouter's unified gateway for Nex and hundreds of other models; sign in with an OpenRouter API key.",
        nuo_protocol::ConnectionAuth::Static,
    ),
    ConnectionTemplate::curated(
        &mp::PROVIDER_QIANWEN,
        "Alibaba's QianwenAI Platform Token Plan serving Qwen, DeepSeek, GLM, and Kimi over one Credits-billed key; sign in with your plan API key (sk-sp-…).",
        nuo_protocol::ConnectionAuth::Static,
    ),
    ConnectionTemplate::curated(
        &mp::PROVIDER_QODER,
        "Alibaba's Qoder subscription for Qoder3 and Qwen coding models with COSY-signed inference; paste a personal access token (pt-…) or authorize via device flow.",
        nuo_protocol::ConnectionAuth::negotiated_const("qoder"),
    )
    .with_url_hint("https://api3.qoder.sh/algo/api/v2/service/pro/sse/agent_chat_generation"),
    ConnectionTemplate::curated(
        &mp::PROVIDER_CODEBUDDY_CN,
        "Tencent AI CodeBuddy domestic service with Hunyuan, DeepSeek, Kimi, and GLM models; sign in with your Tencent account in the browser, or paste a CLI bearer token / API key.",
        nuo_protocol::ConnectionAuth::negotiated_const("codebuddy-cn"),
    ),
    ConnectionTemplate::curated(
        &mp::PROVIDER_CODEBUDDY_INTL,
        "Tencent CodeBuddy AI international service for global users; sign in with your CodeBuddy AI account in the browser, or paste an API key.",
        nuo_protocol::ConnectionAuth::negotiated_const("codebuddy-intl"),
    ),
    ConnectionTemplate::curated(
        &mp::PROVIDER_XAI,
        "Your SuperGrok or X Premium subscription for flagship Grok reasoning models; authorizes in the browser.",
        nuo_protocol::ConnectionAuth::negotiated_const("xai"),
    ),
];

/// The `custom` provider's template — the generic bring-your-own-endpoint
/// definition. It is intentionally separate from
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
    protocol: WireProtocol::OpenAiChatCompletions,
    models: &[],
    needs_url: true,
    url_hint: "https://relay.example.com/v1/chat/completions",
    needs_model: true,
    default_url: None,
    user_agent: None,
    auth: nuo_protocol::ConnectionAuth::Static,
};
