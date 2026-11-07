---
id: ADR-0154
title: "Delete the Unwired Spatiotemporal Aspect Engine and Its Dead Cognitive Tasks"
status: accepted
date: 2026-11-07
scope: cognition/agent-loop, cognition/pipeline, protocol/cognitive, docs/architecture
superseded_by: null
negative_knowledge: true
---

# 0154. Delete the Unwired Spatiotemporal Aspect Engine and Its Dead Cognitive Tasks

- Status: Accepted
- Date: 2026-11-07
- Deciders: Nuo Architecture Working Group
- Consulted: Cognitive Runtime, Context Lifecycle, Performance Engineering
- Supersedes: the `aspects` module references in
  [ADR-0100](0100-cognitive-runtime-consolidation-and-governance-layer-inversion.md)
  and [ADR-0101](0101-runtime-internal-layering-and-governance-inner-layer.md)
  (the historical decisions themselves stand)
- Complements: [ADR-0153](0153-effective-input-ceiling-tool-output-retention-and-ingest-image-normalization.md)

---

## Context and Problem Statement

The harness carried a five-phase "Spatiotemporal Aspect Engine" that described
itself as governing the agent lifecycle. An audit of every call site showed that
the engine governed nothing:

- `AspectEngine::evaluate_pre_flight` ran **once per user round** as a full
  provider request with a 1 s timeout, and its `PreFlightRouteOutput`
  (`tier`, `enable_thinking`, `estimated_complexity`) was only written to a
  `tracing::debug!` line. No code branched on it. It was pure time-to-first-token
  latency in front of the actual request.
- `AspectEngine::fire_round_eol` was invoked at every round end and its body was
  empty; the surrounding comment said titling "runs concurrently on first-prompt
  admission", so the hook had no work.
- `AspectEngine::evaluate_tool_gating` was called only from its own unit tests;
  production tool gating goes through `RoundState::guards` directly.
- `AspectEngine::review_stream_loop` / `review_trajectory_loop` were thin
  wrappers with no production caller; the ReAct loop and dispatch pipeline call
  `CognitivePipeline` directly.
- `HarnessTaskPipeline::sense_environment` (the `EnvironmentSensorTask`) had no
  production caller at all.
- `AspectPhase`, `AspectVerdict`, and `AspectHook` were referenced only inside
  the module and its own tests.

This is not an abstraction boundary; it is a second, unwired description of the
loop. It also cost real latency: the pre-flight model call could delay every user
round by up to one second before the first useful request was assembled.

## Decision

Delete the entire dead surface, with no compatibility shims:

- remove `crates/nuo-harness/src/cognition/aspects/` (`AspectEngine`,
  `AspectPhase`, `AspectVerdict`, `AspectHook`);
- remove `Agent::aspects()`, the per-round pre-flight dispatch in
  `orchestration::start_interactive_round`, and the no-op round-EOL call;
- remove `HarnessTaskPipeline::route_pre_flight` and `sense_environment`;
- remove the protocol types `ExecutionTier`, `PreFlightRouteInput`,
  `PreFlightRouteOutput`, `PreFlightRouterTask`, `EnvironmentSensorInput`,
  `EnvironmentReminderOutput`, and `EnvironmentSensorTask`, together with their
  re-exports and tests.

`HarnessTaskPipeline` keeps exactly the tasks with production consumers: thread
title generation, the in-flight stream-loop reviewer, and the pre-dispatch
trajectory-loop reviewer.

`[INV-COG-01]` **No lifecycle phase without a consumer.** A phase, task, or hook
whose only effect is a debug log, an empty function body, or its own test is not
architecture; it must be deleted rather than kept for a hypothetical future.
A future intent-routing feature reintroduces a task together with the branch that
consumes it, not before.

## Rejected Alternatives

- **Keep pre-flight for "future routing".** Speculative wiring is how the dead
  path survived: a plausible name, a system prompt, and no consumer. The cost is
  paid on every round today; the feature can be reintroduced with its consumer.
- **Keep the empty `fire_round_eol` hook.** An empty lifecycle hook trains readers
  to believe work happens at round end and makes the real round tail harder to
  audit.
- **Keep `evaluate_tool_gating` because it has tests.** Tests of an unwired
  function are tests of dead code; the production guard path is
  `RoundState::guards`, which is independently covered.
- **Deprecate instead of delete.** There is no external consumer (the crate is a
  workspace-internal library), so a deprecation window would only preserve the
  rot.

## Consequences

- Every user round saves up to the pre-flight task's 1 s timeout before its first
  real provider request (and avoids one complete model round-trip's cost when the
  call succeeds quickly).
- `nuo-harness` loses a 184-line module and six protocol types; the cognition
  module graph no longer advertises a lifecycle engine it does not run.
- The remaining cognitive tasks are exactly those the loop consumes, so the next
  audit can start from "who calls this?" without an exception list.
- Historical ADR-0100 / ADR-0101 topology diagrams are annotated as amended where
  they named `aspects`; their architectural decisions are unchanged.

## Validation

- `cargo check --workspace --all-targets` is clean.
- `nuo-protocol`, `nuo-harness`, `nuo-server`, and `nuo` suites remain green; the
  removed tests were the only consumers of the removed APIs.
- A repository-wide search for `AspectEngine`, `PreFlight`, `ExecutionTier`,
  `EnvironmentSensor`, and `AspectPhase` returns no Rust references.
