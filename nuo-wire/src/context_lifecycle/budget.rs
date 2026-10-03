//! Budget arithmetic for request admission (ADR-0277 §1–§3, `INV-ADMIT-02/04`).
//!
//! Shared windows reserve total output once; independently declared input
//! limits do not subtract output. Model contracts carry accounting provenance. Every watermark
//! is a fraction of `I`, not of `W`, so a rename cannot silently change
//! behavior (ADR-0280 §3).
//!
//! All arithmetic is integer and deterministic (`INV-ADMIT-03`): no floating
//! point, so the same inputs always produce the same thresholds.

use std::fmt;

/// The model's total context window `W`, in tokens.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(transparent)]
pub struct ModelWindow {
    /// Total window in tokens.
    pub tokens: u64,
}

impl ModelWindow {
    /// Wrap a token count.
    pub const fn tokens(tokens: u64) -> Self {
        Self { tokens }
    }
}

/// Output accounting supplied by the selected model, never inferred from an
/// effort label. Inclusive caps already contain reasoning tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OutputReserve {
    /// A generation cap including both visible and reasoning output.
    Inclusive { total_tokens: u64 },
    /// Independently bounded components which consume a shared window.
    Additive {
        visible_tokens: u64,
        reasoning_tokens: u64,
    },
}

impl OutputReserve {
    /// Resolve once, refusing arithmetic overflow instead of hiding it.
    pub fn total(self) -> Result<u64, AdmissionError> {
        match self {
            Self::Inclusive { total_tokens } => Ok(total_tokens),
            Self::Additive {
                visible_tokens,
                reasoning_tokens,
            } => visible_tokens
                .checked_add(reasoning_tokens)
                .ok_or(AdmissionError::ArithmeticOverflow),
        }
    }
}

/// Provenance for an admitted accounting contract.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BudgetProvenance {
    Declared {
        source: String,
        revision: String,
    },
    Configured {
        policy_revision: u32,
    },
    Estimated {
        source: String,
        calibration_revision: u64,
    },
}

/// Versioned accounting for a model/route. Absence of a shared window never
/// turns an independent input limit into a shared one.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ModelBudgetContract {
    pub shared_window: Option<u64>,
    pub input_limit: Option<u64>,
    pub output: OutputReserve,
    pub output_limit: Option<u64>,
    pub shared_margin: u64,
    pub input_margin: u64,
    pub provenance: BudgetProvenance,
}

impl ModelBudgetContract {
    pub fn resolve(&self) -> Result<Budget, AdmissionError> {
        let output = self.output.total()?;
        if self.output_limit.is_some_and(|limit| output > limit) {
            return Err(AdmissionError::OutputLimitExceeded);
        }
        let mut ceilings = Vec::with_capacity(2);
        if let Some(window) = self.shared_window {
            let ceiling = window
                .checked_sub(output)
                .and_then(|v| v.checked_sub(self.shared_margin))
                .filter(|v| *v > 0)
                .ok_or(AdmissionError::ZeroInputCeiling {
                    window,
                    output,
                    framing: self.shared_margin,
                })?;
            ceilings.push(ceiling);
        }
        if let Some(window) = self.input_limit {
            ceilings.push(
                window
                    .checked_sub(self.input_margin)
                    .filter(|v| *v > 0)
                    .ok_or(AdmissionError::ZeroInputCeiling {
                        window,
                        output: 0,
                        framing: self.input_margin,
                    })?,
            );
        }
        let input_ceiling = ceilings
            .into_iter()
            .min()
            .ok_or(AdmissionError::BudgetContractUnavailable)?;
        Ok(Budget {
            window: self.shared_window.or(self.input_limit).unwrap_or(0),
            output,
            framing: if self.shared_window.is_some() {
                self.shared_margin
            } else {
                self.input_margin
            },
            input_ceiling,
        })
    }
}

/// The framing/media/safety reserve `F` (ADR-0277 §1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct FramingReserve {
    /// Reserved tokens, including the estimation margin.
    pub tokens: u64,
}

impl FramingReserve {
    /// `F = max(1024, ceil(0.03 * W))`.
    pub const fn for_window(window: ModelWindow) -> Self {
        let three_percent = ((3u128 * window.tokens as u128).div_ceil(100)) as u64;
        Self {
            tokens: if three_percent < 1024 {
                1024
            } else {
                three_percent
            },
        }
    }

    /// An explicit reserve.
    pub const fn tokens(tokens: u64) -> Self {
        Self { tokens }
    }
}

