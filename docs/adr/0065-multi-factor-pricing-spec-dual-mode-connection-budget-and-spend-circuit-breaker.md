---
id: ADR-0065
title: "Multi-Factor Model Pricing Engine, Dual-Mode Connection Budget, and Local Spend Circuit Breaker"
status: accepted
date: 2026-11-05
scope: codec/nuo-model-codec, provider/nuo-provider, wire/nuo-wire, persistence/nuo-persistence, agent/nuo-agent, tui/nuo-tui
superseded_by: null
negative_knowledge: true
---

# 0065. Multi-Factor Model Pricing Engine, Dual-Mode Connection Budget, and Local Spend Circuit Breaker

- Status: Accepted
- Date: 2026-11-05
- Deciders: Nuo Architecture Working Group
- Consulted: Provider Integration, Agent Runtime, Persistence, and TUI Architecture Teams
- Informed: Core Engineering, Developer Experience
- Amends: [ADR-0015](0015-dependency-inversion-architecture-for-model-providers.md), [ADR-0036](0036-provider-quota-aggregation-dashboard-and-type-scoped-batch-inspection.md)

---

## Context and Problem Statement

Modern LLM providers utilize non-linear, multi-factor cost structures. Notable examples include:
- **Google Gemini (AI Studio)**: Frontier models such as **Gemini 4 Argon** ($2.00/1M input, $10.00/1M output, 95% off implicit cached input) and **Gemini 3.8 Flash** ($0.75/1M input, $3.75/1M output, 90% off cached input), along with legacy Gemini 1.5/2.5 tiered thresholds (context doubling over 128k/200k tokens), explicit context caching, and reasoning tokens.
- **Anthropic Claude**: Strict cache-creation write premiums (+25%) and cache-read discounts (90% off).
- **DeepSeek & OpenAI**: Differentiated prompt cache hits, reasoning tokens, and tier-specific completion multipliers.

Previously, Nuo only tracked raw token counts (`TokenUsage`, `RequestUsageRecord`, and daily partitions in `nuo-persistence`). This caused two architectural vulnerabilities:
1. **Financial Blindness & Runaway Risk**: Without an automated pricing engine and local spend cap, an agent entering an unexpected tool-loop or operating over large context could rapidly deplete developer credit balances or incur unexpected cloud billing charges.
2. **Quota Semantic Pollution**: Discussions previously risked overloading `Quota` to represent both upstream remaining balance/rate-limits and local financial costs/limits, violating the domain boundaries established in [ADR-0036](0036-provider-quota-aggregation-dashboard-and-type-scoped-batch-inspection.md).

We require a modern, zero-compromise architecture that establishes a multi-factor pricing engine, incorporates budget policies into connection lifecycles, and implements a pre-flight zero-outflow local spend circuit breaker.

---

## Decision Drivers

1. **Strict Domain Purity (`[INV-PRICING-01]`)**: Unambiguously segregate static model pricing (`PricingSpec`) and local financial accounting (`Spend` / `Cost`) from upstream allowances (`Quota`).
2. **Exact Financial Precision (`[INV-PRICING-02]`)**: Eliminate IEEE-754 floating-point drift by utilizing fixed-point micro/nano unit representations for all cost evaluations.
3. **Multi-Factor & Tiered Fidelity (`[INV-PRICING-03]`)**: Support context-length thresholds (e.g., Gemini 128k boundary), cache-read discounts, cache-write overheads, and reasoning tokens.
4. **Dual-Mode Budget Governance (`[INV-BUDGET-01]`)**: Distinguish between remote-enforced providers (relying gracefully on upstream 402/429 errors without false local interruptions) and client-enforced local budgets (for pay-as-you-go / credit card keys or explicit safety caps).
5. **Zero-Outflow Safety Interception (`[INV-BREAKER-01]`)**: Intercept requests *pre-flight* before bytes hit the wire once a local spend budget is reached.

---

## Architectural Decisions

### 1. Domain Tri-Bifurcation: Quota vs. Pricing vs. Spend vs. Budget

Following the domain purity established in ADR-0036, the financial subsystem is strictly partitioned:

* **`Quota` (Upstream State / Allowance)**:
  - Nature: External, dynamic runtime state fetched from upstream APIs (`/quota`, `ProviderUsage`, `BalanceQuota`).
  - Responsibility: Answering "How much credit or rolling capacity remains on the upstream server?".
* **`Pricing` (Static Catalog Specification)**:
  - Nature: Static or user-overridden rate-card declaration (`ModelPricing`, `PricingTier`).
  - Responsibility: Answering "What is the per-factor unit price (input, output, cache-read, cache-write) for this model?".
* **`Spend / Cost` (Accounting Ledger)**:
  - Nature: Calculated monetary consumption attached to usage records (`RequestCost`, `SessionSpend`).
  - Responsibility: Answering "Exactly how much money did this request or day consume?".
* **`Budget / CircuitBreaker` (Client-Side Defense Guardrail)**:
  - Nature: Connection-scoped configuration and local execution gate (`ConnectionBudget`, `SpendCircuitBreaker`).
  - Responsibility: Halting autonomous execution *before* incurring unwanted cost.

### 2. Multi-Factor Fixed-Point Pricing Model (`nuo-model-codec`)

