---
id: ADR-0188
title: "Guardrails Depend on Contracts, Not Persistence"
status: accepted
date: 2026-11-14
scope: guardrails, protocol/config, host, persistence
superseded_by: null
supersedes: []
negative_knowledge: true
---

# 0188. Guardrails Depend on Contracts, Not Persistence

- Status: Accepted
- Date: 2026-11-14
- Amends: [ADR-0008](0008-single-tool-contract.md), [ADR-0013](0013-decoupled-tool-namespace-and-infrastructure-purity.md)

## Context and Problem Statement

`nuo-guardrails` is the policy layer: permission brokering, bash policy,
execution containment, and loop guards. It depends on `nuo-persistence` for
three kinds of value:

- permission and bash-policy **configuration types**;
- path resolution (`nuo_persistence::paths::Dirs`);
- an atomic file write (`nuo_persistence::fsutil::atomic_write_json`).

That edge links a policy crate to the SQLite-bearing storage layer for data it
does not store. It also means a change to migration or connection code can
invalidate the guardrail crate's build, and the guardrail's dependency graph
misrepresents its actual purity.

The root cause is that the configuration shapes were placed in persistence
because they are deserialized from `config.toml`, not because persistence owns
their meaning.

## Architectural Decisions

### 1. Policy configuration shapes live in `nuo-protocol`

A new `nuo-protocol/src/policy_config.rs` owns the pure serde types:

- `PermissionRuleConfig`
- `BashPolicyConfig`, `BashPolicyRuleConfig`,
  `BashPolicyMatcherConfig`, `BashPolicyActionConfig`

`nuo-persistence::config` re-exports them as part of its configuration facade
so `Config` keeps one public shape. These are data contracts, not storage
rows.

### 2. Guardrails depends on protocol and host only

`nuo-guardrails/Cargo.toml` drops `nuo-persistence` and adds `nuo-host`.
Path resolution and atomic writes use `nuo_host::paths` /
`nuo_host::fsutil`. Policy configuration uses `nuo_protocol::policy_config`.

### 3. Invariant

- **`[INV-GUARD-01]` No Storage Edge**: `nuo-guardrails` must not depend on
  `nuo-persistence`. A manifest test enforces the edge.

## Consequences & Trade-offs

- **Positive**: The policy crate no longer compiles or links the storage stack;
  policy configuration has a zero-I/O owner; the dependency graph tells the
  truth.
- **Negative & Incurred Costs**: A one-time move of five serde structs and the
  guardrail import sweep. `nuo-persistence::config` keeps re-exports so the
  config surface does not grow a second public path.

## Discarded Alternatives (Negative Knowledge)

### Alternative A — Move the config structs into `nuo-guardrails`
- **Why considered**: The policy crate owns policy semantics.
- **Why rejected**: Persistence would then depend on guardrails just to
  deserialize `Config`, creating a storage → policy edge in the opposite
  direction and a new cycle risk.

### Alternative B — Leave the edge and document it
- **Why considered**: Zero churn.
- **Why rejected**: A policy crate linking rusqlite for path strings is a
  misrepresented boundary, not a documented exception.

## Links

- [ADR-0008](0008-single-tool-contract.md)
- [ADR-0013](0013-decoupled-tool-namespace-and-infrastructure-purity.md)
- [ADR-0180](0180-single-ownership-of-shared-vocabulary.md)
