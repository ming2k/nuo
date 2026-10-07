# Provider Consolidation Migration Runbook

Operational runbook for executing **[ADR-0027](../adr/0027-provider-definition-single-source-and-adapters-retirement.md)**:
making `providers/nuo-provider-*` the single authoritative provider definition and retiring
`nuo-provider-adapters`.

> **Status of this document.** The equivalence classes below were established by a read-only audit
> (2026-10-08) and are **sampled**, not exhaustive. Step 0 is a blocking re-verification in the build
> environment. Do not skip it.
>
> **No backward-compatibility requirement.** This migration keeps **no** re-export shim, no alias
> crate, and no transitional re-export facade. `nuo-provider-adapters` is removed outright at the end;
> every reference is repointed to its new owner in the same change. If you find yourself adding a
> `pub use` "so the old path keeps working", that is out of scope — delete the old path instead.

---

## Execution Record (2026-10-06) — **Executed**

This runbook was executed in full; `nuo-provider-adapters` no longer exists and every
`providers/nuo-provider-*` crate is linked by the shipped build. This section is the
durable record that replaces the former `provider-consolidation-handoff.md` brief (folded
here per that brief's §9).

**Delivered:**

- `nuo-oauth` (workspace root) — the **single OAuth crate**: the RFC primitives
  (`nuo_oauth::oauth`: PKCE RFC 7636, device flow RFC 8628, loopback listener, token
  endpoint, JWT helpers) **and** the vendor-agnostic engine (stateful login sessions, the
  per-connection `OAuthCredentialSource`, the `OAuthTokenEnricher` port, and the
  `OAuthProvider` port + registry). It links only `nuo-provider` (credential contract),
  `nuo-model-codec`, and `nuo-provider-transport` (HTTP) — **no vendor crate**. It sits at
  the root because OAuth is a cross-cutting authentication concern, not a provider
  definition.
- Each vendor crate owns its OAuth surface and implements `OAuthProvider`:
  - `nuo-provider-google-antigravity` → Antigravity + Antigravity-CLI configs, enricher, and the
    Cloud-Code onboarding helpers (relocated out of transport).
  - `nuo-provider-xai` / `nuo-provider-copilot` → RFC 8628 configs.
  - `nuo-provider-chatgpt-plan` → ChatGPT config, custom device grant, `account_id`
    enrichment/projection, and `chatgpt_account_id` (relocated out of transport).
  - `nuo-provider-opencode` → OpenCode config, JSON device grant, account/org enrichment.
  - `nuo-provider-qoder` → Qoder/Qoder-CN configs, device session, `drt-` refresh,
    uid/endpoint repair, identity projection.
  `nuo-server::provider_registry::init()` registers them all — the only crate that knows
  every vendor (ADR-0015).
- `nuo-provider-transport` is now **pure wire substrate** (HTTP egress, SSE demuxing,
  request pipeline, endpoint/client identity, prompt cache, vision projection); its
  `oauth` module was folded into `nuo-oauth`, and the vendor presets/onboarding helpers
  moved to their owning provider crates. Every provider still links it for HTTP.
- `providers/nuo-provider-catalog` — `list_models.rs` verbatim, with `CatalogShape`
  parsers delegating to `nuo-provider-opencode::console` and `nuo-provider-qoder`.
- `providers/nuo-provider-siliconflow` — the adapters-only SiliconFlow fetcher.
- `nuo-provider-google-antigravity` — the relocated `retrieve_antigravity_quota_summary` orphan.
- `nuo-server::provider_registry` — the composition root: `MODEL_PROVIDER_SPECS`
  (aggregated from each provider crate's `MODEL_PROVIDER_SPEC`), `model_provider_spec`,
  `route_for_model`, `sync_user_declared_providers`, `build_provider_for_channel`,
  credential-source construction, catalog discovery + signer registration, and `init()`.
  Both `nuo/src/main.rs` and `nuo-server/src/bootstrap.rs` call it. The cross-provider
  baseline tests and `custom_baselines` moved here too (they require all provider crates
  linked, so the composition root is their correct "shared baseline home").
- **Typed quota dispatch (`[INV-PROV-08]`)** — `ModelProviderSpec` gained
  `quota: Option<QuotaPort>`; `QuotaPort` is declared per provider spec and
  `fetch_provider_usage` dispatches on it with **zero** provider-id / base-URL substring
  matching. A regression test asserts a `kimi-code` connection resolves `KimiBalance`.
- Dead abstractions `ProviderDescriptor`, `ProviderRegistry`, and
  `builtin_provider_metadata` were deleted (zero production consumers — `[INV-PROV-07]`).

**Corrections applied to this runbook's original plan:**

- The composition root is `nuo-server` (ADR-0027), not a new crate; the `nuo` CLI reaches
  it through its existing `nuo-server` dependency. `nuo-harness` and `nuo-tui` use
  test-only dev-dependencies on `nuo-server` so the inventory baseline registry is linked
  in their test binaries (`[INV-PROV-04]` remains satisfied for the shipped build).
- The engine is **vendor-agnostic** (shape B in the folded brief's §5.4): vendor configs,
  device grants, enrichers, refresh, and repair live in the owning provider crates and
  register through `OAuthProvider`. There is no `token` wrapper module and **no
  engine→vendor edge**; `cargo tree -p nuo-oauth` shows only `nuo-provider`,
  `nuo-provider-transport`, and `nuo-model-codec`.
- `retrieve_antigravity_quota_summary` went to its owning provider crate, not the engine.
- Adapter tests/examples were re-homed as follows (the composition root is the one crate
  that can reach every owner at once):
  - `qoder_wire_golden.rs` + `qoder_catalog_contract.rs` + fixtures →
    `providers/nuo-provider-qoder/tests/`.
  - `wire.rs` + `integration.rs` → `nuo-server/tests/`; `examples/*` → `nuo-server/examples/`.
- **Kimi defect caveat (§5.6 of the folded brief):** the shipped (adapters) fetcher body is
  carried over verbatim. With typed dispatch the `kimi-code` spec now resolves
  `KimiBalance`, so the connection attempts the shipped `api.moonshot.cn` host instead of
  returning a clean `Unsupported`. This is the single deliberate consequence of replacing
  string-sniffing dispatch; the balance endpoint for `api.kimi.com` still requires a
  live-verified capture before any behaviour fix, per ADR-0027 §Errata.

> **Residual, unrelated:** `nuo-tui::disclosure::renderers::payloads` has one failing
> unit test introduced by separate in-progress work (`normalize_code_content_strips_lines_
> framing_and_embedded_numbers`); it is independent of this migration.

---

## 0. Blocking precondition — re-verify equivalence on the full tree

The audit compared ~20 file pairs byte-for-byte plus a tree-wide symbol scan. Before moving any
code, confirm the equivalence classes hold for **every** file:

```bash
for f in $(cd nuo-provider-adapters/src/protocol && find . -name '*.rs'); do
  a="nuo-provider-adapters/src/protocol/$f"
  p="providers/nuo-provider-$(echo "$f" | cut -d/ -f2)/src/$(echo "$f" | cut -d/ -f3-)"
  [ -f "$p" ] && { diff -q "$a" "$p" >/dev/null || echo "DRIFT: $a <-> $p"; }
done
diff -ru nuo-provider-adapters/src/usage providers/ 2>/dev/null | head
```

**Gate:** anything reported `DRIFT` beyond `crate::X` ↔ `nuo_provider_transport::X` import rewrites is
escalated per-file before proceeding — the `providers/`-authoritative rule is then applied case by
case, not in bulk.

**Known exception (do not "fix" during migration):** the `kimi` usage fetcher differs in *both*
predicate and endpoint, and **neither copy is correct** (ADR-0027 §Context). Carry the shipped
behaviour over verbatim; fix it separately against a live-verified endpoint.

---

## 1. Create the OAuth engine crate

> **Superseded by the Execution Record above.** The OAuth engine is the workspace-root
> `nuo-oauth` crate and is vendor-agnostic; the per-vendor details below were the original
> plan and are retained only as history. Do not follow them literally.

```bash
cargo new --lib nuo-oauth
```

Move (do not copy) from `nuo-provider-adapters/src/oauth/`:

| File | Why it belongs here |
| :--- | :--- |
| `mod.rs` (the `OAuth`, `OAuthLoginSession`, `BrowserLogin`, `build_token_set_from_login` engine) | Vendor-workflow layer over the RFC primitives |
| `credential_source.rs` (`OAuthCredentialSource`, `force_refresh_after_rejection`) | Binds engine → `CredentialSource` port |
| `enricher.rs` (`OAuthTokenEnricher` family) | Per-vendor token post-processing |
| `manual.rs` (`parse_authorization_response`) | Manual-flow helper |
| `token.rs::retrieve_antigravity_quota_summary` | `[INV-PROV-07]` orphan — no other home |

`{device_identity,host,store}.rs` are pure re-export facades of `nuo_provider::credentials::*`;
delete them rather than moving them.

**The device-flow modules split by owner, not by name:**

- `oauth/chatgpt_device.rs` ≡ `providers/nuo-provider-chatgpt-plan/src/device.rs` (byte-identical) →
  **delete the adapters copy**; the owners are the provider crates.
- `oauth/opencode_device.rs` ≡ `providers/nuo-provider-opencode/src/device.rs` (byte-identical) →
  **delete the adapters copy**.

Do **not** move these into `nuo-oauth` — that would make the engine own two vendors'
device flows and re-create the coupling this migration removes.

`Cargo.toml` deps: `nuo-provider`, `nuo-provider-transport`, `nuo-model-codec`, `nuo-host`,
`nuo-wire`, `async-trait`, `serde`, `serde_json`.

> **Circular-dependency guard — the one real edge (verified 2026-10-08).**
> Only **one** import reaches a vendor crate from the OAuth engine:
> `enricher.rs:15` → `crate::registry::qoder::{QoderStoredIdentity, generate_machine_key_hex}`.
> (`credential_source.rs:7`'s `config_by_provider_id` comes from `nuo-provider-transport`'s
> vendor-agnostic `presets` — `oauth → transport` is a DAG, not a cycle. Do not "fix" it.)
>
> So the edge to resolve is `nuo-oauth → nuo-provider-qoder`. Preferred shape: **register,
> don't import** — the engine exposes an enricher registry; `nuo-provider-qoder` registers its
> enricher and keeps the two symbols. Verify no reverse edge with
> `cargo tree -p nuo-provider-qoder` after the move.

## 2. Create the catalog crate

```bash
cargo new --lib providers/nuo-provider-catalog
```

Move `nuo-provider-adapters/src/list_models.rs` here in full (`CatalogParser` trait, the 6 shape
parsers, `models_endpoint_for`, `list_models`, `fetch_remote_catalog`, `validate_catalog_shape`).

**Dependencies this crate actually needs** (verify by compiling, but expect both):
`nuo-provider-qoder` (for `parse_scene_catalog`), and `nuo-provider-opencode` (for
`console::parse_config_catalog`, called by the OpenCode Console parser). Register the
`CatalogDiscovery` impl here.

## 3. Relocate remaining orphans

- `usage/siliconflow.rs` → new `providers/nuo-provider-siliconflow` (no counterpart exists anywhere;
  deleting it removes a live fetcher).
- `registry/custom_baselines.rs` → the provider crate owning those ids, or the shared baseline home.
- `registry/baseline_fidelity_tests.rs` → alongside the `providers/` baselines it validates.

## 4. Replace string-sniffing with typed binding (`[INV-PROV-08]`)

Replace `ProviderUsageFetcher::matches(provider: &str, base_url: &str)` dispatch with a typed port
declared on the provider spec:

```rust
pub enum QuotaPort { DeepSeekBalance, KimiBalance, CommandCodeCredits, OpenRouterKey, Antigravity, SiliconFlow }
// ModelProviderSpec gains:  pub quota: Option<QuotaPort>,
```

`fetch_provider_usage` resolves the port from the spec instead of scanning `registered_fetchers()`
with substring tests. **This removes the `kimi`-class bug's mechanism**, and it is also what makes the
`providers/` fetchers usable at all — they are **inherent `impl` blocks, not
`impl ProviderUsageFetcher`**, so nothing can call them until this dispatch exists. Add a regression
test asserting a `kimi-code` connection resolves its port.

**This is the only step that changes runtime behaviour — land it in its own commit.**

## 5. Move the composition root

`init()` has **two** live call sites; both are the composition root and both must be handled:

- `nuo/src/main.rs:21` — the CLI entry (primary composition root).
- `nuo-server/src/bootstrap.rs:157` — the server's `assemble()`.

Relocate into a single `init()` owned by the composition root:

- `registry/mod.rs::MODEL_PROVIDER_SPECS` → assembled from each `providers/*` crate's
  `MODEL_PROVIDER_SPEC`.
- `lib.rs::init()` → the composition root; `nuo/src/main.rs` and `nuo-server/src/bootstrap.rs`
  call it.
- `registry/mod.rs::build_provider_for_channel` → the composition root, dispatching on `Transport`.
- `lib.rs`'s `AdaptersProviderFactory` / `AdaptersCatalogDiscovery` registrations → the same site.

> Because ADR-0015 forbids `nuo-harness` from linking concrete providers, `init()` must **not** move
> into `nuo-harness`. The harness keeps receiving its driver through the holder.

## 6. Re-home the tests and examples (do not skip)

`nuo-provider-adapters/tests/` and `examples/` reference **internal paths that are being split across
crates**, so this is re-homing, not renaming:

| Path | What it needs |
| :--- | :--- |
| `tests/qoder_wire_golden.rs` (guards `[INV-WIRE-01]`) | Wire-golden for qoder's pipeline → relocate to `providers/nuo-provider-qoder/tests/` |
| `tests/it/qoder_catalog_contract.rs` | `qoder::surface` + `parse_scene_catalog` → `providers/nuo-provider-qoder/tests/` |
| `tests/it/wire.rs` | Mixed: `build_provider_for_channel` (composition root), `oauth::*` (OAuth engine), `protocol::anthropic::request::ANTHROPIC_VERSION` (provider crate) → **split by owner** |
| `tests/integration.rs`, `tests/fixtures/qoder-model-list-*.json` | fixtures follow `qoder_catalog_contract` |
| `examples/{commandcode,qianwen,qoder}_live_smoke.rs`, `examples/qoder_catalog_dump.rs` | Move next to the entry points they exercise; rewire imports to `nuo_provider_<x>::` |

For each file, resolve every `nuo_provider_adapters::` path to its new owner **before** deleting the
old file. A test that cannot find its owner is a signal that the relocation map is incomplete — stop
and fix the map, do not delete the test.

## 7. Delete duplicates (no shim)

Delete from `nuo-provider-adapters/src/`:
`protocol/{openai,anthropic,google}/**`, `registry/*.rs` (specs + `effort_ladders` + `mod.rs`),
`usage/*.rs`, `client.rs`, `endpoint.rs`, `oauth/{chatgpt_device,opencode_device}.rs`.

**Do not add `pub use nuo_provider_x::*;` re-exports to keep old paths alive.** There is no
backward-compatibility requirement; repoint call sites instead (step 8).

## 8. Resolve the dead abstractions (no third state)

For each of `ProviderDescriptor`, `ProviderRegistry` (all methods), `builtin_provider_metadata`:
**wire to a real consumer or delete**. Deleting is the expected outcome for all three —
`model_provider_label` already covers the live label need.

## 9. Repoint **every** reference, then delete the crate

The manifest-only search is **not sufficient** — the overwhelming majority of references are Rust
paths. Enumerate all of them:

```bash
# BOTH manifests and sources:
rg -l 'nuo-provider-adapters|nuo_provider_adapters' --glob '!target/**'
```

Expected call sites (verify, do not trust this list as complete):

- Manifests: `nuo/Cargo.toml:23`, `nuo-server/Cargo.toml:15`, `nuo-tui/Cargo.toml:32`,
  `nuo-harness/Cargo.toml:46` (dev-dep), root `Cargo.toml` members + `[workspace.dependencies]`.
- `nuo/src/main.rs:21` (`init`), `nuo-server/src/bootstrap.rs:157` (`init`).
- `nuo-server/src/handlers_provider.rs` (~20 sites: `model_provider_spec`, `sync_user_declared_providers`,
  `oauth::*`, `fetch_provider_usage`, `RemoteCatalogSource`).
- `nuo-server/src/credentials_host.rs:17,31` (`CredentialHost`, `CredentialStore`).
- `nuo-server/src/session_driver.rs:523` (`oauth::TokenSet`).
- `nuo-harness/src/catalog/tests.rs:5,39,59`, `nuo-server/src/catalog/tests.rs:5,39` (`extern crate` + `init`).
- `nuo-tui/src/providers.rs:891` (`extern crate`, test-only).

Then:

```bash
cargo build --workspace        # must be clean BEFORE deletion
rm -rf nuo-provider-adapters
cargo build --workspace        # must still be clean
```

`[INV-PROV-09]` check: after deletion, assert the shipped binary actually links the
`providers/nuo-provider-*` crates (e.g. `cargo tree -p nuo | rg 'nuo-provider-(openai|anthropic|google)'`).

## 10. Doc sweep

These are comments/docs, not compile errors, but they become **false statements**:

- `nuo-model-codec/src/catalog.rs:12,244`; `nuo-model-codec/src/effort.rs:44,50,413`;
  `nuo-model-codec/src/model.rs:736`
- `nuo-provider/src/factory.rs:46` (**a panic message that names the retired crate**)
- `nuo-provider/README.md:14`, `nuo-server/src/lib.rs:46`, `nuo-server/src/credentials_host.rs:4`
- `nuo-tui/src/providers.rs:57`
- `nuo-provider-transport/src/oauth/presets.rs:3`,
  `providers/nuo-provider-commandcode-plan/src/usage.rs:8`,
  `providers/nuo-provider-qoder/src/lib.rs:503`
- Remove `nuo-provider-adapters/README.md`; its subsystem prose is already mirrored in
  `docs/architecture/subsystems.md`.

## 11. Governance sync (same commit)

- `docs/architecture/subsystems.md` §2.3: remove the "pending retirement" marker; the ADR-0027
  registry row flips to Accepted.
- Flip ADR-0027 `status: draft` → `accepted`; update `docs/adr/index.md`.
- `cargo test --workspace && docgov check`

---

## Verification gates (per step)

| After step | Gate |
| :--- | :--- |
| 0 | Zero unexplained `DRIFT` |
| 1–3 | `cargo check -p <new crate>`; new crates compile standalone |
| 4 | Regression test: `kimi-code` resolves a quota port |
| 5 | `cargo build --workspace`; both `init()` call sites resolve to the new owner |
| 6 | `rg 'nuo_provider_adapters' nuo-provider-adapters/tests nuo-provider-adapters/examples` finds nothing before those files are removed |
| 7–8 | `rg` finds no reference to the deleted abstraction names |
| 9 | `cargo build --workspace` clean **before** and **after** `rm -rf`; `[INV-PROV-09]` link check |
| 10–11 | `docgov check` + full suite green |

## Rollback

Steps 1–3 and 6–8 are pure moves/deletions — `git checkout -- <paths>` per step. **Step 4 changes
runtime behaviour** and must land alone so it can be reverted alone. Step 9 is the point of no return
for the crate name; everything before it is reversible.
