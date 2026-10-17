//! Baseline model pricing registry, fixed-point calculations, and catalog resolution (ADR-0065, ADR-0066).
//!
//! Provides out-of-the-box multi-factor pricing rules for leading model families
//! (Google Gemini 4 Argon, Gemini 3.8 Flash, Gemini 2.5/2.0, Anthropic Claude, DeepSeek, OpenAI),
//! including prompt cache discounts, cache creation overheads, and context window thresholds (`[INV-PRICING-03]`).
//! Depends downward on `nuo_model_codec::TokenBreakdown` via Dependency Inversion (`[INV-DIP-01]`).

use nuo_model_codec::TokenUsage;
use nuo_model_codec::token_classification::TokenBreakdown;
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

/// Complete multi-tier pricing specification for an AI model (`[INV-PRICING-03]`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelPricing {
    /// Currency identifier, e.g. "USD", "CNY".
    #[serde(default = "default_currency")]
    pub currency: String,
    /// Ordered list of pricing tiers.
    pub tiers: Vec<PricingTier>,
}

fn default_currency() -> String {
    "USD".to_string()
}

impl ModelPricing {
    /// Create a single-tier pricing specification.
    pub fn single(tier: PricingTier) -> Self {
        Self {
            currency: "USD".to_string(),
            tiers: vec![tier],
        }
    }

    /// Resolve the matching pricing tier given the prompt context token size.
    pub fn resolve_tier(&self, prompt_tokens: u32) -> Option<&PricingTier> {
        if self.tiers.is_empty() {
            return None;
        }
        for tier in &self.tiers {
            if let Some(threshold) = tier.context_threshold {
                if prompt_tokens <= threshold {
                    return Some(tier);
                }
            } else {
                return Some(tier);
            }
        }
        self.tiers.last()
    }

    /// Calculate precise financial cost from a normalized [`TokenBreakdown`].
    pub fn calculate_breakdown_cost(&self, breakdown: &TokenBreakdown) -> TurnCost {
        let total_input = breakdown.total_input();
        let tier = match self.resolve_tier(total_input as u32) {
            Some(t) => t,
            None => return TurnCost::default(),
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
            input_cost,
            cache_read_cost,
            cache_write_cost,
            output_cost,
        }
    }

    /// Calculate precise financial cost for a turn's [`TokenUsage`].
    pub fn calculate_cost(&self, usage: &TokenUsage) -> TurnCost {
        let breakdown = TokenBreakdown::from_usage(usage);
        self.calculate_breakdown_cost(&breakdown)
    }
}

/// Detailed financial breakdown of a single request or turn.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TurnCost {
    /// Total cost in nano-USD.
    pub amount: NanoUsd,
    /// Currency code (e.g. "USD").
    pub currency: String,
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

/// Gemini 4 Argon baseline pricing (`gemini-4-argon`, Google Frontier flagship):
/// - Standard uncached input: $2.00 / 1M prompt
/// - Context caching read (implicit cache): $0.10 / 1M prompt (95% discount)
/// - Output: $10.00 / 1M completion (including extended thinking/reasoning)
pub fn gemini_4_argon() -> ModelPricing {
    ModelPricing::single(PricingTier {
        context_threshold: None,
        input_standard_nano_per_m: 2_000_000_000,
        cache_read_nano_per_m: 100_000_000,
        cache_write_nano_per_m: 0,
        output_standard_nano_per_m: 10_000_000_000,
    })
}

/// Gemini 3.8 Flash baseline pricing (`gemini-3.8-flash`, long-horizon SWE & autonomous agents):
/// - Standard uncached input: $0.75 / 1M prompt
/// - Context caching read: $0.075 / 1M prompt (90% discount)
/// - Output: $3.75 / 1M completion (including thinking tokens)
pub fn gemini_3_8_flash() -> ModelPricing {
    ModelPricing::single(PricingTier {
        context_threshold: None,
        input_standard_nano_per_m: 750_000_000,
        cache_read_nano_per_m: 75_000_000,
        cache_write_nano_per_m: 0,
        output_standard_nano_per_m: 3_750_000_000,
    })
}

/// Gemini 3.6 / 3.7 Flash baseline pricing:
pub fn gemini_3_flash() -> ModelPricing {
    gemini_3_8_flash()
}

