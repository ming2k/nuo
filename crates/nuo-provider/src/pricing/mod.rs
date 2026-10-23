//! Baseline model pricing registry, fixed-point calculations, and catalog resolution
//! (ADR-0065, ADR-0066, ADR-0080).
//!
//! Provides out-of-the-box multi-factor pricing rules for leading model families
//! (Google Gemini 4 Argon, Gemini 3.8 Flash, Gemini 2.5/2.0, Anthropic Claude, DeepSeek, OpenAI),
//! including prompt cache discounts, cache creation overheads, context window
//! thresholds (`[INV-PRICING-03]`), and — for upstreams that publish them —
//! time-of-day rate cards.
//!
//! Depends downward on `nuo_tool::TokenBreakdown` via Dependency
//! Inversion (`[INV-DIP-01]`).
//!
//! # Rating is a pure function of `(usage, instant)`
//!
//! Every cost entry point takes a [`RatingInstant`]. The module never reads a
//! system clock and never reads the user's time zone: the instant comes from
//! the request record (`started_at_ms`), so re-rating a persisted attempt
//! reproduces the money it was settled at, and the off-peak decision is made in
//! the *upstream's* clock rather than the operator's.

mod registry;
mod schedule;

pub use registry::resolve_model_pricing;
pub use schedule::{
    CivilDate, HolidayRange, MINUTES_PER_DAY, PeakWindow, RateSchedule, RateWindow, RatingInstant,
    Weekday, WeekdayMask,
};

use nuo_inference_wire::token_classification::TokenBreakdown;
use nuo_tool::TokenUsage;
use serde::{Deserialize, Serialize};

/// Fixed-point nanodollar amount (1 USD = 1,000,000,000 nano-USD).
///
/// Prevents IEEE-754 precision drift across millions of token transactions (`[INV-PRICING-02]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize, Deserialize)]
pub struct NanoUsd(pub u64);

impl NanoUsd {
    pub const ZERO: Self = Self(0);
    pub const ONE_USD: Self = Self(1_000_000_000);
    pub const ONE_CENT: Self = Self(10_000_000);

    /// Construct from dollars (f64), clamped to 0.
    pub fn from_usd_f64(usd: f64) -> Self {
        if usd <= 0.0 {
            Self::ZERO
        } else {
            Self((usd * 1_000_000_000.0).round() as u64)
        }
    }

    /// Convert to floating-point USD representation for display.
    pub fn to_usd_f64(self) -> f64 {
        self.0 as f64 / 1_000_000_000.0
    }

    /// Format as human-readable USD string (e.g. `"$0.0024"`, `"$1.25"`).
    pub fn format_usd(self) -> String {
        let usd = self.to_usd_f64();
        if usd == 0.0 {
            "$0.00".to_string()
        } else if usd < 0.001 {
            format!("${:.6}", usd)
        } else if usd < 0.01 {
            format!("${:.4}", usd)
        } else {
            format!("${:.2}", usd)
        }
    }

    /// Format as unit price rate per 1M tokens (e.g. `"$0.30"`, `"$0.006"`, `"$1.20"`).
    pub fn format_rate(self) -> String {
        let usd = self.to_usd_f64();
        if usd == 0.0 {
            "$0.00".to_string()
        } else if usd < 0.01 {
            let s = format!("{usd:.4}");
            format!("${}", s.trim_end_matches('0'))
        } else {
            format!("${usd:.2}")
        }
    }

    pub fn saturating_add(self, other: Self) -> Self {
        Self(self.0.saturating_add(other.0))
    }
}

impl std::ops::Add for NanoUsd {
    type Output = Self;
    fn add(self, rhs: Self) -> Self::Output {
        Self(self.0.saturating_add(rhs.0))
    }
}

impl std::ops::AddAssign for NanoUsd {
    fn add_assign(&mut self, rhs: Self) {
        self.0 = self.0.saturating_add(rhs.0);
    }
}

impl std::iter::Sum for NanoUsd {
    fn sum<I: Iterator<Item = Self>>(iter: I) -> Self {
        iter.fold(Self::ZERO, |a, b| a + b)
    }
}

/// Pricing tier for a model, optionally guarded by a context window threshold.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PricingTier {
    /// Context window upper bound in tokens for this pricing tier.
    /// `None` indicates the default or unbounded tier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_threshold: Option<u32>,
    /// Standard uncached input cost in nano-USD per 1M tokens.
    pub input_standard_nano_per_m: u64,
    /// Prompt cache read/hit cost in nano-USD per 1M tokens.
    pub cache_read_nano_per_m: u64,
    /// Prompt cache write/creation cost in nano-USD per 1M tokens.
    pub cache_write_nano_per_m: u64,
    /// Output/completion generation cost in nano-USD per 1M tokens.
    pub output_standard_nano_per_m: u64,
}

