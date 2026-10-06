---
id: ADR-0027
title: "Provider Definition Single Source: Retire nuo-provider-adapters and Relocate Its Residual Engines"
status: accepted
date: 2026-10-08
scope: providers/architecture, substrate/nuo-provider, providers/namespace, architecture/layering
superseded_by: null
negative_knowledge: true
---

# 0027. Provider Definition Single Source: Retire nuo-provider-adapters and Relocate Its Residual Engines

- Status: Accepted
- Date: 2026-10-08
- Deciders: Nuo Architecture Working Group
- Consulted: Runtime, Capability, Interface Teams, and Model Infrastructure Engineers
- Informed: System Architects
- Amends: [ADR-0015](0015-dependency-inversion-architecture-for-model-providers.md) (executes its unfinished relocation and closes its `[INV-PROV-01..05]` enforcement gap)
- Related: [ADR-0014](0014-model-provider-invocation-schemes-oauth-subscription-lane-and-api-key-byok-lane.md), `ADR-0065`, `ADR-0149`, `ADR-0230`, `ADR-0259`, `ADR-0260`, `ADR-0267`, `ADR-0273`
- Verification: Complete read-only forensic diff of `nuo-provider-adapters` against `providers/*` performed 2026-10-08; results recorded in §Evidence below, including one live production defect.

---

## Context and Problem Statement

[ADR-0015](0015-dependency-inversion-architecture-for-model-providers.md) established the canonical leaf contract crate `nuo-provider` and a dedicated `providers/nuo-provider-*` namespace mirroring `tools/nuo-tool-*`. It was **half-executed**:

- The `providers/` namespace was created — 15 crates, all members of the workspace, all compiling.
- `nuo-provider-adapters` was **not** retired. It retained full copies of the provider registry, the wire protocol implementations, and the usage fetchers.
- Consequently, **every built-in provider is defined twice**, and the shipped binary links the legacy copy, never the `providers/` crates.

Today the only cross-namespace dependency is `nuo-provider-adapters → nuo-provider-qoder` (`nuo-provider-adapters/Cargo.toml:30`). The other 13 `providers/nuo-provider-*` crates are linked by **nothing**. `nuo`, `nuo-server`, `nuo-tui`, and `nuo-harness` depend solely on `nuo-provider-adapters`.

This is not a cosmetic duplication. It is a **live correctness hazard**, and it has already produced one:

> **Kimi Code quota reporting is silently unavailable, and *neither* copy is correct.**
>
> The shipped fetcher (`nuo-provider-adapters/src/usage/kimi.rs:36`) matches only `provider == "kimi"` or `base_url.contains("moonshot.cn")`. A `kimi-code` connection canonicalizes to provider id `"kimi-code"` (`nuo-model-codec/src/model_providers.rs:244`) and its endpoint is `https://api.kimi.com/coding/v1` (`nuo-provider-adapters/src/registry/kimi.rs:111`), so **neither predicate matches** and `fetch_provider_usage` cleanly returns `Unsupported`.
>
> The `providers/nuo-provider-kimi` copy widens the predicate to include `"kimi-code"` and `"kimi.com"` (`providers/nuo-provider-kimi/src/usage.rs:32`) — but its `fetch_usage` still special-cases only `moonshot.cn` and otherwise hardcodes `https://api.moonshot.cn/v1/users/me/balance` (`:41-46`). Linking that copy would therefore **claim** support and then issue a request to the wrong host with a Kimi Code key, converting a clean `Unsupported` into a confusing `Error`.
>
> This is therefore not "the newer copy is right" — it is a **two-definitions defect with no correct copy**. Resolving it requires (a) a *verified* balance endpoint for `api.kimi.com` (none is asserted anywhere in the repository; the guess must not be compiled in unverified) and (b) the typed-port binding of Migration Sequence step 5. It is recorded here as the worked example of the class `[INV-PROV-06]`/`[INV-PROV-08]` exist to prevent — and as a caution **against assuming the `providers/` copy is authoritative merely because it is newer**.

The root cause is structural, not incidental: **with two definitions of the same provider, neither is authoritative, and drift is guaranteed.** Duplication of this kind cannot be maintained correctly by discipline alone — the copies must be collapsed to one.

We now finish the migration ADR-0015 began, and close the enforcement gap that let it stall.

---

## Decision Drivers

