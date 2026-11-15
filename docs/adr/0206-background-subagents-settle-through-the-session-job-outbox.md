---
id: ADR-0206
title: "Background Subagents Settle Through the Session Job Outbox"
status: accepted
date: 2026-11-15
scope: engine/subagents, server/background-jobs, harness
superseded_by: null
supersedes: []
negative_knowledge: true
---

# 0206. Background Subagents Settle Through the Session Job Outbox

- Status: Accepted
- Date: 2026-11-15

## Context and Problem Statement

`spawn_agent` previously rejected `background: true` and forced every subagent
to block the calling turn until completion. That prevented parallel research
subagents from running alongside the parent round or across turns, while a naive
fire-and-forget `tokio::spawn` would lose completion receipts, fail to surface
in `/jobs`, and drop token accounting on failure or cancellation.

At the same time, interrupted or failed foreground subagents reset `token_usage`
to `TokenUsage::default()`, erasing the cost of the most expensive failure modes,
and `Agent::new` unconditionally installed agent-owned `todo` tools into child
agents whose profile did not admit them.

## Architectural Decisions

### 1. SubagentTool delegates background execution through a host seam

`nuo-engine` defines `SubagentBackgroundDispatcher`, which `nuo-server` binds
at composition time. When no dispatcher is bound (such as unit tests or bare
embedded harnesses), `spawn_agent` continues to hide and reject `background`.
When bound, `spawn_agent` advertises `background: boolean` and returns a JSON
handle (`job_id`, `status: "running"`, `description`, `role`) immediately.

### 2. Background subagents share BackgroundJobManager and PendingOutcome

`JobSpec::Subagent { description, role, label }` is registered in
`BackgroundJobManager` under the parent session's `owner_session`. It supports
cancellation via `/jobs kill`, retains its settled `BackgroundJobOutcome` in the
bounded `PendingOutcome` queue, writes its transcript/usage summary to disk logs,
and emits `BackgroundJobEvent::Completed`.

### 3. ThreadDriver delivers completed subagents into FollowUpQueue

`ThreadDriver` subscribes to `BackgroundJobManager` events and claims settled
`JobSpec::Subagent` outcomes for its `session_id` both when idle and at
round-boundary wakes. Each claimed outcome is formatted as a `<subagent_result>`
follow-up message and shipped through `FollowUpQueue` so the parent session
never loses a completion receipt even if the subagent finished mid-turn.

### 4. Child failure/interrupt settlement preserves accumulated token usage

`drive_child_stream` captures `StreamingRoundState::token_usage()` on every exit
path (success, `Interrupted`, and `HarnessError`) so interrupted and failed
subagents report their actual accumulated prompt/completion/cache tokens.

### 5. Subagents only receive `todo` when their profile admits it

`AgentBuilder::without_agent_owned_tools()` skips installing `todo` for child
profiles whose `ToolPolicy::allowed_tools` does not include `BuiltinTool::Todo`.

## Consequences

- **Positive**: Multiple `explore`/`debug`/`skill` subagents can run in
  parallel in the background, appear in `/jobs`, be cancelled cleanly, and wake
  the parent session with structured findings; failed/interrupted subagents
  account real token usage; child toolsets no longer leak `todo`.
- **Negative & Incurred Costs**: Background subagent outcomes are retained in
  the in-process `BackgroundJobManager` outbox and `task_ledger`; a process
  restart while a background subagent is in flight still marks it interrupted
  rather than replaying the child LLM loop.
- **Neutral**: Foreground `spawn_agent` calls keep their existing `ToolOutput::Subagent`
  streaming transcript and duplex human-channel routing unchanged.

## Discarded Alternatives (Negative Knowledge)

- **Fire-and-forget `tokio::spawn` inside `SubagentTool`**: loses `/jobs`
  visibility, cannot be cancelled by job ID, and drops results if the parent
  turn is busy or idle.
- **Polling-only subagent tool**: forces the parent model to burn turns
  sleeping/polling instead of receiving an automatic follow-up receipt.

## Links

- [ADR-0205](0205-bounded-reads-start-before-the-stream-ends.md)