impl PricingTier {
    /// Calculate cost for a discrete token count using fixed-point integer math.
    fn calculate_factor_cost(tokens: u64, nano_per_m: u64) -> NanoUsd {
        if tokens == 0 || nano_per_m == 0 {
            return NanoUsd::ZERO;
        }
        let total_nano = (tokens as u128 * nano_per_m as u128) / 1_000_000;
        NanoUsd(total_nano.min(u64::MAX as u128) as u64)
    }
}

/// The discounted card an upstream grants outside its peak windows, together
/// with the schedule that decides when that is.
///
/// Both halves live in one type so "a schedule without a discounted card" and
/// "a discounted card without a schedule" are unrepresentable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OffPeakPricing {
    /// Rate cards applied while the schedule reports [`RateWindow::OffPeak`].
    pub tiers: Vec<PricingTier>,
    /// The calendar that selects between this card and the base card.
    pub schedule: RateSchedule,
}

/// Complete multi-tier pricing specification for an AI model (`[INV-PRICING-03]`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelPricing {
    /// Currency identifier, e.g. "USD", "CNY".
    #[serde(default = "default_currency")]
    pub currency: String,
    /// Base rate cards, ordered by context threshold. These are the upstream's
    /// standard (peak) prices.
    pub tiers: Vec<PricingTier>,
    /// Time-varying discount cards. `None` prices every hour the same way.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub off_peak: Option<OffPeakPricing>,
}

fn default_currency() -> String {
    "USD".to_string()
}

impl ModelPricing {
    /// Create a single-tier, single-window pricing specification.
    pub fn single(tier: PricingTier) -> Self {
        Self {
            currency: "USD".to_string(),
            tiers: vec![tier],
            off_peak: None,
        }
    }

    /// Add the off-peak card set and the schedule that selects it.
    pub fn with_off_peak(mut self, tiers: Vec<PricingTier>, schedule: RateSchedule) -> Self {
        assert_eq!(
            tiers.len(),
            self.tiers.len(),
            "an off-peak card set must mirror the base card set's tier count"
        );
        assert!(
            tiers.iter().all(|tier| tier.context_threshold.is_some())
                || tiers.iter().all(|tier| tier.context_threshold.is_none()),
            "off-peak tiers must agree with the base tiers on whether they are threshold-tiered"
        );
        self.off_peak = Some(OffPeakPricing { tiers, schedule });
        self
    }

    /// A zero-cost specification for plan-included models.
    pub fn zero() -> Self {
        Self::single(PricingTier {
            context_threshold: None,
            input_standard_nano_per_m: 0,
            cache_read_nano_per_m: 0,
            cache_write_nano_per_m: 0,
            output_standard_nano_per_m: 0,
        })
    }

    /// Which card applies to `instant`.
    pub fn window_at(&self, instant: RatingInstant) -> RateWindow {
        self.off_peak
            .as_ref()
            .map(|off_peak| off_peak.schedule.window_at(instant))
            .unwrap_or(RateWindow::Peak)
    }

    /// The rate cards that apply to `instant`.
    pub fn tiers_at(&self, instant: RatingInstant) -> &[PricingTier] {
        match (&self.off_peak, self.window_at(instant)) {
            (Some(off_peak), RateWindow::OffPeak) => &off_peak.tiers,
            _ => &self.tiers,
        }
    }

    /// Resolve the matching pricing tier for the prompt context size *and* the
    /// rating instant.
    pub fn resolve_tier(&self, prompt_tokens: u32, instant: RatingInstant) -> Option<&PricingTier> {
        select_tier(self.tiers_at(instant), prompt_tokens)
    }

    /// Calculate precise financial cost from a normalized [`TokenBreakdown`].
    pub fn calculate_breakdown_cost(
        &self,
        breakdown: &TokenBreakdown,
        instant: RatingInstant,
    ) -> TurnCost {
        let total_input = breakdown.total_input();
        let Some(tier) = self.resolve_tier(total_input as u32, instant) else {
            return TurnCost {
                currency: self.currency.clone(),
                window: self.window_at(instant),
                ..TurnCost::default()
            };
        };

        let input_cost = PricingTier::calculate_factor_cost(
            breakdown.uncached_input,
            tier.input_standard_nano_per_m,
        );
        let cache_read_cost =
            PricingTier::calculate_factor_cost(breakdown.cache_read, tier.cache_read_nano_per_m);
        let cache_write_cost =
            PricingTier::calculate_factor_cost(breakdown.cache_write, tier.cache_write_nano_per_m);
        let output_cost = PricingTier::calculate_factor_cost(
            breakdown.total_output(),
            tier.output_standard_nano_per_m,
        );

        let total_nano = input_cost + cache_read_cost + cache_write_cost + output_cost;

        TurnCost {
            amount: total_nano,
            currency: self.currency.clone(),
            window: self.window_at(instant),
            input_cost,
            cache_read_cost,
            cache_write_cost,
            output_cost,
        }
    }

