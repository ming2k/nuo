//! Built-in rate cards and the `(provider, model)` → specification resolver.
//!
//! Table shape follows one rule: a published price is transcribed once, next to
//! the id it belongs to, and every retired id is an explicit alias of exactly
//! one canonical card. There is no family-wide fallback inside a vendor — an
//! unknown DeepSeek id yields `None` (unknown price) rather than inheriting a
//! neighbour's rate card, because a 14x under-estimate silently disables the
//! local spend breaker while an explicit "unknown" is visible.

use super::{
    CivilDate, HolidayRange, ModelPricing, PeakWindow, PricingTier, RateSchedule, WeekdayMask,
};

/// Resolve the built-in pricing specification for a provider and model id.
///
/// The lookup key is the wire id without any relay vendor prefix
/// (`deepseek/deepseek-v4-flash` → `deepseek-v4-flash`), lowercased. Ids carrying
/// a `-free` suffix are plan-included lanes and resolve to a zero card.
pub fn resolve_model_pricing(provider: &str, model: &str) -> Option<ModelPricing> {
    let id = lookup_id(model);
    if id.is_empty() {
        return None;
    }
    // `-free` is a lane marker, not a vendor name: the model is included in a
    // plan and the operator is not billed per token (ADR-0080).
    if id.ends_with("-free") {
        return Some(ModelPricing::zero());
    }
    if is_deepseek(provider, &id) {
        return deepseek(&id);
    }

    let p = provider.to_ascii_lowercase();
    if p.contains("google") || p.contains("gemini") || id.contains("gemini") {
        return gemini(&id);
    }
    if p.contains("anthropic") || p.contains("claude") || id.contains("claude") {
        return anthropic(&id);
    }
    if p.contains("openai") || id.contains("gpt") {
        return openai(&id);
    }
    None
}

/// Lowercase the wire id and drop a relay's `vendor/` namespace.
fn lookup_id(model: &str) -> String {
    let lower = model.trim().to_ascii_lowercase();
    match lower.rsplit_once('/') {
        Some((_, tail)) if !tail.is_empty() => tail.to_string(),
        _ => lower,
    }
}

fn is_deepseek(provider: &str, id: &str) -> bool {
    provider.to_ascii_lowercase().contains("deepseek") || id.contains("deepseek")
}

// ---------------------------------------------------------------------------
// DeepSeek
// ---------------------------------------------------------------------------

/// DeepSeek V4.1 Flash canonical wire id.
const DEEPSEEK_FLASH_ID: &str = "deepseek-flash";

/// Wire ids upstream has retired but still accepts — and still bills at Flash
/// rates. Retired from the seed list, retained forever as price aliases: a
/// resumed session or a hand-written connection can carry them.
const DEEPSEEK_FLASH_RETIRED_IDS: &[&str] = &["deepseek-v4-flash", "deepseek-v4-flash-vision-exp"];

/// Relay spellings of the same Flash model (`opencode`, `commandcode`,
/// `novita`). Never `api.deepseek.com` ids (ADR-0083), but a relay route serves
/// the same model and bills it at Flash rates, so it resolves to that card.
const DEEPSEEK_FLASH_RELAY_IDS: &[&str] = &["deepseek-v4.1-flash"];

fn deepseek(id: &str) -> Option<ModelPricing> {
    if id == DEEPSEEK_FLASH_ID
        || DEEPSEEK_FLASH_RETIRED_IDS.contains(&id)
        || DEEPSEEK_FLASH_RELAY_IDS.contains(&id)
    {
        return Some(deepseek_v4_flash());
    }
    if id == "deepseek-v4-pro" {
        return Some(deepseek_v4_pro());
    }
    // Legacy V3 / R1 generations: still callable through relays and old
    // sessions. They are single-window cards — no upstream publishes off-peak
    // rates for them any more.
    if id.contains("reasoner") || id.ends_with("-r1") {
        return Some(deepseek_r1());
    }
    if id.contains("chat") || id.contains("-v3") {
        return Some(deepseek_chat());
    }
    None
}

