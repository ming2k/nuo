---
id: ADR-0204
title: "Provider Cache and Output Budgets Respect the Window"
status: accepted
date: 2026-11-15
scope: providers/cache, providers/anthropic, context
superseded_by: null
supersedes: []
negative_knowledge: true
---

# 0204. Provider Cache and Output Budgets Respect the Window

- Status: Accepted
- Date: 2026-11-15

## Context and Problem Statement

Two route facts were declared but not consumed. A prompt-cache capability can
publish `min_cacheable_tokens`, but the resolved cache policy dropped it, so a
short prompt paid a cache-write control the upstream silently ignored. And the
Anthropic encoder sent the model's full configured `max_tokens` regardless of
how much room the prompt left: on a nearly full window, the request asks for
more completion than the context can hold and the upstream rejects it instead
of truncating.

## Architectural Decisions

### 1. The resolved cache policy carries the route floor

`ResolvedCachePolicy::Enabled` gains `min_cacheable_tokens`. The resolver
copies the declared value; every provider sees the same floor.

### 2. Anthropic omits cache controls below the floor

The encoder estimates the cacheable prompt from actual content (system text,
tool schemas, message content), not the JSON envelope. If the estimate is below
the declared floor, no `cache_control` is emitted. The estimate is deliberately
content-based so JSON punctuation cannot push a small prompt over a real
provider threshold.

### 3. Anthropic fits output to the remaining window

For non-thinking requests, the encoder computes `room = context_window −
estimated_prompt − 1024`, floors `max_tokens` at 1024, and caps it at that
room. Thinking requests keep their configured budget: the thinking block is
part of the same completion, and a blind clamp would silently disable it.

### 4. The rule is provider-side, not request-assembly-side

The encoder is the only layer that knows the concrete wire limit and the
thinking mode together. Request assembly continues to carry the model's
declared output ceiling; each provider lowers it against its own capabilities.

## Consequences

- **Positive**: no cache write is paid below the provider floor; a near-full
  conversation no longer fails on an oversized output request.
- **Negative & Incurred Costs**: the prompt estimate is approximate (chars/4),
  so the encoder may leave a small amount of unused room. The 1024-token floor
  is a deliberate minimum for a usable answer.
- **Neutral**: providers without a declared floor or context window keep their
  existing behavior.

## Discarded Alternatives (Negative Knowledge)

- **Clamp every provider in request assembly**: the thinking budget and wire
  limit are provider-specific; a shared clamp would need a capability matrix
  and would still be wrong for native-compaction routes.
- **Always send the full catalog output limit**: leaves overflow to the
  upstream and turns a preventable rejection into a retry.
- **Estimate the prompt from serialized JSON**: tool-schema punctuation
  inflates the count and suppresses caching for prompts that should cache.

## Links

- [ADR-0065](0065-multi-factor-pricing-spec-dual-mode-connection-budget-and-spend-circuit-breaker.md)
- [ADR-0153](0153-effective-input-ceiling-tool-output-retention-and-ingest-image-normalization.md)
