---
id: ADR-0197
title: "Permission Denial Is an Observation, Not a Round Terminator"
status: accepted
date: 2026-11-15
scope: engine/dispatch, permissions, agent-loop
superseded_by: null
supersedes: []
negative_knowledge: true
---

# 0197. Permission Denial Is an Observation, Not a Round Terminator

- Status: Accepted
- Date: 2026-11-15

## Context and Problem Statement

`dispatch_finalize` treated any `ToolOutput::PermissionDenied` in a batch as a
signal to end the round: `Ok(!denied)`. The model received the denial as a
tool result but never got another turn to act on it. A user who rejected one
write because the path was wrong had to retype the whole request; the model
could not choose a different path, a different tool, or a smaller edit. The
round looked "stopped by the user" even when the user only rejected one call.

A denial is information. The harness already returns it to the model as a
typed observation. Ending the loop on top of that observation is a second,
contradictory policy.

## Architectural Decisions

### 1. Denial never changes the dispatch outcome

`dispatch_tool_calls` records every result, runs the post-tool hooks, and
returns `Ok(true)` regardless of `PermissionDenied`. The normal round-exit
rules apply: no tool calls ends the round; a tool call opens the next turn.

### 2. The model sees the denial and chooses again

`ToolOutput::PermissionDenied` stays distinct from `Error`; its text names the
tool and the refusal. A model that cannot proceed can say so, but it must do
so in its own next message rather than having the harness infer it.

### 3. The conservative stop is still available

`hard_stop_turns`, the stream sentinel, the trajectory guard, `Esc`, and
`abort` remain the explicit stop paths. The harness does not use "a denial
happened" as an implicit one.

## Consequences

- **Positive**: a rejected call becomes a recoverable steering point; the
  user does not have to restate the task; denial and error have consistent
  loop semantics.
- **Negative & Incurred Costs**: a model can keep proposing denied calls. The
  trajectory guard's repeated-signature detection and the turn budget bound
  that behavior; a denial does not need a special one-shot rule.
- **Neutral**: the permission UI still parks and returns a typed decision; no
  authority is widened.

## Discarded Alternatives (Negative Knowledge)

- **End the round on denial** (the retired behavior): forces a retype and
  hides the model's ability to adapt.
- **Treat denial as a normal tool error**: loses the distinct "the user said
  no" meaning in the transcript and hooks.
- **Add a denial-specific turn budget**: the existing loop guards already
  bound repetition; a second budget would be another policy to tune.

## Links

- [ADR-0166](0166-a-tool-call-has-one-identity-and-the-tool-step-reducer-is-idempotent.md)
- [ADR-0196](0196-model-paths-are-confined-at-the-engine-gate.md)
