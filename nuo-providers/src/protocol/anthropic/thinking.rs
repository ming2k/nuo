//! Anthropic Messages — resolved thinking/effort configuration.
//!
//! [`ThinkingConfig`] carries the two orthogonal reasoning knobs —
//! [`ReasoningMode`] (on/off) and [`Effort`] (depth) — that the request layer
//! stamps onto every `/messages` body. This is an Anthropic-transport concern
//! (it configures *how* the request encodes reasoning), so it lives here
//! rather than in `nuo-wire` (which holds only the model *capabilities*:
//! `ReasoningMode`, `Effort`, `ReasoningSupport`).
//!
//! **Reasoning is opt-in.** The default for every model is thinking **off**
//! with no explicit effort — extended thinking is a per-model decision the user
//! makes in the model editor, not something a model enables on its own
//! (ADR-0046). A request only carries a `thinking` object when the user has
//! turned it on for that model.

use nuo_model_codec::{Effort, reasoning::ReasoningSupport};

/// Re-export the on/off enum so callers reaching it through this module keep a
/// stable path.
pub use nuo_model_codec::ReasoningMode;

/// Resolved thinking/effort configuration for an Anthropic Messages provider.
///
/// The two knobs are **orthogonal** ([`ReasoningMode`] = on/off switch,
/// [`Effort`] = depth throttle) and are surfaced as such — never coupled.
///
/// `effort` is `Option<Effort>`:
/// - `None` — no explicit choice: use the model default (`high`) and **omit**
///   `output_config` from the wire.
/// - `Some(e)` — an explicit user override: always emit
///   `output_config: {effort: e}`, **even when `e == High`**. An explicit
///   choice is honored verbatim — "what you set is what you send".
///
/// The chosen effort is clamped to the model's `effort_levels` at request-build
/// time (see `Self::resolve_for`).
#[derive(Debug, Clone, Copy)]
pub struct ThinkingConfig {
    pub mode: ReasoningMode,
    pub effort: Option<Effort>,
}

impl ThinkingConfig {
    /// The default configuration for a model: **thinking off, no explicit
    /// effort**. Extended thinking is opt-in (ADR-0046).
    pub fn for_model(_model: &nuo_model_codec::Model) -> Self {
        Self::default()
    }

    /// Set the thinking mode. Returns `self` for chaining.
    pub fn with_mode(mut self, mode: ReasoningMode) -> Self {
        self.mode = mode;
        self
    }

    /// Set an explicit effort override. The value is **not** clamped here;
    /// clamping against the model's supported levels happens at request-build
    /// time. Once set (even to [`Effort::High`]) the request will emit
    /// `output_config`. Returns `self` for chaining.
    pub fn with_effort(mut self, effort: Effort) -> Self {
        self.effort = Some(effort);
        self
    }

    /// Resolve this config against a concrete model's `effort_levels` (the
    /// open [`nuo_model_codec::EffortLevel`] ladder, which may carry provider-advertised tiers
    /// the vocabulary does not name), returning a new config whose explicit
    /// effort (if any) is clamped to the model's supported levels. The mode and
    /// the effort's explicit/implicit distinction are honored unchanged. An
    /// empty `effort_levels` disables clamping. This is what `request::stamp_thinking`
    /// calls.
    ///
    /// The request effort is always a *known* [`Effort`] (it comes from a
    /// channel/config override), so it clamps against the **known** rungs of
    /// the open ladder; provider-advertised `Other` tiers are not eligible
    /// ranking targets for a known request (their depth is unknowable), though
    /// an exact-name passthrough is honored via [`Effort::clamp_to_levels`].
    pub(super) fn resolve_for(self, effort_levels: &[nuo_model_codec::EffortLevel]) -> Self {
        if effort_levels.is_empty() {
            return self;
        }
        // Clamp a known request against the known rungs of the open ladder.
        let known: Vec<Effort> = effort_levels
            .iter()
            .filter_map(nuo_model_codec::EffortLevel::as_known)
            .collect();
        Self {
            mode: self.mode,
            effort: self.effort.map(|e| e.clamp_to(&known)),
        }
    }

    /// Whether the resolved model requires the manual-thinking beta header.
    /// Used by `request::beta_header`.
    pub(super) fn needs_manual_beta(self, support: ReasoningSupport) -> bool {
        matches!(support, ReasoningSupport::AnthropicManual) && self.mode.is_on()
    }
}

impl Default for ThinkingConfig {
    fn default() -> Self {
        Self {
            mode: ReasoningMode::Off,
            effort: None,
        }
    }
}
