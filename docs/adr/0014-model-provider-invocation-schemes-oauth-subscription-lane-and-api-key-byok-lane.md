---
id: ADR-0014
title: "Model Provider Invocation Schemes: Local Direct, and the Subscription Server-Proxy Lane"
status: accepted
date: 2026-10-03
scope: providers/auth, protocol/wire, model/providers, persistence/connections, server/proxy
superseded_by: null
negative_knowledge: true
---

# 0014. Model Provider Invocation Schemes: Local Direct, and the Subscription Server-Proxy Lane

- Status: Accepted
- Date: 2026-10-03
- Deciders: Nuo Architecture Working Group
- Consulted: Runtime, Capability, Interface Teams, and Billing/Entitlement Engineering
- Informed: System Architects
- Complements: [ADR-0001](0001-flat-workspace-and-microkernel-capability-topology.md), [ADR-0008](0008-single-tool-contract.md), [ADR-0011](0011-nuo-tui-presentation-and-nuo-server-container.md), [ADR-0013](0013-decoupled-tool-namespace-and-infrastructure-purity.md)
- Verification: Wire contract live-probed against `api.commandcode.ai` on 2026-10-04 with a real `individual-goat` subscription key (`whoami`/`billing/credits`/`usage/summary`/`alpha/generate`/`alpha/web-search`, premium denial `MODEL_NOT_IN_PLAN`, opensource allow `Kimi-K2.5@bedrock`, `gpt-5.6-sol@openai`, `deepseek-v4.1-flash@novita`).

---

## Context and Problem Statement

Reverse engineering `command-code` v1.74.1 and the unpacked `@byokkit/cmd-provider-{anthropic,openai,copilot}` modules, plus live probing of its production API with a valid subscription key, established **three distinct ways a model call reaches a model host**. They are of a different architectural kind — collapsing them into one "auth mode" flag is the classic modeling error:

1. **Local Direct Lane (LLD)**: The client resolves credentials (static API key,或在 OAuth 订阅 token) and issues the vendor-protocol request itself, straight to the vendor host (`api.anthropic.com`, `chatgpt.com/backend-api/codex`, `api.githubcopilot.com`, OpenAI-compatible relays).
2. **Subscription Server-Proxy Lane (SSPL)**: The client sends a vendor-agnostic envelope to **our own** control plane (`api.commandcode.ai/alpha/generate`, SSE); the **server** holds vendor credentials (`"gateway"` provider metadata: bedrock / openai / novita / …), routes, and bills subscription credits. The client has **no** model-host credential at all and never sees vendor headers beyond the resolved `providerMetadata`.
3. **BYOK passthrough** (LANE of LANE, not its own): a user-declared provider entry inside either lane above; in LSSPL it is declared in `x-oss-primary-provider` and monitored via `zai/oss`, still billing via Generative Agent Harness Subscription credits but using the user's own key.

Live GOAT (`individual-goat`) probe confirmed the shape: monthly 70 credits, `windowLimits{fiveHour:cap 14, weekly:cap 35}` recomputed per-call, model allow/deny enforced **server-side** with `MODEL_NOT_IN_PLAN: <model> available in Pro and above plans or extra on demand usage`, and `/alpha/generate` upstream model is **agent-decided and rewritten** on the server (`deepseek/deepseek-v4-flash` was served as `deepseek-v4.1-flash` via `novita`; `moonshotai/Kimi-K2.5` via `bedrock`; `gpt-5.6-sol` via `openai`).

Nuo's existing code already models lanes 1 and 2 as enum-tagged `ConnectionAuth::{ApiKey, Subscription{provider}}` over trait-object `CredentialSource`. This ADR ratifies both lanes, codifies the SSPL wire contract as a **future DialectSurface** so a Nuo-native server-proxy tier can be added without re-shaping the outbound request builders, and locks the invariants that keep the lanes orthogonal.

---

## Decision Drivers