/// DeepSeek V4.1 Flash (`deepseek-flash`) — the canonical small model.
///
/// Peak (base) rates, USD per 1M tokens: $0.30 cache-miss input, $0.006
/// cache-hit input, no cache-write charge, $1.20 output (reasoning included).
/// Off-peak halves every factor: $0.15 / $0.003 / $0.60.
pub fn deepseek_v4_flash() -> ModelPricing {
    ModelPricing::single(PricingTier {
        context_threshold: None,
        input_standard_nano_per_m: 300_000_000,
        cache_read_nano_per_m: 6_000_000,
        cache_write_nano_per_m: 0,
        output_standard_nano_per_m: 1_200_000_000,
    })
    .with_off_peak(
        vec![PricingTier {
            context_threshold: None,
            input_standard_nano_per_m: 150_000_000,
            cache_read_nano_per_m: 3_000_000,
            cache_write_nano_per_m: 0,
            output_standard_nano_per_m: 600_000_000,
        }],
        deepseek_rate_schedule(),
    )
}

/// DeepSeek V4 Pro (`deepseek-v4-pro`).
///
/// Peak rates, USD per 1M tokens: $1.32 cache-miss input, $0.044 cache-hit
/// input, no cache-write charge, $3.96 output. Off-peak halves every factor:
/// $0.66 / $0.022 / $1.98.
pub fn deepseek_v4_pro() -> ModelPricing {
    ModelPricing::single(PricingTier {
        context_threshold: None,
        input_standard_nano_per_m: 1_320_000_000,
        cache_read_nano_per_m: 44_000_000,
        cache_write_nano_per_m: 0,
        output_standard_nano_per_m: 3_960_000_000,
    })
    .with_off_peak(
        vec![PricingTier {
            context_threshold: None,
            input_standard_nano_per_m: 660_000_000,
            cache_read_nano_per_m: 22_000_000,
            cache_write_nano_per_m: 0,
            output_standard_nano_per_m: 1_980_000_000,
        }],
        deepseek_rate_schedule(),
    )
}

/// DeepSeek Chat (V3) — legacy, single-window.
pub fn deepseek_chat() -> ModelPricing {
    ModelPricing::single(PricingTier {
        context_threshold: None,
        input_standard_nano_per_m: 140_000_000,
        cache_read_nano_per_m: 14_000_000,
        cache_write_nano_per_m: 0,
        output_standard_nano_per_m: 280_000_000,
    })
}

/// DeepSeek Reasoner (R1) — legacy, single-window.
pub fn deepseek_r1() -> ModelPricing {
    ModelPricing::single(PricingTier {
        context_threshold: None,
        input_standard_nano_per_m: 550_000_000,
        cache_read_nano_per_m: 140_000_000,
        cache_write_nano_per_m: 0,
        output_standard_nano_per_m: 2_190_000_000,
    })
}

/// Asia/Shanghai is UTC+08:00 permanently — the PRC has not observed daylight
/// saving since 1991, so a fixed offset is exact rather than an approximation.
const ASIA_SHANGHAI_OFFSET_MINUTES: i16 = 8 * 60;

/// Last date [`CN_2026_HOLIDAYS`] describes (inclusive).
const CN_2026_HOLIDAYS_COVERED_UNTIL: CivilDate = CivilDate::new(2026, 12, 31);

/// Date by which the holiday table must be extended to cover the next year, or
/// `cn_holiday_table_is_well_formed_and_current` fails. The State Council
/// publishes the following year's arrangement in November, hence the deadline.
#[cfg(test)]
const CN_HOLIDAY_TABLE_REVIEW_DEADLINE: CivilDate = CivilDate::new(2026, 11, 30);

/// Chinese statutory holiday periods for 2026, from
/// 《国务院办公厅关于2026年部分节假日安排的通知》(国办发明电〔2025〕7号,
/// published 2025-11-04), every `(year, from_month, from_day, through_month,
/// through_day)` transcribed as published.
///
/// Upstream states peak pricing as "Monday to Friday, excluding Chinese
/// statutory holidays", so a holiday suspends the windows for the whole day
/// rather than only outside them. Entries whose days fall on a weekend are
/// no-ops (weekends are already off-peak) and are kept so the table can be
/// diffed against the notice line for line.
const CN_2026_HOLIDAYS: &[(u8, u8, u8, u8)] = &[
    (1, 1, 1, 3),   // 元旦
    (2, 15, 2, 23), // 春节
    (4, 4, 4, 6),   // 清明节
    (5, 1, 5, 5),   // 劳动节
    (6, 19, 6, 21), // 端午节
    (9, 25, 9, 27), // 中秋节
    (10, 1, 10, 7), // 国庆节
];

/// Year the [`CN_2026_HOLIDAYS`] table belongs to.
const CN_2026_HOLIDAY_YEAR: i32 = 2026;

