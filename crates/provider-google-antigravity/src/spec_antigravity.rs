//! The `antigravity-oauth` provider preset: Google-native models served
//! via Google Antigravity OAuth subscription.

use protocol::Model;
use protocol::reasoning::ReasoningSupport;

use ::provider::*;
use protocol::surface::RemoteCatalogSource;

/// Models served by Google Antigravity OAuth (Google One AI Premium / Pro).
///
/// This is the **offline floor** (Layer 3 of the three-layer catalog): the live
/// catalog at `/v1internal:fetchAvailableModels` is authoritative for a
/// signed-in account and may add generations this seed does not list. The seed
/// stays current enough that a connection with no network still offers the
/// current tiered generation.
pub use protocol::model_providers::ANTIGRAVITY_OAUTH_MODELS;

/// Baseline capability metadata for the models this provider serves.
pub const MODELS: &[Model] = &[
    Model {
        id: "gemini-4-neon",
        family: "google",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        effort_levels: effort_ladders::GEMINI_LEVEL_XHIGH,
        default_effort: Some(protocol::Effort::High),
        wire_entities: None,
    },
    Model {
        id: "gemini-4-argon",
        family: "google",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        effort_levels: effort_ladders::GEMINI_LEVEL,
        default_effort: Some(protocol::Effort::High),
        wire_entities: None,
    },
    Model {
        id: "gemini-4-hydrogen",
        family: "google",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        effort_levels: effort_ladders::GEMINI_LEVEL_XHIGH,
        default_effort: Some(protocol::Effort::High),
        wire_entities: None,
    },
    Model {
        id: "gemini-4-carbon",
        family: "google",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        effort_levels: effort_ladders::GEMINI_LEVEL,
        default_effort: Some(protocol::Effort::High),
        wire_entities: None,
    },
    Model {
        id: "gemini-3.8-flash",
        family: "google",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        effort_levels: effort_ladders::GEMINI_LEVEL,
        default_effort: Some(protocol::Effort::High),
        wire_entities: None,
    },
    Model {
        id: "claude-sonnet-4-6",
        family: "claude",
        context_window: 1_000_000,
        thinking: ReasoningSupport::Adaptive,
        tool_call: true,
        vision: true,
        effort_levels: effort_ladders::CLAUDE_NO_XHIGH,
        default_effort: Some(protocol::Effort::High),
        wire_entities: None,
    },
    Model {
        id: "claude-opus-4-6",
        family: "claude",
        context_window: 1_000_000,
        thinking: ReasoningSupport::Adaptive,
        tool_call: true,
        vision: true,
        effort_levels: effort_ladders::CLAUDE_NO_XHIGH,
        default_effort: Some(protocol::Effort::High),
        wire_entities: None,
    },
    Model {
        id: "claude-opus-5-5",
        family: "claude",
        context_window: 1_000_000,
        thinking: ReasoningSupport::Adaptive,
        tool_call: true,
        vision: true,
        effort_levels: effort_ladders::CLAUDE_NO_XHIGH,
        default_effort: Some(protocol::Effort::High),
        wire_entities: None,
    },
    Model {
        id: "claude-sonnet-5-5",
        family: "claude",
        context_window: 1_000_000,
        thinking: ReasoningSupport::Adaptive,
        tool_call: true,
        vision: true,
        effort_levels: effort_ladders::CLAUDE_NO_XHIGH,
        default_effort: Some(protocol::Effort::High),
        wire_entities: None,
    },
    Model {
        id: "gpt-oss-120b-medium",
        family: "gpt",
        context_window: 128_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        effort_levels: effort_ladders::OPENAI_GPT,
        default_effort: Some(protocol::Effort::Medium),
        wire_entities: None,
    },
    Model {
        id: "gemini-3.5-flash-lite",
        family: "google",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        effort_levels: effort_ladders::GEMINI_LEVEL,
        default_effort: Some(protocol::Effort::High),
        wire_entities: None,
    },
    Model {
        id: "gemini-3.1-pro-high",
        family: "google",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        effort_levels: &[],
        default_effort: Some(protocol::Effort::High),
        wire_entities: None,
    },
    Model {
        id: "gemini-3.1-pro-low",
        family: "google",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        effort_levels: &[],
        default_effort: Some(protocol::Effort::High),
        wire_entities: None,
    },
    Model {
        id: "gemini-3-flash",
        family: "google",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        effort_levels: &[],
        default_effort: Some(protocol::Effort::High),
        wire_entities: None,
    },
];

inventory::submit!(protocol::model::BaselineModels(MODELS));