- **Plan gate is server-enforced, never client-enforced**: probe proved `claude-sonnet-4-6` and `gpt-5.4` are rejected with `403 MODEL_NOT_IN_PLAN` against a GOAT key even though a picker could render them; `claude-sonnet-5-5` (opensource+premium allowed for GOAT) succeeds but is actually served by a different upstream (`claude-sonnet-5-5` → confirmed `providerMetadata.gateway` route table) — client-side gating is only a UX hint.
- **Upstream model rewrites are normal**: `/alpha/generate` may resolve any declared id to a differentuator family (`deepseek-v4-flash` → `deepseek-v4.1-flash`). A client must treat the returned `providerMetadata.{gateway|openai|novita|...}.routing.planningReasoning` as the **authoritative provenance record**, never assume 1:1.
- **ZDR is honored without a separate protocol**: `x-cmd-zdr: 1` produced `no prompt training` markers in planning reasoning and succeeded against a ZDR-admitting provider (`novita`); no distinct endpoint, only the request-scoped header.
- **Zero model-host credential on ASPL**: client headers carry only the Command Code bearer; the vendor key never touches the client. This is a deliberate complement to the `@ai-sdk/anthropic`-style local-direct lane, not a duplicate.
- **ENTITLEMENT analog is already first-class in Nuo** (`Availability`, `RemoteCatalogSource`, `user_declared_provider_spec`); the subscription lane slots in as `ConnectionAuth::Subscription{provider:"commandcode-lane"}` with no new top-level crate.

---

## Considered Options

- Option 1: Single dispatch flag `isSubscription: bool` on the existing model-provider struct.
- Option 2: First-class `Subscription { gateway_endpoint, entitlement_key }` variant of `ConnectionAuth` with a dedicated `GatewayCredentialSource`.
- Option 3 (chosen): Enum-tagged `ConnectionAuth` extended with a `SubscriptionLane` carrier; `Endpoint`-scoped `DialectSurface` carries its own wire encoder/decoder and status-code mapper.

## Decision Outcome

Chosen option: **Option 3** — preserve the existing `ConnectionAuth` enum (it already discriminates correctly at the connection level) and add a *declarative* `DialectSurface::SubscriptionProxy` variant carrying the `/alpha/generate` wire shape, so the request builder stays vendor-generic and the SSPL path can be exercised without touching `AnthropicMessagesProvider` / `OpenAiResponsesProvider` / `GoogleProvider` routing.

### Invariants & Behavioral Boundaries

- **`[INV-LANE-01] Three-Lane Exclusivity`**: every outbound model call resolves through exactly one invitation lane — `Local Direct (static)` / `Local Direct (OAuth)` / `Subscription Proxy`. A connection cannot mix; pasting an API key into a subscription connection downgrades it to `ConnectionAuth::ApiKey` outright (already implemented in `handlers_provider::add`).
- **`[INV-LANE-02] Credential never escapes its lane`**: subscription-lane bearer tokens are command-plane identities only; they are never forwarded to a model host, and model host keys are never logged, as `x-oauth-token` on a local-direct lane never hits the proxy header set.
- **`[INV-LANE-03] Server-enforced entitlement`**: plan gating, model-availability, credits, and routing are resolved **only** by the proxy in the SSPL lane. Client-side `allowedCategories`/`blockedModels` render as UI hostliness, never firewall decisions.
- **`[INV-LANE-04] Authoritative provenance carries in the finish-step envelope`**: the client records `providerMetadata.<resolved>.routing.planningReasoning` from the `finish-step` event into session telemetry. A lane that cannot produce routing provenance (local direct) leaves this field `None`.
- **`[INV-LANE-05] Window-fenced billing ledger is cross-lane`**: the client does not compute rate limits; it renders `windowLimits.fiveHour.cap` / `windowLimits.weekly.cap` and alert thresholds (75% default) emitted by the control plane. Local-direct lanes report usage only and never mutate the credit ledger.
- **`[INV-LANE-06] Local-only mode is a hard off-switch`**: when launch flag `--local-only` / `NUO_LOCAL_ONLY=1` is set, the SSPL transport refuses before resolution (endpoint-constant `ERR_LOCAL_ONLY_FORBIDDEN`) and the picker collapses to user-declared locals only; misplaced `featureModels` keys pointing at subscriptions are dropped with a `notice` rather than recycled.
- **`[INV-LANE-07] Feature-model resolution honors lane`**: `featureModels` per phase (`planning` / `implementation` / `titleGeneration` / `compaction` / `toolDescription` / `tasteLearning` / `tasteOnboarding` / `branchSummarization` / `vision`) respect `laneServesModel`, not silently downgraded via free/open-source default (`deepseek-v4.1-flash` family); documented in [feature-model contract](../../nuo-model-codec/src/connection_auth.rs).

