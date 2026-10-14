//! Model-reasoning control — the on/off knob for extended thinking.

/// Whether extended thinking is requested, and which on-mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReasoningMode {
    /// Omit the `thinking` field. On Opus 4.7/4.8 this disables thinking; on
    /// Fable/Mythos it is a no-op (thinking is always on there).
    #[default]
    Off,
    /// `thinking: {type: "adaptive"}` — the model decides per request whether
    /// and how much to think.
    Adaptive,
}

impl ReasoningMode {
    /// `true` when this mode requests thinking (i.e. emits a `thinking` field
    /// on the wire).
    pub const fn is_on(self) -> bool {
        matches!(self, ReasoningMode::Adaptive)
    }
}

/// What kind of extended thinking a model supports, and how it is encoded on the wire.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningSupport {
    /// The model cannot think.
    #[default]
    None,
    /// The model reasons, but its reasoning is surfaced via the OpenAI-compatible
    /// `reasoning_content` stream, not an Anthropic `thinking` object.
    ReasoningContent,
    /// The model reasons, but deliberately hides its full reasoning chain:
    /// only a summary of its reasoning is surfaced via `reasoning_summary_text`.
    ReasoningSummary,
    /// Anthropic adaptive thinking: emit `thinking: {type:"adaptive"}` and
    /// drive depth via `output_config.effort`. Opt-in.
    AnthropicAdaptive,
    /// Anthropic adaptive thinking that is always on and cannot be disabled.
    AnthropicAdaptiveAlwaysOn,
    /// Anthropic adaptive thinking that is on by default when the `thinking`
    /// field is omitted, but can be disabled by sending `{type:"disabled"}`.
    AnthropicAdaptiveOnByDefault,
    /// Anthropic manual extended thinking: emit `thinking: {type:"enabled", budget_tokens: N}`.
    AnthropicManual,
}

impl ReasoningSupport {
    /// `true` when the model reasons at all.
    pub const fn reasons(self) -> bool {
        !matches!(self, ReasoningSupport::None)
    }

    /// `true` when the model fully discloses its reasoning chain.
    pub const fn chain_disclosed(self) -> bool {
        !matches!(
            self,
            ReasoningSupport::None | ReasoningSupport::ReasoningSummary
        )
    }
}
