# Provider Extension Design — Adding Dimensions After Single-Definition

- **Status**: Post-Consolidation Design Specification (Reference for ADR-0027 extensions)
- **Scope**: providers, model-declarations, pricing-schedule
- **Role**: Technical design record for extending provider dimensions following single-definition consolidation.

Design notes for extending what a model provider declares: pricing, richer model capability
matrices, and account/tenant dimensions. Written to be executed **after**
[ADR-0027](../adr/0027-provider-definition-single-source-and-adapters-retirement.md) reaches
`accepted` and `[INV-PROV-06]` holds.

> **Dated snapshot (2026-10-08).** The audit below records the tree as it stood
> before the provider consolidation. Crate names from that era
> (`nuo-provider-adapters`, `nuo-provider-transport`) are retired; the live homes
> are `nuo-provider` (specification, transport, OAuth), `nuo-protocol` (error and model
> DTOs), and `nuo-model-routing` (the catalog pipeline). Paths in the audit are
> updated to their current locations so a reader can follow them. The original
> draft used `ModelDecl` / `ProviderManifest`; both are gone, and this revision
> names the live types (`Model`, `ModelProviderSpec`). §3 records the pricing
> decision that has since landed.

---

## 0. Precondition: why this cannot land first

Every dimension below is an *addition* to `ModelProviderSpec`. At the time of this audit each
provider was defined **twice** (see ADR-0027), so an addition had no single home: inside the
retired `nuo-provider-adapters` monolith it hardened that monolith, and in a channel crate it
joined unlinked, drifting code. `[INV-PROV-06]` had to be true first, so that every addition has
exactly one destination.

---

## 1. The placement rule (decides every dimension)

```text
Static fact          (compile-time, verifiable, not behaviour)  →  &'static table on Model / ModelProviderSpec
Runtime fact         (varies per connection/account)            →  RemoteModelMetadata overlay (unchanged)
Behaviour  (has I/O) (quota, health probe, token refresh)       →  a typed port; the table binds WHICH port
```

Corollaries that must hold (`[INV-PROV-07]`, `[INV-PROV-10]`, `[INV-PROV-11]`):

- **No consumer ⇒ no field.** A declaration lands in the same change as its consumer.
- **No `serde_json::Value` catch-all on a core DTO.** Provider-private data goes in a namespaced
  extension bag with a "clients may ignore" contract.
- **Open vocabulary only where the value is passed through.** Anything that *gates a client
  decision* is closed and must model "undeclared" explicitly.

---

## 2. Status audit — what already exists (do not re-add)

Checked against the tree (2026-10-08). Recording "already done" is as important as proposing new
work: it stops the next reader from rebuilding what is present.

| Proposed dimension | Status | Where it already lives |
| :--- | :--- | :--- |
| **Provider health / recovery semantics** | **Already implemented** | `ProviderErrorKind` (13 variants, `#[non_exhaustive]`) + `RetryDisposition::{Never, Retry{retry_after_ms}}` in `protocol/src/error.rs`; `ProviderError::retryable`, `with_status`, `is_context_overflow`; classified in `provider/src/transport/` (`retry_after_ms`), consumed by the harness retry loop. **Nothing to add.** |
| **Quota / balance / credit %** | **Data model done; dispatch is not** | `ProviderQuotaData::{Periodic, Balance, Composite}`, `QuotaWindowBucket.used_fraction`, `BalanceQuota.{credit_limit, consumed_amount, display_primary}` (`protocol/src/connection_detail.rs`). The gap is *dispatch*, not shape — fixed by ADR-0027 step 4 (`[INV-PROV-08]` typed port). **Do not add a new quota schema.** |
| **Account / tenant selection** | **Deferred, with a condition** | Values already ride `ResolvedAuth`'s typed extensions (`OpencodeAuthMetadata.org_id`, `GoogleAuthMetadata.project_id`, `ChatGptAuthMetadata.account_id`). See §5. |
| **`ecosystem` label** | **Rejected** | ADR-0027 §Rejected Alternatives. |
| **`trust` level** | **Rejected** | ADR-0027 §Rejected Alternatives (the gate is the baseline-precedence rule, `nuo-model-routing/src/sync.rs`). |
| **`local_only_safe`** | **Rejected** | ADR-0027 §Rejected Alternatives (feature unimplemented; wrong shape). |

**Net: of the dimensions raised, two are real additions (§3, §4), one is a dispatch fix that ADR-0027
already owns, and four are correctly left out.** This is the expected outcome — most "missing"
provider fields turn out to be either already modelled or without a consumer.

---

## 3. Addition A — Model pricing (landed, shape differs from this proposal)