### SSPL wire contract (verified against production 2026-10-04)

Routing:
- Base: `https://api.commandcode.ai` (`staging-api.commandcode.ai` staging, `http://localhost:9090` local).
- Route: `POST /alpha/generate` (SSE stream), Content-Type `application/json`; exact request shape is a Nuo contract, not a freeform map.
- Headers: `Authorization: Bearer <command-code-key>`, `x-cli-environment`, `x-command-code-version: <semver>`, `x-session-id: <uuid4>`, `x-project-slug`, plus optional `x-oauth-provider`, `x-taste-learning`, `x-cmd-zdr: 1`, `x-oss-primary-provider`.

Request (one envelope per turn):
```json
{
  "config": {"workingDir","date","environment","structure","isGitRepo","currentBranch","mainBranch","gitStatus","recentCommits"},
  "memory": null,
  "taste":  null,
  "skills": null,
  "permissionMode": "standard" | "plan" | "auto-accept",
  "threadId": "<uuid4>",           // ties prompt-cache lifetime
  "mode": "agent" | "learning" | "custom-agent" | "custom-agent-create"
        | "title-gen" | "tool-desc" | "compact" | "vision",
  "promptCache": "off" | "active",
  "params": {"model","messages","tools","system","max_tokens","stream":true}
}
```
Notes: `threadId` must be a UUID (nullable, not `null` string); `mode` is a closed server-side enum; `promptCache: "active"` powers the prompt-cache session-scoped discounts.
Client→server mis-envelopes return `400 BAD_REQUEST` with Zod field paths shown verbatim (`docs` field pointing to the code reference).

Error envelope (all errors):
```json
{"success":false,"error":{"code","status","message","docs"}}
```
Codes the Nuo TUI/daemon must terminate on: `BAD_REQUEST` (400), `FORBIDDEN` (403, includes `MODEL_NOT_IN_PLAN` and `Model/provider not recognized: <lane>/<model>`), `UNAUTHORIZED`/401, `USAGE_EXCEEDED` (spend-cap), `RATE_LIMITED` (429, includes `rateLimit:{window, reset}` for window-limited plans).

Success stream (SSE, Visual per-event wire):
| Event | Meaning | Provenance |
|---|---|---|
| `start` | turn lifecycle gate opens | — |
| `start-step` | request reflection: `request.body.model` is **what the server forwarded** (possibly rewritten), `messages` echo system `<role>/<tone_and_style>` prodos-reproducible, `providerOptions` included | uesed to render step-level "what ran" telemetry |
| `text-start` / `text-delta` / `text-end` | streamed code/text content | `id: "txt-N"` |
| `reasoning-start/-delta/-end` | streamed vendor reasoning when the resolved model emits it | — |
| `tool-start` / `tool-delta` / `tool-end` | streamed tool call assembly | — |
| `provider-metadata` | `providerMetadata.<resolved>.routing.planningReasoning` is a full **provenance sentence**: upstream provider, credential-class, ZDR admission, prompt-training policy | client stores verbatim |
| `finish-step` | step closure; `response.headers` carries **upstream** vendor headers (`x-ratelimit-limit-tokens`, `x-trace-id`, `x-fusion-transport`) and `response.modelId` the served family | provenance record |
| `finish` | turn closure; `totalUsage` (input/output/cached/reasoning; `textTokens`, `reasoningTokens` details) and `response` echo | billing already applied |

