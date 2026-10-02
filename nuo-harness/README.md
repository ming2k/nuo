# nuo-harness

Host execution harness, policy governor, and orchestration engine for Nuo.

## Overview

`nuo-harness` serves as the runtime execution bridge between pure contracts (`nuo-contracts`), capability substrates, and the session daemon (`nuo`). It governs execution safety, human-in-the-loop approvals, tool scheduling, and causal context hygiene around cognitive agent turns.

## Core Responsibilities

- **Host Policy & Approvals**: Evaluates tool execution hazards (`RiskProfile`), enforcing interactive confirmation barriers (`human_broker`) for destructive commands or file modifications.
- **Tool Scheduling**: Dispatches tool invocations across decentralized capability providers (`nuo-host`, `acp`, `nuo-persistence`) with timeout envelopes and StreamGuard limits.
- **Cognitive Meta-Tools**: Implements session-bound meta-tools directly attached to harness state:
  - `ask_user`: Interactive structured questions for resolving ambiguity.
  - `spawn_agent`: Spawns isolated exploration subagents with constrained budgets.
  - `todo`: Manages actionable turn task lists.
- **Context Hygiene & Compaction**: Monitors token watermarks, summarizes intermediate rounds, and generates causal context projections to keep reasoning clean.
- **Stream Supervision**: Real-time loop detection and stream guarding preventing runaway tool generation.