/// A single metered input block, for the per-component report on refusal.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BudgetComponent {
    /// Stable component name (`"system"`, `"tools"`, `"tail"`, …).
    pub name: String,
    /// Measured or estimated tokens for this component.
    pub tokens: u64,
}

/// Why admission failed (ADR-0277 §2, `INV-ADMIT-04`).
///
/// The planner never resolves these by silently deleting mandatory blocks; the
/// caller chooses a larger window or narrows the input.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdmissionError {
    /// No usable input accounting was supplied.
    BudgetContractUnavailable,
    /// A token sum cannot be represented.
    ArithmeticOverflow,
    /// Output controls exceed the declared limit.
    OutputLimitExceeded,
    /// Mandatory blocks alone exceed `I`; the per-component report says which.
    ContextAdmissionExceeded {
        /// The computed input ceiling `I`.
        input_ceiling: u64,
        /// Tokens the mandatory blocks actually require.
        mandatory_tokens: u64,
        /// Per-component metering, ordered by the planner's protection order.
        components: Vec<BudgetComponent>,
    },
    /// `W <= O + F`, so no input can fit.
    ZeroInputCeiling {
        /// Total window `W`.
        window: u64,
        /// Output reserve `O`.
        output: u64,
        /// Framing reserve `F`.
        framing: u64,
    },
    /// Watermark ratios are out of order or out of range.
    InvalidWatermarks {
        /// The offending soft/hard/target basis points.
        soft_bp: u32,
        /// Hard watermark basis points.
        hard_bp: u32,
        /// Target watermark basis points.
        target_bp: u32,
    },
}

impl fmt::Display for AdmissionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BudgetContractUnavailable => write!(f, "model budget contract unavailable"),
            Self::ArithmeticOverflow => write!(f, "token accounting overflow"),
            Self::OutputLimitExceeded => write!(f, "output reserve exceeds declared output limit"),
            AdmissionError::ContextAdmissionExceeded {
                input_ceiling,
                mandatory_tokens,
                ..
            } => write!(
                f,
                "context admission exceeded: mandatory {mandatory_tokens} > input ceiling {input_ceiling}"
            ),
            AdmissionError::ZeroInputCeiling {
                window,
                output,
                framing,
            } => write!(
                f,
                "input ceiling is non-positive: window {window} <= output {output} + framing {framing}"
            ),
            AdmissionError::InvalidWatermarks {
                soft_bp,
                hard_bp,
                target_bp,
            } => write!(
                f,
                "invalid watermarks: soft {soft_bp} / hard {hard_bp} / target {target_bp} (required target <= soft <= hard <= 10000)"
            ),
        }
    }
}

impl std::error::Error for AdmissionError {}

/// The four budget terms of one planning pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    /// Total window `W`.
    pub window: u64,
    /// Output reserve `O` (includes reasoning).
    pub output: u64,
    /// Framing reserve `F`.
    pub framing: u64,
    /// Input ceiling `I = W - O - F`.
    pub input_ceiling: u64,
}

impl Budget {
    /// Resolve a budget, refusing a non-positive input ceiling.
    pub fn resolve(
        window: ModelWindow,
        output: OutputReserve,
        framing: FramingReserve,
    ) -> Result<Self, AdmissionError> {
        ModelBudgetContract {
            shared_window: Some(window.tokens),
            input_limit: None,
            output,
            output_limit: None,
            shared_margin: framing.tokens,
            input_margin: 0,
            provenance: BudgetProvenance::Configured { policy_revision: 1 },
        }
        .resolve()
    }

    /// Refuse when the mandatory blocks alone exceed the input ceiling.
    pub fn admit(&self, mandatory: &[BudgetComponent]) -> Result<(), AdmissionError> {
        let total = mandatory
            .iter()
            .try_fold(0u64, |total, c| total.checked_add(c.tokens))
            .ok_or(AdmissionError::ArithmeticOverflow)?;
        if total > self.input_ceiling {
            return Err(AdmissionError::ContextAdmissionExceeded {
                input_ceiling: self.input_ceiling,
                mandatory_tokens: total,
                components: mandatory.to_vec(),
            });
        }
        Ok(())
    }
}

