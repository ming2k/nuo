---
id: ADR-0017
title: "Concurrent Aside Execution Substrate and Isolated Round Routing (/btw)"
status: accepted
date: 2026-10-04
scope: server/nuo-server, runtime/aside, comm/routing, protocol/wire
superseded_by: null
negative_knowledge: true
---

# 0017. Concurrent Aside Execution Substrate and Isolated Round Routing (/btw)

- Status: Accepted
- Date: 2026-10-04
- Deciders: Nuo Architecture Working Group
- Consulted: Server, Harness, Protocol, and TUI maintainers
- Informed: System Architects
- Complements: [ADR-0005](0005-unified-binary-and-concentric-runtime-architecture.md), [ADR-0006](0006-wire-contract-consolidation.md), [ADR-0010](0010-harness-decomposition-and-agent-unification.md)

---

## Context and Problem Statement

When interacting with a long-running agent executing multi-step tasks (compilation, refactoring, test suites), users frequently need to ask clarifying questions, check auxiliary facts, or give side directions without aborting the running primary round or polluting the main conversation history.

Without an isolated aside execution channel, users were forced to either wait for primary task settlement or interrupt active work, losing in-flight state and cognitive momentum.

## Decision Outcome

Establish a first-class concurrent aside execution substrate (`/btw`) within the server and session driver architecture:

1. **Dedicated Session Substrate**: An aside runs as a distinct execution context peered to the primary session. It inherits the primary session's read-only static snapshot (tools, role context) but operates on an isolated event stream.
2. **Round Isolation (`[INV-ASIDE-01]`)**: Emitting or aborting an aside round must never interrupt, mutate, or block the active primary session's turn state machine.
3. **Transcript Segregation (`[INV-ASIDE-02]`)**: Aside turns accumulate in a dedicated side-conversation transcript buffer and are never interleaved into the durable primary message history.
4. **Wire Envelope Addressing**: Round events carry authoritative session and routing tags (`origin: None | Some(side_id)`), ensuring the presentation tier (TUI) routes deltas unambiguously to the active viewport.

## Invariants & Behavioral Boundaries

- **`[INV-ASIDE-01] Primary Round Non-Interference`**: The lifecycle of an aside round is strictly independent of the primary session driver. Canceling an aside must never propagate cancellation to primary turns.
- **`[INV-ASIDE-02] History Segregation`**: Aside transcript facts remain quarantined from the primary session's durable transcript so model context in subsequent primary turns remains deterministic.

## Rejected Alternatives & Negative Knowledge

### 1. In-Line Injection into Primary Turn Stream
- **Why considered**: Simple implementation requiring zero new session driver concepts.
- **Why rejected**: Directly corrupted prompt caching and distracted the primary agent with unrelated side chatter, degrading output quality.

### 2. Standalone Disconnected Session Spawning
- **Why considered**: Full isolation by spawning a standard secondary session.
- **Why rejected**: Lacked ambient awareness of the active primary workspace, tool environment, and progress, requiring the user to re-explain context manually.
