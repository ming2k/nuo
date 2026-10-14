//! Baseline reasoning-effort ladders for known model families (ADR-0270).
//!
//! Reasoning depth rungs are an intrinsic capability of a model architecture
//! and family. Because upstream `/models` endpoints rarely advertise capability
//! fields (with exceptions like Moonshot and Copilot), providers require
//! compiled baseline seeds for initial capability resolution and static registration.
//!
//! These constants are provider/model capability metadata and belong here in
//! the provider registry rather than in the core `nuo-wire` domain vocabulary.

use nuo_model_codec::effort::Effort;

/// Universal conservative subset: `low`/`medium`/`high`. Safe fallback for
/// models whose full depth capability is unknown.
pub const COMMON: &[Effort] = &[Effort::Low, Effort::Medium, Effort::High];

/// Claude full ladder (`low`..=`max` including `xhigh`).
/// Honored by Claude Opus 4.8 / 4.7 and Fable 5 / Mythos 5.
pub const CLAUDE_FULL: &[Effort] = &[
    Effort::Low,
    Effort::Medium,
    Effort::High,
    Effort::Xhigh,
    Effort::Max,
];

/// Claude ladder without `xhigh` (`low`/`medium`/`high`/`max`).
/// Claude Sonnet 4.6 and Opus 4.6, which honor `max` but reject `xhigh`.
pub const CLAUDE_NO_XHIGH: &[Effort] = &[
    Effort::Low,
    Effort::Medium,
    Effort::High,
    Effort::Max,
];

/// OpenAI GPT <= 5.5: `none`/`minimal`/`low`/`medium`/`high`/`xhigh`.
pub const OPENAI_GPT: &[Effort] = &[
    Effort::None,
    Effort::Minimal,
    Effort::Low,
    Effort::Medium,
    Effort::High,
    Effort::Xhigh,
];

/// OpenAI GPT-5.6 (Sol/Terra/Luna): adds `max`.
pub const OPENAI_GPT_5_6: &[Effort] = &[
    Effort::None,
    Effort::Minimal,
    Effort::Low,
    Effort::Medium,
    Effort::High,
    Effort::Xhigh,
    Effort::Max,
];

/// OpenAI GPT-6 (Astra): `low`/`medium`/`high`/`xhigh`/`max`/`ultra`.
pub const OPENAI_GPT_6: &[Effort] = &[
    Effort::Low,
    Effort::Medium,
    Effort::High,
    Effort::Xhigh,
    Effort::Max,
    Effort::Ultra,
];

/// xAI Grok 4.x: `none`/`low`/`medium`/`high`.
pub const XAI_GROK: &[Effort] = &[
    Effort::None,
    Effort::Low,
    Effort::Medium,
    Effort::High,
];

/// `low`/`high`/`max`: shared capability set for DeepSeek and Moonshot Kimi K3.
///
/// This is the DeepSeek **V4 / V4.1** family ladder, live-verified on the
/// DeepSeek platform and its OpenAI-compatible relays as the accepted
/// `low`/`high`/`max` presets (integer efforts are rejected upstream with HTTP
/// 400). It is the ladder every DeepSeek id on a relay surface carries —
/// including the CommandCode `provider/v1` DeepSeek lanes, whose declared
/// `deepseek/deepseek-v4-flash` the server rewrites to a `deepseek-v4.1-flash`
/// upstream (ADR-0014), so the V4.1 rungs are the ones that actually land.
/// Omitting a supported rung is the failure mode to avoid here; see
/// `docs/reference/commandcode-api.yml` `x-reasoning-effort-ladder` for the
/// full per-family reference table.
pub const LOW_HIGH_MAX: &[Effort] = &[
    Effort::Low,
    Effort::High,
    Effort::Max,
];

/// Z.AI GLM-5.2 / GLM-5.3: `low`/`high`/`xhigh`/`max`.
pub const GLM_5: &[Effort] = &[
    Effort::Low,
    Effort::High,
    Effort::Xhigh,
    Effort::Max,
];

/// Google Gemini 3.x thinkingLevel ladder: `minimal`/`low`/`medium`/`high`.
pub const GEMINI_LEVEL: &[Effort] = &[
    Effort::Minimal,
    Effort::Low,
    Effort::Medium,
    Effort::High,
];

/// Google Gemini 2.5 thinkingBudget ladder: `minimal`/`low`/`medium`/`high`/`max`.
pub const GEMINI_BUDGET: &[Effort] = &[
    Effort::Minimal,
    Effort::Low,
    Effort::Medium,
    Effort::High,
    Effort::Max,
];

/// QianwenAI Token Plan hybrid models (Qwen3.x, GLM-5.2, DeepSeek V4 Flash,
/// live-verified 2026-10): `none`/`low`/`medium`/`high`/`xhigh`/`max`. `none`
/// is a first-class rung on this surface — the endpoint answers with zero
/// reasoning content, making it the thinking off switch.
pub const QWEN_MIXED: &[Effort] = &[
    Effort::None,
    Effort::Low,
    Effort::Medium,
    Effort::High,
    Effort::Xhigh,
    Effort::Max,
];

/// QianwenAI Token Plan DeepSeek V4 Pro snapshots (live-verified 2026-10):
/// `low`/`medium`/`high`/`xhigh`/`max` — the endpoint rejects `none` and
/// `minimal` outright, so "reasoning off" is inexpressible on these models.
pub const DEEPSEEK_V4_PLAN: &[Effort] = &[
    Effort::Low,
    Effort::Medium,
    Effort::High,
    Effort::Xhigh,
    Effort::Max,
];
