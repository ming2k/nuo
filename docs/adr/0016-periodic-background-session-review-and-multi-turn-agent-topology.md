---
id: ADR-0016
title: "Periodic Background Session Review and Multi-Turn Diagnostic Agent Topology"
status: superseded
date: 2026-10-04
scope: harness/review, session/persistence, agent/topology
superseded_by: ADR-0018
negative_knowledge: true
---

# 0016. Periodic Background Session Review and Multi-Turn Diagnostic Agent Topology

- Status: Superseded by [ADR-0018](0018-on-demand-session-review-diagnostics-and-multi-process-history-union.md)
- Date: 2026-10-04
- Deciders: Nuo Architecture Working Group
- Consulted: Runtime, Agent, and Persistence maintainers
- Informed: System Architects
- Superseded by: [ADR-0018](0018-on-demand-session-review-diagnostics-and-multi-process-history-union.md)

---

## Context and Problem Statement

Long-running cognitive agent sessions accumulate large turn histories with diverse tool calls and state mutations. A mechanism was needed to inspect session quality, summarize milestones, and detect infinite execution loops or conversational drift.

The initial proposal called for an autonomous background subagent periodically invoked by the harness every $N$ turns to analyze the transcript and write diagnostic summaries into the session store.

## Decision Outcome

Adopted a periodic background review subagent topology where the harness automatically dispatched an asynchronous diagnostic round at fixed turn intervals.

### Why This Decision Was Superseded

In operational testing, periodic background review introduced severe liabilities:
1. **Token Inefficiency**: Background analysis rounds consumed substantial model quotas even during straightforward linear tasks.
2. **Resource Contention**: Background LLM requests competed for rate limits and I/O concurrency with the user's primary conversational rounds.
3. **Store Invalidation**: Periodic writes to session metadata collided with user-initiated actions, complicating transaction boundaries.

Consequently, [ADR-0018](0018-on-demand-session-review-diagnostics-and-multi-process-history-union.md) superseded this architecture by transitioning session review to explicit user demand (`/review`) and isolating diagnostic verdicts from ongoing session mutation.

## Rejected Alternatives & Negative Knowledge

### 1. Retention of Fixed-Interval Periodic Probes
- **Why considered**: Provides continuous, automated oversight without user intervention.
- **Why rejected**: Proved prohibitively expensive in token consumption and caused unpredictable latency spikes on subsequent user turns.

### 2. Silent Store Injections During Primary Generation
- **Why considered**: Co-scheduling review passes inside model idle frames.
- **Why rejected**: Violated transcript immutability principles and introduced split-brain states when parallel processes manipulated the same workspace.
