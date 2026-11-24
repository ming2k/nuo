---
id: ADR-0211
title: "Code Mode VM Is Deferred Behind Measured Tool Economics"
status: rejected
date: 2026-11-17
scope: tools, cognition, prompt-budget
superseded_by: null
supersedes: []
negative_knowledge: true
---

# 0211. Code Mode VM Is Deferred Behind Measured Tool Economics

- Status: Rejected (deferred) — no Code Mode VM is built; the revisit criteria below are the bar for reopening the proposal.
- Date: 2026-11-17

## Context and Problem Statement

opencode and pi both expose a Code Mode: instead of declaring every tool as a
native function, they declare one `execute(code)` tool plus a compressed tool
catalog. The model writes JavaScript that calls tools, loops, filters, and
aggregates inside a sandbox; only the final result returns to the conversation.

The benefits are real but conditional:

- **Schema/token savings**: N tools × their JSON Schemas per request versus one
  execute tool plus a catalog.
- **Turn savings**: one script can replace many ReAct rounds when the task needs
  loops, pagination, retries, or aggregation.
- **Data reduction**: the script can summarize a large tool result before it
  enters the transcript.
- **Deterministic composition**: parallel calls and retries do not consume
  reasoning tokens.

The costs are equally real: a JS runtime, worker pool, memory/CPU limits,
sandbox escape risk, nested tool-call plumbing, cancellation propagation,
grammar-constrained generation, error surfaces, and a second implementation of
every tool's permission/approval semantics.

## Decision

**Nuo does not introduce a JavaScript Code Mode VM now.** The current tool
surface is:

- 13 native built-ins, each with a compiled schema and request snapshot;
- `tool_search` activation for deferred MCP tools;
- a stable direct prefix for prompt-cache stability;
- typed `Structured`/`Media` results that already carry machine-readable data.

The catalog problem that Code Mode primarily solves is addressed by deferred
exposure plus `tool_search`. The remaining benefit would be composition, and it
is not yet justified by measured demand.

### Revisit criteria

Re-evaluate when **all** of the following hold for a sustained period:

1. A typical request declares more than ~30 tools (or a catalog that accounts
   for >20% of prompt tokens).
2. The median task uses more than ~8 tool calls, or a significant share of tasks
   are loops/pagination/aggregation.
3. Prompt/transcript token telemetry shows schema and repeated tool history as a
   top cost, not just a theoretical one.
4. The team can own a sandboxed runtime with hard CPU, memory, and tool-call
   budgets, and route nested calls through the same `ToolSnapshot`, permission
   chain, journal, and preview.

### If adopted

- Run the VM in a separate process (QuickJS/WASM), never in the agent process.
- Expose only the request snapshot's callable tools; no direct filesystem or
  network access.
- Nested calls go through the same permission chain and journal.
- Hard limits: wall clock, CPU, memory, tool calls, output bytes.
- Progress and cancellation propagate to the outer tool call.
- Large outputs go to artifacts, with a path returned to the model.
- Prefer grammar-constrained generation for the script.

### Middle ground

Before a VM, a Rust-native batch tool (a bounded array of tool calls with
parallel execution and result aggregation) can capture the parallel/read-many
benefit at a fraction of the complexity. It does not provide loops or
conditionals, but those are exactly the cases to measure before paying for a
runtime.

## Consequences

- **Positive**: the tool layer stays one Rust contract; permissions, journal,
  snapshots, and cancellation have one implementation; no new code-execution
  attack surface enters the server.
- **Negative & Incurred Costs**: very large tool catalogs and loop-heavy tasks
  pay more turns than a mature Code Mode would; the model must use multiple tool
  calls rather than one script.
- **Neutral**: `tool_search` and deferred exposure already capture the catalog
  portion of the benefit and remain compatible with a future VM.

## Discarded Alternatives

- **Adopt Code Mode because peers have it**: complexity is not a feature; the
  benefit must be measured against Nuo's actual catalog and task mix.
- **Implement a JS interpreter in-process**: unacceptable security and
  resource-isolation surface for a multi-tenant session server.
- **Invent a bespoke DSL now**: models have far more training exposure to
  JavaScript/TypeScript; a custom DSL would add a learning/error tax without
  removing the sandbox requirement.
