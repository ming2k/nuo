---
id: ADR-0019
title: "Token-Pressure Measurement, Budget Accounting, and Window Headroom Calculation"
status: accepted
date: 2026-10-04
scope: protocol/wire, harness/pressure, model/context
superseded_by: null
negative_knowledge: true
---

# 0019. Token-Pressure Measurement, Budget Accounting, and Window Headroom Calculation

- Status: Accepted
- Date: 2026-10-04
- Deciders: Nuo Architecture Working Group
- Consulted: Wire, Model-Codec, and Harness maintainers
- Informed: System Architects
- Complements: [ADR-0005](0005-unified-binary-and-concentric-runtime-architecture.md), [ADR-0006](0006-wire-contract-consolidation.md), [ADR-0014](0014-model-provider-invocation-schemes-oauth-subscription-lane-and-api-key-byok-lane.md)

---

## Context and Problem Statement

Large Language Models operate within strict context window ceilings. In a cognitive ReAct loop, uncontrolled context expansion causes silent overflow errors, degraded attention, or sudden request rejection.

The system needed a deterministic foundation to measure token pressure, calculate dynamic budget headroom, and trigger context management (such as tool output pruning or compaction) before hitting hard provider limits.

## Decision Outcome

1. **Dual-Layer Accounting (`[INV-PRESS-01]`)**: Stratify token accounting into two distinct metrics:
   - *Diagnostic/Local Pressure*: Fast BPE-estimated token counting used for real-time telemetry, visual pressure indicators, and proactive pruning triggers.
   - *Authoritative Usage*: Exact provider-reported usage returned in completion metadata, stored in the persistent token ledger for billing and audit accuracy.
2. **Dynamic Headroom & Safety Reserve**: Request admission must reserve a framing and generation margin ($F$) subtracted from the target model's maximum window size before admitting new tool outputs.
3. **Threshold-Gated Intervention**: Compaction and output truncation are gated by calibrated watermark ratios (low/high hysteresis bands) to prevent oscillating prune cycles on borderline requests.

## Invariants & Behavioral Boundaries

- **`[INV-PRESS-01] Metric Decoupling`**: Local heuristic token counts must never overwrite provider-reported usage facts in the token ledger.
- **`[INV-PRESS-02] Proactive Boundary Guard`**: Requests approaching the high watermark must execute pruning or compaction before outbound network dispatch rather than failing on provider rejection.

## Rejected Alternatives & Negative Knowledge

### 1. Pure Byte-Count Heuristics
- **Why considered**: Zero tokenizer dependency and negligible computational cost.
- **Why rejected**: Character and byte lengths correlate poorly with BPE tokenization across languages and structured code payloads, causing unpredictable overflow.

### 2. Synchronous Remote Token-Count Probing
- **Why considered**: Perfectly matches remote model token calculations.
- **Why rejected**: Added round-trip latency to every turn preparation and was unsupported by several provider endpoints.
