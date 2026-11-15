//! One versioned runtime context policy.
//!
//! The policy is configuration, not a second context model. It carries the
//! watermarks, the effective input ceiling, and the two tool-output retention
//! bands the request pipeline actually reads. The Phase-A fact/view/checkpoint
//! substrate that once lived beside it was deleted with SessionIR
//! ([ADR-0182](../../docs/adr/0182-context-policy-is-configuration-and-phase-a-substrate-is-deleted.md)).
//!
//! All values are integer-deterministic. Every watermark is a fraction of the
//! effective input ceiling `I = min(declared_window, max_input_tokens)`, never
//! of the model's advertised window.

use crate::ContextBudget;

/// Schema version stamped onto every persisted `context` policy.
pub const CONTEXT_POLICY_SCHEMA_VERSION: u32 = 1;

/// Default effective input ceiling, in tokens.
///
/// A model's advertised context window is a hard ceiling, not a performance
/// target: prefill and attention cost scale with the tokens actually sent, so a
/// 1M-token route that never compacts turns every ReAct turn into a multi-second
/// wait. The policy derives every watermark from the capped window.
pub const DEFAULT_MAX_INPUT_TOKENS: u64 = 128_000;

/// Default horizon, in tokens, for tool-result retention.
pub const DEFAULT_TOOL_OUTPUT_RETENTION_TOKENS: u64 = 24_000;

/// Default verbatim window for the newest tool output, in tokens.
pub const DEFAULT_RECENT_OBSERVATION_PROTECT_TOKENS: u64 = 6_000;

/// Soft/hard/target watermarks as basis points of the effective input ceiling.
///
/// Hysteresis: below `soft` no reclamation runs; between `soft` and `hard`
/// lightweight degradation runs; at or above `hard` strong reclamation runs.
/// `target` is where a rewritten view aims.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WatermarkPolicy {
    /// Soft watermark in basis points of `I` (default 6500).
    pub soft_bp: u32,
    /// Hard watermark in basis points of `I` (default 8500).
    pub hard_bp: u32,
    /// Target watermark in basis points of `I` (default 5000).
    pub target_bp: u32,
}

impl WatermarkPolicy {
    /// The initial default: 0.65 / 0.85 / 0.50.
    pub const DEFAULT: Self = Self {
        soft_bp: 6500,
        hard_bp: 8500,
        target_bp: 5000,
    };

    /// Validate that `target <= soft <= hard <= 10000`.
    pub const fn validate(&self) -> Result<(), WatermarkError> {
        let ordered = self.target_bp <= self.soft_bp
            && self.soft_bp <= self.hard_bp
            && self.hard_bp <= 10_000;
        if ordered {
            Ok(())
        } else {
            Err(WatermarkError::InvalidWatermarks {
                soft_bp: self.soft_bp,
                hard_bp: self.hard_bp,
                target_bp: self.target_bp,
            })
        }
    }

    /// Token count for a basis-point ratio of `I` (integer, floor).
    pub const fn tokens_at(&self, input_ceiling: u64, bp: u32) -> u64 {
        ((input_ceiling as u128 * bp as u128) / 10_000) as u64
    }

    /// The three thresholds in tokens.
    pub const fn thresholds(&self, input_ceiling: u64) -> (u64, u64, u64) {
        (
            self.tokens_at(input_ceiling, self.soft_bp),
            self.tokens_at(input_ceiling, self.hard_bp),
            self.tokens_at(input_ceiling, self.target_bp),
        )
    }
}

/// A watermark ordering rule failed.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WatermarkError {
    /// Watermark ratios are out of order or out of range.
    InvalidWatermarks {
        /// Soft watermark basis points.
        soft_bp: u32,
        /// Hard watermark basis points.
        hard_bp: u32,
        /// Target watermark basis points.
        target_bp: u32,
    },
}

impl std::fmt::Display for WatermarkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidWatermarks {
                soft_bp,
                hard_bp,
                target_bp,
            } => write!(
                f,
                "invalid watermarks: soft {soft_bp} / hard {hard_bp} / target {target_bp} \
                 (required target <= soft <= hard <= 10000)"
            ),
        }
    }
}

