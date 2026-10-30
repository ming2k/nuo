//! The built-in `google` provider preset: the native Google API, one key.

use nuo_inference_wire::WireProtocol;
use nuo_provider::Model;
use nuo_provider::reasoning::ReasoningSupport;

use nuo_provider::*;

/// The Gemini model ids the built-in `google` provider serves (native Google
/// API, one key). Each id exists in the model registry. The set is the
/// canonical text-generation family that Google plus common relays/中转站
/// advertise — image/embedding/video/audio-only models are excluded since an
/// agent only consumes the `generateContent` text surface.
pub use nuo_provider::model_providers::GOOGLE_BUILTIN_MODELS;

/// Baseline capability metadata for the models this provider serves,
/// submitted to `nuo_inference_wire`'s registry at link time (see
/// [`nuo_provider::model::BaselineModels`]).
pub const MODELS: &[Model] = &[
    // Google (native)
    // Native Google REST surface (`generateContent`/`streamGenerateContent`).
    // The id strings mirror Google's official naming and the ids relay/中转站
    // gateways advertise — so a relay-served model resolves to real metadata
    // instead of a generic fallback. See ADR for the configurable
    // `google_base_url`.
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
        id: "gemini-3.7-flash",
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
        id: "gemini-3.5-flash",
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
        id: "gemini-3-pro-preview",
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
        id: "gemini-3-flash-preview",
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
        id: "gemini-3.1-pro-preview",
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
        // Custom-tools variant of 3.1 Pro Preview; serves the same REST surface.
        id: "gemini-3.1-pro-preview-customtools",
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
        id: "gemini-3.1-flash-lite",
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
        id: "gemini-2.5-flash",
        family: "google",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::GoogleGenerateContent,
        effort_levels: effort_ladders::GEMINI_BUDGET,
        default_effort: Some(nuo_provider::Effort::High),
        wire_entities: None,
    },
    Model {
        id: "gemini-2.5-pro",
        family: "google",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::GoogleGenerateContent,
        effort_levels: effort_ladders::GEMINI_BUDGET,
        default_effort: Some(nuo_provider::Effort::High),
        wire_entities: None,
    },
    Model {
        id: "gemini-2.5-flash-lite",
        family: "google",
        context_window: 1_000_000,
        thinking: ReasoningSupport::None,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::GoogleGenerateContent,
        effort_levels: &[],
        default_effort: Some(nuo_provider::Effort::High),
        wire_entities: None,
    },
    Model {
        id: "gemini-2.0-flash",
        family: "google",
        context_window: 1_000_000,
        thinking: ReasoningSupport::None,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::GoogleGenerateContent,
        effort_levels: &[],
        default_effort: Some(nuo_provider::Effort::High),
        wire_entities: None,
    },
];

inventory::submit!(nuo_provider::model::BaselineModels(MODELS));

fn prompt_cache_for_model(_: &str) -> nuo_provider::PromptCacheSpec {
    nuo_provider::PromptCacheSpec {
        modes: &[nuo_provider::PromptCacheMode::Implicit],
        default_mode: Some(nuo_provider::PromptCacheMode::Implicit),
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

pub const MODEL_PROVIDER_SPEC: ModelProviderSpec = ModelProviderSpec {
    dialect: nuo_provider::ProviderDialect::Standard,
    protocol_roots: std::borrow::Cow::Borrowed(&[
        (
            WireProtocol::OpenAiChatCompletions,
            std::borrow::Cow::Borrowed("https://generativelanguage.googleapis.com/v1beta/openai"),
        ),
        (
            WireProtocol::GoogleInteractions,
            std::borrow::Cow::Borrowed("https://generativelanguage.googleapis.com/v1beta"),
        ),
    ]),
    catalog_root_url: None,
    prompt_cache: nuo_provider::PromptCachePolicy::Compiled(prompt_cache_for_model),
    id: std::borrow::Cow::Borrowed("google"),
    baselines: MODELS,
    root_url: std::borrow::Cow::Borrowed("https://generativelanguage.googleapis.com/v1beta"),
    user_agent: None,
    default_headers: std::borrow::Cow::Borrowed(&[]),
    protocol: WireProtocol::GoogleGenerateContent,
    models: GOOGLE_BUILTIN_MODELS,
    catalog_source: RemoteCatalogSource::Endpoint(CatalogShape::Google),
    quota: None,
    display_policy: nuo_provider::DisplayPolicy::Native,
    responses_state: nuo_provider::ResponsesState::Stateless,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn google_provider_spec_validates_and_resolves_protocols() {
        assert!(MODEL_PROVIDER_SPEC.validate().is_ok());
        assert_eq!(
            MODEL_PROVIDER_SPEC
                .endpoint(WireProtocol::GoogleGenerateContent)
                .unwrap(),
            "https://generativelanguage.googleapis.com/v1beta"
        );
        assert_eq!(
            MODEL_PROVIDER_SPEC
                .endpoint(WireProtocol::OpenAiChatCompletions)
                .unwrap(),
            "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions"
        );
        assert_eq!(
            MODEL_PROVIDER_SPEC
                .endpoint(WireProtocol::GoogleInteractions)
                .unwrap(),
            "https://generativelanguage.googleapis.com/v1beta/interactions"
        );

        let argon = nuo_provider::model::resolve("gemini-4-argon");
        assert_eq!(argon.id, "gemini-4-argon");
        assert_eq!(argon.context_window, 1_000_000);
        assert_eq!(argon.protocol, WireProtocol::GoogleGenerateContent);
        assert!(argon.tool_call);
        assert!(argon.thinking.reasons());
        assert_eq!(argon.effort_levels, effort_ladders::GEMINI_LEVEL);

        let flash38 = nuo_provider::model::resolve("gemini-3.8-flash");
        assert_eq!(flash38.id, "gemini-3.8-flash");
        assert_eq!(flash38.context_window, 1_000_000);
        assert_eq!(flash38.protocol, WireProtocol::GoogleGenerateContent);
        assert!(flash38.tool_call);
        assert!(flash38.thinking.reasons());
    }
}
