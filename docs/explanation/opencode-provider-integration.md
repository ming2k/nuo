# OpenCode Provider Integration

This article explains how the two active OpenCode service surfaces (`opencode-plan` and `opencode-zen`) are modeled as distinct Nuo providers, and why the subscription relay's catalog requires a compiled capability baseline.

---

## 1. Two Active Service Surfaces, Two Dedicated Crates

OpenCode integrations are partitioned into two dedicated channel crates per ADR-0015 and ADR-0079:

| Provider id | Channel Crate | Surface | Auth | Catalog shape |
| :--- | :--- | :--- | :--- | :--- |
| `opencode-plan` | `providers/nuo-provider-opencode-plan/` | Zen Go relay (`opencode.ai/zen/go/v1`) | OpenCode Go API key (`OPENCODE_API_KEY`) | `CatalogShape::OpenAi` |
| `opencode-zen` | `providers/nuo-provider-opencode-zen/` | Zen relay (`opencode.ai/zen/v1`) | OpenCode API key / Public Keyless (`OPENCODE_API_KEY`) | `CatalogShape::OpenAi` |

The legacy `opencode` Console provider and its `CatalogShape::OpencodeConsole` parser were retired outright (ADR-0079) as part of tech debt elimination, and the umbrella `nuo-provider-opencode` crate was bifurcated into dedicated channel crates.

They are separate crates and providers because the endpoint family and model universe differ. Per [ADR-0015](../adr/0015-dependency-inversion-architecture-for-model-providers.md), the classification unit is the concrete access channel/plan, never the corporate brand; authentication mode never selects the route (`[INV-AUTH-04]`).

---

## 2. Route Per Model by Wire Format

The two **Zen** surfaces expose a public catalog (`/zen/go/v1/models` for the plan/Go relay, `/zen/v1/models` for the Zen relay) and route per model by wire format (`route_for_model`); the instance default is the OpenAI chat-completions surface.

---

## 3. Where Capability Metadata Comes From

Intrinsic capability metadata (context window, reasoning support, tool calling, vision, effort ladder) is never duplicated per provider. It resolves through the shared model registry (`nuo-model-codec::model`), with a compiled baseline as the fallback when upstream advertises nothing.

---

## 4. The Subscription Relay

### 4.1 Authentication

All OpenCode surfaces authenticate with a plain API key (or public keyless for free models) and serve a public `/models` catalog:
- `opencode-plan` authenticates with an OpenCode Go subscription API key (`oc_sk_...`) and automatically injects the mandatory `x-opencode-session` header.
- `opencode-zen` authenticates with `OPENCODE_API_KEY` (or `Authorization: Bearer public` for promotional free tier models).

OpenCode's legacy Console OAuth device grant (which targeted internal enterprise console SSO organizations rather than personal coding plans) was retired to eliminate broken device authentication flows.

### 4.2 Ids-Only Catalog and the Compiled Baseline

The **Go relay's `/models` payload carries ids only** — no capability fields. This is the pivotal fact for the subscription surface:

> Because the catalog publishes no capabilities, the **compiled baseline in `nuo-provider-opencode-plan` is the only capability source** for the models Nuo intends to present as first-class.

The consequences are precise:

- **Every id the relay serves that Nuo wants to expose needs a baseline entry here.** An id with no entry resolves to the conservative fallback (the 128k / no-effort anonymous model), which silently strips the effort control and under-reports the context window in the pickers.
- **The upstream endpoint table is the authority for what exists.** The Go endpoint table (`opencode.ai/docs/go`) is consulted; ids it has dropped — such as the bare `deepseek-flash` residue of the V4.1-Flash rename — are deliberately **not** registered.
- **A retired id stays unregistered.** A relay that still leaks a retired id surfaces it capability-less (the conservative fallback) rather than via a hand-maintained shadow entry. The registry does not curate aliases for spellings upstream dropped; the compiled baseline never outlives its source.

For example, every DeepSeek id on the Go relay carries the `low`/`high`/`max` ladder and a 1M context window, so a discovered DeepSeek id resolves to a capability-carrying model rather than the anonymous fallback.

---

## 5. Related

- [ADR-0015](../adr/0015-dependency-inversion-architecture-for-model-providers.md): the provider contract crate and channel namespace.
- [ADR-0079](../adr/0079-opencode-provider-channel-bifurcation-and-legacy-container-retirement.md): provider channel bifurcation and legacy container retirement.
- [Reasoning Effort Reference](../reference/effort.md): the effort ladder model that these baselines populate.