impl std::error::Error for WatermarkError {}

/// The single versioned context policy.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextPolicy {
    /// Schema version of this policy record.
    pub schema_version: u32,
    /// Soft/hard/target watermarks as fractions of the effective input ceiling.
    pub watermarks: WatermarkPolicy,
    /// Rounds the planner prefers to keep; never a hard protection quantity.
    pub preferred_recent_rounds: u32,
    /// Whether strong reclamation (checkpointing) is enabled.
    pub checkpoint_enabled: bool,
    /// Whether lightweight degradation may relieve pressure between rounds.
    pub lightweight_degradation_enabled: bool,
    /// Window used when the model declares no context window.
    pub fallback_window_tokens: u64,
    /// Hard cap on the effective input window used for budget derivation.
    #[serde(default = "default_max_input_tokens")]
    pub max_input_tokens: u64,
    /// Verbatim window for the newest tool output, in tokens.
    pub recent_observation_protect_tokens: u64,
    /// Absolute token horizon for tool-result retention, measured from the tail.
    #[serde(default = "default_tool_output_retention_tokens")]
    pub tool_output_retention_tokens: u64,
}

impl Default for ContextPolicy {
    fn default() -> Self {
        Self {
            schema_version: CONTEXT_POLICY_SCHEMA_VERSION,
            watermarks: WatermarkPolicy::DEFAULT,
            preferred_recent_rounds: 6,
            checkpoint_enabled: true,
            lightweight_degradation_enabled: true,
            fallback_window_tokens: 32_000,
            max_input_tokens: DEFAULT_MAX_INPUT_TOKENS,
            recent_observation_protect_tokens: DEFAULT_RECENT_OBSERVATION_PROTECT_TOKENS,
            tool_output_retention_tokens: DEFAULT_TOOL_OUTPUT_RETENTION_TOKENS,
        }
    }
}

/// A policy invariant was violated by an operator-supplied value.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyError {
    /// A finite resource limit is zero.
    InvalidLimit { name: String },
    /// A watermark ordering rule failed.
    Watermarks(WatermarkError),
    /// The tool-output retention horizon is shorter than the verbatim window.
    RetentionBelowProtect { retention: u64, protect: u64 },
    /// The stamped schema version is not the one this runtime knows.
    UnknownSchemaVersion { found: u32, expected: u32 },
}

impl std::fmt::Display for PolicyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidLimit { name } => write!(f, "invalid finite context limit: {name}"),
            Self::Watermarks(error) => write!(f, "watermark policy invalid: {error}"),
            Self::RetentionBelowProtect { retention, protect } => write!(
                f,
                "tool-output retention horizon {retention} is below the verbatim window {protect}"
            ),
            Self::UnknownSchemaVersion { found, expected } => write!(
                f,
                "unknown context policy schema version {found} (expected {expected})"
            ),
        }
    }
}

impl std::error::Error for PolicyError {}

impl ContextPolicy {
    /// Validate every live size setting and its relations.
    pub fn validate(&self) -> Result<(), PolicyError> {
        if self.schema_version != CONTEXT_POLICY_SCHEMA_VERSION {
            return Err(PolicyError::UnknownSchemaVersion {
                found: self.schema_version,
                expected: CONTEXT_POLICY_SCHEMA_VERSION,
            });
        }
        for (name, value) in [
            ("fallback_window_tokens", self.fallback_window_tokens),
            ("max_input_tokens", self.max_input_tokens),
            (
                "recent_observation_protect_tokens",
                self.recent_observation_protect_tokens,
            ),
            (
                "tool_output_retention_tokens",
                self.tool_output_retention_tokens,
            ),
        ] {
            if value == 0 {
                return Err(PolicyError::InvalidLimit { name: name.into() });
            }
        }
        if self.tool_output_retention_tokens < self.recent_observation_protect_tokens {
            return Err(PolicyError::RetentionBelowProtect {
                retention: self.tool_output_retention_tokens,
                protect: self.recent_observation_protect_tokens,
            });
        }
        self.watermarks.validate().map_err(PolicyError::Watermarks)
    }

