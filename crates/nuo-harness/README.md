# nuo-harness

Host execution harness, policy governor, and cognitive orchestration engine for Nuo (傩).

## Overview

In the Nuo architecture, `nuo-harness` is the runtime execution bridge between capability substrates (`nuo-host`, `acp`, `nuo-persistence`), the client/SDK layer (`nuo-client`), and the session daemon (`nuo`).

It embodies the core inspiration of Nuo (傩): in traditional Nuo rituals, the priest dons a Nuo mask to convey intent according to the image and persona embodied by the mask to commune with the divine; in engineering, `nuo-harness` uses an execution harness to employ different identities to communicate and collaborate with underlying intelligence safely.

## Core Responsibilities

- **Identity & Persona Projection**:
  - Dynamically synthesizes system prompts with project context, tool documentation, and peer directories.
  - Configures subagent presets (`explore`, `debug`, `skill`) via `spawn_agent` with scoped token budgets and private working memory.
- **Safety Governance & Hazard Control**:
  - Evaluates tool execution hazards (`RiskProfile`), enforcing interactive confirmation barriers (`human_broker`) for destructive commands or file modifications.
  - Implements real-time loop detection (`stream_loop_detector`) and StreamGuard limits to halt runaway generations.
- **Tool Scheduling & Dispatch**:
  - Schedules and executes tools across decentralized capability providers (`nuo-host`, `acp`, `nuo-persistence`) with timeout envelopes and sandboxing.
- **Cognitive Meta-Tools**:
  - `ask_user`: Interactive structured questions for resolving ambiguity and seeking user authorization.
  - `spawn_agent`: Spawns isolated exploration subagents with constrained budgets and scoped roles.
  - `todo`: Manages actionable turn task lists.
- **Context Hygiene & Causal Compaction**:
  - Tracks token watermarks, summarizes intermediate rounds, and produces causal context projections to keep reasoning clean across multi-turn interactions.