- **A provider must have exactly one definition.** Two copies of a 919-line request builder (`providers/nuo-provider-openai/src/responses/request.rs` ≡ `nuo-provider-adapters/src/protocol/openai/responses/request.rs`, byte-identical) is not a staging state; it is an active risk that will silently diverge on the next edit.
- **A declaration without a consumer is a defect, not a placeholder.** `ProviderDescriptor`, `ProviderRegistry` (`register_quota_tracker`/`get_quota_tracker`/`fetch_quota`/`register_descriptor`/`get_descriptor`), and `builtin_provider_metadata` have **zero production call sites**. ADR-0015's `[INV-PROV-03]` "orthogonal capability segregation" was *declared by types* but never *bound by wiring*. Three dead abstractions are how `[INV-PROV-03]` came to be false in practice.
- **Dispatch must not be string-sniffing.** `ProviderUsageFetcher::matches(provider: &str, base_url: &str)` decides behaviour by substring-matching an identity string and a URL. This is the mechanism that produced the Kimi defect; it is a class of bug, not an instance.
- **The dependency-inversion topology must be real, not aspirational.** `[INV-PROV-04]` is currently satisfied only by luck (the harness→adapters edge is in `[dev-dependencies]`, for tests). The composition root — not a monolithic adapter crate — must be the only thing that knows all providers.
- **Preserve every behavioural guarantee already earned.** Capability resolution order (`ADR-0149`), the fitted-model overlay (`ADR-0065`), vision's three-valued semantics (`ADR-0230`), the root URL algebra (`ADR-0259`), and declarative wire surfaces (`ADR-0260`) are all correct. This migration must be behaviour-preserving *except* where a defect is fixed deliberately and with evidence.

---

## Considered Options

- **Option 1 (chosen):** `providers/nuo-provider-*` becomes the single authoritative definition of every provider. `nuo-provider-adapters` is retired. Its residual content — three items that exist nowhere else — is relocated to purpose-built crates, and the composition root moves to `nuo-server`.
- **Option 2:** Keep `nuo-provider-adapters`; delete the `providers/` namespace as a failed experiment.
- **Option 3:** Keep both; wire the `providers/` crates in as re-exports over the adapters implementations (invert the direction of consolidation).

---

## Decision Outcome

Chosen option: **Option 1 — `providers/` is authoritative; `nuo-provider-adapters` is retired.**

The forensic audit establishes that this is behaviour-preserving for the overwhelming majority of the surface, because the two trees are **byte-identical modulo re-export import paths**: 16 provider specs with 110 baseline models match field-for-field, `effort_ladders` matches (and *already* lives in the leaf crate `nuo-provider/src/effort_ladders.rs`), and the wire protocol implementations differ only in `crate::X` ↔ `nuo_provider_transport::X` path rewrites that resolve to the same items.

Retirement is therefore dominated by **relocation of three orphans**, not by rewriting behaviour:

### Relocation Map

| Residual content | Fate | Target |
| :--- | :--- | :--- |
| `list_models.rs` (1638 lines: `CatalogParser` trait, 6 shape parsers, fetch pipeline) | **Relocate** — exists nowhere else | `providers/nuo-provider-catalog` (new) |
| `oauth/{mod,credential_source,enricher,manual}.rs` — the OAuth **engine** (`OAuth`, `OAuthLoginSession`, `BrowserLogin`, `build_token_set_from_login`, `OAuthCredentialSource`, the enricher family) | **Relocate** — into the same crate as the RFC primitives, so OAuth lives in exactly one place | `nuo-oauth` (new, **workspace root**): RFC primitives (`nuo_oauth::oauth`) **and** the engine. The engine is vendor-agnostic: vendor `OAuthConfig`s, device grants, enrichers, refresh and repair live in their owning `providers/nuo-provider-*` crate and are registered through the typed `OAuthProvider` port at the composition root |
| `usage/siliconflow.rs`, `oauth/token.rs::retrieve_antigravity_quota_summary` | **Relocate** — no counterpart | same new crates as their kin |
| `registry/mod.rs` (`MODEL_PROVIDER_SPECS`, `build_provider_for_channel`), `lib.rs::init()` | **Relocate** — this is composition-root glue | `nuo-server` (the composition root per ADR-0015 §1) |
| `registry/{custom_baselines,baseline_fidelity_tests}.rs` | **Relocate** | owning provider crate or the fidelity-test home |
| All other `protocol/*`, `registry/*` specs, `usage/*` fetchers, `client.rs`, `endpoint.rs`, `oauth/{chatgpt_device,opencode_device}.rs` | **Delete duplicates** — `providers/` already carries them | — |

