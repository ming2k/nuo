//! Model-reasoning control — the on/off knob for extended thinking.

/// Whether extended thinking is requested, and which on-mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReasoningMode {
    /// Omit the `thinking` field. On a route whose thinking is opt-in this
    /// disables it; on a route whose thinking is always on it is a no-op.
    #[default]
    Off,
    /// Ask the model to decide per request whether and how much to think
    /// (the `adaptive` form where the surface defines one).
    Adaptive,
}

impl ReasoningMode {
    /// `true` when this mode requests thinking (i.e. emits a `thinking` field
    /// on the wire).
    pub const fn is_on(self) -> bool {
        matches!(self, ReasoningMode::Adaptive)
    }
}

/// **How** a model's reasoning is surfaced and switched on the wire.
///
/// Variants are named for the **encoding**, never for a vendor. A vendor name in
/// this enum would make the framework core carry one surface's dialect — exactly
/// the defect ADR-0126 removes — and it would read as a lie the moment a second
/// provider adopted the same encoding. The mapping from a variant onto an actual
/// dialect is the owning provider's business.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningSupport {
    /// The model cannot think.
    #[default]
    None,
    /// The model reasons, and its reasoning is surfaced as **raw reasoning
    /// content** interleaved in the stream (`reasoning_content`-style), not as a
    /// structured thinking object.
    ReasoningContent,
    /// The model reasons but deliberately hides its chain: only a summary is
    /// surfaced (`reasoning_summary_text`-style).
    ReasoningSummary,
    /// **Adaptive** thinking, opt-in: the request opts in, and depth is driven
    /// by a separate effort field.
    Adaptive,
    /// **Adaptive** thinking that is always on and cannot be disabled; an
    /// opt-out request is a no-op.
    AdaptiveAlwaysOn,
    /// **Adaptive** thinking that runs when the field is omitted, and can be
    /// turned **off** by an explicit disabled form.
    AdaptiveOnByDefault,
    /// **Manual** extended thinking: the request carries an explicit token
    /// budget instead of an adaptive mode.
    Manual,
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

    /// Whether this encoding has a separate **on/off switch** at all, as opposed
    /// to being driven purely by depth.
    ///
    /// The adaptive and manual encodings all take a field that can be omitted or
    /// explicitly disabled; the content/summary encodings are always-on streams
    /// with nothing to switch. This is the **only** signal a surface may use to
    /// decide whether to render a thinking toggle — it replaces the old
    /// `protocol == "anthropic"` sniff ADR-0126 removed.
    pub const fn has_thinking_switch(self) -> bool {
        matches!(
            self,
            ReasoningSupport::Adaptive
                | ReasoningSupport::AdaptiveAlwaysOn
                | ReasoningSupport::AdaptiveOnByDefault
                | ReasoningSupport::Manual
        )
    }
}