/// The shipped `agy` version this channel mirrors (binary 2026-11-19,
/// ELF 269,443,784 bytes, `agy --version` → `1.5.12`).
pub const ANTIGRAVITY_VERSION: &str = "1.5.12";
/// The `cl=` counter the shipped 1.5.12 binary stamps. Extracted from the
/// bundle's google3 build id (`file:/google_src/files/1024904911/depot/google3`,
/// 2026-11-19); the UA format string is assembled at runtime from
/// `%s/%s (%s)` + `os_type=%s; arch=%s; cl=%d; auth_method=%s`, so the
/// counter only exists as this build id in the image.
pub const ANTIGRAVITY_BUILD_NUMBER: &str = "1024904911";
pub use protocol::model_providers::ANTIGRAVITY_USER_AGENT;

/// The header `User-Agent` the `agy` **h2 telemetry lane** carries
/// (live decrypted capture of `agy` 1.5.12, 2026-11-19): Clearcut posts to
/// `play.googleapis.com /log` ride Go's default HTTP/2 transport, which
/// stamps Go's stock default UA — Go's `EncodeHeaders` writes the default
/// `user-agent` *after* the caller's own headers.
///
/// Since 1.5.12 this stock UA no longer touches the model lane: the
/// streaming call `v1internal:streamGenerateContent?alt=sse` is HTTP/1.1
/// with the brand UA, so the override that used to swap this value onto
/// streaming requests was removed. A relay-configured custom UA remains an
/// explicit user choice on every lane.
pub const ANTIGRAVITY_STREAM_USER_AGENT: &str = "Go-http-client/2.0";

/// The value the real client stamps into the **envelope body's** `userAgent`
/// field: the bare brand echo `"antigravity"`, not the header UA string
/// (live capture, `docs/reference/openapi/antigravity.yml`).
pub const ANTIGRAVITY_ENVELOPE_USER_AGENT: &str = "antigravity";

/// netune TLS/HTTP2 profile name for the `agy` **h2 telemetry lane**
/// (Clearcut `play.googleapis.com /log`, auto-updater `*.run.app`): `h2,
/// http/1.1` ALPN, no GREASE, paired with Go's HTTP/2 framing
/// (see `netune::Preset::AntigravityCliStream` / `H2Fingerprint::go`).
///
/// Live 1.5.12 capture (2026-11-19): the model lanes never negotiate h2, so
/// the provider's request paths do not select this profile — it stays
/// registered for pool isolation and for any future telemetry emulation.
pub const ANTIGRAVITY_TLS_PROFILE_STREAM: &str = "antigravity-cli-stream";

/// netune TLS/HTTP2 profile name for the `agy` **HTTP/1.1 lanes**: every
/// `/v1internal:*` Cloud Code call, unary *and* streaming
/// (`:streamGenerateContent?alt=sse`): `http/1.1` ALPN only, no GREASE, no
/// HTTP/2 (see `netune::Preset::AntigravityCliUnary`).
pub const ANTIGRAVITY_TLS_PROFILE_UNARY: &str = "antigravity-cli-unary";

/// Select the Antigravity TLS profile for a Cloud Code call.
///
/// Live 1.5.12 capture (2026-11-19, decrypted MITM): `is_stream` no longer
/// changes the wire identity. The streaming call
/// `v1internal:streamGenerateContent?alt=sse` is an HTTP/1.1 request to
/// `daily-cloudcode-pa.googleapis.com` with the brand UA and a chunked
/// upload — byte-for-byte the same transport shape as the unary calls. The
/// parameter stays in the signature because the callers already know which
/// lane they drive, and because a future lineage that moves streaming back
/// onto h2 (as 1.5.9 did) should be a deliberate change to this function,
/// not a silent default.
pub const fn antigravity_tls_profile(is_stream: bool) -> &'static str {
    let _ = is_stream;
    ANTIGRAVITY_TLS_PROFILE_UNARY
}

pub const MODEL_PROVIDER_SPEC: ModelProviderSpec = ModelProviderSpec {
    dialect: protocol::ProviderDialect::Antigravity,
    protocol_roots: std::borrow::Cow::Borrowed(&[]),
    catalog_root_url: None,
    prompt_cache: ::provider::PromptCachePolicy::Compiled(::provider::unsupported_prompt_cache),
    id: std::borrow::Cow::Borrowed(protocol::model_providers::PROVIDER_GOOGLE_ANTIGRAVITY.id),
    baselines: MODELS,
    root_url: std::borrow::Cow::Borrowed(
        protocol::model_providers::PROVIDER_GOOGLE_ANTIGRAVITY.root_url,
    ),
    user_agent: protocol::model_providers::PROVIDER_GOOGLE_ANTIGRAVITY.user_agent_cow(),
    default_headers: std::borrow::Cow::Borrowed(&[]),
    protocol: protocol::model_providers::PROVIDER_GOOGLE_ANTIGRAVITY.protocol,
    model_protocols: &[],
    catalog_source: RemoteCatalogSource::Custom(crate::CATALOG_SOURCE_ID),
    quota: Some(::provider::QuotaPort::Antigravity),
    display_policy: ::provider::DisplayPolicy::Native,
    responses_state: protocol::ResponsesState::Stateless,
    models: protocol::model_providers::PROVIDER_GOOGLE_ANTIGRAVITY.models,
};