    /// Calculate precise financial cost for a turn's [`TokenUsage`].
    pub fn calculate_cost(&self, usage: &TokenUsage, instant: RatingInstant) -> TurnCost {
        let breakdown = TokenBreakdown::from_counts(
            usage.prompt_tokens,
            usage.completion_tokens,
            usage.cache_read_input_tokens,
            usage.cache_creation_input_tokens,
            usage.reasoning_tokens,
        );
        self.calculate_breakdown_cost(&breakdown, instant)
    }
}

/// Select the first card whose threshold covers `prompt_tokens`, falling back
/// to the last (unbounded) card (`[INV-PRICING-03]`).
fn select_tier(tiers: &[PricingTier], prompt_tokens: u32) -> Option<&PricingTier> {
    for tier in tiers {
        if let Some(threshold) = tier.context_threshold {
            if prompt_tokens <= threshold {
                return Some(tier);
            }
        } else {
            return Some(tier);
        }
    }
    tiers.last()
}

/// Detailed financial breakdown of a single request or turn.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TurnCost {
    /// Total cost in nano-USD.
    pub amount: NanoUsd,
    /// Currency code (e.g. "USD").
    pub currency: String,
    /// Which published card this turn was rated on. Recorded so a settled cost
    /// stays auditable after the calendar or the rates move.
    #[serde(default)]
    pub window: RateWindow,
    /// Cost attributable to standard uncached input tokens.
    pub input_cost: NanoUsd,
    /// Cost attributable to cache read / hits.
    pub cache_read_cost: NanoUsd,
    /// Cost attributable to cache creation / writes.
    pub cache_write_cost: NanoUsd,
    /// Cost attributable to output / completion tokens.
    pub output_cost: NanoUsd,
}

impl TurnCost {
    pub fn is_zero(&self) -> bool {
        self.amount == NanoUsd::ZERO
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nano_usd_formats_unit_rates_cleanly() {
        assert_eq!(NanoUsd(300_000_000).format_rate(), "$0.30");
        assert_eq!(NanoUsd(1_200_000_000).format_rate(), "$1.20");
        assert_eq!(NanoUsd(6_000_000).format_rate(), "$0.006");
        assert_eq!(NanoUsd(3_000_000).format_rate(), "$0.003");
        assert_eq!(NanoUsd(0).format_rate(), "$0.00");
    }

    #[test]
    fn tiered_cards_split_at_their_threshold() {
        let pricing = ModelPricing {
            currency: "USD".to_string(),
            tiers: vec![
                PricingTier {
                    context_threshold: Some(200_000),
                    input_standard_nano_per_m: 1_000,
                    cache_read_nano_per_m: 100,
                    cache_write_nano_per_m: 0,
                    output_standard_nano_per_m: 2_000,
                },
                PricingTier {
                    context_threshold: None,
                    input_standard_nano_per_m: 3_000,
                    cache_read_nano_per_m: 300,
                    cache_write_nano_per_m: 0,
                    output_standard_nano_per_m: 6_000,
                },
            ],
            off_peak: None,
        };
        let instant = RatingInstant::from_epoch_ms(1);
        assert_eq!(
            pricing
                .resolve_tier(200_000, instant)
                .unwrap()
                .input_standard_nano_per_m,
            1_000
        );
        assert_eq!(
            pricing
                .resolve_tier(200_001, instant)
                .unwrap()
                .input_standard_nano_per_m,
            3_000
        );
        assert_eq!(pricing.window_at(instant), RateWindow::Peak);
    }

    #[test]
    fn off_peak_cards_must_mirror_the_base_card_shape() {
        let base = ModelPricing::single(PricingTier {
            context_threshold: None,
            input_standard_nano_per_m: 10,
            cache_read_nano_per_m: 1,
            cache_write_nano_per_m: 0,
            output_standard_nano_per_m: 20,
        });
        let schedule = RateSchedule::new(480, vec![PeakWindow::new(WeekdayMask::ALL, 0, 60)]);
        let mirrored = base.clone().with_off_peak(
            vec![PricingTier {
                context_threshold: None,
                input_standard_nano_per_m: 5,
                cache_read_nano_per_m: 0,
                cache_write_nano_per_m: 0,
                output_standard_nano_per_m: 10,
            }],
            schedule,
        );
        assert_eq!(mirrored.tiers.len(), 1);
        assert!(mirrored.off_peak.is_some());
    }

    #[test]
    fn zero_priced_models_stay_free_under_every_window() {
        let pricing = ModelPricing::zero();
        let usage = TokenUsage {
            prompt_tokens: 1_000_000,
            completion_tokens: 500_000,
            total_tokens: 1_500_000,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 900_000,
            cache_miss_input_tokens: 100_000,
            reasoning_tokens: 0,
        };
        let cost = pricing.calculate_cost(&usage, RatingInstant::UNKNOWN);
        assert_eq!(cost.amount, NanoUsd::ZERO);
        assert!(cost.is_zero());
    }
}