/// DeepSeek's published peak calendar.
///
/// Peak is 09:00–12:00 and 14:00–18:00 Beijing time, Monday to Friday,
/// excluding the holidays above; every other hour — evenings, nights, weekends,
/// and holidays in full — is off-peak. Windows are expressed in the anchor
/// zone exactly as upstream states them, so the table reads like the price page
/// instead of as UTC arithmetic.
fn deepseek_rate_schedule() -> RateSchedule {
    RateSchedule::new(
        ASIA_SHANGHAI_OFFSET_MINUTES,
        vec![
            PeakWindow::new(WeekdayMask::MON_TO_FRI, 9 * 60, 12 * 60),
            PeakWindow::new(WeekdayMask::MON_TO_FRI, 14 * 60, 18 * 60),
        ],
    )
    .with_holidays(
        CN_2026_HOLIDAYS
            .iter()
            .map(|&(from_month, from_day, through_month, through_day)| {
                HolidayRange::new(
                    CivilDate::new(CN_2026_HOLIDAY_YEAR, from_month, from_day),
                    CivilDate::new(CN_2026_HOLIDAY_YEAR, through_month, through_day),
                )
            })
            .collect(),
        CN_2026_HOLIDAYS_COVERED_UNTIL,
    )
}

// ---------------------------------------------------------------------------
// Google
// ---------------------------------------------------------------------------

fn gemini(id: &str) -> Option<ModelPricing> {
    if id.contains("argon") || id.contains("gemini-4") {
        return Some(gemini_4_argon());
    }
    if id.contains("3.8") || id.contains("3.6") || id.contains("3.7") || id.contains("gemini-3") {
        return Some(gemini_3_8_flash());
    }
    if id.contains("2.5-pro") || id.contains("2.5_pro") {
        return Some(gemini_2_5_pro());
    }
    if id.contains("lite") {
        return Some(gemini_2_0_flash_lite());
    }
    if id.contains("1.5-pro") || id.contains("1.5_pro") {
        return Some(gemini_1_5_pro());
    }
    if id.contains("pro") {
        return Some(gemini_2_5_pro());
    }
    Some(gemini_2_0_flash())
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
        off_peak: None,
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
        off_peak: None,
    }
}

// ---------------------------------------------------------------------------
// Anthropic
// ---------------------------------------------------------------------------