/// Gemini 2.5 Pro tiered baseline pricing (`gemini-2.5-pro`):
/// - Context <= 200k: $2.00 / 1M input, $0.20 / 1M cache read, $12.00 / 1M output
/// - Context > 200k:  $4.00 / 1M input, $0.40 / 1M cache read, $18.00 / 1M output
pub fn gemini_2_5_pro() -> ModelPricing {
    ModelPricing {
        currency: "USD".to_string(),
        tiers: vec![
            PricingTier {
                context_threshold: Some(200_000),
                input_standard_nano_per_m: 2_000_000_000,
                cache_read_nano_per_m: 200_000_000,
                cache_write_nano_per_m: 0,
                output_standard_nano_per_m: 12_000_000_000,
            },
            PricingTier {
                context_threshold: None,
                input_standard_nano_per_m: 4_000_000_000,
                cache_read_nano_per_m: 400_000_000,
                cache_write_nano_per_m: 0,
                output_standard_nano_per_m: 18_000_000_000,
            },
        ],
    }
}

/// Gemini 2.0 Flash baseline pricing (`gemini-2.0-flash`):
pub fn gemini_2_0_flash() -> ModelPricing {
    ModelPricing::single(PricingTier {
        context_threshold: None,
        input_standard_nano_per_m: 100_000_000,
        cache_read_nano_per_m: 25_000_000,
        cache_write_nano_per_m: 0,
        output_standard_nano_per_m: 400_000_000,
    })
}

/// Gemini 2.0 Flash-Lite baseline pricing (`gemini-2.0-flash-lite`):
pub fn gemini_2_0_flash_lite() -> ModelPricing {
    ModelPricing::single(PricingTier {
        context_threshold: None,
        input_standard_nano_per_m: 75_000_000,
        cache_read_nano_per_m: 25_000_000,
        cache_write_nano_per_m: 0,
        output_standard_nano_per_m: 300_000_000,
    })
}

/// Gemini 1.5 Pro tiered baseline pricing (`gemini-1.5-pro`):
pub fn gemini_1_5_pro() -> ModelPricing {
    ModelPricing {
        currency: "USD".to_string(),
        tiers: vec![
            PricingTier {
                context_threshold: Some(128_000),
                input_standard_nano_per_m: 1_250_000_000,
                cache_read_nano_per_m: 312_500_000,
                cache_write_nano_per_m: 0,
                output_standard_nano_per_m: 5_000_000_000,
            },
            PricingTier {
                context_threshold: None,
                input_standard_nano_per_m: 2_500_000_000,
                cache_read_nano_per_m: 625_000_000,
                cache_write_nano_per_m: 0,
                output_standard_nano_per_m: 10_000_000_000,
            },
        ],
    }
}

/// Claude 3.5 / 3.7 Sonnet baseline pricing:
pub fn claude_sonnet() -> ModelPricing {
    ModelPricing::single(PricingTier {
        context_threshold: None,
        input_standard_nano_per_m: 3_000_000_000,
        cache_read_nano_per_m: 300_000_000,
        cache_write_nano_per_m: 3_750_000_000,
        output_standard_nano_per_m: 15_000_000_000,
    })
}

/// Claude 3.5 Haiku baseline pricing:
pub fn claude_haiku() -> ModelPricing {
    ModelPricing::single(PricingTier {
        context_threshold: None,
        input_standard_nano_per_m: 800_000_000,
        cache_read_nano_per_m: 80_000_000,
        cache_write_nano_per_m: 1_000_000_000,
        output_standard_nano_per_m: 4_000_000_000,
    })
}

/// DeepSeek Chat (V3) baseline pricing:
pub fn deepseek_chat() -> ModelPricing {
    ModelPricing::single(PricingTier {
        context_threshold: None,
        input_standard_nano_per_m: 140_000_000,
        cache_read_nano_per_m: 14_000_000,
        cache_write_nano_per_m: 0,
        output_standard_nano_per_m: 280_000_000,
    })
}

/// DeepSeek Reasoner (R1) baseline pricing:
pub fn deepseek_r1() -> ModelPricing {
    ModelPricing::single(PricingTier {
        context_threshold: None,
        input_standard_nano_per_m: 550_000_000,
        cache_read_nano_per_m: 140_000_000,
        cache_write_nano_per_m: 0,
        output_standard_nano_per_m: 2_190_000_000,
    })
}