We introduce high-precision pricing representations:

```rust
/// Nanodollars (1 USD = 1,000,000,000 nano-USD) per 1M tokens to prevent precision loss.
pub type NanoUsdPerMillion = u64;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PricingTier {
    /// Context window upper bound in tokens for this pricing tier.
    /// `None` indicates the default/unbounded tier.
    pub context_threshold: Option<u32>,
    /// Standard uncached input cost per 1M tokens.
    pub input_standard: NanoUsdPerMillion,
    /// Prompt cache read cost per 1M tokens.
    pub cache_read: NanoUsdPerMillion,
    /// Prompt cache creation/write cost per 1M tokens.
    pub cache_write: NanoUsdPerMillion,
    /// Output/completion cost per 1M tokens.
    pub output_standard: NanoUsdPerMillion,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelPricing {
    pub currency: String,
    pub tiers: Vec<PricingTier>,
}
```

The cost calculator accurately computes:
$$\text{Cost} = \frac{\text{uncached\_prompt} \times P_{\text{in}} + \text{cache\_read} \times P_{\text{cr}} + \text{cache\_write} \times P_{\text{cw}} + \text{completion} \times P_{\text{out}}}{10^6}$$

### 3. Dual-Mode Connection Budget Configuration (`nuo-persistence`)

Every `Connection` in `connections.toml` supports an explicit `budget` block configured at creation or edit time:

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum ConnectionBudget {
    /// Remote-managed: upstream controls rate-limits and balances; client lets requests proceed
    /// until upstream returns an authentic refusal (402, 429).
    #[default]
    Remote,
    /// Local-enforced: client tracks accurate cumulative spend and halts execution when limit is reached.
    Local {
        /// Budget ceiling (e.g., 2.50 USD).
        limit: f64,
        /// Scope of the counter.
        #[serde(default)]
        period: BudgetPeriod,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum BudgetPeriod {
    /// Cap applies to the active interactive session.
    #[default]
    Session,
    /// Cap applies to the local calendar day across all sessions.
    Daily,
    /// Cap applies to total cumulative spend recorded on this connection.
    Total,
}
```

### 4. Zero-Outflow Spend Circuit Breaker (`nuo-agent`)

The execution loop enforces a two-phase check:
1. **Pre-flight Gate**:
   - Before executing an LLM generation, the loop queries the active connection's budget.
   - If `mode == Local` and `accumulated_spend >= limit`, the loop refuses to emit the request and returns a structured `BudgetExceeded` error immediately.
2. **Post-flight Settlement**:
   - Upon receiving the terminal response and authoritative `TokenUsage`, the precise cost is evaluated using the model's `PricingTier`.
   - The cost is atomically appended to the session ledger and persistent day file.
   - If the new total meets or exceeds the ceiling, the circuit breaker trips, locking subsequent turns until explicit user override.

---

## Consequences & System Invariants

### Positive Consequences
- **Predictable Financial Safety**: Users can safely run autonomous loops on pay-as-you-go API keys (Gemini, OpenAI, Anthropic) without risk of unbounded expense.
- **Micro-Precision Accounting**: Eliminates billing drift and displays genuine dollar amounts in `/usage` and live status indicators.
- **Respect for Upstream Authority**: Pre-paid providers with remote accounting (DeepSeek, Antigravity) are not prematurely throttled by speculative client limits.

### Negative Consequences
- Baseline pricing tables must be maintained as provider tariffs evolve (mitigated by allowing user overrides in `connections.toml`).

### Binding Invariants
- `[INV-PRICING-01] Domain Purity`: Pricing models and local spend ledgers must never be placed inside the `Quota` type hierarchy.
- `[INV-PRICING-02] Financial Precision`: All cost computations must utilize integer/fixed-point nanos or micros. No floating-point accumulation allowed in storage.
- `[INV-PRICING-03] Tiered Context Fidelity`: Pricing resolution must inspect request input token count to select the applicable `context_threshold` tier.
- `[INV-BUDGET-01] Dual-Mode Governance`: Connection budgets must explicitly delineate between `Remote` and `Local` enforcement.
- `[INV-BREAKER-01] Zero-Outflow Guardrail`: Local budget limits must trip and halt execution before dispatching upstream requests.

---

## Rejected Alternatives & Negative Knowledge

### 1. Merging Pricing into `QuotaPort` / `ProviderQuotaData`
- **Discarded Option**: Extending `ProviderQuotaData` with price per million tokens.
- **Failure Mode**: Violated ADR-0036 domain boundaries. Providers like Gemini AI Studio or OpenAI API-key routes have zero quota endpoints. Coupling pricing to quota queries prevented key-based providers from benefiting from cost tracking.

### 2. Standard IEEE-754 `f64` Accumulation
- **Discarded Option**: Tracking cost as `f64` in dollars directly.
- **Failure Mode**: Successive addition of small turn costs ($0.00012) produces rounding inaccuracies and inconsistent equality checks when matching budget ceilings.

### 3. Pure Speculative Pre-flight Clamping for Remote Providers
- **Discarded Option**: Enforcing hard local spend limits on all connections regardless of mode.
- **Failure Mode**: Caused premature local failures on pre-paid enterprise accounts with ample remote balances due to small model-pricing differences between declared baselines and enterprise custom discounts.
