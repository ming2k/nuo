---
id: ADR-0080
title: "Time-Varying Rate Cards, Pure Dispatch-Anchored Cost Rating, and DeepSeek Off-Peak Support"
status: accepted
date: 2026-10-19
scope: provider/nuo-provider, codec/nuo-model-codec, protocol/nuo-protocol, harness/nuo-harness, agent/nuo-agent, providers/nuo-provider-deepseek
superseded_by: null
negative_knowledge: true
---

# 0080. Time-Varying Rate Cards, Pure Dispatch-Anchored Cost Rating, and DeepSeek Off-Peak Support

- Status: Accepted
- Date: 2026-10-19
- Deciders: Nuo Architecture Working Group
- Consulted: Provider Integration, Agent Runtime, Persistence, Ledger
- Informed: Core Engineering
- Amends: [ADR-0065](0065-multi-factor-pricing-spec-dual-mode-connection-budget-and-spend-circuit-breaker.md)
- Complements: [ADR-0068](0068-canonical-model-catalog-normalization-and-semantic-lifting.md)

---

## Context and Problem Statement

[ADR-0065](0065-multi-factor-pricing-spec-dual-mode-connection-budget-and-spend-circuit-breaker.md) established the multi-factor fixed-point pricing engine (`NanoUsd`, `PricingTier`, `ModelPricing`), which successfully bound context-window thresholds, prompt-cache write premiums, and cache-read discounts without IEEE-754 precision drift (`[INV-PRICING-02]`, `[INV-PRICING-03]`).

However, upstream provider pricing dynamics introduced a second orthogonal dimension: **time-varying rate cards**. Specifically, DeepSeek's official production pricing establishes a 50% off-peak discount across all token billing factors (cache hit, cache miss, completion) outside Beijing working hours (09:00–12:00, 14:00–18:00 Monday–Friday, excluding Chinese statutory holidays).

Prior to this decision, Nuo suffered from four concrete defects:
1. **Financial Miscalculation & Breaker Blindness**: All DeepSeek models fell back to a legacy single-window DeepSeek Chat (V3) card ($0.14/1M input, $0.28/1M output), understating DeepSeek V4 Pro peak completion cost ($3.96/1M) by 14.1×, rendering the `[INV-BREAKER-01]` local spend circuit breaker ineffective.
2. **Provider Baseline Stagnation**: DeepSeek V4.1 Flash's canonical model id (`deepseek-flash`) and native multimodal vision capabilities were not reflected in `nuo-provider-deepseek` seeds.
3. **Temporal Pollution & Ledger Drift**: Pricing had no concept of time, while naive solutions risked reading the system clock inside `calculate_cost`, destroying idempotent re-rating of historical sessions.
4. **Aggregate Re-pricing Bug in Pre-flight Guard**: Harness pre-flight session spend recomputed accumulated `round.state.token_usage` against the current model pricing, which, under time-varying schedules, retroactively applied the current window rate to tokens generated under past windows.

---

## Decision Drivers

1. **Strict Rating Purity (`[INV-PRICING-04]`)**: Cost rating must be an idempotent, side-effect-free pure function of `(TokenUsage, RatingInstant)`. No rating code may sample ambient system time or operator time zones.
2. **Dispatch Anchor Fidelity (`[INV-PRICING-05]`)**: An attempt's durable dispatch timestamp (`started_at_ms`) is recorded once at the actual provider-call boundary and serves as the sole temporal anchor for both ledger storage and rate-card selection.
3. **Orthogonal Rate-Card Separation**: Disentangle the rate card (unit prices) from the calendar schedule (which card applies when). Do not collapse time-of-day policies into model IDs (e.g. `deepseek-flash-offpeak`) or effort ladders (`[ADR-0068]`).
4. **Deterministic Calendar Math**: Reject heavyweight runtime time-zone database (`tzdata`) dependencies. Fixed offsets (e.g., Asia/Shanghai UTC+08:00) with civil Gregorian calendar arithmetic provide permanent determinism and WASM/offline portability.
5. **Conservative Fallback on Stale Metadata**: If a statutory holiday table reaches its covered limit or an attempt instant is unknown, the scheduler must strictly rate at standard peak prices, guaranteeing budget guardrails fail safe.

---

## Architectural Decisions

### 1. Separation of Rate Cards and Temporal Schedules

`PricingTier` remains a pure, non-temporal rate card (nanodollars per 1M tokens across 4 factor dimensions). Time variance is represented as an optional `OffPeakPricing` struct containing discounted tiers and a `RateSchedule`:

```rust
pub struct ModelPricing {
    pub currency: String,
    pub tiers: Vec<PricingTier>,
    pub off_peak: Option<OffPeakPricing>,
}

pub struct OffPeakPricing {
    pub tiers: Vec<PricingTier>,
    pub schedule: RateSchedule,
}
```

If `off_peak` is `None`, standard static rates apply across all instants (preserving zero overhead for 20+ other providers).

### 2. Rating Instant and Pure Calculation Contract

All cost calculation signatures explicitly require a `RatingInstant`:

```rust
impl ModelPricing {
    pub fn calculate_cost(&self, usage: &TokenUsage, instant: RatingInstant) -> TurnCost;
    pub fn calculate_breakdown_cost(&self, breakdown: &TokenBreakdown, instant: RatingInstant) -> TurnCost;
}
```

