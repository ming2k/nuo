# Provider Extension Design — Adding Dimensions After Single-Definition

Design notes for extending what a model provider declares: pricing, richer model capability
matrices, and account/tenant dimensions. Written to be executed **after**
[ADR-0027](../adr/0027-provider-definition-single-source-and-adapters-retirement.md) reaches
`accepted` and `[INV-PROV-06]` holds.

---

## 0. Precondition: why this cannot land first

Every dimension below is an *addition* to `ModelProviderSpec`. Today each provider is defined
**twice** (see ADR-0027), so an addition has no single home: put it in `nuo-provider-adapters` and it
hardens the monolith; put it in `providers/` and it joins unlinked, drifting code. `[INV-PROV-06]`
must therefore be true first, so that every addition has exactly one destination.

---

## 1. The placement rule (decides every dimension)

```text
Static fact          (compile-time, verifiable, not behaviour)  →  &'static table on ModelDecl / ProviderManifest
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
| **Provider health / recovery semantics** | **Already implemented** | `ProviderErrorKind` (11 variants, `#[non_exhaustive]`) + `RetryDisposition::{Never, Retry{retry_after_ms}}` in `nuo-model-codec/src/provider_error.rs`; `ProviderError::retryable`, `with_status`, `is_context_overflow`; classified in `nuo-provider-transport/src/transport.rs` (`retry_after_ms`), consumed by the harness retry loop. **Nothing to add.** |
| **Quota / balance / credit %** | **Data model done; dispatch is not** | `ProviderQuotaData::{Periodic, Balance, Composite}`, `QuotaWindowBucket.used_fraction`, `BalanceQuota.{credit_limit, consumed_amount, display_primary}` (`nuo-model-codec/src/connection_detail.rs`). The gap is *dispatch*, not shape — fixed by ADR-0027 step 4 (`[INV-PROV-08]` typed port). **Do not add a new quota schema.** |
| **Account / tenant selection** | **Deferred, with a condition** | Values already ride `ResolvedAuth`'s typed extensions (`OpencodeAuthMetadata.org_id`, `GoogleAuthMetadata.project_id`, `ChatGptAuthMetadata.account_id`). See §5. |
| **`ecosystem` label** | **Rejected** | ADR-0027 §Rejected Alternatives. |
| **`trust` level** | **Rejected** | ADR-0027 §Rejected Alternatives (the gate is the baseline-precedence rule, `nuo-harness/src/catalog/sync.rs:485`). |
| **`local_only_safe`** | **Rejected** | ADR-0027 §Rejected Alternatives (feature unimplemented; wrong shape). |

**Net: of the dimensions raised, two are real additions (§3, §4), one is a dispatch fix that ADR-0027
already owns, and four are correctly left out.** This is the expected outcome — most "missing"
provider fields turn out to be either already modelled or without a consumer.

---

## 3. Addition A — Model pricing (genuinely absent)

Nothing in the tree models cost. `price_factor` appears only in Qoder's test fixture
(`nuo-provider-adapters/tests/fixtures/qoder-model-list-1.1.58.json`) and is never parsed into a type.

**Shape** — static, per-model, additive on `ModelDecl`:

```rust
pub struct Pricing {
    /// Currency the amounts are denominated in (ISO 4217, e.g. "USD", "CNY").
    pub currency: Currency,
    /// Micro-units per 1M tokens, to avoid float drift. `None` = the provider states no price.
    pub input_per_mtok: Option<u64>,
    pub output_per_mtok: Option<u64>,
    pub cache_read_per_mtok: Option<u64>,
    pub cache_write_per_mtok: Option<u64>,
    /// Relative cost multiplier when the provider prices in abstract credits rather than
    /// currency (Qoder's `price_factor`). Mutually exclusive with the per-token fields.
    pub relative_factor: Option<f32>,
}
```

Design constraints:

- **Two mutually exclusive accounting modes.** A provider either prices in real currency
  (`*_per_mtok`) or in abstract credits (`relative_factor`). Modelling both at once invites summing
  incomparable numbers. Enforce with an enum, not four `Option`s plus a flag.
- **Integer micro-units, never `f64`, for money.** `f64` accumulates error across a session's
  turns; a cost meter is exactly where that shows up.
- **`None` means "not stated", never "free".** Same discipline as `vision` (`[INV-PROV-11]`): an
  unpriced route must not render as `$0.00`.
- **Live prices are a runtime fact.** A provider that advertises pricing in its catalog belongs in
  `RemoteModelMetadata` as an overlay, layered above the static table under the same resolution order
  as every other capability. Do not cache live prices into the static table.

**Consumer required before landing** (`[INV-PROV-07]`): a cost meter or a "cheapest capable model"
selector. Absent one, this stays unbuilt.

---

## 4. Addition B — Model capability matrix (too narrow)

`Model` (`nuo-model-codec/src/model.rs:24`) carries 8 fields. Resolution of what is genuinely needed:

**Add (each has a clear consumer):**

| Field | Consumer |
| :--- | :--- |
| `streaming: bool` | Gate the streaming path; today it is assumed for every routed channel. |
| `structured_output: bool` | Gate JSON-schema / response-format stamping rather than sending it blind. |
| `parallel_tool_calls: bool` | The Responses builder already stamps `parallel_tool_calls: true` unconditionally (`nuo-provider-openai/src/responses/request.rs:226`) — that must become conditional per model. |
| `max_images: Option<u8>` | Boundary-check attachments before send, instead of relying on an upstream 400. |

**Do not add yet (no consumer):** audio/PDF input modalities, output modalities, per-model TPM/RPM
(that is rate-limit data, which belongs with quota — §2, not on the capability record).

**Discipline:** every new field is added to `RemoteModelMetadata` as an **`Option`** in the same
change, so a trusted provider can advertise it and the three-layer resolution (user > remote >
baseline, `ADR-0149`) applies uniformly. A static-only field would be unreachable by any provider
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
// ProviderManifest gains:  pub account_dimensions: &'static [AccountDimension],
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
