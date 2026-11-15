---
id: ADR-0200
title: "The Model Bar Shows Session Spend"
status: accepted
date: 2026-11-15
scope: tui/model-bar, protocol/token-ledger, telemetry
superseded_by: null
supersedes: []
negative_knowledge: true
---

# 0200. The Model Bar Shows Session Spend

- Status: Accepted
- Date: 2026-11-15

## Context and Problem Statement

The token ledger already prices every settled attempt into `cost_nano_usd`,
and `/usage` renders totals, but the live model bar showed only context tokens
and a percentage. A user watching a long agent run had no ambient cost signal;
the cheapest way to discover spend was to open a report. The data existed; the
projection did not.

## Architectural Decisions

### 1. The performance snapshot carries the priced attempt

`TurnPerformanceSnapshot` gains `prompt_tokens`, `cache_read_tokens`,
`cache_write_tokens`, and `cost_nano_usd`. `RequestAccountingGuard` computes
the price with `pricing::resolve_model_pricing(provider, model)` and the
attempt's own `started_at_epoch_ms`, so the live snapshot and the durable
ledger use the same rate window. A route without a published price reports
`None`, never a fabricated zero.

### 2. The session chrome accumulates spend

`SessionChrome::session_cost_nano_usd` adds each settled attempt's cost. It is
session-scoped like the context snapshot, so a background session's spend does
not paint onto the viewed session's bar. A retry is a separate attempt and is
counted separately, which is what the ledger does.

### 3. The context segment renders spend after the percentage

`context_usage_spans` appends ` · $0.0042` when a positive cost exists. The
cost inherits the context segment's truncation behavior: a narrow bar drops
the whole segment rather than showing a clipped price. The context keycap
opens Thread Stats, which already has the detailed breakdown.

### 4. Formatting is owned by `NanoUsd`

The bar calls `NanoUsd::format_usd`; it does not reimplement decimal
thresholds. Price rendering stays consistent with `/usage` and model details.

## Consequences

- **Positive**: ambient spend is visible without opening a report; the
  snapshot is useful to any other client, not just the TUI.
- **Negative & Incurred Costs**: one pricing lookup per settled attempt (a
  small map lookup). A session whose route changes mid-run accumulates costs
  from each attempt's own model, which is correct but not a single-rate
  estimate.
- **Neutral**: a zero-cost/plan-included route shows no cost segment; the
  context percentage remains.

## Discarded Alternatives (Negative Knowledge)

- **Re-price in the TUI from the model label**: duplicates tier/off-peak
  logic and can disagree with the ledger.
- **Show only the last turn's cost**: a long session's spend disappears after
  the next turn.
- **Fetch `/usage` on every turn**: a disk-backed cross-session report is the
  wrong cadence for a per-session bar.

## Links

- [ADR-0065](0065-multi-factor-pricing-spec-dual-mode-connection-budget-and-spend-circuit-breaker.md)
- [ADR-0080](0080-time-varying-rate-cards-and-off-peak-rating.md)