fn anthropic(id: &str) -> Option<ModelPricing> {
    if id.contains("haiku") {
        return Some(claude_haiku());
    }
    if id.contains("sonnet") || id.contains("opus") {
        return Some(claude_sonnet());
    }
    None
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

// ---------------------------------------------------------------------------
// OpenAI
// ---------------------------------------------------------------------------

fn openai(id: &str) -> Option<ModelPricing> {
    if id.contains("mini") {
        return Some(gpt_4o_mini());
    }
    if id.contains("4o") {
        return Some(gpt_4o());
    }
    None
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pricing::{NanoUsd, RateWindow, RatingInstant};
    use nuo_model_codec::TokenUsage;

    /// 2026-10-19 (Monday) at `hour:minute` UTC.
    fn utc_monday(hour: u64, minute: u64) -> RatingInstant {
        let monday = CivilDate::new(2026, 10, 19).days_since_epoch();
        RatingInstant::from_epoch_ms(
            (monday * 86_400_000 + hour as i64 * 3_600_000 + minute as i64 * 60_000) as u64,
        )
    }

    fn usage() -> TokenUsage {
        TokenUsage {
            prompt_tokens: 1_000_000,
            completion_tokens: 1_000_000,
            total_tokens: 2_000_000,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            cache_miss_input_tokens: 1_000_000,
            reasoning_tokens: 0,
        }
    }

    #[test]
    fn deepseek_flash_rates_both_windows() {
        let pricing = resolve_model_pricing("deepseek", "deepseek-flash").unwrap();
        // 01:00 UTC Monday == 09:00 Beijing, inside the peak window.
        let peak = pricing.calculate_cost(&usage(), utc_monday(1, 30));
        assert_eq!(peak.window, RateWindow::Peak);
        assert_eq!(peak.input_cost.0, 300_000_000);
        assert_eq!(peak.output_cost.0, 1_200_000_000);
        assert_eq!(peak.amount.0, 1_500_000_000);

        // 12:00 UTC Monday == 20:00 Beijing, off-peak.
        let off_peak = pricing.calculate_cost(&usage(), utc_monday(12, 0));
        assert_eq!(off_peak.window, RateWindow::OffPeak);
        assert_eq!(off_peak.input_cost.0, 150_000_000);
        assert_eq!(off_peak.output_cost.0, 600_000_000);
        assert_eq!(off_peak.amount.0, 750_000_000);
    }

    #[test]
    fn deepseek_pro_rates_both_windows() {
        let pricing = resolve_model_pricing("deepseek", "deepseek-v4-pro").unwrap();
        let peak = pricing.calculate_cost(&usage(), utc_monday(1, 30));
        assert_eq!(peak.amount.0, 1_320_000_000 + 3_960_000_000);
        let off_peak = pricing.calculate_cost(&usage(), utc_monday(12, 0));
        assert_eq!(off_peak.amount.0, 660_000_000 + 1_980_000_000);
    }

    #[test]
    fn off_peak_halves_every_published_factor() {
        for id in ["deepseek-flash", "deepseek-v4-pro"] {
            let pricing = resolve_model_pricing("deepseek", id).unwrap();
            let off_peak = pricing.off_peak.as_ref().unwrap();
            for (base, discounted) in pricing.tiers.iter().zip(&off_peak.tiers) {
                assert_eq!(
                    base.input_standard_nano_per_m,
                    discounted.input_standard_nano_per_m * 2,
                    "{id}: input"
                );
                assert_eq!(
                    base.cache_read_nano_per_m,
                    discounted.cache_read_nano_per_m * 2,
                    "{id}: cache read"
                );
                assert_eq!(
                    base.output_standard_nano_per_m,
                    discounted.output_standard_nano_per_m * 2,
                    "{id}: output"
                );
            }
        }
    }

    #[test]
    fn retired_wire_ids_alias_the_flash_card() {
        let canonical = resolve_model_pricing("deepseek", "deepseek-flash").unwrap();
        for id in DEEPSEEK_FLASH_RETIRED_IDS {
            let retired = resolve_model_pricing("deepseek", id).unwrap();
            assert_eq!(retired, canonical, "{id} must price as deepseek-flash");
        }
        // Relays namespace the vendor: the prefix is not part of the rate key.
        assert_eq!(
            resolve_model_pricing("commandcode", "deepseek/deepseek-v4-flash").unwrap(),
            canonical
        );
    }

    #[test]
    fn peak_windows_match_the_published_beijing_schedule() {
        let pricing = resolve_model_pricing("deepseek", "deepseek-flash").unwrap();
        let schedule = &pricing.off_peak.as_ref().unwrap().schedule;
        // Beijing 08:59 (00:59 UTC) off-peak; 09:00 (01:00 UTC) peak.
        assert_eq!(schedule.window_at(utc_monday(0, 59)), RateWindow::OffPeak);
        assert_eq!(schedule.window_at(utc_monday(1, 0)), RateWindow::Peak);
        // Beijing 11:59 (03:59 UTC) peak; 12:00 (04:00 UTC) lunch trough.
        assert_eq!(schedule.window_at(utc_monday(3, 59)), RateWindow::Peak);
        assert_eq!(schedule.window_at(utc_monday(4, 0)), RateWindow::OffPeak);
        // Beijing 13:59 (05:59 UTC) still off-peak; 14:00 (06:00 UTC) peak again.
        assert_eq!(schedule.window_at(utc_monday(5, 59)), RateWindow::OffPeak);
        assert_eq!(schedule.window_at(utc_monday(6, 0)), RateWindow::Peak);
        // Beijing 17:59 (09:59 UTC) peak; 18:00 (10:00 UTC) off-peak.
        assert_eq!(schedule.window_at(utc_monday(9, 59)), RateWindow::Peak);
        assert_eq!(schedule.window_at(utc_monday(10, 0)), RateWindow::OffPeak);
    }

    #[test]
    fn weekends_and_holidays_are_off_peak_all_day() {
        let pricing = resolve_model_pricing("deepseek", "deepseek-flash").unwrap();
        let schedule = &pricing.off_peak.as_ref().unwrap().schedule;
        // Saturday 2026-10-24 09:00 Beijing.
        let saturday = utc_monday(1, 30).epoch_ms().unwrap() + 5 * 86_400_000;
        assert_eq!(
            schedule.window_at(RatingInstant::from_epoch_ms(saturday)),
            RateWindow::OffPeak
        );
        // Thursday 2026-10-01 09:00 Beijing is inside National Day.
        let national_day = utc_monday(1, 30).epoch_ms().unwrap() - 18 * 86_400_000;
        assert_eq!(
            schedule.window_at(RatingInstant::from_epoch_ms(national_day)),
            RateWindow::OffPeak
        );
    }

    #[test]
    fn unknown_instants_are_rated_at_peak() {
        let pricing = resolve_model_pricing("deepseek", "deepseek-flash").unwrap();
        let cost = pricing.calculate_cost(&usage(), RatingInstant::UNKNOWN);
        assert_eq!(cost.window, RateWindow::Peak);
        assert_eq!(cost.amount.0, 1_500_000_000);
    }

    #[test]
    fn cn_holiday_table_is_well_formed_and_current() {
        let schedule = deepseek_rate_schedule();
        let covered_until = schedule
            .holidays_covered_until()
            .expect("the Chinese holiday table must declare its coverage");
        assert!(
            covered_until >= CN_HOLIDAY_TABLE_REVIEW_DEADLINE,
            "refresh CN_2026_HOLIDAYS and CN_HOLIDAYS_COVERED_UNTIL from the State Council \
             notice for {}: the table currently stops at {covered_until:?}",
            CN_HOLIDAY_TABLE_REVIEW_DEADLINE.year + 1,
        );
        for range in schedule.holidays() {
            assert!(
                range.from <= range.through,
                "holiday range must not be reversed: {range:?}"
            );
            assert_eq!(range.from.year, CN_2026_HOLIDAY_YEAR);
            assert_eq!(range.through.year, CN_2026_HOLIDAY_YEAR);
            assert!((1..=12).contains(&range.from.month) && (1..=31).contains(&range.from.day));
            assert!(
                (1..=12).contains(&range.through.month) && (1..=31).contains(&range.through.day)
            );
        }
        // The notice's National Day span, transcribed as published.
        assert!(schedule.is_holiday(CivilDate::new(2026, 10, 1)));
        assert!(schedule.is_holiday(CivilDate::new(2026, 10, 7)));
        assert!(!schedule.is_holiday(CivilDate::new(2026, 10, 8)));
        // Spring Festival 2026 spans into the third week of February.
        assert!(schedule.is_holiday(CivilDate::new(2026, 2, 15)));
        assert!(schedule.is_holiday(CivilDate::new(2026, 2, 23)));
        assert!(!schedule.is_holiday(CivilDate::new(2026, 2, 24)));
    }

    #[test]
    fn unknown_deepseek_ids_do_not_inherit_a_neighbour_card() {
        assert!(resolve_model_pricing("deepseek", "deepseek-v9-ultra").is_none());
        assert!(resolve_model_pricing("ds-personal", "deepseek-v4-lite").is_none());
        // A non-DeepSeek provider with an unrecognised id stays unknown too.
        assert!(resolve_model_pricing("some-relay", "mystery-model").is_none());
    }

    #[test]
    fn plan_included_lanes_price_at_zero() {
        for id in [
            "deepseek-v4.1-flash-free",
            "glm-5.3-flash-free",
            "nex-agi/nex-n2.5-pro-free",
        ] {
            let pricing = resolve_model_pricing("opencode-plan", id).unwrap();
            assert_eq!(
                pricing.calculate_cost(&usage(), utc_monday(1, 30)).amount,
                NanoUsd::ZERO,
                "{id} is plan-included"
            );
        }
    }

    #[test]
    fn legacy_generations_keep_their_single_window_cards() {
        let chat = resolve_model_pricing("deepseek", "deepseek-chat").unwrap();
        assert!(chat.off_peak.is_none());
        assert_eq!(chat.tiers[0].output_standard_nano_per_m, 280_000_000);
        let r1 = resolve_model_pricing("deepseek", "deepseek-reasoner").unwrap();
        assert_eq!(r1.tiers[0].output_standard_nano_per_m, 2_190_000_000);
    }

    #[test]
    fn gemini_4_argon_pricing_and_implicit_caching() {
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
        let cost = pricing.calculate_cost(&usage, RatingInstant::UNKNOWN);

        assert_eq!(cost.input_cost.0, 200_000_000);
        assert_eq!(cost.cache_read_cost.0, 90_000_000);
        assert_eq!(cost.output_cost.0, 100_000_000);
        assert_eq!(cost.amount.0, 390_000_000);
        assert_eq!(cost.amount.format_usd(), "$0.39");
    }

    #[test]
    fn gemini_3_8_flash_swe_workhorse_pricing() {
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
        let cost = pricing.calculate_cost(&usage, RatingInstant::UNKNOWN);

        assert_eq!(cost.input_cost.0, 37_500_000);
        assert_eq!(cost.cache_read_cost.0, 11_250_000);
        assert_eq!(cost.output_cost.0, 7_500_000);
        assert_eq!(cost.amount.0, 56_250_000);
    }
}
