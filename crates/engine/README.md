# nuo-engine

Cognitive orchestration engine for Nuo (傩): the single streaming ReAct runtime,
model-request assembly, causal compaction, and multi-agent interop layer.

## Overview

In the Nuo architecture, `nuo-engine` sits between the zero-I/O domain
(`nuo-protocol`), the capability contracts (`nuo-tool`, `nuo-provider`), and the
application core (`nuo-server`). It owns the agent's mind and the policy that
wraps it, not the product UI, the session daemon, or any concrete capability
leaf.

The crate name is `nuo-engine`. "Harness" is a role, not a crate identifier
([ADR-0181](../../docs/adr/0181-cognitive-runtime-identity-nuo-engine-is-the-only-crate-name.md)).

## Subsystems

- `cognition/` — the `Agent`, the streaming ReAct loop, model-request assembly,
  durable conversation context, causal compaction, skills, and interop
  (subagents, human broker).
- `orchestration/` — per-round policy: compaction gating, retries, round
  lifecycle, session titles, and `/schedule`.
- `dispatch/` — the tool execution pipeline and scheduler.
- `runtime/` — the innermost substrate: host ports, execution environment,
  hooks, message/time/retry leaves, and extensions.
- `tools/` — only orchestration-native meta-tools: `ask_user`,
  `recall_memory`, and `todo`.

`nuo-guardrails` is a separate crate, not a `governance/` module of this one
([ADR-0181](../../docs/adr/0181-cognitive-runtime-identity-nuo-engine-is-the-only-crate-name.md)).

## Dependency posture

`nuo-engine` depends on `nuo-protocol` (zero-I/O vocabulary), `nuo-persistence`
(durable state), `nuo-model-routing`, and **`nuo-tool` only** among the tool
crates. Concrete capability leaves (`nuo-tool-fs`, `nuo-tool-exec`,
`nuo-tool-web`, `nuo-tool-ast`) are linked by the composition root
(`nuo-server`), never by the cognition layer
([ADR-0183](../../docs/adr/0183-composition-root-owns-capability-linkage.md)).
