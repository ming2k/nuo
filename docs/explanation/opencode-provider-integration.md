# OpenCode Provider Integration

This article explains how the two active OpenCode service surfaces (`opencode-go` and `opencode-zen`) are modeled as distinct Nuo providers, and why the subscription relay's catalog requires a compiled capability baseline.

---

## 1. Two Active Service Surfaces, Two Dedicated Crates

OpenCode integrations are partitioned into two dedicated channel crates per ADR-0015 and ADR-0079:

| Provider id | Channel Crate | Surface | Auth | Catalog shape |
| :--- | :--- | :--- | :--- | :--- |
| `opencode-go` | `providers/nuo-provider-opencode-go/` | Zen Go relay (`opencode.ai/zen/go/v1`) | OpenCode Go API key (`OPENCODE_API_KEY`) | `CatalogShape::OpenAi` |
| `opencode-zen` | `providers/nuo-provider-opencode-zen/` | Zen relay (`opencode.ai/zen/v1`) | OpenCode API key / Public Keyless (`OPENCODE_API_KEY`) | `CatalogShape::OpenAi` |

The legacy `opencode` Console provider and its `CatalogShape::OpencodeConsole` parser were retired outright (ADR-0079) as part of tech debt elimination, and the umbrella `nuo-provider-opencode` crate was bifurcated into dedicated channel crates.

They are separate crates and providers because the endpoint family and model universe differ. Per [ADR-0015](../adr/0015-dependency-inversion-architecture-for-model-providers.md), the classification unit is the concrete access channel/plan, never the corporate brand; authentication mode never selects the route (`[INV-AUTH-04]`).

---

## 2. Route Per Model by Wire Format

The two **Zen** surfaces expose a public catalog (`/zen/go/v1/models` for the plan/Go relay, `/zen/v1/models` for the Zen relay) and route per model by wire format (`route_for_model`); the instance default is the OpenAI-chat-completions surface for Zen, and the portable Responses surface for Go (ADR-0142).

### 2.1 The Go Surface Is a Translating Gateway

The Go relay mounts `/zen/go/v1/responses` and serves its *whole* fleet through it, not only the models whose native API it is: Console translates each request onto whatever the upstream speaks. That makes it a different kind of endpoint from a native Responses backend, and the provider declares it as `ProviderDialect::Portable` (ADR-0142):

- **Flattened tools.** The gateway validates the OpenAI tool shape; DeepSeek's nested `{type,function:{…}}` is answered with `400 … requires a native Responses target`.
- **No state.** `store` is never sent and `previous_response_id` is never used — both answer `400`, and the vendor calls the mode "portable" because the full conversation must ride along every turn. The spec therefore declares `ResponsesState::Stateless` (ADR-0114).
- **Dual-form reasoning replay.** The upstreams behind the one URL enforce opposite replay rules: one rejects an item without a `summary` array, another rejects a turn whose raw chain was dropped. Every reconstructed reasoning item carries both `summary` and `content` parts, and a pruned older turn carries an empty `summary` array with no `content` key.

Anthropic-routed (Claude, MiniMax, Qwen) and Google-routed (Gemini) models keep their own dedicated `/messages` and `/v1beta` routes via `protocol_roots`.

---

## 3. Where Capability Metadata Comes From

Intrinsic capability metadata (context window, reasoning support, tool calling, vision, effort ladder) is never duplicated per provider. It resolves through the shared model registry (`nuo-provider::model`), with a compiled baseline as the fallback when upstream advertises nothing.

---

## 4. The Subscription Relay

### 4.1 Authentication

All OpenCode surfaces authenticate with a plain API key (or public keyless for free models) and serve a public `/models` catalog:
- `opencode-go` authenticates with an OpenCode Go subscription API key (`oc_sk_...`) and automatically injects the mandatory `x-opencode-session` header.
- `opencode-zen` authenticates with `OPENCODE_API_KEY` (or `Authorization: Bearer public` for promotional free tier models).

