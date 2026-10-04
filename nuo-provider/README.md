# nuo-provider

Canonical model provider contracts, capability specifications, and dynamic registry for the Nuo agent ecosystem (ADR-0015).

## Overview

Directly mirroring `nuo-tool` as a zero-agent-runtime, zero-heavy-persistence contract crate:
- **`Provider`**: Core inference and event streaming contract (`chat`, `stream_chat`, `stream_chat_events`).
- **`CatalogDiscovery`**: Orthogonal capability for remote model discovery (`list_models`).
- **`QuotaTracker`**: Orthogonal capability for credit balance and token quota inspection (`fetch_quota`).
- **`ProviderDescriptor`**: Immutable metadata declaring provider identity, default routes, and capabilities.
- **`ProviderRegistry`**: Dynamic, thread-safe factory and capability registry for pluggable provider drivers.

All high-level orchestration layers (`nuo-harness`, `nuo-server`, `nuo-agent`) and presentation clients (`nuo-tui`) depend strictly on `nuo-provider` abstractions. Concrete wire protocols and HTTP transports are implemented downstream in `nuo-provider-adapters`.
