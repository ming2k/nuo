---
id: ADR-0214
title: "Context Thresholds Follow the Model Window and Are Not User Configuration"
status: accepted
date: 2026-11-18
scope: cognition/context-budget, persistence/config
superseded_by: null
supersedes:
  - "[INV-CONTEXT-01] of ADR-0153 (effective input ceiling `max_input_tokens`)"
  - "the user-facing `[context]` policy of ADR-0182"
negative_knowledge: true
---

# 0214. Context Thresholds Follow the Model Window and Are Not User Configuration

- **Status**: Accepted
- **Date**: 2026-11-18
- **Amends**: [ADR-0153](0153-effective-input-ceiling-tool-output-retention-and-ingest-image-normalization.md)
  (the effective input ceiling is removed; retention invariants `INV-CONTEXT-02..05` stand),
  [ADR-0182](0182-context-policy-is-configuration-and-phase-a-substrate-is-deleted.md)
  (context policy is no longer configuration),
  [ADR-0031](0031-domain-separated-configuration-matrix-and-legacy-retirement.md)
  (`agent.toml` no longer owns a `[context]` table)

## Context and Problem Statement

`agent.toml [context]` exposed eight knobs (schema version, three watermark
basis points, preferred rounds, two enable flags, a fallback window, an input
ceiling, an observation protect budget) plus a validator, an offline legacy
converter, and a fail-fast rejection path for older `compaction.*` keys.

None of them is a decision a user can make better than the runtime:

- `max_input_tokens = 128000` capped every model's window. A 1M-token model
  compacted at ~109k tokens (85% of 128k) while the status bar — which reads
  the declared window — showed ~11% used. Because `Config::save_domain_files`
  writes the whole aggregate, the default was frozen into every user's file, so
  changing the code default would not have reached existing installs.
- The enable flags only disable the relief valves that keep long rounds from
  overflowing the provider window.
- Watermark ratios, protect budgets, and fallback windows are tuning constants
  of the compaction algorithm, not preferences.

## Decision

1. **Thresholds derive from the active model's declared context window only.**
   `protocol::CompactionPolicy` is the single internal policy (prune 65%,
   compact 85%, target 25%, fallback window 32k for unknown models). It carries
   no serde derives and is resolved through
   `ContextProjectionSettings::for_window(window)`.
2. **No user-facing context configuration.** `ContextPolicy`,
   `WatermarkPolicy`, `CONTEXT_POLICY_SCHEMA_VERSION`, `DEFAULT_MAX_INPUT_TOKENS`
   and the offline `migration_policy` converter are deleted. Pruning and the
   mid-turn projection gate are always installed.
3. **Old files keep loading.** `agent.toml [context]` is accepted, logged as
   retired, ignored, and dropped on the next save. Legacy monolithic
   `compaction*` keys are ignored instead of aborting startup.

## Consequences

- A model's advertised window is the budget: a 1M model compacts near 850k.
  Prefill latency on very large windows is the provider's/user's model choice,
  not a hidden cap.
- The status bar and the compaction gate now read the same window.
- Tests that need different thresholds construct a `CompactionPolicy` value
  directly and call `ContextProjectionSettings::from_policy`.

## Negative Knowledge

Do not reintroduce a global input ceiling or user-tunable watermarks. A cap
that is not visible in the UI silently shrinks every model, and any value
written by `save_domain_files` becomes permanent for that install.