Behavior notes discovered in probe:
- The **system prompt is server-authored** (`<role>…</role><tone_and_style>…`) and is **not** carried in from `params.system` verbatim; the envelope's `system` field still works as an injected prompt but the standard `roles` block is always prepended. A Nuo SSPL server may render its own role prompt.
- The **model is server-resolved** — declared id is a hint, not a contract. `claude-sonnet-5-5` via gateway, `moonshotai/Kimi-K2.5` at `bedrock`, `openai/gpt-5.6-sol` via `openai`, `deepseek/deepseek-v4-flash` at `novita`.
- Cached tokens arrive via `inputTokenDetails.cacheReadTokens` — the prompt-cache discount pays out on **the proxy side**, so the client's cache ledger never knows what was free.
- Credits decrement **before** deltas flush; probe showed `monthlyCredits 70 → 69.597` for a single `gpt-5.6-sol` preflight turn. Client renderings must never hoist the "credits left" number.
- `x-cmd-zdr: 1` flowed through to `planningReasoning` (`ZDR requested: all 1 attempts support ZDR`), and returned upstream `novita` with `no prompt training`.

Subscription endpoints (discovered from the binary and probed live):
`GET /alpha/whoami[?limits=1]` → `{"success":true,"user":{"id","name","email","userName"},"org":null|"object"}`
`GET /alpha/billing/subscriptions[?orgId]` → subscription row (`planId:"individual-goat"`, `status:"active"`, `currentPeriodEnd`, Stripe `priceId`)
`GET /alpha/billing/credits[?orgId]` → `monthlyCredits` / `purchasedCredits` / `freeCredits` / `windowLimits{limited,exceeded,fiveHour:{used,cap,exceeded,resetAt},weekly:{...}}` / `sandboxAccess` / `sandboxMinutes`
`GET /alpha/usage/summary` → lifetime counters
`GET /alpha/namespaces` → `{"success":true,"type":"personal","user","orgs":[]}`
`GET /alpha/web-search` (POST) → search results (`/alpha/web-fetch` same)
`POST /alpha/fingerprint/record` — not probed (device fingerprint); treat as optional.

Plan ID vocabulary (`individual-go` 10 · `individual-go-v1` 10 · `individual-goat` 70 · `individual-provider` 15 · `individual-pro` 30 · `individual-pro-v1` 80 · `individual-max` 150 · `individual-ultra` 300 · `teams-pro` 40/pooled) is **server data**, never const'd into the TS/Rust client; Nuo snapshot titles using this vocabulary are display-only.

### Positive Consequences

- Remote-side billing/entitlement logic never leaks into the vendor request builders; `AnthropicMessagesProvider` / `OpenAiResponsesProvider` / `GoogleProvider` remain honest, testable, and vendor-clause-only.
- Adding an SSPL surface is a `DialectSurface` registration + a `CredentialSource` implementation — no new top-level crate, no dependency-axis change.
- The existing `Muta`-side provider usage counters already record `windowLimits`-shaped data (`ConnectionUsageState::RateLimitSpec`, `PeriodicQuota`), so plan-window rendering is a projection update, not a schema migration.
- Live-verified: the contract above is not doc-anchored guesswork — it reproduced `PONG` completion, plan-based denial, and cross-provider rerouting against the real GOAT key.

### Negative Consequences & Trade-offs

- **SSPL is a trust boundary on the server, not a protocol advantage**: the client cannot verify that "credits consumed" match tokens spent; a malicious or buggy proxy could over-bill invisibly. Mitigation: `finish-step`'s `response.headers.x-trace-id` and `planningReasoning` are persisted into session telemetry exactly as-is, so a mismatch dispute has a chain-of-custody record.
- **Model rewriting destabilizes caching assumptions**: the same declared id can land on a different upstream family per turn; `promptCache: "active"` loses consistency across upstream changes. Mitigation: cache keys include resolved routing provenance (not just the declared id), which is derivable from `start-step` body.
- **`finish` post-`provider-metadata` ordering is not a contract upstream**: a few vendors may emit `provider_metadata` *after* `finish-step`; the SSPL surface must subscribe to the last-seen metadata as the provenance winner per turn.

---

## Rejected Alternatives & Negative Knowledge

