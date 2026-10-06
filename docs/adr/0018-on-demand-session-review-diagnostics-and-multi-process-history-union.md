---
id: ADR-0018
title: "On-Demand Session Review Diagnostics and Multi-Process History Union"
status: accepted
date: 2026-10-04
scope: harness/review, session/persistence, history/union
superseded_by: null
negative_knowledge: true
---

# 0018. On-Demand Session Review Diagnostics and Multi-Process History Union

- Status: Accepted
- Date: 2026-10-04
- Deciders: Nuo Architecture Working Group
- Consulted: Harness, CLI, Persistence, and Storage maintainers
- Informed: System Architects
- Supersedes: [ADR-0016](0016-periodic-background-session-review-and-multi-turn-agent-topology.md)
- Complements: [ADR-0005](0005-unified-binary-and-concentric-runtime-architecture.md), [ADR-0008](0008-single-tool-contract.md)

---

## Context and Problem Statement

Following the retirement of periodic background review ([ADR-0016](0016-periodic-background-session-review-and-multi-turn-agent-topology.md)), developers still required a reliable mechanism to diagnose session progress, inspect tool success ratios, and assess cumulative trajectory quality.

Concurrently, multi-process terminal workflows frequently run parallel client commands (`nuo`, `nuo status`, `nuo run`) across tabs, causing last-write-wins file clobbering in shell input history and session ledgers.

## Decision Outcome

1. **On-Demand `/review` Diagnostics**: Session evaluation is made strictly user-invoked via `/review`. The harness executes a single-pass diagnostic analysis producing structured verdicts without mutating the underlying conversation history.
2. **Union-on-Write History Strategy (`[INV-HIST-01]`)**: Persisting command and shell input history across processes must employ an additive union-on-write model. When committing new entries, the process loads concurrent entries from disk, performs a deduplicated set-union, and safely appends without clobbering sibling process history.

## Invariants & Behavioral Boundaries

- **`[INV-HIST-01] Additive History Preservation`**: Concurrent writer processes must never overwrite or truncate peer process entries in history stores.
- **`[INV-REVIEW-01] Review Idempotency`**: Running `/review` produces diagnostic output on the command presentation surface but does not append synthetic user/assistant messages to the persistent transcript.

## Rejected Alternatives & Negative Knowledge

### 1. Last-Write-Wins File Replacement
- **Why considered**: Standard simple file serialization (`fs::write(path, serialize(history))`).
- **Why rejected**: Caused continuous loss of commands executed in secondary terminals and disrupted user recall.

### 2. SQLite Database for Local Shell History
- **Why considered**: Native multi-process ACID locking.
- **Why rejected**: Added binary dependencies and schema migration overhead for simple linear string histories. Union-on-write with filesystem atomicity achieved the requirement cleanly.
