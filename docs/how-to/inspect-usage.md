# How to Inspect Token Usage

Read cross-session token consumption and per-session context accounting.

---

## 1. Cross-Session Usage

Open the usage overlay to view token consumption aggregated across sessions — daily tokens, spend, and per-provider breakdowns:

```text
/usage
```

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

Provider quota (sliding-window allowances, credit balances, and account pools) is surfaced through the provider connection scene and ambient indicators rather than a standalone read command:

```text
/connections
```

Quota and usage are deliberately separate domains: quota reflects provider-enforced allowances, while usage reflects Nuo-side accounting.

---

## Related

- [Connect a Model Provider](connect-a-provider.md)
- [Reference: Lifecycle and Action Semantics](../reference/lifecycle-and-action-semantics.md)