OpenCode's legacy Console OAuth device grant (which targeted internal enterprise console SSO organizations rather than personal coding plans) was retired to eliminate broken device authentication flows.

### 4.2 An Ids-Only Catalog: Baseline, Labels, and Noise

The **Go relay's `/models` payload carries ids only** — no capabilities and no human
labels. That single fact determines three separate answers, all of which live inside the
catalog boundary (`nuo-model-routing`, ADR-0082):

**Capabilities come from the compiled baseline.** Because the catalog publishes no
capability fields, the **compiled baseline in `nuo-provider-opencode-go` is the only
capability source** for the models Nuo intends to present as first-class:

- **Every id the relay serves that Nuo wants to expose needs a baseline entry here.** An
  id with no entry resolves to the conservative fallback (the 128k / no-effort anonymous
  model), which silently strips the effort control and under-reports the context window in
  the pickers.
- **The upstream endpoint table is the authority for what exists.** The Go endpoint table
  (`opencode.ai/docs/go`) is consulted; ids it has dropped — such as the bare
  `deepseek-flash` residue of the V4.1-Flash rename — are deliberately **not** registered
  *in this relay's table*. (`deepseek-flash` is the *official* `api.deepseek.com` id and is
  registered by `nuo-provider-deepseek`; the relay simply does not serve it — ADR-0083.)
- **A retired id stays unregistered.** A relay that still leaks a retired id surfaces it
  capability-less (the conservative fallback) rather than via a hand-maintained shadow
  entry. The registry does not curate aliases for spellings upstream dropped; the compiled
  baseline never outlives its source.

For example, every DeepSeek id on the Go relay carries the platform's accepted reasoning
vocabulary (`none`/`minimal`/`low`/`medium`/`high`/`xhigh`/`max`/`ultra` — the relay forwards
`reasoning_effort` to the same backend, so the ladder is the model's, not the relay's) and a
1M context window, so a discovered DeepSeek id resolves to a capability-carrying model
rather than the anonymous fallback. Rows that another provider also declares must match it
field-for-field: the capability registry is keyed by model id, not by route (ADR-0083).

**Labels are lifted structurally, never dictionary-mapped.** `opencode-go` and
`opencode-zen` declare `DisplayPolicy::Structural`, so a bare id becomes polished
typography by rule — `deepseek-v4.1-flash` → `DeepSeek V4.1 Flash`. A model added upstream
therefore needs no Nuo release. The mapping runs once in
`nuo_model_routing::derive_channel`; projections and clients consume the resolved label and never
re-derive it, and a label that would read identical to the wire id is dropped so the
surfaces fall back to the id rather than echoing it.

**Discovered noise is pruned by structural markers.** The `Curated` pipe valve filters
ephemeral discovery (`-free`, `-preview`, `-contributor`, `alpha-`) out of the *upstream*
listing, because a relay's catalog is a dumping ground. Two boundaries keep this honest:
ids the surface itself seeds are never filtered (the compiled baseline and the surface must
agree — `opencode-go` deliberately seeds two `-free` tiers), and anything the operator
explicitly includes bypasses the valve unconditionally.

A vendor's featured production model is not noise, and there is no per-id exception list —
that is the brittle blacklist ADR-0068 abolished.

---

## 5. Related

- [ADR-0015](../adr/0015-dependency-inversion-architecture-for-model-providers.md): the provider contract crate and channel namespace.
- [ADR-0079](../adr/0079-opencode-provider-channel-bifurcation-and-legacy-container-retirement.md): provider channel bifurcation and legacy container retirement.
- [ADR-0081](../adr/0081-unidirectional-catalog-pipeline-structural-normalization-and-relay-noise-hygiene.md): the catalog pipeline, structural labels, and relay noise hygiene.
- [ADR-0082](../adr/0082-single-catalog-pipeline-definition-in-a-dedicated-layer2-crate.md): the single `nuo-model-routing` crate that owns the pipeline for both the harness and the server.
- [Reasoning Effort Reference](../reference/effort.md): the effort ladder model that these baselines populate.