The crate `nuo-provider-adapters` is removed outright once its content has been relocated; **no re-export shim is retained** (no backward-compatibility requirement). Dependents (`nuo`, `nuo-server`, `nuo-tui`, and `nuo-harness`'s dev-dependency) are repointed in the same change, alongside the crate's own tests and examples, whose internal-path references must be re-homed to the new owners.

### The boundary rule (what belongs where)

This decides every future "add a capability" question, and prevents the ADR-0015 failure from recurring:

```text
Static fact         (compile-time, verifiable, not behaviour)  →  &'static table in the provider crate
Runtime fact        (varies per connection)                    →  RemoteModelMetadata overlay (unchanged)
Behaviour (has I/O) (quota, catalog, token refresh, signing)   →  a typed port; the table binds WHICH port
```

A capability is **never** a bare `serde_json::Value` catch-all on a core DTO, and **never** a substring match on an identity string. ADR-0260's "everything is a table" is correct for *wire shape*; it is **not** a universal rule, because quota/health/OAuth have irreducible behaviour. The honest formulation: **a table declares which implementation to call; a table is not the implementation.**

### Decision on openness of provider-advertised values

Splitting the question that `ADR-0065` tends to over-generalize:

> **Open vocabulary is safe where the value is only passed through; it is dangerous where the value gates a client decision.**

- `EffortLevel::Other(String)` is open and correct — an unknown tier is stamped to the wire verbatim (`ADR-0065`).
- `vision: Option<bool>` **must** stay closed and three-valued — it gates whether images are stripped (`ADR-0230`).
- `Availability::reason` is a free string and correct — `[INV-AVAIL-03]` forbids any code path from parsing it.

Therefore provider-private data that nuo does not model travels in a **namespaced extension bag** with an explicit "clients may ignore" contract (`ADR-0267`'s `ExtensionMap` is the existing pattern), never as a flattened core field.

### Invariants & Behavioral Boundaries

- **`[INV-PROV-06] Single Definition`**: Each model provider id has exactly **one** authoritative `ModelProviderSpec` definition, in its `providers/nuo-provider-*` crate. A second definition of the same id anywhere in the workspace is a defect; a test or check must fail on it.
- **`[INV-PROV-07] No Orphan Contract`**: Every public item in `nuo-provider` has at least one **non-test** consumer in the shipped build. A contract type with zero production consumers must be wired or deleted in the same change that introduces it — never merged as a placeholder. *(This invariant exists specifically because `ProviderDescriptor`, `ProviderRegistry`, and `builtin_provider_metadata` violated it.)*
- **`[INV-PROV-08] No String-Sniffing Dispatch`**: Capability dispatch keys off typed declarations. No code path selects behaviour by matching a provider id string or a base-URL substring. *(Fixes the Kimi defect's whole class.)*
- **`[INV-PROV-09] Providers Are Linked`**: The shipped build links every crate that claims to be a provider. An unlinked `providers/nuo-provider-*` crate is dead code, not a provider.
- **`[INV-PROV-10] Behaviour Behind Ports`**: Static facts are `&'static` tables; runtime facts are overlays; behaviour is a trait port. No fourth mechanism.
- **`[INV-PROV-11] Openness Is Pass-Through Only`**: Open/unknown provider-advertised values are permitted only where they are passed through verbatim; any value that gates a client decision is closed and must model "undeclared" explicitly.
- **`[INV-PROV-12] Behaviour Preservation`**: This migration changes no wire bytes, no capability resolution, and no cache semantics. The two-definitions `kimi` usage defect is **not** silently resolved by the migration: under this invariant the shipped (adapters) predicate and endpoint are carried over verbatim, and the defect is fixed only by a deliberate, verified change (see §Errata).

### Positive Consequences

- One definition per provider removes the drift hazard by construction, not by discipline.
- The `providers/` crates become reachable, so any already-correct content they carry stops being dead. *(Caveat: reachability is not itself correctness — the `kimi` fetcher shows a newer copy can be wrong in a different way; see §Context.)*
- Per-provider crates make the cryptographic sandboxing requirement real: `nuo-provider-qoder`'s RSA/AES/MD5 stack is already isolated, and retiring the monolith stops it from being reachable through a shared blob.
- The OAuth engine and catalog parsers each get a coherent home, which makes the RFC-primitive/workflow split ADR-0015 called for actually true.
- Deleting three orphan abstractions removes exactly the false signals that make a codebase feel larger than it is.

### Negative Consequences & Trade-offs

- **The migration is a large, cross-crate change**, touching `Cargo.toml` manifests, `init()`, and every spec/usage/fetcher. Mitigation: the audit proves the copies are equivalent modulo import paths, so the change is mechanical; it is sequenced so that each step is independently verifiable (§Migration Sequence).
- **Linking every provider crate increases the composition root's compile graph**, re-introducing some of the compile-time cost the monolith was implicitly paying for today. Mitigation: this is the same trade `tools/nuo-tool-*` already made and accepted; feature-gating per provider remains available if build time becomes a problem.
- **The new crates (`nuo-oauth`, `nuo-provider-catalog`, `nuo-provider-siliconflow`) are an increase in crate count.** Mitigation: each has a single, nameable responsibility and zero duplicated content — unlike the state being replaced. `nuo-oauth` sits at the workspace root because OAuth is a cross-cutting authentication concern, not a provider definition; it links no vendor crate.
- **Behaviour-preservation is a hard rule with one *pending* defect.** No deliberate behaviour change is bundled into the mechanical migration. The `kimi` usage defect (§Context) is a genuine bug in *both* copies and is tracked as a separate, verified fix; it must not be mistaken for a routine "adopt the newer copy" merge. Mitigation: it is called out in §Errata and requires a live-verified endpoint plus a regression test before any code moves.

---

## Errata (deliberate behaviour changes)

- **None bundled into this migration.** The `kimi` usage defect is *not* resolved by adopting the `providers/` copy — that copy's `fetch_usage` hardcodes the `moonshot.cn` host and would fail against a Kimi Code key. The fix requires a **live-verified** balance endpoint for the `api.kimi.com` surface, which no artifact in the repository asserts. Until that verification exists, the migration carries the shipped behaviour over unchanged, and the defect is tracked separately. *(Recorded against the temptation to auto-prefer the newer duplicate.)*

---

## Evidence (2026-10-08 read-only forensic audit)

**Byte-identical duplicates** (both trees, same line counts): `openai/{chat_completions/response.rs, responses/response.rs, responses/tool_trace.rs, cache.rs}`; `anthropic/{response.rs, signature.rs, thinking.rs, tests.rs}`; `google/response.rs`; `oauth/chatgpt_device.rs` ↔ `chatgpt-plan/device.rs`; `oauth/opencode_device.rs` ↔ `opencode/device.rs`.

**Drifted by re-export import paths only** (`crate::X` ↔ `nuo_provider_transport::X`; equal line counts): `openai/{chat_completions/mod.rs, chat_completions/request.rs, chat_completions/echo.rs, responses/mod.rs, responses/request.rs}`; `anthropic/{mod.rs, request.rs}`; `google/{mod.rs, request.rs}`.

**Structural drift** (`providers/` side has more): `providers/nuo-provider-openai/src/lib.rs` adds `pub mod spec`; `nuo-provider-anthropic/src/lib.rs` adds `pub mod spec`; `nuo-provider-google/src/lib.rs` adds `spec_antigravity`/`spec_google`/`usage`. The additional modules are exactly the spec/usage tables, which the adapters keep in `registry/` instead.

**Identical content, differing only in trait-shape**: all 16 shared provider specs and their 110 baseline models match field-for-field (`context_window`, `thinking`, `tool_call`, `vision`, `effort_levels`, `root_url`, `protocol`, `dialect`, `catalog_source`, `models`). No side has more models or more fields.

### Drift & defect register

- **`kimi` usage fetcher (two-definitions defect, no correct copy)** — see §Context. Predicate *and* endpoint both differ, and the `providers/` variant introduces a wrong-host failure. **Not** a "newer copy wins" case; requires a verified endpoint before any behaviour change. This is the single item in the audit that is *not* behaviour-preserving under the `providers/`-authoritative rule, and it is explicitly **deferred** to a defect fix rather than folded into the mechanical migration (see §Errata).

**Adapters-only with no counterpart (relocation candidates, never deletable)**: `list_models.rs` (all 6 parsers + pipeline); `usage/siliconflow.rs`; `oauth/{credential_source,enricher,manual}.rs` and the `OAuth`/`OAuthLoginSession`/`BrowserLogin` engine in `oauth/mod.rs`; `oauth/token.rs::retrieve_antigravity_quota_summary`; `registry/{mod,custom_baselines,baseline_fidelity_tests}.rs`; the `ProviderUsageFetcher` trait + dispatcher.

**Dead contract abstractions (zero production consumers)**: `ProviderDescriptor`; `ProviderRegistry` and all its methods; `builtin_provider_metadata`. `model_provider_label` is live.

---

## Correction to the record

An intermediate automated pass reported a syntax error in `openai/responses/mod.rs` (a malformed `let Some(h) = …`). Direct inspection of both copies at the reported location (line 976) shows ordinary, valid code (`provider.build_request_for_auth(...)`). The finding was a misread and is recorded here so no future reader inherits a false alarm.

---

## Migration Sequence

Each step is independently verifiable; the sequence is ordered so that no step leaves the workspace uncompilable.

1. **Re-verify the diff in the build environment** (`diff -ru`/hashes) to confirm the equivalence classes above hold byte-for-byte outside the audited sample. Blocking: if any file is found drifted beyond import paths, it is escalated to the `providers/`-authoritative rule case by case before proceeding.
2. **Create `nuo-oauth` (workspace root)** and relocate the RFC primitives **and** the OAuth engine (`OAuthCredentialSource`, the enricher port, `manual.rs`) into it, so OAuth lives in exactly one crate. It links only `nuo-provider`, `nuo-model-codec`, and `nuo-provider-transport` (HTTP); each vendor owns its `OAuthConfig`, device grant, enricher, refresh and credential repair and registers an `OAuthProvider` at the composition root (no engine→vendor edge).
3. **Create `providers/nuo-provider-catalog`** and relocate `list_models.rs`, including the `CatalogParser` family and the shape→parser map.
4. **Relocate `SiliconFlowUsageFetcher`** (and `retrieve_antigravity_quota_summary`) into the owning provider crates.
5. **Fix `[INV-PROV-08]`:** replace `ProviderUsageFetcher::matches(provider, base_url)` string-sniffing with typed port binding on the provider spec. This is the step that removes the Kimi bug's mechanism, not just its instance; add a regression test asserting a `kimi-code` connection resolves its usage fetcher.
6. **Move the composition root**: `init()` and `MODEL_PROVIDER_SPECS` → the composition root; there are **two** live `init()` call sites (`nuo/src/main.rs:21`, `nuo-server/src/bootstrap.rs:157`), and every provider spec is now sourced from its `providers/` crate.
7. **Re-home the crate's tests and examples** (they reference internal paths being split across crates — `tests/qoder_wire_golden.rs` guards `[INV-WIRE-01]`; `tests/it/{wire,qoder_catalog_contract}.rs`, `tests/integration.rs`, `examples/*_live_smoke.rs`, `examples/qoder_catalog_dump.rs`). This is a re-homing exercise, not a rename.
8. **Delete duplicates** in `nuo-provider-adapters/src/{protocol,registry,usage}/*`. No re-export shim is permitted — there is no backward-compatibility requirement.
9. **Resolve the dead abstractions** (`ProviderDescriptor`, `ProviderRegistry`, `builtin_provider_metadata`): wire each to a real consumer or delete it. No third state.
10. **Repoint every reference and delete the crate**: the reference set is dominated by **Rust paths**, not manifests — `nuo-server/src/handlers_provider.rs` alone holds ~20 `nuo_provider_adapters::` sites, plus `credentials_host.rs`, `session_driver.rs`, `nuo-harness`/`nuo-server` catalog tests, and the `nuo-tui` test `extern crate`. Enumerate with `rg -l 'nuo-provider-adapters|nuo_provider_adapters' --glob '!target/**'`, not a `--glob '*.toml'` search. Then delete `nuo-provider-adapters`.
11. **Doc sweep**: comments and docs that name the retired crate become false — including a **panic message** in `nuo-provider/src/factory.rs:46` and `nuo-provider/README.md`.
12. **Governance sync, same change**: update `docs/architecture/subsystems.md` §2.3 and ADR-0015 (mark its `providers/` migration executed); run `docgov check` and the full suite.

Execution detail for each step — including the per-file relocation map for tests/examples and the
per-step verification gates — is in [`docs/dev/provider-consolidation-migration.md`](../dev/provider-consolidation-migration.md).

---

## Rejected Alternatives & Negative Knowledge

### Option 2 (Rejected) — Keep `nuo-provider-adapters`; delete `providers/`
- **Why considered**: Smallest delta; the monolith is the thing that actually works today, and 13 of its 15 sibling crates are unlinked anyway.
- **Why rejected**: It repeals an accepted ADR to preserve a decision that *caused* the defect being fixed. It also permanently forfeits the per-provider cryptographic sandboxing that `[INV-PROV-05]` requires and that `nuo-provider-qoder` already demonstrates. It offers no resolution for the `kimi` usage defect either: the `providers/` copy is *not* a correct version to preserve (see §Context) — deleting it resolves nothing and keeps the ambiguity in place.

### Option 3 (Rejected) — Keep both; make `providers/` thin re-exports over the adapters implementations
- **Why considered**: Avoids moving the large `list_models.rs` and `oauth/` engines.
- **Why rejected**: It inverts the dependency direction ADR-0015 mandates (`nuo-provider` contract, `providers/` implementation, adapters retired) and preserves the monolith as the real home of all behaviour. It also merely relocates the duplication: the spec tables and wire implementations would still exist twice, with `providers/` becoming a hollow facade. Consolidation that leaves the code in place is not consolidation.

### Rejected — Adding new provider dimensions before consolidation
- **Why considered**: Pricing, richer model capability matrices, health semantics, and account dimensions are all genuinely missing, and adding them is the natural next step.
- **Why rejected for now**: Adding a dimension today forces a choice of *which* of the two definitions receives it, which either hardens the monolith or adds to the unlinked tree — deepening the very divergence this ADR removes. `[INV-PROV-06]` must hold first, so that a new dimension has exactly one place to go. This ADR explicitly **defers** those dimensions rather than rejecting them.

### Rejected — A `trust: TrustLevel` provider field
- **Why considered**: Source comments repeatedly say "trusted provider", suggesting an unmodeled concept.
- **Why rejected**: The actual gate is structural and non-configurable — the fitted-model overlay only accepts ids absent from the compiled baseline (`nuo-harness/src/catalog/sync.rs:485`, `filter(|model| model_by_id(&model.id).is_none())`), which is why a relay cannot inflate a known model. A configurable trust flag would be either redundant with that precedence rule or a **weaker** second gate that can contradict it — and the dangerous case is a maintainer setting it to `Full` and reopening a door the precedence rule closes. Trust is a property of the resolution rule, not of the provider. Making an unconfigurable invariant configurable is a net loss.

### Rejected — `local_only_safe` and `account_dimensions` provider fields
- **Why considered**: `[INV-LANE-06]` references a local-only mode; opencode/Google/Qoder need workspace/project/region selection.
- **Why rejected**: (a) Local-only mode has **zero implementation** — `NUO_LOCAL_ONLY` appears only in documentation — so the flag would guard nothing. (b) Even implemented, a boolean is the wrong shape: `[INV-LANE-06]`'s criterion is "user-declared locals only", which an existing declaration already answers, and "is this local" is properly derived from the resolved endpoint host, not asserted (a user's intranet relay is not "local" nor "remote" in any way a bool captures). (c) `account_dimensions` has no consumer — `ResolvedAuth` extensions already carry the values by type; its only use would be a workspace picker that does not exist. Per `[INV-PROV-07]`, both are deferred until a consumer exists.

### Rejected — `ecosystem: Ecosystem` provider field
- **Why considered**: Convenient browsing taxonomy (`FirstParty` / `Subscription` / `Gateway` / `Local`).
- **Why rejected**: It is a label with zero decision consumers — everything that differs (auth, catalog shape, dialect, quota source) is already declared separately — and it runs against `ADR-0260`, whose stated purpose is to move from brand enums to declarative data tables.

---

## Links

- Amends: [ADR-0015](0015-dependency-inversion-architecture-for-model-providers.md)
- Execution runbook: [`docs/dev/provider-consolidation-migration.md`](../dev/provider-consolidation-migration.md)
- Related: [ADR-0014](0014-model-provider-invocation-schemes-oauth-subscription-lane-and-api-key-byok-lane.md), `ADR-0065`, `ADR-0149`, `ADR-0230`, `ADR-0259`, `ADR-0260`, `ADR-0267`, `ADR-0273`
- Implementation anchors: `nuo-provider-adapters/src/{lib,list_models,client,endpoint}.rs`, `nuo-provider-adapters/src/{protocol,registry,usage,oauth}/`, `providers/nuo-provider-*`, `nuo-provider/src/{capability,registry,descriptor,spec}.rs`, `nuo-server/src/{bootstrap,handlers_provider}.rs`, `nuo-model-codec/src/model_providers.rs`
- Deferred (tracked, not rejected): model-level pricing, extended capability matrix, provider account/tenant dimension declarations. Extension design recorded in [`docs/dev/provider-extension-design.md`](../dev/provider-extension-design.md) — includes the audit finding that provider **health/recovery semantics already exist** (`ProviderErrorKind` + `RetryDisposition`) and need no new fields.