**Status: built.** The proposal below was the input; the shipped design kept the
constraints and changed the shape. Pricing lives in `protocol/src/pricing/`
(`mod.rs`, `registry.rs`, `schedule.rs`).

What shipped:

- `NanoUsd(u64)` — integer nano-USD, the money unit. The proposal said "micro-units";
  nano won because a single cheap token can cost well under one micro-dollar.
- `ModelPricing { currency, tiers: Vec<PricingTier>, off_peak: Option<OffPeakPricing> }`,
  where a `PricingTier` carries `context_threshold` plus the four per-million rates
  (`input_standard`, `cache_read`, `cache_write`, `output`).
- `OffPeakPricing { tiers, schedule }` — a time-varying rate card, which the proposal
  did not consider.
- `TurnCost { amount, currency, window, input_cost, cache_read_cost, cache_write_cost,
  output_cost }` — the per-turn receipt the consumer needed.

What was **not** adopted:

- **`relative_factor: Option<f32>`.** Abstract credit multipliers such as Qoder's
  `price_factor` do not enter the core type. They are not money, and modelling them
  beside `NanoUsd` invites summing incomparable numbers; a provider that prices in
  credits resolves to "no stated price" and the surface says so.
- **Flat `Option<u64>` per rate.** Tiers and off-peak windows are the real shape of a
  modern rate card; a flat field per rate cannot express either.

The proposal's constraints that survived unchanged:

- **Integer units, never `f64`, for money.** `f64` accumulates error across a session's
  turns; a cost meter is exactly where that shows up.
- **"Not stated" is not "free".** An unpriced route must not render as `$0.00`.
- **Live prices are a runtime fact.** A provider that advertises pricing in its catalog
  belongs in `RemoteModelMetadata` as an overlay, layered above the static table under
  the same resolution order as every other capability. Do not cache live prices into
  the static table.

---

## 4. Addition B — Model capability matrix (too narrow)

`Model` (`protocol/src/model.rs`) carries ten fields: `id`, `family`, `context_window`,
`thinking`, `tool_call`, `vision`, `protocol`, `effort_levels`, `default_effort`, and
`wire_entities`. Resolution of what is genuinely needed:

**Add (each has a clear consumer):**

| Field | Consumer |
| :--- | :--- |
| `streaming: bool` | Gate the streaming path; today it is assumed for every routed channel. |
| `structured_output: bool` | Gate JSON-schema / response-format stamping rather than sending it blind. |
| `parallel_tool_calls: bool` | The Responses builder currently gates the field on the dialect (`emits_parallel_tool_calls_field()` in `nuo-provider-openai/src/responses/request.rs`) and then always stamps `true`. Per-model control is still open: a model that rejects the field must not receive it. |
| `max_images: Option<u8>` | Boundary-check attachments before send, instead of relying on an upstream 400. |

**Do not add yet (no consumer):** audio/PDF input modalities, output modalities, per-model TPM/RPM
(that is rate-limit data, which belongs with quota — §2, not on the capability record).

**Discipline:** every new field is added to `RemoteModelMetadata` as an **`Option`** in the same
change, so a trusted provider can advertise it and the three-layer resolution (user > remote >
baseline, ``) applies uniformly. A static-only field would be unreachable by any provider
that learns the fact later.

---

## 5. Addition C — Account/tenant dimensions (deferred, conditional)

The values exist (`ResolvedAuth` typed extensions). What would be new is a **declaration** that a
provider *needs* a dimension selected. That declaration is only useful to a UI that offers the
selection.

**Condition to build it:** a connection editor that presents a workspace/project/region picker.
Until then, `ResolvedAuth` extensions carry the data and nothing declares a need — so per
`[INV-PROV-07]`, nothing is added.

**When the condition is met, the shape is** a bounded list, not a free-form map:

```rust
pub enum AccountDimension { OrgId, ProjectId, AccountId, Region }
// ModelProviderSpec gains:  pub account_dimensions: &'static [AccountDimension],
```

An enum (not `Vec<String>`) keeps the dimension vocabulary closed and the picker's rendering
exhaustive. Region is listed because the current Qoder region handling (`nuo-provider-qoder/src/region.rs`)
has **no** `ModelProviderSpec` counterpart and is a genuine open question about whether it is a
provider-level or endpoint-level axis — settle it against a real use case, not by guessing.

---

## 6. What this design refuses

- A provider-level `pricing: String` or any untyped cost blob.
- Re-adding any of `ecosystem` / `trust` / `local_only_safe`.
- A second quota schema alongside `ProviderQuotaData`.
- A "capability map" whose keys are strings and values are `serde_json::Value`.
- Any new field shipped without a consumer in the same change.

Each refusal has a rejected-alternative record in ADR-0027 or above; none is a matter of taste.
