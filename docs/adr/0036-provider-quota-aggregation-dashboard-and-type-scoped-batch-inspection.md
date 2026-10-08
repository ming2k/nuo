---
id: ADR-0036
title: "Provider Quota Aggregation Dashboard, Type-Scoped Batch Inspection, and Domain Separation from Usage Accounting"
status: accepted
date: 2026-10-15
scope: wire/nuo-wire, server/nuo-server, tui/nuo-tui, cli/nuo, providers/nuo-provider-google-antigravity
superseded_by: null
negative_knowledge: true
---

# 0036. Provider Quota Aggregation Dashboard, Type-Scoped Batch Inspection, and Domain Separation from Usage Accounting

- Status: Accepted
- Date: 2026-10-15
- Deciders: Nuo Architecture Working Group
- Consulted: Provider Integration, Control Plane, TUI, and Human-Interface Architecture Teams
- Informed: Core Engineering, Developer Experience
- Amends: [ADR-0014](0014-model-provider-invocation-schemes-oauth-subscription-lane-and-api-key-byok-lane.md), [ADR-0015](0015-dependency-inversion-architecture-for-model-providers.md), [ADR-0027](0027-provider-definition-single-source-and-adapters-retirement.md), [ADR-0035](0035-domain-scoped-surface-architecture-and-encapsulated-dialog-lifecycle.md)

---

## Context and Problem Statement

Modern AI developers routinely maintain multiple accounts under tiered or rate-limited providers (notably Google Antigravity CodeAssist, Kimi Code, DeepSeek, and CommandCode) to balance sliding-window rate limits, daily allowances, and team quotas.

Previously, inspection of provider allowances in Nuo suffered from three architectural deficiencies:

1. **Vocabulary Conflation (`Usage` vs. `Quota`)**: The system frequently conflated local, historical token consumption (*"How many tokens have I spent?"*) with upstream, forward-looking provider allowances (*"What is my remaining capacity, rolling-window fraction, and reset time?"*). Nuo already possesses `/usage` (ADR-0122) as a durable cross-session token consumption ledger (`data/usage/YYYY-MM-DD.json`). Conflating provider limits under the same term caused cognitive dissonance and command collision.
2. **The $O(2N)$ Manual Drill-Down Antipattern**: Quota inspection was coupled exclusively to individual connection details (`QueryConnectionDetail`). To survey 5 or 10 accounts of the same provider type, an operator was forced to navigate: `Down -> Enter (Wait) -> Esc -> Down -> Enter (Wait) -> Esc`. There was no mechanism to inspect or compare accounts horizontally.
3. **Serial Network Fetching and Cold Client Instantiation**: Each quota query constructed an ephemeral HTTP client without connection pooling or TLS session reuse, passed empty project scoping parameters, and lacked memory-level TTL caching, causing severe cumulative latency when surveying multiple accounts.

We require a modern, zero-compromise architectural standard that strictly decouples **Quota** from **Usage**, provides type-scoped batch concurrent inspection, and exposes ambient indicators, a dedicated dashboard (`/quota`), and headless CLI tooling (`nuo quota`).

---

## Decision Drivers

1. **Semantic Precision & Domain Purity**: Unambiguous separation between spent consumption (`Usage`) and available capacity/entitlement (`Quota`).
2. **Sub-Second Multi-Account Visibility**: Operators with 5–20 accounts of a provider (e.g. Google Antigravity) must inspect their entire account pool concurrently in < 1 second.
3. **Zero Redundant Traffic**: Elimination of cold TCP/TLS handshakes via shared control-plane connection pools, memory TTL caching (3-minute default), and explicit project ID propagation.
4. **Actionable Presentation**: Allow direct activation/switching of accounts directly from the quota dashboard without returning to configuration forms.

---

## Architectural Decisions

### 1. Domain Separation: Usage vs. Quota

The platform formally separates consumption accounting from capacity inspection:

* **Usage (`/usage`, `TokenUsage`, `UsageStatsReport`)**:
  - Nature: Historical, additive consumption (outflow/spent).
  - Metrics: Input tokens, output tokens, thinking/reasoning tokens, compute duration, cost ledger.
  - Storage: Durable day-partitioned local database (`data/usage/`).
* **Quota (`/quota`, `nuo quota`, `ProviderQuotaSnapshot`)**:
  - Nature: Real-time, upstream capacity and allowance state (entitlement/remaining).
  - Metrics: Remaining percentage (e.g. 100%, 42%), rolling window buckets (5-hour, daily, weekly), reset countdowns, credit balance (USD/RMB), rate-limit backoffs.
  - Storage: In-memory TTL cache (`UsageCache` / `QuotaCache`, 180s default) with on-demand concurrent remote fetch.

### 2. Typed Wire Protocol Extensions (`nuo-wire`)