/// OpenAI GPT-4o baseline pricing:
pub fn gpt_4o() -> ModelPricing {
    ModelPricing::single(PricingTier {
        context_threshold: None,
        input_standard_nano_per_m: 2_500_000_000,
        cache_read_nano_per_m: 1_250_000_000,
        cache_write_nano_per_m: 0,
        output_standard_nano_per_m: 10_000_000_000,
    })
}

/// OpenAI GPT-4o-mini baseline pricing:
pub fn gpt_4o_mini() -> ModelPricing {
    ModelPricing::single(PricingTier {
        context_threshold: None,
        input_standard_nano_per_m: 150_000_000,
        cache_read_nano_per_m: 75_000_000,
        cache_write_nano_per_m: 0,
        output_standard_nano_per_m: 600_000_000,
    })
}

/// Resolve built-in pricing specification for a provider and model id.
pub fn resolve_model_pricing(provider: &str, model: &str) -> Option<ModelPricing> {
    let p = provider.to_ascii_lowercase();
    let m = model.to_ascii_lowercase();

    if p.contains("google") || p.contains("gemini") || m.contains("gemini") {
        if m.contains("argon") || m.contains("gemini-4") {
            return Some(gemini_4_argon());
        }
        if m.contains("3.8") || m.contains("3.6") || m.contains("3.7") || m.contains("gemini-3") {
            return Some(gemini_3_8_flash());
        }
        if m.contains("2.5-pro") || m.contains("2.5_pro") {
            return Some(gemini_2_5_pro());
        }
        if m.contains("lite") {
            return Some(gemini_2_0_flash_lite());
        }
        if m.contains("1.5-pro") || m.contains("1.5_pro") {
            return Some(gemini_1_5_pro());
        }
        if m.contains("pro") {
            return Some(gemini_2_5_pro());
        }
        return Some(gemini_2_0_flash());
    }

    if p.contains("anthropic") || p.contains("claude") || m.contains("claude") {
        if m.contains("haiku") {
            return Some(claude_haiku());
        }
        if m.contains("sonnet") || m.contains("opus") {
            return Some(claude_sonnet());
        }
    }

    if p.contains("deepseek") || m.contains("deepseek") {
        if m.contains("reasoner") || m.contains("r1") {
            return Some(deepseek_r1());
        }
        return Some(deepseek_chat());
    }

    if p.contains("openai") || m.contains("gpt") {
        if m.contains("mini") {
            return Some(gpt_4o_mini());
        }
        if m.contains("4o") {
            return Some(gpt_4o());
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gemini_4_argon_pricing_and_implicit_caching() {
        let pricing = resolve_model_pricing("google", "gemini-4-argon").unwrap();
        let usage = TokenUsage {
            prompt_tokens: 1_000_000,
            completion_tokens: 10_000,
            total_tokens: 1_010_000,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 900_000,
            cache_miss_input_tokens: 100_000,
            reasoning_tokens: 5_000,
        };
        let cost = pricing.calculate_cost(&usage);

        assert_eq!(cost.input_cost.0, 200_000_000);
        assert_eq!(cost.cache_read_cost.0, 90_000_000);
        assert_eq!(cost.output_cost.0, 100_000_000);
        assert_eq!(cost.amount.0, 390_000_000);
        assert_eq!(cost.amount.format_usd(), "$0.39");
    }

    #[test]
    fn test_gemini_3_8_flash_swe_workhorse_pricing() {
        let pricing = resolve_model_pricing("google", "gemini-3.8-flash").unwrap();
        let usage = TokenUsage {
            prompt_tokens: 200_000,
            completion_tokens: 2_000,
            total_tokens: 202_000,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 150_000,
            cache_miss_input_tokens: 50_000,
            reasoning_tokens: 1_000,
        };
        let cost = pricing.calculate_cost(&usage);

        assert_eq!(cost.input_cost.0, 37_500_000);
        assert_eq!(cost.cache_read_cost.0, 11_250_000);
        assert_eq!(cost.output_cost.0, 7_500_000);
        assert_eq!(cost.amount.0, 56_250_000);
    }
}
