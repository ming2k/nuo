---
id: ADR-0205
title: "Bounded Reads Start Before the Stream Ends"
status: accepted
date: 2026-11-15
scope: engine/tools, streaming, performance
superseded_by: null
supersedes: []
negative_knowledge: true
---

# 0205. Bounded Reads Start Before the Stream Ends

- Status: Accepted
- Date: 2026-11-15

## Context and Problem Statement

The harness waited for the entire provider stream, validated it, and only then
dispatched every tool call. A multi-call turn therefore paid the full stream
duration before the first read even started. OpenCode starts each tool as its
call arrives, but that model also accepts executing mutating calls from a
response that may later be discarded.

Nuo's finalization gate exists for a reason: a truncated stream is not
executable intent, and a retry must not duplicate side effects. The performance
win and the correctness gate only conflict for tools that mutate state.

## Architectural Decisions

### 1. Only bounded built-in reads are eligible

`Agent::early_tool_eligible` admits `read`, `find_files`, `search_text`,
`code_query`, and `recall_memory` when the call is complete JSON, has an id and
name, declares `RiskProfile::ReadOnly`, has no user prompt, spawns no subagent,
does not affect control flow, and has an `Unspecified` scope target.

A custom or dynamic tool that merely declares `ReadOnly` is not assumed
bounded: an arbitrary plugin read can await a gate forever. The capability must
also be a shipped `BuiltinTool`, which makes the set explicit and testable.

### 2. Eligibility also respects the pre-dispatch guards

The early path consults a non-mutating `GuardRegistry::would_block` preview plus
the exact blocked-signature mask and checkpoint-replay scan. It never advances
the trajectory window; preflight remains the single writer of guard state. A
call that preflight will block therefore never runs early.

### 3. Early results are provisional and side-effect-free

The stream attempt owns a `FuturesUnordered` of read futures. A completed result
is stored by call id. When the stream is valid, `dispatch_preflight` reuses it,
emits the normal `ToolResult`, and marks the call completed; when the attempt is
discarded, the map is dropped. Because only side-effect-free reads are eligible,
dropping the map cannot leave a partial mutation behind.

### 4. Preflight still owns announcement and ordering

`ToolCallStarted` is emitted during the stream as before. The authoritative
`ToolCall` and `ToolResult` events remain preflight/dispatch events, so a
blocked or replayed call still wins over an early execution. The scheduler is
untouched: early calls simply do not enter it.

### 5. Mutating tools still wait for validation

Writes, edits, shell commands, todo mutations, and subagents are not eligible.
Their side effects require a fully validated response; the existing
checkpoint-replay and guard gates stay in front of them.

## Consequences

- **Positive**: independent reads in a multi-call turn overlap the model's
  remaining stream and each other; a discarded response leaves no mutation.
- **Negative & Incurred Costs**: a read whose call is later blocked by the
  trajectory guard has already run (wasted work, never a wrong result); the
  read is retried on a stream retry. The eligibility list needs maintenance
  when a new bounded read capability ships.
- **Neutral**: the transcript, event order, and scheduler contract for normal
  calls are unchanged. Early results are consumed in input order at preflight.

## Discarded Alternatives (Negative Knowledge)

- **Start every tool at its call delta**: a truncated response could execute a
  write, and a retry could duplicate it. The correctness gate is not optional.
- **Start custom read-only tools**: a plugin read can block indefinitely; the
  post-stream drain would then hold the turn open.
- **Let preflight re-run early calls**: wastes the work and can double-count a
  non-idempotent observer. The result map is authoritative for eligible calls.

## Links

- [ADR-0166](0166-a-tool-call-has-one-identity-and-the-tool-step-reducer-is-idempotent.md)
- [ADR-0197](0197-permission-denial-is-an-observation.md)