- When an instant is `RatingInstant::UNKNOWN`, the engine unconditionally defaults to the peak/base card.
- Call sites in the harness and agent runtimes obtain the instant from the attempt's dispatch boundary (`started_at_ms`), never during post-hoc formatting.
- `TurnCost` includes the resolved `window: RateWindow` for verifiable auditability in telemetry and persistence records.

### 3. Chinese Statutory Holiday Notice Integration & Staleness Tripwire

For Asia/Shanghai (`anchor_offset_minutes = 480`), the official holiday schedule from the State Council of the PRC is transcribed verbatim (`CN_2026_HOLIDAYS`).
- Statutory holidays suspend peak windows for the entire day.
- A test assertion `cn_holiday_table_is_well_formed_and_current` enforces that `holidays_covered_until >= CN_HOLIDAY_TABLE_REVIEW_DEADLINE`. When the calendar year ends, CI trips until the next year's notice is appended.
- Past the covered date, unknown days safely evaluate as normal weekdays (peak), preventing budget leakage.

### 4. Direct Spend Accumulation in Pre-flight Circuit Breaker

`RoundState.spend: NanoUsd` accumulates settled turn costs monotonically. The pre-flight session budget check inspects `round.state.spend` rather than recalculating aggregated token totals, ensuring past turns billed under off-peak rates are never retrospectively re-evaluated at peak rates.

### 5. DeepSeek Baseline Model Harmonization

- `deepseek-v4.1-flash` is established with native multimodal `vision: true` and 1M context window.
- Legacy wire ids (`deepseek-v4-flash`, `deepseek-v4-flash-vision-exp`) remain actively supported and registered as baselines for full backward compatibility across relays (`opencode-plan`, `commandcode-plan`).
- Free plan-included lanes (`*-free`) explicitly resolve to `ModelPricing::zero()`.

### 6. Informational Disclosure Discipline: Modal Inspection Over Bottom-Bar Clutter

Rate cards, cache-read discounts, and off-peak schedules represent high-context reference information. Injecting ephemeral cost badges and status glyphs (such as `☾ off-peak`) into the bottom model bar was rejected to preserve the model bar as a lean, distraction-free operational indicator (context token count and active model identity only).

Instead, comprehensive pricing transparency is anchored strictly in on-demand inspection modals:
- **Model Details (`/models` -> `i`)**: Displays standard input/output rates, prompt-cache hit discounts, off-peak rates, human-readable window hours, and live window status.
- **Connection Details (`/connections` -> `Enter` or `@connection`)**: Co-locates the active model's billing policy alongside the provider's live balance (`ProviderUsage::primary_balance`), forming a unified view of account funds and unit costs.

---

## Normative Invariants Established

- **`[INV-PRICING-04]` Rating Purity**: Cost evaluation is a pure function of `(TokenUsage, RatingInstant)`. No pricing code may read system clocks, process environments, or operator time zones.
- **`[INV-PRICING-05]` Dispatch Anchor Fidelity**: An attempt's durable start timestamp (`started_at_ms`) is stamped once at the provider dispatch boundary and is the unique temporal anchor for both cost rating and ledger persistence.

---

## Rejected Alternatives & Negative Knowledge

### 1. Encoding Rate Windows in Model Identifiers
- **Discarded Option**: Minting pseudo-model ids such as `deepseek-flash@offpeak` or `deepseek-flash:peak`.
- **Failure Mode**: Violated ADR-0068. Collapsing ephemeral temporal state into model identity breaks session resumes, picker hierarchies, effort ladders, and cache deduplication keys.

### 2. Runtime IANA Time-Zone Database (`chrono-tz`)
- **Discarded Option**: Linking dynamic `tzdata` crates to resolve time zones.
- **Failure Mode**: Asia/Shanghai is permanently UTC+08:00 without daylight saving. Dynamic tzdata crates introduce build bloat, disk I/O, potential WASM incompatibility, and non-deterministic historical rating drift across tzdata releases.

### 3. Automatic Makeup Workday (调休补班) Calendar
- **Discarded Option**: Modeling Chinese weekend makeup workdays to treat working Saturdays/Sundays as peak hours.
- **Failure Mode**: Upstream DeepSeek terms explicitly state peak hours as "周一至周五（不含中国法定节假日）" (Monday through Friday, excluding statutory holidays). Saturday is unconditionally off-peak; adding makeup workday logic would have caused erroneous overcharges contrary to upstream billing.

### 4. Bottom-Bar Ephemeral Cost and Rate-Window Badges
- **Discarded Option**: Rendering persistent spend counters (e.g. `· $0.042`) and status glyphs (e.g. `☾ off-peak`) on the composer bottom model bar.
- **Failure Mode**: Visual noise and horizontal column contention on constrained terminal viewports. The model bar's role is immediate operational state (context token usage and model identity). Complex time-window schedules and unit rates belong in on-demand inspection surfaces (`/models` detail and `/connections` detail).

---

## Consequences

- **Positive**: Exact, audit-clean cost calculation for DeepSeek V4.1 Flash and Pro in both peak and off-peak windows; budget circuit breaker reliably protects developer accounts.
- **Positive**: Complete purity and idempotency in historical token ledger replays.
- **Negative & Trade-offs**: Chinese statutory holiday tables require an annual review notice update, guarded by an automated staleness test.
