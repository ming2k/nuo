//! The `antigravity-oauth` provider preset: Google-native models served
//! via Google Antigravity OAuth subscription.

use nuo_inference_wire::WireProtocol;
use nuo_provider::Model;
use nuo_provider::reasoning::ReasoningSupport;

use nuo_provider::*;

/// Models served by Google Antigravity OAuth (Google One AI Premium / Pro).
///
/// This is the **offline floor** (Layer 3 of the three-layer catalog): the live
/// catalog at `/v1internal:fetchAvailableModels` is authoritative for a
/// signed-in account and may add generations this seed does not list. The seed
/// stays current enough that a connection with no network still offers the
/// current tiered generation.
pub use nuo_provider::model_providers::ANTIGRAVITY_OAUTH_MODELS;

/// Baseline capability metadata for the models this provider serves.
pub const MODELS: &[Model] = &[
    Model {
        id: "gemini-3.8-flash",
        family: "google",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::GoogleGenerateContent,
        effort_levels: effort_ladders::GEMINI_LEVEL,
        default_effort: Some(nuo_provider::Effort::High),
        wire_entities: None,
    },
    Model {
        id: "claude-sonnet-4-6",
        family: "claude",
        context_window: 1_000_000,
        thinking: ReasoningSupport::Adaptive,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::GoogleGenerateContent,
        effort_levels: effort_ladders::CLAUDE_NO_XHIGH,
        default_effort: Some(nuo_provider::Effort::High),
        wire_entities: None,
    },
    Model {
        id: "claude-opus-4-6",
        family: "claude",
        context_window: 1_000_000,
        thinking: ReasoningSupport::Adaptive,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::GoogleGenerateContent,
        effort_levels: effort_ladders::CLAUDE_NO_XHIGH,
        default_effort: Some(nuo_provider::Effort::High),
        wire_entities: None,
    },
    Model {
        id: "claude-opus-5-5",
        family: "claude",
        context_window: 1_000_000,
        thinking: ReasoningSupport::Adaptive,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::GoogleGenerateContent,
        effort_levels: effort_ladders::CLAUDE_NO_XHIGH,
        default_effort: Some(nuo_provider::Effort::High),
        wire_entities: None,
    },
    Model {
        id: "claude-sonnet-5-5",
        family: "claude",
        context_window: 1_000_000,
        thinking: ReasoningSupport::Adaptive,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::GoogleGenerateContent,
        effort_levels: effort_ladders::CLAUDE_NO_XHIGH,
        default_effort: Some(nuo_provider::Effort::High),
        wire_entities: None,
    },
    Model {
        id: "gpt-oss-120b-medium",
        family: "gpt",
        context_window: 128_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::GoogleGenerateContent,
        effort_levels: effort_ladders::OPENAI_GPT,
        default_effort: Some(nuo_provider::Effort::Medium),
        wire_entities: None,
    },
    Model {
        id: "gemini-3.5-flash-lite",
        family: "google",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::GoogleGenerateContent,
        effort_levels: effort_ladders::GEMINI_LEVEL,
        default_effort: Some(nuo_provider::Effort::High),
        wire_entities: None,
    },
    Model {
        id: "gemini-4-argon",
        family: "google",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::GoogleGenerateContent,
        effort_levels: effort_ladders::GEMINI_LEVEL,
        default_effort: Some(nuo_provider::Effort::High),
        wire_entities: None,
    },
    Model {
        id: "gemini-3.1-pro-high",
        family: "google",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::GoogleGenerateContent,
        effort_levels: &[],
        default_effort: Some(nuo_provider::Effort::High),
        wire_entities: None,
    },
    Model {
        id: "gemini-3.1-pro-low",
        family: "google",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::GoogleGenerateContent,
        effort_levels: &[],
        default_effort: Some(nuo_provider::Effort::High),
        wire_entities: None,
    },
    Model {
        id: "gemini-3-flash",
        family: "google",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::GoogleGenerateContent,
        effort_levels: &[],
        default_effort: Some(nuo_provider::Effort::High),
        wire_entities: None,
    },
];

inventory::submit!(nuo_provider::model::BaselineModels(MODELS));

pub const ANTIGRAVITY_VERSION: &str = "1.5.3";
/// The `cl=` counter the shipped 1.5.3 binary stamps (live capture 2026-11-07).
pub const ANTIGRAVITY_BUILD_NUMBER: &str = "1016078699";
pub const ANTIGRAVITY_USER_AGENT: &str = "antigravity/cli/1.5.3 \
     (aidev_client; os_type=linux; arch=amd64; cl=1016078699; auth_method=consumer)";

/// netune TLS/HTTP2 profile name for the `agy` wire identity: a Go-shaped
/// hello (`h2,http/1.1` ALPN, no GREASE) paired with Go's HTTP/2 framing
/// (see `netune::Preset::AntigravityCli` / `H2Fingerprint::go`).
pub const ANTIGRAVITY_TLS_PROFILE: &str = "antigravity-cli";

pub const MODEL_PROVIDER_SPEC: ModelProviderSpec = ModelProviderSpec {
    dialect: nuo_provider::ProviderDialect::Antigravity,
    protocol_roots: std::borrow::Cow::Borrowed(&[]),
    catalog_root_url: None,
    prompt_cache: nuo_provider::PromptCachePolicy::Compiled(nuo_provider::unsupported_prompt_cache),
    id: std::borrow::Cow::Borrowed("google-antigravity"),
    baselines: MODELS,
    root_url: std::borrow::Cow::Borrowed("https://daily-cloudcode-pa.googleapis.com"),
    user_agent: Some(std::borrow::Cow::Borrowed(ANTIGRAVITY_USER_AGENT)),
    default_headers: std::borrow::Cow::Borrowed(&[]),
    protocol: WireProtocol::GoogleGenerateContent,
    catalog_source: RemoteCatalogSource::Endpoint(CatalogShape::GoogleCloudCode),
    quota: Some(nuo_provider::QuotaPort::Antigravity),
    display_policy: nuo_provider::DisplayPolicy::Native,
    responses_state: nuo_provider::ResponsesState::Stateless,
    models: ANTIGRAVITY_OAUTH_MODELS,
};
