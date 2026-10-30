# How to Inspect Token Usage

Read cross-session token consumption and per-session context accounting.

---

## 1. Cross-Session Usage

View token consumption aggregated across all sessions — daily tokens, spend, and per-provider/model breakdowns:

```text
/usage
```

Model usage is decoupled from thread lifecycles (ADR-0129): it queries the durable store (`data/usage/`) over the session-free control plane. It can be inspected cold from the dashboard (`u` toggle) or via `/usage` even when zero threads are running.

Usage is a reporting surface only: it never mutates a provider's billing ledger.

---

## 2. Session Context Accounting

For the current thread, inspect context-window pressure and streaming throughput (tokens per second, time-to-first-token):

```text
/stats
```

For a latency waterfall of the current session's turns, use `/trace`.

---

## 3. Provider Allowances

Provider quota (sliding-window allowances, credit balances, and account pools) has its own dashboard, separate from usage accounting:

```text
/quotas
/quotas <provider>
```

Pass a provider id to scope the dashboard (for example `/quotas google-antigravity`). Provider connections themselves are managed from `/connections`.

Quota and usage are deliberately separate domains: quota reflects provider-enforced allowances, while usage reflects Nuo-side accounting.

---

## Related

- [Connect a Model Provider](connect-a-provider.md)
- [Reference: Lifecycle and Action Semantics](../reference/lifecycle-and-action-semantics.md)