    /// Resolve a model window into the runtime context budget thresholds.
    ///
    /// The declared window is capped by [`Self::max_input_tokens`] before any
    /// watermark is derived, so a 1M-token route never advertises a compaction
    /// threshold it can never usefully reach.
    pub fn resolve_budget(&self, window_tokens: usize) -> ContextBudget {
        let declared = if window_tokens == 0 {
            self.fallback_window_tokens
        } else {
            u64::try_from(window_tokens).unwrap_or(u64::MAX)
        };
        let window = declared.min(self.max_input_tokens).max(1);
        let threshold = |fraction: f64| (window as f64 * fraction) as usize;
        let quantum_floor = 512.max(threshold(0.05));
        let soft_frac = self.watermarks.soft_bp as f64 / 10_000.0;
        let hard_frac = self.watermarks.hard_bp as f64 / 10_000.0;
        let target_frac = self.watermarks.target_bp as f64 / 10_000.0;
        ContextBudget {
            window_tokens: window as usize,
            prune_threshold_tokens: threshold(soft_frac),
            compaction_threshold_tokens: threshold(hard_frac),
            target_tokens: threshold(target_frac),
            quantum_floor_tokens: quantum_floor,
            cruise_low_tokens: threshold(soft_frac * 0.8),
        }
    }
}

fn default_max_input_tokens() -> u64 {
    DEFAULT_MAX_INPUT_TOKENS
}

fn default_tool_output_retention_tokens() -> u64 {
    DEFAULT_TOOL_OUTPUT_RETENTION_TOKENS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_policy_is_valid() {
        ContextPolicy::default().validate().unwrap();
    }

    #[test]
    fn relations_are_enforced() {
        let p = ContextPolicy {
            schema_version: 999,
            ..ContextPolicy::default()
        };
        assert!(matches!(
            p.validate().unwrap_err(),
            PolicyError::UnknownSchemaVersion { .. }
        ));

        let p = ContextPolicy {
            recent_observation_protect_tokens: 8_000,
            tool_output_retention_tokens: 6_000,
            ..ContextPolicy::default()
        };
        assert!(matches!(
            p.validate().unwrap_err(),
            PolicyError::RetentionBelowProtect { .. }
        ));

        let p = ContextPolicy {
            watermarks: WatermarkPolicy {
                soft_bp: 5_000,
                hard_bp: 9_000,
                target_bp: 7_000,
            },
            ..ContextPolicy::default()
        };
        assert!(matches!(
            p.validate().unwrap_err(),
            PolicyError::Watermarks(_)
        ));
    }

    #[test]
    fn declared_window_is_capped_by_the_effective_input_ceiling() {
        let policy = ContextPolicy::default();
        let budget = policy.resolve_budget(1_000_000);
        assert_eq!(budget.window_tokens, 128_000);
        assert_eq!(budget.prune_threshold_tokens, 83_200);
        assert_eq!(budget.compaction_threshold_tokens, 108_800);
        assert_eq!(budget.target_tokens, 64_000);

        let narrow = policy.resolve_budget(64_000);
        assert_eq!(narrow.window_tokens, 64_000);
        assert_eq!(narrow.prune_threshold_tokens, 41_600);

        let unknown = policy.resolve_budget(0);
        assert_eq!(unknown.window_tokens, 32_000);
    }

    #[test]
    fn new_limits_default_when_absent_from_an_existing_policy_document() {
        let source = r#"
            schema_version = 1
            preferred_recent_rounds = 6
            checkpoint_enabled = true
            lightweight_degradation_enabled = true
            fallback_window_tokens = 32000
            recent_observation_protect_tokens = 6000

            [watermarks]
            soft_bp = 6500
            hard_bp = 8500
            target_bp = 5000
        "#;
        let policy: ContextPolicy = toml::from_str(source).expect("policy parses");
        assert_eq!(policy.max_input_tokens, DEFAULT_MAX_INPUT_TOKENS);
        assert_eq!(
            policy.tool_output_retention_tokens,
            DEFAULT_TOOL_OUTPUT_RETENTION_TOKENS
        );
    }
}