/// Soft/hard/target watermarks as basis points of `I` (ADR-0277 §3, ADR-0280).
///
/// Hysteresis: below `soft` no reclamation runs; between `soft` and `hard`
/// lightweight degradation runs; at or above `hard` strong reclamation
/// (checkpointing) must be attempted. `target` is where a rewritten view aims,
/// and includes the checkpoint itself plus fixed overhead. A real send always
/// stays within `I`; mandatory blocks may make `target` unreachable.
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
    /// The ADR-0280 initial default: 0.65 / 0.85 / 0.50.
    pub const DEFAULT: Self = Self {
        soft_bp: 6500,
        hard_bp: 8500,
        target_bp: 5000,
    };

    /// Validate that `target <= soft <= hard <= 10000`.
    pub const fn validate(&self) -> Result<(), AdmissionError> {
        let ordered = self.target_bp <= self.soft_bp
            && self.soft_bp <= self.hard_bp
            && self.hard_bp <= 10_000;
        if ordered {
            Ok(())
        } else {
            Err(AdmissionError::InvalidWatermarks {
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

/// Per-item ceilings derived from `I` (ADR-0280 §2).
impl Budget {
    /// Observation preview ceiling: `min(2048, floor(0.05 * I))`, per item.
    pub const fn observation_preview_tokens(&self) -> u64 {
        let five_percent = self.input_ceiling / 20;
        if five_percent < 2048 {
            five_percent
        } else {
            2048
        }
    }

    /// Checkpoint output ceiling: `min(4096, floor(0.08 * I))`.
    pub const fn checkpoint_output_tokens(&self) -> u64 {
        let eight_percent = ((self.input_ceiling as u128 * 8) / 100) as u64;
        if eight_percent < 4096 {
            eight_percent
        } else {
            4096
        }
    }
}

/// Finite-planning bound (ADR-0277 §3, `INV-ADMIT-04`): a pass that makes no
/// progress after this many iterations returns a typed error.
pub const PLANNING_MAX_ITERATIONS: u32 = 8;

/// Bounded compare-and-swap retries for a view commit (ADR-0278 §4,
/// `INV-CKPT-04`).
pub const CAS_MAX_ATTEMPTS: u32 = 3;

#[cfg(test)]
mod tests {
    use super::*;

    fn budget(window: u64, visible: u64, reasoning: u64, framing: u64) -> Budget {
        Budget::resolve(
            ModelWindow::tokens(window),
            OutputReserve::Additive {
                visible_tokens: visible,
                reasoning_tokens: reasoning,
            },
            FramingReserve::tokens(framing),
        )
        .unwrap()
    }

    #[test]
    fn model_contract_distinguishes_inclusive_additive_and_independent_limits() {
        let mut contract = ModelBudgetContract {
            shared_window: Some(100_000),
            input_limit: Some(90_000),
            output: OutputReserve::Inclusive {
                total_tokens: 20_000,
            },
            output_limit: Some(40_000),
            shared_margin: 1_000,
            input_margin: 2_000,
            provenance: BudgetProvenance::Declared {
                source: "fixture".into(),
                revision: "1".into(),
            },
        };
        assert_eq!(contract.resolve().unwrap().input_ceiling, 79_000);
        contract.output = OutputReserve::Additive {
            visible_tokens: 20_000,
            reasoning_tokens: 10_000,
        };
        assert_eq!(contract.resolve().unwrap().input_ceiling, 69_000);
        contract.shared_window = None;
        assert_eq!(contract.resolve().unwrap().input_ceiling, 88_000);
        contract.input_limit = None;
        assert_eq!(
            contract.resolve(),
            Err(AdmissionError::BudgetContractUnavailable)
        );
    }

    #[test]
    fn overflowing_budgets_fail_closed_and_large_watermarks_remain_exact() {
        assert_eq!(
            OutputReserve::Additive {
                visible_tokens: u64::MAX,
                reasoning_tokens: 1
            }
            .total(),
            Err(AdmissionError::ArithmeticOverflow)
        );
        assert_eq!(
            WatermarkPolicy::DEFAULT.tokens_at(u64::MAX, 10_000),
            u64::MAX
        );
        let b = budget(100, 1, 0, 1);
        assert_eq!(
            b.admit(&[
                BudgetComponent {
                    name: "a".into(),
                    tokens: u64::MAX
                },
                BudgetComponent {
                    name: "b".into(),
                    tokens: 1
                },
            ]),
            Err(AdmissionError::ArithmeticOverflow)
        );
    }

    #[test]
    fn input_ceiling_subtracts_reasoning_from_output() {
        // The whole point of INV-ADMIT-02: reasoning is inside O.
        let b = budget(200_000, 8_000, 24_000, 4_000);
        assert_eq!(b.output, 32_000, "O includes reasoning");
        assert_eq!(b.input_ceiling, 200_000 - 32_000 - 4_000);
    }

    #[test]
    fn a_reasoning_heavy_call_shrinks_the_input_ceiling() {
        let without_reasoning = budget(100_000, 8_000, 0, 3_000);
        let with_reasoning = budget(100_000, 8_000, 32_000, 3_000);
        assert!(
            with_reasoning.input_ceiling < without_reasoning.input_ceiling,
            "reserving reasoning must reduce I, not be ignored"
        );
        assert_eq!(
            without_reasoning.input_ceiling - with_reasoning.input_ceiling,
            32_000
        );
    }

    #[test]
    fn zero_or_negative_ceiling_is_a_typed_error() {
        let err = Budget::resolve(
            ModelWindow::tokens(4_000),
            OutputReserve::Inclusive {
                total_tokens: 4_000,
            },
            FramingReserve::tokens(1024),
        )
        .unwrap_err();
        assert!(matches!(err, AdmissionError::ZeroInputCeiling { .. }));
    }

    #[test]
    fn mandatory_blocks_over_i_refuse_with_component_report() {
        let b = budget(100_000, 1_000, 0, 1_000);
        let components = vec![
            BudgetComponent {
                name: "system".into(),
                tokens: b.input_ceiling - 10,
            },
            BudgetComponent {
                name: "tools".into(),
                tokens: 50,
            },
        ];
        match b.admit(&components).unwrap_err() {
            AdmissionError::ContextAdmissionExceeded {
                mandatory_tokens,
                components,
                ..
            } => {
                assert_eq!(mandatory_tokens, b.input_ceiling + 40);
                assert_eq!(components.len(), 2, "the refusal names each component");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn watermarks_are_fractions_of_i_and_ordered() {
        let policy = WatermarkPolicy::DEFAULT;
        policy.validate().unwrap();
        let (soft, hard, target) = policy.thresholds(100_000);
        assert_eq!((soft, hard, target), (65_000, 85_000, 50_000));
        assert!(target <= soft && soft <= hard && hard <= 100_000);
    }

    #[test]
    fn watermark_order_is_enforced() {
        let bad = WatermarkPolicy {
            soft_bp: 5_000,
            hard_bp: 9_000,
            target_bp: 7_000, // target above soft
        };
        assert!(matches!(
            bad.validate().unwrap_err(),
            AdmissionError::InvalidWatermarks { .. }
        ));
    }

    #[test]
    fn framing_defaults_and_derived_ceilings_match_the_adr() {
        // F = max(1024, ceil(0.03 * W)).
        assert_eq!(
            FramingReserve::for_window(ModelWindow::tokens(10_000)).tokens,
            1024
        );
        assert_eq!(
            FramingReserve::for_window(ModelWindow::tokens(200_000)).tokens,
            6_000
        );
        // A ceiling that rounds 0.03*W up.
        assert_eq!(
            FramingReserve::for_window(ModelWindow::tokens(34_000)).tokens,
            1024
        );
        assert_eq!(
            FramingReserve::for_window(ModelWindow::tokens(100_000)).tokens,
            3_000
        );
    }

    #[test]
    fn per_item_ceilings_are_capped() {
        let big = budget(1_000_000, 1_000, 0, 1_000);
        assert_eq!(
            big.observation_preview_tokens(),
            2048,
            "0.05*I capped at 2048"
        );
        assert_eq!(
            big.checkpoint_output_tokens(),
            4096,
            "0.08*I capped at 4096"
        );
        let small = budget(20_000, 1_000, 0, 1_000);
        // I = 18000 → 0.05*I = 900, 0.08*I = 1440.
        assert_eq!(small.observation_preview_tokens(), 900);
        assert_eq!(small.checkpoint_output_tokens(), 1440);
    }

    #[test]
    fn the_same_inputs_produce_identical_thresholds() {
        // INV-ADMIT-03 determinism: integer math, no floats.
        let a = WatermarkPolicy::DEFAULT.thresholds(123_457);
        let b = WatermarkPolicy::DEFAULT.thresholds(123_457);
        assert_eq!(a, b);
        assert_eq!(a, (80_247, 104_938, 61_728));
        // Every watermark is a fraction of I, so the hard threshold is always
        // below I; the actual send gate is I itself.
        assert!(a.1 < 123_457);
    }
}
