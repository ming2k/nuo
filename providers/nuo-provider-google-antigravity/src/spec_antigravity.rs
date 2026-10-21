//! The `antigravity-oauth` provider preset: Google-native models served
//! via Google Antigravity OAuth subscription.

use nuo_inference_wire::reasoning::ReasoningSupport;
use nuo_inference_wire::{Model, WireProtocol};

use nuo_provider::*;

/// Models served by Google Antigravity OAuth (Google One AI Premium / Pro).
///
/// This is the **offline floor** (Layer 3 of the three-layer catalog): the live
/// catalog at `/v1internal:fetchAvailableModels` is authoritative for a
/// signed-in account and may add generations this seed does not list. The seed
/// stays current enough that a connection with no network still offers the
/// current tiered generation.
pub use nuo_inference_wire::model_providers::ANTIGRAVITY_OAUTH_MODELS;

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
        model_guidance: "",
        effort_levels: effort_ladders::GEMINI_LEVEL,
        wire_entities: None,
    },
    Model {
        id: "claude-sonnet-4-6",
        family: "claude",
        context_window: 1_000_000,
        thinking: ReasoningSupport::AnthropicAdaptive,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::GoogleGenerateContent,
        model_guidance: "",
        effort_levels: effort_ladders::CLAUDE_NO_XHIGH,
        wire_entities: None,
    },
    Model {
        id: "claude-opus-4-6",
        family: "claude",
        context_window: 1_000_000,
        thinking: ReasoningSupport::AnthropicAdaptive,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::GoogleGenerateContent,
        model_guidance: "",
        effort_levels: effort_ladders::CLAUDE_NO_XHIGH,
        wire_entities: None,
    },
    Model {
        id: "claude-opus-5-5",
        family: "claude",
        context_window: 1_000_000,
        thinking: ReasoningSupport::AnthropicAdaptive,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::GoogleGenerateContent,
        model_guidance: "",
        effort_levels: effort_ladders::CLAUDE_NO_XHIGH,
        wire_entities: None,
    },
    Model {
        id: "claude-sonnet-5-5",
        family: "claude",
        context_window: 1_000_000,
        thinking: ReasoningSupport::AnthropicAdaptive,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::GoogleGenerateContent,
        model_guidance: "",
        effort_levels: effort_ladders::CLAUDE_NO_XHIGH,
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
        model_guidance: "",
        effort_levels: effort_ladders::OPENAI_GPT,
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
        model_guidance: "",
        effort_levels: effort_ladders::GEMINI_LEVEL,
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
        model_guidance: "",
        effort_levels: effort_ladders::GEMINI_LEVEL,
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
        model_guidance: "",
        effort_levels: &[],
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
        model_guidance: "",
        effort_levels: &[],
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
        model_guidance: "",
        effort_levels: &[],
        wire_entities: None,
    },
];

inventory::submit!(nuo_inference_wire::model::BaselineModels(MODELS));

pub const MODEL_PROVIDER_SPEC: ModelProviderSpec = ModelProviderSpec {
    dialect: nuo_inference_wire::ProviderDialect::Antigravity,
    protocol_roots: std::borrow::Cow::Borrowed(&[]),
    catalog_root_url: None,
    prompt_cache: nuo_provider::PromptCachePolicy::Compiled(nuo_provider::unsupported_prompt_cache),
    id: std::borrow::Cow::Borrowed("google-antigravity"),
    baselines: MODELS,
    root_url: std::borrow::Cow::Borrowed("https://daily-cloudcode-pa.googleapis.com"),
    user_agent: Some(std::borrow::Cow::Borrowed(
        nuo_inference_wire::client_identity::ANTIGRAVITY_USER_AGENT,
    )),
    protocol: WireProtocol::GoogleGenerateContent,
    catalog_source: RemoteCatalogSource::Endpoint(CatalogShape::GoogleCloudCode),
    default_client_profile: nuo_inference_wire::ClientPreset::Antigravity,
    client_profile_sensitive: true,
    quota: Some(nuo_provider::QuotaPort::Antigravity),
    display_policy: nuo_provider::DisplayPolicy::Native,
    models: ANTIGRAVITY_OAUTH_MODELS,
};
