//! The built-in `google` provider preset: the native Google API, one key.

use nuo_contracts::reasoning::ReasoningSupport;
use nuo_contracts::{Model, WireProtocol};

use super::{effort_ladders, CatalogShape, ModelProviderSpec, RemoteCatalogSource};

/// The Gemini model ids the built-in `google` provider serves (native Google
/// API, one key). Each id exists in the model registry. The set is the
/// canonical text-generation family that Google plus common relays/中转站
/// advertise — image/embedding/video/audio-only models are excluded since an
/// agent only consumes the `generateContent` text surface.
pub use nuo_contracts::model_providers::GOOGLE_BUILTIN_MODELS;

/// Baseline capability metadata for the models this provider serves,
/// submitted to `nuo_contracts`'s registry at link time (see
/// [`nuo_contracts::model::BaselineModels`]).
pub const MODELS: &[Model] = &[
    // Google (native)
    // Native Google REST surface (`generateContent`/`streamGenerateContent`).
    // The id strings mirror Google's official naming and the ids relay/中转站
    // gateways advertise — so a relay-served model resolves to real metadata
    // instead of a generic fallback. See ADR for the configurable
    // `google_base_url`.
    Model {
        id: "gemini-3.8-flash",
        family: "google",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::GoogleGemini,
        model_guidance: "",
        effort_levels: effort_ladders::GEMINI_LEVEL,
    },
    Model {
        id: "gemini-3.7-flash",
        family: "google",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::GoogleGemini,
        model_guidance: "",
        effort_levels: effort_ladders::GEMINI_LEVEL,
    },
    Model {
        id: "gemini-3.5-flash",
        family: "google",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::GoogleGemini,
        model_guidance: "",
        effort_levels: effort_ladders::GEMINI_LEVEL,
    },
    Model {
        id: "gemini-3-pro-preview",
        family: "google",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::GoogleGemini,
        model_guidance: "",
        effort_levels: effort_ladders::GEMINI_LEVEL,
    },
    Model {
        id: "gemini-3-flash-preview",
        family: "google",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::GoogleGemini,
        model_guidance: "",
        effort_levels: effort_ladders::GEMINI_LEVEL,
    },
    Model {
        id: "gemini-3.1-pro-preview",
        family: "google",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::GoogleGemini,
        model_guidance: "",
        effort_levels: effort_ladders::GEMINI_LEVEL,
    },
    Model {
        // Custom-tools variant of 3.1 Pro Preview; serves the same REST surface.
        id: "gemini-3.1-pro-preview-customtools",
        family: "google",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::GoogleGemini,
        model_guidance: "",
        effort_levels: effort_ladders::GEMINI_LEVEL,
    },
    Model {
        id: "gemini-2.5-flash",
        family: "google",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::GoogleGemini,
        model_guidance: "",
        effort_levels: effort_ladders::GEMINI_BUDGET,
    },
    Model {
        id: "gemini-2.5-pro",
        family: "google",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::GoogleGemini,
        model_guidance: "",
        effort_levels: effort_ladders::GEMINI_BUDGET,
    },
    Model {
        id: "gemini-2.5-flash-lite",
        family: "google",
        context_window: 1_000_000,
        thinking: ReasoningSupport::None,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::GoogleGemini,
        model_guidance: "",
        effort_levels: &[],
    },
    Model {
        id: "gemini-2.0-flash",
        family: "google",
        context_window: 1_000_000,
        thinking: ReasoningSupport::None,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::GoogleGemini,
        model_guidance: "",
        effort_levels: &[],
    },
];

inventory::submit!(nuo_contracts::model::BaselineModels(MODELS));

fn prompt_cache_for_model(_: &str) -> nuo_contracts::PromptCacheSpec {
    nuo_contracts::PromptCacheSpec {
        modes: &[nuo_contracts::PromptCacheMode::Implicit],
        default_mode: Some(nuo_contracts::PromptCacheMode::Implicit),
        supported_retentions: &[],
        default_retention: None,
        disable_supported: false,
        routing_key_supported: false,
        max_breakpoints: None,
        min_cacheable_tokens: None,
        reports_reads: true,
        reports_writes: false,
        reports_misses: false,
    }
}

pub(crate) const MODEL_PROVIDER_SPEC: ModelProviderSpec = ModelProviderSpec {
    dialect: nuo_contracts::ProviderDialect::Standard,
    protocol_roots: std::borrow::Cow::Borrowed(&[]),
    catalog_root_url: None,
    prompt_cache: super::PromptCachePolicy::Compiled(prompt_cache_for_model),
    id: std::borrow::Cow::Borrowed("google"),
    baselines: MODELS,
    root_url: std::borrow::Cow::Borrowed("https://generativelanguage.googleapis.com/v1beta"),
    user_agent: None,
    protocol: WireProtocol::GoogleGemini,
    models: GOOGLE_BUILTIN_MODELS,
    catalog_source: RemoteCatalogSource::Endpoint(CatalogShape::Google),
    default_client_profile: nuo_contracts::ClientPreset::Native,
    client_profile_sensitive: false,
};