We define dedicated request and response types in `nuo-wire`:

```rust
// In AgentRequest
QueryProviderQuotas {
    /// Optional provider type filter (e.g. Some("google-antigravity")), or None for all quota-capable connections.
    provider: Option<String>,
    /// Bypass in-memory TTL cache and force remote fetch.
    #[serde(default)]
    force_refresh: bool,
}

// In AgentResponse
ProviderQuotas(ProviderQuotaSnapshot)
```

`ProviderQuotaSnapshot` aggregates individual `ConnectionQuotaEntry` instances:
- `connection_id`: Canonical connection instance name.
- `provider`: Provider type identifier (e.g. `"google-antigravity"`, `"deepseek"`).
- `account_id` / `user_email`: Account identity if available.
- `is_active`: Whether this connection is the currently active/default route.
- `state`: Typed `ConnectionUsageState` (Available, Fetching, Error, Unsupported).
- `primary_balance`: Extracted primary gauge (e.g. `"100%"`, `"$12.50"`).
- `next_reset_ms`: Earliest upcoming window reset timestamp.

### 3. Server-Side Concurrent Fan-Out & Shared Control-Plane Pool

In `nuo-server`:
- `handlers_provider::query_provider_quotas`:
  1. Inspects the in-memory cache. Entries within the 180-second TTL are returned immediately unless `force_refresh` is true.
  2. For cache misses, streams connection queries concurrently using bounded futures (`stream.buffer_unordered(4)`).
  3. Uses `Http::shared_control_plane()` ensuring TLS keep-alive connection reuse to provider hosts (`daily-cloudcode-pa.googleapis.com`, `api.deepseek.com`, etc.).
  4. Passes known `project_id` scoping to prevent upstream tenant resolution latencies.

### 4. Tri-Modal Presentation Topology

1. **Ambient Status in Connection List (`/connections`)**:
   - The connection list right-aligns primary quota balance badges (e.g. `100%`, `45%`, `0% !`).
   - Entering the modal triggers background prefetching for the filtered provider subset.
2. **Dedicated Quota Dashboard Modal (`/quota` / `/quotas`)**:
   - Accessible via slash command `/quota` or `/quotas`.
   - Visualizes all accounts grouped by provider, rendering multi-window bucket progress bars (5-hour, daily) and reset countdowns.
   - **Interactive Switch**: Pressing `Space` directly activates the highlighted account as the session's active connection.
   - Pressing `r` forces an in-place concurrent refresh.
3. **Headless CLI Command (`nuo quota [provider]`)**:
   - Executes headlessly without spawning the TUI:
     `nuo quota google-antigravity`
   - Prints an ASCII table detailing connection status, balances, window limits, and reset times.

---

## Invariants & Behavioral Boundaries

- **`[INV-QUOTA-01] Domain Boundary Purity`**: Upstream provider allowances, limits, and bucket percentages must NEVER be surfaced under `/usage` or mixed into token ledger APIs.
- **`[INV-QUOTA-02] Bounded Fan-Out`**: Batch quota inspection must bound network concurrency to avoid client-side socket starvation and upstream rate-limit tripping.
- **`[INV-QUOTA-03] Cache Bypass Guarantees`**: Any user-initiated refresh action (`r` key or `force_refresh: true`) MUST invalidate the local cache entry and guarantee an upstream round-trip.
- **`[INV-QUOTA-04] Direct Activation Symmetry`**: The quota dashboard is a control plane surface; selecting an account and pressing `Space` must immediately update session routing without requiring navigation into configuration menus.

---

## Negative Knowledge & Rejected Alternatives

### 1. Merging Quota into the `/usage` Surface
- **Why considered**: Reducing the count of modal commands by combining all numbers under "Usage".
- **Why rejected**: Violates Single Responsibility. `/usage` tracks client token consumption across time (a local financial/diagnostic ledger). Provider quota is volatile, upstream-controlled capacity. Conflating them destroys the mental model and clutters the UI with mismatched timeframes (historical aggregates vs. 5-hour rolling limits).

### 2. Client-Side Sequential Fetching Loop
- **Why considered**: Letting the TUI issue successive `QueryConnectionDetail` calls in an iterative loop.
- **Why rejected**: Produces $O(N)$ serial round-trips over the client-server IPC channel, locks the UI queue, and prevents the server from optimizing connection pooling across connections.

### 3. Continuous Background Daemon Polling
- **Why considered**: Constantly polling all provider quota endpoints in the background every few minutes.
- **Why rejected**: Unnecessary battery/network drain, risks getting user accounts banned or rate-limited by upstream providers (Google Antigravity enforces strict endpoint budgets), and violates on-demand design. On-demand fetch with 180s TTL caching achieves equal responsiveness with zero background waste.