### Option 1 (Rejected) — Single `isSubscription: bool` dispatch flag
- **Why considered**: Smallest-diff extension of `ConnectionAuth`.
- **Why rejected**: Its boolean covers only "whose server am I hitting", losing the "whose credential am I using" axis. The live probe proves these axes diverge for free: `claude-sonnet-5-5` via GOAT plan reaches upstream via `gateway → claude-sonnet-5-5` routing — the same declared model ids succeed or fail purely because of server entitlement and routing, not because of an auth toggle. A boolean would sweep lane identity under the OAuth-vs-key rug again.

### Option 2 (Rejected) — Dedicated `GatewayCredentialSource` as a new top-level credential kind
- **Why considered**: Cleanest type separation for SSPL.
- **Why rejected**: A `CredentialSource::resolve_auth` returning only a proxy bearer is already the `OAuthCredentialSource` case with a different store file; introducing a third trait-object branch splits every generic consumer (`Endpoint::auth_scoped_headers`, `Client::build_request_for_auth`, `fetch_provider_usage`) into three parallel arms. The existing `ConnectionAuth::Subscription{provider}` discriminates precisely, and its `subscription_provider()` string is what routes to `config_by_provider_id`.

### Option 3-adjacent (Rejected) — Client-side plan/credit UI gate as authority
- **Why considered**: Nuo's existing `Availability` filter is entirely local; extending it with the plan matrix looks cheap.
- **Why rejected**: Contradicted live — client paints GOAT as "no premium", server still routes `claude-sonnet-5-5` (premium, but GOAT-allowed) and blocks `gpt-5.4` even though a picker-derived `blockedModels` list might render either way. Entitlement authority belongs to the lane that holds the credential; UI gating must be a projection of a server answer, not the decision itself. Nuo's `effective_availability` on user-declared providers is authoritative because those lanes never see a server answer; for SSPL it must be folded into the response-time projection instead.

### Option 3-adjacent (Rejected) — Local `x-cmd-zdr` mirror in Nuo
- **Why considered**: A ZDR knob is nice UX parity.
- **Why rejected**: Reproducing a vendor-specific opt-out header without a server-side semantics makes the knob a placebo. ZDR is measured by proxies that can actually enforce it (the probe confirmed `no prompt training` came from the server's planning reasoning, transmitted *because* the header was set). Nuo should add it only on a lane whose server controls the training contract.

### Option 4 (Rejected) — Make `/alpha/web-search` and `/alpha/web-fetch` a lane of their own
- **Why considered**: They are subscription-gated tools in the reference client, tempting to model as tool-lanes.
- **Why rejected**: They are just downstream endpoints of the same SSPL surface (same bearer, same headers); giving them a lane status multiplies the model in the wrong direction. Modeled as `Tool`s on the SSPL `DialectSurface` instead, exactly as the reference client wires them.

---

## Links

- Related ADRs: [ADR-0001](0001-flat-workspace-and-microkernel-capability-topology.md), [ADR-0008](0008-single-tool-contract.md), [ADR-0011](0011-nuo-tui-presentation-and-nuo-server-container.md), [ADR-0013](0013-decoupled-tool-namespace-and-infrastructure-purity.md)
- Implementation anchors: `nuo-model-codec/src/connection_auth.rs`, `nuo-model-codec/src/auth.rs`, `nuo-model-codec/src/client_identity.rs`, `nuo-model-codec/src/catalog.rs`, `nuo-providers/src/oauth/{mod,presets,token,enricher,store,credential_source}.rs`, `nuo-providers/src/registry/*`, `nuo-providers/src/protocol/*`, `nuo-providers/src/list_models.rs`, `nuo-server/src/handlers_provider.rs`, `nuo-persistence/src/{connections,model_providers}.rs`, `nuo-host/src/paths.rs`
- Probe artifacts (2026-10-04, GOAT key): `whoami` (`ming2k`, no org), `billing/subscriptions` → `planId:"individual-goat"`, `billing/credits` → `monthlyCredits 70→69.597` after one probe turn (gpt-5.6-sol via `openai`), windowed `fiveHour.cap 14` / `weekly.cap 35`, ZDR honored via `novita` upstream, `claude-sonnet-4-6` and `gpt-5.4` and `meta/muse-spark-1.1` via `FORBIDDEN MODEL_NOT_IN_PLAN`.
