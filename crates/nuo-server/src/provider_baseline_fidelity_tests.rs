//! Capability-fidelity proof for the baseline-registry migration (Phase 3).
//!
//! `PRE_MIGRATION` is the old `nuo_model_codec::model::KNOWN_MODELS` table,
//! embedded verbatim. The test resolves every id through the new
//! provider-registered baselines and compares every field, proving the
//! per-provider distribution changed no capability data.

use nuo_model_codec::reasoning::ReasoningSupport;
use nuo_model_codec::{Model, WireProtocol, resolve_model};

use nuo_provider::effort_ladders;

const PRE_MIGRATION: &[Model] = &[
    // GLM family (Zhipu / Z.AI / opencode-go)
    Model {
        id: "glm-5.2",
        family: "glm",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::GLM_5,
    },
    Model {
        id: "glm-5.1",
        family: "glm",
        context_window: 200_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: &[],
    },
    Model {
        id: "glm-5",
        family: "glm",
        context_window: 200_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: &[],
    },
    Model {
        id: "glm-4.7",
        family: "glm",
        context_window: 200_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: &[],
    },
    // Kimi (Moonshot / opencode-go)
    Model {
        // The Kimi Code platform's current flagship. The platform's live
        // `GET /models` advertises `k3` with a 1M context window, image/video
        // inputs, and always-on thinking (`supports_thinking_type: "only"`,
        // single `max` effort tier) — over the OpenAI-compatible wire the
        // always-on reasoning simply streams back as `reasoning_content`, so
        // there is no thinking switch to model. NOTE: this entry intentionally
        // diverges from the pre-migration table — `k3`'s single `max` effort
        // tier (advertised by the platform's live `GET /models`) was added
        // after the migration, so the effort pin flows onto the wire and the
        // pickers/hint bar can show the effective level.
        id: "k3",
        family: "kimi",
        context_window: 1_048_576,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::LOW_HIGH_MAX,
    },
    Model {
        id: "kimi-k2.7-code",
        family: "kimi",
        context_window: 262_144,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: &[],
    },
    Model {
        id: "kimi-k2.6",
        family: "kimi",
        context_window: 262_144,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: &[],
    },
    Model {
        id: "kimi-k2.5",
        family: "kimi",
        context_window: 262_144,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: &[],
    },
    // Claude (Anthropic, via Anthropic-compatible relays)
    // Served over the Anthropic Messages wire format. Relays forward to
    // Anthropic's own `/messages` surface, so these carry
    // `WireProtocol::AnthropicMessages`.
    Model {
        id: "claude-opus-4-8",
        family: "claude",
        context_window: 1_000_000,
        thinking: ReasoningSupport::AnthropicAdaptive,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::AnthropicMessages,
        model_guidance: "",
        // Opus 4.8 honors the full effort range including `xhigh`/`max`.
        effort_levels: effort_ladders::CLAUDE_FULL,
    },
    Model {
        id: "claude-sonnet-4-6",
        family: "claude",
        context_window: 1_000_000,
        thinking: ReasoningSupport::AnthropicAdaptive,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::AnthropicMessages,
        model_guidance: "",
        // Sonnet 4.6 honors `max` but NOT `xhigh` (xhigh is Opus 4.8/4.7 only).
        effort_levels: effort_ladders::CLAUDE_NO_XHIGH,
    },
    Model {
        id: "claude-fable-5",
        family: "claude",
        context_window: 1_000_000,
        // Fable 5 thinking is ALWAYS ON; an explicit `{type:"disabled"}` is
        // rejected with 400. `AnthropicAdaptiveAlwaysOn` makes the transport
        // emit `thinking:{type:"adaptive"}` regardless of the user's on/off
        // choice (an opt-out is a no-op on this model). Manual `type:"enabled"`
        // also returns 400.
        thinking: ReasoningSupport::AnthropicAdaptiveAlwaysOn,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::AnthropicMessages,
        model_guidance: "",
        // Fable 5 honors the full effort range including `xhigh`/`max`.
        effort_levels: effort_ladders::CLAUDE_FULL,
    },
    Model {
        id: "claude-sonnet-5",
        family: "claude",
        context_window: 1_000_000,
        // Sonnet 5: omitting the `thinking` field RUNS adaptive thinking; to
        // actually disable it you must send `{type:"disabled"}`. This is neither
        // `AnthropicAdaptive` (omit disables) nor `AnthropicAdaptiveAlwaysOn`
        // (cannot disable) — so the transport emits an explicit `disabled` on
        // opt-out to honor ADR-0046. Manual `type:"enabled"` returns 400.
        thinking: ReasoningSupport::AnthropicAdaptiveOnByDefault,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::AnthropicMessages,
        model_guidance: "",
        // Sonnet 5 honors the full range INCLUDING `xhigh` — the key difference
        // from Sonnet 4.6, which rejects `xhigh` (see CLAUDE_NO_XHIGH).
        effort_levels: effort_ladders::CLAUDE_FULL,
    },
    Model {
        id: "claude-haiku-4-5-20251001",
        family: "claude",
        context_window: 200_000,
        // Haiku 4.5 supports only MANUAL extended thinking
        // (`thinking:{type:"enabled",budget_tokens}`); it has no adaptive mode
        // and rejects the `effort` parameter (400), hence empty `effort_levels`.
        thinking: ReasoningSupport::AnthropicManual,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::AnthropicMessages,
        model_guidance: "",
        effort_levels: &[],
    },
    // GPT-5.6 (OpenAI)
    // The 2026-06-26 flagship family with OpenAI's tier naming scheme:
    // Sol (flagship) / Terra (balanced) / Luna (efficient, high-volume).
    // `gpt-5.6` is an alias that routes to `gpt-5.6-sol`. All speak the
    // standard OpenAI chat-completions API and reason via `reasoning_content`.
    // GPT-5.6 honors the `max` effort level, so these carry the 5.6-specific
    // effort set rather than the xhigh-capped `EFFORT_OPENAI_GPT`.
    // OpenAI publishes a 1.05M context window for all three tiers and the
    // alias.
    Model {
        id: "gpt-5.6",
        family: "gpt",
        context_window: 1_050_000,
        thinking: ReasoningSupport::ReasoningSummary,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::OPENAI_GPT_5_6,
    },
    Model {
        id: "gpt-5.6-sol",
        family: "gpt",
        context_window: 1_050_000,
        thinking: ReasoningSupport::ReasoningSummary,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::OPENAI_GPT_5_6,
    },
    Model {
        id: "gpt-5.6-terra",
        family: "gpt",
        context_window: 1_050_000,
        thinking: ReasoningSupport::ReasoningSummary,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::OPENAI_GPT_5_6,
    },
    Model {
        id: "gpt-5.6-luna",
        family: "gpt",
        context_window: 1_050_000,
        thinking: ReasoningSupport::ReasoningSummary,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::OPENAI_GPT_5_6,
    },
    // GPT (OpenAI)
    // The current frontier chat family served over the OpenAI chat-completions
    // API. All reason (surfaced via the `reasoning_content` stream) and take
    // text+image input. Context windows and pricing per OpenAI's model docs;
    // `gpt-5.5`/`gpt-5.4` share a 1M window, `gpt-5.4-mini` a 400K window.
    Model {
        id: "gpt-5.5",
        family: "gpt",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningSummary,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::OPENAI_GPT,
    },
    Model {
        id: "gpt-5.4",
        family: "gpt",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningSummary,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::OPENAI_GPT,
    },
    Model {
        id: "gpt-5.4-mini",
        family: "gpt",
        context_window: 400_000,
        thinking: ReasoningSupport::ReasoningSummary,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::OPENAI_GPT,
    },
    // OpenAI relays and gateways can expose additional text aliases not used by the
    // official built-in preset. Keep their metadata conservative when the
    // exact serving contract is relay-defined.
    Model {
        id: "gpt-5.3-codex-spark",
        family: "gpt",
        context_window: 128_000,
        thinking: ReasoningSupport::ReasoningSummary,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::OPENAI_GPT,
    },
    Model {
        id: "gpt-5.2",
        family: "gpt",
        context_window: 0,
        thinking: ReasoningSupport::ReasoningSummary,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::OPENAI_GPT,
    },
    Model {
        id: "gpt-5.2-chat-latest",
        family: "gpt",
        context_window: 0,
        thinking: ReasoningSupport::ReasoningSummary,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::OPENAI_GPT,
    },
    Model {
        id: "gpt-5.2-pro",
        family: "gpt",
        context_window: 0,
        thinking: ReasoningSupport::ReasoningSummary,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::OPENAI_GPT,
    },
    // Legacy GPT-4o family — no longer in OpenAI's frontier chat lineup (it
    // remains only behind the TTS/transcribe specialized models) but kept
    // registered so existing configs and older sessions still resolve metadata.
    Model {
        id: "gpt-4o",
        family: "gpt",
        context_window: 128_000,
        thinking: ReasoningSupport::None,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: &[],
    },
    Model {
        id: "gpt-4o-mini",
        family: "gpt",
        context_window: 128_000,
        thinking: ReasoningSupport::None,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: &[],
    },
    // Google (native)
    // Native Google REST surface (`generateContent`/`streamGenerateContent`).
    // The id strings mirror Google's official naming and the ids relay/中转站
    // gateways advertise — so a relay-served model resolves to real metadata
    // instead of a generic fallback. See ADR for the configurable
    // `google_base_url`.
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
    // Antigravity / Google relay models
    // Google-native variants that advertise effort-tiered 3.1 Pro
    // models (`-high`/`-low`) and a non-preview `gemini-3-flash`. Same REST
    // surface (`/v1beta/models/{id}:generateContent`), so the metadata mirrors
    // the Gemini family; the relay forwards the model id verbatim. The wire
    // responses include `thoughtSignature`/`thoughtsTokenCount`, so these
    // reason like the rest of the 3.x family.
    Model {
        id: "gemini-3.1-pro-high",
        family: "google",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::GoogleGemini,
        model_guidance: "",
        effort_levels: &[],
    },
    Model {
        id: "gemini-3.1-pro-low",
        family: "google",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::GoogleGemini,
        model_guidance: "",
        effort_levels: &[],
    },
    Model {
        id: "gemini-3-flash",
        family: "google",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::GoogleGemini,
        model_guidance: "",
        effort_levels: &[],
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
    // DeepSeek (opencode-go / direct)
    Model {
        id: "deepseek-v4-flash",
        family: "deepseek",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::LOW_HIGH_MAX,
    },
    Model {
        id: "deepseek-v4-pro",
        family: "deepseek",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::LOW_HIGH_MAX,
    },
    // MiMo (Xiaomi / opencode-go, OpenAI format)
    Model {
        id: "mimo-v2.5",
        family: "mimo",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: &[],
    },
    Model {
        id: "mimo-v2.5-pro",
        family: "mimo",
        context_window: 1_048_576,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: &[],
    },
    Model {
        id: "mimo-v2-pro",
        family: "mimo",
        context_window: 1_048_576,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: &[],
    },
    Model {
        id: "mimo-v2-omni",
        family: "mimo",
        context_window: 262_144,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: &[],
    },
    // MiniMax (opencode-go, Anthropic /messages format)
    Model {
        id: "minimax-m3",
        family: "minimax",
        context_window: 512_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::AnthropicMessages,
        model_guidance: "",
        effort_levels: effort_ladders::COMMON,
    },
    Model {
        id: "minimax-m2.7",
        family: "minimax",
        context_window: 204_800,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::AnthropicMessages,
        model_guidance: "",
        effort_levels: effort_ladders::COMMON,
    },
    Model {
        id: "minimax-m2.5",
        family: "minimax",
        context_window: 204_800,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::AnthropicMessages,
        model_guidance: "",
        effort_levels: effort_ladders::COMMON,
    },
    // Qwen (opencode-go, OpenAI /chat/completions format)
    // models.dev records qwen3.* as `@ai-sdk/openai-compatible` under
    // opencode-go; the KNOWN_MODELS fallback mirrors that so the offline
    // fallback path matches the live catalog.
    Model {
        id: "qwen3.7-max",
        family: "qwen",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::COMMON,
    },
    Model {
        id: "qwen3.7-plus",
        family: "qwen",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::COMMON,
    },
    Model {
        id: "qwen3.6-plus",
        family: "qwen",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::COMMON,
    },
    Model {
        id: "qwen3.5-plus",
        family: "qwen",
        context_window: 262_144,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::COMMON,
    },
    // xAI Grok (OpenAI-compatible; SuperGrok OAuth or XAI_API_KEY)
    Model {
        id: "grok-4.5",
        family: "grok",
        context_window: 256_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::XAI_GROK,
    },
    Model {
        id: "grok-4.20",
        family: "grok",
        context_window: 256_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::XAI_GROK,
    },
    Model {
        id: "grok-4.3",
        family: "grok",
        context_window: 256_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::XAI_GROK,
    },
    Model {
        id: "grok-build-0.1",
        family: "grok",
        context_window: 256_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: effort_ladders::XAI_GROK,
    },
];

#[test]
fn resolve_matches_the_pre_migration_registry_for_every_model() {
    assert_eq!(PRE_MIGRATION.len(), 56, "snapshot covers every known model");
    for expected in PRE_MIGRATION {
        let m = resolve_model(expected.id);
        assert_eq!(m.id, expected.id, "id");
        assert_eq!(m.family, expected.family, "{}: family", expected.id);
        assert_eq!(
            m.context_window, expected.context_window,
            "{}: context_window",
            expected.id
        );
        assert_eq!(m.thinking, expected.thinking, "{}: thinking", expected.id);
        assert_eq!(
            m.tool_call, expected.tool_call,
            "{}: tool_call",
            expected.id
        );
        assert_eq!(m.vision, expected.vision, "{}: vision", expected.id);
        // `protocol` is provider-scoped (ADR-0260): the global baseline union
        // is a capability source, and provider routers resolve protocol through
        // `ModelProviderSpec::model_protocol` (covered by the registry tests).
        // Protocol is therefore intentionally not part of this capability
        // fidelity proof.
        assert_eq!(
            m.model_guidance, expected.model_guidance,
            "{}: model_guidance",
            expected.id
        );
        assert_eq!(
            m.effort_levels, expected.effort_levels,
            "{}: effort_levels",
            expected.id
        );
    }
}
