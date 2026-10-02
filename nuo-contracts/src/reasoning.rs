//! Model-reasoning control — the *on/off* knob for extended thinking, the
//! companion to [`crate::effort::Effort`] (which controls *depth*).
//!
//! Named *reasoning* (ADR-0191): the vendor-neutral accounting term of record
//! (`reasoning_tokens`) and the majority of the internal vocabulary
//! (`[model_reasoning]`, the stream events, the render module). "Thinking" is
//! Anthropic's display word and stays only in user-facing copy.
//!
//! This is the canonical home for [`ReasoningMode`] because whether and *how* a
//! model reasons is a **model capability**, not a transport detail — exactly
//! the same reason [`crate::effort::Effort`] lives here. The two are
//! **orthogonal**:
//!
//! | concept | meaning | wire surface (Anthropic) |
//! |---------|---------|--------------------------|
//! | `ReasoningMode` | reasoning **on/off** (the switch) | `thinking: {type:"adaptive"}` or omit |
//! | `Effort` | reasoning **depth** (the throttle) | `output_config: {effort: "high"}` |
//!
//! On the Anthropic Messages API these are genuinely independent: a request
//! may set `effort` without enabling thinking (depth has no effect when
//! thinking is off), or enable thinking at any depth. Muta must therefore
//! model and surface them as two independent knobs — coupling them (e.g.
//! "setting effort also turns thinking on") is a latent bug that prevents the
//! user from expressing legitimate intent such as "high effort, but don't
//! think".
//!
//! `Off` is the default: it omits the `thinking` field. On Opus 4.7/4.8 this
//! disables thinking; on Fable/Mythos thinking is always on and `Off` is a
//! no-op. `Adaptive` emits `thinking: {type:"adaptive"}` (the only on-mode
//! every current model that still accepts a `thinking` object honors; the
//! legacy `enabled`+`budget_tokens` form is rejected with 400 on
//! Fable/Opus-4.7+). Protocol layers (the Anthropic Messages provider) stamp
//! the chosen mode into the wire field; the choice can also ride on a channel
//! as a user override.

/// Whether extended thinking is requested, and which on-mode.
///
/// See the [module docs](self) for the orthogonality with [`crate::effort::Effort`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReasoningMode {
    /// Omit the `thinking` field. On Opus 4.7/4.8 this disables thinking; on
    /// Fable/Mythos it is a no-op (thinking is always on there).
    #[default]
    Off,
    /// `thinking: {type: "adaptive"}` — the model decides per request whether
    /// and how much to think. The recommended on-mode for every current model
    /// that still accepts a `thinking` object.
    Adaptive,
}

impl ReasoningMode {
    /// `true` when this mode requests thinking (i.e. emits a `thinking` field
    /// on the wire).
    pub const fn is_on(self) -> bool {
        matches!(self, ReasoningMode::Adaptive)
    }
}

/// What kind of extended thinking a model supports, and **how it is encoded on
/// the wire**. [`ReasoningMode`] is the user's on/off *intent*; `ReasoningSupport`
/// is the model's *capability*, and the two combine at request-build time to
/// decide which (if any) `thinking` object the Anthropic transport emits.
///
/// This is the single source of truth for thinking capability — the boolean
/// "does it reason" used for display derives from it via [`Self::reasons`].
/// Carried per-model in the registry exactly like [`crate::effort::Effort`]
/// levels, so the transport never guesses the wire form from a model id.
///
/// The Anthropic Messages API exposes **two mutually exclusive** thinking
/// mechanisms, which is why a single bool is insufficient:
/// - **Adaptive** (`thinking: {type:"adaptive"}` + `output_config.effort`):
///   the model decides depth per request. Opus 4.7+ accept *only* this; the
///   legacy manual form is rejected with 400.
/// - **Manual** (`thinking: {type:"enabled", budget_tokens: N}`): a fixed token
///   budget, no `effort`. The only form Haiku 4.5 / Opus·Sonnet 4.5 / Claude 4
///   accept; needs the `interleaved-thinking` beta header alongside tools.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningSupport {
    /// The model cannot think. (GPT-4o, Gemini, the conservative fallback.)
    #[default]
    None,
    /// The model reasons, but its reasoning is surfaced via the OpenAI-compatible
    /// `reasoning_content` stream, not an Anthropic `thinking` object. (GLM,
    /// Kimi, DeepSeek, MiMo, MiniMax, Qwen — including third-party models served
    /// over an Anthropic-compatible relay, where the transport falls back to the
    /// conservative `adaptive` form if the user turns thinking on.)
    ReasoningContent,
    /// The model reasons, but **deliberately hides its full reasoning chain**:
    /// only a summary of its reasoning is surfaced (via the OpenAI Responses
    /// `reasoning_summary_text` stream), never the raw chain. (GPT-5.x family.)
    /// Unlike [`Self::ReasoningContent`], these models cannot disclose their
    /// complete thought process, so the TUI gates thinking disclosure at message
    /// creation: a `ReasoningSummary` model never produces a `MessageKind::Thinking`
    /// message, avoiding phantom layout entries.
    ReasoningSummary,
    /// Anthropic **adaptive** thinking: emit `thinking: {type:"adaptive"}` and
    /// drive depth via `output_config.effort`. Opt-in (off unless the user turns
    /// it on); manual `type:"enabled"` is rejected with 400. (Opus 4.7/4.8;
    /// Opus·Sonnet 4.6 also accept the deprecated manual form, but adaptive is
    /// canonical.)
    AnthropicAdaptive,
    /// Anthropic adaptive thinking that is **always on** and cannot be disabled;
    /// the `thinking` object is emitted regardless of the user's on/off choice.
    /// (Claude Fable 5 / Mythos 5.)
    AnthropicAdaptiveAlwaysOn,
    /// Anthropic adaptive thinking that is **on by default when the `thinking`
    /// field is omitted**, but CAN be disabled by sending `{type:"disabled"}`.
    /// This is the distinguishing trait: unlike [`Self::AnthropicAdaptive`]
    /// (omitting disables thinking) and [`Self::AnthropicAdaptiveAlwaysOn`]
    /// (it can never be disabled), a request-build layer for this variant MUST
    /// emit an explicit `thinking:{type:"disabled"}` when the user opts OUT —
    /// otherwise the model silently reasons and burns tokens. When opted in,
    /// emit `{type:"adaptive"}`. (Claude Sonnet 5.)
    AnthropicAdaptiveOnByDefault,
    /// Anthropic **manual** extended thinking: emit
    /// `thinking: {type:"enabled", budget_tokens: N}` (no `effort`). Opt-in.
    /// (Claude Haiku 4.5; Opus·Sonnet 4.5; Claude 4 / 4.1.)
    AnthropicManual,
}

impl ReasoningSupport {
    /// `true` when the model reasons at all — the coarse capability used for
    /// display. Everything except [`Self::None`] reasons.
    pub const fn reasons(self) -> bool {
        !matches!(self, ReasoningSupport::None)
    }

    /// `true` when the model fully discloses its reasoning chain — i.e. its
    /// complete thought process can be surfaced to the user. `None` does not
    /// reason at all (nothing to disclose), and [`Self::ReasoningSummary`]
    /// intentionally hides the chain, returning only a summary. All other
    /// variants disclose. The TUI uses this to gate thinking-message creation:
    /// a model whose chain is not disclosed never produces a
    /// `MessageKind::Thinking` message, so no phantom entries leak into layout
    /// counts, selection math, or scroll state.
    pub const fn chain_disclosed(self) -> bool {
        !matches!(
            self,
            ReasoningSupport::None | ReasoningSupport::ReasoningSummary
        )
    }
}
